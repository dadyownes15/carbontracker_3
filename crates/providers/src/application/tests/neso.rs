//! NESO's Carbon Intensity API candidate, against a fake server serving
//! recorded answers: what it reads for each location, and how it reads.

use std::time::Duration;

use chrono::{DateTime, DurationRound, TimeDelta, Utc};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use carbontracker::{Cooldown, Fidelity, GridLocation, ProviderError, Reading, SourceDescription, SourceId};

use crate::adapters::intensity::neso::{probe_neso, probe_neso_forecast, Api, NESO, NESO_URL};
use crate::domain::location::{Coordinates, CountryCode, Estimate, Location};
use crate::ports::Outcome;
use carbontracker::ProviderId;

pub(super) const INTENSITY: &str = include_str!("fixtures/neso/intensity.json");
const INTENSITY_PT24H: &str = include_str!("fixtures/neso/intensity_pt24h.json");
pub(super) const INTENSITY_FW48H: &str = include_str!("fixtures/neso/intensity_fw48h.json");
const REGIONAL: &str = include_str!("fixtures/neso/regional.json");
const REGIONAL_FW48H: &str = include_str!("fixtures/neso/regional_fw48h.json");

const ID: ProviderId = ProviderId(40);
const LONDON: (f64, f64) = (51.51, -0.13);
const BELFAST: (f64, f64) = (54.60, -5.93);
const COPENHAGEN: (f64, f64) = (55.68, 12.57);

fn api(server: &MockServer) -> Api {
    Api::new(&reqwest::Client::new(), &server.uri())
}

fn ok(body: impl Into<String>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(body.into())
}

fn stated(location: GridLocation) -> Location {
    Location::Stated(location)
}

fn zone(id: &str) -> Location {
    stated(GridLocation::Zone { zone_id: id.to_owned() })
}

fn coordinates((latitude, longitude): (f64, f64)) -> Location {
    stated(GridLocation::Coordinates { latitude, longitude })
}

fn estimated(country: &str, at: Option<(f64, f64)>) -> Location {
    Location::Estimated(Estimate {
        country: CountryCode::new(country).unwrap(),
        coordinates: at.and_then(|(latitude, longitude)| Coordinates::new(latitude, longitude)),
        by: "test",
    })
}

fn at(stamp: &str) -> DateTime<Utc> {
    stamp.parse().unwrap()
}

async fn serve(server: &MockServer, endpoint: &str, answer: ResponseTemplate) {
    Mock::given(method("GET")).and(path(endpoint)).respond_with(answer).mount(server).await;
}

async fn serve_national(server: &MockServer) {
    serve(server, "/intensity", ok(INTENSITY)).await;
}

async fn serve_region(server: &MockServer, id: u8) {
    serve(server, &format!("/regional/regionid/{id}"), ok(REGIONAL)).await;
}

/// `answer` with every point moved to start this half-hour, as the live API
/// answers a forecast asked for from now.
pub(super) fn from_this_half_hour(answer: &str) -> String {
    let mut answer: serde_json::Value = serde_json::from_str(answer).unwrap();
    let start = Utc::now().duration_trunc(TimeDelta::minutes(30)).unwrap();
    let points = match answer["data"].get_mut("data") {
        Some(regional) => regional,
        None => &mut answer["data"],
    };
    for (index, point) in points.as_array_mut().unwrap().iter_mut().enumerate() {
        let from = start + TimeDelta::minutes(30 * index as i64);
        point["from"] = from.format("%Y-%m-%dT%H:%MZ").to_string().into();
        point["to"] = (from + TimeDelta::minutes(30)).format("%Y-%m-%dT%H:%MZ").to_string().into();
    }
    answer.to_string()
}

fn described(outcome: &Outcome) -> SourceDescription {
    outcome.descriptions[0].1.clone()
}

/// Two reads, off the async runtime, as the engine makes them.
async fn read_twice(outcome: Outcome) -> [Result<carbontracker::ProviderResponse, ProviderError>; 2] {
    let mut provider = outcome.provider;
    tokio::task::spawn_blocking(move || [provider.read(), provider.read()]).await.unwrap()
}

fn sample_of(response: &carbontracker::ProviderResponse) -> (DateTime<Utc>, f64) {
    match response.get(SourceId::new(ID, 0)) {
        Some(Reading::Samples(samples)) => (*samples[0].observed_at(), samples[0].value()),
        other => panic!("one sample: {other:?}"),
    }
}

async fn declined(location: Option<&Location>) -> (String, usize) {
    let server = MockServer::start().await;
    let reason = match probe_neso(ID, api(&server), location).await {
        Err(reason) => reason,
        Ok(_) => panic!("{location:?} is served"),
    };
    (reason, server.received_requests().await.unwrap().len())
}

