//! RAPL (Linux): the powercap energy counters of each CPU package and DRAM
//! zone. Only probing is Linux's: reading a counter file works anywhere, so
//! the read is tested on every host.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;

use crate::adapters::system::cpu::device_name;
use crate::domain::counter::Accumulator;
use crate::ports::Outcome;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, Reading, SourceId};
use carbontracker::{Fidelity, PowerDomain, PowerScope, SampleKind, SourceDescription};

/// The counter updates about every millisecond, and irregularly; from 10 ms
/// apart, energy deviates by under 1% (Dauner et al., HotCarbon 2026).
const RAPL_COOLDOWN: Duration = Duration::from_millis(10);

const POWERCAP_ROOT: &str = "/sys/class/powercap";

/// Discovers readable package (CPU) and DRAM zones. Skips `psys`, which overlaps both.
/// `energy_uj` is usually root-only since CVE-2020-8694.
struct RaplZone {
    domain: PowerDomain,
    /// Sysfs zone, e.g. `intel-rapl:1`; distinguishes sockets.
    zone: String,
    energy_path: String,
    max_energy_uj: Option<u64>,
}

fn discover_rapl_zones(root: &std::path::Path) -> Vec<RaplZone> {
    let Ok(dir) = std::fs::read_dir(root) else { return Vec::new() };

    let mut entries: Vec<std::path::PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("intel-rapl:"))
        })
        .collect();
    entries.sort();

    let mut zones = Vec::new();
    for zone_dir in &entries {
        let Ok(name) = std::fs::read_to_string(zone_dir.join("name")) else { continue };
        let name = name.trim();

        let domain = if name == "psys" {
            continue;
        } else if name.contains("package") {
            PowerDomain::Cpu
        } else if name.contains("dram") {
            PowerDomain::Ram
        } else {
            continue;
        };

        let energy_path = zone_dir.join("energy_uj");
        if std::fs::read_to_string(&energy_path).is_err() { continue; }

        let max_energy_uj = std::fs::read_to_string(zone_dir.join("max_energy_range_uj"))
            .ok()
            .and_then(|s| s.trim().parse().ok());

        zones.push(RaplZone {
            domain,
            zone: zone_dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("intel-rapl")
                .to_owned(),
            energy_path: energy_path.to_string_lossy().into_owned(),
            max_energy_uj,
        });
    }
    zones
}

pub(crate) fn probe_rapl(id: ProviderId) -> Option<Outcome> {
    probe_rapl_at(id, std::path::Path::new(POWERCAP_ROOT))
}

/// [probe_rapl] over the powercap tree at `root`.
fn probe_rapl_at(id: ProviderId, root: &std::path::Path) -> Option<Outcome> {
    let zones = discover_rapl_zones(root);
    if zones.is_empty() { return None; }

    let device_name = device_name();

    // Multi-socket: include the zone in the name when a domain repeats, else
    // sockets share one coverage slot and all but one are dropped.
    let mut per_domain: HashMap<PowerDomain, usize> = HashMap::new();
    for zone in &zones {
        *per_domain.entry(zone.domain).or_default() += 1;
    }

    let mut descriptions = Vec::new();
    let mut counters = Vec::new();

    for (slot, zone) in zones.into_iter().enumerate() {
        let sid = SourceId::new(id, slot as u16);
        let device_name = if per_domain.get(&zone.domain).copied().unwrap_or(0) > 1 {
            format!("{device_name} ({})", zone.zone)
        } else {
            device_name.clone()
        };

        descriptions.push((
            sid,
            SourceDescription::Power {
                device_name,
                domain: zone.domain,
                scope: PowerScope::DeviceTotal,
                // RAPL exposes a running total, not a rate.
                sample_kind: SampleKind::Total,
                fidelity: Fidelity::Measured,
            },
        ));
        counters.push(Counter::new(sid, zone.energy_path, zone.max_energy_uj));
    }

    Some(Outcome { descriptions, provider: Box::new(Rapl { counters }) })
}

/// One zone's `energy_uj`, in µJ, wrapping at `max_energy_range_uj`. The
/// tracker's watchdog reads it far more often than the shortest wrap (about
/// 20 minutes at 200 W with a 262 kJ range), so it never wraps twice unseen.
struct Counter {
    source: SourceId,
    path: String,
    total: Accumulator,
}

impl Counter {
    fn new(source: SourceId, path: String, range_uj: Option<u64>) -> Self {
        Self { source, path, total: Accumulator::new(range_uj) }
    }

    /// Look at the counter now.
    fn read(&mut self) -> Reading {
        let text = std::fs::read_to_string(&self.path);
        let at = Utc::now();
        match text {
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                Reading::Failed(ProviderError::Denied(format!("{}: {error}", self.path)))
            }
            Err(error) => Reading::Failed(ProviderError::Unavailable(format!("{}: {error}", self.path))),
            Ok(text) => match text.trim().parse::<u64>() {
                Ok(raw_uj) => self.total.reading(raw_uj, at, 1e-6),
                Err(_) => Reading::Failed(ProviderError::Invalid(format!("{}: {:?} is not a count", self.path, text.trim()))),
            },
        }
    }
}

struct Rapl {
    counters: Vec<Counter>,
}

impl Provider for Rapl {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let mut response = ProviderResponse::new();
        for counter in &mut self.counters {
            response.insert(counter.source, counter.read());
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(RAPL_COOLDOWN)
    }
}

#[cfg(test)]
mod tests {
    use carbontracker::Sample;

    use super::*;

    fn source() -> SourceId {
        SourceId::new(ProviderId(1), 0)
    }

