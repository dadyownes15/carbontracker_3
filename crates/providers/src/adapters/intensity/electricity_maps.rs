//! Electricity Maps: live grid intensity, and its forecast for the same grid.
//!
//! Both candidates map the [Location] to the same ranked queries (see
//! [queries]), take the first the key can read, keep the zone the API
//! answers with, and ask for that zone from then on, so every readout
//! measures the same grid.

use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::application::forecaster::Fetch;
use crate::domain::config::ApiKey;
use crate::domain::location::Location;
use crate::ports::Outcome;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, Reading, Sample, SourceId};
use carbontracker::{Fidelity, GridLocation, SourceDescription, Timestamp};

/// Electricity Maps publishes far less often; reading faster would spend the
/// API's request allowance for nothing.
const ELECTRICITY_MAPS_COOLDOWN: Duration = Duration::from_secs(60);
/// How long a request made from a read may wait on the service.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const ELECTRICITY_MAPS_URL: &str = "https://api.electricitymaps.com/v3";
/// The only horizons the forecast endpoint accepts.
const FORECAST_HORIZON_HOURS: [u64; 4] = [6, 24, 48, 72];

/// How Electricity Maps is asked for a grid.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Query {
    Zone(String),
    Coordinates { latitude: f64, longitude: f64 },
}

impl Query {
    /// The query parameters. `disableCallerLookup` stops the API from placing
    /// a query it can't by the caller's IP, a hidden estimate the resolution
    /// wouldn't show; `lifecycle` is the country table's basis.
    fn params(&self) -> Vec<(&'static str, String)> {
        let mut params = match self {
            Self::Zone(zone) => vec![("zone", zone.clone())],
            Self::Coordinates { latitude, longitude } => {
                vec![("lat", latitude.to_string()), ("lon", longitude.to_string())]
            }
        };
        params.push(("disableCallerLookup", "true".to_owned()));
        params.push(("emissionFactorType", "lifecycle".to_owned()));
        params
    }
}

/// The queries for `location`, most precise first, shared by the intensity
/// and forecast candidates so they ask for the same grid. Each candidate
/// takes the first the key can read; none declines.
///
/// The country's zone comes after a sub-zone or coordinates: a key's plan
/// may reach a country's aggregate but not its sub-zones (a free key reaches
/// one zone), and the aggregate is still live data for the grid the host is
/// on. Every country with data has a zone keyed by its code; only `CL` and
/// `RU` don't, and are refused.
pub(crate) fn queries(location: Option<&Location>) -> Vec<Query> {
    let Some(location) = location else { return Vec::new() };
    let precise = match location {
        Location::Stated(GridLocation::Zone { zone_id }) => Some(Query::Zone(zone_id.trim().to_owned())),
        Location::Stated(GridLocation::Coordinates { latitude, longitude }) => {
            Some(Query::Coordinates { latitude: *latitude, longitude: *longitude })
        }
        Location::Stated(GridLocation::DataCenter { .. } | GridLocation::Global) => return Vec::new(),
        Location::Stated(GridLocation::Country { .. }) => None,
        // Coordinates give the precise zone even in a split country.
        Location::Estimated(estimate) => estimate
            .coordinates
            .map(|at| Query::Coordinates { latitude: at.latitude(), longitude: at.longitude() }),
    };
    let country = location.country().map(|code| Query::Zone(code.to_string()));

    let mut queries: Vec<Query> = precise.into_iter().collect();
    queries.extend(country.filter(|country| !queries.contains(country)));
    queries
}

/// Where and with what key both candidates ask.
#[derive(Clone)]
pub(crate) struct Api {
    pub(crate) client: reqwest::Client,
    pub(crate) base_url: String,
    pub(crate) key: String,
}

impl Api {
    pub(crate) fn new(client: &reqwest::Client, base_url: &str, key: &ApiKey) -> Self {
        Self { client: client.clone(), base_url: base_url.to_owned(), key: key.expose().to_owned() }
    }

    async fn get(&self, path: &str, params: &[(&str, String)]) -> Result<serde_json::Value, ProviderError> {
        let response = self
            .client
            .get(format!("{}{path}", self.base_url))
            .query(params)
            .header("auth-token", &self.key)
            .send()
            .await
            .map_err(unreachable)?;
        if let Some(error) = refusal(response.status()) {
            return Err(error);
        }
        let body = response.text().await.map_err(unreachable)?;
        serde_json::from_str(&body).map_err(|_| ProviderError::Invalid("an answer that is not JSON".to_owned()))
    }

