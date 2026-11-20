//! When the prediction is printed, over generated runs whose forecast comes
//! early, late or never, and whose program goes on with more epochs or
//! idles once the prediction falls due.

use std::io::{self, Write};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use carbontracker::{Fidelity, GramsCo2ePerKwh, GridLocation, Sample, TrackerConfig, Watts};
use carbontracker_providers::{ForecastReport, GridConfig, Pinned, ProbeConfig};
use carbontracker_run::{
    ConsoleSink, LatestForecast, Layout, Outcome, Predictor, RunConfig, SourceSelection, Verbosity, Work, start,
};
use proptest::prelude::*;

/// What the legacy console prints first for a prediction.
const PREDICTED: &str = "Predicted consumption for";

/// Room for a busy host on top of what the rule allows.
const SLACK: Duration = Duration::from_secs(1);

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// One generated run.
#[derive(Debug, Clone)]
struct World {
    readout_interval: Duration,
    fetch_timeout: Duration,
    /// Epochs measured before the prediction.
    predict_after: usize,
    /// How long each epoch lasts, the first `predict_after` and then the rest.
    epochs: Vec<Duration>,
    /// When the forecast is first fetched, from the start; `None` never.
    forecast_after: Option<Duration>,
}

fn millis(range: std::ops::RangeInclusive<u64>) -> impl Strategy<Value = Duration> {
    range.prop_map(Duration::from_millis)
}

fn world() -> impl Strategy<Value = World> {
    (
        millis(10..=40),
        millis(100..=400),
        1usize..=3,
        0usize..=2,
        prop_oneof![
            1 => Just(None),
            2 => millis(0..=3000).prop_map(Some),
        ],
    )
        .prop_flat_map(|(readout_interval, fetch_timeout, predict_after, more, forecast_after)| {
            proptest::collection::vec(millis(5..=60), predict_after + more).prop_map(
                move |epochs| World {
                    readout_interval,
                    fetch_timeout,
                    predict_after,
                    epochs,
                    forecast_after,
                },
            )
        })
}

/// A flat forecast, a day long.
fn forecast() -> ForecastReport {
    let now = chrono::Utc::now();
    ForecastReport {
        location: GridLocation::Global,
        horizon: Duration::from_secs(24 * 3600),
        fidelity: Fidelity::Stated,
        curve: vec![
            Sample::new(now, 100.0),
            Sample::new(now + chrono::TimeDelta::days(1), 100.0),
        ],
    }
}

/// Nothing fetched until `after` from now; `None` never.
fn fetched_after(after: Option<Duration>) -> LatestForecast {
    let start = Instant::now();
    LatestForecast::from_fn(move || {
        after
            .filter(|after| start.elapsed() >= *after)
            .map(|_| Some(forecast()))
    })
}

fn config(world: &World, out: &Shared) -> RunConfig {
    let mut tracker = TrackerConfig {
        readout_interval: world.readout_interval,
        ..TrackerConfig::default()
    };
    tracker.fetch.timeout = world.fetch_timeout;
    RunConfig {
        probe: ProbeConfig {
            grid: GridConfig {
                location: Some(GridLocation::Global),
                ..GridConfig::default()
            },
            pinned: Pinned {
                power: Some(Watts::new(10.0).unwrap()),
                intensity: Some(GramsCo2ePerKwh::new(100.0).unwrap()),
                ..Pinned::default()
            },
            ..ProbeConfig::default()
        },
        tracker,
        frontend: "test",
        sinks: vec![Box::new(ConsoleSink::new(
            out.clone(),
            Verbosity::Normal,
            Layout::Legacy {
                decimals: 6,
                equivalents: true,
            },
            None,
        ))],
    }
}

/// Runs `world`; how long after the due epoch ended the prediction was
/// printed, `None` if not by `deadline` after it.
fn printed_after(world: &World, deadline: Duration) -> Option<Duration> {
    let out = Shared::default();
    let printed = Arc::new(OnceLock::new());
    let done = Arc::new(OnceLock::new());
    let watcher = {
        let (out, printed, done) = (out.clone(), Arc::clone(&printed), Arc::clone(&done));
        thread::spawn(move || {
            while done.get().is_none() {
                if out.text().contains(PREDICTED) {
                    let _ = printed.set(Instant::now());
                    return;
                }
                thread::sleep(Duration::from_millis(2));
            }
        })
    };

    let planned = world.epochs.len() + 5;
    let (predict_after, forecast_after) = (world.predict_after, world.forecast_after);
    let run = start(config(world, &out), move |_, _| {
        Predictor::new(
            Some(planned),
            Some(predict_after),
            None,
            fetched_after(forecast_after),
        )
    })
    .unwrap();
    let mut due = None;
    for (n, length) in world.epochs.iter().enumerate() {
        let epoch = run.start_span("epoch", None, SourceSelection::All);
        thread::sleep(*length);
        run.stop_span(epoch, Work::default());
        if n + 1 == world.predict_after {
            due = Some(Instant::now());
        }
    }
    let due = due.unwrap();
    while printed.get().is_none() && due.elapsed() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    let _ = done.set(());
    watcher.join().unwrap();
    let after = printed
        .get()
        .map(|at| at.saturating_duration_since(due))
        .filter(|after| *after <= deadline);
    run.finish(Outcome::Interrupted {
        epochs_done: Some(world.epochs.len() as u64),
    }).unwrap();
    after
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(12))]

    /// AC/RUN-25
    #[test]
    fn the_prediction_is_printed_within_a_readout_and_two_fetch_timeouts(world in world()) {
        let allowed = world.readout_interval + 2 * world.fetch_timeout;
        let printed = printed_after(&world, allowed + SLACK);
        prop_assert!(
            printed.is_some(),
            "not printed within {allowed:?} and {SLACK:?} of slack after the epoch it fell due after"
        );
    }
}
