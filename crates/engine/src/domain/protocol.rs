//! The tracker's protocol: what the [TrackerHandle](crate::TrackerHandle)
//! handle sends it, and what it announces to every subscriber, unasked.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::oneshot;

use crate::application::subscribers::Subscriber;
use crate::domain::{
    CatalogReport, Observation, ReadoutId, SessionReport, SourceId, SourceSelection, SpanId,
    SpanReport, Timestamp, Work,
};

/// Everything the [TrackerHandle](crate::TrackerHandle) sends the tracker.
pub(crate) enum TrackerMessage {
    Command(TrackerCommand),
    Query(TrackerQuery),
    /// Deliver every event from now on to this subscriber.
    Subscribe(Subscriber),
    /// End the session at `at`: see [TrackerHandle::finish](crate::TrackerHandle::finish).
    Finish {
        at: Timestamp,
        reply: Reply<SessionReport>,
    },
}

/// Cuts the timeline: each one the tracker accepts waits on a readout.
pub(crate) enum TrackerCommand {
    /// `id` was handed out by the handle; `name` is a report label only.
    /// `at` is taken by the caller so tracker latency never skews the cut.
    /// A refused start (an unknown or closed parent) is announced.
    Start {
        id: SpanId,
        name: String,
        parent: Option<SpanId>,
        /// Sources this span is billed against; fixed at open.
        sources: SourceSelection,
        at: Timestamp,
    },
    Stop {
        id: SpanId,
        work: Work,
        at: Timestamp,
    },
    Mark {
        id: SpanId,
        work: Work,
        at: Timestamp,
    },
}

/// Asks for a report, answered once its [Need] is met. Every query is about
/// the present: what has been settled is announced, never asked for again.
pub enum TrackerQuery {
    /// An open span's report so far. A closed span is refused at once: its
    /// report is its [TrackerEvent::SpanSettled].
    SpanReport {
        span: SpanId,
        at: Timestamp,
        reply: Reply<SpanReport>,
    },
    SessionStats {
        at: Timestamp,
        reply: Reply<SessionReport>,
    },
    /// The sources this session measures, and how each was chosen.
    Catalog { reply: Reply<CatalogReport> },
}

/// Where the tracker sends a message's answer, or why it refused it.
pub type Reply<T> = oneshot::Sender<Result<T, TrackerError>>;

/// Why the tracker refused a message, or could not answer it.
#[derive(Debug, Clone, thiserror::Error)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum TrackerError {
    #[error("unknown span {}", .0.get())]
    UnknownSpan(SpanId),
    /// A span was started under a parent that has already stopped.
    #[error("parent span {} has already stopped", .0.get())]
    ParentClosed(SpanId),
    #[error("span {} has already stopped", .0.get())]
    AlreadyClosed(SpanId),
    /// A span named a source that is not a power source in the catalog.
    #[error("{0} is not a power source on this host")]
    UnknownSource(SourceId),
    /// The tracker has stopped, so nothing can be sent or answered.
    #[error("the tracker has stopped")]
    Closed,
}

impl TrackerQuery {
    /// What this query needs before it can be answered.
    pub(crate) fn need(&self) -> Need {
        match self {
            TrackerQuery::SpanReport { span, .. } => Need::Settled(vec![*span]),
            // The session answers with what has been accounted so far: every
            // cut touches the root, so waiting for it could wait forever.
            TrackerQuery::SessionStats { .. } | TrackerQuery::Catalog { .. } => Need::Nothing,
        }
    }
}

/// What a [TrackerQuery] needs before it can be answered.
pub(crate) enum Need {
    /// Can be answered at once.
    Nothing,
    /// Every listed span is settled; see [Session::is_settled].
    Settled(Vec<SpanId>),
}

/// Something the tracker announces to every subscriber without being asked.
///
/// Large payloads are shared, so every subscriber's copy is cheap.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum TrackerEvent {
    /// A readout landed: per source, what it reported, that it had nothing
    /// new, or why it failed.
    ReadoutLanded {
        readout: ReadoutId,
        outcomes: Arc<HashMap<SourceId, Observation>>,
    },
    /// A span opened. `parent` is `None` for a top-level span.
    SpanStarted {
        span: SpanId,
        name: String,
        parent: Option<SpanId>,
        selection: SourceSelection,
        at: Timestamp,
    },
    /// A span closed, with the work it reported. Its open descendants closed
    /// with it.
    SpanStopped {
        span: SpanId,
        work: Work,
        at: Timestamp,
    },
    /// Work was reported on an open span.
    Marked {
        span: SpanId,
        work: Work,
        at: Timestamp,
    },
    /// A start, stop or mark on `span` was refused.
    CommandRefused { span: SpanId, error: TrackerError },
    /// A closed span settled: everything it was active in has been accounted.
    /// This is its final report.
    SpanSettled { report: Arc<SpanReport> },
    /// The session finished: every span has settled. The last event; the
    /// stream ends after it.
    SessionEnded { report: Arc<SessionReport> },
}
