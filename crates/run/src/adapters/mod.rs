//! The ports' implementations, and the run's folder:
//!
//! - `sinks`: the console and the record, and [standard_sinks];
//! - `projectors`: the [Predictor];
//! - the run [Folder];
//! - [read_report]: records read back;
//! - a project's folder: [project_folder], [check_project].

mod folder;
mod project;
mod report;
pub mod projectors;
pub mod sinks;

pub use folder::Folder;
pub use project::{check_project, project_folder};
pub use report::read_report;
pub use projectors::{EPOCH, LatestForecast, Predictor, prediction};
pub use sinks::{Console, ConsoleSink, JsonlSink, Verbosity, standard_sinks};
