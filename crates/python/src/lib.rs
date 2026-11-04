//! `carbontracker._native`: the engine for the Python package.
//!
//! A run prints through the Python function it is given (`print`, or
//! `tqdm.write`), so its output reaches a notebook. Everything else comes back
//! as plain dicts, which the Python package turns into dataclasses.

mod convert;
mod console_writer;
mod profiler;
mod report;
mod tracker;

use pyo3::prelude::*;

#[pymodule]
#[pyo3(name = "_native")]
mod native {
    use carbontracker_run::presentation;
    use pyo3::prelude::*;

    #[pymodule_export]
    use crate::report::{equivalents, report};
    #[pymodule_export]
    use crate::tracker::Tracker;

    /// The prefix of every printed line.
    #[pymodule_export]
    const PREFIX: &str = presentation::PREFIX;

    /// The `carbontracker` command: the CLI's `run` on `argv`, program name
    /// first. Returns the exit code.
    #[pyfunction]
    fn cli_main(py: Python<'_>, argv: Vec<String>) -> i32 {
        py.detach(|| carbontracker_cli::run(argv))
    }

    /// Printed when tracking fails and `ignore_errors` is set.
    #[pyfunction]
    fn ignored_error(traceback: &str) -> String {
        presentation::ignored_error(traceback)
    }
}
