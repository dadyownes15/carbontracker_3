//! What a [`SourceId`] measures, and the [SourceCatalog] of every source
//! resolved for a session. The id says who serves it; this says what it is.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::domain::sample_store::SampleKind;

/// A provider's own slot. Opaque outside this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct SourceSlot(u16);

/// One sample series: provider plus that provider's slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId {
    provider_id: ProviderId,
    slot: SourceSlot,
}

impl SourceId {
    pub fn new(provider_id: ProviderId, slot: u16) -> Self {
        Self {
            provider_id,
            slot: SourceSlot(slot),
        }
    }

    pub fn provider(&self) -> ProviderId {
        self.provider_id
    }

    pub fn slot(&self) -> u16 {
        self.slot.0
    }

    #[cfg(test)]
    pub fn for_test(provider: usize, slot: u16) -> Self {
        Self::new(ProviderId(provider), slot)
    }
}

/// `provider.slot`, e.g. `2.0`: a string, so a source can key a JSON map.
impl fmt::Display for SourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.provider_id.0, self.slot.0)
    }
}

#[cfg(feature = "serde")]
impl serde::Serialize for SourceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct ProviderId(pub usize);

/// What a series measures.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SourceDescription {
    Power {
        device_name: String,
        domain: PowerDomain,
        scope: PowerScope,
        sample_kind: SampleKind,
        fidelity: Fidelity,
    },
    Intensity {
        location: GridLocation,
        fidelity: Fidelity,
    },
}

impl SourceDescription {
    pub const fn kind(&self) -> SourceKind {
        match self {
            Self::Power { .. } => SourceKind::Power,
            Self::Intensity { .. } => SourceKind::Intensity,
        }
    }

    /// How the source's series is filled in between samples.
    pub const fn sample_kind(&self) -> SampleKind {
        match self {
            Self::Power { sample_kind, .. } => *sample_kind,
            Self::Intensity { .. } => SampleKind::Held,
        }
    }

    /// How trustworthy this number is.
    pub const fn fidelity(&self) -> Fidelity {
        match self {
            Self::Power { fidelity, .. } | Self::Intensity { fidelity, .. } => *fidelity,
        }
    }

    /// Whether this series already accounts for everything `other` measures.
    /// Scopes must match.
    pub fn subsumes(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Power {
                    domain: outer,
                    scope: outer_scope,
                    ..
                },
                Self::Power {
                    domain: inner,
                    scope: inner_scope,
                    ..
                },
            ) => outer_scope == inner_scope && outer.subsumes(*inner),
            _ => false,
        }
    }
}

/// The sources in `sources` not subsumed by another member; safe to sum.
pub fn independent_sources<Id: Copy>(sources: &HashMap<Id, SourceDescription>) -> Vec<Id> {
    sources
        .iter()
        .filter(|(_, candidate)| !sources.values().any(|other| other.subsumes(candidate)))
        .map(|(&id, _)| id)
        .collect()
}

/// Plain words, e.g. `Apple M2 Pro (cpu)` or `modelled grid DK`. Only a
/// fidelity other than measured is named.
impl fmt::Display for SourceDescription {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Power { device_name, domain, scope, fidelity, .. } => {
                formatter.write_str(&power_label(device_name, &[*domain], scope, *fidelity))
            }
            Self::Intensity { location, fidelity } => {
                write!(formatter, "{}", qualified(*fidelity))?;
                match location {
                    GridLocation::Global => formatter.write_str("world-average grid"),
                    location => write!(formatter, "grid {location}"),
                }
            }
        }
    }
}

/// The labels of `sources`, one per device, scope and fidelity, sorted. A
/// device's domains are listed in source order.
pub fn labels<'a>(sources: impl IntoIterator<Item = (&'a SourceId, &'a SourceDescription)>) -> Vec<String> {
    let mut sources: Vec<_> = sources.into_iter().collect();
    sources.sort_by_key(|(id, _)| **id);
    let mut devices: Vec<(Device, Vec<PowerDomain>)> = Vec::new();
    let mut labels = Vec::new();
    for (_, description) in sources {
        match description {
            SourceDescription::Power { device_name, domain, scope, fidelity, .. } => {
                let key = (device_name.as_str(), scope, *fidelity);
                match devices.iter_mut().find(|(seen, _)| *seen == key) {
                    Some((_, domains)) => domains.push(*domain),
                    None => devices.push((key, vec![*domain])),
                }
            }
            intensity => labels.push(intensity.to_string()),
        }
    }
    labels.extend(
        devices.into_iter().map(|((device, scope, fidelity), domains)| power_label(device, &domains, scope, fidelity)),
    );
    labels.sort();
    labels
}

