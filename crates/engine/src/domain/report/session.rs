//! What the whole session measured, and how much of it the spans claimed.

use std::collections::HashMap;
use std::time::Duration;

use super::{ReportWarning, sum};
use crate::domain::accounting::Usage;
use crate::domain::session::Session;
use crate::domain::source::{SourceDescription, independent_sources};
use crate::domain::{SourceId, SpanId, Timestamp};

/// Covers only the sources the session measured, never the whole machine.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SessionReport {
    /// Measured usage per source, claimed or not.
    pub per_source: HashMap<SourceId, Usage>,
    pub sources: HashMap<SourceId, SourceDescription>,
    /// Everything measured, over the sources nothing else subsumes.
    pub total: Usage,
    /// The part of `total` billed to spans.
    pub claimed: Usage,
    /// The part of `total` no span claimed: time with no span running, and
    /// sources no running span selected.
    pub unclaimed: Usage,
    /// The intensity sources the power sources are billed against.
    pub intensities: HashMap<SourceId, SourceDescription>,
    pub duration: Duration,
    /// Per power source, the samples it has had.
    pub samples: HashMap<SourceId, usize>,
    pub warnings: Vec<ReportWarning>,
}

impl Session {
    /// The whole session as of `at`, over the segments accounted so far.
    pub fn session_report(&self, at: Timestamp) -> SessionReport {
        // The root is billed whatever no span claimed.
        let root = self.report_for(SpanId::ROOT, self.timeline().root(), at);

        // Everything billed under the root, forgotten spans included.
        let claimed_per_source = self.descendants(SpanId::ROOT);
        let claimed = sum(&claimed_per_source, &independent_sources(&root.sources));

        SessionReport {
            per_source: root.per_source,
            sources: root.sources,
            total: root.total,
            claimed,
            unclaimed: root.exclusive,
            intensities: root.intensities,
            duration: root.duration,
            samples: root.samples,
            warnings: root.warnings,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Fidelity, PowerDomain, PowerScope, PowerUsageEffectiveness, ProviderId, ReadoutId, Sample,
        SampleKind, SampleStore, SourceCatalog, SourceSelection,
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
            fidelity: Fidelity::Stated,
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

    /// AC/ENG-21, AC/PROF-52
    #[test]
    fn what_was_measured_splits_into_claimed_and_unclaimed() {
        let catalog = catalog();
        let mut sample_store = sample_store(&catalog);
        for (source, watts) in [(cpu(), 4.0), (gpu(), 10.0)] {
            sample_store
                .store(
                    source,
                    [0, 5, 10].map(|s| Sample::new(t(s), watts)).to_vec(),
                )
                .unwrap();
        }
        let mut session = Session::new(catalog, PowerUsageEffectiveness::default(), t(0));

        let cpu_only = SourceSelection::Domains(vec![PowerDomain::Cpu]);
        let span = session
            .start_next("work".into(), None, cpu_only, t(5), ReadoutId(1))
            .unwrap();
        session.stop(span, t(10), ReadoutId(2)).unwrap();
        session.finish(t(10), ReadoutId(3));
        for readout in 0..=3 {
            session.land(ReadoutId(readout), &sample_store);
        }

        let report = session.session_report(t(10));
        // 0–5 s: no span running, so 20 J of CPU and 50 J of GPU are unclaimed.
        // 5–10 s: the span claims its 20 J of CPU; the GPU's 50 J is unclaimed.
        assert_eq!(report.total.energy.get(), 140.0);
        assert_eq!(report.claimed.energy.get(), 20.0);
        assert_eq!(report.unclaimed.energy.get(), 120.0);
        assert_eq!(report.duration, Duration::from_secs(10));
        assert_eq!(report.sources.len(), 2, "the report names what it covers");
    }
}
