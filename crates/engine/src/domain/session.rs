//! The [Session]: everything the tracker has recorded since it started.

use std::collections::{HashMap, HashSet, VecDeque};

use super::accounting::{PowerUsageEffectiveness, Usage, bill, count_samples};
use super::readout::ReadoutId;
use super::sample_store::SampleStore;
use super::source::{SourceCatalog, SourceDescription, SourceSelection};
use super::timeline::{CutPlaced, Segment, Timeline};
use super::{SourceId, SpanId, Timestamp, TrackerError};

/// One request to one provider.
pub type RequestId = ReadoutId;

/// Which request a cut relies on for each source it asks.
pub trait Relying {
    /// The request that answers a cut stamped `at` for `source`; `None` if
    /// it needs none.
    fn relied(&mut self, source: SourceId, at: Timestamp) -> Option<RequestId>;

    /// Whether `request` was answered and its readout applied before this
    /// cut, so nothing waits on it.
    fn is_answered(&self, _request: RequestId) -> bool {
        false
    }
}

/// Every source on one request.
impl Relying for ReadoutId {
    fn relied(&mut self, _: SourceId, _: Timestamp) -> Option<RequestId> {
        Some(*self)
    }
}

impl<R: Relying + ?Sized> Relying for &mut R {
    fn relied(&mut self, source: SourceId, at: Timestamp) -> Option<RequestId> {
        (**self).relied(source, at)
    }

    fn is_answered(&self, request: RequestId) -> bool {
        (**self).is_answered(request)
    }
}

/// Each source on its own request; a source left out needs none.
#[cfg(test)]
pub struct Relied {
    pub requests: HashMap<SourceId, RequestId>,
    /// Requests already answered.
    pub answered: HashSet<RequestId>,
}

#[cfg(test)]
impl Relying for Relied {
    fn relied(&mut self, source: SourceId, _: Timestamp) -> Option<RequestId> {
        self.requests.get(&source).copied()
    }

    fn is_answered(&self, request: RequestId) -> bool {
        self.answered.contains(&request)
    }
}

/// A segment cut but not accounted, with the requests that answer its end.
struct Waiting {
    segment: Segment,
    power: Option<RequestId>,
    intensity: Option<RequestId>,
}

/// The catalog, the spans and the cuts between them, and what each span has
/// been billed.
///
/// Each source's segments are accounted in order, each once its source and
/// that source's intensity have answered the cut that ends it, then measured
/// and billed once (see [bill]) and dropped. Ledgers are only ever added to.
///
/// A settled span can be [forgotten](Session::forget): what it and its
/// descendants were billed is folded into its parent, and the rest dropped.
/// So the session holds only open spans, closed ones not yet forgotten, and
/// totals.
pub struct Session {
    catalog: SourceCatalog,
    pue: PowerUsageEffectiveness,
    timeline: Timeline,
    /// Per power source, its segments cut but not accounted, oldest first.
    waiting: HashMap<SourceId, VecDeque<Waiting>>,
    /// Requests answered that a waiting segment still names.
    answered: HashSet<RequestId>,
    /// Per span, the segments it was active in that are not accounted yet.
    unaccounted: HashMap<SpanId, usize>,
    /// Closed spans not yet announced as settled.
    closing: HashSet<SpanId>,
    /// Spans closed, or left with nothing unaccounted, since the last
    /// accounting pass: only they and their ancestors can have settled.
    touched: HashSet<SpanId>,
    /// What each span is billed for itself, per source.
    ledgers: HashMap<SpanId, HashMap<SourceId, Usage>>,
    /// Per span, what its forgotten descendants were billed, per source.
    folded: HashMap<SpanId, HashMap<SourceId, Usage>>,
    /// Per span, how many samples each source had in the segments it was
    /// active in. Counted as each segment is accounted, since the samples
    /// themselves are not kept.
    samples: HashMap<SpanId, HashMap<SourceId, usize>>,
    /// Every segment accounted, and what each claimant was billed, in order.
    #[cfg(feature = "replay")]
    accounted: Vec<(Segment, Vec<(SpanId, Usage)>)>,
}

impl Session {
    /// Open a session over `catalog` at `opened_at`, scaling all usage by `pue`.
    pub fn new(catalog: SourceCatalog, pue: PowerUsageEffectiveness, opened_at: Timestamp) -> Self {
        Self {
            timeline: Timeline::open(opened_at, &catalog.descriptions),
            catalog,
            pue,
            waiting: HashMap::new(),
            answered: HashSet::new(),
            unaccounted: HashMap::new(),
            closing: HashSet::new(),
            touched: HashSet::new(),
            ledgers: HashMap::new(),
            folded: HashMap::new(),
            samples: HashMap::new(),
            #[cfg(feature = "replay")]
            accounted: Vec::new(),
        }
    }

