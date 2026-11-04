//! The location, intensity and forecast chains end to end, against fake
//! servers standing in for ipinfo and Electricity Maps: offline, and the
//! fakes' request logs show exactly what was sent.

use std::time::{Duration, Instant};

use chrono::{DurationRound, TimeDelta, Utc};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use carbontracker::{
    Fidelity, GramsCo2ePerKwh, GridLocation, SourceDescription, SourceKind, Watts,
};

use carbontracker::{Cooldown, Reading, SourceId};

use crate::adapters::intensity::country_average::country_intensity;
use crate::application::registry::{ELECTRICITY_MAPS, GLOBAL_AVERAGE};
use crate::application::Endpoints;
use crate::*;

const KEY: &str = "test-key";
const IPINFO: &str = include_str!("fixtures/ipinfo.json");
const LATEST: &str = include_str!("fixtures/latest.json");
const FORECAST: &str = include_str!("fixtures/forecast.json");
const LATEST_PATH: &str = "/v3/carbon-intensity/latest";
const FORECAST_PATH: &str = "/v3/carbon-intensity/forecast";
const TIMEOUT: Duration = Duration::from_millis(200);

fn config(location: Option<GridLocation>, key: bool) -> ProbeConfig {
    ProbeConfig {
        grid: GridConfig {
            location,
            api_keys: key.then(|| (KeyedService::ElectricityMaps, ApiKey::new(KEY))).into_iter().collect(),
            ..GridConfig::default()
        },
        // Only the grid varies.
        pinned: Pinned { power: Some(Watts::new(10.0).unwrap()), ..Pinned::default() },
        ..ProbeConfig::default()
    }
}

async fn probe(server: &MockServer, config: ProbeConfig) -> Resolution {
    let endpoints = Endpoints {
        ip_geolocation: format!("{}/json", server.uri()),
        electricity_maps: format!("{}/v3", server.uri()),
        timeout: TIMEOUT,
    };
    ProviderRegistry::new(config).unwrap().with_endpoints(endpoints).probe().await
}

fn verdict(resolution: &Resolution, name: &str) -> Verdict {
    resolution
        .candidates()
        .iter()
        .find(|outcome| outcome.name == name)
        .map(|outcome| outcome.verdict.clone())
        .unwrap_or_else(|| panic!("{name} is recorded: {resolution}"))
}

fn serving(resolution: &Resolution, name: &str) -> bool {
    matches!(verdict(resolution, name), Verdict::Serving(_))
}

/// The single intensity, and the forecast; panics on two intensities.
fn grid_sources(resolution: &Resolution) -> (SourceDescription, Forecaster) {
    let found: Vec<_> =
        resolution.sources().filter(|(_, s)| s.kind() == SourceKind::Intensity).map(|(_, s)| s.clone()).collect();
    assert_eq!(found.len(), 1, "one intensity per run: {found:?}");
    let forecaster = resolution.forecaster.clone().expect("the forecast chain ran");
    (found.into_iter().next().unwrap(), forecaster)
}

fn zone(id: &str) -> GridLocation {
    GridLocation::Zone { zone_id: id.to_owned() }
}

fn country(code: &str) -> GridLocation {
    GridLocation::Country { country_code: code.to_owned() }
}

fn location_of(description: &SourceDescription) -> &GridLocation {
    match description {
        SourceDescription::Intensity { location, .. } => location,
        SourceDescription::Power { .. } => panic!("not a grid source"),
    }
}

/// The forecast fixture with its points moved to start this hour, as the
/// live API answers; `zone` replaces the fixture's.
fn forecast_answer(zone: &str) -> String {
    let mut answer: serde_json::Value = serde_json::from_str(FORECAST).unwrap();
    let hour = Utc::now().duration_trunc(TimeDelta::hours(1)).unwrap();
    for (index, point) in answer["forecast"].as_array_mut().unwrap().iter_mut().enumerate() {
        point["datetime"] = (hour + TimeDelta::hours(index as i64)).to_rfc3339().into();
    }
    answer["zone"] = zone.into();
    answer.to_string()
}

