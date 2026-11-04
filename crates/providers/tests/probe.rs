//! The default registry probed with power and intensity pinned, so nothing
//! depends on the host's hardware or the network.

use carbontracker::{
    start, Fidelity, GramsCo2ePerKwh, GridLocation, PowerDomain, PowerScope, SourceDescription, TrackerConfig, Watts,
};
use carbontracker_providers::{Pinned, ProbeConfig, ProbeProgress, ProviderRegistry, Verdict};

fn pinned() -> ProbeConfig {
    ProbeConfig {
        pinned: Pinned {
            power: Some(Watts::new(10.0).unwrap()),
            intensity: Some(GramsCo2ePerKwh::new(100.0).unwrap()),
            ..Pinned::default()
        },
        ..ProbeConfig::default()
    }
}

/// AC/PROV-3 AC/PROV-15 AC/PROV-46
#[tokio::test]
async fn the_resolution_reports_what_answered() {
    let resolution = ProviderRegistry::new(pinned()).unwrap().probe().await;

    let serving: Vec<&str> = resolution
        .candidates()
        .iter()
        .filter(|outcome| matches!(outcome.verdict, Verdict::Serving(_)))
        .map(|outcome| outcome.name)
        .collect();
    assert_eq!(serving, vec!["static", "static", "static"], "power, intensity and its forecast are all pinned");
    assert!(
        resolution.sources().all(|(_, source)| source.fidelity() == Fidelity::Stated),
        "pinned sources are the caller's own figures"
    );
}

/// AC/PROV-4 AC/PROV-11 AC/PROV-15
#[tokio::test]
async fn progress_reports_each_candidate_tried_and_decided() {
    let mut progress = Vec::new();
    ProviderRegistry::new(pinned()).unwrap().probe_with(|step| progress.push(step)).await;

    let tried = progress.iter().filter(|step| matches!(step, ProbeProgress::Probing { .. })).count();
    let decided = progress.iter().filter(|step| matches!(step, ProbeProgress::Decided { .. })).count();
    // ip-geolocation is decided `NotWanted` without being tried.
    assert_eq!((tried, decided), (3, 4), "{progress:?}");
}

/// AC/PROV-1
/// Regression: the `Global` fallback and a zone source both resolved, and
/// hash order picked between them (171 vs 475 gCO2/kWh across runs).
#[tokio::test]
async fn exactly_one_intensity_resolves() {
    let mut config = pinned();
    config.pinned.intensity = None;
    config.grid.location = Some(GridLocation::Zone { zone_id: "DK".to_owned() });
    let resolution = ProviderRegistry::new(config).unwrap().probe().await;

    let count = |wanted: fn(&SourceDescription) -> bool| resolution.sources().filter(|(_, s)| wanted(s)).count();
    assert_eq!(count(|s| matches!(s, SourceDescription::Intensity { .. })), 1, "one grid per run");
}

/// AC/PROV-12 AC/PROV-16 AC/PROV-17
#[tokio::test]
async fn the_resolution_starts_a_tracker_over_the_winners_alone() {
    let resolution = ProviderRegistry::new(pinned()).unwrap().probe().await;
    let (sources, forecaster) = resolution.resolve();
    let tracker = start(sources, TrackerConfig::default()).unwrap();

    let (reply, answer) = tokio::sync::oneshot::channel();
    tracker.query(carbontracker::TrackerQuery::Catalog { reply });
    let catalog = answer.await.unwrap().unwrap();
    assert_eq!(catalog.sources.len(), 2, "power and intensity");
    let intensity = *catalog.pairs.values().next().expect("an intensity resolves");
    assert!(matches!(&catalog.sources[&intensity], SourceDescription::Intensity { location, .. } if location == forecaster.location()));
}

/// AC/PROV-11
#[test]
fn an_invalid_zone_is_refused_before_probing() {
    let mut config = pinned();
    config.grid.location = Some(GridLocation::Zone { zone_id: String::new() });
    assert!(ProviderRegistry::new(config).is_err());
}

/// Only the intensity pinned, so every power candidate on this host is tried.
fn power_probed() -> ProbeConfig {
    ProbeConfig {
        pinned: Pinned { intensity: Some(GramsCo2ePerKwh::new(100.0).unwrap()), ..Pinned::default() },
        ..ProbeConfig::default()
    }
}

