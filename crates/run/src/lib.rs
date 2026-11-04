//! A carbontracker run, for every frontend.
//!
//! [start] probes this host, starts the engine and returns a [RunHandle]:
//! the frontend's only way into the engine. Everything the run does, from
//! the first probe step to the session's end, reaches the frontend's
//! [Sink]s in one order:
//!
//! - the forwarder thread takes the engine's events, hands each item to the
//!   run's [Projection], and passes it on; it does no I/O, so it never falls
//!   behind the engine;
//! - the writer thread hands each item to every sink.
//!
//! The crate is laid out as the engine is:
//!
//! - [domain]: a run's items, its record and its words; no threads, no I/O,
//!   and no dependency on the other layers;
//! - `ports`: the traits the run drives, [Sink] and [Projection];
//! - `application`: [start], [RunHandle], [View] and the two actors;
//! - `adapters`: the ports' implementations ([ConsoleSink], [JsonlSink],
//!   [Predictor]), the run [Folder], and [read_report].

mod adapters;
mod application;
pub mod domain;
mod ports;

pub use adapters::{
    Console, ConsoleSink, EPOCH, Folder, read_report, JsonlSink, LatestForecast, Predictor, Verbosity, prediction,
    standard_sinks,
};
pub use application::{RunConfig, RunError, RunHandle, VERSION, View, start};
pub use domain::item::{Block, ForecastSource, Item, Layout, Outcome, Started};
pub use domain::report::{Report, Run, SpanGroup, Total, Unreadable};
pub use domain::{presentation, record};
pub use ports::{Projection, Sink, deliver};

// What a frontend needs from the engine.
pub use carbontracker::{SessionReport, SpanId, Work, WorkEntry};
