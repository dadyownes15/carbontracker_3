//! Reports as plain dicts of floats and strings, for the Python dataclasses.
//! Times are seconds since the Unix epoch; energy kWh; emissions grams.

use std::collections::HashMap;

use carbontracker::{ReportWarning, SessionReport, SpanReport, SourceDescription, SourceId, SourceKind, Timestamp, Usage};
use carbontracker_run::prediction::Prediction;
use carbontracker_run::presentation;
use pyo3::prelude::*;
use pyo3::types::PyDict;

pub fn seconds(at: &Timestamp) -> f64 {
    at.timestamp_micros() as f64 / 1e6
}

pub fn usage<'py>(py: Python<'py>, usage: &Usage) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("energy_kwh", usage.energy.to_kilowatt_hours().get())?;
    dict.set_item("emissions_g", usage.emissions.map(|grams| grams.get()))?;
    Ok(dict)
}

pub fn source<'py>(py: Python<'py>, id: SourceId, description: &SourceDescription) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("id", id.to_string())?;
    let kind = match description.kind() {
        SourceKind::Power => "power",
        SourceKind::Intensity => "intensity",
    };
    dict.set_item("kind", kind)?;
    dict.set_item("label", presentation::source(description))?;
    dict.set_item("fidelity", description.fidelity().to_string())?;
    Ok(dict)
}

fn per_source<'py>(py: Python<'py>, usages: &HashMap<SourceId, Usage>) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (id, used) in usages {
        dict.set_item(id.to_string(), usage(py, used)?)?;
    }
    Ok(dict)
}

pub fn session<'py>(py: Python<'py>, report: &SessionReport) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("duration_s", report.duration.as_secs_f64())?;
    dict.set_item("total", usage(py, &report.total)?)?;
    dict.set_item("claimed", usage(py, &report.claimed)?)?;
    dict.set_item("unclaimed", usage(py, &report.unclaimed)?)?;
    dict.set_item("per_source", per_source(py, &report.per_source)?)?;
    let sources = PyDict::new(py);
    for (&id, description) in &report.sources {
        sources.set_item(id.to_string(), source(py, id, description)?)?;
    }
    dict.set_item("sources", sources)?;
    let intensities = PyDict::new(py);
    for (&id, description) in &report.intensities {
        intensities.set_item(id.to_string(), source(py, id, description)?)?;
    }
    dict.set_item("intensities", intensities)?;
    dict.set_item("warnings", report.warnings.iter().map(ToString::to_string).collect::<Vec<_>>())?;
    Ok(dict)
}

pub fn prediction<'py>(py: Python<'py>, prediction: &Prediction) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    let total = Usage { energy: prediction.energy, emissions: Some(prediction.emissions) };
    dict.set_item("total", usage(py, &total)?)?;
    dict.set_item("duration_s", prediction.duration.as_secs_f64())?;
    dict.set_item("remaining_s", prediction.remaining.as_secs_f64())?;
    dict.set_item("intensity_g_per_kwh", prediction.intensity.get())?;
    dict.set_item("measured", prediction.measured)?;
    dict.set_item("of_total", prediction.total)?;
    let warnings: Vec<String> = prediction.warnings.iter().map(ToString::to_string).collect();
    dict.set_item("warnings", warnings)?;
    Ok(dict)
}

/// One settled span, for profiler mode: energy in joules, and the fewest
/// samples any of the `paced` sources had in it (any source when none is
/// paced).
pub fn span<'py>(py: Python<'py>, report: &SpanReport, paced: &[SourceId]) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("id", report.span_id.get())?;
    dict.set_item("name", &report.span_name)?;
    dict.set_item("duration_s", report.duration.as_secs_f64())?;
    dict.set_item("energy_j", report.total.energy.get())?;
    let counts = report.samples.iter().filter(|(source, _)| paced.is_empty() || paced.contains(source));
    dict.set_item("samples", counts.map(|(_, &count)| count).min().unwrap_or(0))?;
    // Electricity only: no warning about intensity, and none about coverage
    // of a stated draw, which holds one value.
    let warnings: Vec<String> = report
        .warnings
        .iter()
        .filter(|warning| match warning {
            ReportWarning::ThinCoverage { .. } => !paced.is_empty(),
            ReportWarning::NoSelectedSources => true,
            _ => false,
        })
        .map(ToString::to_string)
        .collect();
    dict.set_item("warnings", warnings)?;
    Ok(dict)
}
