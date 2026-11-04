//! Text printed before and after a run: the [sources] and [ready] lines, the
//! [consumption] block (the CLI's [summary], or the classic block followed by
//! [grid_line] and [logs_line] in Python), and [notes].

use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

use carbontracker::{CatalogReport, SessionReport, SourceDescription, SourceId, Usage};
use carbontracker_providers::Location;

use super::{caveat, clock, equivalents, grid, significant};
use crate::domain::item::{ForecastSource, Outcome};

/// The sources a session measures: power, intensity and forecast, with one
/// power line per device and fidelity, and the provider behind the grid lines.
///
/// ```text
/// power      Apple M2 Pro (cpu, gpu, ane)  measured
/// intensity  DK-DK2                        measured by electricity-maps
/// forecast   DK-DK2, next 24 h             modelled by projection
/// ```
pub fn sources(
    catalog: &CatalogReport,
    served_by: &HashMap<SourceId, &str>,
    forecast: Option<&ForecastSource>,
) -> Vec<String> {
    let mut sources: Vec<_> = catalog.sources.iter().collect();
    sources.sort_by_key(|(id, _)| **id);

    let by = |id: &SourceId, fidelity: String| match served_by.get(id) {
        Some(provider) => format!("{fidelity} by {provider}"),
        None => fidelity,
    };
    let mut devices: BTreeMap<(&str, String), Vec<String>> = BTreeMap::new();
    let mut intensity = Vec::new();
    for (id, source) in sources {
        match source {
            SourceDescription::Power {
                device_name,
                domain,
                fidelity,
                ..
            } => {
                devices
                    .entry((device_name.as_str(), fidelity.to_string()))
                    .or_default()
                    .push(super::domain(*domain));
            }
            SourceDescription::Intensity { location, fidelity } => {
                intensity.push((grid(location), by(id, fidelity.to_string())));
            }
        }
    }
    let forecast: Vec<(String, String)> = forecast
        .map(|forecast| {
            let hours = (forecast.horizon.as_secs_f64() / 3600.0).round();
            (
                format!("{}, next {hours} h", grid(&forecast.location)),
                format!("{} by {}", forecast.fidelity, forecast.served_by),
            )
        })
        .into_iter()
        .collect();
    let mut power: Vec<(String, String)> = devices
        .into_iter()
        .map(|((device, fidelity), domains)| {
            (format!("{device} ({})", domains.join(", ")), fidelity)
        })
        .collect();
    if power.is_empty() {
        power.push(("nothing found".to_owned(), String::new()));
    }

    let rows: Vec<(&str, (String, String))> = [
        ("power", power),
        ("intensity", intensity),
        ("forecast", forecast),
    ]
    .into_iter()
    .flat_map(|(role, rows)| {
        rows.into_iter()
            .enumerate()
            .map(move |(index, row)| (if index == 0 { role } else { "" }, row))
    })
    .collect();
    let width = rows
        .iter()
        .map(|(_, (what, _))| what.chars().count())
        .max()
        .unwrap_or(0);
    rows.into_iter()
        .map(|(role, (what, fidelity))| {
            format!("{role:<10} {what:<width$}  {fidelity}")
                .trim_end()
                .to_owned()
        })
        .collect()
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
            let row = |label: &str, value: &str| format!("\n\t{:<12}{value}", format!("{label}:"));
            block.push_str(&row("Time", &clock(duration)));
            block.push_str(&row("Energy", &format!("{} kWh", significant(kwh))));
            if let Some(intensity) = intensity {
                block.push_str(&row("Intensity", &intensity));
            }
            let co2 = grams.map_or_else(|| UNKNOWN.to_owned(), |grams| format!("{} g", significant(grams)));
            block.push_str(&row("CO2eq", &co2));
            if !equivalents.is_empty() {
                block.push_str("\n\tThis is equivalent to:");
                for (amount, unit) in equivalents {
                    block.push_str(&format!("\n\t{} {unit}", significant(amount)));
                }
            }
            if let Some(logs) = logs {
                block.push_str(&row("Logs", logs));
            }
        }
    }
    block
}

/// The CLI's session summary, titled `<title> summary:`.
pub fn summary(
    title: &str,
    report: &SessionReport,
    logs: Option<&str>,
    with_equivalents: bool,
) -> String {
    let layout = Layout::Summary {
        intensity: intensity(report),
        logs,
    };
    consumption(
        &format!("{title} summary:"),
        report.duration,
        report.total,
        layout,
        with_equivalents,
    )
}

/// The session's mean intensity, grids and fidelity, e.g.
/// `49.0 gCO2eq/kWh avg, DK-DK2 (measured)`. `None` without a grid.
pub fn intensity(report: &SessionReport) -> Option<String> {
    let grids = grids(report)?;
    Some(match mean(report) {
        Some(mean) => format!("{} gCO2eq/kWh avg, {grids}", significant(mean)),
        None => grids,
    })
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

/// Each grid as `DK-DK2 (measured)`, ordered by source id, comma separated.
fn grids(report: &SessionReport) -> Option<String> {
    let grids = grid_list(report);
    (!grids.is_empty()).then(|| join(grids))
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

/// A note per estimated source, then a warning per report warning.
pub fn notes(report: &SessionReport) -> Vec<String> {
    let mut sources: Vec<_> = report.sources.iter().collect();
    sources.sort_by_key(|(id, _)| **id);
    let caveats = sources
        .into_iter()
        .filter_map(|(_, source)| caveat(source))
        .map(|text| note(&text));
    caveats
        .chain(
            report
                .warnings
                .iter()
                .map(|text| warning(&text.to_string())),
        )
        .collect()
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
    fn the_source_lines_name_each_device_once_and_line_up() {
        let served_by = HashMap::from([(SourceId::new(ProviderId(1), 3), "electricity-maps")]);
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
                power("NVIDIA A100", PowerDomain::Gpu, Fidelity::Stated),
                dk2(),
            ]),
            &served_by,
            Some(&forecast),
        );
        assert_eq!(
            lines,
            [
                "power      Apple M2 Pro (cpu, gpu)  measured",
                "           NVIDIA A100 (gpu)        stated",
                "intensity  DK-DK2                   measured by electricity-maps",
                "forecast   DK-DK2, next 24 h        modelled by projection",
            ]
        );
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
            ),
            "python train.py summary:\n\
             \tTime:       0:42:17\n\
             \tEnergy:     0.0448 kWh\n\
             \tIntensity:  49.1 gCO2eq/kWh avg, DK-DK2 (measured)\n\
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

    #[test]
    fn estimates_are_noted() {
        assert_eq!(
            notes(&session()),
            ["note: Intel Xeon is modelled, not measured: treat its energy as an estimate"]
        );
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
