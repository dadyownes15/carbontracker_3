//! NESO's Carbon Intensity API: live intensity and its forecast for Great
//! Britain, nationally or for one of NESO's regions.
//!
//! National figures are read from the latest half-hour with an `actual`;
//! a region has only a `forecast`, so it is read from the current
//! half-hour's. The API takes no key and is sent nothing but the grid asked
//! for.

// Not yet placed in the chains.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::adapters::intensity::electricity_maps::{cut_to_horizon, ForecastProbe};
use crate::application::forecaster::Fetch;
use crate::domain::location::{Coordinates, Location};
use crate::ports::{Outcome, Probed};
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, Reading, Sample, SourceId};
use carbontracker::{Fidelity, GridLocation, SourceDescription, Timestamp};

pub(crate) const NESO_URL: &str = "https://api.carbonintensity.org.uk";
/// The candidate's name. NESO's factors count the CO2 emitted at generation,
/// not lifecycle emissions.
pub(crate) const NESO: &str = "neso (direct CO2)";
/// NESO publishes every half-hour.
const NESO_COOLDOWN: Duration = Duration::from_secs(5 * 60);
/// How long a request made from a read may wait on the service.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// The forecast endpoints reach 48 hours.
const FORECAST_REACH: Duration = Duration::from_secs(48 * 60 * 60);
/// NESO's region ids: 1 to 14 the distribution regions, 15 to 17 England,
/// Scotland and Wales.
const REGION_IDS: std::ops::RangeInclusive<u8> = 1..=17;
/// The form NESO takes and gives times in: `2026-11-20T00:30Z`.
const TIME_FORMAT: &str = "%Y-%m-%dT%H:%MZ";

/// Which of NESO's grids is read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Target {
    National,
    Region(u8),
}

impl Target {
    /// The zone the readout is described as.
    fn zone(self) -> String {
        match self {
            Self::National => "GB".to_owned(),
            Self::Region(id) => format!("GB-NESO-{id}"),
        }
    }

    fn latest_path(self) -> String {
        match self {
            Self::National => "/intensity".to_owned(),
            Self::Region(id) => format!("/regional/regionid/{id}"),
        }
    }

    fn forecast_path(self, from: DateTime<Utc>) -> String {
        let from = from.format(TIME_FORMAT);
        match self {
            Self::National => format!("/intensity/{from}/fw48h"),
            Self::Region(id) => format!("/regional/intensity/{from}/fw48h/regionid/{id}"),
        }
    }
}

/// The grid `location` reads, or why NESO cannot read it. Decided without a
/// request.
pub(crate) fn target(location: Option<&Location>) -> Result<Target, String> {
    let Some(location) = location else {
        return Err("no location, and NESO publishes only Great Britain's grid".to_owned());
    };
    let outside = || format!("{location} is outside Great Britain, the only grid NESO publishes");
    let northern_ireland = || format!("{location} is in Northern Ireland, which is not on the Great Britain grid");
    match location {
        Location::Stated(GridLocation::Zone { zone_id }) => {
            let zone = zone_id.trim().to_ascii_uppercase();
            match zone.as_str() {
                "GB" => Ok(Target::National),
                "GB-NIR" => Err(northern_ireland()),
                _ => match zone.strip_prefix("GB-NESO-") {
                    Some(id) => id
                        .parse()
                        .ok()
                        .filter(|id| REGION_IDS.contains(id))
                        .map(Target::Region)
                        .ok_or_else(|| format!("{} is not a NESO region: region ids are 1 to 17", zone_id.trim())),
                    None => Err(outside()),
                },
            }
        }
        Location::Stated(GridLocation::Country { country_code }) if country_code.trim().eq_ignore_ascii_case("GB") => {
            Ok(Target::National)
        }
        Location::Stated(GridLocation::Coordinates { latitude, longitude }) => {
            match Coordinates::new(*latitude, *longitude).map(place) {
                Some(Place::Britain) => Ok(Target::National),
                Some(Place::NorthernIreland) => Err(northern_ireland()),
                _ => Err(outside()),
            }
        }
        Location::Stated(_) => Err(outside()),
        Location::Estimated(estimate) if estimate.country.as_str() != "GB" => Err(outside()),
        // The country is Great Britain's; coordinates only tell Northern Ireland apart.
        Location::Estimated(estimate) => match estimate.coordinates.map(place) {
            Some(Place::NorthernIreland) => Err(northern_ireland()),
            // An estimate reads national until its postcode is looked up (AC/OPEN-41).
            _ => Ok(Target::National),
        },
    }
}

