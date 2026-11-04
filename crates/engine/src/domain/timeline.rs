use std::collections::{HashMap, HashSet};
use std::time::Duration;

use super::source::{SourceDescription, SourceId, SourceSelection};
use super::{Timestamp, TrackerError};

/// A span's unique identity within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct SpanId(usize);

impl SpanId {
    /// The root span: opens with the session, never closes, and holds the
    /// measured usage no other span claims. Callers never see it as a span.
    pub(crate) const ROOT: SpanId = SpanId(0);

    pub fn new(n: usize) -> Self {
        Self(n)
    }

    /// The raw counter, e.g. for FFI or logging.
    pub fn get(&self) -> usize {
        self.0
    }
}

impl From<usize> for SpanId {
    fn from(n: usize) -> Self {
        Self(n)
    }
}

#[derive(Debug, Clone)]
pub struct Span {
    pub name: String,
    pub parent: Option<SpanId>,
    pub selection: SourceSelection,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
}

impl Span {
    /// A span a caller started at `opened_at`, open.
    pub fn new(
        name: String,
        parent: SpanId,
        selection: SourceSelection,
        opened_at: Timestamp,
    ) -> Self {
        Self {
            name,
            parent: Some(parent),
            selection,
            opened_at,
            closed_at: None,
        }
    }

    /// The root span, open from `opened_at`: no parent, and every source selected.
    pub fn root(opened_at: Timestamp) -> Self {
        Self {
            name: "session".to_owned(),
            parent: None,
            selection: SourceSelection::All,
            opened_at,
            closed_at: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.closed_at.is_none()
    }

    /// Time from opening to closing, or to `until` while still open.
    pub fn open_for(&self, until: Timestamp) -> Duration {
        let end = self.closed_at.unwrap_or(until);
        (end - self.opened_at).to_std().unwrap_or_default()
    }
}

/// The stretch of one power source between two adjacent cuts on it.
#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub source: SourceId,
    pub from: Timestamp,
    pub to: Timestamp,
    /// The spans running throughout that selected the source, the root first.
    pub active: Vec<SpanId>,
    /// The innermost running spans that selected the source, else the root.
    pub claimants: Vec<SpanId>,
}

/// What a cut did.
pub struct CutPlaced {
    /// Where the cut was placed, after any moving up.
    pub at: Timestamp,
    /// The segment it finished on each source it cut.
    pub segments: Vec<Segment>,
    /// The spans this cut closed: the stopped span, then its open descendants.
    pub closed: Vec<SpanId>,
}

/// The spans of a session and the cuts between them, one timeline per power
/// source.
///
/// A start, stop or mark cuts the power sources the spans it opens or closes
/// selected, and no other; [Timeline::finish] cuts every one. Each cut hands
/// back the segment it finished on each source it cut; the timeline keeps no
/// segments itself. A closed span can be [forgotten](Timeline::forget); its id
/// is still known to have closed. Work reported on a span is not kept.
pub struct Timeline {
    /// What each power source measures.
    powers: HashMap<SourceId, SourceDescription>,
    /// Per power source, where its segment in progress starts.
    heads: HashMap<SourceId, Timestamp>,
    /// The latest cut on any source: one clock for all of them.
    latest: Timestamp,
    /// Open right now, in opening order.
    open: Vec<SpanId>,
    spans: HashMap<SpanId, Span>,
    /// Each held span's held children.
    children: HashMap<SpanId, HashSet<SpanId>>,
    /// The highest id any start carried, accepted or refused. The handle
    /// hands ids out, so they may arrive out of order, but nothing names a
    /// span before its start has arrived.
    newest: SpanId,
}

impl Timeline {
    /// Open a [Timeline] at `opened_at` over the power sources among
    /// `sources`, with the root span open and a cut on every source.
    pub fn open(opened_at: Timestamp, sources: &HashMap<SourceId, SourceDescription>) -> Self {
        let powers: HashMap<SourceId, SourceDescription> = sources
            .iter()
            .filter(|(_, description)| matches!(description, SourceDescription::Power { .. }))
            .map(|(&id, description)| (id, description.clone()))
            .collect();
        Self {
            heads: powers.keys().map(|&id| (id, opened_at)).collect(),
            powers,
            latest: opened_at,
            open: vec![SpanId::ROOT],
            spans: HashMap::from([(SpanId::ROOT, Span::root(opened_at))]),
            children: HashMap::new(),
            newest: SpanId::ROOT,
        }
    }

