//! How a tracker runs. What it measures is the provider registry's business;
//! see [ProbeConfig](crate::ProbeConfig).

use std::collections::BTreeMap;
use std::time::Duration;

use crate::domain::{PowerUsageEffectiveness, ProviderId, UnreadyActions};

/// How a tracker runs, checked by [start](crate::start).
#[derive(Debug, Clone)]
pub struct TrackerConfig {
    /// The longest gap between readouts. Every accepted cut reads at once
    /// and resets it, so the watchdog only reads when no cut has. Must be
    /// above zero.
    pub readout_interval: Duration,
    /// The facility's overhead per watt the machine draws (cooling, power
    /// conversion). Scales every span's energy and emissions; 1.0 by default.
    pub pue: PowerUsageEffectiveness,
    /// Which events the tracker announces.
    pub announce: Announcements,
    /// How long a read may take, and what each provider does while unready.
    pub fetch: FetchPolicy,
}

/// How providers are read: the timeout every read is held to, and what each
/// provider does with a request while unready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPolicy {
    /// How long a read may take before it is abandoned. 10 s by default;
    /// above zero.
    pub timeout: Duration,
    /// How long a cut's request to a provider that queues stays open after a
    /// failed read, sent again each time the provider is ready. 30 s by
    /// default; zero lets a failed read answer it at once.
    pub retry_for: Duration,
    /// What each provider does while unready. A provider left out queues on
    /// both if it serves power, and skips on both if it serves the intensity.
    pub providers: BTreeMap<ProviderId, UnreadyActions>,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            retry_for: Duration::from_secs(30),
            providers: BTreeMap::new(),
        }
    }
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            readout_interval: Duration::from_secs(1),
            pue: PowerUsageEffectiveness::default(),
            announce: Announcements::default(),
            fetch: FetchPolicy::default(),
        }
    }
}

impl TrackerConfig {
    /// Check everything a tracker relies on.
    ///
    /// # Errors
    ///
    /// [ConfigError::ZeroReadoutInterval] if the readout interval is zero,
    /// [ConfigError::ZeroFetchTimeout] if the fetch timeout is.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.readout_interval.is_zero() {
            return Err(ConfigError::ZeroReadoutInterval);
        }
        if self.fetch.timeout.is_zero() {
            return Err(ConfigError::ZeroFetchTimeout);
        }
        Ok(())
    }
}

/// Why a [TrackerConfig] can't start a tracker.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("the readout interval must be above zero")]
    ZeroReadoutInterval,
    #[error("the fetch timeout must be above zero")]
    ZeroFetchTimeout,
    /// The fetch policy names a provider the sources do not have.
    #[error("the fetch policy names provider {0:?}, which the sources do not have")]
    UnknownProvider(ProviderId),
}

/// Which events the tracker announces. One that is off costs nothing: its
/// payload is never built. All are on by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announcements {
    /// `ReadoutLanded`, with every source's samples.
    pub readouts: bool,
    /// `SpanStarted`, `SpanStopped` and `Marked`.
    pub cuts: bool,
    /// `SpanSettled`, with the span's final report.
    pub settled: bool,
    /// `CommandRefused`.
    pub refusals: bool,
}

impl Default for Announcements {
    fn default() -> Self {
        Self {
            readouts: true,
            cuts: true,
            settled: true,
            refusals: true,
        }
    }
}
