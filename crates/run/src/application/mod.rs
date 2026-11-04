//! The run's lifecycle ([start], [RunHandle]) and the two actors behind it:
//! the forwarder, which projects, and the writer, which drives the sinks.

mod forwarder;
mod run;
mod view;
mod writer;

pub use run::{RunConfig, RunError, RunHandle, VERSION, start};
pub use view::View;
