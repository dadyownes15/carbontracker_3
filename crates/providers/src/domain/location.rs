//! Where the host is, as probing knows it: stated by the user or estimated.
//! Each intensity and forecast provider reads it in its own terms.

use std::fmt;

use carbontracker::GridLocation;

/// What probing knows about where the host is. No location at all (nothing
/// stated, no estimator answered) is `None` wherever one is passed.
#[derive(Debug, Clone, PartialEq)]
pub enum Location {
    /// From the config. Never estimated over.
    Stated(GridLocation),
    /// Found by an estimator.
    Estimated(Estimate),
}

impl Location {
    /// The country this location is in, if it names one: a stated country,
    /// a stated zone's country (`DK-DK2` → `DK`), or an estimate's.
    pub fn country(&self) -> Option<CountryCode> {
        match self {
            Self::Stated(GridLocation::Country { country_code }) => CountryCode::new(country_code),
            Self::Stated(GridLocation::Zone { zone_id }) => {
                // Every Electricity Maps zone key starts with its country code.
                let prefix = zone_id.trim().split('-').next()?;
                CountryCode::new(prefix)
            }
            Self::Stated(_) => None,
            Self::Estimated(estimate) => Some(estimate.country),
        }
    }
}

impl fmt::Display for Location {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stated(location) => match location {
                GridLocation::Zone { zone_id } => write!(formatter, "zone {zone_id}"),
                GridLocation::Country { country_code } => write!(formatter, "country {country_code}"),
                GridLocation::Coordinates { latitude, longitude } => {
                    write!(formatter, "({latitude:.2}, {longitude:.2})")
                }
                GridLocation::DataCenter { cloud_provider, region } => {
                    write!(formatter, "data centre {cloud_provider} {region}")
                }
                GridLocation::Global => formatter.write_str("global"),
            },
            Self::Estimated(estimate) => estimate.fmt(formatter),
        }
    }
}

/// What an estimator found. Keeps everything, so a provider that can use
/// coordinates uses them and one that only knows countries uses the country.
#[derive(Debug, Clone, PartialEq)]
pub struct Estimate {
    pub country: CountryCode,
    pub coordinates: Option<Coordinates>,
    /// The estimator's candidate name.
    pub by: &'static str,
}

impl fmt::Display for Estimate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.coordinates {
            Some(at) => write!(formatter, "{} {at}", self.country),
            None => self.country.fmt(formatter),
        }
    }
}

/// An ISO 3166-1 alpha-2 code, uppercased: `"dk"` is `DK`, as Electricity
/// Maps zone keys and the country table expect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CountryCode([u8; 2]);

impl CountryCode {
    /// `None` unless `code` is exactly two ASCII letters, ignoring
    /// surrounding whitespace (the rule `GridLocation::validate` applies).
    pub fn new(code: &str) -> Option<Self> {
        match code.trim().as_bytes() {
            &[first, second] if first.is_ascii_alphabetic() && second.is_ascii_alphabetic() => {
                Some(Self([first.to_ascii_uppercase(), second.to_ascii_uppercase()]))
            }
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        // Two ASCII letters by construction.
        std::str::from_utf8(&self.0).expect("ASCII")
    }
}

impl fmt::Display for CountryCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A point on the globe: finite, latitude within ±90, longitude within ±180.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coordinates {
    latitude: f64,
    longitude: f64,
}

impl Coordinates {
    /// `None` outside the ranges `GridLocation::validate` accepts.
    pub fn new(latitude: f64, longitude: f64) -> Option<Self> {
        let valid = latitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && longitude.is_finite()
            && (-180.0..=180.0).contains(&longitude);
        valid.then_some(Self { latitude, longitude })
    }

    pub fn latitude(&self) -> f64 {
        self.latitude
    }

    pub fn longitude(&self) -> f64 {
        self.longitude
    }
}

impl fmt::Display for Coordinates {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "({:.2}, {:.2})", self.latitude, self.longitude)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stated(location: GridLocation) -> Location {
        Location::Stated(location)
    }

    #[test]
    fn a_country_code_is_two_letters_uppercased() {
        assert_eq!(CountryCode::new(" dk ").map(|code| code.to_string()), Some("DK".to_owned()));
        assert_eq!(CountryCode::new("DNK"), None);
        assert_eq!(CountryCode::new("D1"), None);
        assert_eq!(CountryCode::new(""), None);
    }

    #[test]
    fn coordinates_out_of_range_are_refused() {
        assert!(Coordinates::new(55.68, 12.57).is_some());
        assert!(Coordinates::new(91.0, 0.0).is_none());
        assert!(Coordinates::new(0.0, -181.0).is_none());
        assert!(Coordinates::new(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn a_location_names_its_country_where_it_has_one() {
        let dk = CountryCode::new("DK");
        assert_eq!(stated(GridLocation::Country { country_code: "dk".into() }).country(), dk);
        assert_eq!(stated(GridLocation::Zone { zone_id: "DK-DK2".into() }).country(), dk);
        assert_eq!(stated(GridLocation::Zone { zone_id: "DK".into() }).country(), dk);
        assert_eq!(stated(GridLocation::Zone { zone_id: "US-CAL-CISO".into() }).country(), CountryCode::new("US"));
        assert_eq!(stated(GridLocation::Coordinates { latitude: 55.0, longitude: 12.0 }).country(), None);
        assert_eq!(stated(GridLocation::Global).country(), None);

        let estimate = Estimate { country: dk.unwrap(), coordinates: None, by: "test" };
        assert_eq!(Location::Estimated(estimate).country(), dk);
    }

    #[test]
    fn an_estimate_reads_as_country_and_coordinates() {
        let estimate = Estimate {
            country: CountryCode::new("DK").unwrap(),
            coordinates: Coordinates::new(55.6759, 12.5655),
            by: "test",
        };
        assert_eq!(estimate.to_string(), "DK (55.68, 12.57)");
    }
}
