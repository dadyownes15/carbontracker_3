//! The contract every provider implements.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::domain::reading::Cooldown;
use crate::domain::sample_store::Sample;
use crate::domain::{ProviderError, SourceId};

/// Reads its sources. Knows how to get a value from a device, a file or a
/// service, and nothing about when it will be asked.
///
/// The engine lends each read a thread of its own, never reads a provider
/// concurrently or sooner than its cooldown allows, and never reads, asks or
/// drops it on the tracker thread. A provider bounds its own waits, owns
/// everything that outlives a read, and reports failure as a
/// [ProviderError], never by panicking.
pub trait Provider: Send + 'static {
    /// Read every source this provider serves, with exactly one [Reading]
    /// each. May block.
    ///
    /// # Errors
    ///
    /// A [ProviderError] if the read failed as a whole: it fails every source
    /// the provider serves.
    fn read(&mut self) -> Result<ProviderResponse, ProviderError>;

    /// How long a reading stays current. Asked once, when the tracker starts.
    fn cooldown(&self) -> Cooldown {
        Cooldown::After(Duration::ZERO)
    }
}

/// A boxed provider reads as the provider in it.
impl<P: Provider + ?Sized> Provider for Box<P> {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        (**self).read()
    }

    fn cooldown(&self) -> Cooldown {
        (**self).cooldown()
    }
}

/// What one source had to say in one read.
#[derive(Debug, Clone, PartialEq)]
pub enum Reading {
    /// New observations since this provider's last read; never empty.
    Samples(Vec<Sample>),
    /// Read, with no new observation since the last read.
    Unchanged,
    /// This source could not be read, or what it read cannot be used.
    Failed(ProviderError),
}

/// What a read answered: one [Reading] per source the provider serves.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderResponse {
    readings: BTreeMap<SourceId, Reading>,
}

impl ProviderResponse {
    /// No readings yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// `source`'s [Reading], replacing any earlier one.
    pub fn insert(&mut self, source: SourceId, reading: Reading) -> &mut Self {
        self.readings.insert(source, reading);
        self
    }

    /// `source`'s samples, or [Reading::Unchanged] if there are none.
    pub fn samples(&mut self, source: SourceId, samples: Vec<Sample>) -> &mut Self {
        let reading = if samples.is_empty() {
            Reading::Unchanged
        } else {
            Reading::Samples(samples)
        };
        self.insert(source, reading)
    }

    /// `source`'s [Reading], if it has one.
    pub fn get(&self, source: SourceId) -> Option<&Reading> {
        self.readings.get(&source)
    }

    /// Every source with its [Reading].
    pub fn into_readings(self) -> impl Iterator<Item = (SourceId, Reading)> {
        self.readings.into_iter()
    }
}
