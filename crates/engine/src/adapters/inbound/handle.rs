//! `TrackerHandle`: the public API, the handle half of the tracker actor.
//! Clones are cheap and share one tracker. [start](crate::start) makes one.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{mpsc, oneshot};

use crate::application::subscribers::{DEFAULT_CAPACITY, Subscription, subscription};
use crate::domain::{SessionReport, SourceSelection, SpanId, TrackerError, TrackerQuery, Work};
use crate::domain::{TrackerCommand, TrackerMessage};
use crate::ports::outbound::Clock;

#[derive(Clone)]
pub struct TrackerHandle {
    sender: mpsc::UnboundedSender<TrackerMessage>,
    clock: Arc<dyn Clock>,
    /// The next span's id, shared by every clone. 0 is the root span.
    next_span: Arc<AtomicUsize>,
}

impl TrackerHandle {
    /// A handle sending to the tracker behind `sender`, stamping cuts with `clock`.
    pub(crate) fn new(
        sender: mpsc::UnboundedSender<TrackerMessage>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            sender,
            clock,
            next_span: Arc::new(AtomicUsize::new(1)),
        }
    }

    /// Receive every event from now on, in a queue of 1024.
    ///
    /// See [Self::subscribe_with_capacity].
    pub fn subscribe(&self) -> Subscription {
        self.subscribe_with_capacity(DEFAULT_CAPACITY)
    }

    /// Receive every event from now on, in a queue of `capacity`.
    ///
    /// The tracker never waits for a subscriber. One that lets its queue fill
    /// up is cut off, and its [Subscription] says so once drained; keeping up,
    /// or buffering further, is the subscriber's job. Events from commands
    /// sent after this call are always delivered.
    pub fn subscribe_with_capacity(&self, capacity: usize) -> Subscription {
        let (subscriber, subscription) = subscription(capacity);
        let _ = self.sender.send(TrackerMessage::Subscribe(subscriber));
        subscription
    }

    /// Open a span, nested under `parent` if given, and return its id. Never
    /// waits: the id is handed out here, not by the tracker. `sources` limits
    /// what the span is billed for; all sources are still sampled.
    ///
    /// A start the tracker refuses (`parent` unknown or stopped, or the
    /// tracker finishing) is announced as
    /// [TrackerEvent::CommandRefused](crate::TrackerEvent::CommandRefused)
    /// with this id; stops and marks on it are then refused too.
    pub fn start_span(
        &self,
        name: &str,
        parent: Option<SpanId>,
        sources: SourceSelection,
    ) -> SpanId {
        // Relaxed is enough: the id reaches anyone else only after the send,
        // and the channel orders the send.
        let id = SpanId::new(self.next_span.fetch_add(1, Ordering::Relaxed));
        self.send(TrackerCommand::Start {
            id,
            name: name.to_owned(),
            parent,
            sources,
            at: self.clock.now(),
        });
        id
    }

    /// Report work done so far (tokens, batches, steps). Places a cut, which
    /// costs a measurement; in hot loops pass the total to [`Self::stop_span`].
    pub fn mark(&self, span: SpanId, work: Work) {
        self.send(TrackerCommand::Mark {
            id: span,
            work,
            at: self.clock.now(),
        });
    }

    /// Close a span, reporting the work it did. Stopping an unknown or
    /// already-stopped span is ignored by the tracker.
    pub fn stop_span(&self, span: SpanId, work: Work) {
        self.send(TrackerCommand::Stop {
            id: span,
            work,
            at: self.clock.now(),
        });
    }

    /// Fire-and-forget: never blocks. The message is lost only if the tracker has stopped.
    fn send(&self, command: TrackerCommand) {
        let _ = self.sender.send(TrackerMessage::Command(command));
    }

    /// Ask the tracker something; the answer comes on the query's reply.
    /// Never waits. A query sent after the tracker stopped is answered with
    /// [TrackerError::Closed], by its reply being dropped.
    pub fn query(&self, query: TrackerQuery) {
        let _ = self.sender.send(TrackerMessage::Query(query));
    }

    /// A handle that doesn't keep the tracker alive: the tracker stops once
    /// every [TrackerHandle] is dropped, weak ones or not.
    pub fn downgrade(&self) -> WeakTrackerHandle {
        WeakTrackerHandle {
            sender: self.sender.downgrade(),
            clock: self.clock.clone(),
            next_span: self.next_span.clone(),
        }
    }

    /// End the session: stop every open span at one final cut, wait until
    /// every span has settled, announce
    /// [TrackerEvent::SessionEnded](crate::TrackerEvent::SessionEnded), and
    /// return the session's final report. The tracker then stops.
    ///
    /// # Errors
    ///
    /// [TrackerError::Closed] if the tracker has already stopped, a finish
    /// before this one included.
    pub async fn finish(&self) -> Result<SessionReport, TrackerError> {
        let (reply, rx) = oneshot::channel();
        let at = self.clock.now();
        let _ = self.sender.send(TrackerMessage::Finish { at, reply });
        rx.await.map_err(|_| TrackerError::Closed)?
    }
}

/// A [TrackerHandle] that doesn't keep the tracker alive. Made by
/// [TrackerHandle::downgrade].
#[derive(Clone)]
pub struct WeakTrackerHandle {
    sender: mpsc::WeakUnboundedSender<TrackerMessage>,
    clock: Arc<dyn Clock>,
    next_span: Arc<AtomicUsize>,
}

impl WeakTrackerHandle {
    /// The handle, while the tracker is running; `None` once it has stopped.
    pub fn upgrade(&self) -> Option<TrackerHandle> {
        let sender = self.sender.upgrade()?;
        Some(TrackerHandle {
            sender,
            clock: self.clock.clone(),
            next_span: self.next_span.clone(),
        })
    }
}