#[derive(Debug, PartialEq)]
enum Place {
    Britain,
    NorthernIreland,
    Elsewhere,
}

/// Where a point lies, by coarse boxes: good to a few kilometres at the
/// coasts and the Irish border.
fn place(at: Coordinates) -> Place {
    let (latitude, longitude) = (at.latitude(), at.longitude());
    let ireland = longitude < -5.4
        && (51.3..=55.45).contains(&latitude)
        // The Mull of Kintyre lies west of Belfast.
        && !(longitude > -6.0 && latitude > 55.25);
    if ireland {
        return if latitude >= 54.0 && longitude > -8.2 { Place::NorthernIreland } else { Place::Elsewhere };
    }
    let britain = (49.85..=60.9).contains(&latitude)
        && (-8.7..=1.8).contains(&longitude)
        // The French coast from Calais south.
        && !(latitude < 51.05 && longitude > 1.45);
    if britain { Place::Britain } else { Place::Elsewhere }
}

/// Where both candidates ask.
#[derive(Clone)]
pub(crate) struct Api {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
}

impl Api {
    pub(crate) fn new(client: &reqwest::Client, base_url: &str) -> Self {
        Self { client: client.clone(), base_url: base_url.to_owned() }
    }

    async fn get(&self, path: &str) -> Result<serde_json::Value, ProviderError> {
        let response = self.client.get(format!("{}{path}", self.base_url)).send().await.map_err(unreachable)?;
        if let Some(error) = refusal(response.status()) {
            return Err(error);
        }
        let body = response.text().await.map_err(unreachable)?;
        serde_json::from_str(&body).map_err(|_| not_json())
    }

    async fn latest(&self, target: Target) -> Result<Sample, ProviderError> {
        match latest_or(target, &self.get(&target.latest_path()).await?)? {
            Latest::Found(sample) => Ok(sample),
            Latest::Ask(path) => last_actual(&self.get(&path).await?),
        }
    }
}

/// Blocking GET of `path`, for reads and the forecaster. Builds its own
/// client, so it never runs on, or outlives, an async runtime.
fn blocking_get(base_url: &str, path: &str) -> Result<serde_json::Value, ProviderError> {
    let client = reqwest::blocking::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(unreachable)?;
    let response = client.get(format!("{base_url}{path}")).send().map_err(unreachable)?;
    if let Some(error) = refusal(response.status()) {
        return Err(error);
    }
    let body = response.text().map_err(unreachable)?;
    serde_json::from_str(&body).map_err(|_| not_json())
}

fn blocking_latest(base_url: &str, target: Target) -> Result<Sample, ProviderError> {
    match latest_or(target, &blocking_get(base_url, &target.latest_path())?)? {
        Latest::Found(sample) => Ok(sample),
        Latest::Ask(path) => last_actual(&blocking_get(base_url, &path)?),
    }
}

fn unreachable(error: reqwest::Error) -> ProviderError {
    ProviderError::Unavailable(format!("NESO: {error}"))
}

fn not_json() -> ProviderError {
    ProviderError::Invalid("an answer that is not JSON".to_owned())
}