    /// Cut the sources `selection` selects, then open span `id`: it is active
    /// on them from the next segment on. With no `parent` it sits directly
    /// under the root span.
    ///
    /// # Errors
    ///
    /// [TrackerError::UnknownSpan] or [TrackerError::ParentClosed] if
    /// `parent` cannot take a child. No cut is placed and the span doesn't
    /// open, but `id` counts as handed out, so later commands on it are
    /// refused as on a closed span.
    pub fn start(
        &mut self,
        id: SpanId,
        name: String,
        parent: Option<SpanId>,
        selection: SourceSelection,
        at: Timestamp,
    ) -> Result<CutPlaced, TrackerError> {
        debug_assert!(id != SpanId::ROOT && !self.spans.contains_key(&id), "span {id:?} started twice");
        self.handed_out(id);
        if let Some(parent) = parent {
            if self.was_closed(parent) {
                return Err(TrackerError::ParentClosed(parent));
            }
            if !self.is_open(parent) {
                return Err(TrackerError::UnknownSpan(parent));
            }
        }

        let sources = self.selected_by(&selection);
        let placed = self.cut(at, &sources, Vec::new());
        let parent = parent.unwrap_or(SpanId::ROOT);
        self.spans.insert(id, Span::new(name, parent, selection, placed.at));
        self.children.entry(parent).or_default().insert(id);
        self.open.push(id);
        Ok(placed)
    }

    /// Record that `id` was handed out, whether or not its span opens.
    pub fn handed_out(&mut self, id: SpanId) {
        self.newest = self.newest.max(id);
    }

    /// [Timeline::start] with the next id, as the handle would hand it out.
    #[cfg(test)]
    pub fn start_next(
        &mut self,
        name: String,
        parent: Option<SpanId>,
        selection: SourceSelection,
        at: Timestamp,
    ) -> Result<(SpanId, CutPlaced), TrackerError> {
        let id = self.next_id();
        self.start(id, name, parent, selection, at).map(|placed| (id, placed))
    }

    /// The id after the newest handed out.
    #[cfg(test)]
    pub fn next_id(&self) -> SpanId {
        SpanId::new(self.newest.get() + 1)
    }

    /// Cut every source `span` or an open descendant selected, then close
    /// them all. Each is active in the segments this cut finishes, and not
    /// after.
    ///
    /// # Errors
    ///
    /// [TrackerError::UnknownSpan] or [TrackerError::AlreadyClosed]. Nothing
    /// changes and no cut is placed.
    pub fn stop(&mut self, span: SpanId, at: Timestamp) -> Result<CutPlaced, TrackerError> {
        self.check_open(span)?;
        let descendants: HashSet<SpanId> = self.descendants_of(span).into_iter().collect();
        let closing: Vec<SpanId> = std::iter::once(span)
            .chain(self.open.iter().copied().filter(|open| descendants.contains(open)))
            .collect();
        let sources = self.selected_by_any(&closing);
        let placed = self.cut(at, &sources, closing);
        self.close(&placed);
        Ok(placed)
    }

    /// Cut the sources `span` selected without changing who is open, for
    /// work reported on it.
    ///
    /// # Errors
    ///
    /// [TrackerError::UnknownSpan] or [TrackerError::AlreadyClosed]. Nothing
    /// changes and no cut is placed.
    pub fn mark(&mut self, span: SpanId, at: Timestamp) -> Result<CutPlaced, TrackerError> {
        self.check_open(span)?;
        let sources = self.selected_by_any(&[span]);
        Ok(self.cut(at, &sources, Vec::new()))
    }

    /// Cut every source, then close every open span but the root, which
    /// never closes: the session's last cut.
    pub fn finish(&mut self, at: Timestamp) -> CutPlaced {
        let closing: Vec<SpanId> = self.open.iter().copied().filter(|&id| id != SpanId::ROOT).collect();
        let sources: Vec<SourceId> = self.sorted_powers();
        let placed = self.cut(at, &sources, closing);
        self.close(&placed);
        placed
    }