    /// Open span `id` at a cut that relies on `relied`.
    ///
    /// # Errors
    ///
    /// [TrackerError::UnknownSource] if `selection` names a source that is
    /// not a power source in the catalog; otherwise as [Timeline::start]. Then no cut is
    /// placed; `id` still counts as handed out.
    pub fn start(
        &mut self,
        id: SpanId,
        name: String,
        parent: Option<SpanId>,
        selection: SourceSelection,
        at: Timestamp,
        relied: impl Relying,
    ) -> Result<(), TrackerError> {
        if let SourceSelection::Sources(sources) = &selection
            && let Some(&unknown) = sources.iter().find(|source| {
                !matches!(self.catalog.descriptions.get(source), Some(SourceDescription::Power { .. }))
            })
        {
            self.timeline.handed_out(id);
            return Err(TrackerError::UnknownSource(unknown));
        }
        let placed = self.timeline.start(id, name, parent, selection, at)?;
        self.placed(relied, placed);
        Ok(())
    }

    /// [Session::start] with the next id, as the handle would hand it out.
    #[cfg(test)]
    pub fn start_next(
        &mut self,
        name: String,
        parent: Option<SpanId>,
        selection: SourceSelection,
        at: Timestamp,
        relied: impl Relying,
    ) -> Result<SpanId, TrackerError> {
        let id = self.timeline.next_id();
        self.start(id, name, parent, selection, at, relied).map(|()| id)
    }

    /// Close `span` and its open descendants at a cut that relies on `relied`.
    ///
    /// # Errors
    ///
    /// As [Timeline::stop]; then no cut is placed.
    pub fn stop(&mut self, span: SpanId, at: Timestamp, relied: impl Relying) -> Result<(), TrackerError> {
        let placed = self.timeline.stop(span, at)?;
        self.placed(relied, placed);
        Ok(())
    }

    /// Cut at `at`, relying on `relied`, for work reported on `span`.
    ///
    /// # Errors
    ///
    /// As [Timeline::mark]; then no cut is placed.
    pub fn mark(&mut self, span: SpanId, at: Timestamp, relied: impl Relying) -> Result<(), TrackerError> {
        let placed = self.timeline.mark(span, at)?;
        self.placed(relied, placed);
        Ok(())
    }

    /// Place the session's last cut at `at`, relying on `relied`, closing
    /// every open span.
    pub fn finish(&mut self, at: Timestamp, relied: impl Relying) {
        let placed = self.timeline.finish(at);
        self.placed(relied, placed);
    }

    /// Whether every segment cut so far has been accounted.
    pub fn is_drained(&self) -> bool {
        self.waiting.values().all(VecDeque::is_empty)
    }

    /// Queue each segment the cut finished behind the requests it relies on.
    fn placed(&mut self, mut relied: impl Relying, placed: CutPlaced) {
        for segment in placed.segments {
            for &span in &segment.active {
                *self.unaccounted.entry(span).or_default() += 1;
            }
            let source = segment.source;
            // A request answered before this cut is waited on by nobody.
            let mut wait_on = |source: SourceId| relied.relied(source, placed.at).filter(|&r| !relied.is_answered(r));
            let power = wait_on(source);
            let intensity = self.catalog.intensity_of(source).and_then(&mut wait_on);
            let waiting = Waiting { power, intensity, segment };
            self.waiting.entry(source).or_default().push_back(waiting);
        }
        self.touched.extend(&placed.closed);
        self.closing.extend(placed.closed);
    }

    /// Request `request` has been answered, and its samples are already in
    /// `sample_store`.
    ///
    /// Accounts, per source and in order, every segment whose requests have
    /// now all been answered, and returns the closed spans that just settled.
    pub fn land(&mut self, request: RequestId, sample_store: &SampleStore) -> Vec<SpanId> {
        let named = self.waiting.values().flatten().any(|w| w.power == Some(request) || w.intensity == Some(request));
        if named {
            self.answered.insert(request);
        }
        self.account_ready(sample_store)
    }

    /// How many answered requests are held.
    #[cfg(test)]
    pub fn answered_held(&self) -> usize {
        self.answered.len()
    }

    /// How many per-source usages the ledgers and folded totals hold.
    #[cfg(test)]
    pub fn ledger_entries(&self) -> usize {
        self.ledgers.values().chain(self.folded.values()).map(HashMap::len).sum()
    }

    /// Account every segment whose requests have all been answered, as of
    /// `sample_store`, and return the closed spans that just settled. A cut
    /// that relies on requests already answered is accounted here.
    pub fn account_ready(&mut self, sample_store: &SampleStore) -> Vec<SpanId> {
        let answered = |request: Option<RequestId>, set: &HashSet<RequestId>| request.is_none_or(|r| set.contains(&r));

        let mut ready = Vec::new();
        for queue in self.waiting.values_mut() {
            while queue
                .front()
                .is_some_and(|w| answered(w.power, &self.answered) && answered(w.intensity, &self.answered))
            {
                ready.push(queue.pop_front().expect("checked").segment);
            }
        }
        for segment in ready {
            self.account(&segment, sample_store);
        }
        // Forget answers no waiting segment names any more.
        if !self.answered.is_empty() {
            let named: HashSet<RequestId> =
                self.waiting.values().flatten().flat_map(|w| [w.power, w.intensity]).flatten().collect();
            self.answered.retain(|request| named.contains(request));
        }

        // A child settles before, or with, its parent: a parent waits for a
        // child still out on a source the parent did not select.
        let mut candidates = HashSet::new();
        for span in std::mem::take(&mut self.touched) {
            let mut current = Some(span);
            while let Some(id) = current {
                if !candidates.insert(id) {
                    break;
                }
                current = self.timeline.parent_of(id);
            }
        }
        let settled: Vec<SpanId> = candidates
            .into_iter()
            .filter(|span| self.closing.contains(span) && self.is_subtree_settled(*span))
            .collect();
        for span in &settled {
            self.closing.remove(span);
        }
        settled
    }