/// AC/OPEN-3 AC/OPEN-14 AC/OPEN-15 AC/OPEN-17
#[tokio::test]
async fn gb_is_read_from_the_latest_national_actual_and_reported_once() {
    let server = MockServer::start().await;
    serve_national(&server).await;

    let (outcome, g_per_kwh) = probe_neso(ID, api(&server), Some(&zone("GB"))).await.unwrap();

    assert_eq!(g_per_kwh, 166.0, "the fixture's actual");
    assert_eq!(
        described(&outcome),
        SourceDescription::Intensity {
            location: GridLocation::Zone { zone_id: "GB".to_owned() },
            fidelity: Fidelity::Measured
        }
    );
    let [first, second] = read_twice(outcome).await;
    assert_eq!(sample_of(&first.unwrap()), (at("2026-11-19T23:30:00Z"), 166.0), "stamped at the half-hour's start");
    assert_eq!(second.unwrap().get(SourceId::new(ID, 0)), Some(&Reading::Unchanged));
    assert_eq!(server.received_requests().await.unwrap().len(), 2, "the probe's, and one read");
}

/// AC/OPEN-15
#[tokio::test]
async fn a_national_half_hour_without_an_actual_gives_way_to_the_last_with_one() {
    let server = MockServer::start().await;
    let mut no_actual: serde_json::Value = serde_json::from_str(INTENSITY).unwrap();
    no_actual["data"][0]["intensity"]["actual"] = serde_json::Value::Null;
    serve(&server, "/intensity", ok(no_actual.to_string())).await;
    Mock::given(method("GET")).and(path_regex(r"^/intensity/[^/]+/pt24h$")).respond_with(ok(INTENSITY_PT24H)).mount(&server).await;

    let (outcome, g_per_kwh) = probe_neso(ID, api(&server), Some(&zone("GB"))).await.unwrap();

    assert_eq!(g_per_kwh, 168.0, "the fixture's last actual, not a forecast");
    let [first, _] = read_twice(outcome).await;
    assert_eq!(sample_of(&first.unwrap()), (at("2026-11-19T23:00:00Z"), 168.0));
}

/// AC/OPEN-8 AC/OPEN-38 AC/OPEN-37
#[tokio::test]
async fn a_region_is_read_from_the_current_half_hours_forecast() {
    let server = MockServer::start().await;
    serve_region(&server, 13).await;

    let (outcome, g_per_kwh) = probe_neso(ID, api(&server), Some(&zone("GB-NESO-13"))).await.unwrap();

    assert_eq!(g_per_kwh, 202.0, "the fixture's forecast");
    assert_eq!(
        described(&outcome),
        SourceDescription::Intensity {
            location: GridLocation::Zone { zone_id: "GB-NESO-13".to_owned() },
            fidelity: Fidelity::Measured
        }
    );
    let [first, second] = read_twice(outcome).await;
    assert_eq!(sample_of(&first.unwrap()), (at("2026-11-19T23:30:00Z"), 202.0));
    assert_eq!(second.unwrap().get(SourceId::new(ID, 0)), Some(&Reading::Unchanged));
}

/// AC/OPEN-37
#[tokio::test]
async fn a_region_id_outside_1_to_17_is_declined_saying_which() {
    for id in ["0", "18", "london"] {
        let (reason, sent) = declined(Some(&zone(&format!("GB-NESO-{id}")))).await;
        assert!(reason.contains(&format!("GB-NESO-{id}")) && reason.contains("1 to 17"), "{reason:?}");
        assert_eq!(sent, 0);
    }
}

/// AC/OPEN-6
#[tokio::test]
async fn a_location_off_the_gb_grid_is_declined_without_a_request() {
    let elsewhere = [
        zone("DK-DK2"),
        stated(GridLocation::Country { country_code: "DK".into() }),
        coordinates(COPENHAGEN),
        estimated("DK", Some(COPENHAGEN)),
        estimated("FR", None),
        stated(GridLocation::Global),
    ];
    for location in elsewhere {
        let (reason, sent) = declined(Some(&location)).await;
        assert!(reason.contains("Great Britain"), "{location:?}: {reason:?}");
        assert_eq!(sent, 0, "{location:?}");
    }
    let (_, sent) = declined(None).await;
    assert_eq!(sent, 0);
}

/// AC/OPEN-9 AC/OPEN-10
#[tokio::test]
async fn a_country_or_coordinates_in_great_britain_read_gb_national() {
    let in_britain = [
        stated(GridLocation::Country { country_code: "gb".into() }),
        coordinates(LONDON),
        estimated("GB", Some(LONDON)),
        estimated("GB", None),
    ];
    for location in in_britain {
        let server = MockServer::start().await;
        serve_national(&server).await;

        let (outcome, _) = probe_neso(ID, api(&server), Some(&location)).await.unwrap();

        assert!(
            matches!(described(&outcome), SourceDescription::Intensity { location: GridLocation::Zone { ref zone_id }, .. } if zone_id == "GB"),
            "{location:?}"
        );
    }
}

/// AC/OPEN-12
#[tokio::test]
async fn northern_ireland_is_declined_as_off_the_gb_grid() {
    for location in [zone("GB-NIR"), coordinates(BELFAST), estimated("GB", Some(BELFAST))] {
        let (reason, sent) = declined(Some(&location)).await;
        assert!(reason.contains("Northern Ireland"), "{location:?}: {reason:?}");
        assert_eq!(sent, 0, "{location:?}");
    }
}

