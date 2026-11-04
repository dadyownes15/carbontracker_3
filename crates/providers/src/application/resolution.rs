//! [Resolution]: what probing found, and [ProbeProgress] while it runs.

use std::collections::HashMap;
use std::fmt;

use carbontracker::{Cooldown, Fidelity, GridLocation, Provider, ProviderId, SourceDescription, SourceId, Sources};

use crate::adapters::intensity::constant::WORLD_AVERAGE_G_CO2_PER_KWH;
use crate::application::forecaster::Forecaster;
use crate::application::registry::flat;
use crate::domain::location::Location;
use crate::domain::verdict::{CandidateOutcome, Verdict};

/// What probing found: every candidate the registry tried, in chain order,
/// the sources the winners serve, and the forecast. [Resolution::resolve]
/// turns it into [Sources] and a [Forecaster]; its `Display` is a summary for
/// a frontend to show.
pub struct Resolution {
    pub(crate) providers: HashMap<ProviderId, Box<dyn Provider>>,
    pub(crate) descriptions: HashMap<SourceId, SourceDescription>,
    /// Outcome of every candidate, in chain order.
    pub(crate) candidates: Vec<CandidateOutcome>,
    /// The candidate behind each provider kept.
    pub(crate) served_by: HashMap<ProviderId, &'static str>,
    pub(crate) location: Option<Location>,
    /// The forecast for the billed grid; set once the forecast chain ran.
    pub(crate) forecaster: Option<Forecaster>,
}

impl Resolution {
    pub(crate) fn empty() -> Self {
        Self {
            providers: HashMap::new(),
            descriptions: HashMap::new(),
            candidates: Vec::new(),
            served_by: HashMap::new(),
            location: None,
            forecaster: None,
        }
    }

    /// The candidate serving `source`, e.g. `"nvml"`; `None` for a source
    /// this resolution doesn't have.
    pub fn served_by(&self, source: SourceId) -> Option<&'static str> {
        self.descriptions.contains_key(&source).then(|| self.served_by.get(&source.provider()).copied()).flatten()
    }

    /// The cooldown the provider serving `source` declares; `None` for a
    /// source this resolution doesn't have.
    pub fn cooldown(&self, source: SourceId) -> Option<Cooldown> {
        if !self.descriptions.contains_key(&source) {
            return None;
        }
        self.providers.get(&source.provider()).map(|provider| provider.cooldown())
    }

    /// Where probing knew the host to be: stated, estimated, or `None` when
    /// neither (the global average serves). The grid each source actually
    /// measures is in its description.
    pub fn location(&self) -> Option<&Location> {
        self.location.as_ref()
    }

    /// The forecast for the billed grid; `None` until the forecast chain ran.
    pub fn forecaster(&self) -> Option<&Forecaster> {
        self.forecaster.as_ref()
    }

    /// Every candidate in chain order, with its verdict.
    pub fn candidates(&self) -> &[CandidateOutcome] {
        &self.candidates
    }

    /// Every source that will be measured, and what it measures.
    pub fn sources(&self) -> impl Iterator<Item = (SourceId, &SourceDescription)> {
        self.descriptions.iter().map(|(&id, description)| (id, description))
    }

    /// The sources to start a tracker over, and the forecast for the grid it
    /// bills. Only the winners' sources go to the tracker; show the verdicts
    /// before resolving.
    pub fn resolve(self) -> (Sources, Forecaster) {
        let mut by_provider: HashMap<ProviderId, Vec<(SourceId, SourceDescription)>> = HashMap::new();
        for (source, description) in self.descriptions {
            by_provider.entry(source.provider()).or_default().push((source, description));
        }

        let mut sources = Sources::new();
        for (provider, service) in self.providers {
            let served = by_provider.remove(&provider).unwrap_or_default();
            // The chain keeps one source per coverage slot, so one intensity
            // at most: `Sources` can't refuse what it built.
            sources.add_provider(provider, service, served).expect("the chain keeps one source per slot");
        }
        // The forecast chain always ends in a projection.
        let forecaster = self.forecaster.unwrap_or_else(|| {
            let day = std::time::Duration::from_secs(24 * 60 * 60);
            flat(WORLD_AVERAGE_G_CO2_PER_KWH, GridLocation::Global, day, Fidelity::Assumed, "projection")
        });
        (sources, forecaster)
    }
}

impl fmt::Display for Resolution {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, outcome) in self.candidates.iter().enumerate() {
            if index > 0 {
                writeln!(formatter)?;
            }
            write!(formatter, "{}: {}", outcome.name, outcome.verdict)?;
        }
        Ok(())
    }
}

/// What the registry is doing while it probes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeProgress {
    /// About to try a candidate; may take a while (a driver, sudo, the network).
    Probing { candidate: &'static str },
    /// Done with a candidate.
    Decided { candidate: &'static str, verdict: Verdict },
}

impl fmt::Display for ProbeProgress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Probing { candidate } => write!(formatter, "probing {candidate}…"),
            Self::Decided { candidate, verdict } => write!(formatter, "{candidate}: {verdict}"),
        }
    }
}
