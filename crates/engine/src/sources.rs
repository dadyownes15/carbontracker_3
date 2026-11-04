//! [Sources]: every provider a tracker reads, and what each of their sources
//! measures. A provider registry creates it; [start](crate::start) runs on it.

use std::collections::HashMap;

use crate::adapters::outbound::readers::Reader;
use crate::config::{ConfigError, FetchPolicy};
use crate::domain::source::{SourceCatalog, SourceDescription, SourceKind};
use crate::domain::{ProviderId, SourceId, UnreadyActions};
use crate::ports::outbound::Provider;

/// Every provider a tracker reads, and what each of their sources measures.
///
/// Checked as it is built. A source's role follows from its description: the
/// one [Intensity](SourceDescription::Intensity) source is the grid
/// emissions are counted against.
#[derive(Default)]
pub struct Sources {
    providers: HashMap<ProviderId, Box<dyn Provider>>,
    descriptions: HashMap<SourceId, SourceDescription>,
    intensity: Option<SourceId>,
}

/// Why [Sources] refused a provider.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourcesError {
    #[error("provider {0:?} was already added")]
    DuplicateProvider(ProviderId),
    #[error("source {0} was already added")]
    DuplicateSource(SourceId),
    #[error("source {id} does not belong to provider {provider:?}")]
    ForeignSource { id: SourceId, provider: ProviderId },
    #[error("source {0} would be a second grid intensity; emissions are counted against one")]
    SecondIntensity(SourceId),
    #[error("provider {0:?} serves both power and intensity sources; a provider serves one kind")]
    MixedKinds(ProviderId),
}

impl Sources {
    /// No providers yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add `reader` under `provider`, serving `sources`. Nothing is added if
    /// any of it is refused.
    ///
    /// # Errors
    ///
    /// [SourcesError] if `provider` or a source was already added, a source
    /// names another provider, the sources are not all of one kind, or a
    /// source would be a second intensity.
    pub fn add_provider(
        &mut self,
        provider: ProviderId,
        reader: impl Provider,
        sources: impl IntoIterator<Item = (SourceId, SourceDescription)>,
    ) -> Result<(), SourcesError> {
        if self.providers.contains_key(&provider) {
            return Err(SourcesError::DuplicateProvider(provider));
        }
        let sources: Vec<(SourceId, SourceDescription)> = sources.into_iter().collect();
        if sources.windows(2).any(|pair| pair[0].1.kind() != pair[1].1.kind()) {
            return Err(SourcesError::MixedKinds(provider));
        }

        let mut intensity = self.intensity;
        for (index, (source, description)) in sources.iter().enumerate() {
            if source.provider() != provider {
                return Err(SourcesError::ForeignSource {
                    id: *source,
                    provider,
                });
            }
            if self.descriptions.contains_key(source)
                || sources[..index].iter().any(|(seen, _)| seen == source)
            {
                return Err(SourcesError::DuplicateSource(*source));
            }
            match description.kind() {
                SourceKind::Intensity if intensity.is_some() => {
                    return Err(SourcesError::SecondIntensity(*source));
                }
                SourceKind::Intensity => intensity = Some(*source),
                SourceKind::Power => {}
            }
        }

        self.providers.insert(provider, Box::new(reader));
        self.descriptions.extend(sources);
        self.intensity = intensity;
        Ok(())
    }

    /// Split into a [Reader] per provider, read under `fetch`, and the
    /// [SourceCatalog] that describes the sources. Asks each provider its
    /// cooldown.
    ///
    /// # Errors
    ///
    /// [ConfigError::UnknownProvider] if `fetch` names a provider these
    /// sources do not have.
    pub(crate) fn split(
        self,
        fetch: &FetchPolicy,
    ) -> Result<(HashMap<ProviderId, Reader>, SourceCatalog), ConfigError> {
        if let Some(&unknown) = fetch
            .providers
            .keys()
            .find(|id| !self.providers.contains_key(id))
        {
            return Err(ConfigError::UnknownProvider(unknown));
        }
        let mut by_provider: HashMap<ProviderId, Vec<SourceId>> = HashMap::new();
        for &source in self.descriptions.keys() {
            by_provider
                .entry(source.provider())
                .or_default()
                .push(source);
        }

        let readers = self
            .providers
            .into_iter()
            .map(|(id, provider)| {
                let serves = by_provider.remove(&id).unwrap_or_default();
                // A provider serves one kind: power queues, the intensity skips.
                let serves_intensity = serves
                    .iter()
                    .any(|source| self.descriptions[source].kind() == SourceKind::Intensity);
                let default = if serves_intensity {
                    UnreadyActions::SKIP
                } else {
                    UnreadyActions::QUEUE
                };
                let policy = fetch.providers.get(&id).copied().unwrap_or(default);
                (id, Reader::new(provider, serves, policy, fetch.timeout).retrying(fetch.retry_for))
            })
            .collect();
        let catalog = SourceCatalog::pairing_all(self.descriptions, self.intensity);
        Ok((readers, catalog))
    }
}
