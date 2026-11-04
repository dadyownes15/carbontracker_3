//! Converts engine events into [record] lines. This is the
//! only module that depends on both the engine types and the record types.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use carbontracker::{
    Failure, ProviderError, Skip, CatalogReport, GridLocation, SampleKind, Observation, PowerDomain, PowerScope, SessionReport,
    SourceDescription, SourceId, SourceSelection, SpanReport, Timestamp, TrackerEvent, Usage, Work,
};
use carbontracker_providers::{Location, Measure, Resolution, Verdict};

use crate::domain::item::Outcome;
use crate::domain::presentation;
use crate::domain::record::{self, Line};

/// Converts one session's catalog, events and outcome into record lines.
#[derive(Default)]
pub struct Encoder {
    /// Each power source's intensity source, for the session line.
    pairs: HashMap<SourceId, SourceId>,
}

impl Encoder {
    /// Encodes the catalog.
    pub fn catalog(&mut self, catalog: &CatalogReport) -> Line {
        self.pairs = catalog.pairs.clone();
        let mut sources: Vec<_> = catalog.sources.iter().collect();
        sources.sort_by_key(|(id, _)| **id);
        Line::Catalog(record::Catalog {
            sources: sources
                .into_iter()
                .map(|(&id, description)| source(id, description))
                .collect(),
            pairs: catalog.pairs.iter().map(|(power, intensity)| (power.to_string(), intensity.to_string())).collect(),
        })
    }

    /// Encodes an event, with its timestamp if the event carries one.
    pub fn event(&self, event: &TrackerEvent) -> (Option<Timestamp>, Line) {
        match event {
            TrackerEvent::ReadoutLanded { outcomes, .. } => {
                let mut readout = record::Readout {
                    samples: BTreeMap::new(),
                    unchanged: BTreeSet::new(),
                    skipped: BTreeMap::new(),
                    failed: BTreeMap::new(),
                };
                for (id, outcome) in outcomes.iter() {
                    match outcome {
                        Observation::Samples(samples) => {
                            let points = samples
                                .iter()
                                .map(|sample| (*sample.observed_at(), sample.value()))
                                .collect();
                            readout.samples.insert(id.to_string(), points);
                        }
                        Observation::Unchanged => {
                            readout.unchanged.insert(id.to_string());
                        }
                        Observation::Skipped(skip) => {
                            readout.skipped.insert(id.to_string(), skipped(*skip).to_owned());
                        }
                        Observation::Failed(failure) => {
                            readout.failed.insert(id.to_string(), failed(failure));
                        }
                    }
                }
                (None, Line::Readout(readout))
            }
            TrackerEvent::SpanStarted {
                span,
                name,
                parent,
                selection,
                at,
            } => (
                Some(*at),
                Line::SpanStarted(record::SpanStarted {
                    span: span.get() as u64,
                    name: name.clone(),
                    parent: parent.map(|parent| parent.get() as u64),
                    billed: billed(selection),
                }),
            ),
            TrackerEvent::SpanStopped { span, work, at } => (
                Some(*at),
                Line::SpanStopped(record::SpanStopped {
                    span: span.get() as u64,
                    work: entries(work),
                }),
            ),
            TrackerEvent::Marked { span, work, at } => (
                Some(*at),
                Line::Marked(record::Marked {
                    span: span.get() as u64,
                    work: entries(work),
                }),
            ),
            TrackerEvent::CommandRefused { span, error } => (
                None,
                Line::Refused(record::Refused {
                    span: span.get() as u64,
                    error: error.to_string(),
                }),
            ),
            TrackerEvent::SpanSettled { report } => (None, Line::SpanSettled(settled(report))),
            TrackerEvent::SessionEnded { report } => (None, Line::Session(self.session(report))),
        }
    }

    /// Encodes how the run ended.
    pub fn outcome(&self, outcome: &Outcome) -> Line {
        let (ended, epochs_done, code, signal) = match *outcome {
            Outcome::Completed { epochs_done } => {
                (record::Ended::Completed, epochs_done, None, None)
            }
            Outcome::Monitored { epochs_done } => {
                (record::Ended::Monitored, Some(epochs_done), None, None)
            }
            Outcome::Interrupted { epochs_done } => {
                (record::Ended::Interrupted, epochs_done, None, None)
            }
            Outcome::Failed { epochs_done } => (record::Ended::Failed, epochs_done, None, None),
            Outcome::Exited { code } => (record::Ended::Exited, None, Some(code), None),
            Outcome::Signalled { signal } => (record::Ended::Signalled, None, None, Some(signal)),
        };
        Line::Outcome(record::Outcome {
            ended,
            epochs_done,
            code,
            signal,
        })
    }

