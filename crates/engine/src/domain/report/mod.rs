//! What a [Session](super::Session) reports back to the user: one submodule
//! per report, each holding its `impl Session` and the logic only it needs.

mod session;
mod span;

use std::collections::HashMap;
use std::fmt;

use crate::domain::accounting::Usage;
use crate::domain::source::SourceId;

pub use session::SessionReport;
pub use span::SpanReport;

/// Something a report must say out loud rather than quietly print a number.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ReportWarning {
    /// Fewer than two samples of `sources` landed inside the span, so their
    /// energy is a held level or interpolated, not an integrated curve.
    /// `samples` is the fewest any of them had.
    ThinCoverage { samples: usize, sources: Vec<String> },
    /// `sources` drew energy with no intensity to bill it against, so the
    /// emissions are absent.
    NoIntensity { sources: Vec<String> },
    /// The intensity `sources` reported nothing inside the span, so their
    /// emissions rest on a value read before it began.
    HeldIntensity { sources: Vec<String> },
    /// Part of the total is an estimate, not a counter on this machine.
    Estimated { sources: Vec<String> },
    /// The span selected sources nothing on this host resolved, so its totals
    /// are zero rather than unmeasured.
    NoSelectedSources,
}

impl fmt::Display for ReportWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ThinCoverage { samples: 0, sources } => write!(
                formatter,
                "no samples of {} landed inside this span; it was probably \
                 shorter than the sampling interval, so lower the interval or \
                 lengthen the span",
                sources.join(", ")
            ),
            Self::ThinCoverage { samples, sources } => write!(
                formatter,
                "only {samples} sample of {} landed inside this span, so its \
                 energy is a held level rather than an integrated curve",
                sources.join(", ")
            ),
            Self::NoIntensity { sources } => write!(
                formatter,
                "no grid intensity covered {} in this span, so its emissions \
                 are unknown",
                sources.join(", ")
            ),
            Self::HeldIntensity { sources } => write!(
                formatter,
                "{} landed nothing inside this span, so its emissions rest on \
                 a value read before the span began",
                sources.join(", ")
            ),
            Self::Estimated { sources } => write!(
                formatter,
                "part of this total is an estimate, not measured on this machine: {}",
                sources.join(", ")
            ),
            Self::NoSelectedSources => write!(
                formatter,
                "this span selected sources that nothing on this host \
                 resolved, so its totals are zero rather than unmeasured"
            ),
        }
    }
}

/// The usage of the `counted` sources in `per_source`, added up.
fn sum(per_source: &HashMap<SourceId, Usage>, counted: &[SourceId]) -> Usage {
    counted
        .iter()
        .filter_map(|id| per_source.get(id))
        .fold(Usage::default(), |mut acc, &usage| {
            acc += usage;
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{GramsCo2e, Joules, ProviderId};

    fn usage(joules: f64, grams: Option<f64>) -> Usage {
        Usage { energy: Joules::new(joules).unwrap(), emissions: grams.map(|g| GramsCo2e::new(g).unwrap()) }
    }

    /// AC/PROF-45
    #[test]
    fn a_total_over_sources_ignores_absent_emissions_of_a_source_with_no_energy() {
        let (cpu, ram) = (SourceId::new(ProviderId(0), 0), SourceId::new(ProviderId(0), 1));
        let idle = HashMap::from([(cpu, usage(5.0, Some(1.0))), (ram, usage(0.0, None))]);
        assert_eq!(sum(&idle, &[cpu, ram]).emissions, usage(0.0, Some(1.0)).emissions);
        let drawing = HashMap::from([(cpu, usage(5.0, Some(1.0))), (ram, usage(1.0, None))]);
        assert_eq!(sum(&drawing, &[cpu, ram]).emissions, None);
    }
}