    /// Bill `segment` and count its samples, once.
    fn account(&mut self, segment: &Segment, sample_store: &SampleStore) {
        let billed = bill(&self.catalog, sample_store, segment, self.pue);
        for &(span, amount) in &billed {
            *self.ledgers.entry(span).or_default().entry(segment.source).or_default() += amount;
        }
        #[cfg(feature = "replay")]
        self.accounted.push((segment.clone(), billed));
        for (source, count) in count_samples(&self.catalog, sample_store, segment) {
            for &span in &segment.active {
                *self.samples.entry(span).or_default().entry(source).or_default() += count;
            }
        }
        for span in &segment.active {
            let count = self.unaccounted.get_mut(span).expect("counted when the segment was cut");
            *count -= 1;
            if *count == 0 {
                self.touched.insert(*span);
            }
        }
    }

    /// Whether `span` and every held descendant are settled. Settled
    /// descendants are forgotten, so the walk stops at the first one still out.
    fn is_subtree_settled(&self, span: SpanId) -> bool {
        let mut pending = vec![span];
        while let Some(span) = pending.pop() {
            if !self.is_settled(span) {
                return false;
            }
            pending.extend(self.timeline.children_of(span));
        }
        true
    }

    /// The segments accounted since last asked, with what each claimant
    /// was billed.
    #[cfg(feature = "replay")]
    pub fn take_accounted(&mut self) -> Vec<(Segment, Vec<(SpanId, Usage)>)> {
        std::mem::take(&mut self.accounted)
    }