fn latest_answer(zone: &str) -> String {
    let mut answer: serde_json::Value = serde_json::from_str(LATEST).unwrap();
    answer["zone"] = zone.into();
    answer.to_string()
}

fn ok(body: impl Into<String>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(body.into())
}

async fn serve_ipinfo(server: &MockServer) {
    Mock::given(method("GET")).and(path("/json")).respond_with(ok(IPINFO)).mount(server).await;
}

/// Electricity Maps answering only a query for the fixture's coordinates,
/// with the key, caller lookup off and lifecycle factors.
async fn serve_by_coordinates(server: &MockServer, endpoint: &str, answer: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(endpoint))
        .and(header("auth-token", KEY))
        .and(query_param("lat", "55.6759"))
        .and(query_param("lon", "12.5655"))
        .and(query_param("disableCallerLookup", "true"))
        .and(query_param("emissionFactorType", "lifecycle"))
        .respond_with(answer)
        .mount(server)
        .await;
}

async fn requests_to(server: &MockServer, endpoint: &str) -> usize {
    server.received_requests().await.unwrap().iter().filter(|request| request.url.path() == endpoint).count()
}

/// The forecast, as it stands now.
async fn forecast(forecaster: &Forecaster) -> Option<ForecastReport> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    forecaster.ask(move |report| {
        let _ = reply.send(report);
    });
    answer.await.expect("every ask is answered")
}

/// AC/PROV-6 AC/PROV-7 AC/PROV-8 AC/PROV-17 AC/PROV-18 AC/PROV-19
#[tokio::test]
async fn no_config_with_a_key_bills_and_forecasts_the_estimated_zone() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ok(forecast_answer("DK-DK2"))).await;

    let resolution = probe(&server, config(None, true)).await;

    assert!(matches!(resolution.location(), Some(Location::Estimated(estimate)) if estimate.country.as_str() == "DK"));
    assert!(serving(&resolution, "ip-geolocation"));
    assert!(serving(&resolution, "electricity-maps"));
    assert_eq!(verdict(&resolution, "country-average"), Verdict::Covered);
    assert_eq!(verdict(&resolution, "global-average"), Verdict::Covered);
    assert!(serving(&resolution, "electricity-maps-forecast"));
    assert_eq!(verdict(&resolution, "projection"), Verdict::Covered);

    let (intensity, forecast) = grid_sources(&resolution);
    assert_eq!(intensity, SourceDescription::Intensity { location: zone("DK-DK2"), fidelity: Fidelity::Measured });
    assert_eq!((forecast.location(), forecast.fidelity()), (&zone("DK-DK2"), Fidelity::Measured));
    // The answer reaches less far than the configured day.
    assert!(forecast.horizon() < GridConfig::default().forecast_horizon, "{:?}", forecast.horizon());

    let asked = server.received_requests().await.unwrap();
    let forecast_query = asked.iter().find(|request| request.url.path() == FORECAST_PATH).unwrap();
    assert!(forecast_query.url.query_pairs().any(|(key, value)| key == "horizonHours" && value == "24"));
}

/// AC/PROV-7 AC/PROV-8 AC/PROV-17 AC/PROV-46
#[tokio::test]
async fn no_config_and_no_key_bills_the_estimated_country_average() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;

    let resolution = probe(&server, config(None, false)).await;

    assert_eq!(verdict(&resolution, "electricity-maps"), Verdict::Declined);
    assert!(serving(&resolution, "country-average"));
    assert_eq!(verdict(&resolution, "electricity-maps-forecast"), Verdict::Declined);
    assert_eq!(verdict(&resolution, "projection"), Verdict::Serving(vec!["modelled forecast for DK, 24 h".to_owned()]));

    let (intensity, forecast) = grid_sources(&resolution);
    assert_eq!(intensity, SourceDescription::Intensity { location: country("DK"), fidelity: Fidelity::Modelled });
    assert_eq!(forecast.location(), &country("DK"), "the projection is on the intensity's grid");
    assert_eq!(requests_to(&server, LATEST_PATH).await + requests_to(&server, FORECAST_PATH).await, 0);
}

