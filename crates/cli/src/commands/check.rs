//! `carbontracker check`: how each value would be obtained, and where each
//! key came from.

use std::io::{self, IsTerminal};

use carbontracker::{Fidelity, PowerDomain, SourceDescription};
use carbontracker_providers::{CandidateOutcome, KeyedService, Location, Resolution, Verdict};

use crate::args::{Args, Keys};

/// Probe, and print one row per value: how it is obtained and by whom. With
/// `-v`, every candidate's verdict follows.
pub fn check(args: &Args) -> Result<i32, String> {
    let keys = args.keys()?;
    let registry = args.registry(&keys)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start: {error}"))?;
    let resolution = runtime.block_on(registry.probe());

    let colour = io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let mut rows = rows(&resolution);
    rows.extend(key_rows(&keys));
    rows.push(providers_row(&resolution));
    print!("{}", render(&rows, &warnings(resolution.candidates(), &keys), colour));
    if args.verbose {
        println!("\n{resolution}");
    }
    Ok(0)
}

/// How a value is obtained, best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum How {
    Live,
    Stated,
    Modelled,
    Fallback,
}

impl How {
    fn of(fidelity: Fidelity) -> Self {
        match fidelity {
            Fidelity::Measured => Self::Live,
            Fidelity::Stated => Self::Stated,
            Fidelity::Modelled => Self::Modelled,
            Fidelity::Assumed => Self::Fallback,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Stated => "stated",
            Self::Modelled => "modelled",
            Self::Fallback => "fallback",
        }
    }

    /// ANSI colour: green for live or stated, yellow for modelled, red for
    /// fallback.
    fn colour(self) -> &'static str {
        match self {
            Self::Live | Self::Stated => "32",
            Self::Modelled => "33",
            Self::Fallback => "31",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    label: &'static str,
    how: Option<How>,
    what: String,
}

const DOMAINS: [PowerDomain; 5] = [PowerDomain::Cpu, PowerDomain::Gpu, PowerDomain::Ane, PowerDomain::Ram, PowerDomain::System];

fn rows(resolution: &Resolution) -> Vec<Row> {
    let mut power = Vec::new();
    let mut intensity = None;
    for (source, description) in resolution.sources() {
        match description {
            SourceDescription::Power { domain, fidelity, .. } => power.push((*domain, How::of(*fidelity))),
            SourceDescription::Intensity { location, fidelity } => {
                let what = match resolution.served_by(source).unwrap_or("unknown") {
                    "static" => "the given value".to_owned(),
                    "country-average" => format!("country average for {location}"),
                    "electricity-maps" => format!("Electricity Maps for {location}"),
                    "global-average" => "world average".to_owned(),
                    name => format!("{name}: {location}"),
                };
                intensity = Some(Row { label: "intensity", how: Some(How::of(*fidelity)), what });
            }
        }
    }
    let mut rows = power_rows(&power);

    let pinned = intensity.as_ref().is_some_and(|row| row.how == Some(How::Stated));
    match resolution.location() {
        Some(Location::Stated(location)) => {
            rows.push(Row { label: "location", how: Some(How::Stated), what: location.to_string() });
        }
        Some(Location::Estimated(estimate)) => {
            let what = match estimate.by {
                "ip-geolocation" => format!("{estimate}, from this IP"),
                by => format!("{estimate}, by {by}"),
            };
            rows.push(Row { label: "location", how: Some(How::Live), what });
        }
        // A pinned intensity needs no location.
        None if pinned => {}
        None => rows.push(Row { label: "location", how: Some(How::Fallback), what: "none, so the world average".into() }),
    }
    rows.extend(intensity);

    if let Some(forecaster) = resolution.forecaster() {
        let hours = forecaster.horizon().as_secs_f64() / 3600.0;
        let what = match forecaster.served_by() {
            "static" => "the given value".to_owned(),
            "projection" => "current intensity held flat".to_owned(),
            "electricity-maps-forecast" => "Electricity Maps forecast".to_owned(),
            name => name.to_owned(),
        };
        rows.push(Row { label: "forecast", how: Some(How::of(forecaster.fidelity())), what: format!("{what}, {hours} h") });
    }
    rows
}

/// Every provider serving, in chain order, once each.
fn providers_row(resolution: &Resolution) -> Row {
    let mut providers: Vec<&str> = Vec::new();
    for outcome in resolution.candidates() {
        if matches!(outcome.verdict, Verdict::Serving(_)) && !providers.contains(&outcome.name) {
            providers.push(outcome.name);
        }
    }
    Row { label: "providers", how: None, what: providers.join(", ") }
}

/// One power row per way of obtaining it, best first, naming its components.
fn power_rows(power: &[(PowerDomain, How)]) -> Vec<Row> {
    let mut hows: Vec<How> = power.iter().map(|(_, how)| *how).collect();
    hows.sort();
    hows.dedup();
    hows.into_iter()
        .map(|how| {
            let domains: Vec<String> = DOMAINS
                .iter()
                .filter(|domain| power.iter().any(|(served, served_how)| served == *domain && *served_how == how))
                .map(ToString::to_string)
                .collect();
            Row { label: "power", how: Some(how), what: domains.join(", ") }
        })
        .collect()
}

fn key_rows(keys: &Keys) -> Vec<Row> {
    KeyedService::ALL
        .into_iter()
        .map(|service| {
            let what = match keys.get(&service) {
                Some((key, origin)) => format!("{} {}, from {origin}", service.name(), key.hint()),
                None => format!("none for {}; run `carbontracker init`", service.name()),
            };
            Row { label: "key", how: None, what }
        })
        .collect()
}

/// The candidate a keyed service's key unlocks.
fn candidate_of(service: KeyedService) -> &'static str {
    match service {
        KeyedService::ElectricityMaps => "electricity-maps",
    }
}

