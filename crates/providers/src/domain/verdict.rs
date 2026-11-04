//! How each candidate fared, and the slots the chain fills. Probing's own
//! vocabulary: the engine only ever sees the winners' sources.

use std::fmt;

use carbontracker::{GridLocation, PowerDomain, PowerScope, SourceDescription};

/// What became of one probe candidate.
#[derive(Debug, Clone)]
pub struct CandidateOutcome {
    pub name: &'static str,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Chosen, and serving these series.
    Serving(Vec<String>),
    /// Probed, and had nothing to offer: no counter, no credential, no driver.
    Declined,
    /// Not probed; every slot it could fill was already taken.
    Covered,
    /// Not probed. Nothing it offers was asked for.
    NotWanted,
}

impl fmt::Display for Verdict {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serving(series) => write!(formatter, "serving {}", series.join(", ")),
            Self::Declined => formatter.write_str("not available on this host"),
            Self::Covered => formatter.write_str("covered by a better candidate"),
            Self::NotWanted => formatter.write_str("not asked for"),
        }
    }
}

/// One question a source answers; the chain keeps one source per coverage.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Coverage {
    Power { device_name: String, domain: PowerDomain, scope: PowerScope },
    Intensity { location: GridLocation },
}

impl Coverage {
    /// What `description` covers, independent of how it is obtained. Excludes
    /// the sample kind and `fidelity`; includes `device_name` so two GPUs are two
    /// slots.
    pub(crate) fn of(description: &SourceDescription) -> Self {
        match description {
            SourceDescription::Power { device_name, domain, scope, .. } => {
                Self::Power { device_name: device_name.clone(), domain: *domain, scope: scope.clone() }
            }
            SourceDescription::Intensity { location, .. } => Self::Intensity { location: location.clone() },
        }
    }
}