/// AC/PROV-4
#[tokio::test]
async fn a_stated_location_is_never_estimated() {
    let server = MockServer::start().await;

    let resolution = probe(&server, config(Some(zone("DK-DK2")), false)).await;

    assert!(matches!(resolution.location(), Some(Location::Stated(_))));
    assert!(serving(&resolution, "stated-location"));
    assert_eq!(verdict(&resolution, "ip-geolocation"), Verdict::NotWanted);
    assert!(serving(&resolution, "country-average"), "DK-DK2 is in Denmark");
    assert!(server.received_requests().await.unwrap().is_empty(), "nothing went out");
}

/// AC/PROV-4 AC/PROV-15 AC/PROV-16 AC/PROV-17 AC/PROV-18
#[tokio::test]
async fn a_stated_global_or_pinned_intensity_touches_no_network() {
    let mut pinned = config(None, true);
    pinned.pinned.intensity = Some(GramsCo2ePerKwh::new(100.0).unwrap());
    let mut pinned_and_stated = pinned.clone();
    pinned_and_stated.grid.location = Some(zone("DK-DK2"));

    // The global average held flat is assumed; a pinned figure is stated.
    let cases = [
        (config(Some(GridLocation::Global), true), Fidelity::Assumed),
        (pinned, Fidelity::Stated),
        (pinned_and_stated, Fidelity::Stated),
    ];
    for (config, fidelity) in cases {
        let server = MockServer::start().await;
        let resolution = probe(&server, config).await;

        assert_eq!(verdict(&resolution, "ip-geolocation"), Verdict::NotWanted);
        assert!(server.received_requests().await.unwrap().is_empty(), "{resolution}");
        let (intensity, forecast) = grid_sources(&resolution);
        assert_eq!(forecast.location(), location_of(&intensity), "the forecast is for the billed grid");
        assert_eq!(forecast.fidelity(), fidelity);
    }
}

/// AC/PROV-6 AC/PROV-7 AC/PROV-9 AC/PROV-46
#[tokio::test]
async fn an_ip_service_that_is_slow_failing_or_countryless_falls_to_the_global_average() {
    for answer in [
        ok(IPINFO).set_delay(Duration::from_secs(5)),
        ResponseTemplate::new(500),
        ok(r#"{"ip":"127.0.0.1","bogon":true}"#),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/json")).respond_with(answer).mount(&server).await;

        let started = Instant::now();
        let mut resolution = probe(&server, config(None, true)).await;

        assert!(started.elapsed() < Duration::from_secs(2), "the timeout bounds probing");
        assert_eq!(resolution.location(), None);
        assert_eq!(verdict(&resolution, "ip-geolocation"), Verdict::Declined);
        assert!(serving(&resolution, "global-average"));
        assert!(serving(&resolution, "projection"));

        let (intensity, _) = grid_sources(&resolution);
        assert_eq!(intensity, SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Assumed });
        let mut global = resolution.providers.remove(&GLOBAL_AVERAGE).expect("the global average serves");
        let read = global.read().unwrap();
        assert!(
            matches!(read.get(SourceId::new(GLOBAL_AVERAGE, 0)), Some(Reading::Samples(samples)) if samples[0].value() == 475.0),
            "{read:?}"
        );
    }
}

/// AC/PROV-7 AC/PROV-8
#[tokio::test]
async fn a_refused_key_falls_to_the_country_average() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ResponseTemplate::new(401)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ResponseTemplate::new(401)).await;

    let resolution = probe(&server, config(None, true)).await;

    assert_eq!(verdict(&resolution, "electricity-maps"), Verdict::Declined);
    assert!(serving(&resolution, "country-average"));
    assert_eq!(verdict(&resolution, "electricity-maps-forecast"), Verdict::Declined);
    assert!(serving(&resolution, "projection"));
}

