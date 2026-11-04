//! Country average: one intensity per country from a table built into the
//! crate, so a location is worth something without an API key.

use std::collections::HashMap;
use std::sync::LazyLock;

use carbontracker::{Fidelity, GridLocation, ProviderId};

use crate::adapters::intensity::constant::constant_intensity;
use crate::domain::location::{CountryCode, Location};
use crate::ports::Outcome;

/// `carbon-intensities.csv`: Our World in Data, carbon
/// intensity of electricity (lifecycle, gCO2eq/kWh), CC BY. Each row keeps
/// its year (mostly 2022–2024).
const TABLE: &str = include_str!("../../../data/carbon-intensities.csv");

static INTENSITIES: LazyLock<HashMap<CountryCode, f64>> = LazyLock::new(|| {
    TABLE
        .lines()
        .skip(1)
        .filter_map(|row| {
            // alpha-2, Year, Entity, Code, intensity
            let fields: Vec<&str> = row.split(',').collect();
            let [code, _, _, _, intensity] = fields[..] else { return None };
            Some((CountryCode::new(code)?, intensity.trim().parse().ok()?))
        })
        .collect()
});

/// The table's figure for `country`, if it has one.
pub(crate) fn country_intensity(country: CountryCode) -> Option<f64> {
    INTENSITIES.get(&country).copied()
}

/// Serves any location that names a country in the table. Coordinates, data
/// centres and no location decline: there is no way to place them in a
/// country.
pub(crate) fn probe_country_average(id: ProviderId, location: Option<&Location>) -> Option<(Outcome, f64)> {
    let country = location?.country()?;
    let g_per_kwh = country_intensity(country)?;
    let described = GridLocation::Country { country_code: country.to_string() };
    Some((constant_intensity(id, g_per_kwh, described, Fidelity::Modelled), g_per_kwh))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::location::Estimate;
    use carbontracker::SourceDescription;

    const ID: ProviderId = ProviderId(90);

    fn served(location: Location) -> Option<SourceDescription> {
        let (outcome, _) = probe_country_average(ID, Some(&location))?;
        outcome.descriptions.into_iter().next().map(|(_, description)| description)
    }

    #[test]
    fn the_table_parses_every_row() {
        assert_eq!(INTENSITIES.len(), TABLE.lines().count() - 1);
    }

    /// AC/PROV-10 AC/PROV-46
    #[test]
    fn a_country_zone_or_estimate_serves_its_country_modelled() {
        let dk = SourceDescription::Intensity {
            location: GridLocation::Country { country_code: "DK".into() },
            fidelity: Fidelity::Modelled,
        };
        let estimate = Estimate { country: CountryCode::new("DK").unwrap(), coordinates: None, by: "test" };

        for location in [
            Location::Stated(GridLocation::Country { country_code: "dk".into() }),
            Location::Stated(GridLocation::Zone { zone_id: "DK-DK2".into() }),
            Location::Estimated(estimate),
        ] {
            assert_eq!(served(location.clone()), Some(dk.clone()), "{location}");
        }
    }

    /// AC/PROV-10
    #[test]
    fn what_names_no_country_in_the_table_declines() {
        for location in [
            Location::Stated(GridLocation::Coordinates { latitude: 55.0, longitude: 12.0 }),
            Location::Stated(GridLocation::DataCenter { cloud_provider: "aws".into(), region: "eu-north-1".into() }),
            Location::Stated(GridLocation::Global),
            Location::Stated(GridLocation::Country { country_code: "ZZ".into() }),
        ] {
            assert_eq!(served(location.clone()), None, "{location}");
        }
        assert!(probe_country_average(ID, None).is_none());
    }
}
