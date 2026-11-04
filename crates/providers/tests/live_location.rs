//! Estimates this host's location against the live IP geolocation service.
//! Sends this host's IP to ipinfo.io, so it is ignored by default:
//!
//!     cargo test -p carbontracker-providers --test live_location -- --ignored
//!
//! Checks the answer's shape, not where the host is. When it fails on shape,
//! re-record `src/tests/fixtures/ipinfo.json`.

use carbontracker::Watts;
use carbontracker_providers::{Location, Pinned, ProbeConfig, ProviderRegistry, Verdict};

/// AC/TST-2
#[tokio::test]
#[ignore = "needs network; sends this host's IP to ipinfo.io"]
async fn the_ip_lookup_answers_a_country_and_coordinates() {
    let probe = ProbeConfig {
        pinned: Pinned { power: Some(Watts::new(10.0).unwrap()), ..Pinned::default() },
        ..ProbeConfig::default()
    };
    let resolution = ProviderRegistry::new(probe).unwrap().probe().await;

    let estimated = resolution.candidates().iter().find(|o| o.name == "ip-geolocation").map(|o| o.verdict.clone());
    assert!(matches!(estimated, Some(Verdict::Serving(_))), "{resolution}");
    let Some(Location::Estimated(estimate)) = resolution.location() else { panic!("an estimate: {resolution}") };
    assert!(estimate.coordinates.is_some(), "ipinfo answers `loc`: {estimate}");
}
