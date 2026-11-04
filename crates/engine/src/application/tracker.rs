//! The tracker's synchronous core: when cuts are placed, readouts land and
//! queries are answered. What the answers are is the [Session]'s business.

use std::collections::HashMap;
use std::sync::Arc;

use crate::application::subscribers::Subscribers;
use crate::config::Announcements;
use crate::domain::protocol::Reply;
use crate::domain::session::Relying;
use crate::domain::*;
use crate::domain::{TrackerCommand, TrackerMessage, TrackerQuery};

pub(crate) struct Tracker {
    subscribers: Subscribers,
    /// Which events are announced at all.
    announce: Announcements,
    session: Session,
    sample_store: SampleStore,

    /// Queries parked until their spans are settled.
    waiting: Vec<(Vec<SpanId>, TrackerQuery)>,

    /// Where the session ends, once a finish placed its last cut.
    finish_at: Option<Timestamp>,
    /// Every finish waiting for the session's final report.
    finishing: Vec<Reply<SessionReport>>,
    /// The session has ended and the tracker should stop.
    done: bool,
}

/// How much a [Tracker] holds; see [Tracker::held].
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Held {
    pub spans: usize,
    pub samples: usize,
}

/// Initial per-series capacity (~1 h at 1 Hz).
const SERIES_CAPACITY: usize = 4096;

impl Tracker {
    /// Create a tracker over the sources in `catalog`, with its session opened
    /// at `opened_at`, all usage scaled by `pue`, announcing the events in `announce`.
    pub(crate) fn new(
        catalog: SourceCatalog,
        pue: PowerUsageEffectiveness,
        announce: Announcements,
        opened_at: Timestamp,
    ) -> Self {
        Self {
            subscribers: Subscribers::default(),
            announce,
            sample_store: SampleStore::new_from_source(&catalog.descriptions, SERIES_CAPACITY)
                .expect("No duplicate source keys"),
            session: Session::new(catalog, pue, opened_at),
            waiting: Vec::new(),
            finish_at: None,
            finishing: Vec::new(),
            done: false,
        }
    }

    /// Handle one message from the [TrackerHandle](crate::TrackerHandle).
    ///
    /// A start, stop or mark cuts the timeline, asking `relying` which
    /// request answers each source it cuts. A refused one (a closed parent, a
    /// repeated stop, a mark on a closed span), every query and every
    /// subscription ask nothing.
    pub(crate) fn handle(&mut self, msg: TrackerMessage, relying: &mut dyn Relying) {
        match msg {
            TrackerMessage::Command(command) => self.command(command, relying),
            TrackerMessage::Query(query) => self.ask(query),
            TrackerMessage::Subscribe(subscriber) => self.subscribers.add(subscriber),
            TrackerMessage::Finish { at, reply } => self.finish(at, reply, relying),
        }
        // A cut may rely only on requests already answered.
        let settled = self.session.account_ready(&self.sample_store);
        self.settle(settled);
    }

    /// [Tracker::handle], every source a cut asks relying on one fresh
    /// request: the request, if one was asked.
    #[cfg(test)]
    pub(crate) fn handle_message(&mut self, msg: TrackerMessage) -> Option<ReadoutRequest> {
        struct Fresh(ReadoutId, bool);
        impl Relying for Fresh {
            fn relied(&mut self, _: SourceId, _: Timestamp) -> Option<ReadoutId> {
                self.1 = true;
                Some(self.0)
            }
        }
        thread_local!(static NEXT: std::cell::Cell<usize> = const { std::cell::Cell::new(1) });
        let id = ReadoutId(NEXT.with(|next| next.replace(next.get() + 1)));
        let mut fresh = Fresh(id, false);
        self.handle(msg, &mut fresh);
        fresh.1.then(|| ReadoutRequest { id, sources: self.session.catalog().billable() })
    }

    /// Place the session's last cut at `at`, closing every open span, and
    /// answer `reply` once everything has settled. A finish arriving while
    /// the session is finishing waits for the same report.
    fn finish(&mut self, at: Timestamp, reply: Reply<SessionReport>, relying: &mut dyn Relying) {
        self.finishing.push(reply);
        if self.finish_at.is_some() {
            return;
        }
        self.session.finish(at, relying);
        self.finish_at = Some(self.session.timeline().latest_cut());
    }

    /// Whether the session has ended, so the tracker should stop.
    pub(crate) fn is_done(&self) -> bool {
        self.done
    }

    /// Once the last cut has been accounted, announce the session's end and
    /// answer every finish.
    fn end_if_drained(&mut self) {
        let Some(at) = self.finish_at else { return };
        if self.done || !self.session.is_drained() {
            return;
        }
        let report = self.session.session_report(at);
        self.subscribers.send(TrackerEvent::SessionEnded {
            report: Arc::new(report.clone()),
        });
        for reply in self.finishing.drain(..) {
            let _ = reply.send(Ok(report.clone()));
        }
        self.done = true;
    }

    /// Place the cut `command` asks for. A refused command places nothing.
    fn command(&mut self, command: TrackerCommand, relying: &mut dyn Relying) {
        if self.finish_at.is_some() {
            let span = match &command {
                TrackerCommand::Start { id, .. }
                | TrackerCommand::Stop { id, .. }
                | TrackerCommand::Mark { id, .. } => *id,
            };
            self.refuse(span, TrackerError::Closed);
            return;
        }
        match command {
            TrackerCommand::Start {
                id,
                name,
                parent,
                sources,
                at,
            } => match self.session.start(id, name, parent, sources, at, relying) {
                Ok(()) => {
                    if self.should_announce(self.announce.cuts) {
                        self.announce_start(id, parent);
                    }
                }
                Err(error) => self.refuse(id, error),
            },
            // The work is not kept: it goes out in the event, or nowhere.
            TrackerCommand::Stop { id, work, at } => {
                let stopped = self.session.stop(id, at, relying);
                self.cut(id, stopped, move |at| TrackerEvent::SpanStopped {
                    span: id,
                    work,
                    at,
                })
            }
            TrackerCommand::Mark { id, work, at } => {
                let marked = self.session.mark(id, at, relying);
                self.cut(id, marked, move |at| TrackerEvent::Marked {
                    span: id,
                    work,
                    at,
                })
            }
        }
    }