/// The power candidates decided, in order: every step before the location chain.
fn power_decided(progress: &[ProbeProgress]) -> Vec<&'static str> {
    progress
        .iter()
        .filter_map(|step| match step {
            ProbeProgress::Decided { candidate, .. } => Some(*candidate),
            ProbeProgress::Probing { .. } => None,
        })
        .take_while(|candidate| !matches!(*candidate, "stated-location" | "ip-geolocation"))
        .collect()
}

/// Each power source's domain, device and scope.
fn power_sources(resolution: &carbontracker_providers::Resolution) -> Vec<(PowerDomain, String, PowerScope, Fidelity)> {
    resolution
        .sources()
        .filter_map(|(_, source)| match source {
            SourceDescription::Power { domain, device_name, scope, fidelity, .. } => {
                Some((*domain, device_name.clone(), scope.clone(), *fidelity))
            }
            SourceDescription::Intensity { .. } => None,
        })
        .collect()
}

/// AC/PROV-5 AC/PROV-3
#[tokio::test]
async fn the_power_candidates_are_tried_best_first() {
    let mut progress = Vec::new();
    ProviderRegistry::new(power_probed()).unwrap().probe_with(|step| progress.push(step)).await;

    let expected: &[&str] = if cfg!(target_os = "linux") {
        &["rapl", "nvml-energy", "nvml-power", "generic-cpu"]
    } else if cfg!(target_os = "macos") {
        &["nvml-energy", "nvml-power", "ioreport", "macmon", "powermetrics", "generic-cpu"]
    } else {
        &["nvml-energy", "nvml-power", "generic-cpu"]
    };
    let decided = power_decided(&progress);
    assert_eq!(decided, expected, "{progress:?}");
    assert!(!decided.iter().any(|name| name.starts_with("static")), "no stated figure was asked for");
}

/// AC/PROV-9 AC/PROV-2
#[tokio::test]
async fn power_probing_always_measures_the_cpu_and_only_whole_devices() {
    let resolution = ProviderRegistry::new(power_probed()).unwrap().probe().await;
    let power = power_sources(&resolution);

    assert!(power.iter().any(|(domain, ..)| *domain == PowerDomain::Cpu), "generic-cpu at worst: {resolution}");
    assert!(power.iter().all(|(_, _, scope, _)| *scope == PowerScope::DeviceTotal), "{power:?}");
}

/// AC/PROV-4 AC/PROV-46
#[tokio::test]
async fn a_stated_gpu_or_cpu_draw_settles_that_part_and_leaves_the_rest_probed() {
    // The stated domain, its candidate, a candidate it settles, and one left to probe.
    for (domain, candidate, settled, probed) in [
        (PowerDomain::Gpu, "static-gpu", "nvml-energy", "generic-cpu"),
        (PowerDomain::Cpu, "static-cpu", "generic-cpu", "nvml-energy"),
    ] {
        let mut config = power_probed();
        let stated = Some(Watts::new(50.0).unwrap());
        match domain {
            PowerDomain::Gpu => config.pinned.gpu_power = stated,
            _ => config.pinned.cpu_power = stated,
        }
        let mut progress = Vec::new();
        let resolution = ProviderRegistry::new(config).unwrap().probe_with(|step| progress.push(step)).await;

        let of_domain: Vec<_> = power_sources(&resolution).into_iter().filter(|(d, ..)| *d == domain).collect();
        assert_eq!(of_domain.len(), 1, "{resolution}");
        assert_eq!((of_domain[0].1.as_str(), of_domain[0].3), ("static", Fidelity::Stated));
        assert_eq!(power_decided(&progress).first(), Some(&candidate));

        let verdict = |name: &str| resolution.candidates().iter().find(|outcome| outcome.name == name).map(|o| o.verdict.clone());
        assert_eq!(verdict(settled), Some(Verdict::Covered), "{resolution}");
        let tried = progress.iter().any(|step| matches!(step, ProbeProgress::Probing { candidate } if *candidate == probed));
        // generic-cpu is the last power candidate: a SoC probe may already serve the CPU.
        assert!(tried || verdict(probed) == Some(Verdict::Covered), "{probed} is left to the chain: {resolution}");
    }
}
