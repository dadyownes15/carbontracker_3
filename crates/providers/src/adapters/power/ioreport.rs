//! IOReport counters (macOS): Apple Silicon's "Energy Model" running totals,
//! read at each read. No sudo, no sampler thread. Only the snapshot is
//! macOS's: turning a snapshot into readings is tested on every host.

use std::collections::HashMap;
use std::time::Duration;

use chrono::Utc;

use crate::domain::counter::Accumulator;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, SourceId};
use carbontracker::{Fidelity, PowerDomain, PowerScope, SampleKind, SourceDescription};

/// A snapshot takes about 1.5 ms, and the totals moved at every snapshot
/// 3 ms apart (one M2 Pro, macOS 26.2).
pub(crate) const IOREPORT_COOLDOWN: Duration = Duration::from_millis(10);

/// The domains IOReport's energy model counts.
pub(crate) const IOREPORT_DOMAINS: [PowerDomain; 4] =
    [PowerDomain::Cpu, PowerDomain::Gpu, PowerDomain::Ane, PowerDomain::Ram];

/// One channel's running total in one snapshot.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Channel {
    pub name: String,
    pub unit: String,
    pub value: i64,
}

/// Takes snapshots of the energy model's running totals.
pub(crate) trait Snapshots: Send + 'static {
    fn take(&mut self) -> Result<Vec<Channel>, ProviderError>;
}

/// The domain a channel counts; `None` for the many it does not use.
/// "CPU Energy" on most chips, "DIE_{n}_CPU Energy" on an Ultra; "ANE",
/// "ANE0" or "ANE0_{n}"; "DRAM" and its numbered forms.
pub(crate) fn domain_of(channel: &str) -> Option<PowerDomain> {
    if channel.ends_with("CPU Energy") {
        Some(PowerDomain::Cpu)
    } else if channel == "GPU Energy" {
        Some(PowerDomain::Gpu)
    } else if channel.starts_with("ANE") {
        Some(PowerDomain::Ane)
    } else if channel.starts_with("DRAM") {
        Some(PowerDomain::Ram)
    } else {
        None
    }
}

/// Nanojoules per count of a unit; `None` for a unit that is not energy.
pub(crate) fn nanojoules_per_count(unit: &str) -> Option<u64> {
    match unit.trim() {
        "mJ" => Some(1_000_000),
        "uJ" => Some(1_000),
        "nJ" => Some(1),
        _ => None,
    }
}

/// Each domain's total in a snapshot, in nanojoules; a channel that is not
/// energy, or a negative count, is left out.
fn nanojoules(channels: &[Channel]) -> HashMap<PowerDomain, u64> {
    let mut totals = HashMap::new();
    for channel in channels {
        let (Some(domain), Some(scale), Ok(count)) =
            (domain_of(&channel.name), nanojoules_per_count(&channel.unit), u64::try_from(channel.value))
        else {
            continue;
        };
        *totals.entry(domain).or_insert(0u64) += count.saturating_mul(scale);
    }
    totals
}

/// Reads IOReport's running totals, one source per domain it counts.
pub(crate) struct IoReport<S> {
    snapshots: S,
    domains: Vec<(PowerDomain, SourceId)>,
    totals: HashMap<PowerDomain, Accumulator>,
}

impl<S: Snapshots> IoReport<S> {
    /// The provider over `snapshots`, with its sources: one per domain the
    /// first snapshot has a channel for. `None` when it has none.
    pub(crate) fn new(id: ProviderId, device_name: &str, mut snapshots: S) -> Option<(Self, Vec<(SourceId, SourceDescription)>)> {
        let found = nanojoules(&snapshots.take().ok()?);
        let mut domains = Vec::new();
        let mut descriptions = Vec::new();
        for (slot, domain) in IOREPORT_DOMAINS.into_iter().enumerate() {
            if !found.contains_key(&domain) {
                continue;
            }
            let source = SourceId::new(id, slot as u16);
            domains.push((domain, source));
            descriptions.push((
                source,
                SourceDescription::Power {
                    device_name: device_name.to_owned(),
                    domain,
                    scope: PowerScope::DeviceTotal,
                    sample_kind: SampleKind::Total,
                    fidelity: Fidelity::Measured,
                },
            ));
        }
        if domains.is_empty() {
            return None;
        }
        // The totals have no known wrap: a decrease is a restart.
        let totals = domains.iter().map(|&(domain, _)| (domain, Accumulator::new(None))).collect();
        Some((Self { snapshots, domains, totals }, descriptions))
    }
}

