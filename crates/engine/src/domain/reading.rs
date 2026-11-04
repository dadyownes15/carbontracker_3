//! What the engine says became of a request to read a provider, and what a
//! provider and its policy say about when it may be read.

use std::time::Duration;

use crate::domain::ProviderError;

/// How long a provider's reading stays current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cooldown {
    /// Read again once this long has passed since the last read began.
    After(Duration),
    /// Read once: no later read can say anything new.
    Never,
}

/// Why a source was not read, while all is well.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Skip {
    /// Inside the provider's cooldown.
    Cooldown,
    /// A read is still running, inside the timeout.
    Busy,
}

/// Why a source was not read, or not readable, when something is wrong.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Failure {
    /// The provider failed this source, or the whole read.
    #[error(transparent)]
    Provider(ProviderError),
    /// The provider's response left this source out.
    #[error("the provider's response left this source out")]
    Unanswered,
    /// The read answering this request outlived the timeout.
    #[error("the read timed out")]
    TimedOut,
    /// Not read: an earlier read is still running past the timeout.
    #[error("an earlier read is still running past the timeout")]
    Stalled,
    /// The read answering this request panicked; the provider is retired.
    #[error("the read panicked: {0}")]
    Panicked(String),
    /// Not read: the provider panicked in an earlier read.
    #[error("the provider was retired after it panicked")]
    Retired,
}

/// What the engine does with a request to a provider that is unready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionWhenUnready {
    /// Answer at once, without reading: `Skipped(Cooldown)` or `Skipped(Busy)`.
    Skip,
    /// Queue the request, unless one is queued already, and answer it with
    /// the read that starts once the provider is ready.
    Queue,
}

/// What a provider does while unready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnreadyActions {
    /// When the provider is inside its cooldown.
    pub on_cooldown: ActionWhenUnready,
    /// When the provider is out on a read.
    pub on_busy: ActionWhenUnready,
}

impl UnreadyActions {
    /// Queue whenever unready: what a power provider does by default.
    pub const QUEUE: Self = Self {
        on_cooldown: ActionWhenUnready::Queue,
        on_busy: ActionWhenUnready::Queue,
    };
    /// Skip whenever unready: what the intensity's provider does by default.
    pub const SKIP: Self = Self {
        on_cooldown: ActionWhenUnready::Skip,
        on_busy: ActionWhenUnready::Skip,
    };
}
