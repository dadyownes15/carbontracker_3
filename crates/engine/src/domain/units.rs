//! Non-negative, finite `f64` newtypes for physical units and their conversions,
//! plus the [Timestamp]s the domain counts in.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Wall-clock time. `DateTime<Utc>` for ergonomic arithmetic; nanosecond precision.
pub type Timestamp = DateTime<Utc>;

const JOULES_PER_KILOWATT_HOUR: f64 = 3_600_000.0;
const MILLIWATTS_PER_WATT: f64 = 1_000.0;
const MICROJOULES_PER_JOULE: f64 = 1_000_000.0;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum UnitError {
    #[error("{unit} must be finite, got {value}")]
    NonFinite { unit: &'static str, value: f64 },

    #[error("{unit} must be nonnegative, got {value}")]
    Negative { unit: &'static str, value: f64 },

    #[error("{unit} arithmetic produced an invalid value: {value}")]
    ArithmeticOverflow { unit: &'static str, value: f64 },
}

fn validate_nonnegative_finite(unit: &'static str, value: f64) -> Result<f64, UnitError> {
    if !value.is_finite() {
        return Err(UnitError::NonFinite { unit, value });
    }

    if value < 0.0 {
        return Err(UnitError::Negative { unit, value });
    }

    Ok(value)
}

macro_rules! nonnegative_unit {
    (
        $(#[$meta:meta])*
        $name:ident,
        unit = $unit:literal,
        symbol = $symbol:literal
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
        #[cfg_attr(feature = "serde", derive(serde::Serialize))]
        pub struct $name(f64);

        impl $name {
            pub const ZERO: Self = Self(0.0);

            pub fn new(value: f64) -> Result<Self, UnitError> {
                Ok(Self(validate_nonnegative_finite($unit, value)?))
            }

            pub const fn get(self) -> f64 {
                self.0
            }

            pub fn is_zero(self) -> bool {
                self.0 == 0.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::ZERO
            }
        }

        impl std::ops::Add for $name {
            type Output = Self;

            fn add(self, other: Self) -> Self {
                Self(self.0 + other.0)
            }
        }

        impl std::ops::AddAssign for $name {
            fn add_assign(&mut self, other: Self) {
                self.0 += other.0;
            }
        }

        /// Scaling by a share or a PUE. `factor` must be finite and non-negative.
        impl std::ops::Mul<f64> for $name {
            type Output = Self;

            fn mul(self, factor: f64) -> Self {
                debug_assert!(
                    factor.is_finite() && factor >= 0.0,
                    "{factor} would leave {}",
                    $unit
                );
                Self(self.0 * factor)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{} {}", self.0, $symbol)
            }
        }

        impl TryFrom<f64> for $name {
            type Error = UnitError;

            fn try_from(value: f64) -> Result<Self, Self::Error> {
                Self::new(value)
            }
        }

        impl From<$name> for f64 {
            fn from(value: $name) -> Self {
                value.get()
            }
        }
    };
}

nonnegative_unit!(
    /// Instantaneous power in watts.
    Watts,
    unit = "watts",
    symbol = "W"
);

nonnegative_unit!(
    /// Energy in joules.
    Joules,
    unit = "joules",
    symbol = "J"
);

nonnegative_unit!(
    /// Energy in kilowatt-hours.
    KilowattHours,
    unit = "kilowatt-hours",
    symbol = "kWh"
);

nonnegative_unit!(
    /// Carbon emissions in grams of CO2 equivalent.
    GramsCo2e,
    unit = "grams CO2e",
    symbol = "gCO2e"
);

nonnegative_unit!(
    /// Carbon intensity in grams of CO2 equivalent per kilowatt-hour.
    GramsCo2ePerKwh,
    unit = "grams CO2e per kilowatt-hour",
    symbol = "gCO2e/kWh"
);

impl Watts {
    pub fn from_milliwatts(milliwatts: f64) -> Result<Self, UnitError> {
        Self::new(milliwatts / MILLIWATTS_PER_WATT)
    }

    pub fn as_milliwatts(self) -> f64 {
        self.0 * MILLIWATTS_PER_WATT
    }

    pub fn energy_over(self, duration: Duration) -> Result<Joules, UnitError> {
        Joules::new(self.0 * duration.as_secs_f64())
    }
}

impl Joules {
    pub fn from_microjoules(microjoules: f64) -> Result<Self, UnitError> {
        Self::new(microjoules / MICROJOULES_PER_JOULE)
    }

    pub fn as_microjoules(self) -> f64 {
        self.0 * MICROJOULES_PER_JOULE
    }

    pub fn to_kilowatt_hours(self) -> KilowattHours {
        KilowattHours(self.0 / JOULES_PER_KILOWATT_HOUR)
    }
}

impl GramsCo2e {
    /// From `∫P·I dt`, with P in watts, I in gCO2e/kWh and t in seconds.
    /// Divides out J/kWh to get grams.
    pub fn from_power_intensity_integral(value: f64) -> Result<Self, UnitError> {
        Self::new(value / JOULES_PER_KILOWATT_HOUR)
    }
}

impl KilowattHours {
    pub fn to_joules(self) -> Result<Joules, UnitError> {
        Joules::new(self.0 * JOULES_PER_KILOWATT_HOUR)
    }

    pub fn emissions_at(self, intensity: GramsCo2ePerKwh) -> Result<GramsCo2e, UnitError> {
        GramsCo2e::new(self.0 * intensity.get())
    }
}