/// A device's name, scope and fidelity: what a label groups domains by.
type Device<'a> = (&'a str, &'a PowerScope, Fidelity);

fn power_label(device: &str, domains: &[PowerDomain], scope: &PowerScope, fidelity: Fidelity) -> String {
    let domains: Vec<String> = domains.iter().map(ToString::to_string).collect();
    let scope = match scope {
        PowerScope::DeviceTotal => String::new(),
        PowerScope::Process { pid } => format!(" for process {pid}"),
        PowerScope::Job { job_id } => format!(" for job {job_id}"),
        PowerScope::Container { container_id } => format!(" for container {container_id}"),
    };
    format!("{}{device} ({}){scope}", qualified(fidelity), domains.join(", "))
}

/// `modelled ` and the like; empty for a measured source.
fn qualified(fidelity: Fidelity) -> String {
    match fidelity {
        Fidelity::Measured => String::new(),
        other => format!("{other} "),
    }
}

/// How close a number is to the thing it describes. Ordered best first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Fidelity {
    /// A counter on this device, or live data for this grid.
    Measured,
    /// A figure the caller supplied.
    Stated,
    /// A nameplate figure or table lookup for this class of device or place.
    Modelled,
    /// A fixed fallback, e.g. the world-average grid.
    Assumed,
}

impl Fidelity {
    /// Whether a total resting on this should be flagged in reports.
    pub const fn needs_caveat(self) -> bool {
        matches!(self, Self::Modelled | Self::Assumed)
    }
}

impl fmt::Display for Fidelity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let word = match self {
            Self::Measured => "measured",
            Self::Stated => "stated",
            Self::Modelled => "modelled",
            Self::Assumed => "assumed",
        };
        formatter.write_str(word)
    }
}

/// Which sources a caller means, by domain. Never names a provider.
///
/// A selection decides what a span can be billed for, and so what a readout
/// reads.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SourceSelection {
    /// Every power and intensity source.
    #[default]
    All,
    /// Only these power domains. Intensity sources always match.
    Domains(Vec<PowerDomain>),
    /// Exactly these sources, as `catalog()` names them. Intensity sources
    /// always match.
    Sources(Vec<SourceId>),
}

impl SourceSelection {
    /// Whether source `id`, which measures `description`, is selected.
    pub fn matches(&self, id: SourceId, description: &SourceDescription) -> bool {
        match (self, description) {
            (_, SourceDescription::Intensity { .. }) => true,
            (Self::All, SourceDescription::Power { .. }) => true,
            (Self::Domains(wanted), SourceDescription::Power { domain, .. }) => {
                wanted.contains(domain)
            }
            (Self::Sources(wanted), SourceDescription::Power { .. }) => wanted.contains(&id),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SourceKind {
    Power,
    Intensity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum PowerDomain {
    Cpu,
    Gpu,
    Ram,
    Ane,
    System,
}

impl fmt::Display for PowerDomain {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
            Self::Ram => "ram",
            Self::Ane => "ane",
            Self::System => "system",
        })
    }
}

impl PowerDomain {
    /// Whether a reading of this domain already contains `other`'s.
    /// `System` is the whole-device rail and covers every component.
    pub const fn subsumes(self, other: Self) -> bool {
        matches!(self, Self::System) && !matches!(other, Self::System)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum PowerScope {
    DeviceTotal,

    Process { pid: u32 },

    Job { job_id: String },

    Container { container_id: String },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum GridLocation {
    Zone {
        zone_id: String,
    },

    Country {
        country_code: String,
    },

    DataCenter {
        cloud_provider: String,
        region: String,
    },

    Coordinates {
        latitude: f64,
        longitude: f64,
    },

    Global,
}

/// The grid's own name, e.g. `DK-DK2`, `DK` or `aws eu-north-1`.
impl fmt::Display for GridLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zone { zone_id } => formatter.write_str(zone_id),
            Self::Country { country_code } => formatter.write_str(&country_code.to_uppercase()),
            Self::DataCenter { cloud_provider, region } => write!(formatter, "{cloud_provider} {region}"),
            Self::Coordinates { latitude, longitude } => write!(formatter, "({latitude:.2}, {longitude:.2})"),
            Self::Global => formatter.write_str("world average"),
        }
    }
}

// Manual Eq/Hash over f64 bit patterns so `GridLocation` can be a map key.
impl PartialEq for GridLocation {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Zone { zone_id: left }, Self::Zone { zone_id: right }) => left == right,
            (
                Self::Country { country_code: left },
                Self::Country {
                    country_code: right,
                },
            ) => left == right,
            (
                Self::DataCenter {
                    cloud_provider: left_provider,
                    region: left_region,
                },
                Self::DataCenter {
                    cloud_provider: right_provider,
                    region: right_region,
                },
            ) => left_provider == right_provider && left_region == right_region,
            (
                Self::Coordinates {
                    latitude: left_latitude,
                    longitude: left_longitude,
                },
                Self::Coordinates {
                    latitude: right_latitude,
                    longitude: right_longitude,
                },
            ) => {
                left_latitude.to_bits() == right_latitude.to_bits()
                    && left_longitude.to_bits() == right_longitude.to_bits()
            }
            (Self::Global, Self::Global) => true,
            _ => false,
        }
    }
}