/// A free key reaches one country's zone but not its sub-zones.
/// AC/PROV-10
#[tokio::test]
async fn a_key_refused_the_precise_zone_falls_back_to_the_country_zone() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ResponseTemplate::new(401)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ResponseTemplate::new(401)).await;
    Mock::given(path(LATEST_PATH))
        .and(query_param("zone", "DK"))
        .and(query_param("disableCallerLookup", "true"))
        .respond_with(ok(latest_answer("DK")))
        .mount(&server)
        .await;
    Mock::given(path(FORECAST_PATH)).and(query_param("zone", "DK")).respond_with(ok(forecast_answer("DK"))).mount(&server).await;

    let resolution = probe(&server, config(None, true)).await;

    assert!(serving(&resolution, "electricity-maps"), "{resolution}");
    assert!(serving(&resolution, "electricity-maps-forecast"), "{resolution}");
    let (intensity, forecast) = grid_sources(&resolution);
    assert_eq!(intensity, SourceDescription::Intensity { location: zone("DK"), fidelity: Fidelity::Measured });
    assert_eq!(forecast.location(), &zone("DK"));
}

/// AC/PROV-8
#[tokio::test]
async fn a_key_without_forecast_access_gets_the_projection() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ResponseTemplate::new(403)).await;

    let resolution = probe(&server, config(None, true)).await;

    assert!(serving(&resolution, "electricity-maps"));
    assert_eq!(verdict(&resolution, "electricity-maps-forecast"), Verdict::Declined);
    assert!(serving(&resolution, "projection"));
    let (_, forecast) = grid_sources(&resolution);
    assert_eq!(forecast.location(), &zone("DK-DK2"));
}

/// AC/PROV-10
#[tokio::test]
async fn a_country_without_its_own_zone_falls_to_the_country_average() {
    let server = MockServer::start().await;
    Mock::given(path(LATEST_PATH)).and(query_param("zone", "CL")).respond_with(ResponseTemplate::new(404)).mount(&server).await;

    let resolution = probe(&server, config(Some(country("CL")), true)).await;

    assert_eq!(verdict(&resolution, "electricity-maps"), Verdict::Declined);
    let (intensity, _) = grid_sources(&resolution);
    assert_eq!(location_of(&intensity), &country("CL"));
    assert_eq!(requests_to(&server, LATEST_PATH).await, 1);
}

/// AC/PROV-10 AC/PROV-17
#[tokio::test]
async fn a_forecast_for_another_zone_is_declined() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ok(forecast_answer("DK-DK1"))).await;

    let resolution = probe(&server, config(None, true)).await;

    assert_eq!(verdict(&resolution, "electricity-maps-forecast"), Verdict::Declined);
    assert!(serving(&resolution, "projection"));
    let (_, forecast) = grid_sources(&resolution);
    assert_eq!(forecast.location(), &zone("DK-DK2"));
}

/// AC/PROV-10
#[tokio::test]
async fn a_lowercase_country_reaches_electricity_maps_uppercased() {
    let server = MockServer::start().await;
    Mock::given(path(LATEST_PATH)).and(query_param("zone", "DK")).respond_with(ok(latest_answer("DK"))).mount(&server).await;

    let resolution = probe(&server, config(Some(country("dk")), true)).await;

    assert!(serving(&resolution, "electricity-maps"), "{resolution}");
    let (intensity, _) = grid_sources(&resolution);
    assert_eq!(location_of(&intensity), &zone("DK"));
}

/// AC/PROV-13 AC/PROV-24
#[tokio::test]
async fn the_forecast_is_served_from_its_cache() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ok(forecast_answer("DK-DK2"))).await;

    let resolution = probe(&server, config(None, true)).await;
    let (_, forecaster) = resolution.resolve();

    for _ in 0..2 {
        let report = forecast(&forecaster).await.expect("a forecast resolved");
        assert_eq!(report.location, zone("DK-DK2"));
        assert_eq!(report.curve.first().map(|sample| sample.value()), Some(301.0), "the fixture's curve");
    }
    assert_eq!(requests_to(&server, FORECAST_PATH).await, 1, "only the confirming fetch");
}

