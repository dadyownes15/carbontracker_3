//! Probing: the registry walks each chain of candidates, best first, and
//! keeps what the winners serve in a [Resolution].

mod chain;
pub(crate) mod forecaster;
mod registry;
mod resolution;
#[cfg(test)]
mod tests;

pub use forecaster::{ForecastReport, Forecaster};
pub use registry::ProviderRegistry;
#[cfg(test)]
use registry::Endpoints;
pub use resolution::{ProbeProgress, Resolution};
