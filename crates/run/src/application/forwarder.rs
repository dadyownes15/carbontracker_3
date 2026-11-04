//! The forwarder: a thread that takes the engine's events, projects each
//! item, and passes it on to the writer. It does no I/O and never touches
//! Python, so it never falls behind the engine's bounded queue.

use std::sync::Arc;
use std::thread;

use carbontracker::{Subscription, SubscriptionEnded, TrackerEvent};
use tokio::sync::oneshot;

use crate::application::view::View;
use crate::application::writer::WriterHandle;
use crate::domain::item::{Item, Outcome, Started};
use crate::ports::Projection;

struct Finish {
    outcome: Outcome,
    done: oneshot::Sender<()>,
}

/// Tells the forwarder how the run ended.
pub(crate) struct ForwarderHandle {
    finish: oneshot::Sender<Finish>,
}

impl ForwarderHandle {
    pub fn new<P: Projection>(
        started: Arc<Started>,
        warnings: Vec<String>,
        view: View<P>,
        events: Subscription,
        writer: WriterHandle,
    ) -> Self {
        let (finish, finished) = oneshot::channel();
        let actor = ForwarderActor {
            events,
            finished: Some(finished),
            view,
            writer,
        };
        thread::Builder::new()
            .name("carbontracker-forwarder".to_owned())
            .spawn(move || run_forwarder(actor, started, warnings))
            .expect("a thread can be spawned");
        Self { finish }
    }

    /// Sends how the run ended; the receiver answers once everything is
    /// written.
    pub fn finish(self, outcome: Outcome) -> oneshot::Receiver<()> {
        let (done, written) = oneshot::channel();
        let _ = self.finish.send(Finish { outcome, done });
        written
    }
}

/// Generic over the projection; [ForwarderHandle] is not.
struct ForwarderActor<P: Projection> {
    events: Subscription,
    finished: Option<oneshot::Receiver<Finish>>,
    view: View<P>,
    /// The only one: the writer stops once it is dropped or closed.
    writer: WriterHandle,
}

impl<P: Projection> ForwarderActor<P> {
    /// Projects one item, then passes on the blocks it returned and the item.
    fn forward(&self, item: Item) {
        let blocks = self.view.lock().project(&item);
        for block in blocks {
            self.writer.item(Item::Block(Arc::from(block)));
        }
        self.writer.item(item);
    }
}

/// The run's start and its warnings, its events, how it ended, then closes
/// the writer.
fn run_forwarder<P: Projection>(mut actor: ForwarderActor<P>, started: Arc<Started>, warnings: Vec<String>) {
    actor.forward(Item::Started(started));
    for warning in warnings {
        actor.forward(Item::Warning(warning));
    }

    // The engine's events, until the session ends or the engine stops.
    let ended = loop {
        match actor.events.blocking_recv() {
            Ok(event @ TrackerEvent::SessionEnded { .. }) => break Some(event),
            Ok(event) => actor.forward(Item::Event(event)),
            Err(SubscriptionEnded::CutOff) => {
                actor.forward(Item::Gap);
                break None;
            }
            Err(SubscriptionEnded::Closed) => break None,
        }
    };

    // How the run ended: sent before the engine finished, so it's here
    // already. An error means the RunHandle was dropped without finish.
    let finish = actor
        .finished
        .take()
        .and_then(|finished| finished.blocking_recv().ok());
    if let Some(Finish { outcome, .. }) = &finish {
        actor.forward(Item::Outcome(*outcome));
    }
    if let Some(event) = ended {
        actor.forward(Item::Event(event));
    }

    match finish {
        Some(Finish { done, .. }) => actor.writer.close(done),
        None => drop(actor.writer),
    }
}