    /// The zone answered, and its current intensity.
    async fn latest(&self, query: &Query) -> Result<(String, Sample), ProviderError> {
        parse_latest(&self.get("/carbon-intensity/latest", &query.params()).await?)
    }

    /// The zone answered and its forecast curve.
    async fn forecast(&self, query: &Query, horizon: Duration) -> Result<(String, Vec<Sample>), ProviderError> {
        let mut params = query.params();
        params.push(("horizonHours", horizon_hours(horizon).to_string()));
        let answer = self.get("/carbon-intensity/forecast", &params).await?;
        parse_forecast(&answer).ok_or_else(|| ProviderError::Invalid("a forecast that does not parse".to_owned()))
    }
}

/// Blocking GET of `path`, for reads and the forecaster. Builds its own
/// client, so it never runs on, or outlives, an async runtime.
fn blocking_get(base_url: &str, key: &str, path: &str, params: &[(&str, String)]) -> Result<serde_json::Value, ProviderError> {
    let client = reqwest::blocking::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(unreachable)?;
    let response =
        client.get(format!("{base_url}{path}")).query(params).header("auth-token", key).send().map_err(unreachable)?;
    if let Some(error) = refusal(response.status()) {
        return Err(error);
    }
    let body = response.text().map_err(unreachable)?;
    serde_json::from_str(&body).map_err(|_| ProviderError::Invalid("an answer that is not JSON".to_owned()))
}

fn unreachable(error: reqwest::Error) -> ProviderError {
    ProviderError::Unavailable(format!("Electricity Maps: {error}"))
}

/// The error for a status that refuses the request, if it does.
pub(crate) fn refusal(status: reqwest::StatusCode) -> Option<ProviderError> {
    let error = match status.as_u16() {
        200..=299 => return None,
        401 | 403 => ProviderError::Denied(format!("Electricity Maps refused the key ({status})")),
        429 => ProviderError::RateLimited(format!("Electricity Maps was asked too often ({status})")),
        _ => ProviderError::Unavailable(format!("Electricity Maps answered {status}")),
    };
    Some(error)
}

/// `{"zone": "DK-DK2", "carbonIntensity": 318, "datetime": "…"}`: the zone,
/// and the intensity stamped with the time it was measured for.
pub(crate) fn parse_latest(answer: &serde_json::Value) -> Result<(String, Sample), ProviderError> {
    let invalid = || ProviderError::Invalid("a latest intensity that does not parse".to_owned());
    let zone = answer.get("zone").and_then(|zone| zone.as_str()).ok_or_else(invalid)?;
    let intensity = answer.get("carbonIntensity").and_then(|value| value.as_f64()).ok_or_else(invalid)?;
    let at = answer
        .get("datetime")
        .and_then(|at| at.as_str())
        .and_then(|at| DateTime::parse_from_rfc3339(at).ok())
        .ok_or_else(invalid)?;
    Ok((zone.to_owned(), Sample::new(at.with_timezone(&Utc), intensity)))
}

/// The smallest horizon the endpoint accepts that covers `horizon`, at most 72 hours.
pub(crate) fn horizon_hours(horizon: Duration) -> u64 {
    FORECAST_HORIZON_HOURS
        .into_iter()
        .find(|hours| hours * 60 * 60 >= horizon.as_secs())
        .unwrap_or(72)
}

/// `{"zone": "DK-DK2", "forecast": [{"carbonIntensity": 318, "datetime": "…"}, …]}`.
/// A point that doesn't parse fails the whole answer.
pub(crate) fn parse_forecast(answer: &serde_json::Value) -> Option<(String, Vec<Sample>)> {
    let zone = answer.get("zone")?.as_str()?.to_owned();
    let curve = answer
        .get("forecast")?
        .as_array()?
        .iter()
        .map(|point| {
            let at = DateTime::parse_from_rfc3339(point.get("datetime")?.as_str()?).ok()?;
            Some(Sample::new(at.with_timezone(&Utc), point.get("carbonIntensity")?.as_f64()?))
        })
        .collect::<Option<Vec<_>>>()?;
    Some((zone, curve))
}

