//! The Energinet candidates against a fake Energi Data Service serving
//! recorded answers: offline, and the fake's request log shows what was sent.

use std::time::Duration;

use carbontracker::{
    Cooldown, Fidelity, GridLocation, ProviderError, ProviderId, ProviderResponse, Reading, SampleKind,
    SourceDescription, SourceId,
};
use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::adapters::intensity::electricity_maps::ForecastProbe;
use crate::adapters::intensity::energinet::{probe_energinet, probe_energinet_forecast, ENERGINET};
use crate::domain::location::{Coordinates, CountryCode, Estimate, Location};
use crate::ports::{Outcome, Probed};

const ID: ProviderId = ProviderId(99);
const CO2EMIS: &str = include_str!("fixtures/energinet/co2emis.json");
const CO2EMISPROG: &str = include_str!("fixtures/energinet/co2emisprog.json");
const RATE_LIMITED: &str = include_str!("fixtures/energinet/rate_limited.json");
pub(super) const CO2EMIS_PATH: &str = "/dataset/CO2Emis";
pub(super) const CO2EMISPROG_PATH: &str = "/dataset/CO2EmisProg";
const HOUR: Duration = Duration::from_secs(60 * 60);

fn ok(body: impl Into<String>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(body.into())
}

fn zone(id: &str) -> Location {
    Location::Stated(GridLocation::Zone { zone_id: id.to_owned() })
}

fn country(code: &str) -> Location {
    Location::Stated(GridLocation::Country { country_code: code.to_owned() })
}

fn coordinates(latitude: f64, longitude: f64) -> Location {
    Location::Stated(GridLocation::Coordinates { latitude, longitude })
}

fn estimated(code: &str, at: Option<(f64, f64)>) -> Location {
    Location::Estimated(Estimate {
        country: CountryCode::new(code).unwrap(),
        coordinates: at.and_then(|(latitude, longitude)| Coordinates::new(latitude, longitude)),
        by: "test",
    })
}

/// The start of the five minutes `now` lies in.
fn this_interval() -> DateTime<Utc> {
    Utc::now().duration_trunc(TimeDelta::minutes(5)).unwrap()
}

/// `fixture` with its records restamped five minutes apart from `first`,
/// `step` intervals each, and its price area set to `area`.
fn restamped(fixture: &str, area: &str, first: DateTime<Utc>, step: i64) -> serde_json::Value {
    let mut answer: serde_json::Value = serde_json::from_str(fixture).unwrap();
    for (index, record) in answer["records"].as_array_mut().unwrap().iter_mut().enumerate() {
        let at = first + TimeDelta::minutes(5 * step * index as i64);
        record["Minutes5UTC"] = at.format("%Y-%m-%dT%H:%M:%S").to_string().into();
        record["PriceArea"] = area.into();
    }
    answer
}

/// The CO2Emis fixture, latest first, its latest figure for the interval now.
pub(super) fn latest_answer(area: &str) -> serde_json::Value {
    restamped(CO2EMIS, area, this_interval(), -1)
}

/// The CO2EmisProg fixture, starting with the interval now.
pub(super) fn forecast_answer(area: &str) -> serde_json::Value {
    restamped(CO2EMISPROG, area, this_interval(), 1)
}

async fn probe(server: &MockServer, location: Option<Location>) -> Probed<(Outcome, f64)> {
    probe_energinet(ID, &reqwest::Client::new(), &server.uri(), location.as_ref()).await
}

async fn probe_forecast(server: &MockServer, location: Option<Location>, horizon: Duration) -> Probed<ForecastProbe> {
    probe_energinet_forecast(&reqwest::Client::new(), &server.uri(), location.as_ref(), horizon).await
}

