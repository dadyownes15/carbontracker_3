//! What a span and its descendants used.

use std::collections::HashMap;
use std::time::Duration;

use super::{ReportWarning, sum};
use crate::domain::accounting::Usage;
use crate::domain::session::Session;
use crate::domain::source::{self, SourceDescription, SourceSelection, independent_sources};
use crate::domain::timeline::Span;
use crate::domain::{Fidelity, SourceId, SpanId, Timestamp, TrackerError};

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SpanReport {
    pub span_id: SpanId,
    /// A label; it may repeat across spans.
    pub span_name: String,
    /// Per source, descendants included.
    pub per_source: HashMap<SourceId, Usage>,
    pub sources: HashMap<SourceId, SourceDescription>,
    /// Over the sources nothing else subsumes, descendants included. Not the
    /// sum of `per_source`: on an Apple SoC the `System` rail already contains
    /// CPU, GPU, ANE and RAM.
    pub total: Usage,
    /// What the span used itself, descendants excluded.
    pub exclusive: Usage,
    /// The intensity sources the selected power sources are billed against.
    pub intensities: HashMap<SourceId, SourceDescription>,
    pub duration: Duration,
    /// Per selected power source, the samples it had inside the span.
    pub samples: HashMap<SourceId, usize>,
    pub warnings: Vec<ReportWarning>,
    /// The sources the span asked to be billed against.
    pub selection: SourceSelection,
}

impl Session {
    /// What `span` and its descendants were billed, as of `at`.
    ///
    /// # Errors
    ///
    /// [TrackerError::UnknownSpan] if no such span was started.
    pub fn span_report(&self, span: SpanId, at: Timestamp) -> Result<SpanReport, TrackerError> {
        let record = self
            .timeline()
            .span(span)
            .ok_or(TrackerError::UnknownSpan(span))?;
        Ok(self.report_for(span, record, at))
    }

    /// [Session::span_report] without the lookup, so the root can be reported too.
    pub(super) fn report_for(&self, span: SpanId, record: &Span, at: Timestamp) -> SpanReport {
        let catalog = self.catalog();

        let exclusive_per_source = self.ledger(span);
        let mut per_source = self.subtree(span);

        // Selected at query time, so spans over the same interval can bill
        // different sources.
        let selection = record.selection.clone();
        let sources = catalog.selected(per_source.keys().copied(), &selection);
        per_source.retain(|id, _| sources.contains_key(id));

        let counted = independent_sources(&sources);
        let total = sum(&per_source, &counted);
        let exclusive = sum(&exclusive_per_source, &counted);

        let selected = catalog.selected(catalog.descriptions.keys().copied(), &selection);
        let intensities = catalog.intensities_of(selected.into_keys());
        let counts = self.samples(span);
        let count = |id: &SourceId| counts.get(id).copied().unwrap_or(0);
        let samples: HashMap<SourceId, usize> = sources.keys().map(|id| (*id, count(id))).collect();
        let intensity_samples: HashMap<SourceId, usize> = intensities.keys().map(|id| (*id, count(id))).collect();

        let estimated = source::labels(
            counted
                .iter()
                .filter_map(|id| sources.get_key_value(id))
                .chain(intensities.iter())
                .filter(|(_, description)| description.fidelity().needs_caveat()),
        );
        let coverage = Coverage {
            sources: &sources,
            per_source: &per_source,
            samples: &samples,
            intensities: &intensities,
            intensity_samples: &intensity_samples,
        };
        let warnings = span_warnings(&selection, &coverage, estimated);

        SpanReport {
            span_id: span,
            span_name: record.name.clone(),
            per_source,
            sources,
            total,
            exclusive,
            intensities,
            duration: record.open_for(at),
            samples,
            warnings,
            selection,
        }
    }
}

/// What a span's warnings are read from.
struct Coverage<'a> {
    sources: &'a HashMap<SourceId, SourceDescription>,
    per_source: &'a HashMap<SourceId, Usage>,
    samples: &'a HashMap<SourceId, usize>,
    intensities: &'a HashMap<SourceId, SourceDescription>,
    intensity_samples: &'a HashMap<SourceId, usize>,
}