/// The points up to `now + horizon`.
pub(crate) fn cut_to_horizon(curve: &[Sample], now: DateTime<Utc>, horizon: Duration) -> Vec<Sample> {
    let end = now + chrono::Duration::from_std(horizon).unwrap_or(chrono::Duration::zero());
    curve.iter().filter(|sample| *sample.observed_at() <= end).cloned().collect()
}

/// Returns the outcome and the intensity from the confirming fetch.
pub(crate) async fn probe_electricity_maps(
    id: ProviderId,
    api: Option<Api>,
    location: Option<&Location>,
) -> Option<(Outcome, f64)> {
    let api = api?;

    // Confirms the key and the location work; the first query it can read wins.
    let mut answer = None;
    for query in queries(location) {
        if let Ok(answered) = api.latest(&query).await {
            answer = Some(answered);
            break;
        }
    }
    let (zone, current) = answer?;
    let g_per_kwh = current.value();

    let sid = SourceId::new(id, 0);
    let provider = ElectricityMaps {
        base_url: api.base_url,
        key: api.key,
        query: Query::Zone(zone.clone()),
        source: sid,
        // The confirming fetch above answers the first read.
        primed: Some(current),
        last: None,
    };
    let description =
        SourceDescription::Intensity { location: GridLocation::Zone { zone_id: zone }, fidelity: Fidelity::Measured };
    Some((Outcome::single(sid, description, provider), g_per_kwh))
}

/// The latest intensity of one zone.
struct ElectricityMaps {
    base_url: String,
    key: String,
    query: Query,
    source: SourceId,
    /// The probe's confirming fetch, answered once instead of refetching.
    primed: Option<Sample>,
    /// When the last measurement reported was for.
    last: Option<Timestamp>,
}

impl Provider for ElectricityMaps {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let sample = match self.primed.take() {
            Some(sample) => sample,
            None => parse_latest(&blocking_get(&self.base_url, &self.key, "/carbon-intensity/latest", &self.query.params())?)?.1,
        };
        let mut response = ProviderResponse::new();
        // The same measurement as last time is no new observation.
        if self.last.replace(*sample.observed_at()) == Some(*sample.observed_at()) {
            response.insert(self.source, Reading::Unchanged);
        } else {
            response.samples(self.source, vec![sample]);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(ELECTRICITY_MAPS_COOLDOWN)
    }
}

/// What the forecast probe confirmed: the zone answered, the curve it
/// fetched, how far the curve reaches, and how to fetch it again.
pub(crate) struct ForecastProbe {
    pub(crate) zone: String,
    pub(crate) curve: Vec<Sample>,
    pub(crate) horizon: Duration,
    pub(crate) fetch: Fetch,
}

/// A forecast for the zone the location maps to, trying the same queries in
/// the same order as the intensity. Declines when the key's plan reaches none
/// of them, or the answer has no point ahead.
pub(crate) async fn probe_electricity_maps_forecast(
    api: Option<Api>,
    location: Option<&Location>,
    horizon: Duration,
) -> Option<ForecastProbe> {
    let api = api?;
    // The endpoint reaches 72 hours at most.
    let horizon = horizon.min(Duration::from_secs(72 * 60 * 60));

    let mut answer = None;
    for query in queries(location) {
        if let Ok(answered) = api.forecast(&query, horizon).await {
            answer = Some(answered);
            break;
        }
    }
    let (zone, curve) = answer?;
    let now = Utc::now();
    let reach = (*curve.last()?.observed_at() - now).to_std().ok().filter(|reach| !reach.is_zero())?;

    let (base_url, key, query) = (api.base_url.clone(), api.key.clone(), Query::Zone(zone.clone()));
    let fetch: Fetch = Arc::new(move || {
        let (_, curve) = fetch_forecast(&base_url, &key, &query, horizon).ok()?;
        Some(cut_to_horizon(&curve, Utc::now(), horizon))
    });
    Some(ForecastProbe { zone, curve: cut_to_horizon(&curve, now, horizon), horizon: horizon.min(reach), fetch })
}

