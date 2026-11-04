//! The default provider registry. Probes the host and assigns each coverage slot `(device, domain, scope)` to
//! the first (best) candidate that serves it. One slot never gets two sources.
//!
//! - Power: static > static-cpu, static-gpu > RAPL (Linux) > NVML > macmon (macOS) > powermetrics (macOS)
//!   > generic-cpu. A static CPU or GPU replaces every other candidate's sources in its domain.
//! - Location: stated-location > ip-geolocation (none: the global average)
//! - Intensity: static > electricity-maps > country-average > global-average (475 gCO2/kWh)
//! - Forecast: static > electricity-maps-forecast > projection (current intensity held flat)
//!
//! Each intensity and forecast candidate takes the [Location] and reads it in
//! its own terms, or declines. Each chain ends in a candidate that cannot
//! fail, so probing is infallible.
//!
//! Probing returns a [Resolution]: what was chosen and why, for a frontend to
//! show, which [Resolution::resolve] turns into the [Sources](carbontracker::Sources) a tracker starts
//! over. The registry never prints; progress goes to a callback.
//!
//! Laid out as the engine is: `domain` what probing is asked for and finds;
//! `ports` what a candidate yields; `application` the registry and its
//! chains; `adapters` the candidates, by power, intensity and system.

mod adapters;
mod application;
mod domain;
mod ports;

pub use adapters::constant::StaticProvider;
pub use adapters::system::config_file;
pub use adapters::system::process::process_tree;
pub use application::{ForecastReport, Forecaster, ProbeProgress, ProviderRegistry, Resolution};
pub use domain::config::{
    ApiKey, GridConfig, KeyedService, Measure, MeasureParseError, Pinned, ProbeConfig, ProbeConfigError,
};
pub use domain::location::{Coordinates, CountryCode, Estimate, Location};
pub use domain::verdict::{CandidateOutcome, Verdict};
