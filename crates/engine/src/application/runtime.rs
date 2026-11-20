//! Runs the tracker on tokio: the shell around the synchronous core.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::time::sleep_until;

use crate::adapters::outbound::readers::{ReadReturn, Reader, Readers, Returns};
use crate::application::tracker::Tracker;
use crate::domain::session::{Relying, RequestId};
use crate::domain::{ProviderId, ReadoutResponse, SourceId, Timestamp, TrackerMessage};
use crate::ports::outbound::Clock;

/// Asynchronously drive `tracker` until the session ends or every handle is dropped.
///
/// The session opens with a cut on every source, so every provider is asked
/// first. Then each input is handed to the [Engine], in the order
/// [next_input] gives. A provider no request reached for `every` (or its
/// cooldown, if longer) is asked by the watchdog. Provider code never runs
/// here: each provider is read and dropped on a worker thread of its own.
pub(crate) async fn run_tracker(
    tracker: Tracker,
    readers: HashMap<ProviderId, Reader>,
    mut receiver: mpsc::UnboundedReceiver<TrackerMessage>,
    every: Duration,
    clock: Arc<dyn Clock>,
    opened_at: Timestamp,
) {
    let (readers, mut returns) = Readers::new(readers, every);
    let mut engine = Engine::new(tracker, readers);
    engine.open(opened_at, Instant::now(), clock.now());
    while !engine.is_done() {
        let Some(input) = next_input(&mut receiver, &mut returns, engine.wake_at()).await else { break };
        engine.step(input, Instant::now(), clock.now());
    }
    // A finish sent while the session was finishing gets the same report.
    while let Ok(message) = receiver.try_recv() {
        if matches!(message, TrackerMessage::Finish { .. }) {
            engine.step(Input::Command(message), Instant::now(), clock.now());
        }
    }
    // Dropping the tracker ends every subscriber's stream, after SessionEnded.
    engine.close(returns);
}

/// One thing the engine reacts to.
pub(crate) enum Input {
    /// A command or query from a handle.
    Command(TrackerMessage),
    /// A read came back from a worker.
    Returned(ReadReturn),
    /// The earliest deadline passed: a read's timeout, the end of a
    /// cooldown a request waits on, or the watchdog's period.
    Tick,
}

/// The next input: a read that has come back, then a command, then the
/// earliest deadline. A command or a deadline never goes before a read that
/// has already returned, so no read is timed out because the engine looked
/// late. `None` once every handle is dropped.
pub(crate) async fn next_input(
    receiver: &mut mpsc::UnboundedReceiver<TrackerMessage>,
    returns: &mut Returns,
    wake: Option<Instant>,
) -> Option<Input> {
    tokio::select! {
        biased;
        Some(read) = returns.recv() => Some(Input::Returned(read)),
        message = receiver.recv() => message.map(Input::Command),
        () = wake_at(wake) => Some(Input::Tick),
    }
}

/// The tracker and its readers, handling one input at a time. It does no
/// waiting of its own: time is what each step is given.
pub(crate) struct Engine {
    tracker: Tracker,
    readers: Readers,
}

impl Engine {
    pub(crate) fn new(tracker: Tracker, readers: Readers) -> Self {
        Self { tracker, readers }
    }

    /// The session's opening cut, stamped `opened_at`: every provider is asked.
    pub(crate) fn open(&mut self, opened_at: Timestamp, now: Instant, wall: Timestamp) {
        let mut landed = Vec::new();
        for provider in self.readers.providers() {
            landed.extend(self.readers.ask(provider, opened_at, now, wall).1);
        }
        apply(&mut self.tracker, landed);
    }

    /// Handle `input` at `now` (`wall` on the stamps' clock), applying every
    /// readout that lands. A read or deadline after the session has ended
    /// lands nothing.
    pub(crate) fn step(&mut self, input: Input, now: Instant, wall: Timestamp) {
        let landed = match input {
            Input::Command(message) => handle(&mut self.tracker, &mut self.readers, message, now, wall),
            Input::Returned(read) if !self.tracker.is_done() => self.readers.returned(read, now, wall),
            Input::Tick if !self.tracker.is_done() => self.readers.advance(now, wall),
            Input::Returned(_) | Input::Tick => Vec::new(),
        };
        apply(&mut self.tracker, landed);
    }

    /// When the earliest deadline passes, if any is set.
    pub(crate) fn wake_at(&self) -> Option<Instant> {
        self.readers.wake_at()
    }

    pub(crate) fn is_done(&self) -> bool {
        self.tracker.is_done()
    }

    /// Stop every worker.
    pub(crate) fn close(self, returns: Returns) {
        self.readers.close(returns);
    }
}

/// Hand `message` to the tracker, its cuts asking `readers`: the readouts
/// that landed meanwhile.
fn handle(
    tracker: &mut Tracker,
    readers: &mut Readers,
    message: TrackerMessage,
    now: Instant,
    wall: Timestamp,
) -> Vec<ReadoutResponse> {
    let mut asking = Asking::new(readers, now, wall);
    tracker.handle(message, &mut asking);
    asking.landed
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
    fn new(readers: &'a mut Readers, now: Instant, wall: Timestamp) -> Self {
        Self { readers, now, wall, asked: HashMap::new(), landed: Vec::new() }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::subscribers::subscription;
    use crate::ports::outbound::ProviderResponse;

    /// AC/MEAS-98
    #[tokio::test]
    async fn a_returned_read_goes_before_a_command_and_a_deadline() {
        let (commands, mut receiver) = mpsc::unbounded_channel();
        let (results, mut returns) = mpsc::unbounded_channel();
        let (subscriber, _subscription) = subscription(1);
        commands.send(TrackerMessage::Subscribe(subscriber)).unwrap();
        results.send(ReadReturn::new(ProviderId(1), Ok(Ok(ProviderResponse::new())), Instant::now())).unwrap();
        let overdue = Instant::now().checked_sub(Duration::from_secs(1));

        let order = [
            next_input(&mut receiver, &mut returns, overdue).await,
            next_input(&mut receiver, &mut returns, overdue).await,
            next_input(&mut receiver, &mut returns, overdue).await,
        ];
        assert!(matches!(order[0], Some(Input::Returned(_))), "the read that came back first");
        assert!(matches!(order[1], Some(Input::Command(_))), "then the command");
        assert!(matches!(order[2], Some(Input::Tick)), "then the deadline");
    }
}