/// The labels of the sources in `descriptions` that `keep` keeps.
fn labels(descriptions: &HashMap<SourceId, SourceDescription>, keep: impl Fn(&SourceId) -> bool) -> Vec<String> {
    source::labels(descriptions.iter().filter(|(id, _)| keep(id)))
}

fn span_warnings(selection: &SourceSelection, coverage: &Coverage, estimated: Vec<String>) -> Vec<ReportWarning> {
    let mut warnings = Vec::new();
    if coverage.sources.is_empty() && !matches!(selection, SourceSelection::All) {
        warnings.push(ReportWarning::NoSelectedSources);
    }
    let count = |id: &SourceId| coverage.samples.get(id).copied().unwrap_or(0);
    // A fixed figure holds one level by nature: only a measured source can be thin.
    let measured = |id: &SourceId| coverage.sources[id].fidelity() == Fidelity::Measured;
    let thin = labels(coverage.sources, |id| measured(id) && count(id) < 2);
    if !thin.is_empty() {
        let samples = coverage.sources.keys().filter(|id| measured(id)).map(count).min().unwrap_or(0);
        warnings.push(ReportWarning::ThinCoverage { samples, sources: thin });
    }
    let absent = labels(coverage.sources, |id| {
        coverage.per_source.get(id).is_some_and(|usage| usage.energy.get() > 0.0 && usage.emissions.is_none())
    });
    if !absent.is_empty() {
        warnings.push(ReportWarning::NoIntensity { sources: absent });
    }
    let held = labels(coverage.intensities, |id| coverage.intensity_samples.get(id).copied().unwrap_or(0) == 0);
    if !held.is_empty() {
        warnings.push(ReportWarning::HeldIntensity { sources: held });
    }
    if !estimated.is_empty() {
        warnings.push(ReportWarning::Estimated { sources: estimated });
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Fidelity, GramsCo2e, Joules, PowerDomain, PowerScope, PowerUsageEffectiveness, ProviderId,
        ReadoutId, Sample, SampleKind, SampleStore, SourceCatalog,
    };

    /// `secs` seconds after the epoch.
    fn t(secs: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(secs)
    }

    fn cpu() -> SourceId {
        SourceId::new(ProviderId(1), 0)
    }

    fn gpu() -> SourceId {
        SourceId::new(ProviderId(1), 1)
    }

    /// [cpu] and [gpu], read linearly, with no intensity series.
    fn catalog() -> SourceCatalog {
        let power = |domain| SourceDescription::Power {
            device_name: format!("{domain:?}"),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Measured,
        };
        SourceCatalog {
            descriptions: HashMap::from([
                (cpu(), power(PowerDomain::Cpu)),
                (gpu(), power(PowerDomain::Gpu)),
            ]),
            ..SourceCatalog::default()
        }
    }

    /// Empty, with one appending series per source in `catalog`.
    fn sample_store(catalog: &SourceCatalog) -> SampleStore {
        SampleStore::new_from_source(&catalog.descriptions, 64)
            .expect("the keys of a map are distinct")
    }

    /// AC/PROF-21, AC/PROF-36, AC/PROF-37, AC/PROF-39, AC/PROF-40
    #[test]
    fn a_parent_report_includes_its_child_but_its_exclusive_figure_does_not() {
        let catalog = catalog();
        let mut sample_store = sample_store(&catalog);
        for (source, watts) in [(cpu(), 4.0), (gpu(), 10.0)] {
            sample_store
                .store(source, (0..=20).map(|s| Sample::new(t(s), watts)).collect())
                .unwrap();
        }
        let mut session = Session::new(catalog, PowerUsageEffectiveness::default(), t(0));

        let parent = session
            .start_next("p".into(), None, SourceSelection::All, t(0), ReadoutId(1))
            .unwrap();
        let cpu_only = SourceSelection::Domains(vec![PowerDomain::Cpu]);
        let child = session
            .start_next("c".into(), Some(parent), cpu_only, t(5), ReadoutId(2))
            .unwrap();
        session.stop(child, t(15), ReadoutId(3)).unwrap();
        session.stop(parent, t(20), ReadoutId(4)).unwrap();
        for readout in 1..=4 {
            session.land(ReadoutId(readout), &sample_store);
        }

        let report = session.span_report(parent, t(20)).unwrap();
        // 20 s of both, 14 W: the child took 10 s of CPU (40 J).
        assert_eq!(report.total.energy.get(), 280.0);
        assert_eq!(report.per_source[&cpu()].energy.get(), 80.0, "4 W over 20 s");
        assert_eq!(report.per_source[&gpu()].energy.get(), 200.0, "10 W over 20 s");
        assert_eq!(report.exclusive.energy.get(), 240.0);
        assert_eq!(report.duration, Duration::from_secs(20));

        let child_report = session.span_report(child, t(20)).unwrap();
        assert_eq!(child_report.total.energy.get(), 40.0);
        assert!(
            !child_report.per_source.contains_key(&gpu()),
            "the child selected the CPU only"
        );
    }

    /// AC/ENG-23
    #[test]
    fn an_unknown_span_is_an_error() {
        let catalog = catalog();
        let session = Session::new(catalog, PowerUsageEffectiveness::default(), t(0));
        assert!(matches!(
            session.span_report(SpanId::new(9), t(0)),
            Err(TrackerError::UnknownSpan(_))
        ));
    }

    fn label(source: SourceId) -> String {
        catalog_with_grids().descriptions[&source].to_string()
    }

    fn dk1() -> SourceId {
        SourceId::new(ProviderId(2), 0)
    }

    fn se3() -> SourceId {
        SourceId::new(ProviderId(3), 0)
    }

    /// [catalog] with the CPU on `dk1` and the GPU on `se3`.
    fn catalog_with_grids() -> SourceCatalog {
        let mut catalog = catalog();
        for id in [dk1(), se3()] {
            catalog.descriptions.insert(
                id,
                SourceDescription::Intensity { location: crate::domain::GridLocation::Global, fidelity: Fidelity::Stated },
            );
        }
        catalog.pairs = HashMap::from([(cpu(), dk1()), (gpu(), se3())]);
        catalog
    }

    fn used(joules: f64, grams: Option<f64>) -> Usage {
        Usage { energy: Joules::new(joules).unwrap(), emissions: grams.map(|g| GramsCo2e::new(g).unwrap()) }
    }

    /// The warnings for a span over CPU and GPU with these counts and usage.
    fn warn(
        samples: &[(SourceId, usize)],
        per_source: &[(SourceId, Usage)],
        intensity_samples: &[(SourceId, usize)],
    ) -> Vec<ReportWarning> {
        let catalog = catalog_with_grids();
        let sources: HashMap<_, _> = [cpu(), gpu()].map(|id| (id, catalog.descriptions[&id].clone())).into();
        let intensities = catalog.intensities_of([cpu(), gpu()]);
        let coverage = Coverage {
            sources: &sources,
            per_source: &per_source.iter().copied().collect(),
            samples: &samples.iter().copied().collect(),
            intensities: &intensities,
            intensity_samples: &intensity_samples.iter().copied().collect(),
        };
        span_warnings(&SourceSelection::All, &coverage, Vec::new())
    }

    /// AC/PROF-43, AC/PROF-47
    #[test]
    fn thin_coverage_names_each_source_with_fewer_than_two_samples() {
        let warnings = warn(&[(cpu(), 4), (gpu(), 1)], &[], &[(dk1(), 3), (se3(), 3)]);
        assert_eq!(warnings, vec![ReportWarning::ThinCoverage { samples: 1, sources: vec![label(gpu())] }]);
    }

    /// AC/PROF-47
    #[test]
    fn a_fixed_figure_is_never_thin() {
        let catalog = catalog_with_grids();
        let mut sources: HashMap<_, _> = [cpu(), gpu()].map(|id| (id, catalog.descriptions[&id].clone())).into();
        for fidelity in [Fidelity::Stated, Fidelity::Modelled, Fidelity::Assumed] {
            if let Some(SourceDescription::Power { fidelity: held, .. }) = sources.get_mut(&gpu()) {
                *held = fidelity;
            }
            let coverage = Coverage {
                sources: &sources,
                per_source: &HashMap::new(),
                samples: &HashMap::from([(cpu(), 4), (gpu(), 1)]),
                intensities: &catalog.intensities_of([cpu(), gpu()]),
                intensity_samples: &HashMap::from([(dk1(), 3), (se3(), 3)]),
            };
            assert_eq!(span_warnings(&SourceSelection::All, &coverage, Vec::new()), vec![], "{fidelity:?}");
        }
    }

    /// AC/PROF-43, AC/PROF-48
    #[test]
    fn no_intensity_names_each_source_with_absent_emissions() {
        let per_source = [(cpu(), used(5.0, Some(1.0))), (gpu(), used(5.0, None))];
        let warnings = warn(&[(cpu(), 4), (gpu(), 4)], &per_source, &[(dk1(), 3), (se3(), 3)]);
        assert_eq!(warnings, vec![ReportWarning::NoIntensity { sources: vec![label(gpu())] }]);
    }

    /// AC/PROF-48
    #[test]
    fn a_source_with_no_energy_and_no_intensity_does_not_warn() {
        let per_source = [(cpu(), used(5.0, Some(1.0))), (gpu(), used(0.0, None))];
        assert_eq!(warn(&[(cpu(), 4), (gpu(), 4)], &per_source, &[(dk1(), 3), (se3(), 3)]), vec![]);
    }

    /// AC/PROF-49
    #[test]
    fn held_intensity_names_each_intensity_source_with_no_sample_in_the_span() {
        let per_source = [(cpu(), used(5.0, Some(1.0))), (gpu(), used(5.0, Some(1.0)))];
        let warnings = warn(&[(cpu(), 4), (gpu(), 4)], &per_source, &[(se3(), 1)]);
        assert_eq!(warnings, vec![ReportWarning::HeldIntensity { sources: vec![label(dk1())] }]);
    }

    /// AC/PROF-5, AC/PROF-51
    #[test]
    fn a_span_that_selects_nothing_here_reports_zero_and_warns() {
        let gpu_only = SourceSelection::Domains(vec![PowerDomain::Ane]);
        let empty = HashMap::new();
        let coverage = Coverage {
            sources: &HashMap::new(),
            per_source: &HashMap::new(),
            samples: &empty,
            intensities: &HashMap::new(),
            intensity_samples: &empty,
        };
        assert_eq!(span_warnings(&gpu_only, &coverage, Vec::new()), vec![ReportWarning::NoSelectedSources]);
    }

    /// AC/PROF-41
    #[test]
    fn a_span_counts_samples_per_selected_source() {
        let catalog = catalog();
        let mut sample_store = sample_store(&catalog);
        sample_store.store(cpu(), (0..4).map(|s| Sample::new(t(s), 1.0)).collect()).unwrap();
        sample_store.store(gpu(), vec![Sample::new(t(1), 1.0)]).unwrap();
        let mut session = Session::new(catalog, PowerUsageEffectiveness::default(), t(0));
        let span = session.start_next("s".into(), None, SourceSelection::All, t(0), ReadoutId(1)).unwrap();
        session.stop(span, t(4), ReadoutId(2)).unwrap();
        session.land(ReadoutId(1), &sample_store);
        session.land(ReadoutId(2), &sample_store);
        let report = session.span_report(span, t(4)).unwrap();
        assert_eq!(report.samples, HashMap::from([(cpu(), 4), (gpu(), 1)]));
    }

    /// AC/PROF-42
    #[test]
    fn a_span_reports_the_intensities_of_the_sources_it_selected() {
        let dk1 = SourceId::new(ProviderId(2), 0);
        let se3 = SourceId::new(ProviderId(3), 0);
        let mut catalog = catalog();
        for id in [dk1, se3] {
            catalog.descriptions.insert(
                id,
                SourceDescription::Intensity { location: crate::domain::GridLocation::Global, fidelity: Fidelity::Stated },
            );
        }
        catalog.pairs = HashMap::from([(cpu(), dk1), (gpu(), se3)]);
        let mut session = Session::new(catalog, PowerUsageEffectiveness::default(), t(0));
        let cpu_only = SourceSelection::Domains(vec![PowerDomain::Cpu]);
        let span = session.start_next("c".into(), None, cpu_only, t(0), ReadoutId(1)).unwrap();

        let report = session.span_report(span, t(5)).unwrap();
        assert_eq!(report.intensities.into_keys().collect::<Vec<_>>(), vec![dk1]);
    }

    /// AC/PROF-42
    #[test]
    fn a_power_only_span_reports_no_intensity() {
        let mut session = Session::new(catalog(), PowerUsageEffectiveness::default(), t(0));
        let span = session.start_next("p".into(), None, SourceSelection::All, t(0), ReadoutId(1)).unwrap();
        assert!(session.span_report(span, t(5)).unwrap().intensities.is_empty());
    }

    /// Span "s" over every source from 0 s to 10 s in a session over
    /// `catalog`, each power source drawing 10 W and each grid at 100 g/kWh,
    /// stopped if `stop`; its report as of 30 s.
    fn ten_seconds_over(catalog: SourceCatalog, stop: bool) -> SpanReport {
        let mut sample_store = sample_store(&catalog);
        for (&id, description) in &catalog.descriptions {
            let value = if matches!(description, SourceDescription::Intensity { .. }) { 100.0 } else { 10.0 };
            sample_store.store(id, vec![Sample::new(t(0), value), Sample::new(t(10), value)]).unwrap();
        }
        let mut session = Session::new(catalog, PowerUsageEffectiveness::default(), t(0));
        let span = session.start_next("s".into(), None, SourceSelection::All, t(0), ReadoutId(1)).unwrap();
        if stop {
            session.stop(span, t(10), ReadoutId(2)).unwrap();
        } else {
            session.mark(span, t(10), ReadoutId(2)).unwrap();
        }
        session.land(ReadoutId(1), &sample_store);
        session.land(ReadoutId(2), &sample_store);
        session.span_report(span, t(30)).unwrap()
    }

    fn power(domain: PowerDomain, fidelity: Fidelity) -> SourceDescription {
        SourceDescription::Power {
            device_name: "soc".to_owned(),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity,
        }
    }

    /// AC/PROF-38, AC/PROF-46
    #[test]
    fn the_total_leaves_out_a_source_another_selected_source_subsumes() {
        let catalog = SourceCatalog {
            descriptions: HashMap::from([
                (cpu(), power(PowerDomain::Cpu, Fidelity::Stated)),
                (gpu(), power(PowerDomain::System, Fidelity::Stated)),
            ]),
            ..SourceCatalog::default()
        };
        let report = ten_seconds_over(catalog, true);
        assert_eq!(report.per_source[&cpu()].energy.get(), 100.0);
        assert_eq!(report.per_source[&gpu()].energy.get(), 100.0);
        assert_eq!(report.total.energy.get(), 100.0, "the System rail already holds the CPU");
    }

    /// AC/PROF-40
    #[test]
    fn an_open_span_s_duration_runs_to_the_query_and_a_stopped_one_s_to_its_stop() {
        assert_eq!(ten_seconds_over(catalog(), false).duration, Duration::from_secs(30));
        assert_eq!(ten_seconds_over(catalog(), true).duration, Duration::from_secs(10));
    }

    /// AC/PROF-50
    #[test]
    fn a_counted_source_or_its_intensity_that_is_estimated_warns() {
        let estimated = |report: &SpanReport| {
            report.warnings.iter().find_map(|warning| match warning {
                ReportWarning::Estimated { sources } => Some(sources.clone()),
                _ => None,
            })
        };
        let with = |cpu_fidelity, grid_fidelity| SourceCatalog {
            descriptions: HashMap::from([
                (cpu(), power(PowerDomain::Cpu, cpu_fidelity)),
                (dk1(), SourceDescription::Intensity { location: crate::domain::GridLocation::Global, fidelity: grid_fidelity }),
            ]),
            pairs: HashMap::from([(cpu(), dk1())]),
        };
        let label = |catalog: &SourceCatalog, id| catalog.descriptions[&id].to_string();

        let modelled = with(Fidelity::Modelled, Fidelity::Measured);
        assert_eq!(estimated(&ten_seconds_over(modelled.clone(), true)), Some(vec![label(&modelled, cpu())]));
        let assumed = with(Fidelity::Measured, Fidelity::Assumed);
        assert_eq!(estimated(&ten_seconds_over(assumed.clone(), true)), Some(vec![label(&assumed, dk1())]));
        assert_eq!(estimated(&ten_seconds_over(with(Fidelity::Stated, Fidelity::Measured), true)), None);
    }
}
