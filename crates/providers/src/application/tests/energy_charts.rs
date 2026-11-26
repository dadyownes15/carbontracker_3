//! The Energy-Charts candidates against a fake server answering with a
//! recorded `co2eq` answer, moved to the present.

use std::time::Duration;

use chrono::{DurationRound, TimeDelta, Utc};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use carbontracker::{
    Cooldown, Fidelity, GridLocation, Provider, ProviderError, ProviderId, ProviderResponse, Reading, SampleKind,
    SourceDescription, SourceId,
};

use crate::adapters::intensity::energy_charts::{probe_energy_charts, probe_energy_charts_forecast};
use crate::domain::location::Location;

const CO2EQ: &str = include_str!("fixtures/energy-charts-co2eq.json");
pub(super) const CO2EQ_PATH: &str = "/co2eq";
const ID: ProviderId = ProviderId(91);
/// The fixture's last interval with a `co2eq`, and its figure.
pub(super) const LAST_MEASURED: usize = 17;
pub(super) const LAST_FIGURE: f64 = 424.7;
/// The fixture's first interval with a `co2eq_forecast`, and its figure.
const FIRST_FORECAST: usize = 18;
const FIRST_FORECAST_FIGURE: f64 = 438.5;

fn client() -> reqwest::Client {
    reqwest::Client::builder().timeout(Duration::from_millis(500)).build().unwrap()
}

/// The fixture with its intervals moved so `current` is the one under way.
pub(super) fn answer_with_current(current: usize) -> String {
    let mut answer: serde_json::Value = serde_json::from_str(CO2EQ).unwrap();
    let start = Utc::now().duration_trunc(TimeDelta::minutes(15)).unwrap() - TimeDelta::minutes(15 * current as i64);
    for (index, stamp) in answer["unix_seconds"].as_array_mut().unwrap().iter_mut().enumerate() {
        *stamp = (start + TimeDelta::minutes(15 * index as i64)).timestamp().into();
    }
    answer.to_string()
}

/// The start of interval `index` in an answer moved by [answer_with_current].
fn interval_start(answer: &str, index: usize) -> chrono::DateTime<Utc> {
    let answer: serde_json::Value = serde_json::from_str(answer).unwrap();
    chrono::DateTime::from_timestamp(answer["unix_seconds"][index].as_i64().unwrap(), 0).unwrap()
}

fn ok(body: impl Into<String>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_string(body.into())
}

/// Energy-Charts answering only for `country`, in its own lowercase code.
pub(super) async fn serve(server: &MockServer, country: &str, answer: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(CO2EQ_PATH))
        .and(query_param("country", country))
        .respond_with(answer)
        .mount(server)
        .await;
}

fn stated(location: GridLocation) -> Location {
    Location::Stated(location)
}

fn zone(id: &str) -> Location {
    stated(GridLocation::Zone { zone_id: id.to_owned() })
}

async fn probe(server: &MockServer, location: &Location) -> Result<(crate::ports::Outcome, f64), String> {
    probe_energy_charts(ID, &client(), &server.uri(), Some(location)).await
}

async fn requests(server: &MockServer) -> usize {
    server.received_requests().await.unwrap().len()
}

/// Two reads of `provider`, off the async runtime, as the engine makes them.
async fn read_twice(mut provider: Box<dyn Provider>) -> [Result<ProviderResponse, ProviderError>; 2] {
    tokio::task::spawn_blocking(move || [provider.read(), provider.read()]).await.unwrap()
}

/// AC/OPEN-4 AC/OPEN-11 AC/OPEN-15 AC/OPEN-17
#[tokio::test]
async fn a_finer_zone_is_served_as_its_country_from_the_latest_begun_interval() {
    let server = MockServer::start().await;
    serve(&server, "se", ok(answer_with_current(LAST_MEASURED))).await;

    let (outcome, g_per_kwh) = probe(&server, &zone("SE-SE3")).await.unwrap();

    assert_eq!(g_per_kwh, LAST_FIGURE);
    let (_, description) = &outcome.descriptions[0];
    assert_eq!(
        description,
        &SourceDescription::Intensity {
            location: GridLocation::Country { country_code: "SE".to_owned() },
            fidelity: Fidelity::Measured
        },
        "described as the country, not the zone asked for"
    );
    assert_eq!(description.sample_kind(), SampleKind::Held);
}

/// AC/OPEN-15
#[tokio::test]
async fn a_figure_for_an_interval_still_to_come_is_never_read() {
    let server = MockServer::start().await;
    // The last two figures are for intervals that have not begun.
    serve(&server, "de", ok(answer_with_current(LAST_MEASURED - 2))).await;

    let (_, g_per_kwh) = probe(&server, &zone("DE")).await.unwrap();

    assert_eq!(g_per_kwh, 423.3, "the current interval's figure");
}

/// AC/OPEN-7
#[tokio::test]
async fn it_asks_with_the_country_and_nothing_else() {
    let server = MockServer::start().await;
    serve(&server, "de", ok(answer_with_current(LAST_MEASURED))).await;

    probe(&server, &stated(GridLocation::Country { country_code: "de".to_owned() })).await.unwrap();

    let asked = server.received_requests().await.unwrap();
    assert_eq!(asked.len(), 1);
    let query: Vec<(String, String)> = asked[0].url.query_pairs().into_owned().collect();
    assert_eq!(query, vec![("country".to_owned(), "de".to_owned())]);
    for header in ["authorization", "auth-token", "cookie", "x-api-key"] {
        assert!(!asked[0].headers.contains_key(header), "sends no {header}");
    }
}

