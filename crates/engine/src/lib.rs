#![warn(unnameable_types)]

mod adapters;
mod application;
mod config;
mod domain;
mod ports;
mod sources;
mod start;

/// The pure replay the QuickCheck suite drives; see `quickcheck/`.
#[cfg(feature = "replay")]
pub mod replay;

#[cfg(test)]
mod tests;

// Starting a tracker, and its handle.
pub use adapters::inbound::handle::{TrackerHandle, WeakTrackerHandle};
pub use application::subscribers::{Subscription, SubscriptionEnded};
pub use config::{Announcements, ConfigError, FetchPolicy, TrackerConfig};
pub use start::{start, start_subscribed};

// The provider port: what a provider registry builds and a tracker reads.
pub use domain::{ActionWhenUnready, Cooldown, Failure, Observation, Skip, UnreadyActions};
pub use ports::outbound::{Provider, ProviderResponse, Reading};
pub use sources::{Sources, SourcesError};

// What callers pass in.
pub use domain::{
    GridLocation, GridLocationError, PowerDomain, PowerUsageEffectiveness, SourceSelection, SpanId,
    Work, WorkEntry,
};

// What the tracker sends back: events, reports and errors.
pub use domain::{
    ProviderError, ReadoutId, ReportWarning, Sample, SessionReport, SpanReport, TrackerError,
    TrackerEvent, Usage,
};

// What callers may ask the tracker, and where it answers.
pub use domain::{Reply, TrackerQuery};

// What the sources are.
pub use domain::{
    CatalogReport, Fidelity, PowerScope, ProviderId, SampleKind, SourceDescription, SourceId,
    SourceKind,
};

// Units.
pub use domain::{GramsCo2e, GramsCo2ePerKwh, Joules, KilowattHours, Timestamp, UnitError, Watts};
