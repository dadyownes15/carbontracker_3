//! Building [Sources] through the public provider port, as a registry does.

use carbontracker::{
    Fidelity, GridLocation, PowerDomain, PowerScope, Provider, ProviderError, ProviderId,
    ProviderResponse, SampleKind, SourceDescription, SourceId, Sources, SourcesError,
};

/// A provider that is never read here.
struct Idle;

impl Provider for Idle {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        Ok(ProviderResponse::new())
    }
}

fn power() -> SourceDescription {
    SourceDescription::Power {
        device_name: "cpu".to_owned(),
        domain: PowerDomain::Cpu,
        scope: PowerScope::DeviceTotal,
        sample_kind: SampleKind::Instant,
        fidelity: Fidelity::Stated,
    }
}

fn grid() -> SourceDescription {
    SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Stated }
}

/// AC/ENG-36
#[test]
fn adding_a_provider_is_checked() {
    let (one, two) = (ProviderId(1), ProviderId(2));
    let mut sources = Sources::new();
    sources.add_provider(one, Idle, [(SourceId::new(one, 0), power())]).expect("a provider of its own source");

    assert_eq!(
        sources.add_provider(one, Idle, [(SourceId::new(one, 1), power())]),
        Err(SourcesError::DuplicateProvider(one))
    );
    assert_eq!(
        sources.add_provider(two, Idle, [(SourceId::new(one, 1), power())]),
        Err(SourcesError::ForeignSource { id: SourceId::new(one, 1), provider: two })
    );
    assert_eq!(
        sources.add_provider(two, Idle, [(SourceId::new(two, 0), power()), (SourceId::new(two, 0), power())]),
        Err(SourcesError::DuplicateSource(SourceId::new(two, 0)))
    );
    sources.add_provider(two, Idle, [(SourceId::new(two, 0), grid())]).expect("nothing of a refusal was added");
}

/// AC/ENG-40
#[test]
fn a_provider_serves_sources_of_one_kind() {
    let provider = ProviderId(1);
    let mixed = [(SourceId::new(provider, 0), power()), (SourceId::new(provider, 1), grid())];
    assert_eq!(Sources::new().add_provider(provider, Idle, mixed), Err(SourcesError::MixedKinds(provider)));
}

/// AC/ENG-1
#[test]
fn the_engine_depends_on_no_provider_set() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).expect("the manifest");
    assert!(!manifest.contains("carbontracker-"), "the engine never probes, so it needs no provider set or frontend");
}