/// AC/PROV-8 AC/PROV-18
#[tokio::test]
async fn the_projection_holds_the_country_figure() {
    let server = MockServer::start().await;

    let resolution = probe(&server, config(Some(country("DK")), false)).await;
    let (_, forecaster) = resolution.resolve();
    assert_eq!(forecaster.fidelity(), Fidelity::Modelled);

    let report = forecast(&forecaster).await.expect("the projection resolves");
    let expected = country_intensity(CountryCode::new("DK").unwrap()).unwrap();
    assert!(report.curve.iter().all(|sample| sample.value() == expected));
}

#[test]
fn a_stated_data_centre_is_refused_before_probing() {
    let location = GridLocation::DataCenter { cloud_provider: "aws".into(), region: "eu-north-1".into() };
    let refused = ProviderRegistry::new(config(Some(location), true)).err();
    assert_eq!(refused, Some(ProbeConfigError::DataCenterUnsupported));
}

/// Electricity Maps' provider, from probing the fixture's zone, and its source.
async fn live_intensity(server: &MockServer) -> (Box<dyn carbontracker::Provider>, carbontracker::SourceId) {
    serve_ipinfo(server).await;
    serve_by_coordinates(server, LATEST_PATH, ok(LATEST)).await;
    let mut resolution = probe(server, config(None, true)).await;
    let provider = resolution.providers.remove(&ELECTRICITY_MAPS).expect("electricity maps serves");
    (provider, carbontracker::SourceId::new(ELECTRICITY_MAPS, 0))
}

/// Two reads of `provider`, off the async runtime, as the engine makes them.
async fn read_twice(
    mut provider: Box<dyn carbontracker::Provider>,
) -> [Result<carbontracker::ProviderResponse, carbontracker::ProviderError>; 2] {
    tokio::task::spawn_blocking(move || [provider.read(), provider.read()]).await.unwrap()
}

/// AC/PROV-13 AC/PROV-44 AC/ENG-54 AC/ENG-58 AC/MEAS-37
#[tokio::test]
async fn the_live_intensity_reports_each_measurement_once() {
    use carbontracker::Reading;
    let server = MockServer::start().await;
    Mock::given(path(LATEST_PATH)).and(query_param("zone", "DK-DK2")).respond_with(ok(LATEST)).mount(&server).await;
    let (provider, source) = live_intensity(&server).await;

    let [first, second] = read_twice(provider).await;
    let measured: chrono::DateTime<Utc> = "2026-10-24T12:00:00Z".parse().unwrap();
    let first = first.unwrap();
    assert!(
        matches!(first.get(source), Some(Reading::Samples(samples)) if *samples[0].observed_at() == measured),
        "the probe's fetch answers the first read, stamped when it was measured"
    );
    assert_eq!(second.unwrap().get(source), Some(&Reading::Unchanged), "the same measurement is nothing new");
    assert_eq!(requests_to(&server, LATEST_PATH).await, 2, "the probe's, and one read");
}

