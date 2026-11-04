//! [start]: the composition root. Wires the sources a registry made, the
//! meter that reads them and the tracker, on a thread of their own.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::adapters::inbound::handle::TrackerHandle;
use crate::adapters::outbound::system_clock::SystemClock;
use crate::application::runtime::run_tracker;
use crate::application::tracker::Tracker;
use crate::config::{ConfigError, TrackerConfig};
use crate::ports::outbound::Clock;
use crate::application::subscribers::Subscription;
use crate::sources::Sources;

/// Start a tracker over `sources`, and return its handle. Never probes:
/// a provider registry made `sources`.
///
/// The tracker gets a thread and a runtime of its own, so it keeps running
/// whatever the caller's thread is doing: a loop that never yields, a
/// single-threaded runtime, or no runtime at all. It stops once the session
/// is finished or every handle is dropped.
///
/// # Errors
///
/// [ConfigError] if `config` is invalid; nothing is started.
pub fn start(sources: Sources, config: TrackerConfig) -> Result<TrackerHandle, ConfigError> {
    config.validate()?;
    start_with(sources, config, Arc::new(SystemClock), |_| ())
}

/// [start], with a subscription taken before the engine reads anything, so
/// it hears every event, the opening readouts included. A subscription taken
/// with [TrackerHandle::subscribe] after [start] can miss those.
///
/// # Errors
///
/// [ConfigError] if `config` is invalid; nothing is started.
pub fn start_subscribed(sources: Sources, config: TrackerConfig) -> Result<(TrackerHandle, Subscription), ConfigError> {
    config.validate()?;
    let mut subscription = None;
    let handle = start_with(sources, config, Arc::new(SystemClock), |handle| subscription = Some(handle.subscribe()))?;
    Ok((handle, subscription.expect("subscribed before the engine started")))
}

/// [start] with `clock` stamping every cut, and `config` already valid.
pub(crate) fn start_with(
    sources: Sources,
    config: TrackerConfig,
    clock: Arc<dyn Clock>,
    before: impl FnOnce(&TrackerHandle),
) -> Result<TrackerHandle, ConfigError> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let handle = TrackerHandle::new(sender, Arc::clone(&clock));
    // Queued before the engine thread exists, so handled before any read.
    before(&handle);

    // Each provider's cooldown is asked here, on the caller's thread.
    let (readers, catalog) = sources.split(&config.fetch)?;
    let opened_at = clock.now();
    let tracker = Tracker::new(catalog, config.pue, config.announce, opened_at);
    let readout_interval = config.readout_interval;
    let tracker_clock = Arc::clone(&clock);
    std::thread::Builder::new()
        .name("carbontracker".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("the tracker's runtime starts");
            runtime.block_on(run_tracker(tracker, readers, receiver, readout_interval, tracker_clock, opened_at));
        })
        .expect("the tracker thread starts");

    Ok(handle)
}
