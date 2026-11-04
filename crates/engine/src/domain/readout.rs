//! The request and response of one [readout](ReadoutRequest): a pull from every provider at once.

use std::collections::HashMap;

use super::reading::{Failure, Skip};
use super::sample_store::Sample;
use super::source::SourceId;

/// Identifies one readout.
///
/// A [Cut](crate::domain::Cut) waits on the readout with its id before the segments it bounds can settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ReadoutId(pub usize);

impl From<usize> for ReadoutId {
    fn from(n: usize) -> Self {
        Self(n)
    }
}

/// Ask a [Meter](crate::ports::outbound::Meter) to read `sources` once.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(test)]
pub struct ReadoutRequest {
    pub id: ReadoutId,
    /// The sources to read. Only the providers serving them are called.
    pub sources: Vec<SourceId>,
}

/// What became of one source in one readout. A provider says what it read;
/// the engine says what became of each source.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Observation {
    /// The provider's fresh samples. Never empty.
    Samples(Vec<Sample>),
    /// The provider had nothing new.
    Unchanged,
    /// Not read, while all is well.
    Skipped(Skip),
    /// Not read, or not readable, and something is wrong.
    Failed(Failure),
}

/// The answer to one [ReadoutRequest]: one [Observation] per source.
#[derive(Debug)]
pub struct ReadoutResponse {
    pub id: ReadoutId,
    outcomes: HashMap<SourceId, Observation>,
}

impl ReadoutResponse {
    /// Create an empty response to the [ReadoutRequest] with this [ReadoutId].
    pub fn new(id: ReadoutId) -> Self {
        Self {
            id,
            outcomes: HashMap::new(),
        }
    }

    /// Record the [Sample]s `source` reported, as [Observation::Unchanged] if
    /// none. Replaces any earlier outcome for `source`.
    #[cfg(test)]
    pub fn read(&mut self, source: SourceId, samples: Vec<Sample>) {
        let observation = if samples.is_empty() {
            Observation::Unchanged
        } else {
            Observation::Samples(samples)
        };
        self.outcomes.insert(source, observation);
    }

    /// Record `source`'s [Observation], whatever it is. Replaces any earlier
    /// outcome for `source`.
    pub fn observe(&mut self, source: SourceId, observation: Observation) {
        self.outcomes.insert(source, observation);
    }

    /// Iterate over the sources that reported, with their [Sample]s.
    #[cfg(test)]
    pub fn samples(&self) -> impl Iterator<Item = (SourceId, &[Sample])> {
        self.outcomes
            .iter()
            .filter_map(|(&source, outcome)| match outcome {
                Observation::Samples(samples) => Some((source, samples.as_slice())),
                _ => None,
            })
    }

    /// Consume the response, yielding each source's [Observation].
    pub fn into_outcomes(self) -> impl Iterator<Item = (SourceId, Observation)> {
        self.outcomes.into_iter()
    }
}

/// Why a provider could not read a source. The variant says what a user
/// would have to do about it; the detail is for people.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum ProviderError {
    /// The device, file or service could not be reached.
    #[error("unavailable: {0}")]
    Unavailable(String),
    /// Reached, and access was refused.
    #[error("access denied: {0}")]
    Denied(String),
    /// The service refused because it was asked too often.
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// It answered, with something that cannot be used.
    #[error("invalid answer: {0}")]
    Invalid(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::source::ProviderId;

    fn source(n: u16) -> SourceId {
        SourceId::new(ProviderId(1), n)
    }

    fn sample() -> Sample {
        Sample::new(chrono::DateTime::UNIX_EPOCH, 1.0)
    }

    /// AC/ENG-47, AC/ENG-48, AC/MEAS-18
    #[test]
    fn reading_no_samples_records_the_source_as_unchanged() {
        let mut response = ReadoutResponse::new(ReadoutId(1));
        response.read(source(0), Vec::new());

        assert_eq!(
            response.into_outcomes().collect::<Vec<_>>(),
            vec![(source(0), Observation::Unchanged)]
        );
    }

    #[test]
    fn the_latest_outcome_for_a_source_wins() {
        let mut response = ReadoutResponse::new(ReadoutId(1));
        response.observe(source(0), Observation::Failed(Failure::TimedOut));
        response.read(source(0), vec![sample()]);

        assert_eq!(response.samples().count(), 1);
    }

    /// AC/ENG-63
    #[test]
    fn every_provider_error_carries_its_detail_for_people() {
        let detail = "the counter file is gone";
        for error in [
            ProviderError::Unavailable(detail.to_owned()),
            ProviderError::Denied(detail.to_owned()),
            ProviderError::RateLimited(detail.to_owned()),
            ProviderError::Invalid(detail.to_owned()),
        ] {
            assert!(error.to_string().contains(detail), "{error}");
        }
    }
}