/// The error for a status that refuses the request, if it does.
fn refusal(status: reqwest::StatusCode) -> Option<ProviderError> {
    let error = match status.as_u16() {
        200..=299 => return None,
        429 => ProviderError::RateLimited(format!("NESO was asked too often ({status})")),
        _ => ProviderError::Unavailable(format!("NESO answered {status}")),
    };
    Some(error)
}

/// One half-hour: when it starts, and its figures.
struct Point {
    from: DateTime<Utc>,
    forecast: Option<f64>,
    actual: Option<f64>,
}

/// The half-hours of an answer, national (`{"data": [point, …]}`) or
/// regional (`{"data": [{"data": [point, …]}]}`, or `{"data": {"data": […]}}`
/// for a regional forecast). A point that doesn't parse fails the answer.
fn points(answer: &serde_json::Value) -> Option<Vec<Point>> {
    let data = answer.get("data")?;
    let list = match data {
        serde_json::Value::Object(region) => region.get("data")?,
        serde_json::Value::Array(items) => match items.first().and_then(|first| first.get("data")) {
            Some(regional) => regional,
            None => data,
        },
        _ => return None,
    };
    list.as_array()?
        .iter()
        .map(|point| {
            let from = NaiveDateTime::parse_from_str(point.get("from")?.as_str()?, TIME_FORMAT).ok()?.and_utc();
            let intensity = point.get("intensity")?;
            let figure = |name: &str| intensity.get(name).and_then(|value| value.as_f64());
            Some(Point { from, forecast: figure("forecast"), actual: figure("actual") })
        })
        .collect()
}

fn unparsed() -> ProviderError {
    ProviderError::Invalid("an intensity that does not parse".to_owned())
}

enum Latest {
    Found(Sample),
    /// No actual in the answer: the past day's half-hours, from this path, have one.
    Ask(String),
}

/// The figure for `target` in an answer from its latest path: national,
/// the latest begun half-hour's actual; a region, its forecast.
fn latest_or(target: Target, answer: &serde_json::Value) -> Result<Latest, ProviderError> {
    let now = Utc::now();
    let begun = points(answer).ok_or_else(unparsed)?.into_iter().filter(|point| point.from <= now);
    let found = match target {
        Target::National => begun.filter_map(|point| Some(Sample::new(point.from, point.actual?))).next_back(),
        Target::Region(_) => begun.filter_map(|point| Some(Sample::new(point.from, point.forecast?))).next_back(),
    };
    match (found, target) {
        (Some(sample), _) => Ok(Latest::Found(sample)),
        (None, Target::National) => Ok(Latest::Ask(format!("/intensity/{}/pt24h", now.format(TIME_FORMAT)))),
        (None, Target::Region(_)) => Err(ProviderError::Invalid("a region's answer without a forecast".to_owned())),
    }
}

/// The latest begun half-hour with an actual.
fn last_actual(answer: &serde_json::Value) -> Result<Sample, ProviderError> {
    let now = Utc::now();
    points(answer)
        .ok_or_else(unparsed)?
        .into_iter()
        .filter(|point| point.from <= now)
        .filter_map(|point| Some(Sample::new(point.from, point.actual?)))
        .next_back()
        .ok_or_else(|| ProviderError::Invalid("no half-hour in the past day with an actual".to_owned()))
}

/// The forecast curve, each point stamped at its half-hour's start.
fn parse_forecast(answer: &serde_json::Value) -> Result<Vec<Sample>, ProviderError> {
    let curve = points(answer)
        .ok_or_else(|| ProviderError::Invalid("a forecast that does not parse".to_owned()))?
        .into_iter()
        .filter_map(|point| Some(Sample::new(point.from, point.forecast?)))
        .collect();
    Ok(curve)
}

