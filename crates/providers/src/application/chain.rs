//! [Chain]: walks the candidates, best first, keeping one source per
//! coverage slot.

use std::collections::HashSet;

use carbontracker::{
    Cooldown, PowerDomain, Provider, ProviderError, ProviderId, ProviderResponse, SourceDescription, SourceId,
    SourceKind,
};

use crate::application::resolution::{ProbeProgress, Resolution};
use crate::domain::config::Measure;
use crate::domain::verdict::{CandidateOutcome, Coverage, Verdict};
use crate::ports::Outcome;

/// Chain walk state: covered slots and per-candidate outcomes.
pub(crate) struct Chain<'a> {
    wanted: &'a Measure,
    covered: HashSet<Coverage>,
    /// Domains a static CPU or GPU took: no other candidate serves them.
    pub(crate) pinned: Vec<PowerDomain>,
    pub(crate) result: Resolution,
    progress: &'a mut dyn FnMut(ProbeProgress),
}

impl<'a> Chain<'a> {
    pub(crate) fn new(wanted: &'a Measure, progress: &'a mut dyn FnMut(ProbeProgress)) -> Self {
        Self { wanted, covered: HashSet::new(), pinned: Vec::new(), result: Resolution::empty(), progress }
    }

    /// Report that `candidate` is about to be tried.
    pub(crate) fn probing(&mut self, candidate: &'static str) {
        (self.progress)(ProbeProgress::Probing { candidate });
    }

    /// Whether probing this candidate could still add anything. Checked before
    /// probing since probes can be expensive (sudo, sampler threads).
    pub(crate) fn worth_probing(&mut self, name: &'static str, domains: &[PowerDomain]) -> bool {
        if !self.wanted.wants_any(domains) {
            self.record(name, Verdict::NotWanted);
            return false;
        }

        // Domain-level only (ignores device), hence NVML runs before SoC probes.
        let covered = |domain: &&PowerDomain| {
            self.covered.iter().any(|coverage| {
                matches!(coverage, Coverage::Power { domain: taken, .. } if taken == *domain)
            })
        };

        if domains.iter().all(|d| covered(&d)) {
            self.record(name, Verdict::Covered);
            return false;
        }

        self.probing(name);
        true
    }

    /// Keep the slots this candidate newly covers; drop it entirely if none.
    pub(crate) fn take(&mut self, id: ProviderId, name: &'static str, outcome: Option<Outcome>) {
        let Some(outcome) = outcome else {
            self.record(name, Verdict::Declined);
            return;
        };

        let offered = outcome.descriptions.len();
        let mut labels = Vec::new();
        let mut kept = HashSet::new();
        for (source, description) in outcome.descriptions {
            if !self.wanted.wants(&description) {
                continue;
            }
            if matches!(&description, SourceDescription::Power { domain, .. } if self.pinned.contains(domain)) {
                continue;
            }
            // Slot already taken by a better candidate.
            if !self.covered.insert(Coverage::of(&description)) {
                continue;
            }
            labels.push(description.to_string());
            kept.insert(source);
            let displaced = self.result.descriptions.insert(source, description);
            debug_assert!(displaced.is_none(), "{source:?} minted twice");
        }

        if labels.is_empty() {
            self.record(name, Verdict::Covered);
            return;
        }

        self.record(name, Verdict::Serving(labels));
        // A provider serves exactly the sources kept for it.
        let provider = if kept.len() == offered { outcome.provider } else { Box::new(Kept { provider: outcome.provider, kept }) };
        self.result.providers.insert(id, provider);
        self.result.served_by.insert(id, name);
    }

    /// Whether any source of this kind is already resolved. Needed for intensity,
    /// where zone and global coverage keys differ but must not both be kept.
    pub(crate) fn already_has(&self, kind: SourceKind) -> bool {
        self.result.descriptions.values().any(|d| d.kind() == kind)
    }

    pub(crate) fn record(&mut self, name: &'static str, verdict: Verdict) {
        (self.progress)(ProbeProgress::Decided { candidate: name, verdict: verdict.clone() });
        self.result.candidates.push(CandidateOutcome { name, verdict });
    }
}

/// A provider whose answers are cut to the sources the chain kept for it.
struct Kept {
    provider: Box<dyn Provider>,
    kept: HashSet<SourceId>,
}

impl Provider for Kept {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let mut kept = ProviderResponse::new();
        for (source, reading) in self.provider.read()?.into_readings() {
            if self.kept.contains(&source) {
                kept.insert(source, reading);
            }
        }
        Ok(kept)
    }

    fn cooldown(&self) -> Cooldown {
        self.provider.cooldown()
    }
}
