//! `report(paths)`: the runs recorded under `paths`, as plain dicts, with
//! their tables, CSV and citation already written.

use std::path::PathBuf;

use carbontracker_run::presentation::{self, report as text};
use carbontracker_run::{SpanGroup, Total, read_report};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

/// The report as a dict: `runs`, `spans`, `total`, `unreadable` and `text`.
#[pyfunction]
pub fn report(py: Python<'_>, paths: Vec<PathBuf>) -> PyResult<Bound<'_, PyDict>> {
    let read = py.detach(|| read_report(&paths))?;
    let dict = PyDict::new(py);

    let runs = PyList::empty(py);
    for run in &read.runs {
        let item = PyDict::new(py);
        item.set_item("file", &run.file)?;
        item.set_item("started", crate::convert::seconds(&run.started))?;
        item.set_item("frontend", &run.frontend)?;
        item.set_item("command", &run.command)?;
        item.set_item("ended", run.ended())?;
        item.set_item("code", run.outcome.as_ref().and_then(|outcome| outcome.code))?;
        item.set_item("signal", run.outcome.as_ref().and_then(|outcome| outcome.signal))?;
        item.set_item("duration_s", run.session.as_ref().map(|session| session.duration_s))?;
        item.set_item("energy_kwh", run.session.as_ref().map(|session| session.energy_kwh))?;
        item.set_item("emissions_g", run.session.as_ref().and_then(|session| session.emissions_g))?;
        item.set_item("complete", run.is_complete())?;
        runs.append(item)?;
    }
    dict.set_item("runs", runs)?;

    let spans = PyList::empty(py);
    for group in read.spans() {
        spans.append(span_group(py, group)?)?;
    }
    dict.set_item("spans", spans)?;
    dict.set_item("total", total(py, &read.total())?)?;

    let unreadable = PyList::empty(py);
    for file in &read.unreadable {
        let item = PyDict::new(py);
        item.set_item("file", &file.file)?;
        item.set_item("reason", &file.reason)?;
        unreadable.append(item)?;
    }
    dict.set_item("unreadable", unreadable)?;

    let words = PyDict::new(py);
    words.set_item("runs", text::runs(&read))?;
    words.set_item("spans", text::spans(&read))?;
    words.set_item("runs_csv", text::runs_csv(&read))?;
    words.set_item("spans_csv", text::spans_csv(&read))?;
    words.set_item("citation", text::citation(&read.total()))?;
    words.set_item("notes", text::notes(&read).iter().map(|note| presentation::note(note)).collect::<Vec<_>>())?;
    dict.set_item("text", words)?;
    Ok(dict)
}

/// What `grams` of CO2eq is equivalent to, as `{unit: amount}`.
#[pyfunction]
pub fn equivalents(py: Python<'_>, grams: f64) -> PyResult<Bound<'_, PyDict>> {
    let dict = PyDict::new(py);
    for (amount, unit) in presentation::equivalents(grams) {
        dict.set_item(unit, amount)?;
    }
    Ok(dict)
}

fn span_group<'py>(py: Python<'py>, group: &SpanGroup) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("path", &group.path)?;
    dict.set_item("count", group.count)?;
    dict.set_item("energy_kwh", group.energy_kwh)?;
    dict.set_item("mean_kwh", group.mean_kwh())?;
    dict.set_item("min_kwh", group.min_kwh)?;
    dict.set_item("max_kwh", group.max_kwh)?;
    dict.set_item("emissions_g", group.emissions_g)?;
    dict.set_item("without_emissions", group.without_emissions)?;
    Ok(dict)
}

fn total<'py>(py: Python<'py>, total: &Total) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("runs", total.runs)?;
    dict.set_item("duration_s", total.duration_s)?;
    dict.set_item("energy_kwh", total.energy_kwh)?;
    dict.set_item("emissions_g", total.emissions_g)?;
    dict.set_item("without_emissions", total.without_emissions)?;
    Ok(dict)
}
