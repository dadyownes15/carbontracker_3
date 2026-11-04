//! Runs the tracker on tokio: the shell around the synchronous core.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::adapters::outbound::readers::{Reader, Readers};
use crate::application::tracker::Tracker;
use crate::domain::session::{Relying, RequestId};
use crate::domain::{ProviderId, ReadoutResponse, SourceId, Timestamp, TrackerMessage};
use crate::ports::outbound::Clock;

/// Asynchronously drive `tracker` until the session ends or every handle is dropped.
///
/// The session opens with a cut on every source, so every provider is asked
/// first. Then each message is delivered to the [Tracker], whose cuts ask
/// the `readers`, and each readout that lands is applied. A provider no
/// request reached for `every` (or its cooldown, if longer) is asked by the
/// watchdog. Provider code never runs here: reads run on threads of their
/// own, and the providers are dropped off this thread when the session ends.
pub(crate) async fn run_tracker(
    mut tracker: Tracker,
    readers: HashMap<ProviderId, Reader>,
    mut receiver: mpsc::UnboundedReceiver<TrackerMessage>,
    every: Duration,
    clock: Arc<dyn Clock>,
    opened_at: Timestamp,
) {
    let (mut readers, mut returns) = Readers::new(readers, every);
    let mut landed = Vec::new();
    let now = Instant::now();
    for provider in readers.providers() {
        landed.extend(readers.ask(provider, opened_at, now, clock.now()).1);
    }
    apply(&mut tracker, landed);

    loop {
        let wake = readers.wake_at();
        let landed = tokio::select! {
            message = receiver.recv() => {
                let Some(message) = message else { break };
                handle(&mut tracker, &mut readers, message, clock.now())
            }
            Some(read) = returns.recv() => {
                // Messages sent before the read came back go first, so a
                // subscriber hears what lands after it subscribed.
                handle_queued(&mut tracker, &mut readers, &mut receiver, clock.as_ref());
                // Taken back even once the session has ended, so the provider is dropped off this thread.
                let landed = readers.returned(read, Instant::now(), clock.now());
                if tracker.is_done() { Vec::new() } else { landed }
            }
            () = wake_at(wake) => {
                handle_queued(&mut tracker, &mut readers, &mut receiver, clock.as_ref());
                let landed = readers.advance(Instant::now(), clock.now());
                if tracker.is_done() { Vec::new() } else { landed }
            }
        };
        apply(&mut tracker, landed);
        // Dropping the tracker ends every subscriber's stream, after SessionEnded.
        if tracker.is_done() {
            break;
        }
    }
    readers.close(returns);
}

/// Hand `message` to the tracker, its cuts asking `readers`: the readouts
/// that landed meanwhile.
fn handle(tracker: &mut Tracker, readers: &mut Readers, message: TrackerMessage, wall: Timestamp) -> Vec<ReadoutResponse> {
    let mut asking = Asking::new(readers, wall);
    tracker.handle(message, &mut asking);
    asking.landed
}

/// Handle the messages queued now, and no more, applying what lands.
fn handle_queued(
    tracker: &mut Tracker,
    readers: &mut Readers,
    receiver: &mut mpsc::UnboundedReceiver<TrackerMessage>,
    clock: &dyn Clock,
) {
    for _ in 0..receiver.len() {
        if tracker.is_done() {
            break;
        }
        let Ok(message) = receiver.try_recv() else { break };
        let landed = handle(tracker, readers, message, clock.now());
        apply(tracker, landed);
    }
}

/// One message's cuts asking the readers: each provider once per cut.
struct Asking<'a> {
    readers: &'a mut Readers,
    now: Instant,
    wall: Timestamp,
    asked: HashMap<(ProviderId, Timestamp), Option<RequestId>>,
    landed: Vec<ReadoutResponse>,
}

impl<'a> Asking<'a> {
    fn new(readers: &'a mut Readers, wall: Timestamp) -> Self {
        Self { readers, now: Instant::now(), wall, asked: HashMap::new(), landed: Vec::new() }
    }
}

impl Relying for Asking<'_> {
    fn relied(&mut self, source: SourceId, at: Timestamp) -> Option<RequestId> {
        let provider = source.provider();
        if let Some(&request) = self.asked.get(&(provider, at)) {
            return request;
        }
        let (request, landed) = self.readers.ask(provider, at, self.now, self.wall);
        self.landed.extend(landed);
        self.asked.insert((provider, at), request);
        request
    }

    /// Answered, and its readout already applied: not one landed during
    /// this message, which is applied after it.
    fn is_answered(&self, request: RequestId) -> bool {
        self.readers.is_answered(request) && !self.landed.iter().any(|response| response.id == request)
    }
}

fn apply(tracker: &mut Tracker, landed: Vec<ReadoutResponse>) {
    for response in landed {
        tracker.apply(response);
    }
}

/// Ready at `at`; never, without one.
async fn wake_at(at: Option<Instant>) {
    match at {
        Some(at) => sleep_until(tokio::time::Instant::from_std(at)).await,
        None => std::future::pending().await,
    }
}
