//! Resolves grid intensity against the live Electricity Maps API. Needs
//! network and a key, so it is ignored by default.
//!
//! Electricity Maps plans reach different things, so the tests come in two
//! sets, each with its own key:
//!
//! - `free_key_*`, with `CARBONTRACKER_EM_FREE_KEY`: a free key reaches
//!   `latest` for one zone, `CARBONTRACKER_EM_FREE_ZONE` (default `DK`), and
//!   nothing else: not its sub-zones, not the forecast;
//! - `paid_key_*`, with `CARBONTRACKER_EM_PAID_KEY`: a key reaching `latest`
//!   and `forecast` for `DK-DK2`.
//!
//! Run a set with its key:
//!
//!     CARBONTRACKER_EM_FREE_KEY=... cargo test -p carbontracker-providers --test live_intensity -- --ignored free_key
//!     CARBONTRACKER_EM_PAID_KEY=... cargo test -p carbontracker-providers --test live_intensity -- --ignored paid_key
//!
//! Each test checks the shape of the answers, not their values. One fetch per
//! endpoint per test, to stay inside the rate limit. When one fails on shape,
//! re-record `src/tests/fixtures/`.

use std::time::Duration;

use carbontracker::{
    start, GridLocation, SourceDescription, SourceSelection, TrackerConfig, TrackerEvent, Watts, Work,
};
use carbontracker_providers::{
    ApiKey, ForecastReport, Forecaster, GridConfig, KeyedService, Pinned, ProbeConfig, ProviderRegistry, Resolution,
    Verdict,
};

fn key(variable: &str) -> ApiKey {
    ApiKey::new(std::env::var(variable).unwrap_or_else(|_| panic!("set {variable}")))
}

fn free_zone() -> String {
    std::env::var("CARBONTRACKER_EM_FREE_ZONE").unwrap_or_else(|_| "DK".to_owned())
}

fn zone(id: &str) -> GridLocation {
    GridLocation::Zone { zone_id: id.to_owned() }
}

fn probe_config(location: GridLocation, key: ApiKey) -> ProbeConfig {
    ProbeConfig {
        grid: GridConfig {
            location: Some(location),
            api_keys: [(KeyedService::ElectricityMaps, key)].into(),
            forecast_horizon: Duration::from_secs(6 * 60 * 60),
        },
        // Pin power; only the grid varies.
        pinned: Pinned { power: Some(Watts::new(100.0).unwrap()), ..Pinned::default() },
        ..ProbeConfig::default()
    }
}

async fn probe(location: GridLocation, key: ApiKey) -> Resolution {
    ProviderRegistry::new(probe_config(location, key)).expect("a valid location").probe().await
}

fn verdict(resolution: &Resolution, name: &str) -> Option<Verdict> {
    resolution.candidates().iter().find(|o| o.name == name).map(|o| o.verdict.clone())
}

fn serving(resolution: &Resolution, name: &str) -> bool {
    matches!(verdict(resolution, name), Some(Verdict::Serving(_)))
}

fn grid_location(resolution: &Resolution) -> GridLocation {
    resolution
        .sources()
        .find_map(|(_, source)| match source {
            SourceDescription::Intensity { location, .. } => Some(location.clone()),
            SourceDescription::Power { .. } => None,
        })
        .expect("the grid source resolved")
}

/// The forecast, as it stands now.
async fn forecast(forecaster: &Forecaster) -> Option<ForecastReport> {
    let (reply, answer) = tokio::sync::oneshot::channel();
    forecaster.ask(move |report| {
        let _ = reply.send(report);
    });
    answer.await.expect("every ask is answered")
}