/// A key is set, yet the candidate it unlocks declined.
fn warnings(candidates: &[CandidateOutcome], keys: &Keys) -> Vec<String> {
    keys.iter()
        .filter(|(service, _)| {
            candidates.iter().any(|outcome| outcome.name == candidate_of(**service) && outcome.verdict == Verdict::Declined)
        })
        .map(|(service, (key, _))| {
            format!(
                "! {} key {} found, but {} is unavailable\n    run `carbontracker init` to fix the key",
                service.name(),
                key.hint(),
                candidate_of(*service)
            )
        })
        .collect()
}

fn render(rows: &[Row], warnings: &[String], colour: bool) -> String {
    let paint = |code: &str, text: String| if colour { format!("\x1b[{code}m{text}\x1b[0m") } else { text };
    let mut out = String::from("\n");
    for row in rows {
        let label = paint("2", format!("{:<10}", row.label));
        match row.how {
            Some(how) => {
                let how = paint(how.colour(), format!("{:<10}", how.word()));
                out.push_str(&format!("  {label} {how} {}\n", row.what));
            }
            None => out.push_str(&format!("  {label} {}\n", row.what)),
        }
    }
    for warning in warnings {
        out.push_str(&format!("\n  {}\n", paint("33", warning.clone())));
    }
    out
}

#[cfg(test)]
mod tests {
    use carbontracker_providers::ApiKey;

    use super::*;
    use crate::args::Origin;

    fn keyed() -> Keys {
        Keys::from([(KeyedService::ElectricityMaps, (ApiKey::new("secret-1234"), Origin::ConfigFile))])
    }

    fn outcome(name: &'static str, verdict: Verdict) -> CandidateOutcome {
        CandidateOutcome { name, verdict }
    }

    /// AC/FE-61
    #[test]
    fn a_key_whose_candidate_declined_is_warned_about() {
        let candidates = [outcome("electricity-maps", Verdict::Declined)];
        let [warning] = warnings(&candidates, &keyed()).try_into().unwrap();
        assert!(warning.starts_with("! electricity_maps key …1234 found, but electricity-maps is unavailable"));
        assert!(!warning.contains("secret"));
    }

    /// AC/FE-61
    #[test]
    fn no_warning_when_the_candidate_serves_was_not_asked_or_there_is_no_key() {
        let serving = [outcome("electricity-maps", Verdict::Serving(vec!["DK-DK2".into()]))];
        assert!(warnings(&serving, &keyed()).is_empty());
        let pinned = [outcome("static", Verdict::Serving(vec![]))];
        assert!(warnings(&pinned, &keyed()).is_empty());
        let declined = [outcome("electricity-maps", Verdict::Declined)];
        assert!(warnings(&declined, &Keys::new()).is_empty());
    }

    #[test]
    fn power_is_grouped_by_how_each_component_is_obtained() {
        let power = [(PowerDomain::Gpu, How::Live), (PowerDomain::Cpu, How::Stated), (PowerDomain::Ram, How::Live), (PowerDomain::Gpu, How::Live)];
        let rows = power_rows(&power);
        assert_eq!(rows, [
            Row { label: "power", how: Some(How::Live), what: "gpu, ram".into() },
            Row { label: "power", how: Some(How::Stated), what: "cpu".into() },
        ]);
    }

    #[test]
    fn rows_are_aligned_and_painted_only_with_colour() {
        let rows = [
            Row { label: "power", how: Some(How::Live), what: "ioreport (cpu)".into() },
            Row { label: "key", how: None, what: "electricity_maps …1234, from the config file".into() },
        ];
        let plain = render(&rows, &[], false);
        assert_eq!(plain, "\n  power      live       ioreport (cpu)\n  key        electricity_maps …1234, from the config file\n");
        let painted = render(&rows, &[], true);
        assert!(painted.contains("\x1b[32mlive      \x1b[0m"), "{painted:?}");
        assert!(painted.contains("\x1b[2mpower     \x1b[0m"), "{painted:?}");
    }
}