    fn session(&self, report: &SessionReport) -> record::Session {
        let (energy_kwh, emissions_g) = kwh_and_grams(report.total);
        record::Session {
            duration_s: report.duration.as_secs_f64(),
            energy_kwh,
            emissions_g,
            claimed: usage(report.claimed),
            unclaimed: usage(report.unclaimed),
            per_source: per_source(&report.per_source),
            samples: samples(&report.samples),
            intensities: self.intensities(report),
            warnings: report.warnings.iter().map(ToString::to_string).collect(),
        }
    }
}

impl Encoder {
    /// Each intensity in `report`, ordered by id, with the mean over the power
    /// sources paired with it.
    fn intensities(&self, report: &SessionReport) -> Vec<record::Intensity> {
        let mut intensities: Vec<_> = report.intensities.iter().collect();
        intensities.sort_by_key(|(id, _)| **id);
        intensities
            .into_iter()
            .map(|(&id, description)| {
                let (mut kwh, mut grams) = (0.0, 0.0);
                for (power, usage) in &report.per_source {
                    if self.pairs.get(power) == Some(&id)
                        && let (k, Some(g)) = kwh_and_grams(*usage)
                    {
                        kwh += k;
                        grams += g;
                    }
                }
                record::Intensity {
                    source: id.to_string(),
                    label: presentation::source(description),
                    fidelity: description.fidelity().to_string(),
                    mean_g_per_kwh: (kwh > 0.0).then(|| grams / kwh),
                }
            })
            .collect()
    }
}

fn settled(report: &SpanReport) -> record::SpanSettled {
    let (energy_kwh, emissions_g) = kwh_and_grams(report.total);
    let (exclusive_energy_kwh, exclusive_emissions_g) = kwh_and_grams(report.exclusive);
    record::SpanSettled {
        span: report.span_id.get() as u64,
        name: report.span_name.clone(),
        duration_s: report.duration.as_secs_f64(),
        energy_kwh,
        emissions_g,
        exclusive_energy_kwh,
        exclusive_emissions_g,
        per_source: per_source(&report.per_source),
        samples: samples(&report.samples),
        warnings: report.warnings.iter().map(ToString::to_string).collect(),
    }
}

fn source(id: SourceId, description: &SourceDescription) -> record::Source {
    let mut source = record::Source {
        id: id.to_string(),
        role: String::new(),
        label: presentation::source(description),
        device: None,
        domain: None,
        scope: None,
        location: None,
        unit: String::new(),
        fidelity: description.fidelity().to_string(),
    };
    match description {
        SourceDescription::Power {
            device_name,
            domain: power_domain,
            scope,
            sample_kind,
            ..
        } => {
            source.role = "power".to_owned();
            source.device = Some(device_name.clone());
            source.domain = Some(domain(*power_domain));
            source.scope = Some(
                match scope {
                    PowerScope::DeviceTotal => "device_total",
                    PowerScope::Process { .. } => "process",
                    PowerScope::Job { .. } => "job",
                    PowerScope::Container { .. } => "container",
                }
                .to_owned(),
            );
            source.unit = if *sample_kind == SampleKind::Total {
                "J"
            } else {
                "W"
            }
            .to_owned();
        }
        SourceDescription::Intensity { location, .. } => {
            source.role = "intensity".to_owned();
            source.location = Some(grid(location));
            source.unit = "g/kWh".to_owned();
        }
    }
    source
}

fn skipped(skip: Skip) -> &'static str {
    match skip {
        Skip::Cooldown => "cooldown",
        Skip::Busy => "busy",
    }
}

fn failed(failure: &Failure) -> record::Failed {
    let (name, error, detail) = match failure {
        Failure::Provider(error) => {
            let (kind, detail) = match error {
                ProviderError::Unavailable(detail) => ("unavailable", detail),
                ProviderError::Denied(detail) => ("denied", detail),
                ProviderError::RateLimited(detail) => ("rate_limited", detail),
                ProviderError::Invalid(detail) => ("invalid", detail),
            };
            ("provider", Some(kind.to_owned()), Some(detail.clone()))
        }
        Failure::Unanswered => ("unanswered", None, None),
        Failure::TimedOut => ("timed_out", None, None),
        Failure::Stalled => ("stalled", None, None),
        Failure::Panicked(message) => ("panicked", None, Some(message.clone())),
        Failure::Retired => ("retired", None, None),
    };
    record::Failed { failure: name.to_owned(), error, detail }
}

/// Converts a grid location to its record form.
pub fn grid(location: &GridLocation) -> record::Grid {
    match location {
        GridLocation::Zone { zone_id } => record::Grid::Zone(zone_id.clone()),
        GridLocation::Country { country_code } => {
            record::Grid::Country(country_code.to_uppercase())
        }
        GridLocation::Coordinates {
            latitude,
            longitude,
        } => record::Grid::Coordinates([*latitude, *longitude]),
        GridLocation::DataCenter {
            cloud_provider,
            region,
        } => record::Grid::DataCenter {
            provider: cloud_provider.clone(),
            region: region.clone(),
        },
        GridLocation::Global => record::Grid::Global,
    }
}