/// Fetch the forecast for `query`, blocking.
fn fetch_forecast(
    base_url: &str,
    key: &str,
    query: &Query,
    horizon: Duration,
) -> Result<(String, Vec<Sample>), ProviderError> {
    let mut params = query.params();
    params.push(("horizonHours", horizon_hours(horizon).to_string()));
    let answer = blocking_get(base_url, key, "/carbon-intensity/forecast", &params)?;
    parse_forecast(&answer).ok_or_else(|| ProviderError::Invalid("a forecast that does not parse".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::location::{Coordinates, CountryCode, Estimate};

    fn zone(id: &str) -> Query {
        Query::Zone(id.to_owned())
    }

    fn copenhagen() -> Query {
        Query::Coordinates { latitude: 55.68, longitude: 12.57 }
    }

    fn stated(location: GridLocation) -> Vec<Query> {
        queries(Some(&Location::Stated(location)))
    }

    fn estimated(coordinates: Option<Coordinates>) -> Vec<Query> {
        let estimate = Estimate { country: CountryCode::new("DK").unwrap(), coordinates, by: "test" };
        queries(Some(&Location::Estimated(estimate)))
    }

    /// AC/PROV-10
    #[test]
    fn every_location_maps_to_its_queries_most_precise_first() {
        assert_eq!(stated(GridLocation::Zone { zone_id: "DK-DK2".into() }), vec![zone("DK-DK2"), zone("DK")]);
        assert_eq!(stated(GridLocation::Zone { zone_id: "DK".into() }), vec![zone("DK")]);
        assert_eq!(stated(GridLocation::Country { country_code: "dk".into() }), vec![zone("DK")]);
        assert_eq!(stated(GridLocation::Coordinates { latitude: 55.68, longitude: 12.57 }), vec![copenhagen()]);
        assert_eq!(stated(GridLocation::DataCenter { cloud_provider: "aws".into(), region: "eu-north-1".into() }), vec![]);
        assert_eq!(stated(GridLocation::Global), vec![]);
        assert_eq!(estimated(Coordinates::new(55.68, 12.57)), vec![copenhagen(), zone("DK")]);
        assert_eq!(estimated(None), vec![zone("DK")]);
        assert_eq!(queries(None), vec![]);
    }

    #[test]
    fn every_query_disables_caller_lookup_and_asks_for_lifecycle() {
        for query in [Query::Zone("DK".into()), Query::Coordinates { latitude: 1.0, longitude: 2.0 }] {
            let params = query.params();
            assert!(params.contains(&("disableCallerLookup", "true".to_owned())), "{query:?}");
            assert!(params.contains(&("emissionFactorType", "lifecycle".to_owned())), "{query:?}");
        }
    }

    #[test]
    fn the_forecast_asks_for_the_smallest_horizon_that_covers() {
        let hours = |hours: u64| horizon_hours(Duration::from_secs(hours * 60 * 60));
        assert_eq!(horizon_hours(Duration::from_secs(60)), 6);
        assert_eq!(hours(6), 6);
        assert_eq!(hours(7), 24);
        assert_eq!(hours(24), 24);
        assert_eq!(hours(30), 48);
        assert_eq!(hours(72), 72);
        assert_eq!(hours(100), 72);
    }

    #[test]
    fn a_forecast_answer_parses_into_a_curve() {
        let answer = serde_json::json!({
            "zone": "DK-DK2",
            "forecast": [
                {"carbonIntensity": 120, "datetime": "2026-10-24T12:00:00.000Z"},
                {"carbonIntensity": 140.5, "datetime": "2026-10-24T13:00:00.000Z"},
            ],
        });
        let (zone, curve) = parse_forecast(&answer).unwrap();
        assert_eq!(zone, "DK-DK2");
        assert_eq!(curve.iter().map(Sample::value).collect::<Vec<_>>(), vec![120.0, 140.5]);

        let broken = serde_json::json!({"zone": "DK", "forecast": [{"carbonIntensity": 1, "datetime": "noon"}]});
        assert!(parse_forecast(&broken).is_none());
        assert!(parse_forecast(&serde_json::json!({"forecast": []})).is_none());
    }

    #[test]
    fn a_curve_is_cut_to_the_horizon() {
        let now = Utc::now();
        let curve: Vec<Sample> =
            (0..48).map(|hour| Sample::new(now + chrono::Duration::hours(hour), hour as f64)).collect();
        let cut = cut_to_horizon(&curve, now, Duration::from_secs(6 * 60 * 60));
        assert_eq!(cut.len(), 7, "hours 0 to 6 inclusive");
    }
}
