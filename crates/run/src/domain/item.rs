//! [Item]: everything a run writes, in the order its sinks get it.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use carbontracker::{CatalogReport, Cooldown, Fidelity, GridLocation, SourceId, TrackerEvent};
use carbontracker_providers::ProbeProgress;

use crate::domain::record;

/// One thing a run writes. Cheap to clone: reports are shared.
#[derive(Clone)]
pub enum Item {
    /// A probe step, before [Item::Started].
    Probing(ProbeProgress),
    /// The run has started: its record header, what probing found and the
    /// engine's catalog.
    Started(Arc<Started>),
    Event(TrackerEvent),
    /// The engine cut the run off: events were lost here.
    Gap,
    /// How the run ended; before the session's end.
    Outcome(Outcome),
    /// The run's own warning, such as a sink that failed.
    Warning(String),
    /// The run's own error, such as a start that failed.
    Error(String),
    /// Console text a projection added.
    Block(Arc<dyn Block>),
}

/// What a run starts with: the record's `run` line, the probe's outcome, the
/// catalog, the candidate serving each of its sources, and the forecast.
#[derive(Debug, Clone)]
pub struct Started {
    pub header: record::Header,
    pub probe: record::Probe,
    pub catalog: CatalogReport,
    /// The probe candidate serving each source, e.g. `"electricity-maps"`.
    pub served_by: HashMap<SourceId, &'static str>,
    /// How soon each source can be read again, as its provider declares.
    pub cooldowns: HashMap<SourceId, Cooldown>,
    /// How long a read may take before it is abandoned.
    pub fetch_timeout: Duration,
    /// The forecast the run's projections can ask for; `None` when there is
    /// none to ask.
    pub forecast: Option<ForecastSource>,
}

/// What the run's forecast is.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastSource {
    pub location: GridLocation,
    pub horizon: Duration,
    pub fidelity: Fidelity,
    /// The probe candidate behind it, e.g. `"projection"`.
    pub served_by: &'static str,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// All planned epochs ran, or a run without epochs stopped normally.
    Completed { epochs_done: Option<u64> },
    /// Tracking stopped after `monitor_epochs`.
    Monitored { epochs_done: u64 },
    /// Stopped early.
    Interrupted { epochs_done: Option<u64> },
    /// Tracking failed and the program continued.
    Failed { epochs_done: Option<u64> },
    /// The tracked command exited with `code`.
    Exited { code: i32 },
    /// A signal killed the tracked command.
    Signalled { signal: i32 },
}

/// Console text a projection adds. The run never looks inside.
pub trait Block: Send + Sync + fmt::Debug {
    /// The lines as printed, prefix included where it belongs.
    fn lines(&self, layout: &Layout) -> Vec<String>;
}

/// How the console lays out its blocks.
#[derive(Debug, Clone, PartialEq)]
pub enum Layout {
    /// The CLI's: a summary titled by what ran.
    Summary { title: String },
    /// carbontracker 2.4.7's blocks, numbers at `decimals`.
    Legacy { decimals: usize, equivalents: bool },
}