    fn close(&mut self, placed: &CutPlaced) {
        for id in &placed.closed {
            self.spans.get_mut(id).expect("open spans exist").closed_at = Some(placed.at);
        }
        let closed: HashSet<SpanId> = placed.closed.iter().copied().collect();
        self.open.retain(|id| !closed.contains(id));
    }

    /// A span callers started. The root span is not one.
    pub fn span(&self, id: SpanId) -> Option<&Span> {
        if id == SpanId::ROOT { None } else { self.spans.get(&id) }
    }

    /// The root span: open since the session started, parent of every top-level span.
    pub fn root(&self) -> &Span {
        &self.spans[&SpanId::ROOT]
    }

    /// When the latest cut on any source was placed, after any moving up.
    pub fn latest_cut(&self) -> Timestamp {
        self.latest
    }

    /// Where `source`'s segment in progress starts; `None` if it is not a
    /// power source.
    pub fn head(&self, source: SourceId) -> Option<Timestamp> {
        self.heads.get(&source).copied()
    }

    /// Whether `span` selected power source `source`. The root selects every one.
    pub fn selects(&self, span: SpanId, source: SourceId) -> bool {
        let (Some(span), Some(description)) = (self.spans.get(&span), self.powers.get(&source)) else {
            return false;
        };
        span.selection.matches(source, description)
    }

    /// Whether `id` was handed out and has since closed, forgotten or not (or
    /// its start was refused). Forgotten spans are gone from the timeline, but
    /// nothing names a span before its start arrives, so any id no newer than
    /// the newest start was handed out.
    pub fn was_closed(&self, id: SpanId) -> bool {
        match self.span(id) {
            Some(span) => !span.is_open(),
            None => id != SpanId::ROOT && id <= self.newest,
        }
    }

    /// Remove a closed span. `None` if it is unknown, open, or the root.
    ///
    /// Forget a span only once it is settled and its descendants are
    /// forgotten: nothing still to be accounted may refer to it.
    pub fn forget(&mut self, id: SpanId) -> Option<Span> {
        self.span(id).filter(|span| !span.is_open())?;
        let span = self.spans.remove(&id)?;
        self.children.remove(&id);
        if let Some(siblings) = span.parent.and_then(|parent| self.children.get_mut(&parent)) {
            siblings.remove(&id);
        }
        Some(span)
    }