    /// Finish a stop or mark on `span`: announce it with `event` at the cut it
    /// placed, or announce its refusal.
    fn cut(
        &mut self,
        span: SpanId,
        placed: Result<(), TrackerError>,
        event: impl FnOnce(Timestamp) -> TrackerEvent,
    ) {
        match placed {
            Ok(()) => {
                if self.should_announce(self.announce.cuts) {
                    let at = self.session.timeline().latest_cut();
                    self.subscribers.send(event(at));
                }
            }
            Err(error) => self.refuse(span, error),
        }
    }

    /// Announce that a command on `span` was refused with `error`.
    fn refuse(&mut self, span: SpanId, error: TrackerError) {
        if self.should_announce(self.announce.refusals) {
            self.subscribers
                .send(TrackerEvent::CommandRefused { span, error });
        }
    }

    /// Announce that `span` opened under `parent`.
    fn announce_start(&mut self, span: SpanId, parent: Option<SpanId>) {
        let record = self
            .session
            .timeline()
            .span(span)
            .expect("the span just opened");
        let event = TrackerEvent::SpanStarted {
            span,
            name: record.name.clone(),
            parent,
            selection: record.selection.clone(),
            at: record.opened_at,
        };
        self.subscribers.send(event);
    }

    /// Whether to build an event of a `kind` that may be off: only if it is
    /// on and someone is listening.
    fn should_announce(&self, kind: bool) -> bool {
        kind && !self.subscribers.is_empty()
    }

    /// Apply a landed [ReadoutResponse].
    ///
    /// Stores what each source reported, lets the session account the
    /// segments the readout finishes, and announces the readout. Each span
    /// that settled is announced with its final report, has the queries
    /// waiting on it answered, and is forgotten. Then samples no segment
    /// still needs are dropped.
    pub(crate) fn apply(&mut self, response: ReadoutResponse) {
        let readout = response.id;
        let mut outcomes: HashMap<_, _> = response.into_outcomes().collect();

        self.store(&mut outcomes);
        if self.should_announce(self.announce.readouts) {
            let outcomes = Arc::new(outcomes);
            self.subscribers
                .send(TrackerEvent::ReadoutLanded { readout, outcomes });
        }

        let settled = self.session.land(readout, &self.sample_store);
        self.settle(settled);
    }

    /// Announce each span in `settled` with its final report, answer the
    /// queries waiting on it and forget it. Then drop the samples no segment
    /// still needs, and end the session once it has drained.
    fn settle(&mut self, mut settled: Vec<SpanId>) {
        // A child's id is always higher than its parent's. Forgetting children
        // first folds their usage into a parent that is still there.
        settled.sort_unstable_by(|a, b| b.cmp(a));
        for span in settled {
            if self.should_announce(self.announce.settled) {
                let at = self.session.timeline().latest_cut();
                if let Ok(report) = self.session.span_report(span, at) {
                    self.subscribers.send(TrackerEvent::SpanSettled {
                        report: Arc::new(report),
                    });
                }
            }
            // A profile asked while the span was open is answered before it goes.
            self.answer_settled();
            self.session.forget(span);
        }
        self.answer_settled();

        self.sample_store
            .delete_before_each(&self.session.needed_from_each());
        self.end_if_drained();
    }

    /// How much the tracker holds: spans still in the timeline (the root
    /// included) and samples across every series.
    #[cfg(test)]
    pub(crate) fn held(&self) -> Held {
        Held {
            spans: self.session.timeline().len(),
            samples: self.sample_store.len(),
        }
    }

    /// Store the [Sample]s each source reported. Quiet and failed sources are
    /// only announced.
    /// Store each source's samples. Samples that break their kind's rule are
    /// not stored, and their source is announced as having failed with them.
    fn store(&mut self, outcomes: &mut HashMap<SourceId, Observation>) {
        for (&source, outcome) in outcomes.iter_mut() {
            if let Observation::Samples(samples) = outcome
                && let Err(SampleStoreError::Invalid(why)) =
                    self.sample_store.store(source, samples.clone())
            {
                *outcome =
                    Observation::Failed(Failure::Provider(ProviderError::Invalid(why.to_owned())));
            }
        }
    }

    /// Answer `query` now if its [Need] is met.
    ///
    /// Otherwise park it until [Tracker::answer_settled] can. A report on a
    /// closed span is refused at once, settled or not, so the answer never
    /// depends on how fast readouts land.
    fn ask(&mut self, query: TrackerQuery) {
        let query = match query {
            TrackerQuery::SpanReport { span, reply, .. } if self.is_closed(span) => {
                let _ = reply.send(Err(TrackerError::AlreadyClosed(span)));
                return;
            }
            query => query,
        };
        match query.need() {
            Need::Nothing => self.answer(query),
            Need::Settled(spans) if self.all_settled(&spans) => self.answer(query),
            Need::Settled(spans) => self.waiting.push((spans, query)),
        }
    }

    /// Whether `span` was started and has since closed, forgotten or not.
    fn is_closed(&self, span: SpanId) -> bool {
        self.session.timeline().was_closed(span)
    }

    /// Whether every span in `spans` is settled.
    fn all_settled(&self, spans: &[SpanId]) -> bool {
        spans.iter().all(|&span| self.session.is_settled(span))
    }

