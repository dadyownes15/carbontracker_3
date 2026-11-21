//! Energinet: live intensity of the two Danish grids from Energi Data
//! Service, and its forecast for the same grid. No key, and nothing sent
//! but the grid asked for.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, NaiveDateTime, Utc};

use crate::adapters::intensity::electricity_maps::{cut_to_horizon, ForecastProbe};
use crate::application::forecaster::Fetch;
use crate::domain::location::{CountryCode, Location};
use crate::ports::{Outcome, Probed};
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, Reading, Sample, SourceId};
use carbontracker::{Fidelity, GridLocation, SourceDescription, Timestamp};

/// The candidate's name: Energinet publishes direct CO2 from production.
pub(crate) const ENERGINET: &str = "energinet (direct CO2)";
pub(crate) const ENERGINET_URL: &str = "https://api.energidataservice.dk";
/// A new figure is published every five minutes.
const ENERGINET_COOLDOWN: Duration = Duration::from_secs(60);
/// How long a request made from a read may wait on the service.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
const CO2EMIS: &str = "/dataset/CO2Emis";
const CO2EMISPROG: &str = "/dataset/CO2EmisProg";
const INTERVAL: Duration = Duration::from_secs(5 * 60);

/// One of Denmark's two grids, as Energi Data Service names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Area {
    Dk1,
    Dk2,
}