/// The record name of a power domain.
pub fn domain(domain: PowerDomain) -> String {
    presentation::domain(domain).to_owned()
}

fn entries(work: &Work) -> Vec<record::WorkEntry> {
    work.entries()
        .iter()
        .map(|entry| record::WorkEntry { kind: entry.kind.clone(), amount: entry.amount })
        .collect()
}

fn billed(selection: &SourceSelection) -> record::Billed {
    match selection {
        SourceSelection::All => record::Billed::All,
        SourceSelection::Domains(domains) => {
            record::Billed::Domains(domains.iter().copied().map(domain).collect())
        }
        SourceSelection::Sources(ids) => {
            record::Billed::Sources(ids.iter().map(ToString::to_string).collect())
        }
    }
}

fn per_source(usages: &HashMap<SourceId, Usage>) -> BTreeMap<String, record::Usage> {
    usages
        .iter()
        .map(|(id, used)| (id.to_string(), usage(*used)))
        .collect()
}

fn usage(usage: Usage) -> record::Usage {
    let (energy_kwh, emissions_g) = kwh_and_grams(usage);
    record::Usage {
        energy_kwh,
        emissions_g,
    }
}

fn kwh_and_grams(usage: Usage) -> (f64, Option<f64>) {
    (usage.energy.to_kilowatt_hours().get(), usage.emissions.map(|grams| grams.get()))
}

fn samples(samples: &HashMap<SourceId, usize>) -> BTreeMap<String, u64> {
    samples.iter().map(|(id, &n)| (id.to_string(), n as u64)).collect()
}

/// What probing found, for the record's `probe` line.
pub fn probe(resolution: &Resolution) -> record::Probe {
    let candidates = resolution
        .candidates()
        .iter()
        .map(|candidate| {
            let (verdict, series) = match &candidate.verdict {
                Verdict::Serving(series) => ("serving", series.clone()),
                Verdict::Declined => ("declined", Vec::new()),
                Verdict::Covered => ("covered", Vec::new()),
                Verdict::NotWanted => ("not_wanted", Vec::new()),
            };
            record::Candidate {
                name: candidate.name.to_owned(),
                verdict: verdict.to_owned(),
                series,
            }
        })
        .collect();
    record::Probe { candidates }
}

/// Where probing knew the host to be, for the record's `run` line.
pub fn location(location: Option<&Location>) -> Option<record::Location> {
    Some(match location? {
        Location::Stated(location) => record::Location::Stated(grid(location)),
        Location::Estimated(estimate) => record::Location::Estimated {
            country: estimate.country.as_str().to_owned(),
            coordinates: estimate
                .coordinates
                .map(|at| [at.latitude(), at.longitude()]),
            by: estimate.by.to_owned(),
        },
    })
}

/// The power domains measured, for the record's `run` line; `None` for all.
pub fn components(measure: &Measure) -> Option<Vec<String>> {
    match measure {
        Measure::All => None,
        Measure::Only(domains) => Some(domains.iter().copied().map(domain).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failed_as(failure: Failure) -> (String, Option<String>, Option<String>) {
        let record::Failed { failure, error, detail } = failed(&failure);
        (failure, error, detail)
    }

    fn named(failure: &str, error: Option<&str>, detail: Option<&str>) -> (String, Option<String>, Option<String>) {
        (failure.to_owned(), error.map(str::to_owned), detail.map(str::to_owned))
    }

    /// AC/REC-17
    #[test]
    fn every_observation_is_recorded_by_its_name() {
        assert_eq!((skipped(Skip::Cooldown), skipped(Skip::Busy)), ("cooldown", "busy"));
        for (error, name, detail) in [
            (ProviderError::Unavailable("down".into()), "unavailable", "down"),
            (ProviderError::Denied("bad key".into()), "denied", "bad key"),
            (ProviderError::RateLimited("429".into()), "rate_limited", "429"),
            (ProviderError::Invalid("NaN".into()), "invalid", "NaN"),
        ] {
            assert_eq!(failed_as(Failure::Provider(error)), named("provider", Some(name), Some(detail)));
        }
        for (failure, name) in [
            (Failure::Unanswered, "unanswered"),
            (Failure::TimedOut, "timed_out"),
            (Failure::Stalled, "stalled"),
            (Failure::Retired, "retired"),
        ] {
            assert_eq!(failed_as(failure), named(name, None, None));
        }
        assert_eq!(failed_as(Failure::Panicked("boom".into())), named("panicked", None, Some("boom")));
    }
}
