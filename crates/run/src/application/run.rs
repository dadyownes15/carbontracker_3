//! [start] and [RunHandle]: a run's lifecycle, and a frontend's only way
//! into the engine.

use std::sync::Arc;

use carbontracker::{
    ConfigError, Fidelity, GridLocation, SessionReport, SourceDescription, SourceSelection, SpanId, Subscription,
    TrackerConfig, Work, TrackerError, TrackerHandle, TrackerQuery, WeakTrackerHandle,
};
use carbontracker_providers::{
    Forecaster, KeyedService, ProbeConfig, ProbeConfigError, ProviderRegistry, Resolution,
};

use crate::application::forwarder::ForwarderHandle;
use crate::application::view::View;
use crate::application::writer::WriterHandle;
use crate::domain::encode;
use crate::domain::item::{ForecastSource, Item, Outcome, Started};
use crate::domain::{presentation, record};
use crate::ports::Projection;
use crate::ports::Sink;
use tokio::sync::oneshot;

/// The carbontracker version, written into each record.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// What a run needs from its frontend.
pub struct RunConfig {
    pub probe: ProbeConfig,
    pub tracker: TrackerConfig,
    /// The record's `frontend`: `cli`, `python`, `slurm`.
    pub frontend: &'static str,
    /// Everything the run writes to, in this order.
    pub sinks: Vec<Box<dyn Sink>>,
}

/// Why a run couldn't start or finish.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Probe(#[from] ProbeConfigError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Tracker(#[from] TrackerError),
    #[error("cannot probe: {0}")]
    Io(#[from] std::io::Error),
}

/// Starts a run: probes, starts the engine, and the threads that project and
/// write what it does. Blocks until the run has started. Everything from the
/// first probe step on reaches `config.sinks`; if starting fails, they say
/// why before this returns.
///
/// `projection` builds the run's projection once the engine is up, from a
/// handle on it that never keeps it running, and the run's [Forecaster]: a
/// projection that wants the forecast asks it, through
/// [LatestForecast](crate::LatestForecast). `|_, _| ()` for none.
///
/// # Errors
///
/// An invalid configuration, or an engine that couldn't start.
pub fn start<P: Projection>(
    config: RunConfig,
    projection: impl FnOnce(&WeakTrackerHandle, &Forecaster) -> P,
) -> Result<RunHandle<P>, RunError> {
    let RunConfig {
        probe,
        tracker,
        frontend,
        sinks,
    } = config;
    let writer = WriterHandle::new(sinks);
    match begin(probe, tracker, frontend, &writer) {
        Ok((engine, forecaster, mut started, events, warnings)) => {
            let projection = projection(&engine.downgrade(), &forecaster);
            projection.describe(&mut started.header);
            let started = Arc::new(started);
            let view = View::new(projection);
            let forwarder =
                ForwarderHandle::new(Arc::clone(&started), warnings, view.clone(), events, writer);
            Ok(RunHandle {
                engine,
                forwarder,
                view,
                started,
            })
        }
        Err(error) => {
            writer.item(Item::Error(error.to_string()));
            writer.close_and_wait();
            Err(error)
        }
    }
}

/// Probes, builds the record's header and the warnings to print after it,
/// and starts the engine, subscribed before any span exists.
fn begin(
    probe: ProbeConfig,
    tracker: TrackerConfig,
    frontend: &'static str,
    writer: &WriterHandle,
) -> Result<(TrackerHandle, Forecaster, Started, Subscription, Vec<String>), RunError> {
    let components = encode::components(&probe.measure);
    let keyed = probe.grid.api_keys.contains_key(&KeyedService::ElectricityMaps);
    let global = matches!(probe.grid.location, Some(GridLocation::Global));
    let registry = ProviderRegistry::new(probe)?;
    // Some probes are network calls: a runtime that lives only for the probe.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let resolution = runtime.block_on(registry.probe_with(|step| writer.item(Item::Probing(step))));
    drop(runtime);

    let header = record::Header {
        format: record::FORMAT,
        carbontracker: VERSION.to_owned(),
        frontend: frontend.to_owned(),
        command: std::env::args().collect(),
        config: record::Config {
            interval_s: tracker.readout_interval.as_secs_f64(),
            pue: tracker.pue.get(),
            components,
            location: encode::location(resolution.location()),
        },
        epochs: None,
    };
    let assumed = resolution
        .sources()
        .any(|(_, source)| matches!(source, SourceDescription::Intensity { fidelity: Fidelity::Assumed, .. }));
    let warnings = (assumed && !global)
        .then(|| presentation::assumed_grid(resolution.location(), keyed))
        .into_iter()
        .collect();
    let probe = encode::probe(&resolution);
    let served_by = resolution
        .sources()
        .filter_map(|(id, _)| Some((id, resolution.served_by(id)?)))
        .collect();
    let cooldowns = resolution
        .sources()
        .filter_map(|(id, _)| Some((id, resolution.cooldown(id)?)))
        .collect();
    let fetch_timeout = tracker.fetch.timeout;

    let (sources, forecaster) = Resolution::resolve(resolution);
    let forecast = ForecastSource {
        location: forecaster.location().clone(),
        horizon: forecaster.horizon(),
        fidelity: forecaster.fidelity(),
        served_by: forecaster.served_by(),
    };
    // Subscribed before the engine reads, so the record holds the opening readouts.
    let (engine, events) = carbontracker::start_subscribed(sources, tracker)?;
    let (reply, answer) = oneshot::channel();
    engine.query(TrackerQuery::Catalog { reply });
    let catalog = answer.blocking_recv().map_err(|_| TrackerError::Closed)??;
    Ok((
        engine,
        forecaster,
        Started {
            header,
            probe,
            catalog,
            served_by,
            cooldowns,
            fetch_timeout,
            forecast: Some(forecast),
        },
        events,
        warnings,
    ))
}

/// A running run. Span calls go straight to the engine and never wait;
/// [RunHandle::finish] ends the run once everything is written.
pub struct RunHandle<P: Projection = ()> {
    engine: TrackerHandle,
    forwarder: ForwarderHandle,
    view: View<P>,
    started: Arc<Started>,
}

impl<P: Projection> RunHandle<P> {
    /// Opens a span under `parent`, or at the top level.
    pub fn start_span(&self, name: &str, parent: Option<SpanId>) -> SpanId {
        self.engine.start_span(name, parent, SourceSelection::All)
    }

    /// Reports work done so far on an open span.
    pub fn mark(&self, span: SpanId, work: Work) {
        self.engine.mark(span, work);
    }

    /// Closes a span, reporting the work it did.
    pub fn stop_span(&self, span: SpanId, work: Work) {
        self.engine.stop_span(span, work);
    }

    /// The projection, readable at any time.
    pub fn view(&self) -> &View<P> {
        &self.view
    }

    /// What the run started with: its header, the probe and the catalog.
    pub fn started(&self) -> &Started {
        &self.started
    }

    /// Ends the run: says how it ended, finishes the engine, and returns once
    /// every sink has written everything. Returns the session and the view,
    /// which still reads the projection's final state.
    ///
    /// # Errors
    ///
    /// If the engine stopped before it could report the session.
    pub fn finish(self, outcome: Outcome) -> Result<(SessionReport, View<P>), RunError> {
        // Before the engine finishes, so the outcome comes before the session.
        let written = self.forwarder.finish(outcome);
        let report = futures::executor::block_on(self.engine.finish());
        let _ = written.blocking_recv();
        Ok((report?, self.view))
    }
}
