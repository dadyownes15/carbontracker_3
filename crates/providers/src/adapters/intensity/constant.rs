//! Fixed intensities: stated, or the world average.

use carbontracker::{Fidelity, GridLocation, ProviderId, SourceDescription, SourceId};

use crate::adapters::constant::StaticProvider;
use crate::ports::Outcome;

pub(crate) fn constant_intensity(
    id: ProviderId,
    g_per_kwh: f64,
    location: GridLocation,
    fidelity: Fidelity,
) -> Outcome {
    let sid = SourceId::new(id, 0);
    Outcome::single(
        sid,
        SourceDescription::Intensity { location, fidelity },
        StaticProvider::new([(sid, g_per_kwh)]),
    )
}

pub(crate) const WORLD_AVERAGE_G_CO2_PER_KWH: f64 = 475.0;
