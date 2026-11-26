//! Text printed before and after a run: the [sources] and [ready] lines, the
//! [consumption] block (the CLI's [summary], or the classic block followed by
//! [grid_line] and [logs_line] in Python), and the warnings for sources
//! nobody asked to estimate ([start_warnings]).

use std::collections::HashMap;
use std::time::Duration;

use carbontracker::{CatalogReport, Fidelity, GridLocation, PowerDomain, SessionReport, SourceDescription, SourceId, Usage};
use carbontracker_providers::{national_average, Location, WORLD_AVERAGE_G_CO2_PER_KWH};

use super::{caveat, clock, equivalents, grid, significant};
use crate::domain::item::{ForecastSource, Outcome};

/// How a value is obtained, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum How {
    Live,
    Stated,
    Fallback,
}

impl How {
    pub fn of(fidelity: Fidelity) -> Self {
        match fidelity {
            Fidelity::Measured => Self::Live,
            Fidelity::Stated => Self::Stated,
            Fidelity::Modelled | Fidelity::Assumed => Self::Fallback,
        }
    }

    pub fn word(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Stated => "stated",
            Self::Fallback => "fallback",
        }
    }
}

/// One setup line: a kind (empty on a further line of the same kind), how
/// the value is obtained, what it is, and the provider behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceLine {
    pub kind: &'static str,
    pub how: Option<How>,
    pub what: String,
    pub provider: Option<String>,
}

impl SourceLine {
    pub fn plain(&self) -> String {
        let named = provided(self.what.clone(), self.provider.as_deref());
        match self.how {
            Some(how) => format!("{:<10} {:<10} {named}", self.kind, how.word()),
            None => format!("{:<10} {named}", self.kind),
        }
    }
}

/// The sources a session measures: power, one line per provider, then
/// intensity and forecast, each with how it is obtained and what it is, in
/// the words `check` uses too. A fallback names what it falls back to.
///
/// ```text
/// power      live       system (macmon)
///            live       cpu, gpu, ane, ram (ioreport)
/// intensity  fallback   national average for DK, 148 gCO2eq/kWh (country-average)
/// forecast   fallback   projects the current intensity (projection)
/// ```
pub fn source_lines(
    catalog: &CatalogReport,
    served_by: &HashMap<SourceId, &str>,
    forecast: Option<&ForecastSource>,
) -> Vec<SourceLine> {
    let mut sources: Vec<_> = catalog.sources.iter().collect();
    sources.sort_by_key(|(id, _)| **id);

    let mut parts = Vec::new();
    let mut lines = Vec::new();
    for (id, source) in &sources {
        let provider = served_by.get(id).copied().unwrap_or("unknown");
        if let SourceDescription::Power { domain, fidelity, .. } = source {
            parts.push(PowerPart { provider, domain: *domain, fidelity: *fidelity, watts: None });
        }
    }
    let groups = power_groups(&parts);
    if groups.is_empty() {
        lines.push(SourceLine { kind: "power", how: None, what: "nothing found".to_owned(), provider: None });
    }
    for (index, group) in groups.iter().enumerate() {
        lines.push(SourceLine {
            kind: if index == 0 { "power" } else { "" },
            how: Some(group.how),
            what: group.parts(false),
            provider: Some(group.provider.to_owned()),
        });
    }
    for (id, source) in &sources {
        if let SourceDescription::Intensity { location, fidelity } = source {
            lines.push(SourceLine {
                kind: "intensity",
                how: Some(How::of(*fidelity)),
                what: intensity_what(location, *fidelity),
                provider: Some(served_by.get(id).copied().unwrap_or("unknown").to_owned()),
            });
        }
    }
    if let Some(forecast) = forecast {
        lines.push(SourceLine {
            kind: "forecast",
            how: Some(How::of(forecast.fidelity)),
            what: forecast_what(forecast),
            provider: Some(forecast.served_by.to_owned()),
        });
    }
    lines
}