    /// A RAPL provider over one counter file, and a way to set its count.
    fn rapl(range: Option<u64>) -> (Rapl, impl Fn(&str), tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("energy_uj");
        let write = {
            let path = path.clone();
            move |count: &str| std::fs::write(&path, count).unwrap()
        };
        let counter = Counter::new(source(), path.to_string_lossy().into_owned(), range);
        (Rapl { counters: vec![counter] }, write, dir)
    }

    fn joules(rapl: &mut Rapl) -> Option<f64> {
        match rapl.read().unwrap().get(source()) {
            Some(Reading::Samples(samples)) => samples.first().map(Sample::value),
            _ => None,
        }
    }

    /// AC/ENG-53 AC/ENG-56
    #[test]
    fn each_read_reports_the_total_since_the_first_in_joules() {
        let (mut rapl, set, _dir) = rapl(Some(262_143_328_850));
        set("1000000\n");
        assert_eq!(joules(&mut rapl), Some(0.0));
        set("3500000\n");
        assert_eq!(joules(&mut rapl), Some(2.5));
    }

    /// AC/PROV-35 AC/ENG-56
    #[test]
    fn a_wrap_between_reads_is_counted() {
        let (mut rapl, set, _dir) = rapl(Some(10_000_000));
        set("9000000");
        joules(&mut rapl);
        set("1000000");
        assert_eq!(joules(&mut rapl), Some(2.0));
    }

    /// AC/ENG-50 AC/ENG-59 AC/ENG-62
    #[test]
    fn a_missing_or_unreadable_counter_says_why() {
        let (mut rapl, set, dir) = rapl(None);
        set("not a number");
        assert!(matches!(rapl.read().unwrap().get(source()), Some(Reading::Failed(ProviderError::Invalid(_)))));
        drop(dir);
        assert!(matches!(rapl.read().unwrap().get(source()), Some(Reading::Failed(ProviderError::Unavailable(_)))));
    }

    /// A powercap tree as Linux lays it out: every zone and subzone at the
    /// top, each with its name, counter and range.
    fn powercap(zones: &[(&str, &str)]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for (zone, name) in zones {
            let dir = root.path().join(zone);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("name"), format!("{name}\n")).unwrap();
            std::fs::write(dir.join("energy_uj"), "1000\n").unwrap();
            std::fs::write(dir.join("max_energy_range_uj"), "262143328850\n").unwrap();
        }
        root
    }

    fn served(outcome: &Outcome) -> Vec<(PowerDomain, SampleKind)> {
        outcome
            .descriptions
            .iter()
            .map(|(_, description)| match description {
                SourceDescription::Power { domain, sample_kind, .. } => (*domain, *sample_kind),
                SourceDescription::Intensity { .. } => panic!("a power provider"),
            })
            .collect()
    }

    /// AC/PROV-26 AC/PROV-34 AC/PROV-36
    #[test]
    fn it_serves_each_package_and_dram_zone_as_totals_and_leaves_out_psys() {
        let root = powercap(&[
            ("intel-rapl:0", "package-0"),
            ("intel-rapl:0:0", "core"),
            ("intel-rapl:0:1", "dram"),
            ("intel-rapl:1", "psys"),
        ]);
        let outcome = probe_rapl_at(ProviderId(1), root.path()).expect("readable zones serve");

        assert_eq!(served(&outcome), [(PowerDomain::Cpu, SampleKind::Total), (PowerDomain::Ram, SampleKind::Total)]);
        assert_eq!(outcome.provider.cooldown(), Cooldown::After(Duration::from_millis(10)));
    }

    /// AC/PROV-1
    #[test]
    fn two_packages_are_two_devices() {
        let root = powercap(&[("intel-rapl:0", "package-0"), ("intel-rapl:1", "package-1")]);
        let outcome = probe_rapl_at(ProviderId(1), root.path()).unwrap();
        let names: std::collections::HashSet<String> = outcome
            .descriptions
            .iter()
            .filter_map(|(_, description)| match description {
                SourceDescription::Power { device_name, .. } => Some(device_name.clone()),
                SourceDescription::Intensity { .. } => None,
            })
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    /// Makes `path` unreadable; false when this process reads it anyway, as root does.
    #[cfg(unix)]
    fn lock(path: &std::path::Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::read_to_string(path).is_err()
    }

    /// AC/PROV-34 AC/ENG-60
    #[cfg(unix)]
    #[test]
    fn without_root_the_probe_declines_and_a_later_refusal_is_denied() {
        let root = powercap(&[("intel-rapl:0", "package-0")]);
        let counter = root.path().join("intel-rapl:0").join("energy_uj");
        let mut provider = Rapl { counters: vec![Counter::new(source(), counter.to_string_lossy().into_owned(), None)] };
        assert!(matches!(provider.read().unwrap().get(source()), Some(Reading::Samples(_))));

        if !lock(&counter) {
            return; // Root reads every file.
        }
        assert!(probe_rapl_at(ProviderId(1), root.path()).is_none());
        assert!(matches!(provider.read().unwrap().get(source()), Some(Reading::Failed(ProviderError::Denied(_)))));
    }

    /// AC/PROV-35
    #[test]
    fn a_zone_with_no_range_fails_a_decrease_as_invalid() {
        let (mut rapl, set, _dir) = rapl(None);
        set("9000000");
        joules(&mut rapl);
        set("1000000");
        assert!(matches!(rapl.read().unwrap().get(source()), Some(Reading::Failed(ProviderError::Invalid(_)))));
    }
}