    /// The held children of `span`.
    pub fn children_of(&self, span: SpanId) -> impl Iterator<Item = SpanId> + '_ {
        self.children.get(&span).into_iter().flatten().copied()
    }

    /// The parent of a held span; `None` for the root or an unknown span.
    pub fn parent_of(&self, span: SpanId) -> Option<SpanId> {
        self.spans.get(&span)?.parent
    }

    /// How many spans are held, the root included.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    pub fn is_open(&self, id: SpanId) -> bool {
        self.span(id).is_some_and(Span::is_open)
    }

    /// Every span under `ancestor`, at any depth.
    pub fn descendants_of(&self, ancestor: SpanId) -> Vec<SpanId> {
        let mut found: Vec<SpanId> = self.children_of(ancestor).collect();
        let mut next = 0;
        while let Some(&span) = found.get(next) {
            found.extend(self.children_of(span));
            next += 1;
        }
        found
    }

    fn check_open(&self, span: SpanId) -> Result<(), TrackerError> {
        if self.is_open(span) {
            Ok(())
        } else if self.was_closed(span) {
            Err(TrackerError::AlreadyClosed(span))
        } else {
            Err(TrackerError::UnknownSpan(span))
        }
    }

    /// The power sources `selection` selects, in id order.
    fn selected_by(&self, selection: &SourceSelection) -> Vec<SourceId> {
        self.sorted_powers()
            .into_iter()
            .filter(|&source| selection.matches(source, &self.powers[&source]))
            .collect()
    }

    /// The power sources any of `spans` selected, in id order.
    fn selected_by_any(&self, spans: &[SpanId]) -> Vec<SourceId> {
        self.sorted_powers()
            .into_iter()
            .filter(|&source| spans.iter().any(|&span| self.selects(span, source)))
            .collect()
    }

    fn sorted_powers(&self) -> Vec<SourceId> {
        let mut sources: Vec<SourceId> = self.powers.keys().copied().collect();
        sources.sort();
        sources
    }

    /// Finish the segment in progress on each of `sources`, and start the
    /// next one here.
    ///
    /// A cut earlier than the latest on any source is moved up to it, so cuts
    /// never go backwards: two threads can read the clock in one order and
    /// send in the other.
    fn cut(&mut self, at: Timestamp, sources: &[SourceId], closed: Vec<SpanId>) -> CutPlaced {
        let at = at.max(self.latest);
        self.latest = at;
        let segments = sources
            .iter()
            .map(|&source| {
                let from = self.heads.insert(source, at).expect("every power source has a head");
                let active: Vec<SpanId> =
                    self.open.iter().copied().filter(|&span| self.selects(span, source)).collect();
                let claimants = self.claimants(&active);
                Segment { source, from, to: at, active, claimants }
            })
            .collect();
        CutPlaced { at, segments, closed }
    }

    /// The members of `active` with no active descendant, the root only when
    /// no other span is active.
    fn claimants(&self, active: &[SpanId]) -> Vec<SpanId> {
        // Every ancestor of an active span, each chain walked until it meets one seen.
        let mut ancestors = HashSet::new();
        for &span in active {
            let mut current = self.parent_of(span);
            while let Some(id) = current {
                if !ancestors.insert(id) {
                    break;
                }
                current = self.parent_of(id);
            }
        }
        let innermost: Vec<SpanId> = active
            .iter()
            .copied()
            .filter(|&candidate| candidate != SpanId::ROOT && !ancestors.contains(&candidate))
            .collect();
        if innermost.is_empty() { vec![SpanId::ROOT] } else { innermost }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Fidelity, GridLocation, PowerDomain, PowerScope, SampleKind};

    /// `secs` seconds after the epoch.
    fn t(secs: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(secs)
    }

    fn ms(ms: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::milliseconds(ms)
    }

    fn cpu() -> SourceId { SourceId::for_test(1, 0) }
    fn gpu() -> SourceId { SourceId::for_test(2, 0) }
    fn ram() -> SourceId { SourceId::for_test(1, 1) }
    fn dk1() -> SourceId { SourceId::for_test(3, 0) }

    fn power(domain: PowerDomain) -> SourceDescription {
        SourceDescription::Power {
            device_name: format!("{domain:?}"),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Stated,
        }
    }

    /// CPU, GPU and RAM, and an intensity the timeline never cuts.
    fn sources() -> HashMap<SourceId, SourceDescription> {
        HashMap::from([
            (cpu(), power(PowerDomain::Cpu)),
            (gpu(), power(PowerDomain::Gpu)),
            (ram(), power(PowerDomain::Ram)),
            (dk1(), SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Stated }),
        ])
    }

    fn open() -> Timeline {
        Timeline::open(t(0), &sources())
    }

    fn only(domain: PowerDomain) -> SourceSelection {
        SourceSelection::Domains(vec![domain])
    }

    fn start_at(timeline: &mut Timeline, parent: Option<SpanId>, selection: SourceSelection, at: Timestamp) -> SpanId {
        let (id, _) = timeline.start_next("work".to_owned(), parent, selection, at).expect("the parent can take a child");
        id
    }

    fn start(timeline: &mut Timeline, parent: Option<SpanId>, selection: SourceSelection, at: i64) -> SpanId {
        start_at(timeline, parent, selection, t(at))
    }

    /// `(source, from, to, claimants)` for each segment, sorted.
    fn summary(segments: &[Segment]) -> Vec<(SourceId, Timestamp, Timestamp, Vec<SpanId>)> {
        let mut rows: Vec<_> = segments.iter().map(|s| (s.source, s.from, s.to, s.claimants.clone())).collect();
        rows.sort_by_key(|row| (row.0, row.1));
        rows
    }

    /// AC/PROF-2, AC/PROF-20
    #[test]
    fn a_span_cuts_only_the_sources_it_selected() {
        let mut timeline = open();
        let (_, placed) = timeline.start_next("a".to_owned(), None, only(PowerDomain::Cpu), t(1)).unwrap();
        assert_eq!(summary(&placed.segments), vec![(cpu(), t(0), t(1), vec![SpanId::ROOT])]);
    }

    /// AC/PROF-20
    #[test]
    fn a_span_that_did_not_select_a_source_does_not_change_who_claims_it() {
        let mut timeline = open();
        let p = start(&mut timeline, None, SourceSelection::Domains(vec![PowerDomain::Cpu, PowerDomain::Gpu]), 0);
        let a = start(&mut timeline, Some(p), only(PowerDomain::Cpu), 1);
        timeline.stop(a, t(3)).unwrap();
        let placed = timeline.stop(p, t(4)).unwrap();
        let gpu_segments: Vec<_> = summary(&placed.segments).into_iter().filter(|row| row.0 == gpu()).collect();
        assert_eq!(gpu_segments, vec![(gpu(), t(0), t(4), vec![p])]);
    }

    /// AC/PROF-2, AC/PROF-8
    #[test]
    fn finish_cuts_every_source() {
        let mut timeline = open();
        let placed = timeline.finish(t(5));
        assert_eq!(
            summary(&placed.segments),
            vec![
                (cpu(), t(0), t(5), vec![SpanId::ROOT]),
                (ram(), t(0), t(5), vec![SpanId::ROOT]),
                (gpu(), t(0), t(5), vec![SpanId::ROOT]),
            ]
        );
    }

    /// AC/PROF-2
    #[test]
    fn the_opening_cut_cuts_every_source() {
        let mut timeline = open();
        let (_, placed) = timeline.start_next("a".to_owned(), None, only(PowerDomain::Ram), t(2)).unwrap();
        assert_eq!(placed.segments[0].from, t(0));
        let placed = timeline.finish(t(3));
        assert!(placed.segments.iter().filter(|s| s.source != ram()).all(|s| s.from == t(0)));
    }

    /// AC/PROF-1, AC/PROF-4
    #[test]
    fn cuts_called_together_leave_a_segment_as_wide_as_their_stamps() {
        let mut timeline = open();
        let both = SourceSelection::Domains(vec![PowerDomain::Cpu, PowerDomain::Gpu]);
        let p = start_at(&mut timeline, None, both.clone(), ms(2900));
        let b = start_at(&mut timeline, Some(p), both.clone(), ms(2950));
        let a = start_at(&mut timeline, Some(p), only(PowerDomain::Cpu), ms(3050));
        timeline.stop(b, ms(3300)).unwrap();
        let (_, placed) = timeline.start_next("c".to_owned(), Some(p), both, ms(3310)).unwrap();
        assert_eq!(
            summary(&placed.segments),
            vec![(cpu(), ms(3300), ms(3310), vec![a]), (gpu(), ms(3300), ms(3310), vec![p])]
        );
    }

    /// AC/PROF-19, AC/PROF-21
    #[test]
    fn the_claimants_are_the_innermost_running_spans_that_selected_the_source() {
        let mut timeline = open();
        let p = start(&mut timeline, None, SourceSelection::All, 0);
        let a = start(&mut timeline, Some(p), only(PowerDomain::Cpu), 1);
        let b = start(&mut timeline, Some(p), SourceSelection::All, 2);
        let placed = timeline.finish(t(3));
        let at_end = |source| placed.segments.iter().find(|s| s.source == source).unwrap().claimants.clone();
        assert_eq!(at_end(cpu()), vec![a, b]);
        assert_eq!(at_end(gpu()), vec![b]);
    }

    /// AC/PROF-10, AC/PROF-19
    #[test]
    fn the_root_claims_a_source_no_running_span_selected() {
        let mut timeline = open();
        start(&mut timeline, None, only(PowerDomain::Cpu), 1);
        let placed = timeline.finish(t(2));
        let gpu_segment = placed.segments.iter().find(|s| s.source == gpu()).unwrap();
        assert_eq!(gpu_segment.claimants, vec![SpanId::ROOT]);
    }

    /// AC/PROF-5
    #[test]
    fn a_span_that_selects_nothing_here_cuts_nothing() {
        let mut timeline = open();
        let (span, started) = timeline.start_next("ane".to_owned(), None, only(PowerDomain::Ane), t(1)).unwrap();
        assert!(started.segments.is_empty());
        assert!(timeline.stop(span, t(2)).unwrap().segments.is_empty());
    }

    /// AC/PROF-28
    #[test]
    fn an_intensity_source_is_never_cut() {
        let mut timeline = open();
        assert!(timeline.finish(t(1)).segments.iter().all(|s| s.source != dk1()));
    }

    /// AC/PROF-2, AC/PROF-3
    #[test]
    fn jagged_cuts_split_each_source_at_its_own_stamps() {
        let mut timeline = open();
        let (a, mut cut) = timeline.start_next("a".to_owned(), None, only(PowerDomain::Cpu), t(1)).unwrap();
        let (b, placed) = timeline.start_next("b".to_owned(), None, only(PowerDomain::Gpu), t(2)).unwrap();
        let mut cut = std::mem::take(&mut cut.segments);
        cut.extend(placed.segments);
        cut.extend(timeline.stop(a, t(3)).unwrap().segments);
        cut.extend(timeline.stop(b, t(4)).unwrap().segments);
        cut.extend(timeline.finish(t(5)).segments);
        let root = SpanId::ROOT;
        assert_eq!(
            summary(&cut).into_iter().filter(|row| row.0 != ram()).collect::<Vec<_>>(),
            vec![
                (cpu(), t(0), t(1), vec![root]),
                (cpu(), t(1), t(3), vec![a]),
                (cpu(), t(3), t(5), vec![root]),
                (gpu(), t(0), t(2), vec![root]),
                (gpu(), t(2), t(4), vec![b]),
                (gpu(), t(4), t(5), vec![root]),
            ]
        );
    }

    /// AC/PROF-20
    #[test]
    fn a_cut_on_one_source_inside_a_segment_of_another_leaves_it_whole() {
        let mut timeline = open();
        let a = start(&mut timeline, None, only(PowerDomain::Cpu), 1);
        let (_, gpu_cut) = timeline.start_next("b".to_owned(), None, only(PowerDomain::Gpu), t(2)).unwrap();
        assert!(gpu_cut.segments.iter().all(|s| s.source == gpu()));
        let placed = timeline.stop(a, t(3)).unwrap();
        assert_eq!(summary(&placed.segments), vec![(cpu(), t(1), t(3), vec![a])]);
    }

    /// AC/PROF-7
    #[test]
    fn a_span_is_active_in_the_segments_its_stop_finishes_and_not_its_start() {
        let mut timeline = open();
        let (span, opened) = timeline.start_next("work".to_owned(), None, only(PowerDomain::Cpu), t(1)).unwrap();
        let closed = timeline.stop(span, t(3)).unwrap();
        assert_eq!(opened.segments[0].active, vec![SpanId::ROOT]);
        assert_eq!(closed.segments[0].active, vec![SpanId::ROOT, span]);
    }

    /// AC/PROF-11, AC/PROF-12, AC/PROF-13
    #[test]
    fn a_closed_or_unknown_parent_is_refused_without_a_cut() {
        let mut timeline = open();
        let parent = start(&mut timeline, None, SourceSelection::All, 1);
        timeline.stop(parent, t(2)).unwrap();

        let closed = timeline.start_next("child".to_owned(), Some(parent), SourceSelection::All, t(3));
        assert!(matches!(closed, Err(TrackerError::ParentClosed(p)) if p == parent));
        let unknown = timeline.start_next("child".to_owned(), Some(SpanId::new(99)), SourceSelection::All, t(3));
        assert!(matches!(unknown, Err(TrackerError::UnknownSpan(_))));

        // The next cut still starts from the stop's cut.
        let next = start(&mut timeline, None, SourceSelection::All, 4);
        let placed = timeline.stop(next, t(5)).unwrap();
        assert!(placed.segments.iter().all(|s| s.from == t(4)));
    }

    /// AC/ENG-8, AC/PROF-8
    #[test]
    fn stopping_a_parent_closes_its_open_children_at_the_same_cut() {
        let mut timeline = open();
        let parent = start(&mut timeline, None, only(PowerDomain::Cpu), 1);
        let child = start(&mut timeline, Some(parent), SourceSelection::All, 2);
        let grandchild = start(&mut timeline, Some(child), only(PowerDomain::Gpu), 3);

        let placed = timeline.stop(parent, t(4)).unwrap();

        assert_eq!(placed.closed[0], parent);
        assert_eq!(placed.closed.len(), 3);
        for span in [parent, child, grandchild] {
            assert!(!timeline.is_open(span));
            assert_eq!(timeline.span(span).unwrap().closed_at, Some(t(4)));
        }
        let mut cut: Vec<_> = placed.segments.iter().map(|s| s.source).collect();
        cut.sort();
        assert_eq!(cut, vec![cpu(), ram(), gpu()], "every source a closing span selected is cut");
    }

    /// AC/PROF-15
    #[test]
    fn a_closed_span_cannot_be_stopped_or_marked_again() {
        let mut timeline = open();
        let span = start(&mut timeline, None, SourceSelection::All, 1);
        timeline.stop(span, t(2)).unwrap();
        assert!(matches!(timeline.stop(span, t(3)), Err(TrackerError::AlreadyClosed(_))));
        assert!(matches!(timeline.mark(span, t(3)), Err(TrackerError::AlreadyClosed(_))));
    }

    /// AC/PROF-6
    #[test]
    fn a_cut_earlier_than_the_latest_on_any_source_is_moved_up_to_it() {
        let mut timeline = open();
        let a = start(&mut timeline, None, only(PowerDomain::Cpu), 1);
        timeline.stop(a, t(3)).unwrap();
        let (b, placed) = timeline.start_next("b".to_owned(), None, only(PowerDomain::Gpu), t(2)).unwrap();
        assert_eq!(placed.segments[0].to, t(3));
        assert_eq!(timeline.span(b).unwrap().opened_at, t(3));
    }

    /// AC/PROF-10
    #[test]
    fn the_root_cannot_be_named_by_callers() {
        let mut timeline = open();
        let root = SpanId::ROOT;
        assert!(timeline.span(root).is_none());
        assert!(matches!(timeline.stop(root, t(1)), Err(TrackerError::UnknownSpan(_))));
        let child = timeline.start_next("x".to_owned(), Some(root), SourceSelection::All, t(1));
        assert!(matches!(child, Err(TrackerError::UnknownSpan(_))));
        assert!(matches!(timeline.mark(root, t(1)), Err(TrackerError::UnknownSpan(_))));
        assert!(timeline.root().is_open());
        timeline.finish(t(2));
        assert!(timeline.root().is_open(), "the root never closes");
    }

    /// AC/PROF-10
    #[test]
    fn opening_the_timeline_opens_the_root_at_that_moment() {
        let timeline = open();
        let root = timeline.root();
        assert!(root.is_open());
        assert_eq!(root.opened_at, t(0));
        assert_eq!(root.parent, None);
        assert_eq!(root.selection, SourceSelection::All);
        assert_eq!(root.open_for(t(10)), Duration::from_secs(10), "open until whenever it is asked");
    }

    /// AC/PROF-27
    #[test]
    fn a_selection_of_sources_cuts_and_claims_exactly_those() {
        let mut timeline = open();
        let (span, placed) = timeline.start_next("g".to_owned(), None, SourceSelection::Sources(vec![gpu()]), t(1)).unwrap();
        assert_eq!(summary(&placed.segments), vec![(gpu(), t(0), t(1), vec![SpanId::ROOT])]);
        let placed = timeline.finish(t(2));
        let claims = |source| placed.segments.iter().find(|s| s.source == source).unwrap().claimants.clone();
        assert_eq!(claims(gpu()), vec![span]);
        assert_eq!(claims(cpu()), vec![SpanId::ROOT]);
        assert_eq!(claims(ram()), vec![SpanId::ROOT]);
    }
}
