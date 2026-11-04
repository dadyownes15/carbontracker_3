//! NVML: NVIDIA GPUs, dlopened at runtime. A GPU with an energy counter is
//! read by [energy]; one without, by its power reading in [power]. The two
//! need different cooldowns, so they are two providers.

pub(crate) mod energy;
pub(crate) mod power;

use std::sync::Arc;

use nvml_wrapper::enums::device::DeviceArchitecture;

use crate::domain::counter::Accumulator;
use crate::ports::Outcome;
use carbontracker::{Fidelity, PowerDomain, PowerScope, ProviderId, SampleKind, SourceDescription, SourceId};

use energy::NvmlEnergy;
use power::NvmlPower;

/// What a provider asks of the GPUs, by index. The driver answers in
/// production; a fake answers in tests.
pub(crate) trait Board: Send + Sync {
    /// Energy since the driver loaded, in millijoules.
    fn energy_mj(&self, index: u32) -> Result<u64, String>;
    /// The power reading, in milliwatts.
    fn power_mw(&self, index: u32) -> Result<u32, String>;
}

impl Board for nvml_wrapper::Nvml {
    fn energy_mj(&self, index: u32) -> Result<u64, String> {
        self.device_by_index(index).and_then(|device| device.total_energy_consumption()).map_err(|error| error.to_string())
    }

    fn power_mw(&self, index: u32) -> Result<u32, String> {
        self.device_by_index(index).and_then(|device| device.power_usage()).map_err(|error| error.to_string())
    }
}

/// What probing NVML found: the GPUs with an energy counter, and the rest.
pub(crate) struct NvmlOutcomes {
    pub(crate) energy: Option<Outcome>,
    pub(crate) power: Option<Outcome>,
}

/// What one look at the driver finds of a GPU.
pub(crate) struct Gpu {
    pub(crate) index: u32,
    /// The driver's name with the index, so identical cards are two devices.
    pub(crate) device_name: String,
    pub(crate) has_power: bool,
    pub(crate) has_energy: bool,
    /// Whether its power reading averages the last second.
    pub(crate) averages: bool,
    /// The processes running on it; looked up only when the run measures given ones.
    pub(crate) running: Vec<u32>,
}

/// One source per GPU, or per GPU running one of `processes` if given.
/// `libnvidia-ml` is dlopened, so this finds nothing where there is no
/// driver.
pub(crate) fn probe_nvml(energy_id: ProviderId, power_id: ProviderId, processes: Option<&[u32]>) -> NvmlOutcomes {
    let none = || NvmlOutcomes { energy: None, power: None };
    let Ok(nvml) = nvml_wrapper::Nvml::init() else { return none() };
    let Ok(count) = nvml.device_count() else { return none() };

    let mut gpus = Vec::new();
    for index in 0..count {
        let Ok(device) = nvml.device_by_index(index) else { continue };
        // Compute and graphics processes both.
        let running = match processes {
            Some(_) => [device.running_compute_processes(), device.running_graphics_processes()]
                .into_iter()
                .flatten()
                .flatten()
                .map(|process| process.pid)
                .collect(),
            None => Vec::new(),
        };
        let name = device.name().unwrap_or_else(|_| "NVIDIA GPU".to_owned());
        gpus.push(Gpu {
            index,
            device_name: format!("{name} #{index}"),
            has_power: device.power_usage().is_ok(),
            has_energy: device.total_energy_consumption().is_ok(),
            averages: averages_over_a_second(device.architecture().ok(), &name),
            running,
        });
    }
    split(Arc::new(nvml), &gpus, energy_id, power_id, processes)
}

/// The GPUs with an energy counter for `energy_id`, the rest with a power
/// reading for `power_id`, read through `board`.
pub(crate) fn split(
    board: Arc<dyn Board>,
    gpus: &[Gpu],
    energy_id: ProviderId,
    power_id: ProviderId,
    processes: Option<&[u32]>,
) -> NvmlOutcomes {
    // Skip cards without a queryable power sensor (some consumer/virtualised boards).
    let served = gpus
        .iter()
        .filter(|gpu| gpu.has_power)
        .filter(|gpu| processes.is_none_or(|wanted| runs_any(&gpu.running, wanted)));
    let (energy, power): (Vec<&Gpu>, Vec<&Gpu>) = served.partition(|gpu| gpu.has_energy);

    let describe = |id: ProviderId, gpus: &[&Gpu], kind: SampleKind| -> Vec<(SourceId, SourceDescription)> {
        gpus.iter()
            .map(|gpu| {
                let description = SourceDescription::Power {
                    device_name: gpu.device_name.clone(),
                    domain: PowerDomain::Gpu,
                    scope: PowerScope::DeviceTotal,
                    sample_kind: kind,
                    fidelity: Fidelity::Measured,
                };
                (SourceId::new(id, gpu.index as u16), description)
            })
            .collect()
    };

    let energy_outcome = (!energy.is_empty()).then(|| {
        let counters = energy
            .iter()
            .map(|gpu| (gpu.index, SourceId::new(energy_id, gpu.index as u16), Accumulator::new(None)))
            .collect();
        Outcome {
            descriptions: describe(energy_id, &energy, SampleKind::Total),
            provider: Box::new(NvmlEnergy::new(Arc::clone(&board), counters)),
        }
    });
    let power_outcome = (!power.is_empty()).then(|| {
        let gpus = power.iter().map(|gpu| (gpu.index, SourceId::new(power_id, gpu.index as u16), gpu.averages)).collect();
        Outcome {
            descriptions: describe(power_id, &power, SampleKind::Instant),
            provider: Box::new(NvmlPower::new(Arc::clone(&board), gpus)),
        }
    });
    NvmlOutcomes { energy: energy_outcome, power: power_outcome }
}