    pub fn catalog(&self) -> &SourceCatalog {
        &self.catalog
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    /// Whether nothing billed to `span` is still waiting on a readout.
    ///
    /// An open span is settled up to its latest cut. An unknown span has
    /// nothing pending, so it is settled.
    pub fn is_settled(&self, span: SpanId) -> bool {
        self.unaccounted.get(&span).is_none_or(|&count| count == 0)
    }

    /// What `span` is billed for itself, per source: not its descendants.
    /// Empty for an unknown span or one billed nothing yet.
    pub fn ledger(&self, span: SpanId) -> HashMap<SourceId, Usage> {
        self.ledgers.get(&span).cloned().unwrap_or_default()
    }

    /// What `span` and every descendant were billed, per source, forgotten
    /// descendants included.
    pub fn subtree(&self, span: SpanId) -> HashMap<SourceId, Usage> {
        let mut total = self.ledger(span);
        add(&mut total, Some(&self.descendants(span)));
        total
    }

    /// What `span`'s descendants were billed, per source, forgotten ones
    /// included. Only living descendants are walked; forgotten ones are
    /// already folded into a living ancestor.
    pub fn descendants(&self, span: SpanId) -> HashMap<SourceId, Usage> {
        let mut total = HashMap::new();
        add(&mut total, self.folded.get(&span));
        for id in self.timeline.descendants_of(span) {
            add(&mut total, self.ledgers.get(&id));
            add(&mut total, self.folded.get(&id));
        }
        total
    }

    /// Per source, the samples `span` had in the segments it was active in,
    /// as accounted so far: its power sources and their intensities.
    pub fn samples(&self, span: SpanId) -> HashMap<SourceId, usize> {
        self.samples.get(&span).cloned().unwrap_or_default()
    }

    /// Forget a closed, settled span: fold what it and its descendants were
    /// billed into its parent, and drop everything else about it.
    ///
    /// Its descendants must be forgotten first, or their usage would be
    /// folded into a span that is gone. A child's id is always higher than
    /// its parent's, so forgetting in descending id order is enough.
    pub fn forget(&mut self, span: SpanId) {
        debug_assert!(self.is_settled(span), "forgot {span:?} before it settled");
        let Some(record) = self.timeline.forget(span) else {
            return;
        };
        let parent = record
            .parent
            .expect("only the root has no parent, and it never closes");

        let mut subtree = self.ledgers.remove(&span).unwrap_or_default();
        add(&mut subtree, self.folded.remove(&span).as_ref());
        add(self.folded.entry(parent).or_default(), Some(&subtree));

        self.unaccounted.remove(&span);
        self.samples.remove(&span);
    }

    /// Per series, the earliest time a segment still to be accounted reads
    /// it from. A power series is read from its oldest unaccounted segment's
    /// start, or its segment in progress; an intensity series from the
    /// earliest of the power sources paired with it. Older samples (but those
    /// just before) are no longer needed; a series left out needs only its
    /// latest.
    pub fn needed_from_each(&self) -> HashMap<SourceId, Timestamp> {
        let mut cutoffs: HashMap<SourceId, Timestamp> = HashMap::new();
        for &source in self.catalog.descriptions.keys() {
            let Some(head) = self.timeline.head(source) else { continue };
            let from = self
                .waiting
                .get(&source)
                .and_then(|queue| queue.front())
                .map_or(head, |waiting| waiting.segment.from);
            cutoffs.insert(source, from);
        }
        for (&power, &intensity) in &self.catalog.pairs {
            if let Some(&from) = cutoffs.get(&power) {
                let entry = cutoffs.entry(intensity).or_insert(from);
                *entry = (*entry).min(from);
            }
        }
        cutoffs
    }
}

/// Add each source's usage in `from` to `into`.
fn add(into: &mut HashMap<SourceId, Usage>, from: Option<&HashMap<SourceId, Usage>>) {
    for (&source, &usage) in from.into_iter().flatten() {
        *into.entry(source).or_default() += usage;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Fidelity, PowerDomain, PowerScope, ProviderId, Sample, SampleKind, SourceDescription,
        SourceKind, TimeInterval,
    };

    /// `secs` seconds after the epoch.
    fn t(secs: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(secs)
    }

    fn cpu() -> SourceId {
        SourceId::new(ProviderId(1), 0)
    }

    fn gpu() -> SourceId {
        SourceId::new(ProviderId(1), 1)
    }

    /// [cpu] and [gpu], read linearly, with no intensity series.
    fn catalog() -> SourceCatalog {
        let power = |domain| SourceDescription::Power {
            device_name: format!("{domain:?}"),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Stated,
        };
        SourceCatalog {
            descriptions: HashMap::from([
                (cpu(), power(PowerDomain::Cpu)),
                (gpu(), power(PowerDomain::Gpu)),
            ]),
            ..SourceCatalog::default()
        }
    }

    /// Empty, with one appending series per source in `catalog`.
    fn sample_store(catalog: &SourceCatalog) -> SampleStore {
        SampleStore::new_from_source(&catalog.descriptions, 64)
            .expect("the keys of a map are distinct")
    }

    /// A session over a CPU and a GPU, with no samples yet.
    fn session() -> (Session, SampleStore) {
        let catalog = catalog();
        let sample_store = sample_store(&catalog);
        (
            Session::new(catalog, PowerUsageEffectiveness::default(), t(0)),
            sample_store,
        )
    }

    /// Readout `n` landed: every source drew the given watts at `at`.
    fn land(
        session: &mut Session,
        sample_store: &mut SampleStore,
        n: usize,
        at: i64,
        cpu_w: f64,
        gpu_w: f64,
    ) -> Vec<SpanId> {
        sample_store
            .store(cpu(), vec![Sample::new(t(at), cpu_w)])
            .unwrap();
        sample_store
            .store(gpu(), vec![Sample::new(t(at), gpu_w)])
            .unwrap();
        session.land(ReadoutId(n), sample_store)
    }

    fn start(
        session: &mut Session,
        parent: Option<SpanId>,
        selection: SourceSelection,
        at: i64,
        n: usize,
    ) -> SpanId {
        session
            .start_next("work".to_owned(), parent, selection, t(at), ReadoutId(n))
            .unwrap()
    }

    fn energy(session: &Session, span: SpanId, source: SourceId) -> f64 {
        session
            .ledger(span)
            .get(&source)
            .map_or(0.0, |a| a.energy.get())
    }

    /// AC/MEAS-81
    #[test]
    fn ten_watts_for_ten_seconds_is_billed_once_the_stop_lands() {
        let (mut session, mut sample_store) = session();
        let span = start(&mut session, None, SourceSelection::All, 1, 1);
        session.stop(span, t(11), ReadoutId(2)).unwrap();

        land(&mut session, &mut sample_store, 1, 1, 10.0, 0.0);
        assert_eq!(energy(&session, span, cpu()), 0.0);
        assert!(!session.is_settled(span));

        let settled = land(&mut session, &mut sample_store, 2, 11, 10.0, 0.0);
        assert_eq!(energy(&session, span, cpu()), 100.0);
        assert_eq!(settled, vec![span]);
    }

    /// AC/PROF-29, AC/PROF-32
    #[test]
    fn a_span_waits_for_a_cut_another_span_made() {
        let (mut session, mut sample_store) = session();
        let first = start(&mut session, None, SourceSelection::All, 1, 1);
        start(&mut session, None, SourceSelection::All, 6, 2);
        session.stop(first, t(11), ReadoutId(3)).unwrap();

        land(&mut session, &mut sample_store, 1, 1, 10.0, 0.0);
        assert!(
            land(&mut session, &mut sample_store, 3, 11, 10.0, 0.0).is_empty(),
            "the segment cut at 6 s is not in"
        );
        assert!(!session.is_settled(first));

        assert_eq!(
            land(&mut session, &mut sample_store, 2, 6, 10.0, 0.0),
            vec![first]
        );
        assert_eq!(
            energy(&session, first, cpu()),
            75.0,
            "alone for 5 s, then half of 5 s"
        );
    }

    /// AC/ENG-20, AC/PROF-34
    #[test]
    fn an_open_span_is_settled_up_to_its_latest_cut_but_never_announced() {
        let (mut session, mut sample_store) = session();
        let span = start(&mut session, None, SourceSelection::All, 1, 1);
        session.mark(span, t(11), ReadoutId(2)).unwrap();

        land(&mut session, &mut sample_store, 1, 1, 10.0, 0.0);
        assert!(land(&mut session, &mut sample_store, 2, 11, 10.0, 0.0).is_empty());
        assert!(session.is_settled(span));
        assert_eq!(energy(&session, span, cpu()), 100.0);
    }

    /// AC/MEAS-66
    #[test]
    fn a_watchdog_readout_finishes_no_segment() {
        let (mut session, mut sample_store) = session();
        let span = start(&mut session, None, SourceSelection::All, 1, 1);
        session.stop(span, t(11), ReadoutId(3)).unwrap();
        land(&mut session, &mut sample_store, 1, 1, 10.0, 0.0);

        assert!(land(&mut session, &mut sample_store, 2, 6, 10.0, 0.0).is_empty());
        assert!(!session.is_settled(span));
    }

    /// AC/MEAS-13, AC/MEAS-74, AC/MEAS-89
    #[test]
    fn an_empty_readout_settles_its_segment_on_the_last_value_and_never_revisits_it() {
        let (mut session, mut sample_store) = session();
        let span = start(&mut session, None, SourceSelection::All, 1, 1);
        session.stop(span, t(11), ReadoutId(2)).unwrap();

        land(&mut session, &mut sample_store, 1, 1, 10.0, 0.0);
        // The stop's readout comes back with no samples, as a provider asked
        // sooner than its minimal interval answers.
        let settled = session.land(ReadoutId(2), &sample_store);

        assert_eq!(settled, vec![span]);
        assert_eq!(
            energy(&session, span, cpu()),
            100.0,
            "10 W held from 1 s to 11 s"
        );

        // A later sample shows the draw was climbing, but the segment is
        // already billed.
        sample_store
            .store(cpu(), vec![Sample::new(t(21), 30.0)])
            .unwrap();
        assert_eq!(
            sample_store
                .energy(cpu(), &TimeInterval::new(t(1), t(11)).unwrap())
                .unwrap()
                .get(),
            150.0
        );
        assert_eq!(energy(&session, span, cpu()), 100.0);
    }

    /// AC/MEAS-75
    #[test]
    fn a_source_that_never_reported_is_billed_to_no_one() {
        let (mut session, mut sample_store) = session();
        let span = start(&mut session, None, SourceSelection::All, 1, 1);
        session.stop(span, t(11), ReadoutId(2)).unwrap();

        for (n, at) in [(1, 1), (2, 11)] {
            sample_store
                .store(cpu(), vec![Sample::new(t(at), 10.0)])
                .unwrap();
            session.land(ReadoutId(n), &sample_store);
        }

        assert_eq!(energy(&session, span, cpu()), 100.0);
        assert!(!session.ledger(span).contains_key(&gpu()));
        assert!(
            !session.ledger(SpanId::ROOT).contains_key(&gpu()),
            "not even the root takes it"
        );
    }

    /// AC/PROF-8, AC/PROF-20, AC/PROF-21
    #[test]
    fn a_child_over_the_cpu_leaves_its_parent_the_gpu() {
        let (mut session, mut sample_store) = session();
        let parent = start(&mut session, None, SourceSelection::All, 1, 1);
        let child = start(
            &mut session,
            Some(parent),
            SourceSelection::Domains(vec![PowerDomain::Cpu]),
            1,
            2,
        );
        session.stop(parent, t(11), ReadoutId(3)).unwrap();

        land(&mut session, &mut sample_store, 1, 1, 4.0, 10.0);
        land(&mut session, &mut sample_store, 2, 1, 4.0, 10.0);
        let mut settled = land(&mut session, &mut sample_store, 3, 11, 4.0, 10.0);
        settled.sort();

        assert_eq!(energy(&session, child, cpu()), 40.0);
        assert_eq!(energy(&session, child, gpu()), 0.0);
        assert_eq!(energy(&session, parent, gpu()), 100.0);
        assert_eq!(energy(&session, parent, cpu()), 0.0);
        assert_eq!(
            settled,
            vec![parent, child],
            "stopping the parent closed the child too"
        );
    }

    /// AC/MEAS-2, AC/PROF-11, AC/PROF-12
    #[test]
    fn a_refused_start_awaits_no_readout() {
        let (mut session, mut sample_store) = session();
        let refused = session.start_next(
            "child".to_owned(),
            Some(SpanId::new(99)),
            SourceSelection::All,
            t(1),
            ReadoutId(1),
        );
        assert!(matches!(refused, Err(TrackerError::UnknownSpan(_))));

        let span = start(&mut session, None, SourceSelection::All, 2, 2);
        session.stop(span, t(12), ReadoutId(3)).unwrap();
        land(&mut session, &mut sample_store, 2, 2, 10.0, 0.0);
        assert_eq!(
            land(&mut session, &mut sample_store, 3, 12, 10.0, 0.0),
            vec![span]
        );
    }

    /// AC/PROF-29, AC/PROF-52
    #[test]
    fn a_span_is_billed_the_whole_draw_while_it_runs_and_the_root_the_rest() {
        // Held samples, so the GPU steps from 20 W to 120 W and back exactly.
        let mut sample_store = SampleStore::new(
            [
                (cpu(), SourceKind::Power, SampleKind::Instant),
                (gpu(), SourceKind::Power, SampleKind::Held),
            ],
            8,
        )
        .unwrap();
        let mut session = Session::new(catalog(), PowerUsageEffectiveness::default(), t(0));
        let gpu_only = SourceSelection::Domains(vec![PowerDomain::Gpu]);

        let a = start(&mut session, None, gpu_only, 10, 1);
        session.stop(a, t(20), ReadoutId(2)).unwrap();
        start(&mut session, None, SourceSelection::All, 30, 3); // cuts the idle stretch after A
        for (readout, at, watts) in [(0, 0, 20.0), (1, 10, 120.0), (2, 20, 20.0), (3, 30, 20.0)] {
            sample_store
                .store(gpu(), vec![Sample::new(t(at), watts)])
                .unwrap();
            session.land(ReadoutId(readout), &sample_store);
        }

        // A's 10 s at 120 W, baseline included: nothing is subtracted.
        assert_eq!(energy(&session, a, gpu()), 1200.0);
        // 10 s at 20 W before A and 10 s after.
        assert_eq!(energy(&session, SpanId::ROOT, gpu()), 400.0);

        let report = session.session_report(t(30));
        assert_eq!(report.total.energy.get(), 1600.0);
        assert_eq!(report.claimed.energy.get(), 1200.0);
        assert_eq!(report.unclaimed.energy.get(), 400.0);
    }

    fn r(n: usize) -> ReadoutId {
        ReadoutId(n)
    }

    fn dk1() -> SourceId {
        SourceId::new(ProviderId(3), 0)
    }

    /// [catalog] with the CPU on `dk1` and the GPU unpaired.
    fn paired() -> (Session, SampleStore) {
        let mut catalog = catalog();
        catalog.descriptions.insert(
            dk1(),
            SourceDescription::Intensity { location: crate::domain::GridLocation::Global, fidelity: Fidelity::Stated },
        );
        catalog.pairs = HashMap::from([(cpu(), dk1())]);
        let sample_store = sample_store(&catalog);
        (Session::new(catalog, PowerUsageEffectiveness::default(), t(0)), sample_store)
    }

    fn relied(pairs: &[(SourceId, usize)]) -> Relied {
        Relied { requests: pairs.iter().map(|&(source, n)| (source, r(n))).collect(), answered: HashSet::new() }
    }

    fn cpu_only() -> SourceSelection {
        SourceSelection::Domains(vec![PowerDomain::Cpu])
    }

    fn gpu_only() -> SourceSelection {
        SourceSelection::Domains(vec![PowerDomain::Gpu])
    }

    /// AC/MEAS-4, AC/MEAS-81, AC/PROF-32
    #[test]
    fn a_segment_waits_for_its_source_and_its_intensity() {
        let (mut session, sample_store) = paired();
        let span = session.start_next("a".to_owned(), None, cpu_only(), t(0), relied(&[(cpu(), 1), (dk1(), 2)])).unwrap();
        session.stop(span, t(1), relied(&[(cpu(), 3), (dk1(), 4)])).unwrap();
        session.land(r(1), &sample_store);
        session.land(r(2), &sample_store);
        assert!(session.land(r(3), &sample_store).is_empty());
        assert_eq!(session.land(r(4), &sample_store), vec![span]);
    }

    /// AC/MEAS-81
    #[test]
    fn a_source_with_no_intensity_waits_only_for_itself() {
        let (mut session, sample_store) = paired();
        let span = session.start_next("g".to_owned(), None, gpu_only(), t(0), relied(&[(gpu(), 1)])).unwrap();
        session.stop(span, t(1), relied(&[(gpu(), 2)])).unwrap();
        session.land(r(1), &sample_store);
        assert_eq!(session.land(r(2), &sample_store), vec![span]);
    }

    /// AC/MEAS-81, AC/TST-14
    #[test]
    fn a_segment_is_not_accounted_before_an_earlier_one_on_its_source() {
        let (mut session, sample_store) = session();
        let span = start(&mut session, None, gpu_only(), 1, 1);
        session.mark(span, t(2), r(2)).unwrap();
        session.stop(span, t(3), r(3)).unwrap();
        session.land(r(1), &sample_store);
        assert!(session.land(r(3), &sample_store).is_empty(), "the segment ending at 2 is not in");
        assert!(!session.is_settled(span));
        assert_eq!(session.land(r(2), &sample_store), vec![span], "both are accounted together");
    }

    /// AC/MEAS-13
    #[test]
    fn a_request_answered_before_the_cut_counts_when_relied_on() {
        let (mut session, sample_store) = session();
        let span = start(&mut session, None, gpu_only(), 1, 1);
        session.land(r(1), &sample_store);
        let mut relying = relied(&[(gpu(), 2)]);
        relying.answered.insert(r(2));
        session.stop(span, t(2), relying).unwrap();
        assert_eq!(session.account_ready(&sample_store), vec![span]);
    }

    /// AC/ENG-29
    #[test]
    fn answered_requests_are_forgotten_once_nothing_waits_on_them() {
        let (mut session, sample_store) = session();
        for n in 100..200 {
            session.land(r(n), &sample_store);
        }
        assert_eq!(session.answered_held(), 0, "watchdog answers nobody waits on are not kept");
        let span = start(&mut session, None, gpu_only(), 1, 1);
        session.stop(span, t(2), r(2)).unwrap();
        session.land(r(1), &sample_store);
        session.land(r(2), &sample_store);
        assert!(session.is_settled(span));
        assert_eq!(session.answered_held(), 0, "nothing waits on them any more");
    }

    /// AC/MEAS-4, AC/PROF-32, AC/TST-17
    #[test]
    fn a_cpu_only_span_settles_without_any_gpu_answer() {
        let (mut session, sample_store) = session();
        let span = session.start_next("a".to_owned(), None, cpu_only(), t(1), relied(&[(cpu(), 1)])).unwrap();
        session.stop(span, t(2), relied(&[(cpu(), 2)])).unwrap();
        session.finish(t(3), relied(&[(cpu(), 3), (gpu(), 4)]));
        session.land(r(1), &sample_store);
        assert_eq!(session.land(r(2), &sample_store), vec![span]);
        assert!(!session.is_drained(), "the GPU is still out");
    }

    /// AC/ENG-20, AC/PROF-34
    #[test]
    fn an_open_cpu_span_report_waits_for_no_gpu_answer() {
        let (mut session, sample_store) = session();
        let parent = start(&mut session, None, SourceSelection::All, 1, 1);
        let child = session.start_next("a".to_owned(), Some(parent), cpu_only(), t(2), relied(&[(cpu(), 2)])).unwrap();
        session.mark(child, t(3), relied(&[(cpu(), 3)])).unwrap();
        for n in 1..=3 {
            session.land(r(n), &sample_store);
        }
        assert!(session.is_settled(child));
    }

    /// AC/PROF-2, AC/MEAS-81
    #[test]
    fn jagged_segments_are_accounted_per_source_whatever_the_other_source_waits_on() {
        let (mut session, mut sample_store) = session();
        let a = session.start_next("a".to_owned(), None, cpu_only(), t(1), relied(&[(cpu(), 1)])).unwrap();
        let b = session.start_next("b".to_owned(), None, gpu_only(), t(2), relied(&[(gpu(), 2)])).unwrap();
        session.stop(a, t(3), relied(&[(cpu(), 3)])).unwrap();
        session.stop(b, t(4), relied(&[(gpu(), 4)])).unwrap();
        for at in 0..=4 {
            sample_store.store(cpu(), vec![Sample::new(t(at), 10.0)]).unwrap();
            sample_store.store(gpu(), vec![Sample::new(t(at), 100.0)]).unwrap();
        }
        session.land(r(1), &sample_store);
        assert_eq!(session.land(r(3), &sample_store), vec![a], "the CPU waits on no GPU answer");
        assert_eq!(energy(&session, a, cpu()), 20.0);
        assert_eq!(energy(&session, SpanId::ROOT, gpu()), 0.0, "the GPU's 0-2 is still out");
        session.land(r(2), &sample_store);
        assert_eq!(energy(&session, SpanId::ROOT, gpu()), 200.0);
        assert_eq!(session.land(r(4), &sample_store), vec![b]);
        assert_eq!(energy(&session, b, gpu()), 200.0);
    }

    /// AC/PROF-32
    #[test]
    fn a_span_over_both_sources_settles_once_its_jagged_segments_on_each_are_accounted() {
        let (mut session, sample_store) = session();
        let p = start(&mut session, None, SourceSelection::All, 0, 1);
        let a = session.start_next("a".to_owned(), Some(p), cpu_only(), t(1), relied(&[(cpu(), 2)])).unwrap();
        let b = session.start_next("b".to_owned(), Some(p), gpu_only(), t(2), relied(&[(gpu(), 3)])).unwrap();
        session.stop(a, t(3), relied(&[(cpu(), 4)])).unwrap();
        session.stop(b, t(4), relied(&[(gpu(), 5)])).unwrap();
        session.stop(p, t(5), r(6)).unwrap();
        for n in [1, 2, 4, 6] {
            session.land(r(n), &sample_store);
        }
        assert!(session.is_settled(a));
        assert!(!session.is_settled(p), "the GPU from 2 is out");
        session.land(r(3), &sample_store);
        assert!(!session.is_settled(p));
        let mut settled = session.land(r(5), &sample_store);
        settled.sort();
        assert_eq!(settled, vec![p, b]);
    }

    /// AC/ENG-26, AC/PROF-35
    #[test]
    fn finish_is_answered_once_every_segment_is_accounted() {
        let (mut session, sample_store) = session();
        session.finish(t(5), relied(&[(cpu(), 1), (gpu(), 2)]));
        session.land(r(1), &sample_store);
        assert!(!session.is_drained());
        session.land(r(2), &sample_store);
        assert!(session.is_drained());
    }

    /// AC/ENG-30, AC/MEAS-85
    #[test]
    fn a_power_series_keeps_only_what_its_oldest_unaccounted_segment_reads() {
        let (mut session, sample_store) = paired();
        let a = session.start_next("a".to_owned(), None, cpu_only(), t(6), relied(&[(cpu(), 1)])).unwrap();
        session.land(r(1), &sample_store);
        session.mark(a, t(8), relied(&[(cpu(), 2)])).unwrap();
        let cutoffs = session.needed_from_each();
        assert_eq!(cutoffs[&cpu()], t(6), "the CPU's oldest unaccounted segment starts at 6");
        assert_eq!(cutoffs[&gpu()], t(0), "the GPU's segment in progress starts at 0");
    }

    /// AC/ENG-30, AC/MEAS-85
    #[test]
    fn an_intensity_series_keeps_what_its_paired_power_sources_still_read() {
        let (mut session, sample_store) = paired();
        let a = session.start_next("a".to_owned(), None, cpu_only(), t(4), relied(&[(cpu(), 1), (dk1(), 1)])).unwrap();
        session.land(r(1), &sample_store);
        assert_eq!(session.needed_from_each()[&dk1()], t(4), "only the CPU is on dk1");
        session.mark(a, t(9), relied(&[(cpu(), 2), (dk1(), 2)])).unwrap();
        assert_eq!(session.needed_from_each()[&dk1()], t(4));
    }

    /// AC/PROF-14, AC/PROF-27
    #[test]
    fn a_selection_naming_an_intensity_source_is_refused() {
        let (mut session, _) = paired();
        let refused = session.start_next(
            "s".into(),
            None,
            SourceSelection::Sources(vec![dk1()]),
            t(0),
            ReadoutId(1),
        );
        assert!(matches!(refused, Err(TrackerError::UnknownSource(id)) if id == dk1()), "{refused:?}");
    }

    /// AC/MEAS-75
    #[test]
    fn a_counter_with_one_sample_measures_nothing() {
        let mut sample_store = SampleStore::new(
            [(cpu(), SourceKind::Power, SampleKind::Instant), (gpu(), SourceKind::Power, SampleKind::Total)],
            8,
        )
        .unwrap();
        let mut session = Session::new(catalog(), PowerUsageEffectiveness::default(), t(0));
        let span = start(&mut session, None, SourceSelection::All, 1, 1);
        session.stop(span, t(11), ReadoutId(2)).unwrap();
        sample_store.store(cpu(), vec![Sample::new(t(1), 10.0)]).unwrap();
        sample_store.store(gpu(), vec![Sample::new(t(1), 500.0)]).unwrap();
        session.land(ReadoutId(1), &sample_store);
        assert_eq!(session.land(ReadoutId(2), &sample_store), vec![span]);

        assert_eq!(energy(&session, span, cpu()), 100.0, "one sample reads a draw");
        assert!(!session.ledger(span).contains_key(&gpu()), "one total reads no rate");
        assert!(!session.ledger(SpanId::ROOT).contains_key(&gpu()), "not even the root takes it");
    }

    /// AC/PROF-33
    #[test]
    fn a_parent_settles_no_sooner_than_a_child_over_sources_it_did_not_select() {
        let (mut session, sample_store) = session();
        let parent = session.start_next("p".to_owned(), None, gpu_only(), t(1), relied(&[(gpu(), 1)])).unwrap();
        let child = session.start_next("c".to_owned(), Some(parent), SourceSelection::All, t(2), relied(&[(cpu(), 2), (gpu(), 3)])).unwrap();
        session.stop(parent, t(3), relied(&[(cpu(), 4), (gpu(), 5)])).unwrap();
        for n in [1, 2, 3, 5] {
            assert!(session.land(r(n), &sample_store).is_empty(), "the child's CPU is still out after {n}");
        }
        let mut settled = session.land(r(4), &sample_store);
        settled.sort();
        assert_eq!(settled, vec![parent, child], "together");
    }

    /// An epoch of `n` spans, each cut relying on a readout that lands only
    /// after the epoch stops, with every message followed by an accounting
    /// pass as the tracker does. Returns how long it took and what settled.
    fn epoch_of(n: usize) -> (std::time::Duration, usize) {
        let (mut session, mut sample_store) = session();
        let began = std::time::Instant::now();
        let epoch = start(&mut session, None, SourceSelection::All, 1, 1);
        for i in 0..n {
            let span = start(&mut session, Some(epoch), SourceSelection::All, 2, 2);
            session.account_ready(&sample_store);
            session.stop(span, t(2 + i as i64), ReadoutId(2)).unwrap();
            session.account_ready(&sample_store);
        }
        session.stop(epoch, t(2 + n as i64), ReadoutId(2)).unwrap();
        land(&mut session, &mut sample_store, 1, 1, 10.0, 0.0);
        let settled = land(&mut session, &mut sample_store, 2, 2 + n as i64, 10.0, 0.0).len();
        (began.elapsed(), settled)
    }

    /// AC/ENG-66
    #[test]
    fn settling_grows_linearly_with_the_spans_waiting_to_settle() {
        // Best of three, so a busy machine slows only some runs.
        let best = |n| (0..3).map(|_| epoch_of(n)).min().unwrap();
        let (small, settled) = best(500);
        assert_eq!(settled, 501);
        let (large, settled) = best(2000);
        assert_eq!(settled, 2001);
        let ratio = large.as_secs_f64() / small.as_secs_f64();
        assert!(ratio < 8.0, "4x the spans took {ratio:.1}x as long: {small:?} then {large:?}");
    }
}