impl<S: Snapshots> Provider for IoReport<S> {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let snapshot = self.snapshots.take()?;
        let at = Utc::now();
        let found = nanojoules(&snapshot);
        let mut response = ProviderResponse::new();
        for &(domain, source) in &self.domains {
            let reading = match (found.get(&domain), self.totals.get_mut(&domain)) {
                (Some(&count), Some(total)) => total.reading(count, at, 1e-9),
                _ => carbontracker::Reading::Failed(ProviderError::Invalid(format!("no {domain:?} channel in this snapshot"))),
            };
            response.insert(source, reading);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(IOREPORT_COOLDOWN)
    }
}

/// Finds the energy model and serves its counters; `None` on a Mac without
/// one (Intel) or when IOReport refuses.
#[cfg(target_os = "macos")]
pub(crate) fn probe_ioreport(id: ProviderId) -> Option<crate::ports::Outcome> {
    let snapshots = macos::Subscription::energy_model()?;
    let (provider, descriptions) = IoReport::new(id, &crate::adapters::system::cpu::device_name(), snapshots)?;
    Some(crate::ports::Outcome { descriptions, provider: Box::new(provider) })
}

#[cfg(target_os = "macos")]
mod macos {
    //! The few IOReport calls a snapshot needs: subscribe to the energy
    //! model's channels this provider uses, and copy their totals.

    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use core_foundation::base::TCFType;
    use core_foundation::string::CFString;
    use core_foundation_sys::array::{
        kCFTypeArrayCallBacks, CFArrayAppendValue, CFArrayCreateMutable, CFArrayGetCount, CFArrayGetValueAtIndex,
        CFArrayRef,
    };
    use core_foundation_sys::base::{kCFAllocatorDefault, CFRelease, CFTypeRef};
    use core_foundation_sys::dictionary::{
        CFDictionaryCreateMutableCopy, CFDictionaryGetCount, CFDictionaryGetValue, CFDictionaryRef,
        CFDictionarySetValue, CFMutableDictionaryRef,
    };
    use core_foundation_sys::string::CFStringRef;

    use super::{domain_of, Channel, Snapshots};
    use carbontracker::ProviderError;

    #[link(name = "IOReport", kind = "dylib")]
    unsafe extern "C" {
        fn IOReportCopyChannelsInGroup(group: CFStringRef, subgroup: CFStringRef, a: u64, b: u64, c: u64) -> CFDictionaryRef;
        fn IOReportCreateSubscription(
            a: *const c_void,
            channels: CFMutableDictionaryRef,
            subscribed: *mut CFMutableDictionaryRef,
            d: u64,
            e: CFTypeRef,
        ) -> CFTypeRef;
        fn IOReportCreateSamples(subscription: CFTypeRef, channels: CFMutableDictionaryRef, c: CFTypeRef) -> CFDictionaryRef;
        fn IOReportChannelGetChannelName(item: CFDictionaryRef) -> CFStringRef;
        fn IOReportChannelGetUnitLabel(item: CFDictionaryRef) -> CFStringRef;
        fn IOReportSimpleGetIntegerValue(item: CFDictionaryRef, index: i32) -> i64;
    }

    fn text(string: CFStringRef) -> String {
        if string.is_null() {
            return String::new();
        }
        // SAFETY: IOReport hands out a valid CFString it keeps alive.
        unsafe { CFString::wrap_under_get_rule(string) }.to_string()
    }

    /// Every channel in `sample`'s channel array.
    ///
    /// SAFETY: `sample` is a dictionary IOReport created.
    unsafe fn items(sample: CFDictionaryRef) -> Vec<CFDictionaryRef> {
        let key = CFString::from_static_string("IOReportChannels");
        let array = unsafe { CFDictionaryGetValue(sample, key.as_concrete_TypeRef() as *const c_void) } as CFArrayRef;
        if array.is_null() {
            return Vec::new();
        }
        let count = unsafe { CFArrayGetCount(array) };
        (0..count).map(|index| unsafe { CFArrayGetValueAtIndex(array, index) } as CFDictionaryRef).collect()
    }

    /// A subscription to the energy model's channels a domain uses.
    pub(crate) struct Subscription {
        subscription: CFTypeRef,
        channels: CFMutableDictionaryRef,
    }

    // SAFETY: the engine reads a provider on one thread at a time, and
    // nothing else holds these handles.
    unsafe impl Send for Subscription {}