/// Whether a GPU's power reading is the average over the last second: on
/// Ampere and newer, except the GA100 (A100, A30), which reads it
/// instantaneously (`nvml.h`, `nvmlDeviceGetPowerUsage`).
fn averages_over_a_second(architecture: Option<DeviceArchitecture>, name: &str) -> bool {
    let ga100 = name.contains("A100") || name.contains("A30");
    let newer = matches!(
        architecture,
        Some(
            DeviceArchitecture::Ampere
                | DeviceArchitecture::Ada
                | DeviceArchitecture::Hopper
                | DeviceArchitecture::Blackwell
                | DeviceArchitecture::Unknown
        ) | None
    );
    newer && !ga100
}

/// Whether any of `running` (the processes on a GPU) is one of `wanted`.
fn runs_any(running: &[u32], wanted: &[u32]) -> bool {
    running.iter().any(|pid| wanted.contains(pid))
}

#[cfg(test)]
pub(crate) mod fake {
    //! A board whose answers a test sets.

    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::Board;

    #[derive(Default)]
    pub(crate) struct FakeBoard {
        pub(crate) energy_mj: Mutex<HashMap<u32, Result<u64, String>>>,
        pub(crate) power_mw: Mutex<HashMap<u32, Result<u32, String>>>,
    }

    impl Board for FakeBoard {
        fn energy_mj(&self, index: u32) -> Result<u64, String> {
            self.energy_mj.lock().unwrap().get(&index).cloned().unwrap_or_else(|| Err("no such GPU".to_owned()))
        }

        fn power_mw(&self, index: u32) -> Result<u32, String> {
            self.power_mw.lock().unwrap().get(&index).cloned().unwrap_or_else(|| Err("no such GPU".to_owned()))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use carbontracker::Cooldown;

    use super::*;

    /// AC/PROV-37
    #[test]
    fn a_gpu_counts_if_one_of_its_processes_is_wanted() {
        assert!(runs_any(&[10, 42], &[42, 43]));
        assert!(!runs_any(&[10, 11], &[42]));
        assert!(!runs_any(&[], &[42]), "an idle GPU runs nothing of ours");
    }

    /// AC/PROV-39
    #[test]
    fn only_ampere_and_newer_but_the_ga100_average_over_a_second() {
        assert!(averages_over_a_second(Some(DeviceArchitecture::Ada), "NVIDIA GeForce RTX 4090"));
        assert!(!averages_over_a_second(Some(DeviceArchitecture::Ampere), "NVIDIA A100-SXM4-40GB"));
        assert!(!averages_over_a_second(Some(DeviceArchitecture::Volta), "Tesla V100"));
    }

    fn gpu(index: u32, has_energy: bool, has_power: bool, running: &[u32]) -> Gpu {
        Gpu {
            index,
            device_name: format!("Test GPU #{index}"),
            has_power,
            has_energy,
            averages: true,
            running: running.to_vec(),
        }
    }

    /// Each served GPU's index and sample kind.
    fn served(outcome: Option<&Outcome>) -> Vec<(u16, SampleKind)> {
        let Some(outcome) = outcome else { return Vec::new() };
        outcome
            .descriptions
            .iter()
            .map(|(source, description)| match description {
                SourceDescription::Power { sample_kind, domain: PowerDomain::Gpu, .. } => (source.slot(), *sample_kind),
                other => panic!("{other:?}"),
            })
            .collect()
    }

    const ENERGY: ProviderId = ProviderId(2);
    const POWER: ProviderId = ProviderId(15);

    /// AC/PROV-37 AC/PROV-27 AC/PROV-28 AC/PROV-38 AC/PROV-39 AC/ENG-40
    #[test]
    fn one_look_splits_the_gpus_by_what_they_report() {
        let gpus = [gpu(0, true, true, &[]), gpu(1, false, true, &[]), gpu(2, false, false, &[])];
        let found = split(Arc::new(fake::FakeBoard::default()), &gpus, ENERGY, POWER, None);

        assert_eq!(served(found.energy.as_ref()), [(0, SampleKind::Total)]);
        assert_eq!(served(found.power.as_ref()), [(1, SampleKind::Instant)], "GPU 2 has no power reading");
        assert_eq!(found.energy.unwrap().provider.cooldown(), Cooldown::After(Duration::from_secs(1)));
        assert_eq!(found.power.unwrap().provider.cooldown(), Cooldown::After(Duration::from_millis(100)));
    }

    /// AC/PROV-37
    #[test]
    fn measuring_given_processes_serves_only_the_gpus_running_one() {
        let gpus = [gpu(0, true, true, &[42]), gpu(1, true, true, &[7]), gpu(2, false, true, &[43, 9]), gpu(3, false, true, &[])];
        let found = split(Arc::new(fake::FakeBoard::default()), &gpus, ENERGY, POWER, Some(&[42, 43]));

        assert_eq!(served(found.energy.as_ref()), [(0, SampleKind::Total)]);
        assert_eq!(served(found.power.as_ref()), [(2, SampleKind::Instant)]);
    }
}