/// AC/PROV-45 AC/ENG-59 AC/ENG-60 AC/ENG-61
#[tokio::test]
async fn a_refusal_says_what_a_user_would_do_about_it() {
    for (status, refused) in [
        (401, carbontracker::ProviderError::Denied(String::new())),
        (429, carbontracker::ProviderError::RateLimited(String::new())),
        (503, carbontracker::ProviderError::Unavailable(String::new())),
    ] {
        let server = MockServer::start().await;
        Mock::given(path(LATEST_PATH))
            .and(query_param("zone", "DK-DK2"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        let (provider, _) = live_intensity(&server).await;

        let [_, second] = read_twice(provider).await;
        let error = second.expect_err("refused");
        assert_eq!(std::mem::discriminant(&error), std::mem::discriminant(&refused), "{status}: {error}");
    }
}

/// AC/PROV-20
#[tokio::test]
async fn the_forecaster_names_the_candidate_behind_it() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
    serve_by_coordinates(&server, FORECAST_PATH, ok(forecast_answer("DK-DK2"))).await;
    let mut pinned = config(None, false);
    pinned.pinned.intensity = Some(GramsCo2ePerKwh::new(100.0).unwrap());

    for (config, candidate) in
        [(config(None, true), "electricity-maps-forecast"), (config(None, false), "projection"), (pinned, "static")]
    {
        let (_, forecaster) = grid_sources(&probe(&server, config).await);
        assert_eq!(forecaster.served_by(), candidate);
    }
}

/// AC/PROV-31 AC/PROV-45
#[tokio::test]
async fn the_live_intensity_is_held_and_read_at_most_once_a_minute() {
    let server = MockServer::start().await;
    serve_ipinfo(&server).await;
    serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
    let resolution = probe(&server, config(None, true)).await;

    let (intensity, _) = grid_sources(&resolution);
    assert!(matches!(intensity, SourceDescription::Intensity { fidelity: Fidelity::Measured, .. }), "one zone, measured");
    assert_eq!(resolution.providers[&ELECTRICITY_MAPS].cooldown(), Cooldown::After(Duration::from_secs(60)));
}

/// AC/ENG-38
#[tokio::test]
async fn a_service_that_never_answers_a_read_is_given_up_on_its_own_timeout() {
    let server = MockServer::start().await;
    Mock::given(path(LATEST_PATH))
        .and(query_param("zone", "DK-DK2"))
        .respond_with(ok(LATEST).set_delay(Duration::from_secs(60)))
        .mount(&server)
        .await;
    let (provider, _) = live_intensity(&server).await;

    let started = Instant::now();
    let [_, hung] = read_twice(provider).await;
    assert!(matches!(hung, Err(carbontracker::ProviderError::Unavailable(_))), "{hung:?}");
    assert!(started.elapsed() < Duration::from_secs(10), "a few seconds: {:?}", started.elapsed());
}

/// Set in the copy of this test binary that probes, so the original can
/// watch what it writes.
const PROBING_CHILD: &str = "CARBONTRACKER_PROBING_CHILD";
const BEGIN: &str = "<<probing>>";
const END: &str = "<<probed>>";

/// AC/PROV-11
#[tokio::test]
async fn the_registry_never_prints() {
    use std::io::Write;

    if std::env::var_os(PROBING_CHILD).is_some() {
        // Every chain unpinned, against a fake ipinfo and Electricity Maps.
        let server = MockServer::start().await;
        serve_ipinfo(&server).await;
        serve_by_coordinates(&server, LATEST_PATH, ok(LATEST)).await;
        serve_by_coordinates(&server, FORECAST_PATH, ok(forecast_answer("DK-DK2"))).await;
        let mut config = config(None, true);
        config.pinned = Pinned::default();
        let endpoints = Endpoints {
            ip_geolocation: format!("{}/json", server.uri()),
            electricity_maps: format!("{}/v3", server.uri()),
            timeout: TIMEOUT,
        };
        let registry = ProviderRegistry::new(config).unwrap().with_endpoints(endpoints);

        let mark = |text: &str| {
            for mut stream in [Box::new(std::io::stdout()) as Box<dyn Write>, Box::new(std::io::stderr())] {
                writeln!(stream, "{text}").unwrap();
                stream.flush().unwrap();
            }
        };
        mark(BEGIN);
        let mut steps = 0;
        let resolution = registry.probe_with(|_| steps += 1).await;
        let _ = resolution.resolve();
        mark(END);
        assert!(steps > 0, "progress went to the callback");
        return;
    }

    let name = format!("{}::the_registry_never_prints", module_path!().split_once("::").unwrap().1);
    let output = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
        .env(PROBING_CHILD, "1")
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    for (stream, written) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
        let written = String::from_utf8_lossy(written);
        let between = written.split_once(BEGIN).and_then(|(_, rest)| rest.split_once(END)).map(|(between, _)| between);
        assert_eq!(between, Some("\n"), "{stream} while probing: {written}");
    }
}
