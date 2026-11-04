//! Driven side: what the tracker needs from the world.

pub mod clock;
pub mod provider;

pub use clock::Clock;
pub use provider::{Provider, ProviderResponse, Reading};