/// [source_lines] as plain text.
pub fn sources(
    catalog: &CatalogReport,
    served_by: &HashMap<SourceId, &str>,
    forecast: Option<&ForecastSource>,
) -> Vec<String> {
    source_lines(catalog, served_by, forecast).iter().map(SourceLine::plain).collect()
}

/// One power source: its provider, what it reads, how, and its watts once read.
#[derive(Debug, Clone, Copy)]
pub struct PowerPart<'a> {
    pub provider: &'a str,
    pub domain: PowerDomain,
    pub fidelity: Fidelity,
    pub watts: Option<f64>,
}

/// A provider's power sources: each part once, with its total watts.
#[derive(Debug, Clone)]
pub struct PowerGroup<'a> {
    pub provider: &'a str,
    /// The least measured of its parts.
    pub how: How,
    pub parts: Vec<(PowerDomain, Option<f64>)>,
}

impl PowerGroup<'_> {
    /// `cpu, gpu`, or with `watts` each part's total, `cpu 0.7 W, gpu 0.6 W`,
    /// `…` until it has read.
    pub fn parts(&self, watts: bool) -> String {
        self.parts
            .iter()
            .map(|(domain, value)| match (watts, value) {
                (false, _) => super::domain(*domain),
                (true, Some(value)) => format!("{} {value:.1} W", super::domain(*domain)),
                (true, None) => format!("{} …", super::domain(*domain)),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Power by provider, in the order first seen, summing a part read twice.
pub fn power_groups<'a>(parts: &[PowerPart<'a>]) -> Vec<PowerGroup<'a>> {
    let mut groups: Vec<PowerGroup> = Vec::new();
    for part in parts {
        let index = match groups.iter().position(|group| group.provider == part.provider) {
            Some(index) => index,
            None => {
                groups.push(PowerGroup { provider: part.provider, how: How::of(part.fidelity), parts: Vec::new() });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        group.how = group.how.max(How::of(part.fidelity));
        match group.parts.iter_mut().find(|(seen, _)| *seen == part.domain) {
            Some((_, total)) => *total = total.zip(part.watts).map(|(a, b)| a + b),
            None => group.parts.push((part.domain, part.watts)),
        }
    }
    groups
}

/// Power by provider, one line each: `cpu, gpu (ioreport)`, or with `watts`
/// `cpu 0.7 W, gpu 0.6 W (ioreport)`.
pub fn power_what(parts: &[PowerPart], watts: bool) -> String {
    power_groups(parts)
        .iter()
        .map(|group| provided(group.parts(watts), Some(group.provider)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An intensity source: where it reads, the given value, or the average it
/// falls back to with its figure.
pub fn intensity_what(location: &GridLocation, fidelity: Fidelity) -> String {
    match (fidelity, location) {
        (Fidelity::Stated, _) => "the given value".to_owned(),
        (Fidelity::Measured, _) => grid(location),
        (_, GridLocation::Global) => format!("world average, {WORLD_AVERAGE_G_CO2_PER_KWH} gCO2eq/kWh"),
        (_, GridLocation::Country { country_code }) => {
            let country = country_code.to_uppercase();
            match national_average(&country) {
                Some(figure) => format!("national average for {country}, {figure:.0} gCO2eq/kWh"),
                None => format!("national average for {country}"),
            }
        }
        (_, location) => grid(location),
    }
}

/// The forecast: published, or projecting the current or given value.
pub fn forecast_what(forecast: &ForecastSource) -> String {
    match forecast.fidelity {
        Fidelity::Measured => grid(&forecast.location),
        Fidelity::Stated => "projects the given value".to_owned(),
        Fidelity::Modelled | Fidelity::Assumed => "projects the current intensity".to_owned(),
    }
}

/// What a source reads: its part for power, its place for intensity.
pub fn part(description: &SourceDescription) -> String {
    match description {
        SourceDescription::Power { domain: power, .. } => super::domain(*power),
        SourceDescription::Intensity { location, .. } => grid(location),
    }
}

/// `what (provider)`, the one way every source names its provider. A
/// provider's own qualifier, as in `neso (direct CO2)`, goes with what it
/// reads: `GB direct CO2 (neso)`.
pub fn provided(what: String, provider: Option<&str>) -> String {
    match provider.map(qualified) {
        Some((name, Some(qualifier))) => format!("{what} {qualifier} ({name})"),
        Some((name, None)) => format!("{what} ({name})"),
        None => what,
    }
}

/// A provider's name and its own qualifier: `neso (direct CO2)` gives
/// `neso` and `direct CO2`.
pub fn qualified(provider: &str) -> (&str, Option<&str>) {
    match provider.split_once(" (") {
        Some((name, qualifier)) => (name, Some(qualifier.trim_end_matches(')'))),
        None => (provider, None),
    }
}

/// Shown while a provider is probed, e.g. `probing nvml…`.
pub fn probing(candidate: &str) -> String {
    format!("probing {candidate}…")
}

/// What [ready] announces.
pub enum Ready<'a> {
    /// The CLI is about to run this command.
    Command(&'a str),
    /// Python tracking has started.
    Tracking,
}

/// The line printed before the tracked program's output.
pub fn ready(ready: Ready<'_>) -> String {
    match ready {
        Ready::Command(command) => format!("ready. Starting {command}"),
        Ready::Tracking => "ready. Tracking started.".to_owned(),
    }
}

/// A summary block: its title, then its rows.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub title: String,
    pub rows: Vec<Row>,
}

/// One row of a [Summary].
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// `Label:` aligned, its figure, and a note after it, e.g. `(average)`.
    Field { label: &'static str, figure: String, note: Option<&'static str> },
    /// A line that introduces the ones after it.
    Heading(&'static str),
    Text(String),
}

impl Summary {
    pub fn plain(&self) -> String {
        let mut block = self.title.clone();
        for row in &self.rows {
            block.push_str("\n\t");
            match row {
                Row::Field { label, figure, note } => {
                    block.push_str(&format!("{:<12}{figure}", format!("{label}:")));
                    if let Some(note) = note {
                        block.push_str(&format!(" {note}"));
                    }
                }
                Row::Heading(text) => block.push_str(text),
                Row::Text(text) => block.push_str(text),
            }
        }
        block
    }
}

/// The summary layout's rows: time, energy, intensity, CO2eq, what it
/// equals, and the log folder.
pub fn summary_rows(
    title: &str,
    duration: Duration,
    usage: Usage,
    intensity: Option<String>,
    logs: Option<&str>,
    with_equivalents: bool,
) -> Summary {
    let kwh = usage.energy.to_kilowatt_hours().get();
    let grams = usage.emissions.map(|grams| grams.get());
    let field = |label, figure: String| Row::Field { label, figure, note: None };
    let mut rows = vec![field("Time", clock(duration)), field("Energy", format!("{} kWh", significant(kwh)))];
    if let Some(intensity) = intensity {
        rows.push(Row::Field { label: "Intensity", figure: intensity, note: Some("(average)") });
    }
    rows.push(field("CO2eq", grams.map_or_else(|| UNKNOWN.to_owned(), |grams| format!("{} g", significant(grams)))));
    let equivalents = match grams {
        Some(grams) if with_equivalents => equivalents(grams),
        _ => Vec::new(),
    };
    if !equivalents.is_empty() {
        rows.push(Row::Heading("This is equivalent to:"));
        rows.extend(equivalents.into_iter().map(|(amount, unit)| Row::Text(format!("{} {unit}", significant(amount)))));
    }
    if let Some(logs) = logs {
        rows.push(field("Logs", logs.to_owned()));
    }
    Summary { title: title.to_owned(), rows }
}

/// The layout of a [consumption] block.
pub enum Layout<'a> {
    /// Tab-separated labels, numbers at `decimals`.
    Legacy { decimals: usize },
    /// Aligned labels, three significant figures, and optional `Intensity`
    /// and `Logs` rows.
    Summary {
        intensity: Option<String>,
        logs: Option<&'a str>,
    },
}

/// The consumption block. In the legacy layout at 12 decimals:
///
/// ```text
/// Actual consumption for 3 epoch(s):
///         Time:   0:03:29
///         Energy: 0.012300000000 kWh
///         CO2eq:  1.850000000000 g
///         This is equivalent to:
///         0.017322097378 km travelled by car
/// ```
/// What is printed for emissions that are absent.
pub const UNKNOWN: &str = "unknown (no grid intensity)";

pub fn consumption(
    title: &str,
    duration: Duration,
    usage: Usage,
    layout: Layout<'_>,
    with_equivalents: bool,
) -> String {
    let kwh = usage.energy.to_kilowatt_hours().get();
    let grams = usage.emissions.map(|grams| grams.get());
    let equivalents = match grams {
        Some(grams) if with_equivalents => equivalents(grams),
        _ => Vec::new(),
    };
    let mut block = String::from(title);
    match layout {
        Layout::Legacy { decimals } => {
            block.push_str(&format!(
                "\n\tTime:\t{}\n\tEnergy:\t{kwh:.decimals$} kWh\n\tCO2eq:\t{}",
                clock(duration),
                grams.map_or_else(|| UNKNOWN.to_owned(), |grams| format!("{grams:.decimals$} g"))
            ));
            if !equivalents.is_empty() {
                block.push_str("\n\tThis is equivalent to:");
                for (amount, unit) in equivalents {
                    block.push_str(&format!("\n\t{amount:.decimals$} {unit}"));
                }
            }
        }
        Layout::Summary { intensity, logs } => {
            return summary_rows(title, duration, usage, intensity, logs, with_equivalents).plain();
        }
    }
    block
}

/// The CLI's session summary, titled `<title> summary:`.
pub fn summary(title: &str, report: &SessionReport, logs: Option<&str>, with_equivalents: bool) -> Summary {
    let title = format!("{title} summary:");
    summary_rows(&title, report.duration, report.total, intensity(report), logs, with_equivalents)
}

/// The session's mean intensity, e.g. `49.0 gCO2eq/kWh`; the setup lines
/// already named the grid. `None` without one.
pub fn intensity(report: &SessionReport) -> Option<String> {
    mean(report).map(|mean| format!("{} gCO2eq/kWh", significant(mean)))
}

/// The grid line printed after the consumption block in Python, e.g.
/// `grid DK-DK2, 49.0 gCO2eq/kWh avg (measured)`. `None` without a grid.
pub fn grid_line(report: &SessionReport) -> Option<String> {
    let mut grids = grid_list(report);
    if grids.is_empty() {
        return None;
    }
    let mean = mean(report).map(|mean| format!("{} gCO2eq/kWh avg", significant(mean)));
    Some(match (grids.len(), mean) {
        (1, Some(mean)) => {
            let (location, fidelity) = grids.remove(0);
            format!("grid {location}, {mean} ({fidelity})")
        }
        (_, Some(mean)) => format!("grid {}, {mean}", join(grids)),
        (_, None) => format!("grid {}", join(grids)),
    })
}

/// The session's emissions over its energy; `None` with no energy.
fn mean(report: &SessionReport) -> Option<f64> {
    let kwh = report.total.energy.to_kilowatt_hours().get();
    let grams = report.total.emissions?.get();
    (kwh > 0.0).then(|| grams / kwh)
}

fn grid_list(report: &SessionReport) -> Vec<(String, String)> {
    let mut intensities: Vec<_> = report.intensities.iter().collect();
    intensities.sort_by_key(|(id, _)| **id);
    intensities
        .into_iter()
        .filter_map(|(_, description)| match description {
            SourceDescription::Intensity { location, fidelity } => Some((grid(location), fidelity.to_string())),
            SourceDescription::Power { .. } => None,
        })
        .collect()
}

fn join(grids: Vec<(String, String)>) -> String {
    grids.into_iter().map(|(location, fidelity)| format!("{location} ({fidelity})")).collect::<Vec<_>>().join(", ")
}

/// The log files' location: `logs in <path>`.
pub fn logs_line(logs: &str) -> String {
    format!("logs in {logs}")
}

/// Why the grid is the world average, which nobody asked for: no location,
/// or no provider serves it.
pub fn assumed_grid(location: Option<&Location>, keyed: bool) -> String {
    let Some(location) = location else {
        return "the world-average grid is assumed: no location could be estimated from this IP; \
                state a zone, country or coordinates"
            .to_owned();
    };
    let key = if keyed {
        "Electricity Maps declined its key"
    } else {
        "Electricity Maps has no key (run `carbontracker init`)"
    };
    let table = match location.country() {
        Some(country) => format!("the country table has no {country}"),
        None => "the country table needs a country or zone".to_owned(),
    };
    format!("the world-average grid is assumed for {location}: {key}, and {table}")
}

/// The warnings printed at start: one per source that is modelled or assumed
/// and was not asked for, in source order. A stated figure warns nothing.
/// `assumed` is the warning for the world-average grid, such as
/// [assumed_grid]; `None` when the user asked for it.
pub fn start_warnings<'a>(
    sources: impl IntoIterator<Item = (SourceId, &'a SourceDescription)>,
    assumed: Option<&str>,
) -> Vec<String> {
    let mut sources: Vec<_> = sources.into_iter().collect();
    sources.sort_by_key(|(id, _)| *id);
    sources
        .into_iter()
        .filter_map(|(_, source)| match source {
            SourceDescription::Intensity {
                fidelity: Fidelity::Assumed,
                ..
            } => assumed.map(str::to_owned),
            source => caveat(source),
        })
        .collect()
}

/// A source that could not be read, named by what it reads and its provider.
pub fn failed_read(source: &str, provider: Option<&str>, error: &str) -> String {
    format!("{} was not read: {error}; its last value is used", provided(source.to_owned(), provider))
}

/// A command on a span that the engine did not accept.
pub fn refused(span: &str, error: &str) -> String {
    format!("a command on {span} was refused: {error}")
}

/// A span report's warning, naming the span.
pub fn span_warning(span: &str, warning: &str) -> String {
    format!("{span}: {warning}")
}

/// Formats a note.
pub fn note(text: &str) -> String {
    format!("note: {text}")
}

/// Formats a warning.
pub fn warning(text: &str) -> String {
    format!("WARNING - {text}")
}

/// Formats an error.
pub fn error(text: &str) -> String {
    format!("ERROR - {text}")
}

/// Warns that the config file is readable by other users.
pub fn config_readable(path: &std::path::Path) -> String {
    warning(&format!(
        "{} can be read by others: chmod 600 {0}",
        path.display()
    ))
}

/// Describes how a run ended.
pub fn outcome(outcome: &Outcome) -> String {
    let epochs = |done: Option<u64>| {
        done.map(|done| format!(" after {done} epoch(s)"))
            .unwrap_or_default()
    };
    match *outcome {
        Outcome::Completed { epochs_done } => format!("ended: completed{}", epochs(epochs_done)),
        Outcome::Monitored { epochs_done } => {
            format!("ended: tracking stopped after {epochs_done} monitored epoch(s)")
        }
        Outcome::Interrupted { epochs_done } => {
            format!("ended: interrupted{}", epochs(epochs_done))
        }
        Outcome::Failed { epochs_done } => format!(
            "ended: tracking failed{}; the program went on",
            epochs(epochs_done)
        ),
        Outcome::Exited { code } => format!("ended: the command exited with {code}"),
        Outcome::Signalled { signal } => {
            format!("ended: the command was killed by signal {signal}")
        }
    }
}

// Lines printed by the Python tracker.

/// Printed when training stops before all epochs were monitored.
pub fn interrupted(monitor_epochs: usize) -> String {
    format!("Training was interrupted before all {monitor_epochs} epochs were monitored.")
}

/// Printed when tracking finishes.
pub const FINISHED: &str = "Finished monitoring.";

/// Printed when tracking fails and `ignore_errors` is set.
pub fn ignored_error(traceback: &str) -> String {
    format!("Ignored error: {traceback}Continued training without monitoring...")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use carbontracker::{
        Fidelity, GramsCo2e, GridLocation, SampleKind, Joules, PowerDomain, PowerScope,
        ProviderId, SourceId,
    };

    use super::*;

    fn usage(kwh: f64, grams: f64) -> Usage {
        Usage {
            energy: Joules::new(kwh * 3_600_000.0).unwrap(),
            emissions: Some(GramsCo2e::new(grams).unwrap()),
        }
    }

    fn power(device: &str, domain: PowerDomain, fidelity: Fidelity) -> SourceDescription {
        SourceDescription::Power {
            device_name: device.to_owned(),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity,
        }
    }

    fn dk2() -> SourceDescription {
        SourceDescription::Intensity {
            location: GridLocation::Zone {
                zone_id: "DK-DK2".to_owned(),
            },
            fidelity: Fidelity::Measured,
        }
    }

    fn catalog(sources: Vec<SourceDescription>) -> CatalogReport {
        let mut catalog = CatalogReport::default();
        for (slot, source) in sources.into_iter().enumerate() {
            catalog
                .sources
                .insert(SourceId::new(ProviderId(1), slot as u16), source);
        }
        catalog
    }

    fn session() -> SessionReport {
        let sources = catalog(vec![
            power("Intel Xeon", PowerDomain::Cpu, Fidelity::Modelled),
            dk2(),
        ])
        .sources;
        SessionReport {
            per_source: HashMap::new(),
            sources,
            total: usage(0.0448, 2.2),
            claimed: usage(0.04, 2.0),
            unclaimed: usage(0.0048, 0.2),
            intensities: HashMap::from([(SourceId::new(ProviderId(7), 0), dk2())]),
            duration: Duration::from_secs(2537),
            samples: HashMap::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn the_source_lines_name_each_provider_once_and_line_up() {
        let id = |slot| SourceId::new(ProviderId(1), slot);
        let served_by = HashMap::from([(id(0), "ioreport"), (id(1), "ioreport"), (id(2), "nvml"), (id(3), "electricity-maps")]);
        let forecast = ForecastSource {
            location: GridLocation::Zone {
                zone_id: "DK-DK2".to_owned(),
            },
            horizon: Duration::from_secs(24 * 3600),
            fidelity: Fidelity::Modelled,
            served_by: "projection",
        };
        let lines = sources(
            &catalog(vec![
                power("Apple M2 Pro", PowerDomain::Cpu, Fidelity::Measured),
                power("Apple M2 Pro", PowerDomain::Gpu, Fidelity::Measured),
                power("NVIDIA A100", PowerDomain::Gpu, Fidelity::Measured),
                dk2(),
            ]),
            &served_by,
            Some(&forecast),
        );
        assert_eq!(
            lines,
            [
                "power      live       cpu, gpu (ioreport)",
                "           live       gpu (nvml)",
                "intensity  live       DK-DK2 (electricity-maps)",
                "forecast   fallback   projects the current intensity (projection)",
            ]
        );
    }

    #[test]
    fn a_fallback_says_what_it_falls_back_to() {
        let intensity = |location, fidelity| SourceDescription::Intensity { location, fidelity };
        let dk = GridLocation::Country { country_code: "DK".to_owned() };
        let forecast = |fidelity, served_by| ForecastSource {
            location: GridLocation::Global,
            horizon: Duration::from_secs(24 * 3600),
            fidelity,
            served_by,
        };
        let lines = |source, provider, forecast: ForecastSource| {
            let served_by = HashMap::from([(SourceId::new(ProviderId(1), 0), "generic-cpu"), (SourceId::new(ProviderId(1), 1), provider)]);
            sources(&catalog(vec![power("Apple M2 Pro", PowerDomain::Cpu, Fidelity::Modelled), source]), &served_by, Some(&forecast))
        };
        let national = carbontracker_providers::national_average("DK").unwrap();
        assert_eq!(
            lines(intensity(dk, Fidelity::Modelled), "country-average", forecast(Fidelity::Modelled, "projection")),
            [
                "power      fallback   cpu (generic-cpu)".to_owned(),
                format!("intensity  fallback   national average for DK, {national:.0} gCO2eq/kWh (country-average)"),
                "forecast   fallback   projects the current intensity (projection)".to_owned(),
            ]
        );
        assert_eq!(
            lines(intensity(GridLocation::Global, Fidelity::Assumed), "global-average", forecast(Fidelity::Assumed, "projection"))[1],
            "intensity  fallback   world average, 475 gCO2eq/kWh (global-average)"
        );
        assert_eq!(
            lines(intensity(GridLocation::Global, Fidelity::Stated), "static", forecast(Fidelity::Stated, "static"))[1..],
            ["intensity  stated     the given value (static)", "forecast   stated     projects the given value (static)"]
        );
    }

    #[test]
    fn power_names_each_provider_once_with_its_parts_and_their_watts() {
        let part = |provider, domain, watts| PowerPart { provider, domain, fidelity: Fidelity::Measured, watts };
        let parts = [
            part("macmon", PowerDomain::System, Some(7.2)),
            part("ioreport", PowerDomain::Cpu, Some(0.7)),
            part("ioreport", PowerDomain::Gpu, Some(0.3)),
            part("ioreport", PowerDomain::Gpu, Some(0.3)),
            part("ioreport", PowerDomain::Ram, None),
        ];
        assert_eq!(power_what(&parts, true), "system 7.2 W (macmon)\ncpu 0.7 W, gpu 0.6 W, ram … (ioreport)");
        assert_eq!(power_what(&parts, false), "system (macmon)\ncpu, gpu, ram (ioreport)");
    }

    /// AC/OUT-37
    #[test]
    fn each_power_provider_says_how_it_is_obtained() {
        let id = |slot| SourceId::new(ProviderId(1), slot);
        let served_by = HashMap::from([(id(0), "macmon"), (id(1), "generic-cpu")]);
        let lines = sources(
            &catalog(vec![
                power("Apple M2 Pro", PowerDomain::System, Fidelity::Measured),
                power("Apple M2 Pro", PowerDomain::Cpu, Fidelity::Modelled),
            ]),
            &served_by,
            None,
        );
        assert_eq!(lines, ["power      live       system (macmon)", "           fallback   cpu (generic-cpu)"]);
    }

    /// AC/OUT-37
    #[test]
    fn a_providers_own_qualifier_goes_with_the_source_inside_one_pair_of_brackets() {
        assert_eq!(provided("GB".to_owned(), Some("neso (direct CO2)")), "GB direct CO2 (neso)");
        assert_eq!(provided("DK-DK2".to_owned(), Some("energinet")), "DK-DK2 (energinet)");
        assert_eq!(provided("DK-DK2".to_owned(), None), "DK-DK2");
    }

    #[test]
    fn the_ways_a_value_is_obtained_rank_live_first() {
        assert_eq!(How::of(Fidelity::Measured), How::Live);
        assert_eq!(How::of(Fidelity::Modelled), How::Fallback);
        assert_eq!(How::of(Fidelity::Assumed), How::Fallback);
        assert!(How::Live < How::Stated && How::Stated < How::Fallback);
        assert_eq!([How::Live, How::Stated, How::Fallback].map(How::word), ["live", "stated", "fallback"]);
    }

    #[test]
    fn a_catalog_without_power_says_so() {
        assert_eq!(
            sources(&CatalogReport::default(), &HashMap::new(), None),
            ["power      nothing found"]
        );
    }

    #[test]
    fn the_legacy_block_reads_as_carbontracker_2s() {
        let block = consumption(
            "Actual consumption for 3 epoch(s):",
            Duration::from_secs(209),
            usage(0.0123, 1.85),
            Layout::Legacy { decimals: 12 },
            true,
        );
        assert_eq!(
            block,
            "Actual consumption for 3 epoch(s):\n\
             \tTime:\t0:03:29\n\
             \tEnergy:\t0.012300000000 kWh\n\
             \tCO2eq:\t1.850000000000 g\n\
             \tThis is equivalent to:\n\
             \t0.017322097378 km travelled by car"
        );
    }

    #[test]
    fn equivalents_can_be_left_out() {
        let block = consumption(
            "Actual consumption:",
            Duration::ZERO,
            usage(1.0, 100.0),
            Layout::Legacy { decimals: 2 },
            false,
        );
        assert!(!block.contains("equivalent"), "{block}");
    }

    #[test]
    fn the_summary_adds_intensity_and_logs_rows() {
        assert_eq!(
            summary(
                "python train.py",
                &session(),
                Some("logs/run-20261027_141203-k3x9q2m7"),
                true
            )
            .plain(),
            "python train.py summary:\n\
             \tTime:       0:42:17\n\
             \tEnergy:     0.0448 kWh\n\
             \tIntensity:  49.1 gCO2eq/kWh (average)\n\
             \tCO2eq:      2.20 g\n\
             \tThis is equivalent to:\n\
             \t0.0206 km travelled by car\n\
             \tLogs:       logs/run-20261027_141203-k3x9q2m7"
        );
    }

    #[test]
    fn python_gets_the_grid_and_logs_as_lines() {
        assert_eq!(
            grid_line(&session()).unwrap(),
            "grid DK-DK2, 49.1 gCO2eq/kWh avg (measured)"
        );
        assert_eq!(logs_line("logs/x"), "logs in logs/x");
    }

    /// AC/OUT-22, AC/OUT-32
    #[test]
    fn a_modelled_cpu_and_a_world_average_nobody_asked_for_are_each_named() {
        let world = SourceDescription::Intensity {
            location: GridLocation::Global,
            fidelity: Fidelity::Assumed,
        };
        let cpu = power("Intel Xeon", PowerDomain::Cpu, Fidelity::Modelled);
        let sources = catalog(vec![cpu, world.clone()]).sources;
        assert_eq!(
            start_warnings(sources.iter().map(|(id, source)| (*id, source)), Some("the grid is assumed")),
            [
                "Intel Xeon is modelled, not measured: treat its energy as an estimate",
                "the grid is assumed",
            ]
        );

        let stated = power("static", PowerDomain::Cpu, Fidelity::Stated);
        let asked = catalog(vec![stated, world]).sources;
        assert!(start_warnings(asked.iter().map(|(id, source)| (*id, source)), None).is_empty());
    }

    /// AC/RUN-23
    #[test]
    fn an_assumed_grid_says_what_failed() {
        assert!(assumed_grid(None, false).contains("no location could be estimated"));
        let zone = Location::Stated(GridLocation::Zone { zone_id: "XX-1".into() });
        assert_eq!(
            assumed_grid(Some(&zone), true),
            "the world-average grid is assumed for zone XX-1: Electricity Maps declined its key, \
             and the country table has no XX"
        );
    }
}
