//! User-facing text, shared by the CLI, the Python package and the
//! [ConsoleSink](crate::ConsoleSink).
//!
//! These are pure functions that format data as lines; callers decide where
//! and when to print them.
//!
//! - this module: the line prefix, labels and number formatting;
//! - [console]: the blocks printed before and after a run;
//! - [events]: one line per engine event, for `-v`;
//! - [report]: runs read back from their records.

pub mod console;
pub mod events;
pub mod report;

use std::sync::LazyLock;
use std::time::Duration;

use carbontracker::{GridLocation, PowerDomain, PowerUsageEffectiveness, SourceDescription};

pub use console::*;

/// The prefix of every printed line.
pub const PREFIX: &str = "CarbonTracker: ";

/// Names a sink that failed and stopped.
pub fn sink_failed(sink: &str, error: &std::io::Error) -> String {
    format!("the {sink} sink failed and stopped: {error}")
}

/// Adds [PREFIX] to each line of `text`.
pub fn prefixed(text: &str) -> String {
    text.lines()
        .map(|line| format!("{PREFIX}{line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// CO2eq conversion factors (`co2eq.csv`). Only the last row is used.
const CONVERSIONS: &str = include_str!("../../../data/co2eq.csv");

struct Conversion {
    grams_per_unit: f64,
    unit: String,
    lower: f64,
    upper: f64,
}

static LATEST_CONVERSION: LazyLock<Option<Conversion>> = LazyLock::new(|| {
    CONVERSIONS
        .lines()
        .skip(1)
        .filter(|row| !row.trim().is_empty())
        .last()
        .and_then(|row| {
            // name, gCO2eq_per_unit, unit, lowerbound, upperbound, source
            let fields: Vec<&str> = row.split(',').collect();
            let [_, per_unit, unit, lower, upper, ..] = fields[..] else {
                return None;
            };
            Some(Conversion {
                grams_per_unit: per_unit.parse().ok()?,
                unit: unit.to_owned(),
                lower: lower.parse().ok()?,
                upper: upper.parse().ok()?,
            })
        })
});

/// What `grams` of CO2eq is equivalent to, as `(amount, unit)` pairs.
pub fn equivalents(grams: f64) -> Vec<(f64, String)> {
    LATEST_CONVERSION
        .iter()
        .filter(|conversion| (conversion.lower..=conversion.upper).contains(&grams))
        .map(|conversion| (grams / conversion.grams_per_unit, conversion.unit.clone()))
        .collect()
}

/// A line stating the PUE and its source.
pub fn pue_note(pue: PowerUsageEffectiveness) -> String {
    if pue == PowerUsageEffectiveness::UPTIME_2023 {
        format!(
            "Energy includes a PUE coefficient of {} (Daniel Bizo, 2023, Uptime Institute Global Data \
             Center Survey).",
            pue.get()
        )
    } else if pue == PowerUsageEffectiveness::default() {
        "Energy includes no PUE coefficient (1.0).".to_owned()
    } else {
        format!(
            "Energy includes a PUE coefficient of {}, as stated.",
            pue.get()
        )
    }
}

/// Formats a duration as `h:mm:ss`.
pub fn clock(duration: Duration) -> String {
    let seconds = duration.as_secs_f64().round() as u64;
    format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// Formats `value` with three significant figures and no exponent.
pub fn significant(value: f64) -> String {
    if value == 0.0 || !value.is_finite() {
        return format!("{value}");
    }
    let digits = (3 - 1 - value.abs().log10().floor() as i32).max(0) as usize;
    format!("{value:.digits$}")
}

/// A display label for a grid location.
pub fn grid(location: &GridLocation) -> String {
    location.to_string()
}

/// A display label for a recorded location, e.g. `zone DK-DK2` or
/// `DK (55.68, 12.57)`.
pub fn location(location: &crate::domain::record::Location) -> String {
    use crate::domain::record::{Grid, Location};
    match location {
        Location::Stated(Grid::Zone(zone)) => format!("zone {zone}"),
        Location::Stated(Grid::Country(country)) => format!("country {country}"),
        Location::Stated(Grid::Coordinates([latitude, longitude])) => {
            format!("({latitude:.2}, {longitude:.2})")
        }
        Location::Stated(Grid::DataCenter { provider, region }) => {
            format!("data centre {provider} {region}")
        }
        Location::Stated(Grid::Global) => "global".to_owned(),
        Location::Estimated {
            country,
            coordinates: Some([latitude, longitude]),
            ..
        } => {
            format!("{country} ({latitude:.2}, {longitude:.2})")
        }
        Location::Estimated {
            country,
            coordinates: None,
            ..
        } => country.clone(),
    }
}

/// A display label for a power domain.
pub fn domain(domain: PowerDomain) -> String {
    domain.to_string()
}

/// A display label for a source.
pub fn source(description: &SourceDescription) -> String {
    match description {
        SourceDescription::Power {
            device_name,
            domain: power,
            ..
        } => format!("{device_name} ({})", domain(*power)),
        SourceDescription::Intensity { location, .. } => format!("grid {}", grid(location)),
    }
}

/// A caveat for a source that is not measured; `None` if it is.
pub fn caveat(description: &SourceDescription) -> Option<String> {
    let fidelity = description.fidelity();
    if !fidelity.needs_caveat() {
        return None;
    }
    Some(match description {
        SourceDescription::Power { device_name, .. } => {
            format!("{device_name} is {fidelity}, not measured: treat its energy as an estimate")
        }
        SourceDescription::Intensity {
            location: GridLocation::Global,
            ..
        } => "no location was found, so emissions use the world-average grid".to_owned(),
        SourceDescription::Intensity { location, .. } => {
            format!(
                "the grid intensity for {} is {fidelity}, not live",
                grid(location)
            )
        }
    })
}

#[cfg(test)]
mod tests {
    use carbontracker::{Fidelity, SampleKind, PowerScope};

    use super::*;

    #[test]
    fn every_line_gets_the_prefix() {
        assert_eq!(
            prefixed("one\n\ttwo"),
            "CarbonTracker: one\nCarbonTracker: \ttwo"
        );
    }

    #[test]
    fn durations_read_as_hours_minutes_seconds() {
        assert_eq!(clock(Duration::from_secs(0)), "0:00:00");
        assert_eq!(clock(Duration::from_millis(59_600)), "0:01:00");
        assert_eq!(clock(Duration::from_secs(3 * 3600 + 7 * 60 + 5)), "3:07:05");
    }

    #[test]
    fn three_significant_figures() {
        assert_eq!(significant(0.0123456), "0.0123");
        assert_eq!(significant(21.33), "21.3");
        assert_eq!(significant(1234.4), "1234");
        assert_eq!(significant(0.0000194), "0.0000194");
    }

    #[test]
    fn the_pue_note_names_its_source() {
        assert!(pue_note(PowerUsageEffectiveness::UPTIME_2023).contains("Uptime Institute"));
        assert!(pue_note(PowerUsageEffectiveness::new(1.58).unwrap()).contains("Uptime Institute"));
        assert!(pue_note(PowerUsageEffectiveness::default()).contains("no PUE"));
    }

    #[test]
    fn only_estimates_get_a_caveat() {
        let cpu = |fidelity| SourceDescription::Power {
            device_name: "Intel Xeon".to_owned(),
            domain: PowerDomain::Cpu,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity,
        };
        assert_eq!(caveat(&cpu(Fidelity::Measured)), None);
        assert!(
            caveat(&cpu(Fidelity::Modelled))
                .unwrap()
                .contains("Intel Xeon")
        );
        let world = SourceDescription::Intensity {
            location: GridLocation::Global,
            fidelity: Fidelity::Assumed,
        };
        assert!(caveat(&world).unwrap().contains("world-average"));
    }
}