    /// Answer every parked query whose spans are now settled.
    fn answer_settled(&mut self) {
        for (spans, query) in std::mem::take(&mut self.waiting) {
            if self.all_settled(&spans) {
                self.answer(query);
            } else {
                self.waiting.push((spans, query));
            }
        }
    }

    /// Send `query` its answer, built by the [Session].
    fn answer(&self, query: TrackerQuery) {
        match query {
            TrackerQuery::SpanReport { span, at, reply } => {
                let _ = reply.send(self.session.span_report(span, at));
            }
            TrackerQuery::SessionStats { at, reply } => {
                let _ = reply.send(Ok(self.session.session_report(at)));
            }
            TrackerQuery::Catalog { reply } => {
                let _ = reply.send(Ok(self.session.catalog().report()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use tokio::sync::oneshot;

    use super::*;
    use crate::application::subscribers::{Subscription, subscription};
    use crate::domain::source::SourceDescription;

    fn t(secs: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(secs)
    }

    fn gpu() -> SourceId {
        SourceId::new(ProviderId(1), 0)
    }

    /// A tracker over one GPU, with every event kind on and one subscriber.
    fn tracker() -> (Tracker, Subscription) {
        let description = SourceDescription::Power {
            device_name: "gpu".to_owned(),
            domain: PowerDomain::Gpu,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Stated,
        };
        let catalog = SourceCatalog {
            descriptions: HashMap::from([(gpu(), description)]),
            ..SourceCatalog::default()
        };
        let mut tracker = Tracker::new(
            catalog,
            PowerUsageEffectiveness::default(),
            Announcements::default(),
            t(0),
        );
        let (subscriber, events) = subscription(64);
        tracker.handle_message(TrackerMessage::Subscribe(subscriber));
        (tracker, events)
    }

    /// Every event queued so far.
    fn drain(events: &mut Subscription) -> Vec<TrackerEvent> {
        std::iter::from_fn(|| events.try_recv().ok().flatten()).collect()
    }

    /// The response to `request`: the GPU drew `watts` at `at`.
    fn gpu_drew(request: ReadoutRequest, at: Timestamp, watts: f64) -> ReadoutResponse {
        let mut response = ReadoutResponse::new(request.id);
        response.read(gpu(), vec![Sample::new(at, watts)]);
        response
    }

    fn start(tracker: &mut Tracker, at: Timestamp) -> (SpanId, ReadoutRequest) {
        start_under(tracker, None, at)
    }

    fn start_under(
        tracker: &mut Tracker,
        parent: Option<SpanId>,
        at: Timestamp,
    ) -> (SpanId, ReadoutRequest) {
        let id = tracker.session.timeline().next_id();
        let request = send_start(tracker, id, parent, at).expect("an open parent takes a child");
        (id, request)
    }

    /// Send a start for `id`, as the handle would; `None` if it was refused.
    fn send_start(
        tracker: &mut Tracker,
        id: SpanId,
        parent: Option<SpanId>,
        at: Timestamp,
    ) -> Option<ReadoutRequest> {
        tracker.handle_message(TrackerMessage::Command(TrackerCommand::Start {
            id,
            name: "work".to_owned(),
            parent,
            sources: SourceSelection::All,
            at,
        }))
    }

    fn stop(tracker: &mut Tracker, span: SpanId, at: Timestamp) -> Option<ReadoutRequest> {
        tracker.handle_message(TrackerMessage::Command(TrackerCommand::Stop {
            id: span,
            work: Work::default(),
            at,
        }))
    }

    type Answer = oneshot::Receiver<Result<SpanReport, TrackerError>>;

    /// Ask for `span`'s profile; the answer may not be ready yet.
    fn ask_profile(tracker: &mut Tracker, span: SpanId, at: Timestamp) -> Answer {
        let (reply, answer) = oneshot::channel();
        let request = tracker.handle_message(TrackerMessage::Query(TrackerQuery::SpanReport {
            span,
            at,
            reply,
        }));
        assert!(request.is_none(), "a query starts no readout");
        answer
    }

    /// The report `span` settled with, among the events queued so far.
    fn settled(events: &mut Subscription, span: SpanId) -> Option<Arc<SpanReport>> {
        drain(events).into_iter().find_map(|event| match event {
            TrackerEvent::SpanSettled { report } if report.span_id == span => Some(report),
            _ => None,
        })
    }

    /// Profile an open span whose latest cut has landed.
    fn profile(tracker: &mut Tracker, span: SpanId, at: Timestamp) -> SpanReport {
        ask_profile(tracker, span, at)
            .try_recv()
            .expect("an open span settled up to its latest cut is answered at once")
            .expect("the span exists")
    }

    fn steps() -> Work {
        Work::of("steps", 1.0)
    }

    fn energy(profile: &SpanReport) -> f64 {
        profile.total.energy.get()
    }

    /// AC/ENG-15, AC/PROF-23
    #[test]
    fn ten_watts_for_ten_seconds_is_a_hundred_joules() {
        let (mut tracker, mut events) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");

        tracker.apply(gpu_drew(opened, t(1), 10.0));
        tracker.apply(gpu_drew(closed, t(11), 10.0));

        assert_eq!(
            energy(&settled(&mut events, span).expect("both readouts landed")),
            100.0
        );
    }

    /// AC/ENG-17, AC/ENG-23
    #[test]
    fn a_closed_span_is_refused_before_it_settles() {
        let (mut tracker, _) = tracker();
        let (span, _) = start(&mut tracker, t(1));
        stop(&mut tracker, span, t(11)).expect("the first stop cuts");

        let mut answer = ask_profile(&mut tracker, span, t(11));
        assert!(matches!(
            answer.try_recv(),
            Ok(Err(TrackerError::AlreadyClosed(_)))
        ));
    }

    /// AC/PROF-32
    #[test]
    fn a_span_waits_for_cuts_made_by_other_spans() {
        let (mut tracker, mut events) = tracker();
        let (s, opened_s) = start(&mut tracker, t(1));
        let (_, opened_t) = start(&mut tracker, t(6));
        let closed_s = stop(&mut tracker, s, t(11)).expect("the first stop cuts");
        tracker.apply(gpu_drew(opened_s, t(1), 10.0));
        tracker.apply(gpu_drew(closed_s, t(11), 10.0));

        // s's segments are c1→c2 and c2→c3; c2 belongs to the other span.
        assert!(settled(&mut events, s).is_none());

        tracker.apply(gpu_drew(opened_t, t(6), 10.0));
        assert!(settled(&mut events, s).is_some());
    }

    /// AC/ENG-20, AC/PROF-34
    #[test]
    fn an_open_span_is_answered_up_to_its_latest_cut() {
        let (mut tracker, _) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let mark = TrackerCommand::Mark {
            id: span,
            work: steps(),
            at: t(11),
        };
        let marked = tracker
            .handle_message(TrackerMessage::Command(mark))
            .expect("a mark cuts");
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        tracker.apply(gpu_drew(marked, t(11), 10.0));

        // Still open, but nothing it was active in is pending.
        assert_eq!(energy(&profile(&mut tracker, span, t(30))), 100.0);
    }

    /// AC/ENG-23
    #[test]
    fn an_unknown_span_is_answered_at_once() {
        let (mut tracker, _) = tracker();
        let mut answer = ask_profile(&mut tracker, SpanId::new(99), t(1));
        assert!(matches!(
            answer.try_recv(),
            Ok(Err(TrackerError::UnknownSpan(_)))
        ));
    }

    /// AC/MEAS-3, AC/MEAS-68
    #[test]
    fn readouts_landing_out_of_order_still_integrate_the_whole_ramp() {
        let (mut tracker, mut events) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");

        // 0 W ramping to 20 W over 10 s is 100 J; the later readout lands first.
        tracker.apply(gpu_drew(closed, t(11), 20.0));
        tracker.apply(gpu_drew(opened, t(1), 0.0));

        assert_eq!(
            energy(&settled(&mut events, span).expect("both readouts landed")),
            100.0
        );
    }

    /// AC/ENG-15, AC/PROF-33
    #[test]
    fn a_settled_span_is_announced() {
        let (mut tracker, mut events) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        tracker.apply(gpu_drew(closed, t(11), 10.0));

        let settled: Vec<f64> = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                TrackerEvent::SpanSettled { report } if report.span_id == span => {
                    Some(energy(&report))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            settled,
            vec![100.0],
            "announced once, with its final report"
        );
    }

    /// AC/MEAS-2, AC/PROF-11, AC/PROF-15
    #[test]
    fn stopping_twice_costs_no_second_readout() {
        let (mut tracker, _) = tracker();
        let (span, _) = start(&mut tracker, t(1));
        assert!(stop(&mut tracker, span, t(11)).is_some());
        assert!(stop(&mut tracker, span, t(21)).is_none());
    }

    /// AC/MEAS-2, AC/PROF-11
    #[test]
    fn marking_an_unknown_span_costs_no_readout() {
        let (mut tracker, _) = tracker();
        let mark = TrackerCommand::Mark {
            id: SpanId::new(99),
            work: steps(),
            at: t(1),
        };
        assert!(
            tracker
                .handle_message(TrackerMessage::Command(mark))
                .is_none()
        );
    }

    /// AC/MEAS-70, AC/ENG-49, AC/ENG-62
    #[test]
    fn a_sample_that_breaks_its_rule_is_announced_as_invalid_and_not_stored() {
        let (mut tracker, mut events) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        tracker.apply(gpu_drew(opened, t(1), 10.0));

        let stopped = stop(&mut tracker, span, t(11)).expect("a stop cuts");
        let mut response = ReadoutResponse::new(stopped.id);
        response.read(gpu(), vec![Sample::new(t(11), -5.0)]);
        tracker.apply(response);

        let invalid = drain(&mut events).into_iter().any(|event| match event {
            TrackerEvent::ReadoutLanded { outcomes, .. } => {
                matches!(
                    outcomes.get(&gpu()),
                    Some(Observation::Failed(Failure::Provider(
                        ProviderError::Invalid(_)
                    )))
                )
            }
            _ => false,
        });
        assert!(invalid);
        assert_eq!(tracker.held().samples, 1, "only the valid sample is held");
    }

    /// AC/ENG-10, AC/ENG-49
    #[test]
    fn a_failed_source_is_reported() {
        let (mut tracker, mut events) = tracker();
        let (_, opened) = start(&mut tracker, t(1));

        let mut response = ReadoutResponse::new(opened.id);
        response.observe(gpu(), Observation::Failed(Failure::TimedOut));
        tracker.apply(response);

        let reported = drain(&mut events).into_iter().any(|event| match event {
            TrackerEvent::ReadoutLanded { outcomes, .. } => {
                matches!(
                    outcomes.get(&gpu()),
                    Some(Observation::Failed(Failure::TimedOut))
                )
            }
            _ => false,
        });
        assert!(reported);
    }

    /// AC/MEAS-1, AC/ENG-10
    #[test]
    fn every_readout_lands_under_its_own_id() {
        let (mut tracker, mut events) = tracker();
        let watchdog = ReadoutRequest { id: ReadoutId(999), sources: Vec::new() };
        tracker.apply(ReadoutResponse::new(watchdog.id));

        assert!(matches!(
            events.try_recv(),
            Ok(Some(TrackerEvent::ReadoutLanded { readout, .. })) if readout == watchdog.id
        ));
    }

    /// AC/MEAS-17, AC/MEAS-52, AC/ENG-10
    #[test]
    fn a_readout_carries_the_samples_it_landed() {
        let (mut tracker, mut events) = tracker();
        let (_, opened) = start(&mut tracker, t(1));
        drain(&mut events);

        tracker.apply(gpu_drew(opened, t(1), 10.0));

        let Some(TrackerEvent::ReadoutLanded { outcomes, .. }) =
            drain(&mut events).into_iter().next()
        else {
            panic!("a readout landed");
        };
        assert_eq!(
            outcomes[&gpu()],
            Observation::Samples(vec![Sample::new(t(1), 10.0)])
        );
    }

    /// AC/PROF-6
    #[test]
    fn every_accepted_cut_is_announced_at_the_time_it_was_placed() {
        let (mut tracker, mut events) = tracker();
        let (span, _) = start(&mut tracker, t(1));
        let mark = TrackerCommand::Mark {
            id: span,
            work: steps(),
            at: t(5),
        };
        tracker.handle_message(TrackerMessage::Command(mark));
        stop(&mut tracker, span, t(11));

        let cuts: Vec<(SpanId, Timestamp)> = drain(&mut events)
            .into_iter()
            .map(|event| match event {
                TrackerEvent::SpanStarted {
                    span,
                    parent: None,
                    at,
                    ..
                } => (span, at),
                TrackerEvent::Marked { span, at, .. } => (span, at),
                TrackerEvent::SpanStopped { span, at, .. } => (span, at),
                other => panic!("no readout has landed, got {other:?}"),
            })
            .collect();
        assert_eq!(cuts, vec![(span, t(1)), (span, t(5)), (span, t(11))]);
    }

    /// AC/ENG-6, AC/ENG-14, AC/PROF-11, AC/PROF-16
    #[test]
    fn a_refused_stop_is_announced() {
        let (mut tracker, mut events) = tracker();
        stop(&mut tracker, SpanId::new(99), t(1));

        assert!(matches!(
            drain(&mut events).as_slice(),
            [TrackerEvent::CommandRefused {
                error: TrackerError::UnknownSpan(_),
                ..
            }]
        ));
    }

    /// AC/ENG-4
    #[test]
    fn starts_arriving_out_of_order_both_open_and_settle() {
        // Two threads took 5 and 6 from the handle, and 6's start arrived first.
        let (mut tracker, mut events) = tracker();
        let (early, late) = (SpanId::new(5), SpanId::new(6));
        let opened_late = send_start(&mut tracker, late, None, t(1)).expect("a new id opens");
        let opened_early =
            send_start(&mut tracker, early, None, t(2)).expect("an older id still opens");
        let closed_late = stop(&mut tracker, late, t(3)).expect("the stop cuts");
        let closed_early = stop(&mut tracker, early, t(4)).expect("the stop cuts");
        for (request, at) in [
            (opened_late, 1),
            (opened_early, 2),
            (closed_late, 3),
            (closed_early, 4),
        ] {
            tracker.apply(gpu_drew(request, t(at), 10.0));
        }

        let mut settled: Vec<SpanId> = drain(&mut events)
            .iter()
            .filter_map(|event| match event {
                TrackerEvent::SpanSettled { report } => Some(report.span_id),
                _ => None,
            })
            .collect();
        settled.sort();
        assert_eq!(settled, [early, late]);
    }

    /// AC/ENG-3
    #[test]
    fn an_event_kind_that_is_off_is_never_sent() {
        let (mut tracker, mut events) = tracker();
        tracker.announce = Announcements {
            cuts: false,
            ..Announcements::default()
        };
        let (span, opened) = start(&mut tracker, t(1));
        let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        tracker.apply(gpu_drew(closed, t(11), 10.0));

        assert!(drain(&mut events).iter().all(|event| !matches!(
            event,
            TrackerEvent::SpanStarted { .. }
                | TrackerEvent::SpanStopped { .. }
                | TrackerEvent::Marked { .. }
        )));
    }

    /// The session's report as of `at`.
    fn session_report(tracker: &mut Tracker, at: Timestamp) -> SessionReport {
        let (reply, mut answer) = oneshot::channel();
        tracker.handle_message(TrackerMessage::Query(TrackerQuery::SessionStats {
            at,
            reply,
        }));
        answer
            .try_recv()
            .expect("answered at once")
            .expect("always answered")
    }

    /// AC/ENG-21, AC/ENG-29, AC/ENG-34, AC/MEAS-88, AC/PROF-35
    #[test]
    fn settled_spans_and_samples_no_segment_needs_are_not_kept() {
        let (mut tracker, _events) = tracker();
        for n in 0..100 {
            let (span, opened) = start(&mut tracker, t(1 + 2 * n));
            let closed = stop(&mut tracker, span, t(2 + 2 * n)).expect("the first stop cuts");
            tracker.apply(gpu_drew(opened, t(1 + 2 * n), 10.0));
            tracker.apply(gpu_drew(closed, t(2 + 2 * n), 10.0));
        }

        let held = tracker.held();
        assert_eq!(held.spans, 1, "only the root is left");
        assert!(
            tracker.session.ledger_entries() <= 2,
            "the root's ledger and folded total, one source each"
        );
        assert!(
            held.samples <= 2,
            "only what the next segment reads, got {}",
            held.samples
        );

        // A hundred 1 s spans at 10 W, still in the totals.
        assert_eq!(
            session_report(&mut tracker, t(201)).claimed.energy.get(),
            1000.0
        );
    }

    /// AC/ENG-32, AC/PROF-21, AC/PROF-33, AC/PROF-39
    #[test]
    fn a_parent_reports_a_child_that_was_forgotten_before_it() {
        let (mut tracker, mut events) = tracker();
        let (parent, r1) = start(&mut tracker, t(1));
        let (child, r2) = start_under(&mut tracker, Some(parent), t(3));
        let r3 = stop(&mut tracker, child, t(5)).expect("the child stops");
        for (request, at) in [(r1, 1), (r2, 3), (r3, 5)] {
            tracker.apply(gpu_drew(request, t(at), 10.0));
        }
        assert!(settled(&mut events, child).is_some());
        assert_eq!(
            tracker.held().spans,
            2,
            "the child is forgotten, the parent is open"
        );

        let r4 = stop(&mut tracker, parent, t(9)).expect("the parent stops");
        tracker.apply(gpu_drew(r4, t(9), 10.0));

        let report = settled(&mut events, parent).expect("the parent settled");
        assert_eq!(
            report.total.energy.get(),
            80.0,
            "8 s at 10 W, the child's 2 s included"
        );
        assert_eq!(
            report.exclusive.energy.get(),
            60.0,
            "the child claimed its own 2 s"
        );
    }

    /// AC/ENG-6, AC/ENG-23, AC/PROF-13, AC/PROF-15
    #[test]
    fn a_forgotten_span_is_still_known_to_have_closed() {
        let (mut tracker, mut events) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let closed = stop(&mut tracker, span, t(2)).expect("the first stop cuts");
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        tracker.apply(gpu_drew(closed, t(2), 10.0));
        assert_eq!(tracker.held().spans, 1, "the span is forgotten");
        drain(&mut events);

        assert!(stop(&mut tracker, span, t(3)).is_none());
        assert!(matches!(
            drain(&mut events).as_slice(),
            [TrackerEvent::CommandRefused {
                error: TrackerError::AlreadyClosed(_),
                ..
            }]
        ));
        assert!(matches!(
            ask_profile(&mut tracker, span, t(3)).try_recv(),
            Ok(Err(TrackerError::AlreadyClosed(_)))
        ));

        let late = tracker.session.timeline().next_id();
        assert!(send_start(&mut tracker, late, Some(span), t(3)).is_none());
        assert!(matches!(
            drain(&mut events).as_slice(),
            [TrackerEvent::CommandRefused { span, error: TrackerError::ParentClosed(_) }] if *span == late
        ));

        assert!(matches!(
            ask_profile(&mut tracker, SpanId::new(99), t(3)).try_recv(),
            Ok(Err(TrackerError::UnknownSpan(_)))
        ));
    }

    /// AC/ENG-30, AC/ENG-31, AC/PROF-41
    #[test]
    fn an_open_span_counts_its_samples_after_they_are_dropped() {
        let (mut tracker, _events) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        for at in 2..=10 {
            let mark = TrackerCommand::Mark {
                id: span,
                work: steps(),
                at: t(at),
            };
            let marked = tracker
                .handle_message(TrackerMessage::Command(mark))
                .expect("a mark cuts");
            tracker.apply(gpu_drew(marked, t(at), 10.0));
        }

        assert!(tracker.held().samples <= 2);
        // One sample per second in [1 s, 10 s); the one at 10 s opens the next segment.
        assert_eq!(profile(&mut tracker, span, t(10)).samples[&gpu()], 9);
    }

    fn mark(tracker: &mut Tracker, span: SpanId, work: Work, at: Timestamp) -> Option<ReadoutRequest> {
        tracker.handle_message(TrackerMessage::Command(TrackerCommand::Mark { id: span, work, at }))
    }

    fn refusals(events: &mut Subscription) -> Vec<(SpanId, TrackerError)> {
        drain(events)
            .into_iter()
            .filter_map(|event| match event {
                TrackerEvent::CommandRefused { span, error } => Some((span, error)),
                _ => None,
            })
            .collect()
    }

    /// AC/ENG-3
    #[test]
    fn each_event_kind_is_on_by_default_and_turns_off_alone() {
        let all = Announcements::default();
        assert!(all.readouts && all.cuts && all.settled && all.refusals);

        type Switch = fn(&mut Announcements);
        type Kind = fn(&TrackerEvent) -> bool;
        let kinds: [(Switch, Kind); 4] = [
            (|a| a.readouts = false, |e| matches!(e, TrackerEvent::ReadoutLanded { .. })),
            (|a| a.cuts = false, |e| {
                matches!(e, TrackerEvent::SpanStarted { .. } | TrackerEvent::SpanStopped { .. } | TrackerEvent::Marked { .. })
            }),
            (|a| a.settled = false, |e| matches!(e, TrackerEvent::SpanSettled { .. })),
            (|a| a.refusals = false, |e| matches!(e, TrackerEvent::CommandRefused { .. })),
        ];
        for (off, (switch, _)) in kinds.iter().enumerate() {
            let (mut tracker, mut events) = tracker();
            switch(&mut tracker.announce);
            let (span, opened) = start(&mut tracker, t(1));
            let marked = mark(&mut tracker, span, steps(), t(5)).expect("a mark cuts");
            let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");
            stop(&mut tracker, span, t(12));
            for (request, at) in [(opened, 1), (marked, 5), (closed, 11)] {
                tracker.apply(gpu_drew(request, t(at), 10.0));
            }

            let sent = drain(&mut events);
            for (kind, (_, is_kind)) in kinds.iter().enumerate() {
                assert_eq!(sent.iter().any(is_kind), kind != off, "kind {kind} with kind {off} off");
            }
        }
    }

    /// AC/ENG-5, AC/PROF-15, AC/PROF-16
    #[test]
    fn a_refused_mark_is_announced_with_why() {
        let (mut tracker, mut events) = tracker();
        let unknown = SpanId::new(99);
        assert!(mark(&mut tracker, unknown, steps(), t(1)).is_none());
        assert!(matches!(refusals(&mut events).as_slice(), [(span, TrackerError::UnknownSpan(_))] if *span == unknown));

        let (span, _) = start(&mut tracker, t(2));
        stop(&mut tracker, span, t(3)).expect("the first stop cuts");
        drain(&mut events);
        assert!(mark(&mut tracker, span, steps(), t(4)).is_none());
        assert!(matches!(refusals(&mut events).as_slice(), [(s, TrackerError::AlreadyClosed(_))] if *s == span));
    }

    /// AC/ENG-11
    #[test]
    fn a_started_span_is_announced_with_its_name_parent_selection_and_time() {
        let (mut tracker, mut events) = tracker();
        let (parent, _) = start(&mut tracker, t(1));
        let child = tracker.session.timeline().next_id();
        let selection = SourceSelection::Domains(vec![PowerDomain::Gpu]);
        tracker.handle_message(TrackerMessage::Command(TrackerCommand::Start {
            id: child,
            name: "eval".to_owned(),
            parent: Some(parent),
            sources: selection.clone(),
            at: t(2),
        }));

        let started = drain(&mut events).into_iter().find_map(|event| match event {
            TrackerEvent::SpanStarted { span, name, parent, selection, at } if span == child => {
                Some((name, parent, selection, at))
            }
            _ => None,
        });
        assert_eq!(started, Some(("eval".to_owned(), Some(parent), selection, t(2))));
    }

    /// AC/ENG-12, AC/ENG-13, AC/PROF-9
    #[test]
    fn the_work_reported_goes_out_in_its_event() {
        let (mut tracker, mut events) = tracker();
        let (span, _) = start(&mut tracker, t(1));
        mark(&mut tracker, span, Work::of("samples", 64.0), t(2));
        tracker.handle_message(TrackerMessage::Command(TrackerCommand::Stop {
            id: span,
            work: Work::of("steps", 3.0),
            at: t(3),
        }));

        let work: Vec<(&str, Work, Timestamp)> = drain(&mut events)
            .into_iter()
            .filter_map(|event| match event {
                TrackerEvent::Marked { span: s, work, at } if s == span => Some(("marked", work, at)),
                TrackerEvent::SpanStopped { span: s, work, at } if s == span => Some(("stopped", work, at)),
                _ => None,
            })
            .collect();
        assert_eq!(
            work,
            vec![("marked", Work::of("samples", 64.0), t(2)), ("stopped", Work::of("steps", 3.0), t(3))]
        );
    }

    /// AC/PROF-9, AC/PROF-22
    #[test]
    fn reported_work_weighs_nothing() {
        // Siblings a and b share 1 s to 11 s at 10 W; only a marks, with work.
        let billed = |work: Work| {
            let (mut tracker, mut events) = tracker();
            let (a, r1) = start(&mut tracker, t(1));
            let (b, r2) = start(&mut tracker, t(1));
            let r3 = mark(&mut tracker, a, work, t(6)).expect("a mark cuts");
            let r4 = stop(&mut tracker, a, t(11)).expect("a stops");
            let r5 = stop(&mut tracker, b, t(11)).expect("b stops");
            for (request, at) in [(r1, 1), (r2, 1), (r3, 6), (r4, 11), (r5, 11)] {
                tracker.apply(gpu_drew(request, t(at), 10.0));
            }
            let sent = drain(&mut events);
            let energy_of = |span| {
                sent.iter()
                    .find_map(|event| match event {
                        TrackerEvent::SpanSettled { report } if report.span_id == span => Some(energy(report)),
                        _ => None,
                    })
                    .expect("settled")
            };
            (energy_of(a), energy_of(b))
        };
        assert_eq!(billed(Work::of("tokens", 1.0)), (50.0, 50.0));
        assert_eq!(billed(Work::of("tokens", 1e6)), (50.0, 50.0));
        assert_eq!(billed(Work::default()), (50.0, 50.0));
    }

    /// AC/ENG-19
    #[test]
    fn a_query_answers_what_is_accounted_now_whenever_it_names() {
        let (mut tracker, _) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let marked = mark(&mut tracker, span, steps(), t(11)).expect("a mark cuts");
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        tracker.apply(gpu_drew(marked, t(11), 10.0));

        let (now, later) = (profile(&mut tracker, span, t(11)), profile(&mut tracker, span, t(3600)));
        assert_eq!(now.per_source, later.per_source);
        assert_eq!(energy(&now), 100.0);
        assert_eq!(session_report(&mut tracker, t(11)).total, session_report(&mut tracker, t(3600)).total);
    }

    /// AC/PROF-17
    #[test]
    fn after_finish_every_command_is_refused_as_closed() {
        let (mut tracker, mut events) = tracker();
        let (span, _) = start(&mut tracker, t(1));
        let (reply, _answer) = oneshot::channel();
        assert!(tracker.handle_message(TrackerMessage::Finish { at: t(5), reply }).is_some());
        drain(&mut events);

        let late = SpanId::new(7);
        assert!(send_start(&mut tracker, late, None, t(6)).is_none());
        assert!(mark(&mut tracker, span, steps(), t(6)).is_none());
        assert!(stop(&mut tracker, span, t(6)).is_none());
        let refused = refusals(&mut events);
        assert_eq!(refused.len(), 3);
        assert!(refused.iter().all(|(_, error)| matches!(error, TrackerError::Closed)), "{refused:?}");
    }

    /// AC/PROF-18
    #[test]
    fn a_refused_start_s_id_is_already_closed() {
        let (mut tracker, mut events) = tracker();
        let orphan = tracker.session.timeline().next_id();
        assert!(send_start(&mut tracker, orphan, Some(SpanId::new(99)), t(1)).is_none());
        let unknown_source = SpanId::new(orphan.get() + 1);
        tracker.handle_message(TrackerMessage::Command(TrackerCommand::Start {
            id: unknown_source,
            name: "work".to_owned(),
            parent: None,
            sources: SourceSelection::Sources(vec![SourceId::new(ProviderId(9), 0)]),
            at: t(1),
        }));
        assert!(matches!(
            refusals(&mut events).as_slice(),
            [(_, TrackerError::UnknownSpan(_)), (_, TrackerError::UnknownSource(_))]
        ));

        for span in [orphan, unknown_source] {
            assert!(stop(&mut tracker, span, t(2)).is_none());
            assert!(mark(&mut tracker, span, steps(), t(2)).is_none());
            let refused = refusals(&mut events);
            assert!(
                refused.iter().all(|(s, error)| *s == span && matches!(error, TrackerError::AlreadyClosed(_)))
                    && refused.len() == 2,
                "{refused:?}"
            );
        }
    }

    /// AC/PROF-34
    #[test]
    fn an_open_span_s_report_waits_for_its_latest_cut_and_no_longer() {
        let (mut tracker, _) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        tracker.apply(gpu_drew(opened, t(1), 10.0));
        let marked = mark(&mut tracker, span, steps(), t(11)).expect("a mark cuts");

        let mut answer = ask_profile(&mut tracker, span, t(11));
        assert!(answer.try_recv().is_err(), "the mark's readout is still out");
        tracker.apply(gpu_drew(marked, t(11), 10.0));
        let report = answer.try_recv().expect("answered as it lands").expect("the span is open");
        assert_eq!(energy(&report), 100.0);
    }

    /// AC/PROF-35
    #[test]
    fn session_stats_answer_at_once_with_what_is_accounted() {
        let (mut tracker, _) = tracker();
        let (span, opened) = start(&mut tracker, t(1));
        let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");
        tracker.apply(gpu_drew(opened, t(1), 10.0));

        assert_eq!(session_report(&mut tracker, t(11)).claimed.energy.get(), 0.0, "the stop is still out");
        tracker.apply(gpu_drew(closed, t(11), 10.0));
        assert_eq!(session_report(&mut tracker, t(11)).claimed.energy.get(), 100.0);
    }

    /// AC/PROF-5
    #[test]
    fn a_span_that_selects_nothing_here_asks_no_provider() {
        let (mut tracker, _) = tracker();
        let span = tracker.session.timeline().next_id();
        let start = TrackerCommand::Start {
            id: span,
            name: "ane".to_owned(),
            parent: None,
            sources: SourceSelection::Domains(vec![PowerDomain::Ane]),
            at: t(1),
        };
        assert!(tracker.handle_message(TrackerMessage::Command(start)).is_none());
        assert!(mark(&mut tracker, span, steps(), t(2)).is_none());
        assert!(stop(&mut tracker, span, t(3)).is_none());
    }

    /// AC/MEAS-2
    #[test]
    fn subscriptions_queries_and_refused_commands_send_no_request() {
        let (mut tracker, _) = tracker();
        let (subscriber, _events) = subscription(4);
        assert!(tracker.handle_message(TrackerMessage::Subscribe(subscriber)).is_none());
        let (reply, _) = oneshot::channel();
        assert!(tracker.handle_message(TrackerMessage::Query(TrackerQuery::SessionStats { at: t(1), reply })).is_none());
        let (reply, _) = oneshot::channel();
        assert!(tracker.handle_message(TrackerMessage::Query(TrackerQuery::Catalog { reply })).is_none());
        ask_profile(&mut tracker, SpanId::new(99), t(1));

        assert!(send_start(&mut tracker, SpanId::new(1), Some(SpanId::new(99)), t(1)).is_none());
        assert!(stop(&mut tracker, SpanId::new(98), t(1)).is_none());
        assert!(mark(&mut tracker, SpanId::new(98), steps(), t(1)).is_none());
    }

    /// AC/MEAS-21, AC/MEAS-28, AC/MEAS-69
    #[test]
    fn every_observation_but_samples_is_accounted_alike() {
        let closing = [
            Observation::Unchanged,
            Observation::Skipped(Skip::Cooldown),
            Observation::Skipped(Skip::Busy),
            Observation::Failed(Failure::TimedOut),
            Observation::Failed(Failure::Stalled),
            Observation::Failed(Failure::Unanswered),
            Observation::Failed(Failure::Retired),
            Observation::Failed(Failure::Provider(ProviderError::Unavailable("gone".to_owned()))),
        ];
        for observation in closing {
            let (mut tracker, mut events) = tracker();
            let (span, opened) = start(&mut tracker, t(1));
            let closed = stop(&mut tracker, span, t(11)).expect("the first stop cuts");
            tracker.apply(gpu_drew(opened, t(1), 10.0));
            let mut response = ReadoutResponse::new(closed.id);
            response.observe(gpu(), observation.clone());
            tracker.apply(response);

            let report = settled(&mut events, span).expect("the stop was answered");
            assert_eq!(energy(&report), 100.0, "10 W held over 10 s, closing on {observation:?}");
            assert_eq!(tracker.held().samples, 1, "nothing stored for {observation:?}");
        }
    }

    /// AC/MEAS-84
    #[test]
    fn a_source_no_span_cuts_is_held_and_accounted_at_finish() {
        let (mut tracker, _events) = tracker();
        for s in 1..=5 {
            let mut watchdog = ReadoutResponse::new(ReadoutId(900 + s));
            watchdog.read(gpu(), vec![Sample::new(t(s as i64), 10.0)]);
            tracker.apply(watchdog);
        }
        assert_eq!(tracker.held().samples, 5, "every sample since the opening cut");

        let (reply, mut answer) = oneshot::channel();
        let last = tracker
            .handle_message(TrackerMessage::Finish { at: t(6), reply })
            .expect("finish cuts every source");
        assert!(answer.try_recv().is_err(), "the last cut is still out");
        tracker.apply(gpu_drew(last, t(6), 10.0));
        let report = answer.try_recv().expect("answered").expect("finished");
        assert_eq!(report.total.energy.get(), 60.0, "10 W from the opening at 0 s to 6 s");
        assert_eq!(report.unclaimed.energy.get(), 60.0);
    }
}