    impl Subscription {
        pub(crate) fn energy_model() -> Option<Self> {
            let group = CFString::from_static_string("Energy Model");
            // SAFETY: plain IOReport and CoreFoundation calls; every object
            // created here is released here or owned by the subscription.
            unsafe {
                let all = IOReportCopyChannelsInGroup(group.as_concrete_TypeRef(), null(), 0, 0, 0);
                if all.is_null() {
                    return None;
                }
                let used = CFArrayCreateMutable(kCFAllocatorDefault, 0, &kCFTypeArrayCallBacks);
                for item in items(all) {
                    if domain_of(&text(IOReportChannelGetChannelName(item))).is_some() {
                        CFArrayAppendValue(used, item as *const c_void);
                    }
                }
                let channels = CFDictionaryCreateMutableCopy(kCFAllocatorDefault, CFDictionaryGetCount(all), all);
                let key = CFString::from_static_string("IOReportChannels");
                CFDictionarySetValue(channels, key.as_concrete_TypeRef() as *const c_void, used as *const c_void);
                CFRelease(used as CFTypeRef);
                CFRelease(all as CFTypeRef);

                let mut subscribed: CFMutableDictionaryRef = null_mut();
                let subscription = IOReportCreateSubscription(null(), channels, &mut subscribed, 0, null());
                if !subscribed.is_null() {
                    CFRelease(subscribed as CFTypeRef);
                }
                if subscription.is_null() {
                    CFRelease(channels as CFTypeRef);
                    return None;
                }
                Some(Self { subscription, channels })
            }
        }
    }

    impl Snapshots for Subscription {
        fn take(&mut self) -> Result<Vec<Channel>, ProviderError> {
            // SAFETY: the subscription and its channels live as long as self.
            unsafe {
                let sample = IOReportCreateSamples(self.subscription, self.channels, null());
                if sample.is_null() {
                    return Err(ProviderError::Unavailable("IOReport returned no sample".to_owned()));
                }
                let channels = items(sample)
                    .into_iter()
                    .map(|item| Channel {
                        name: text(IOReportChannelGetChannelName(item)),
                        unit: text(IOReportChannelGetUnitLabel(item)),
                        value: IOReportSimpleGetIntegerValue(item, 0),
                    })
                    .collect();
                CFRelease(sample as CFTypeRef);
                Ok(channels)
            }
        }
    }