/// A tracker over live intensity bills a span, and its forecast reads.
/// AC/TST-2
#[tokio::test]
#[ignore = "needs network and CARBONTRACKER_EM_FREE_KEY"]
async fn free_key_bills_its_zone_live() {
    let zone_id = free_zone();
    let resolution = probe(zone(&zone_id), key("CARBONTRACKER_EM_FREE_KEY")).await;

    assert!(serving(&resolution, "electricity-maps"), "electricity-maps should serve {zone_id}: {resolution}");
    assert_eq!(verdict(&resolution, "country-average"), Some(Verdict::Covered));
    assert_eq!(verdict(&resolution, "global-average"), Some(Verdict::Covered));
    assert_eq!(grid_location(&resolution), zone(&zone_id));

    let config = TrackerConfig { readout_interval: Duration::from_millis(500), ..TrackerConfig::default() };
    let (sources, forecaster) = resolution.resolve();
    let tracker = start(sources, config).expect("a valid config");
    let mut events = tracker.subscribe();

    let span = tracker.start_span("epoch", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(1200)).await;
    tracker.stop_span(span, Work::default());

    let report = loop {
        match events.recv().await.expect("the stream stays open") {
            TrackerEvent::SpanSettled { report } if report.span_id == span => break report,
            _ => {}
        }
    };
    assert!(report.total.emissions.expect("a live zone bills emissions").get() > 0.0, "a live zone has non-zero intensity");

    // A free plan has no forecast, so this is the projection holding the live
    // intensity forward; a plan that gains one serves it instead.
    let forecast = forecast(&forecaster).await.expect("a forecast resolves");
    assert!(!forecast.curve.is_empty());
    assert!(forecast.curve.iter().all(|sample| sample.value() > 0.0));
}

/// A free key refused a sub-zone falls back to its country's zone, live,
/// rather than the country table. Needs a country zone with sub-zones as
/// `CARBONTRACKER_EM_FREE_ZONE` (the default `DK` has `DK-DK2`).
/// AC/TST-2
#[tokio::test]
#[ignore = "needs network and CARBONTRACKER_EM_FREE_KEY"]
async fn free_key_refused_a_sub_zone_bills_the_country_zone_live() {
    let country = free_zone();
    let sub_zone = std::env::var("CARBONTRACKER_EM_FREE_SUB_ZONE").unwrap_or_else(|_| "DK-DK2".to_owned());
    let resolution = probe(zone(&sub_zone), key("CARBONTRACKER_EM_FREE_KEY")).await;

    assert!(serving(&resolution, "electricity-maps"), "{resolution}");
    assert_eq!(grid_location(&resolution), zone(&country));
    assert!(serving(&resolution, "projection"), "a free plan has no forecast: {resolution}");
}

/// Coordinates in a split country give its sub-zone, not the aggregate.
/// Needs `latest` for `DK-DK2`.
/// AC/TST-2
#[tokio::test]
#[ignore = "needs network and CARBONTRACKER_EM_PAID_KEY"]
async fn paid_key_resolves_coordinates_to_their_sub_zone() {
    let copenhagen = GridLocation::Coordinates { latitude: 55.6759, longitude: 12.5655 };
    let resolution = probe(copenhagen, key("CARBONTRACKER_EM_PAID_KEY")).await;

    assert!(serving(&resolution, "electricity-maps"), "{resolution}");
    assert_eq!(grid_location(&resolution), zone("DK-DK2"));
}

/// The live forecast is for the zone the run is billed against. Needs
/// `latest` and `forecast` for `DK-DK2`.
/// AC/TST-2
#[tokio::test]
#[ignore = "needs network and CARBONTRACKER_EM_PAID_KEY"]
async fn paid_key_forecasts_the_zone_it_bills() {
    let resolution = probe(zone("DK-DK2"), key("CARBONTRACKER_EM_PAID_KEY")).await;

    assert!(serving(&resolution, "electricity-maps-forecast"), "{resolution}");
    assert_eq!(verdict(&resolution, "projection"), Some(Verdict::Covered));
    let billed = grid_location(&resolution);
    let (_, forecaster) = resolution.resolve();
    assert_eq!(forecaster.location(), &billed);

    let forecast = forecast(&forecaster).await.expect("the forecast resolves");
    assert!(!forecast.curve.is_empty());
    assert!(forecast.curve.iter().all(|sample| sample.value() > 0.0));
}
