//! The chain's coverage rule, on candidates faked without probing.

use carbontracker::{Fidelity, PowerDomain, PowerScope, ProviderId, SampleKind, SourceDescription, SourceId};

use crate::adapters::constant::StaticProvider;
use crate::application::chain::Chain;
use crate::domain::config::Measure;
use crate::domain::verdict::Verdict;
use crate::ports::Outcome;

const A: ProviderId = ProviderId(90);
const B: ProviderId = ProviderId(91);

/// A candidate offering `domains` on one device, at measured fidelity.
fn offering(id: ProviderId, device: &str, domains: &[PowerDomain]) -> Option<Outcome> {
    scoped(id, device, domains, PowerScope::DeviceTotal)
}

/// [offering], measuring `scope` of the device.
fn scoped(id: ProviderId, device: &str, domains: &[PowerDomain], scope: PowerScope) -> Option<Outcome> {
    let descriptions = domains
        .iter()
        .enumerate()
        .map(|(slot, &domain)| {
            (
                SourceId::new(id, slot as u16),
                SourceDescription::Power {
                    device_name: device.to_owned(),
                    domain,
                    scope: scope.clone(),
                    sample_kind: SampleKind::Instant,
                    fidelity: Fidelity::Measured,
                },
            )
        })
        .collect();

    let provider = StaticProvider::new((0..domains.len()).map(|slot| (SourceId::new(id, slot as u16), 1.0)));
    Some(Outcome { descriptions, provider: Box::new(provider) })
}

fn verdict(chain: &Chain<'_>, name: &str) -> Verdict {
    chain
        .result
        .candidates
        .iter()
        .find(|outcome| outcome.name == name)
        .map(|outcome| outcome.verdict.clone())
        .expect("every candidate is recorded")
}

fn domains_taken(chain: &Chain<'_>) -> Vec<PowerDomain> {
    let mut taken: Vec<PowerDomain> = chain
        .result
        .descriptions
        .values()
        .filter_map(|description| match description {
            SourceDescription::Power { domain, .. } => Some(*domain),
            _ => None,
        })
        .collect();
    taken.sort_by_key(|domain| format!("{domain:?}"));
    taken
}

/// AC/PROV-1 AC/PROV-14
#[test]
fn one_slot_takes_one_source() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);

    chain.take(A, "first", offering(A, "Test CPU", &[PowerDomain::Cpu]));
    chain.take(B, "second", offering(B, "Test CPU", &[PowerDomain::Cpu]));

    assert_eq!(domains_taken(&chain), vec![PowerDomain::Cpu]);
    assert_eq!(verdict(&chain, "second"), Verdict::Covered);
    assert!(!chain.result.providers.contains_key(&B));
}

/// AC/PROV-1
#[test]
fn the_same_domain_on_two_devices_is_two_slots() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);

    chain.take(A, "gpu-0", offering(A, "Card #0", &[PowerDomain::Gpu]));
    chain.take(B, "gpu-1", offering(B, "Card #1", &[PowerDomain::Gpu]));

    assert_eq!(domains_taken(&chain), vec![PowerDomain::Gpu; 2]);
    assert!(chain.result.providers.contains_key(&B));
}

/// AC/PROV-48
#[test]
fn a_candidate_whose_domains_are_all_taken_is_not_probed() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);
    chain.take(A, "first", offering(A, "Test CPU", &[PowerDomain::Cpu]));

    assert!(!chain.worth_probing("second", &[PowerDomain::Cpu]));
    assert_eq!(verdict(&chain, "second"), Verdict::Covered);
}

/// AC/PROV-14 AC/PROV-48 AC/ENG-40
#[test]
fn a_partially_covered_candidate_is_probed_and_keeps_what_is_new() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);
    chain.take(A, "rapl", offering(A, "Test CPU", &[PowerDomain::Cpu]));

    assert!(chain.worth_probing("soc", &[PowerDomain::Cpu, PowerDomain::Gpu]));
    chain.take(B, "soc", offering(B, "Test CPU", &[PowerDomain::Cpu, PowerDomain::Gpu]));

    assert_eq!(domains_taken(&chain), vec![PowerDomain::Cpu, PowerDomain::Gpu]);
    assert!(matches!(verdict(&chain, "soc"), Verdict::Serving(labels) if labels.len() == 1));

    // Every read answers the GPU it kept, and nothing of the CPU it lost.
    let mut soc = chain.result.providers.remove(&B).expect("the soc serves its GPU");
    let answered: Vec<SourceId> = soc.read().unwrap().into_readings().map(|(source, _)| source).collect();
    assert_eq!(answered, [SourceId::new(B, 1)]);
}

