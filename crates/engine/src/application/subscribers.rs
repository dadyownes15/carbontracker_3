//! Delivering [TrackerEvent]s: one bounded queue per subscriber.
//!
//! The tracker never waits on a subscriber and never holds more than the
//! queues' fixed sizes. A subscriber whose queue is full is cut off and told
//! so; keeping up is its own job.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};

use crate::domain::TrackerEvent;

/// How many events a subscriber's queue holds when it does not say.
pub(crate) const DEFAULT_CAPACITY: usize = 1024;

/// The tracker's end of one subscriber's queue.
pub(crate) struct Subscriber {
    events: mpsc::Sender<TrackerEvent>,
    cut_off: Arc<AtomicBool>,
}

/// A subscriber's end: the events the tracker announces, in order.
pub struct Subscription {
    events: mpsc::Receiver<TrackerEvent>,
    cut_off: Arc<AtomicBool>,
}

/// Why a [Subscription] has no more events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SubscriptionEnded {
    /// The tracker has stopped.
    #[error("the tracker has stopped")]
    Closed,
    /// The queue filled up, so the tracker dropped this subscriber. Events
    /// from then on were not delivered; subscribe again to resume.
    #[error("the subscriber fell behind and was cut off")]
    CutOff,
}

/// A queue holding up to `capacity` events (at least one).
pub(crate) fn subscription(capacity: usize) -> (Subscriber, Subscription) {
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    let cut_off = Arc::new(AtomicBool::new(false));
    (
        Subscriber {
            events: sender,
            cut_off: cut_off.clone(),
        },
        Subscription {
            events: receiver,
            cut_off,
        },
    )
}

impl Subscription {
    /// Wait for the next event.
    ///
    /// Events queued before the stream ended are still delivered first.
    pub async fn recv(&mut self) -> Result<TrackerEvent, SubscriptionEnded> {
        self.events.recv().await.ok_or_else(|| self.ended())
    }

    /// Wait for the next event, blocking the thread. For callers outside any
    /// async runtime; panics if called inside one.
    pub fn blocking_recv(&mut self) -> Result<TrackerEvent, SubscriptionEnded> {
        self.events.blocking_recv().ok_or_else(|| self.ended())
    }

    /// The next event if one is queued, `None` if not yet.
    pub fn try_recv(&mut self) -> Result<Option<TrackerEvent>, SubscriptionEnded> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(self.ended()),
        }
    }

    fn ended(&self) -> SubscriptionEnded {
        if self.cut_off.load(Ordering::Acquire) {
            SubscriptionEnded::CutOff
        } else {
            SubscriptionEnded::Closed
        }
    }
}

/// Everyone subscribed to the tracker.
#[derive(Default)]
pub(crate) struct Subscribers {
    list: Vec<Subscriber>,
}

impl Subscribers {
    pub(crate) fn add(&mut self, subscriber: Subscriber) {
        self.list.push(subscriber);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Queue `event` for every subscriber without waiting. One whose queue is
    /// full is cut off; one that has hung up is dropped.
    pub(crate) fn send(&mut self, event: TrackerEvent) {
        self.list.retain(
            |subscriber| match subscriber.events.try_send(event.clone()) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => {
                    subscriber.cut_off.store(true, Ordering::Release);
                    false
                }
                Err(TrySendError::Closed(_)) => false,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ReadoutId;

    fn event(n: usize) -> TrackerEvent {
        TrackerEvent::ReadoutLanded {
            readout: ReadoutId(n),
            outcomes: Arc::default(),
        }
    }

    /// AC/ENG-18
    #[test]
    fn a_full_queue_cuts_its_subscriber_off_after_what_was_queued() {
        let mut subscribers = Subscribers::default();
        let (subscriber, mut subscription) = subscription(2);
        subscribers.add(subscriber);

        for n in 0..3 {
            subscribers.send(event(n));
        }

        assert!(subscribers.is_empty(), "the third event did not fit");
        assert!(subscription.try_recv().unwrap().is_some());
        assert!(subscription.try_recv().unwrap().is_some());
        assert_eq!(
            subscription.try_recv().unwrap_err(),
            SubscriptionEnded::CutOff
        );
    }

    /// AC/ENG-9, AC/ENG-18
    #[test]
    fn a_slow_subscriber_does_not_cut_off_a_fast_one() {
        let mut subscribers = Subscribers::default();
        let (slow, _slow) = subscription(1);
        let (fast, mut fast_end) = subscription(8);
        subscribers.add(slow);
        subscribers.add(fast);

        for n in 0..3 {
            subscribers.send(event(n));
        }

        let received = std::iter::from_fn(|| fast_end.try_recv().unwrap()).count();
        assert_eq!(received, 3);
    }

    /// AC/ENG-18
    #[test]
    fn a_stopped_tracker_reads_as_closed() {
        let (subscriber, mut subscription) = subscription(1);
        drop(subscriber);
        assert_eq!(
            subscription.try_recv().unwrap_err(),
            SubscriptionEnded::Closed
        );
    }

    #[test]
    fn a_hung_up_subscriber_is_dropped() {
        let mut subscribers = Subscribers::default();
        let (subscriber, subscription) = subscription(1);
        subscribers.add(subscriber);
        drop(subscription);

        subscribers.send(event(0));
        assert!(subscribers.is_empty());
    }
}