/// Returns the outcome and the intensity from the confirming fetch.
pub(crate) async fn probe_neso(id: ProviderId, api: Api, location: Option<&Location>) -> Probed<(Outcome, f64)> {
    let target = target(location)?;
    let current = api.latest(target).await.map_err(|error| format!("{}: {error}", target.zone()))?;
    let g_per_kwh = current.value();

    let sid = SourceId::new(id, 0);
    let provider = Neso {
        base_url: api.base_url,
        target,
        source: sid,
        // The confirming fetch above answers the first read.
        primed: Some(current),
        last: None,
    };
    let description = SourceDescription::Intensity {
        location: GridLocation::Zone { zone_id: target.zone() },
        fidelity: Fidelity::Measured,
    };
    Ok((Outcome::single(sid, description, provider), g_per_kwh))
}

/// The latest intensity of one of NESO's grids.
struct Neso {
    base_url: String,
    target: Target,
    source: SourceId,
    /// The probe's confirming fetch, answered once instead of refetching.
    primed: Option<Sample>,
    /// When the last figure reported was for.
    last: Option<Timestamp>,
}

impl Provider for Neso {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let sample = match self.primed.take() {
            Some(sample) => sample,
            None => blocking_latest(&self.base_url, self.target)?,
        };
        let mut response = ProviderResponse::new();
        // The same half-hour as last time is no new observation.
        if self.last.replace(*sample.observed_at()) == Some(*sample.observed_at()) {
            response.insert(self.source, Reading::Unchanged);
        } else {
            response.samples(self.source, vec![sample]);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(NESO_COOLDOWN)
    }
}

/// NESO's forecast for the grid `location` reads, as far as NESO publishes
/// it. Declines what [probe_neso] declines, or an answer with no point ahead.
pub(crate) async fn probe_neso_forecast(api: Api, location: Option<&Location>, horizon: Duration) -> Probed<ForecastProbe> {
    let target = target(location)?;
    let zone = target.zone();
    let horizon = horizon.min(FORECAST_REACH);
    let now = Utc::now();

    let answer = api.get(&target.forecast_path(now)).await.map_err(|error| format!("{zone}: {error}"))?;
    let curve = parse_forecast(&answer).map_err(|error| format!("{zone}: {error}"))?;
    let reach = curve
        .last()
        .and_then(|last| (*last.observed_at() - now).to_std().ok())
        .filter(|reach| !reach.is_zero())
        .ok_or_else(|| format!("the forecast for {zone} has no point ahead"))?;

    let base_url = api.base_url.clone();
    let fetch: Fetch = Arc::new(move || {
        let now = Utc::now();
        let curve = parse_forecast(&blocking_get(&base_url, &target.forecast_path(now)).ok()?).ok()?;
        Some(cut_to_horizon(&curve, now, horizon))
    });
    Ok(ForecastProbe { zone, curve: cut_to_horizon(&curve, now, horizon), horizon: horizon.min(reach), fetch })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(latitude: f64, longitude: f64) -> Place {
        place(Coordinates::new(latitude, longitude).unwrap())
    }

    /// AC/OPEN-10 AC/OPEN-12
    #[test]
    fn places_near_the_edges_of_the_gb_grid_fall_on_the_right_side() {
        for (name, latitude, longitude) in [
            ("Land's End", 50.07, -5.71),
            ("Dover", 51.13, 1.31),
            ("Mull of Kintyre", 55.29, -5.80),
            ("Stranraer", 54.90, -5.02),
            ("Lerwick", 60.15, -1.15),
            ("Stornoway", 58.21, -6.39),
        ] {
            assert_eq!(at(latitude, longitude), Place::Britain, "{name}");
        }
        for (name, latitude, longitude) in [("Belfast", 54.60, -5.93), ("Derry", 55.00, -7.32), ("Enniskillen", 54.34, -7.64)] {
            assert_eq!(at(latitude, longitude), Place::NorthernIreland, "{name}");
        }
        for (name, latitude, longitude) in [("Dublin", 53.35, -6.26), ("Calais", 50.95, 1.86), ("Cork", 51.90, -8.47)] {
            assert_eq!(at(latitude, longitude), Place::Elsewhere, "{name}");
        }
    }
}
