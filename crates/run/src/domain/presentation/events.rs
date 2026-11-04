//! Formats engine events as lines, for the console at everything
//! verbosity (the CLI's `-v`).

use carbontracker::{SampleKind, Sample, SourceDescription, SpanReport};

use super::{clock, significant};

/// Said when events were lost.
pub const GAP: &str = "events were lost here: the record is incomplete";

pub fn started(name: &str, parent: Option<&str>) -> String {
    match parent {
        Some(parent) => format!("{name} started in {parent}"),
        None => format!("{name} started"),
    }
}

pub fn stopped(name: &str) -> String {
    format!("{name} stopped")
}

pub fn marked(name: &str, kind: &str, amount: f64) -> String {
    format!("{name}: {kind} +{amount}")
}

pub fn refused(name: &str, error: &str) -> String {
    format!("refused: {name}: {error}")
}

/// A readout line listing each source's latest value.
pub fn readout(figures: &[String]) -> String {
    format!("readout: {}", figures.join(", "))
}

/// A source that failed during a readout.
pub fn source_failed(label: &str, error: &str) -> String {
    format!("{label}: {error}")
}

/// The latest sample with its unit; `None` without samples.
pub fn figure(description: Option<&SourceDescription>, samples: &[Sample]) -> Option<String> {
    let latest = significant(samples.last()?.value());
    Some(match description {
        Some(SourceDescription::Power {
            sample_kind: SampleKind::Total,
            ..
        }) => format!("{latest} J"),
        Some(SourceDescription::Power { .. }) => format!("{latest} W"),
        Some(SourceDescription::Intensity { .. }) => format!("{latest} gCO2eq/kWh"),
        None => latest,
    })
}

/// A settled span's line, followed by its warnings.
pub fn settled(report: &SpanReport) -> Vec<String> {
    let usage = report.total;
    let mut lines = vec![format!(
        "{} settled: {}, {} kWh, {}",
        report.span_name,
        clock(report.duration),
        significant(usage.energy.to_kilowatt_hours().get()),
        usage.emissions.map_or_else(
            || "CO2eq unknown".to_owned(),
            |grams| format!("{} gCO2eq", significant(grams.get()))
        ),
    )];
    lines.extend(
        report
            .warnings
            .iter()
            .map(|warning| format!("  {}: {warning}", report.span_name)),
    );
    lines
}
