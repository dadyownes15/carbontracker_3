//! Stated draws: the whole machine, or one device in one domain.

use carbontracker::{Fidelity, PowerDomain, PowerScope, ProviderId, SampleKind, SourceDescription, SourceId};

use crate::adapters::constant::StaticProvider;
use crate::ports::Outcome;

/// A fixed whole-machine draw, reported as `System`.
pub(crate) fn constant_power(id: ProviderId, watts: f64) -> Outcome {
    constant_device(id, "static", PowerDomain::System, watts, Fidelity::Stated)
}

/// A fixed draw for one device in one domain.
pub(crate) fn constant_device(
    id: ProviderId,
    name: &str,
    domain: PowerDomain,
    watts: f64,
    fidelity: Fidelity,
) -> Outcome {
    let sid = SourceId::new(id, 0);
    Outcome::single(
        sid,
        SourceDescription::Power {
            device_name: name.to_owned(),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity,
        },
        StaticProvider::new([(sid, watts)]),
    )
}