/// AC/OPEN-6
#[tokio::test]
async fn a_location_outside_its_countries_is_declined_without_a_request() {
    let server = MockServer::start().await;

    for location in [zone("US-CAL-CISO"), stated(GridLocation::Global)] {
        assert!(probe(&server, &location).await.is_err(), "{location}");
        assert!(probe_energy_charts_forecast(&client(), &server.uri(), Some(&location), Duration::from_secs(3600))
            .await
            .is_err());
    }
    assert_eq!(requests(&server).await, 0);
}

#[tokio::test]
async fn an_answer_that_does_not_parse_or_has_no_figure_is_declined() {
    for answer in ["not json".to_owned(), r#"{"unix_seconds":[1],"co2eq":[null]}"#.to_owned()] {
        let server = MockServer::start().await;
        serve(&server, "de", ok(answer.clone())).await;
        assert!(probe(&server, &zone("DE")).await.is_err(), "{answer}");
    }
}

/// AC/OPEN-14
#[tokio::test]
async fn each_figure_is_read_once_stamped_at_the_start_of_its_interval() {
    let server = MockServer::start().await;
    let answer = answer_with_current(LAST_MEASURED);
    serve(&server, "de", ok(answer.clone())).await;
    let (outcome, _) = probe(&server, &zone("DE")).await.unwrap();
    let source = SourceId::new(ID, 0);

    let [first, second] = read_twice(outcome.provider).await;

    let first = first.unwrap();
    assert!(
        matches!(first.get(source), Some(Reading::Samples(samples))
            if *samples[0].observed_at() == interval_start(&answer, LAST_MEASURED) && samples[0].value() == LAST_FIGURE),
        "the probe's fetch answers the first read: {first:?}"
    );
    assert_eq!(second.unwrap().get(source), Some(&Reading::Unchanged), "the same interval is nothing new");
    assert_eq!(requests(&server).await, 2, "the probe's, and one read");
}

/// AC/OPEN-23
#[tokio::test]
async fn a_refusal_for_asking_too_often_is_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(path(CO2EQ_PATH))
        .respond_with(ok(answer_with_current(LAST_MEASURED)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    serve(&server, "de", ResponseTemplate::new(429).insert_header("Retry-After", "60")).await;
    let (outcome, _) = probe(&server, &zone("DE")).await.unwrap();

    let [_, second] = read_twice(outcome.provider).await;

    assert!(matches!(second, Err(ProviderError::RateLimited(_))), "{second:?}");
}

/// AC/OPEN-21
#[tokio::test]
async fn it_is_read_at_most_every_five_minutes() {
    let server = MockServer::start().await;
    serve(&server, "de", ok(answer_with_current(LAST_MEASURED))).await;
    let (outcome, _) = probe(&server, &zone("DE")).await.unwrap();

    assert_eq!(outcome.provider.cooldown(), Cooldown::After(Duration::from_secs(5 * 60)));
}

/// AC/OPEN-30
#[tokio::test]
async fn the_forecast_reaches_as_far_as_published_or_as_asked() {
    let server = MockServer::start().await;
    let answer = answer_with_current(LAST_MEASURED);
    serve(&server, "se", ok(answer.clone())).await;
    let day = Duration::from_secs(24 * 60 * 60);

    let probed = probe_energy_charts_forecast(&client(), &server.uri(), Some(&zone("SE-SE3")), day).await.unwrap();
    assert_eq!(probed.country.as_str(), "SE");
    assert!(probed.horizon < day, "the fixture reaches less than a day ahead: {:?}", probed.horizon);
    assert!(probed.horizon > day - Duration::from_secs(60 * 60), "{:?}", probed.horizon);
    assert_eq!(probed.curve[0].value(), FIRST_FORECAST_FIGURE);
    assert_eq!(*probed.curve[0].observed_at(), interval_start(&answer, FIRST_FORECAST));

    let hour = Duration::from_secs(60 * 60);
    let probed = probe_energy_charts_forecast(&client(), &server.uri(), Some(&zone("SE")), hour).await.unwrap();
    assert_eq!(probed.horizon, hour);
    let end = Utc::now() + TimeDelta::hours(1);
    assert!(probed.curve.iter().all(|sample| *sample.observed_at() <= end), "cut to the hour asked for");

    let fetch = probed.fetch;
    let again = tokio::task::spawn_blocking(move || fetch()).await.unwrap().expect("fetched again");
    assert_eq!(again[0].value(), FIRST_FORECAST_FIGURE);
}

#[tokio::test]
async fn a_forecast_with_no_point_ahead_is_declined() {
    let server = MockServer::start().await;
    // Every interval in the answer has passed.
    serve(&server, "de", ok(answer_with_current(200))).await;

    let probed = probe_energy_charts_forecast(&client(), &server.uri(), Some(&zone("DE")), Duration::from_secs(3600)).await;

    assert!(probed.is_err());
}