/// AC/OPEN-7
#[tokio::test]
async fn nothing_sent_identifies_the_user() {
    let server = MockServer::start().await;
    serve_national(&server).await;
    serve(&server, "/regional/regionid/13", ok(REGIONAL)).await;

    probe_neso(ID, api(&server), Some(&estimated("GB", Some(LONDON)))).await.unwrap();
    probe_neso(ID, api(&server), Some(&zone("GB-NESO-13"))).await.unwrap();

    for request in server.received_requests().await.unwrap() {
        assert_eq!(request.url.query(), None, "{}", request.url);
        assert!(!request.url.path().contains("51.5"), "no coordinates: {}", request.url);
        for name in request.headers.keys() {
            assert!(!["authorization", "auth-token", "x-api-key", "cookie"].contains(&name.as_str()), "{name}");
        }
    }
}

/// AC/OPEN-20
#[tokio::test]
async fn neso_is_read_at_most_every_five_minutes() {
    let server = MockServer::start().await;
    serve_national(&server).await;

    let (outcome, _) = probe_neso(ID, api(&server), Some(&zone("GB"))).await.unwrap();

    assert_eq!(outcome.provider.cooldown(), Cooldown::After(Duration::from_secs(5 * 60)));
}

/// AC/OPEN-23
#[tokio::test]
async fn a_refusal_for_asking_too_often_is_rate_limited() {
    let server = MockServer::start().await;
    serve_national(&server).await;
    let (outcome, _) = probe_neso(ID, api(&server), Some(&zone("GB"))).await.unwrap();
    server.reset().await;
    serve(&server, "/intensity", ResponseTemplate::new(429)).await;

    let [_, second] = read_twice(outcome).await;

    assert!(matches!(second, Err(ProviderError::RateLimited(_))), "{second:?}");
    let reason = match probe_neso(ID, api(&server), Some(&zone("GB"))).await {
        Err(reason) => reason,
        Ok(_) => panic!("served"),
    };
    assert!(reason.contains("too often"), "{reason:?}");
}

/// AC/OPEN-3 AC/OPEN-30
#[tokio::test]
async fn the_national_forecast_reaches_as_far_as_neso_publishes() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/intensity/\d{4}-\d{2}-\d{2}T\d{2}:\d{2}Z/fw48h$"))
        .respond_with(ok(from_this_half_hour(INTENSITY_FW48H)))
        .mount(&server)
        .await;
    let asked = Duration::from_secs(72 * 60 * 60);

    let probed = probe_neso_forecast(api(&server), Some(&zone("GB")), asked).await.unwrap();

    assert_eq!(probed.zone, "GB");
    let values: Vec<f64> = probed.curve.iter().map(|sample| sample.value()).collect();
    assert_eq!(values, vec![169.0, 169.0, 166.0, 165.0, 162.0, 154.0, 148.0, 150.0], "the fixture's forecasts");
    // The fixture's last point starts 3.5 hours after this half-hour began.
    assert!(probed.horizon <= Duration::from_secs(7 * 30 * 60), "{:?}", probed.horizon);
    assert!(probed.horizon > Duration::from_secs(3 * 60 * 60 - 60), "{:?}", probed.horizon);

    let refetched = tokio::task::spawn_blocking(move || (probed.fetch)()).await.unwrap().expect("refetches");
    assert_eq!(refetched.first().map(|sample| sample.value()), Some(169.0));
}

/// AC/OPEN-30 AC/OPEN-38
#[tokio::test]
async fn a_regions_forecast_comes_from_its_regional_curve() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(r"^/regional/intensity/[^/]+/fw48h/regionid/13$"))
        .respond_with(ok(from_this_half_hour(REGIONAL_FW48H)))
        .mount(&server)
        .await;

    let probed = probe_neso_forecast(api(&server), Some(&zone("GB-NESO-13")), Duration::from_secs(24 * 60 * 60))
        .await
        .unwrap();

    assert_eq!(probed.zone, "GB-NESO-13");
    let values: Vec<f64> = probed.curve.iter().map(|sample| sample.value()).collect();
    assert_eq!(values, vec![197.0, 185.0, 176.0, 190.0]);
    assert!(probed.horizon <= Duration::from_secs(90 * 60), "{:?}", probed.horizon);
}

/// AC/OPEN-6 AC/OPEN-12
#[tokio::test]
async fn the_forecast_declines_what_the_intensity_declines() {
    for location in [zone("DK-DK2"), zone("GB-NIR"), zone("GB-NESO-18")] {
        let server = MockServer::start().await;
        assert!(probe_neso_forecast(api(&server), Some(&location), Duration::from_secs(3600)).await.is_err());
        assert!(server.received_requests().await.unwrap().is_empty(), "{location:?}");
    }
}

/// AC/OPEN-18
#[test]
fn each_open_source_is_named_by_its_publisher_alone() {
    assert_eq!(NESO, "neso");
    assert_eq!(crate::adapters::intensity::energy_charts::ENERGY_CHARTS, "energy-charts");
    assert_eq!(NESO_URL, "https://api.carbonintensity.org.uk");
}
