//! What the default [ProviderRegistry](super::ProviderRegistry) probes for.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use carbontracker::{GridLocation, GridLocationError, PowerDomain, SourceDescription};
use carbontracker::{GramsCo2ePerKwh, Watts};

/// What to probe for. `ProbeConfig::default()` measures every power domain it
/// can find against the grid where it estimates the host is.
#[derive(Debug, Clone, Default)]
pub struct ProbeConfig {
    /// The power domains to measure. Unselected domains are never probed.
    pub measure: Measure,
    pub grid: GridConfig,
    /// Figures that replace probing, for deterministic runs.
    pub pinned: Pinned,
    /// Measure only the GPUs running one of these processes. `None` (the
    /// default) measures every GPU. CPU, RAM and SoC rails are whole devices
    /// and are always measured whole.
    pub processes: Option<Vec<u32>>,
}

impl ProbeConfig {
    /// Check everything probing relies on, before anything is probed.
    ///
    /// # Errors
    ///
    /// [ProbeConfigError] naming the first thing wrong.
    pub fn validate(&self) -> Result<(), ProbeConfigError> {
        if matches!(&self.measure, Measure::Only(domains) if domains.is_empty()) {
            return Err(ProbeConfigError::NothingToMeasure);
        }
        if self.grid.forecast_horizon.is_zero() {
            return Err(ProbeConfigError::ZeroForecastHorizon);
        }
        if let Some(location) = &self.grid.location {
            location.validate()?;
            // No provider here serves one, so it could only give the world average.
            if matches!(location, GridLocation::DataCenter { .. }) {
                return Err(ProbeConfigError::DataCenterUnsupported);
            }
        }
        Ok(())
    }
}

/// Which power domains to probe for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Measure {
    /// Every domain this host has.
    #[default]
    All,
    /// Only these. May not be empty.
    Only(Vec<PowerDomain>),
}

impl Measure {
    /// Whether any of `domains` is wanted; checked before probing.
    pub(crate) fn wants_any(&self, domains: &[PowerDomain]) -> bool {
        match self {
            Self::All => true,
            Self::Only(wanted) => domains.iter().any(|domain| wanted.contains(domain)),
        }
    }

    /// Whether a source is wanted. Only power sources are filtered; the grid
    /// is always wanted.
    pub(crate) fn wants(&self, description: &SourceDescription) -> bool {
        match description {
            SourceDescription::Power { domain, .. } => self.wants_any(&[*domain]),
            SourceDescription::Intensity { .. } => true,
        }
    }
}

/// `all`, `cpu` (with RAM), `gpu`, or a comma-separated list: `cpu,gpu`.
impl FromStr for Measure {
    type Err = MeasureParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let mut domains = Vec::new();
        for part in text.split(',').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "all" => return Ok(Self::All),
                "cpu" => domains.extend([PowerDomain::Cpu, PowerDomain::Ram]),
                "gpu" => domains.push(PowerDomain::Gpu),
                _ => return Err(MeasureParseError),
            }
        }
        Ok(Self::Only(domains))
    }
}

/// Why a [Measure] couldn't be read from text.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("expected all, cpu, gpu, or cpu,gpu")]
pub struct MeasureParseError;

/// Where the electricity comes from, and how to look up its intensity.
#[derive(Debug, Clone)]
pub struct GridConfig {
    /// Stated, and never estimated over. `None` estimates where the host is
    /// (one request to an IP geolocation service), falling to the world
    /// average if that fails; state `Global` to skip it. Must be valid if
    /// given; a data centre isn't supported yet.
    pub location: Option<GridLocation>,
    /// Keys for the services that take one: Electricity Maps, for live
    /// intensity and its forecast. A key without a usable location is
    /// declined at probing, not refused here.
    pub api_keys: BTreeMap<KeyedService, ApiKey>,
    /// How far ahead the forecast reaches. Must be above zero.
    pub forecast_horizon: Duration,
}

impl Default for GridConfig {
    fn default() -> Self {
        Self {
            location: None,
            api_keys: BTreeMap::new(),
            // One day: roughly how far forecast APIs publish.
            forecast_horizon: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// Figures that replace probing. Typed, so they can't be negative or NaN.
#[derive(Debug, Clone, Default)]
pub struct Pinned {
    /// A fixed draw replacing every probed power source.
    pub power: Option<Watts>,
    /// A fixed grid intensity replacing every probed intensity and forecast.
    pub intensity: Option<GramsCo2ePerKwh>,
    /// A fixed CPU draw replacing probing for the CPU domain only.
    pub cpu_power: Option<Watts>,
    /// A fixed GPU draw replacing probing for the GPU domain only.
    pub gpu_power: Option<Watts>,
}

/// The services in this set that take an API key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyedService {
    ElectricityMaps,
}

impl KeyedService {
    /// Every keyed service, for listing what is supported.
    pub const ALL: [Self; 1] = [Self::ElectricityMaps];

    /// The names a caller may give it; the first is the one to show.
    pub fn names(self) -> &'static [&'static str] {
        match self {
            Self::ElectricityMaps => &["electricity_maps", "electricitymaps"],
        }
    }

    /// The name to show, and to store it under.
    pub fn name(self) -> &'static str {
        self.names()[0]
    }

    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|service| service.names().contains(&name))
    }
}

/// A secret: `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself, for the request that needs it.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    /// Its last four characters, to tell keys apart without showing one:
    /// `…a1b2`.
    pub fn hint(&self) -> String {
        let tail: String = self.0.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("…{tail}")
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApiKey(…)")
    }
}

/// Why a [ProbeConfig] can't be probed with.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProbeConfigError {
    #[error("no power domain is selected to measure")]
    NothingToMeasure,
    #[error("the forecast horizon must be above zero")]
    ZeroForecastHorizon,
    #[error("the grid location is invalid: {0}")]
    InvalidLocation(#[from] GridLocationError),
    #[error("a data centre can't be served yet; state its zone, country or coordinates")]
    DataCenterUnsupported,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_keyed_service_is_listed_and_found_by_each_name() {
        for service in KeyedService::ALL {
            for name in service.names() {
                assert_eq!(KeyedService::from_name(name), Some(service));
            }
        }
        // A new service fails this match: add it to ALL as well.
        let listed = |service| match service {
            KeyedService::ElectricityMaps => KeyedService::ALL.contains(&service),
        };
        assert!(listed(KeyedService::ElectricityMaps));
        assert_eq!(KeyedService::from_name("electricity-maps"), None);
    }

    #[test]
    fn components_map_to_power_domains() {
        assert_eq!("all".parse(), Ok(Measure::All));
        assert_eq!("gpu".parse(), Ok(Measure::Only(vec![PowerDomain::Gpu])));
        assert_eq!(
            " CPU, gpu".parse(),
            Ok(Measure::Only(vec![PowerDomain::Cpu, PowerDomain::Ram, PowerDomain::Gpu]))
        );
        assert_eq!("tpu".parse::<Measure>(), Err(MeasureParseError));
    }
}