impl Eq for GridLocation {}

impl Hash for GridLocation {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match self {
            Self::Zone { zone_id } => {
                0_u8.hash(state);
                zone_id.hash(state);
            }
            Self::Country { country_code } => {
                1_u8.hash(state);
                country_code.hash(state);
            }
            Self::DataCenter {
                cloud_provider,
                region,
            } => {
                2_u8.hash(state);
                cloud_provider.hash(state);
                region.hash(state);
            }
            Self::Coordinates {
                latitude,
                longitude,
            } => {
                3_u8.hash(state);
                latitude.to_bits().hash(state);
                longitude.to_bits().hash(state);
            }
            Self::Global => 4_u8.hash(state),
        }
    }
}

impl GridLocation {
    pub fn validate(&self) -> Result<(), GridLocationError> {
        match self {
            Self::Zone { zone_id } => {
                if zone_id.trim().is_empty() {
                    return Err(GridLocationError::EmptyZoneId);
                }
            }

            Self::Country { country_code } => {
                let country_code = country_code.trim();

                if country_code.len() != 2
                    || !country_code.bytes().all(|byte| byte.is_ascii_alphabetic())
                {
                    return Err(GridLocationError::InvalidCountryCode {
                        value: country_code.to_owned(),
                    });
                }
            }

            Self::DataCenter {
                cloud_provider,
                region,
            } => {
                if cloud_provider.trim().is_empty() {
                    return Err(GridLocationError::EmptyDataCenterProvider);
                }

                if region.trim().is_empty() {
                    return Err(GridLocationError::EmptyDataCenterRegion);
                }
            }

            Self::Coordinates {
                latitude,
                longitude,
            } => {
                if !latitude.is_finite() || !(-90.0..=90.0).contains(latitude) {
                    return Err(GridLocationError::InvalidLatitude { value: *latitude });
                }

                if !longitude.is_finite() || !(-180.0..=180.0).contains(longitude) {
                    return Err(GridLocationError::InvalidLongitude { value: *longitude });
                }
            }

            Self::Global => {}
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum GridLocationError {
    #[error("grid zone identifier cannot be empty")]
    EmptyZoneId,

    #[error("country code must contain exactly two ASCII letters, got {value}")]
    InvalidCountryCode { value: String },

    #[error("data-center provider cannot be empty")]
    EmptyDataCenterProvider,

    #[error("data-center region cannot be empty")]
    EmptyDataCenterRegion,

    #[error("latitude must be finite and between -90 and 90, got {value}")]
    InvalidLatitude { value: f64 },

    #[error("longitude must be finite and between -180 and 180, got {value}")]
    InvalidLongitude { value: f64 },
}

/// What the session knows about the resolved sources.
///
/// Unlike the providers' own wiring, it holds no providers.
#[derive(Debug, Clone, Default)]
pub struct SourceCatalog {
    /// What each source measures (labels, units, subsumption).
    pub descriptions: HashMap<SourceId, SourceDescription>,
    /// Each power source's intensity source. The pairing never changes.
    pub pairs: HashMap<SourceId, SourceId>,
}

impl SourceCatalog {
    /// A catalog of `descriptions` with every power source paired with
    /// `intensity`, or with none.
    pub fn pairing_all(
        descriptions: HashMap<SourceId, SourceDescription>,
        intensity: Option<SourceId>,
    ) -> Self {
        let pairs = intensity
            .map(|intensity| {
                descriptions
                    .iter()
                    .filter(|(_, description)| description.kind() == SourceKind::Power)
                    .map(|(&power, _)| (power, intensity))
                    .collect()
            })
            .unwrap_or_default();
        Self {
            descriptions,
            pairs,
        }
    }

    /// The intensity source `power` is billed against, if any.
    pub fn intensity_of(&self, power: SourceId) -> Option<SourceId> {
        self.pairs.get(&power).copied()
    }

    /// The intensity sources `powers` are paired with, with what each measures.
    pub fn intensities_of(&self, powers: impl IntoIterator<Item = SourceId>) -> HashMap<SourceId, SourceDescription> {
        powers
            .into_iter()
            .filter_map(|power| self.intensity_of(power))
            .filter_map(|id| Some((id, self.descriptions.get(&id)?.clone())))
            .collect()
    }


    /// The sources a readout for a cut reads: everything [SourceSelection::All]
    /// selects, so every power and intensity source.
    #[cfg(test)]
    pub fn billable(&self) -> Vec<SourceId> {
        self.selected(self.descriptions.keys().copied(), &SourceSelection::All)
            .into_keys()
            .collect()
    }

}

/// The sources the session measures, and which acts as the grid intensity.
/// How they were chosen is the provider registry's to
/// tell, before the tracker starts.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct CatalogReport {
    /// Every source being sampled, and what it measures.
    pub sources: HashMap<SourceId, SourceDescription>,
    /// Each power source's intensity source; a power source with none is absent.
    pub pairs: HashMap<SourceId, SourceId>,
}

impl SourceCatalog {
    /// Every source being sampled, and the grid intensity.
    pub fn report(&self) -> CatalogReport {
        CatalogReport {
            sources: self.descriptions.clone(),
            pairs: self.pairs.clone(),
        }
    }

    /// The sources among `ids` that `selection` matches, with what each measures.
    pub fn selected(
        &self,
        ids: impl Iterator<Item = SourceId>,
        selection: &SourceSelection,
    ) -> HashMap<SourceId, SourceDescription> {
        ids.filter_map(|id| {
            let description = self.descriptions.get(&id)?;
            selection
                .matches(id, description)
                .then(|| (id, description.clone()))
        })
        .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu() -> SourceId {
        SourceId::for_test(0, 0)
    }
    fn gpu() -> SourceId {
        SourceId::for_test(1, 0)
    }
    fn dk1() -> SourceId {
        SourceId::for_test(2, 0)
    }
    fn se3() -> SourceId {
        SourceId::for_test(3, 0)
    }

    fn power(domain: PowerDomain) -> SourceDescription {
        SourceDescription::Power {
            device_name: format!("{domain:?}"),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Stated,
        }
    }

    fn intensity() -> SourceDescription {
        SourceDescription::Intensity {
            location: GridLocation::Global,
            fidelity: Fidelity::Stated,
        }
    }

    fn descriptions() -> HashMap<SourceId, SourceDescription> {
        HashMap::from([
            (cpu(), power(PowerDomain::Cpu)),
            (gpu(), power(PowerDomain::Gpu)),
            (dk1(), intensity()),
            (se3(), intensity()),
        ])
    }

    /// AC/ENG-24, AC/PROF-28
    #[test]
    fn a_power_source_is_billed_against_the_intensity_it_is_paired_with() {
        let catalog = SourceCatalog {
            descriptions: descriptions(),
            pairs: HashMap::from([(cpu(), dk1()), (gpu(), se3())]),
        };
        assert_eq!(catalog.intensity_of(cpu()), Some(dk1()));
        assert_eq!(catalog.intensity_of(gpu()), Some(se3()));
    }

    /// AC/ENG-24, AC/PROF-28
    #[test]
    fn a_power_source_with_no_pairing_has_no_intensity() {
        let catalog = SourceCatalog {
            descriptions: descriptions(),
            pairs: HashMap::from([(cpu(), dk1())]),
        };
        assert_eq!(catalog.intensity_of(gpu()), None);
    }

    /// AC/ENG-24
    #[test]
    fn pairing_all_pairs_every_power_source_with_the_one_intensity() {
        let catalog = SourceCatalog::pairing_all(descriptions(), Some(dk1()));
        assert_eq!(
            catalog.pairs,
            HashMap::from([(cpu(), dk1()), (gpu(), dk1())])
        );
    }

    /// AC/ENG-24, AC/PROF-28
    #[test]
    fn pairing_all_never_pairs_an_intensity_source() {
        let catalog = SourceCatalog::pairing_all(descriptions(), Some(dk1()));
        assert_eq!(catalog.intensity_of(dk1()), None);
        assert_eq!(catalog.intensity_of(se3()), None);
    }

    /// AC/ENG-24
    #[test]
    fn a_power_only_catalog_pairs_nothing() {
        let catalog = SourceCatalog::pairing_all(descriptions(), None);
        assert!(catalog.pairs.is_empty());
        assert_eq!(catalog.intensity_of(cpu()), None);
    }

    /// AC/PROF-42
    #[test]
    fn the_intensities_of_power_sources_are_those_they_are_paired_with() {
        let catalog = SourceCatalog { descriptions: descriptions(), pairs: HashMap::from([(cpu(), dk1()), (gpu(), se3())]) };
        let keys = |sources: &[SourceId]| {
            let mut keys: Vec<_> = catalog.intensities_of(sources.iter().copied()).into_keys().collect();
            keys.sort();
            keys
        };
        assert_eq!(keys(&[cpu()]), vec![dk1()]);
        assert_eq!(keys(&[cpu(), gpu()]), vec![dk1(), se3()]);
    }

    /// AC/ENG-22
    #[test]
    fn the_catalog_report_lists_each_pair() {
        let pairs = HashMap::from([(cpu(), dk1()), (gpu(), se3())]);
        let catalog = SourceCatalog { descriptions: descriptions(), pairs: pairs.clone() };
        assert_eq!(catalog.report().pairs, pairs);
    }

    /// AC/ENG-25
    #[test]
    fn a_source_is_power_or_intensity_and_never_a_forecast() {
        for description in descriptions().values() {
            // Exhaustive: a kind of its own for a forecast would not compile here.
            match description.kind() {
                SourceKind::Power | SourceKind::Intensity => {}
            }
        }
    }

    fn on(device: &str, domain: PowerDomain, scope: PowerScope, fidelity: Fidelity) -> SourceDescription {
        SourceDescription::Power { device_name: device.into(), domain, scope, sample_kind: SampleKind::Instant, fidelity }
    }

    #[test]
    fn a_source_reads_as_plain_words() {
        let grid = |location, fidelity| SourceDescription::Intensity { location, fidelity }.to_string();
        let m2 = "Apple M2 Pro";
        assert_eq!(on(m2, PowerDomain::Cpu, PowerScope::DeviceTotal, Fidelity::Measured).to_string(), "Apple M2 Pro (cpu)");
        assert_eq!(
            on(m2, PowerDomain::Gpu, PowerScope::Process { pid: 7 }, Fidelity::Modelled).to_string(),
            "modelled Apple M2 Pro (gpu) for process 7"
        );
        assert_eq!(grid(GridLocation::Country { country_code: "dk".into() }, Fidelity::Modelled), "modelled grid DK");
        assert_eq!(grid(GridLocation::Zone { zone_id: "DK-DK2".into() }, Fidelity::Measured), "grid DK-DK2");
        assert_eq!(grid(GridLocation::Global, Fidelity::Assumed), "assumed world-average grid");
    }

    #[test]
    fn labels_name_each_device_once_with_its_domains_in_source_order() {
        let m2 = "Apple M2 Pro";
        let sources = HashMap::from([
            (SourceId::for_test(0, 2), on(m2, PowerDomain::Ane, PowerScope::DeviceTotal, Fidelity::Measured)),
            (SourceId::for_test(0, 0), on(m2, PowerDomain::Cpu, PowerScope::DeviceTotal, Fidelity::Measured)),
            (SourceId::for_test(0, 1), on(m2, PowerDomain::Gpu, PowerScope::DeviceTotal, Fidelity::Measured)),
            (SourceId::for_test(1, 0), on("A100", PowerDomain::Gpu, PowerScope::DeviceTotal, Fidelity::Measured)),
            (SourceId::for_test(2, 0), on(m2, PowerDomain::Ram, PowerScope::DeviceTotal, Fidelity::Modelled)),
        ]);
        assert_eq!(
            labels(sources.iter()),
            vec!["A100 (gpu)", "Apple M2 Pro (cpu, gpu, ane)", "modelled Apple M2 Pro (ram)"]
        );
    }
}
