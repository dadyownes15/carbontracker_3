//! Record format 1. Units are in field names (`_kwh`, `_g`, `_s`,
//! `_g_per_kwh`), ids are strings and durations are seconds. Open-ended
//! values (domain, fidelity, unit, frontend) are strings so that new values
//! don't break older readers.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One line of a record, with its UTC timestamp.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub at: DateTime<Utc>,
    #[serde(flatten)]
    pub line: Line,
}

/// A line's content, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Line {
    /// The first line: what wrote the record, and its configuration.
    Run(Header),
    /// The second line: what probing found, candidate by candidate.
    Probe(Probe),
    /// The third line: the sources being measured.
    Catalog(Catalog),
    Readout(Readout),
    SpanStarted(SpanStarted),
    SpanStopped(SpanStopped),
    Marked(Marked),
    /// A start, stop or mark refused by the engine.
    Refused(Refused),
    SpanSettled(SpanSettled),
    /// Events were lost at this point.
    Gap,
    /// How the run ended.
    Outcome(Outcome),
    /// The last line, written when the session ends normally.
    Session(Session),
    /// A kind added by a later version; skipped when reading.
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    /// The record format version.
    pub format: u32,
    /// The carbontracker version that wrote the record.
    pub carbontracker: String,
    /// `cli`, `python` or `slurm`.
    pub frontend: String,
    /// How this process was started: its command line.
    pub command: Vec<String>,
    pub config: Config,
    /// `None` if the run has no epochs.
    pub epochs: Option<Epochs>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub interval_s: f64,
    pub pue: f64,
    /// The power domains measured; `None` for all.
    pub components: Option<Vec<String>>,
    /// The grid location, stated or estimated; `None` if unknown.
    pub location: Option<Location>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    Stated(Grid),
    Estimated {
        country: String,
        /// `[latitude, longitude]`.
        coordinates: Option<[f64; 2]>,
        /// The estimator, e.g. `ip-geolocation`.
        by: String,
    },
}

/// A grid location, e.g. `{"zone": "DK-DK2"}`, `{"country": "DK"}`,
/// `{"coordinates": [55.68, 12.57]}` or `"global"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grid {
    Zone(String),
    Country(String),
    Coordinates([f64; 2]),
    DataCenter { provider: String, region: String },
    Global,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Epochs {
    pub planned: u64,
    pub monitor: u64,
    pub before_prediction: i64,
}

/// What probing found: every candidate in chain order, with its verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub name: String,
    /// `serving`, `declined`, `covered` or `not_wanted`.
    pub verdict: String,
    /// What a serving candidate serves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub series: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    pub sources: Vec<Source>,
    /// Each power source's intensity source, by id.
    pub pairs: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    /// The source id used by other lines, e.g. `"3.0"`.
    pub id: String,
    /// `power` or `intensity`.
    pub role: String,
    /// A display label, e.g. `Apple M2 Pro (cpu)`.
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// `cpu`, `gpu`, `ram`, `ane`, `system`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// `device_total`, `process`, `job`, `container`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Grid>,
    /// The unit of the readout samples: `W`, `J` (cumulative) or `g/kWh`.
    pub unit: String,
    /// `measured`, `stated`, `modelled`, `assumed`.
    pub fidelity: String,
}

/// One readout: each source it read, by id, under what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Readout {
    /// `[time, value]` samples per source id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub samples: BTreeMap<String, Vec<(DateTime<Utc>, f64)>>,
    /// Sources that were read and had nothing new, so the last value stands.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unchanged: BTreeSet<String>,
    /// Sources not read while all is well: `cooldown` or `busy`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub skipped: BTreeMap<String, String>,
    /// Sources not read, or not readable, and what went wrong.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failed: BTreeMap<String, Failed>,
}

/// What went wrong with one source in one readout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failed {
    /// `provider`, `unanswered`, `timed_out`, `stalled`, `panicked` or `retired`.
    pub failure: String,
    /// For a provider's failure: `unavailable`, `denied`, `rate_limited` or `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// For people: the provider's detail, or a panic's message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanStarted {
    pub span: u64,
    pub name: String,
    pub parent: Option<u64>,
    pub billed: Billed,
}

/// What a span is billed for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Billed {
    All,
    Domains(Vec<String>),
    Sources(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanStopped {
    pub span: u64,
    pub work: Vec<WorkEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkEntry {
    pub kind: String,
    pub amount: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Marked {
    pub span: u64,
    pub work: Vec<WorkEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    pub span: u64,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpanSettled {
    pub span: u64,
    pub name: String,
    pub duration_s: f64,
    pub energy_kwh: f64,
    /// `None` where energy was drawn with no intensity to bill it against.
    pub emissions_g: Option<f64>,
    /// Excluding what child spans used.
    pub exclusive_energy_kwh: f64,
    pub exclusive_emissions_g: Option<f64>,
    pub per_source: BTreeMap<String, Usage>,
    /// Per power source id, the samples it had.
    pub samples: BTreeMap<String, u64>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub energy_kwh: f64,
    /// `None` where energy was drawn with no intensity to bill it against.
    pub emissions_g: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub ended: Ended,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub epochs_done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ended {
    /// Python: all planned epochs ran, or a run without epochs stopped.
    Completed,
    /// Python: tracking stopped after `monitor_epochs`.
    Monitored,
    /// Python: `stop()` was called early.
    Interrupted,
    /// Python: tracking failed with `ignore_errors` set.
    Failed,
    /// CLI: the command exited with `code`.
    Exited,
    /// CLI: the command was killed by a signal.
    Signalled,
    /// A value added by a later version.
    #[serde(untagged)]
    Other(String),
}

impl Ended {
    /// The serialized name, e.g. `completed`.
    pub fn name(&self) -> &str {
        match self {
            Self::Completed => "completed",
            Self::Monitored => "monitored",
            Self::Interrupted => "interrupted",
            Self::Failed => "failed",
            Self::Exited => "exited",
            Self::Signalled => "signalled",
            Self::Other(name) => name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub duration_s: f64,
    pub energy_kwh: f64,
    /// `None` where energy was drawn with no intensity to bill it against.
    pub emissions_g: Option<f64>,
    /// Usage attributed to spans.
    pub claimed: Usage,
    /// Usage while no span was running.
    pub unclaimed: Usage,
    pub per_source: BTreeMap<String, Usage>,
    /// Per power source id, the samples it had.
    pub samples: BTreeMap<String, u64>,
    /// The intensity sources the power sources were billed against.
    pub intensities: Vec<Intensity>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Intensity {
    pub source: String,
    pub label: String,
    pub fidelity: String,
    /// Emissions divided by energy, over the power sources paired with this
    /// source; `None` if they used no energy.
    pub mean_g_per_kwh: Option<f64>,
}