    impl Drop for Subscription {
        fn drop(&mut self) {
            // SAFETY: both were created by this subscription and are released once.
            unsafe {
                CFRelease(self.subscription);
                CFRelease(self.channels as CFTypeRef);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use carbontracker::Reading;

    use super::*;

    const ID: ProviderId = ProviderId(16);

    /// Snapshots handed out in order; the last repeats.
    struct Scripted(Vec<Result<Vec<Channel>, ProviderError>>);

    impl Snapshots for Scripted {
        fn take(&mut self) -> Result<Vec<Channel>, ProviderError> {
            if self.0.len() > 1 { self.0.remove(0) } else { self.0[0].clone() }
        }
    }

    fn channel(name: &str, unit: &str, value: i64) -> Channel {
        Channel { name: name.to_owned(), unit: unit.to_owned(), value }
    }

    /// An M2: one CPU, a GPU, an ANE, DRAM, and channels it does not use.
    fn m2(cpu_mj: i64, gpu_mj: i64) -> Vec<Channel> {
        vec![
            channel("CPU Energy", "mJ", cpu_mj),
            channel("GPU Energy", "mJ", gpu_mj),
            channel("ANE", "mJ", 7),
            channel("DRAM", "mJ", 900),
            channel("EACC_CPU0", "mJ", 1),
            channel("GPU SRAM0", "mJ", 3),
        ]
    }

    fn joules(response: &ProviderResponse, source: SourceId) -> Vec<f64> {
        match response.get(source) {
            Some(Reading::Samples(samples)) => samples.iter().map(|sample| sample.value()).collect(),
            other => panic!("expected samples, got {other:?}"),
        }
    }

    fn source(sources: &[(SourceId, SourceDescription)], domain: PowerDomain) -> SourceId {
        sources
            .iter()
            .find(|(_, description)| matches!(description, SourceDescription::Power { domain: d, .. } if *d == domain))
            .map(|(id, _)| *id)
            .expect("served")
    }

    /// AC/PROV-51
    #[test]
    fn each_used_channel_counts_toward_its_domain() {
        assert_eq!(domain_of("CPU Energy"), Some(PowerDomain::Cpu));
        assert_eq!(domain_of("DIE_1_CPU Energy"), Some(PowerDomain::Cpu));
        assert_eq!(domain_of("GPU Energy"), Some(PowerDomain::Gpu));
        assert_eq!(domain_of("ANE0_1"), Some(PowerDomain::Ane));
        assert_eq!(domain_of("DRAM0"), Some(PowerDomain::Ram));
        assert_eq!(domain_of("GPU SRAM0"), None, "inside the GPU's own count");
        assert_eq!(domain_of("EACC_CPU0"), None, "one core cluster, inside CPU Energy");
    }

    #[test]
    fn units_are_energy_in_nanojoules() {
        assert_eq!(nanojoules_per_count("mJ"), Some(1_000_000));
        assert_eq!(nanojoules_per_count("uJ"), Some(1_000));
        assert_eq!(nanojoules_per_count("nJ"), Some(1));
        assert_eq!(nanojoules_per_count("ms"), None);
    }

    /// AC/PROV-49
    #[test]
    fn it_serves_a_running_total_for_each_domain_it_finds() {
        let (_, sources) = IoReport::new(ID, "Apple M2", Scripted(vec![Ok(m2(100, 50))])).expect("found");
        let served: Vec<(PowerDomain, SampleKind, Fidelity)> = sources
            .iter()
            .map(|(_, description)| match description {
                SourceDescription::Power { domain, sample_kind, fidelity, .. } => (*domain, *sample_kind, *fidelity),
                SourceDescription::Intensity { .. } => panic!("a power provider"),
            })
            .collect();
        let counted = |domain| (domain, SampleKind::Total, Fidelity::Measured);
        assert_eq!(served, IOREPORT_DOMAINS.map(counted));
    }

    /// AC/PROV-51
    #[test]
    fn a_chip_without_an_energy_model_is_not_served() {
        let none = Scripted(vec![Ok(vec![channel("EACC_CPU0", "mJ", 1)])]);
        assert!(IoReport::new(ID, "Apple M2", none).is_none());
        assert!(IoReport::new(ID, "Apple M2", Scripted(vec![Err(ProviderError::Unavailable("no IOReport".into()))])).is_none());
    }

    /// AC/PROV-50
    #[test]
    fn a_read_is_the_energy_since_the_first_in_joules() {
        let snapshots = Scripted(vec![Ok(m2(1_000, 500)), Ok(m2(1_000, 500)), Ok(m2(3_500, 600))]);
        let (mut ioreport, sources) = IoReport::new(ID, "Apple M2", snapshots).expect("found");
        let (cpu, gpu) = (source(&sources, PowerDomain::Cpu), source(&sources, PowerDomain::Gpu));

        let first = ioreport.read().unwrap();
        assert_eq!(joules(&first, cpu), [0.0]);
        let second = ioreport.read().unwrap();
        assert_eq!(joules(&second, cpu), [2.5], "2500 mJ since the first read");
        assert_eq!(joules(&second, gpu), [0.1]);
    }

    /// AC/PROV-51
    #[test]
    fn a_die_split_cpu_is_one_total() {
        let ultra = |a, b| vec![channel("DIE_0_CPU Energy", "mJ", a), channel("DIE_1_CPU Energy", "mJ", b)];
        let snapshots = Scripted(vec![Ok(ultra(0, 0)), Ok(ultra(0, 0)), Ok(ultra(1_000, 2_000))]);
        let (mut ioreport, sources) = IoReport::new(ID, "Apple M2 Ultra", snapshots).expect("found");
        let cpu = source(&sources, PowerDomain::Cpu);
        ioreport.read().unwrap();
        assert_eq!(joules(&ioreport.read().unwrap(), cpu), [3.0]);
    }

    /// AC/PROV-33
    #[test]
    fn a_total_that_went_down_fails_that_read_and_carries_on() {
        let snapshots = Scripted(vec![Ok(m2(5_000, 0)), Ok(m2(5_000, 0)), Ok(m2(1_000, 0)), Ok(m2(2_000, 0))]);
        let (mut ioreport, sources) = IoReport::new(ID, "Apple M2", snapshots).expect("found");
        let cpu = source(&sources, PowerDomain::Cpu);
        ioreport.read().unwrap();
        assert!(matches!(ioreport.read().unwrap().get(cpu), Some(Reading::Failed(ProviderError::Invalid(_)))));
        assert_eq!(joules(&ioreport.read().unwrap(), cpu), [1.0], "the total carries on from the new count");
    }

    #[test]
    fn a_failed_snapshot_fails_the_read() {
        let snapshots = Scripted(vec![Ok(m2(0, 0)), Err(ProviderError::Unavailable("gone".into()))]);
        let (mut ioreport, _) = IoReport::new(ID, "Apple M2", snapshots).expect("found");
        assert!(matches!(ioreport.read(), Err(ProviderError::Unavailable(_))));
    }

    /// AC/PROV-49 AC/PROV-52
    #[test]
    fn its_cooldown_is_10_ms() {
        let (ioreport, _) = IoReport::new(ID, "Apple M2", Scripted(vec![Ok(m2(0, 0))])).expect("found");
        assert_eq!(ioreport.cooldown(), Cooldown::After(Duration::from_millis(10)));
    }
}