impl Area {
    fn price_area(self) -> &'static str {
        match self {
            Self::Dk1 => "DK1",
            Self::Dk2 => "DK2",
        }
    }

    fn zone(self) -> String {
        format!("DK-{}", self.price_area())
    }

    fn filter(self) -> (&'static str, String) {
        ("filter", format!(r#"{{"PriceArea":["{}"]}}"#, self.price_area()))
    }
}

const TWO_GRIDS: &str = "Denmark is two grids, DK-DK1 and DK-DK2; state the zone or coordinates";

/// The grid `location` lies in, or why Energinet cannot read it.
fn area(location: Option<&Location>) -> Result<Area, String> {
    let location = location.ok_or("no location")?;
    let outside = || format!("energinet reads only DK-DK1 and DK-DK2, and {location} is outside them");
    let denmark = CountryCode::new("DK");
    match location {
        Location::Stated(GridLocation::Zone { zone_id }) => match zone_id.trim() {
            "DK-DK1" => Ok(Area::Dk1),
            "DK-DK2" => Ok(Area::Dk2),
            "DK" => Err(TWO_GRIDS.to_owned()),
            _ => Err(outside()),
        },
        Location::Stated(GridLocation::Country { country_code }) if CountryCode::new(country_code) == denmark => {
            Err(TWO_GRIDS.to_owned())
        }
        Location::Stated(GridLocation::Coordinates { latitude, longitude }) => {
            area_at(*latitude, *longitude).ok_or_else(outside)
        }
        Location::Estimated(estimate) if Some(estimate.country) == denmark => match estimate.coordinates {
            Some(at) => area_at(at.latitude(), at.longitude()).ok_or_else(outside),
            None => Err(TWO_GRIDS.to_owned()),
        },
        _ => Err(outside()),
    }
}

/// Boxes around Danish land, as (south, north, west, east), coarse to about
/// a kilometre at the borders.
const DENMARK: [(f64, f64, f64, f64); 9] = [
    (55.07, 57.80, 8.00, 10.98),  // Jutland and Funen, north of Sylt
    (54.82, 55.07, 8.60, 9.50),   // along the German border
    (54.87, 55.07, 9.50, 9.65),   // north shore of Flensburg Fjord
    (54.82, 55.07, 9.65, 10.98),  // Als and Ærø
    (54.72, 54.82, 10.60, 10.98), // southern Langeland
    (56.60, 57.40, 10.98, 11.70), // Læsø and Anholt
    (54.55, 55.75, 10.98, 12.78), // Zealand, Lolland, Falster, Møn, Amager
    (55.75, 56.15, 10.98, 12.65), // North Zealand, short of Helsingborg
    (54.98, 55.31, 14.68, 15.20), // Bornholm
];

/// The grid a point in Denmark lies in: `DK2` east of the Great Belt and on
/// Bornholm, `DK1` west of it.
fn area_at(latitude: f64, longitude: f64) -> Option<Area> {
    let inside = |(south, north, west, east): &(f64, f64, f64, f64)| {
        (*south..=*north).contains(&latitude) && (*west..=*east).contains(&longitude)
    };
    if !DENMARK.iter().any(inside) {
        return None;
    }
    let east_of_great_belt = match latitude {
        // The Kattegat islands are DK1.
        lat if lat >= 56.2 => false,
        // Between Samsø and Røsnæs.
        lat if lat >= 55.6 => longitude >= 10.80,
        // Between Funen and Langeland, and Zealand and Lolland.
        _ => longitude >= 10.98,
    };
    Some(if east_of_great_belt { Area::Dk2 } else { Area::Dk1 })
}

/// `{"records": [{"Minutes5UTC": "2026-11-20T00:20:00", "CO2Emission": 90.0}, …]}`:
/// each figure stamped at the start of its five minutes, in UTC. A record
/// that doesn't parse fails the whole answer.
fn parse_records(answer: &serde_json::Value) -> Option<Vec<Sample>> {
    answer
        .get("records")?
        .as_array()?
        .iter()
        .map(|record| {
            let at = NaiveDateTime::parse_from_str(record.get("Minutes5UTC")?.as_str()?, "%Y-%m-%dT%H:%M:%S").ok()?;
            Some(Sample::new(at.and_utc(), record.get("CO2Emission")?.as_f64()?))
        })
        .collect()
}

fn invalid() -> ProviderError {
    ProviderError::Invalid("an answer that does not parse".to_owned())
}

/// The latest figure for an interval that has begun by `now`.
fn latest_begun(answer: &serde_json::Value, now: DateTime<Utc>) -> Result<Sample, ProviderError> {
    parse_records(answer)
        .ok_or_else(invalid)?
        .into_iter()
        .filter(|sample| *sample.observed_at() <= now)
        .max_by_key(|sample| *sample.observed_at())
        .ok_or_else(|| ProviderError::Invalid("no figure for an interval that has begun".to_owned()))
}

fn latest_params(area: Area) -> Vec<(&'static str, String)> {
    vec![area.filter(), ("end", "now".to_owned()), ("sort", "Minutes5UTC DESC".to_owned()), ("limit", "3".to_owned())]
}

/// From the interval now, as many points as `horizon` holds.
fn forecast_params(area: Area, horizon: Duration) -> Vec<(&'static str, String)> {
    let points = horizon.as_secs() / INTERVAL.as_secs() + 2;
    vec![
        area.filter(),
        ("start", "now-PT5M".to_owned()),
        ("sort", "Minutes5UTC ASC".to_owned()),
        ("limit", points.to_string()),
    ]
}

async fn get(client: &reqwest::Client, url: String, params: &[(&str, String)]) -> Result<serde_json::Value, ProviderError> {
    let response = client.get(url).query(params).send().await.map_err(unreachable)?;
    if let Some(error) = refusal(response.status()) {
        return Err(error);
    }
    let body = response.text().await.map_err(unreachable)?;
    serde_json::from_str(&body).map_err(|_| ProviderError::Invalid("an answer that is not JSON".to_owned()))
}

/// Blocking GET, for reads and the forecaster. Builds its own client, so it
/// never runs on, or outlives, an async runtime.
fn blocking_get(url: String, params: &[(&str, String)]) -> Result<serde_json::Value, ProviderError> {
    let client = reqwest::blocking::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(unreachable)?;
    let response = client.get(url).query(params).send().map_err(unreachable)?;
    if let Some(error) = refusal(response.status()) {
        return Err(error);
    }
    let body = response.text().map_err(unreachable)?;
    serde_json::from_str(&body).map_err(|_| ProviderError::Invalid("an answer that is not JSON".to_owned()))
}

fn unreachable(error: reqwest::Error) -> ProviderError {
    ProviderError::Unavailable(format!("Energi Data Service: {error}"))
}

/// The error for a status that refuses the request, if it does.
fn refusal(status: reqwest::StatusCode) -> Option<ProviderError> {
    let error = match status.as_u16() {
        200..=299 => return None,
        429 => ProviderError::RateLimited(format!("Energi Data Service was asked too often ({status})")),
        _ => ProviderError::Unavailable(format!("Energi Data Service answered {status}")),
    };
    Some(error)
}

/// Returns the outcome and the intensity from the confirming fetch.
pub(crate) async fn probe_energinet(
    id: ProviderId,
    client: &reqwest::Client,
    base_url: &str,
    location: Option<&Location>,
) -> Probed<(Outcome, f64)> {
    let area = area(location)?;
    let answer = get(client, format!("{base_url}{CO2EMIS}"), &latest_params(area))
        .await
        .map_err(|error| format!("{}: {error}", area.zone()))?;
    let current = latest_begun(&answer, Utc::now()).map_err(|error| format!("{}: {error}", area.zone()))?;
    let g_per_kwh = current.value();

    let source = SourceId::new(id, 0);
    let provider = Energinet { base_url: base_url.to_owned(), area, source, primed: Some(current), last: None };
    let description =
        SourceDescription::Intensity { location: GridLocation::Zone { zone_id: area.zone() }, fidelity: Fidelity::Measured };
    Ok((Outcome::single(source, description, provider), g_per_kwh))
}

/// The latest intensity of one Danish grid.
struct Energinet {
    base_url: String,
    area: Area,
    source: SourceId,
    /// The probe's confirming fetch, answered once instead of refetching.
    primed: Option<Sample>,
    /// The interval the last figure reported was for.
    last: Option<Timestamp>,
}

impl Provider for Energinet {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let sample = match self.primed.take() {
            Some(sample) => sample,
            None => latest_begun(&blocking_get(format!("{}{CO2EMIS}", self.base_url), &latest_params(self.area))?, Utc::now())?,
        };
        let mut response = ProviderResponse::new();
        if self.last.replace(*sample.observed_at()) == Some(*sample.observed_at()) {
            response.insert(self.source, Reading::Unchanged);
        } else {
            response.samples(self.source, vec![sample]);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(ENERGINET_COOLDOWN)
    }
}

/// The `CO2EmisProg` forecast for the grid the location lies in, reaching
/// as far as Energinet publishes, at most `horizon`. Declines when the
/// answer has no point ahead.
pub(crate) async fn probe_energinet_forecast(
    client: &reqwest::Client,
    base_url: &str,
    location: Option<&Location>,
    horizon: Duration,
) -> Probed<ForecastProbe> {
    let area = area(location)?;
    let url = format!("{base_url}{CO2EMISPROG}");
    let params = forecast_params(area, horizon);
    let answer = get(client, url.clone(), &params).await.map_err(|error| format!("{}: {error}", area.zone()))?;
    let curve = parse_records(&answer).ok_or_else(|| format!("{}: {}", area.zone(), invalid()))?;
    let now = Utc::now();
    let reach = curve
        .last()
        .and_then(|last| (*last.observed_at() - now).to_std().ok())
        .filter(|reach| !reach.is_zero())
        .ok_or_else(|| format!("the forecast for {} has no point ahead", area.zone()))?;

    let fetch: Fetch = Arc::new(move || {
        let curve = parse_records(&blocking_get(url.clone(), &params).ok()?)?;
        Some(cut_to_horizon(&curve, Utc::now(), horizon))
    });
    Ok(ForecastProbe { zone: area.zone(), curve: cut_to_horizon(&curve, now, horizon), horizon: horizon.min(reach), fetch })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC/OPEN-10
    #[test]
    fn the_great_belt_splits_denmark_into_its_two_grids() {
        for (place, latitude, longitude, grid) in [
            ("Copenhagen", 55.68, 12.57, Area::Dk2),
            ("Korsør", 55.33, 11.14, Area::Dk2),
            ("Røsnæs", 55.74, 10.90, Area::Dk2),
            ("Helsingør", 56.03, 12.60, Area::Dk2),
            ("Gedser", 54.57, 11.93, Area::Dk2),
            ("Rønne, Bornholm", 55.10, 14.70, Area::Dk2),
            ("Nyborg", 55.31, 10.79, Area::Dk1),
            ("Samsø", 55.85, 10.62, Area::Dk1),
            ("Aarhus", 56.16, 10.20, Area::Dk1),
            ("Skagen", 57.72, 10.58, Area::Dk1),
            ("Læsø", 57.27, 11.05, Area::Dk1),
            ("Esbjerg", 55.47, 8.45, Area::Dk1),
            ("Sønderborg", 54.91, 9.79, Area::Dk1),
            ("Rudkøbing, Langeland", 54.94, 10.71, Area::Dk1),
        ] {
            assert_eq!(area_at(latitude, longitude), Some(grid), "{place}");
        }
    }

    /// AC/OPEN-6
    #[test]
    fn a_point_outside_denmark_lies_in_neither_grid() {
        for (place, latitude, longitude) in [
            ("Malmö", 55.60, 13.00),
            ("Helsingborg", 56.05, 12.70),
            ("Gothenburg", 57.71, 11.97),
            ("Flensburg", 54.79, 9.43),
            ("Sylt", 54.95, 8.35),
            ("Fehmarn", 54.45, 11.10),
            ("Oslo", 59.91, 10.75),
        ] {
            assert_eq!(area_at(latitude, longitude), None, "{place}");
        }
    }

    #[test]
    fn the_forecast_asks_from_the_interval_now_for_the_horizon() {
        let params = forecast_params(Area::Dk1, Duration::from_secs(60 * 60));
        assert!(params.contains(&("filter", r#"{"PriceArea":["DK1"]}"#.to_owned())));
        assert!(params.contains(&("limit", "14".to_owned())), "{params:?}");
    }
}