/// Answers `endpoint` for `area` only.
pub(super) async fn serve(server: &MockServer, endpoint: &str, area: &str, answer: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(query_param("filter", format!(r#"{{"PriceArea":["{area}"]}}"#)))
        .respond_with(answer)
        .mount(server)
        .await;
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

fn description(area: &str) -> SourceDescription {
    SourceDescription::Intensity {
        location: GridLocation::Zone { zone_id: format!("DK-{area}") },
        fidelity: Fidelity::Measured,
    }
}

/// Reads of `outcome`'s provider, off the async runtime, as the engine makes them.
async fn read(outcome: Outcome, times: usize) -> Vec<Result<ProviderResponse, ProviderError>> {
    let mut provider = outcome.provider;
    tokio::task::spawn_blocking(move || (0..times).map(|_| provider.read()).collect()).await.unwrap()
}

/// AC/OPEN-2 AC/OPEN-8 AC/OPEN-17
#[tokio::test]
async fn a_stated_danish_zone_is_served_as_measured_from_co2emis() {
    for area in ["DK1", "DK2"] {
        let server = MockServer::start().await;
        serve(&server, CO2EMIS_PATH, area, ok(latest_answer(area).to_string())).await;

        let (outcome, g_per_kwh) = probe(&server, Some(zone(&format!("DK-{area}")))).await.expect("serves");

        assert_eq!(g_per_kwh, 90.0, "the fixture's latest figure");
        assert_eq!(outcome.descriptions, vec![(SourceId::new(ID, 0), description(area))]);
        assert_eq!(outcome.descriptions[0].1.sample_kind(), SampleKind::Held);
    }
}

/// AC/OPEN-6 AC/OPEN-9
#[tokio::test]
async fn a_location_outside_its_grids_is_declined_without_a_request() {
    let server = MockServer::start().await;
    let outside = [
        Some(zone("SE-SE4")),
        Some(zone("DE")),
        Some(country("SE")),
        Some(coordinates(55.60, 13.00)),  // Malmö
        Some(coordinates(56.05, 12.70)),  // Helsingborg
        Some(coordinates(54.79, 9.43)),   // Flensburg
        Some(coordinates(54.95, 8.35)),   // Sylt
        Some(coordinates(48.86, 2.35)),   // Paris
        Some(estimated("SE", Some((55.60, 13.00)))),
        Some(Location::Stated(GridLocation::Global)),
        Some(Location::Stated(GridLocation::DataCenter { cloud_provider: "aws".into(), region: "eu-north-1".into() })),
        None,
    ];
    for location in outside {
        assert!(probe(&server, location.clone()).await.is_err(), "{location:?}");
        assert!(probe_forecast(&server, location.clone(), HOUR).await.is_err(), "{location:?}");
    }
    assert!(requests(&server).await.is_empty(), "declined without asking");
}

/// AC/OPEN-9
#[tokio::test]
async fn denmark_as_a_country_is_declined_as_two_grids() {
    let server = MockServer::start().await;
    for location in [country("DK"), country("dk"), zone("DK"), estimated("DK", None)] {
        let reason = probe(&server, Some(location.clone())).await.err().expect("declined");
        assert!(reason.contains("two grids"), "{location:?}: {reason}");
        assert!(probe_forecast(&server, Some(location), HOUR).await.is_err());
    }
    assert!(requests(&server).await.is_empty(), "declined without asking");
}

/// AC/OPEN-10
#[tokio::test]
async fn coordinates_are_read_as_the_grid_they_lie_in() {
    for (location, area) in [
        (coordinates(55.68, 12.57), "DK2"),
        (estimated("DK", Some((55.68, 12.57))), "DK2"),
        (coordinates(55.40, 10.39), "DK1"),
        (estimated("DK", Some((56.16, 10.20))), "DK1"),
    ] {
        let server = MockServer::start().await;
        serve(&server, CO2EMIS_PATH, area, ok(latest_answer(area).to_string())).await;
        serve(&server, CO2EMISPROG_PATH, area, ok(forecast_answer(area).to_string())).await;

        let (outcome, _) = probe(&server, Some(location.clone())).await.expect("serves");
        assert_eq!(outcome.descriptions[0].1, description(area), "{location:?}");
        let forecast = probe_forecast(&server, Some(location.clone()), HOUR).await.expect("forecasts");
        assert_eq!(forecast.zone, format!("DK-{area}"), "{location:?}");
    }
}

/// AC/OPEN-7
#[tokio::test]
async fn a_request_carries_no_key_and_nothing_finer_than_the_grid() {
    let server = MockServer::start().await;
    serve(&server, CO2EMIS_PATH, "DK2", ok(latest_answer("DK2").to_string())).await;
    serve(&server, CO2EMISPROG_PATH, "DK2", ok(forecast_answer("DK2").to_string())).await;
    let copenhagen = estimated("DK", Some((55.6759, 12.5655)));

    probe(&server, Some(copenhagen.clone())).await.expect("serves");
    probe_forecast(&server, Some(copenhagen), HOUR).await.expect("forecasts");

    let sent = requests(&server).await;
    assert_eq!(sent.len(), 2);
    for request in sent {
        for header in ["auth-token", "authorization", "x-api-key", "cookie"] {
            assert!(!request.headers.contains_key(header), "{header} sent");
        }
        let query = request.url.query().unwrap_or_default();
        assert!(!query.contains("55.67") && !query.contains("12.56"), "coordinates sent: {query}");
        for (name, value) in request.url.query_pairs() {
            assert!(["filter", "start", "end", "sort", "limit"].contains(&name.as_ref()), "{name}={value}");
            if name == "filter" {
                assert_eq!(value, r#"{"PriceArea":["DK2"]}"#);
            }
        }
    }
}

/// AC/OPEN-14
#[tokio::test]
async fn each_read_takes_the_latest_figure_once() {
    let server = MockServer::start().await;
    let start = this_interval();
    serve(&server, CO2EMIS_PATH, "DK2", ok(restamped(CO2EMIS, "DK2", start, -1).to_string())).await;
    let (outcome, _) = probe(&server, Some(zone("DK-DK2"))).await.expect("serves");
    let source = SourceId::new(ID, 0);

    let reads = read(outcome, 2).await;
    let first = reads[0].as_ref().unwrap();
    assert!(
        matches!(first.get(source), Some(Reading::Samples(samples)) if *samples[0].observed_at() == start),
        "the probe's fetch answers the first read, stamped at the start of its interval: {first:?}"
    );
    assert_eq!(reads[1].as_ref().unwrap().get(source), Some(&Reading::Unchanged), "the same stamp is nothing new");
    assert_eq!(requests(&server).await.len(), 2, "the probe's, and one read");
}

/// AC/OPEN-15
#[tokio::test]
async fn a_figure_for_an_interval_still_to_come_is_never_read() {
    let server = MockServer::start().await;
    let mut answer = latest_answer("DK2");
    let ahead = (this_interval() + TimeDelta::minutes(10)).format("%Y-%m-%dT%H:%M:%S").to_string();
    let future = serde_json::json!({"Minutes5UTC": ahead, "PriceArea": "DK2", "CO2Emission": 500.0});
    answer["records"].as_array_mut().unwrap().insert(0, future);
    serve(&server, CO2EMIS_PATH, "DK2", ok(answer.to_string())).await;

    let (_, g_per_kwh) = probe(&server, Some(zone("DK-DK2"))).await.expect("serves");

    assert_eq!(g_per_kwh, 90.0, "the latest interval that has begun");
    let sent = &requests(&server).await[0];
    assert!(sent.url.query_pairs().any(|(name, value)| name == "end" && value == "now"), "asks for published figures");
}

/// AC/OPEN-18
#[test]
fn the_candidate_name_says_the_publishers_basis() {
    assert_eq!(ENERGINET, "energinet (direct CO2)");
}

/// AC/OPEN-19
#[tokio::test]
async fn the_live_intensity_is_read_at_most_once_a_minute() {
    let server = MockServer::start().await;
    serve(&server, CO2EMIS_PATH, "DK1", ok(latest_answer("DK1").to_string())).await;
    let (outcome, _) = probe(&server, Some(zone("DK-DK1"))).await.expect("serves");
    assert_eq!(outcome.provider.cooldown(), Cooldown::After(Duration::from_secs(60)));
}

/// AC/OPEN-23
#[tokio::test]
async fn a_refusal_for_asking_too_often_is_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(path(CO2EMIS_PATH))
        .respond_with(ok(latest_answer("DK2").to_string()))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(path(CO2EMIS_PATH)).respond_with(ResponseTemplate::new(429).set_body_string(RATE_LIMITED)).mount(&server).await;
    let (outcome, _) = probe(&server, Some(zone("DK-DK2"))).await.expect("serves");

    let reads = read(outcome, 2).await;
    assert!(matches!(reads[1], Err(ProviderError::RateLimited(_))), "{:?}", reads[1]);
}

/// AC/OPEN-2 AC/OPEN-30
#[tokio::test]
async fn the_forecast_comes_from_co2emisprog_and_reaches_what_it_publishes() {
    let server = MockServer::start().await;
    serve(&server, CO2EMISPROG_PATH, "DK2", ok(forecast_answer("DK2").to_string())).await;

    let long = probe_forecast(&server, Some(zone("DK-DK2")), 48 * HOUR).await.expect("forecasts");
    assert_eq!(long.zone, "DK-DK2");
    assert_eq!(long.curve.len(), 24, "every published point");
    assert!(long.horizon < 2 * HOUR, "what the fixture reaches, under two hours: {:?}", long.horizon);
    assert!(long.horizon > HOUR, "{:?}", long.horizon);

    let short = probe_forecast(&server, Some(zone("DK-DK2")), HOUR / 2).await.expect("forecasts");
    assert_eq!(short.horizon, HOUR / 2, "what was asked for");
    assert_eq!(short.curve.len(), 7, "the points to half an hour ahead");

    let fetch = long.fetch;
    let refetched = tokio::task::spawn_blocking(move || fetch()).await.unwrap().expect("a curve");
    assert_eq!(refetched.len(), 24);
    assert!(requests(&server).await.iter().all(|request| request.url.path() == CO2EMISPROG_PATH));
}