/// AC/PROV-4
#[test]
fn a_domain_nobody_asked_for_is_never_probed() {
    let wanted = Measure::Only(vec![PowerDomain::Cpu]);
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);

    assert!(!chain.worth_probing("nvml", &[PowerDomain::Gpu]));
    assert_eq!(verdict(&chain, "nvml"), Verdict::NotWanted);
}

/// AC/PROV-4
#[test]
fn an_unwanted_domain_is_dropped_from_what_a_candidate_serves() {
    let wanted = Measure::Only(vec![PowerDomain::Cpu]);
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);

    chain.take(A, "soc", offering(A, "Test CPU", &[PowerDomain::Cpu, PowerDomain::Gpu]));

    assert_eq!(domains_taken(&chain), vec![PowerDomain::Cpu]);
}

/// AC/PROV-9
#[test]
fn a_chain_that_took_nothing_still_resolves() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let chain = Chain::new(&wanted, &mut quiet);

    assert_eq!(chain.result.sources().count(), 0);
    let _ = chain.result.resolve();
}

/// AC/PROV-4
#[test]
fn a_static_device_replaces_its_domain_and_leaves_the_rest() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);

    chain.take(A, "static-cpu", offering(A, "static", &[PowerDomain::Cpu]));
    chain.pinned.push(PowerDomain::Cpu);
    // A SoC probe names its CPU differently, so only the pinned domain rule
    // keeps it from serving a second CPU.
    chain.take(B, "soc", offering(B, "Apple M2", &[PowerDomain::Cpu, PowerDomain::Gpu]));

    assert_eq!(domains_taken(&chain), vec![PowerDomain::Cpu, PowerDomain::Gpu]);
    let cpus: Vec<&str> = chain
        .result
        .descriptions
        .values()
        .filter_map(|description| match description {
            SourceDescription::Power { device_name, domain: PowerDomain::Cpu, .. } => Some(device_name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(cpus, ["static"]);
}

#[test]
fn the_resolution_names_who_serves_each_source() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);
    chain.take(A, "nvml", offering(A, "Card #0", &[PowerDomain::Gpu]));

    assert_eq!(chain.result.served_by(SourceId::new(A, 0)), Some("nvml"));
    assert_eq!(chain.result.served_by(SourceId::new(B, 0)), None);
}

/// AC/PROV-2
#[test]
fn one_device_at_two_scopes_is_two_questions() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);

    chain.take(A, "whole-card", offering(A, "Card #0", &[PowerDomain::Gpu]));
    chain.take(B, "job", scoped(B, "Card #0", &[PowerDomain::Gpu], PowerScope::Job { job_id: "7".to_owned() }));

    assert_eq!(domains_taken(&chain), vec![PowerDomain::Gpu; 2], "both kept, so both billed");
    assert!(chain.result.providers.contains_key(&A) && chain.result.providers.contains_key(&B));
}

/// A counter whose reads are a second apart.
struct SlowCounter;

impl carbontracker::Provider for SlowCounter {
    fn read(&mut self) -> Result<carbontracker::ProviderResponse, carbontracker::ProviderError> {
        Ok(carbontracker::ProviderResponse::new())
    }

    fn cooldown(&self) -> carbontracker::Cooldown {
        carbontracker::Cooldown::After(std::time::Duration::from_secs(1))
    }
}

/// AC/PRF-3
#[test]
fn the_resolution_gives_each_source_its_providers_cooldown() {
    let wanted = Measure::All;
    let mut quiet = |_| {};
    let mut chain = Chain::new(&wanted, &mut quiet);
    let mut counter = offering(A, "Card #0", &[PowerDomain::Gpu]).expect("offered");
    counter.provider = Box::new(SlowCounter);
    chain.take(A, "nvml", Some(counter));
    chain.take(B, "static-cpu", offering(B, "static", &[PowerDomain::Cpu]));

    let second = carbontracker::Cooldown::After(std::time::Duration::from_secs(1));
    assert_eq!(chain.result.cooldown(SourceId::new(A, 0)), Some(second));
    assert_eq!(chain.result.cooldown(SourceId::new(B, 0)), Some(carbontracker::Cooldown::Never));
    assert_eq!(chain.result.cooldown(SourceId::new(B, 1)), None);
}
