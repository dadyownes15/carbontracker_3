//! Property tests of the accounting model, over random sessions.
//!
//! A [Scenario] is a random script of span starts, stops and marks, each cut
//! settled by its own readout with random power and grid intensity. Playing it
//! builds a [Session]; landing its readouts, in some order, bills the ledgers.
//! The properties are the guarantees the model states: conservation, settling
//! exactly once and for good, and billing only what a span selected. Ledgers
//! independent of landing order is checked too, but is known not to hold.

use std::collections::{HashMap, HashSet};

use chrono::TimeDelta;
use quickcheck::{Arbitrary, Gen, QuickCheck, TestResult, Testable};

use super::{
    Fidelity, GridLocation, PowerDomain, PowerScope, PowerUsageEffectiveness, ProviderId,
    ReadoutId, Sample, SampleKind, SampleStore, Session, SourceCatalog, SourceDescription,
    SourceId, SourceSelection, SpanId, TimeInterval, Timestamp,
};
use super::session::Relying;
use super::timeline::{CutPlaced, Segment, Timeline};

/// `secs` seconds after the epoch.
fn t(secs: i64) -> Timestamp {
    chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(secs)
}

fn cpu() -> SourceId {
    SourceId::new(ProviderId(1), 0)
}

fn gpu() -> SourceId {
    SourceId::new(ProviderId(2), 0)
}

fn ram() -> SourceId {
    SourceId::new(ProviderId(3), 0)
}

fn grid() -> SourceId {
    SourceId::new(ProviderId(4), 0)
}

/// Each source has a provider of its own.
const PROVIDERS: [ProviderId; 4] = [ProviderId(1), ProviderId(2), ProviderId(3), ProviderId(4)];

/// The request cut `n` sends `provider`.
fn request(n: ReadoutId, provider: ProviderId) -> ReadoutId {
    ReadoutId(n.0 * 8 + provider.0)
}

/// Cut `n` relies on one request per provider.
struct PerProvider(ReadoutId);

impl Relying for PerProvider {
    fn relied(&mut self, source: SourceId, _: Timestamp) -> Option<ReadoutId> {
        Some(request(self.0, source.provider()))
    }
}

fn power_sources() -> [SourceId; 3] {
    [cpu(), gpu(), ram()]
}

fn t0() -> Timestamp {
    t(0)
}

fn power(domain: PowerDomain, sample_kind: SampleKind) -> SourceDescription {
    SourceDescription::Power {
        device_name: format!("{domain:?}"),
        domain,
        scope: PowerScope::DeviceTotal,
        sample_kind,
        fidelity: Fidelity::Stated,
    }
}

/// A CPU and RAM read linearly, a GPU read as held values, and a grid.
fn catalog() -> SourceCatalog {
    SourceCatalog::pairing_all(
        HashMap::from([
            (cpu(), power(PowerDomain::Cpu, SampleKind::Instant)),
            (gpu(), power(PowerDomain::Gpu, SampleKind::Held)),
            (ram(), power(PowerDomain::Ram, SampleKind::Instant)),
            (
                grid(),
                SourceDescription::Intensity {
                    location: GridLocation::Global,
                    fidelity: Fidelity::Stated,
                },
            ),
        ]),
        Some(grid()),
    )
}

fn sample_store() -> SampleStore {
    let catalog = catalog();
    SampleStore::new_from_source(&catalog.descriptions, 64).expect("the keys of a map are distinct")
}

// ---------------------------------------------------------------------------
// Scenarios

/// What every source reads at one readout.
#[derive(Clone, Copy, Debug)]
struct Snapshot {
    watts: [f64; 3],
    grams_per_kwh: f64,
    /// Power sources that returned no samples, as a provider asked sooner
    /// than its minimal interval does.
    skipped: [bool; 3],
}

impl Arbitrary for Snapshot {
    fn arbitrary(g: &mut Gen) -> Self {
        let mut watts = || f64::from(u16::arbitrary(g) % 400);
        Self {
            watts: [watts(), watts(), watts()],
            grams_per_kwh: f64::from(u16::arbitrary(g) % 800),
            skipped: [false; 3],
        }
    }
}

/// A span is named by its index among the spans open at the time, so every
/// script stays playable however it is shrunk.
#[derive(Clone, Debug)]
enum Action {
    Start { parent: Option<u8>, selection: u8 },
    Stop(u8),
    Mark(u8),
}

#[derive(Clone, Debug)]
struct Step {
    action: Action,
    /// Milliseconds since the previous cut; often zero, so cuts coincide.
    after_ms: i64,
    snapshot: Snapshot,
}

impl Arbitrary for Step {
    fn arbitrary(g: &mut Gen) -> Self {
        let action = match u8::arbitrary(g) % 5 {
            0 | 1 => Action::Start {
                parent: Option::arbitrary(g),
                selection: u8::arbitrary(g),
            },
            2 | 3 => Action::Stop(u8::arbitrary(g)),
            _ => Action::Mark(u8::arbitrary(g)),
        };
        let after_ms = if u8::arbitrary(g) % 4 == 0 {
            0
        } else {
            i64::from(u16::arbitrary(g) % 5_000)
        };
        Self {
            action,
            after_ms,
            snapshot: Snapshot::arbitrary(g),
        }
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        // Round gaps to whole seconds so a failure reads easily.
        let rounded = (self.after_ms / 1_000) * 1_000;
        let rounded = (rounded != self.after_ms).then(|| Self {
            after_ms: rounded,
            ..self.clone()
        });
        Box::new(rounded.into_iter())
    }
}

#[derive(Clone, Debug)]
struct Scenario {
    /// The snapshot at the session's opening cut, stored before anything lands.
    opening: Snapshot,
    steps: Vec<Step>,
    pue: f64,
}

impl Arbitrary for Scenario {
    fn arbitrary(g: &mut Gen) -> Self {
        let pue = *g.choose(&[1.0, 1.1, 1.58, 2.0]).expect("non-empty");
        Self {
            opening: Snapshot::arbitrary(g),
            steps: Vec::arbitrary(g),
            pue,
        }
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        let (opening, pue) = (self.opening, self.pue);
        Box::new(self.steps.shrink().map(move |steps| Self {
            opening,
            steps,
            pue,
        }))
    }
}

/// A [Scenario] in which any power source may skip any readout but the
/// opening one.
#[derive(Clone, Debug)]
struct Skipping(Scenario);

impl Arbitrary for Skipping {
    fn arbitrary(g: &mut Gen) -> Self {
        let mut scenario = Scenario::arbitrary(g);
        for step in &mut scenario.steps {
            step.snapshot.skipped = [bool::arbitrary(g), bool::arbitrary(g), bool::arbitrary(g)];
        }
        Self(scenario)
    }

    fn shrink(&self) -> Box<dyn Iterator<Item = Self>> {
        Box::new(self.0.shrink().map(Self))
    }
}

fn selection(n: u8) -> SourceSelection {
    match n % 4 {
        0 => SourceSelection::All,
        1 => SourceSelection::Domains(vec![PowerDomain::Cpu]),
        2 => SourceSelection::Domains(vec![PowerDomain::Gpu]),
        _ => SourceSelection::Domains(vec![PowerDomain::Cpu, PowerDomain::Ram]),
    }
}

// ---------------------------------------------------------------------------
// Playing a scenario

/// A scenario's cuts placed on a session, none of their readouts landed yet.
struct Played {
    session: Session,
    /// Each cut's readout, when it read, and what it read.
    readouts: Vec<(ReadoutId, Timestamp, Snapshot)>,
    /// Every span started, with its selection.
    spans: Vec<(SpanId, SourceSelection)>,
    opening: Snapshot,
    end: Timestamp,
}

impl Scenario {
    fn play(&self) -> Played {
        let pue = PowerUsageEffectiveness::new(self.pue).expect("at least one");
        let mut session = Session::new(catalog(), pue, t0());
        let mut readouts = Vec::new();
        let mut spans: Vec<(SpanId, SourceSelection)> = Vec::new();
        let mut at = t0();
        let mut read_at = t0();

        for step in &self.steps {
            let open: Vec<SpanId> = spans
                .iter()
                .map(|&(id, _)| id)
                .filter(|&id| session.timeline().is_open(id))
                .collect();
            let pick = |n: u8| open.get(usize::from(n) % open.len().max(1)).copied();
            let next_at = at + TimeDelta::milliseconds(step.after_ms);
            let readout = ReadoutId(readouts.len() + 1);

            let placed = match step.action {
                Action::Start {
                    parent,
                    selection: n,
                } => {
                    let parent = parent.and_then(pick);
                    let id = session
                        .start_next("work".to_owned(), parent, selection(n), next_at, PerProvider(readout))
                        .expect("an open parent takes a child");
                    spans.push((id, selection(n)));
                    true
                }
                Action::Stop(n) => pick(n)
                    .map(|span| {
                        session
                            .stop(span, next_at, PerProvider(readout))
                            .expect("open spans stop")
                    })
                    .is_some(),
                Action::Mark(n) => pick(n)
                    .map(|span| {
                        session
                            .mark(span, next_at, PerProvider(readout))
                            .expect("open spans take marks")
                    })
                    .is_some(),
            };
            if placed {
                at = next_at;
                // Stamped at read time, as real providers stamp: after its cut
                // and after every earlier readout, even when cuts coincide. A
                // microsecond apart, the resolution replays are written at.
                read_at = at.max(read_at) + TimeDelta::microseconds(1);
                readouts.push((readout, read_at, step.snapshot));
            }
        }

        // Every session ends with finish(), which cuts every source.
        let readout = ReadoutId(readouts.len() + 1);
        session.finish(at, PerProvider(readout));
        read_at = at.max(read_at) + TimeDelta::microseconds(1);
        let last = self.steps.last().map_or(self.opening, |step| step.snapshot);
        readouts.push((readout, read_at, last));

        Played {
            session,
            readouts,
            spans,
            opening: self.opening,
            end: at,
        }
    }
}

/// Store what `provider` read in `snapshot` at `at`.
fn store_one(sample_store: &mut SampleStore, at: Timestamp, snapshot: Snapshot, provider: ProviderId) {
    for (i, source) in power_sources().into_iter().enumerate() {
        if source.provider() == provider && !snapshot.skipped[i] {
            sample_store.store(source, vec![Sample::new(at, snapshot.watts[i])]).expect("known source");
        }
    }
    if grid().provider() == provider {
        sample_store.store(grid(), vec![Sample::new(at, snapshot.grams_per_kwh)]).expect("known source");
    }
}

/// An order to land every provider's requests in: the providers interleaved
/// by `keys`, each provider's own requests in the order they were sent, as
/// `(cut, provider)`.
fn interleaving(n: usize, keys: &[u32]) -> Vec<(usize, ProviderId)> {
    let mut next = [0usize; 4];
    let mut order = Vec::new();
    for step in 0..n * PROVIDERS.len() {
        let key = keys.get(step).copied().unwrap_or(0) as usize;
        let k = (0..PROVIDERS.len()).map(|d| (key + d) % PROVIDERS.len()).find(|&k| next[k] < n).expect("some left");
        order.push((next[k], PROVIDERS[k]));
        next[k] += 1;
    }
    order
}

fn store(sample_store: &mut SampleStore, at: Timestamp, snapshot: Snapshot) {
    for ((source, watts), skipped) in power_sources()
        .into_iter()
        .zip(snapshot.watts)
        .zip(snapshot.skipped)
    {
        if !skipped {
            sample_store
                .store(source, vec![Sample::new(at, watts)])
                .expect("known source");
        }
    }
    sample_store
        .store(grid(), vec![Sample::new(at, snapshot.grams_per_kwh)])
        .expect("known source");
}

/// The order to land `n` readouts in: by `keys`, ties by position.
fn permutation(n: usize, keys: &[u32]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (keys.get(i).copied().unwrap_or(0), i));
    order
}

/// Every span's ledger, the root's included.
type Ledgers = HashMap<SpanId, HashMap<SourceId, (f64, Option<f64>)>>;

impl Played {
    /// Store the opening snapshot, then land the readouts in `order`, each
    /// storing its samples first. Returns each landing's settled spans.
    fn land(&mut self, order: &[usize]) -> (SampleStore, Vec<Vec<SpanId>>) {
        let mut sample_store = sample_store();
        store(&mut sample_store, t0(), self.opening);
        let announced = order
            .iter()
            .map(|&i| {
                let (readout, at, snapshot) = self.readouts[i];
                store(&mut sample_store, at, snapshot);
                self.land_cut(readout, &sample_store)
            })
            .collect();
        (sample_store, announced)
    }

    /// Land every request cut `readout` sent, its samples already stored.
    fn land_cut(&mut self, readout: ReadoutId, sample_store: &SampleStore) -> Vec<SpanId> {
        PROVIDERS.iter().flat_map(|&provider| self.session.land(request(readout, provider), sample_store)).collect()
    }

    /// Store the opening snapshot, then land each `(cut, provider)` request
    /// in `order`, each storing only what its provider read.
    fn land_requests(&mut self, order: &[(usize, ProviderId)]) {
        let mut sample_store = sample_store();
        store(&mut sample_store, t0(), self.opening);
        for &(i, provider) in order {
            let (readout, at, snapshot) = self.readouts[i];
            store_one(&mut sample_store, at, snapshot, provider);
            self.session.land(request(readout, provider), &sample_store);
        }
    }

    fn land_in_order(&mut self) -> (SampleStore, Vec<Vec<SpanId>>) {
        self.land(&(0..self.readouts.len()).collect::<Vec<_>>())
    }

    fn all_spans(&self) -> impl Iterator<Item = SpanId> + '_ {
        std::iter::once(SpanId::ROOT).chain(self.spans.iter().map(|&(id, _)| id))
    }

    fn ledgers(&self) -> Ledgers {
        self.all_spans()
            .map(|span| {
                let ledger = self.session.ledger(span);
                (
                    span,
                    ledger
                        .into_iter()
                        .map(|(s, u)| (s, (u.energy.get(), u.emissions.map(|g| g.get()))))
                        .collect(),
                )
            })
            .collect()
    }

    fn closed(&self) -> HashSet<SpanId> {
        self.spans
            .iter()
            .map(|&(id, _)| id)
            .filter(|&id| !self.session.timeline().is_open(id))
            .collect()
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
}

fn same_ledgers(a: &Ledgers, b: &Ledgers) -> Result<(), String> {
    for span in a.keys().chain(b.keys()) {
        let (left, right) = (
            &a.get(span).cloned().unwrap_or_default(),
            &b.get(span).cloned().unwrap_or_default(),
        );
        for source in left.keys().chain(right.keys()) {
            let (l, r) = (
                left.get(source).copied().unwrap_or_default(),
                right.get(source).copied().unwrap_or_default(),
            );
            let same_emissions = match (l.1, r.1) {
                (Some(l), Some(r)) => close(l, r),
                (l, r) => l == r,
            };
            if !close(l.0, r.0) || !same_emissions {
                return Err(format!("{span:?} on {source:?}: {l:?} vs {r:?}"));
            }
        }
    }
    Ok(())
}

fn verdict(result: Result<(), String>) -> TestResult {
    match result {
        Ok(()) => TestResult::passed(),
        Err(why) => TestResult::error(why),
    }
}

fn check(property: impl Testable) {
    QuickCheck::new().tests(500).quickcheck(property);
}

// ---------------------------------------------------------------------------
// Properties

/// Conservation: once every readout has landed, the ledgers, root included,
/// sum per source to what that source measured over the whole session, times
/// the PUE. Nothing is lost and nothing is billed twice.
fn conserves(scenario: Scenario) -> TestResult {
    let mut played = scenario.play();
    let (sample_store, _) = played.land_in_order();
    let ledgers = played.ledgers();

    let whole = TimeInterval::new(t0(), played.end).ok();
    verdict(power_sources().into_iter().try_for_each(|source| {
        let billed = ledgers
            .values()
            .filter_map(|ledger| ledger.get(&source))
            .fold((0.0, 0.0), |acc, u| (acc.0 + u.0, acc.1 + u.1.unwrap_or(0.0)));
        let energy = whole
            .as_ref()
            .and_then(|w| sample_store.energy(source, w))
            .map_or(0.0, |j| j.get());
        let emissions = whole
            .as_ref()
            .and_then(|w| sample_store.emissions(source, grid(), w))
            .map_or(0.0, |g| g.get());
        let (energy, emissions) = (energy * scenario.pue, emissions * scenario.pue);
        if close(billed.0, energy) && close(billed.1, emissions) {
            Ok(())
        } else {
            Err(format!(
                "{source:?}: billed {billed:?}, measured × PUE {:?}",
                (energy, emissions)
            ))
        }
    }))
}

/// AC/ENG-2, AC/PROF-30, AC/PROF-31, AC/PROF-44, AC/TST-15
#[test]
fn ledgers_sum_to_measured_usage_times_the_pue() {
    check(conserves as fn(Scenario) -> TestResult);
}

/// Conservation when providers skip reads.
///
/// Known not to hold. A segment is accounted once its readouts land, even
/// when its closing one came back empty for a source, so that source is held
/// at its last sample past it. The next sample draws a line back over the
/// same stretch, and the ledgers no longer sum to the series.
/// AC/MEAS-90, AC/TST-23, AC/TST-24
#[test]
#[ignore = "known: a segment closed by an empty readout is billed a held value that later samples contradict"]
fn ledgers_sum_to_measured_usage_even_when_reads_are_skipped() {
    check((|Skipping(scenario)| conserves(scenario)) as fn(Skipping) -> TestResult);
}

/// Readouts land in any order; the ledgers come out the same.
///
/// Known not to hold. Samples are stamped at read time, just after their cut,
/// and a segment is accounted once its own two readouts land. One accounted
/// before an earlier readout lands bills the sliver between its start and its
/// first sample from an older neighbour, by up to the read delay's worth.
fn order_invariant(scenario: Scenario, keys: Vec<u32>) -> TestResult {
    verdict(order_invariant_as_given(&scenario, &keys))
}

/// AC/MEAS-90, AC/TST-23, AC/TST-24, AC/TST-25
#[test]
#[ignore = "known: under Skip, a segment accounted before an earlier read of another provider lands bills its first sliver from an older sample"]
fn ledgers_do_not_depend_on_the_order_readouts_land_in() {
    check(order_invariant as fn(Scenario, Vec<u32>) -> TestResult);
}

/// Under `Queue` each provider's reads land in the order they were sent, so
/// only the interleaving of providers varies; the ledgers come out the same.
fn queue_order_invariant(scenario: Scenario, keys: Vec<u32>) -> TestResult {
    let mut in_order = scenario.play();
    in_order.land_in_order();
    let mut interleaved = scenario.play();
    let order = interleaving(interleaved.readouts.len(), &keys);
    interleaved.land_requests(&order);
    verdict(same_ledgers(&in_order.ledgers(), &interleaved.ledgers()).map_err(|why| format!("landing {order:?}: {why}")))
}

/// AC/MEAS-82, AC/MEAS-91, AC/PROF-26, AC/TST-25
#[test]
fn under_queue_answer_order_does_not_change_ledgers() {
    check(queue_order_invariant as fn(Scenario, Vec<u32>) -> TestResult);
}

/// Two spans start at the same instant, so two readouts follow one cut. Each
/// reads at its own time, so the bill does not depend on which lands first.
/// AC/PROF-4
#[test]
fn coinciding_cuts_bill_the_same_whichever_readout_lands_first() {
    let snapshot = |w: f64| Snapshot {
        watts: [w; 3],
        grams_per_kwh: 100.0,
        skipped: [false; 3],
    };
    let start = Action::Start {
        parent: None,
        selection: 0,
    };
    let scenario = Scenario {
        opening: snapshot(200.0),
        steps: vec![
            Step {
                action: start.clone(),
                after_ms: 3_000,
                snapshot: snapshot(100.0),
            },
            Step {
                action: start,
                after_ms: 0,
                snapshot: snapshot(140.0),
            },
        ],
        pue: 1.0,
    };
    let result = order_invariant_as_given(&scenario, &[1, 0]);
    assert!(result.is_ok(), "{}", result.unwrap_err());
}

fn order_invariant_as_given(scenario: &Scenario, keys: &[u32]) -> Result<(), String> {
    let mut in_order = scenario.play();
    in_order.land_in_order();
    let mut shuffled = scenario.play();
    let order = permutation(shuffled.readouts.len(), keys);
    shuffled.land(&order);
    same_ledgers(&in_order.ledgers(), &shuffled.ledgers())
        .map_err(|why| format!("landing {order:?}: {why}"))
}

/// Every closed span is announced settled exactly once, an open span never,
/// and an announced span's ledger never changes afterwards.
fn settles_once_and_for_good(scenario: Scenario, keys: Vec<u32>) -> TestResult {
    let mut played = scenario.play();
    let order = permutation(played.readouts.len(), &keys);

    let mut sample_store = sample_store();
    store(&mut sample_store, t0(), played.opening);
    let mut announced: HashMap<SpanId, HashMap<SourceId, (f64, Option<f64>)>> = HashMap::new();
    // Which landing settled each span.
    let mut settled_at: HashMap<SpanId, usize> = HashMap::new();
    for (landing, &i) in order.iter().enumerate() {
        let (readout, at, snapshot) = played.readouts[i];
        store(&mut sample_store, at, snapshot);
        for span in played.land_cut(readout, &sample_store) {
            if announced.contains_key(&span) {
                return TestResult::error(format!("{span:?} announced twice"));
            }
            announced.insert(span, played.ledgers().remove(&span).unwrap_or_default());
            settled_at.insert(span, landing);
        }
    }
    for (&span, &landing) in &settled_at {
        let parent = played.session.timeline().span(span).and_then(|record| record.parent);
        if let Some(&parent_landing) = parent.and_then(|parent| settled_at.get(&parent))
            && parent_landing < landing
        {
            return TestResult::error(format!("{span:?} settled after its parent {parent:?}"));
        }
    }

    let closed = played.closed();
    let keys: HashSet<SpanId> = announced.keys().copied().collect();
    if keys != closed {
        return TestResult::error(format!("announced {keys:?}, closed {closed:?}"));
    }
    if let Some(span) = played
        .all_spans()
        .find(|&span| !played.session.is_settled(span))
    {
        return TestResult::error(format!("{span:?} unsettled after every readout landed"));
    }
    let now: Ledgers = played
        .ledgers()
        .into_iter()
        .filter(|(span, _)| announced.contains_key(span))
        .collect();
    verdict(same_ledgers(&announced, &now).map_err(|why| format!("billed after settling: {why}")))
}

/// AC/MEAS-81, AC/MEAS-82, AC/PROF-33, AC/TST-14, AC/TST-16
#[test]
fn every_closed_span_settles_exactly_once_and_stays_settled() {
    check(settles_once_and_for_good as fn(Scenario, Vec<u32>) -> TestResult);
}

/// A span is billed only for sources it selected, and never a negative amount.
fn bills_only_what_was_selected(scenario: Scenario) -> TestResult {
    let mut played = scenario.play();
    played.land_in_order();
    let catalog = catalog();
    let ledgers = played.ledgers();

    verdict(played.spans.iter().try_for_each(|(span, selection)| {
        ledgers[span]
            .iter()
            .try_for_each(|(source, &(energy, emissions))| {
                if !selection.matches(*source, &catalog.descriptions[source]) {
                    Err(format!(
                        "{span:?} selected {selection:?} but was billed {source:?}"
                    ))
                } else if energy < 0.0 || emissions.is_some_and(|g| g < 0.0) {
                    Err(format!(
                        "{span:?} billed {energy} J, {emissions:?} g on {source:?}"
                    ))
                } else {
                    Ok(())
                }
            })
    }))
}

/// AC/PROF-20, AC/PROF-27
#[test]
fn a_span_is_billed_only_for_the_sources_it_selected() {
    check(bills_only_what_was_selected as fn(Scenario) -> TestResult);
}

/// Landing a readout again bills nothing twice and announces nothing.
fn idempotent(scenario: Scenario) -> TestResult {
    let mut played = scenario.play();
    let (sample_store, _) = played.land_in_order();
    let before = played.ledgers();
    for i in 0..played.readouts.len() {
        let readout = played.readouts[i].0;
        if let settled @ [_, ..] = played.land_cut(readout, &sample_store).as_slice() {
            return TestResult::error(format!("relanding {readout:?} announced {settled:?}"));
        }
    }
    verdict(same_ledgers(&before, &played.ledgers()))
}

/// AC/TST-14
#[test]
fn landing_a_readout_twice_changes_nothing() {
    check(idempotent as fn(Scenario) -> TestResult);
}

/// The session report splits its total exactly into claimed and unclaimed.
fn report_splits_total(scenario: Scenario) -> TestResult {
    let mut played = scenario.play();
    played.land_in_order();
    let report = played.session.session_report(played.end);

    let (total, claimed, unclaimed) = (report.total, report.claimed, report.unclaimed);
    let energy = close(
        total.energy.get(),
        claimed.energy.get() + unclaimed.energy.get(),
    );
    let grams = |usage: crate::domain::accounting::Usage| usage.emissions.map_or(0.0, |g| g.get());
    let emissions = close(grams(total), grams(claimed) + grams(unclaimed));
    verdict(if energy && emissions {
        Ok(())
    } else {
        Err(format!(
            "total {total:?} ≠ claimed {claimed:?} + unclaimed {unclaimed:?}"
        ))
    })
}

/// AC/PROF-52
#[test]
fn the_session_report_splits_its_total_into_claimed_and_unclaimed() {
    check(report_splits_total as fn(Scenario) -> TestResult);
}

// ---------------------------------------------------------------------------
// The timeline alone: commands in, segments out

/// A cut added to a scenario's own.
#[derive(Clone, Copy, PartialEq)]
enum Added {
    Nothing,
    /// A span over the GPU alone, started after this many steps.
    GpuSpan(usize),
    /// A mark on an open span halfway into every gap.
    Marks,
}

/// A span as its commands placed it: by the index of the cut that opened
/// and the one that closed it.
struct Record {
    parent: SpanId,
    selection: SourceSelection,
    opened: usize,
    closed: Option<usize>,
}

/// One segment, with the indices of the cuts either side, and whether an
/// added cut ended it.
struct Cut {
    segment: Segment,
    from: usize,
    to: usize,
    added: bool,
}

/// A scenario's commands placed on a timeline, with nothing landed.
struct Placed {
    timeline: Timeline,
    /// Every segment, in the order the cuts placed them.
    cuts: Vec<Cut>,
    spans: HashMap<SpanId, Record>,
    /// Per source, the index of its latest cut; the opening cut is 0.
    heads: HashMap<SourceId, usize>,
    count: usize,
    end: Timestamp,
}

impl Placed {
    fn record(&mut self, placed: CutPlaced, added: bool) {
        self.count += 1;
        for segment in placed.segments {
            let from = self.heads.insert(segment.source, self.count).expect("a power source");
            self.cuts.push(Cut { segment, from, to: self.count, added });
        }
        for span in placed.closed {
            self.spans.get_mut(&span).expect("started").closed = Some(self.count);
        }
    }

    fn start(&mut self, id: SpanId, parent: Option<SpanId>, selection: SourceSelection, at: Timestamp, added: bool) {
        let placed = self.timeline.start(id, "work".to_owned(), parent, selection.clone(), at).expect("an open parent takes a child");
        self.record(placed, added);
        let parent = parent.unwrap_or(SpanId::ROOT);
        self.spans.insert(id, Record { parent, selection, opened: self.count, closed: None });
    }

    /// The segments on `source`, in order.
    fn on(&self, source: SourceId) -> Vec<&Cut> {
        self.cuts.iter().filter(|cut| cut.segment.source == source).collect()
    }

    fn is_descendant(&self, mut span: SpanId, of: SpanId) -> bool {
        while let Some(record) = self.spans.get(&span) {
            if record.parent == of {
                return true;
            }
            span = record.parent;
        }
        false
    }
}

impl Scenario {
    /// Place this scenario's commands, and `added`, on a timeline alone.
    fn place(&self, added: Added) -> Placed {
        let catalog = catalog();
        let mut placed = Placed {
            timeline: Timeline::open(t0(), &catalog.descriptions),
            cuts: Vec::new(),
            spans: HashMap::new(),
            heads: power_sources().into_iter().map(|source| (source, 0)).collect(),
            count: 0,
            end: t0(),
        };
        let mut started: Vec<SpanId> = Vec::new();
        let mut at = t0();
        for (index, step) in self.steps.iter().enumerate() {
            if added == Added::GpuSpan(index) {
                placed.start(SpanId::new(1_000_000), None, SourceSelection::Domains(vec![PowerDomain::Gpu]), at, true);
            }
            let open: Vec<SpanId> = started.iter().copied().filter(|&id| placed.timeline.is_open(id)).collect();
            let pick = |n: u8| open.get(usize::from(n) % open.len().max(1)).copied();
            if added == Added::Marks
                && let Some(&span) = open.first()
            {
                let halfway = at + TimeDelta::microseconds(step.after_ms * 500);
                let cut = placed.timeline.mark(span, halfway).expect("open spans take marks");
                placed.record(cut, true);
            }
            let next_at = at + TimeDelta::milliseconds(step.after_ms);
            let cut = match step.action {
                Action::Start { parent, selection: n } => {
                    let id = SpanId::new(started.len() + 1);
                    started.push(id);
                    placed.start(id, parent.and_then(pick), selection(n), next_at, false);
                    None
                }
                Action::Stop(n) => pick(n).map(|span| placed.timeline.stop(span, next_at).expect("open spans stop")),
                Action::Mark(n) => pick(n).map(|span| placed.timeline.mark(span, next_at).expect("open spans take marks")),
            };
            if let Some(cut) = cut {
                placed.record(cut, false);
            }
            at = placed.timeline.latest_cut();
        }
        if added == Added::GpuSpan(self.steps.len()) {
            placed.start(SpanId::new(1_000_000), None, SourceSelection::Domains(vec![PowerDomain::Gpu]), at, true);
        }
        let last = placed.timeline.finish(at);
        placed.record(last, false);
        placed.end = placed.timeline.latest_cut();
        placed
    }
}

/// On each source, segments run from the opening cut to the last with no
/// gap or overlap.
fn tiles(scenario: Scenario) -> TestResult {
    let placed = scenario.place(Added::Nothing);
    verdict(power_sources().into_iter().try_for_each(|source| {
        let mut reached = t0();
        for cut in placed.on(source) {
            let segment = &cut.segment;
            if segment.from != reached || segment.to < segment.from {
                return Err(format!("{source:?}: {segment:?} after {reached}"));
            }
            reached = segment.to;
        }
        if reached == placed.end { Ok(()) } else { Err(format!("{source:?} ends at {reached}, the session at {}", placed.end)) }
    }))
}

/// AC/TST-10, AC/PROF-3
#[test]
fn on_each_source_segments_tile_the_session() {
    check(tiles as fn(Scenario) -> TestResult);
}

/// A segment's claimants are the spans active throughout it that selected
/// its source and have no active descendant that did; the root if none.
fn claims_by_the_rule(scenario: Scenario) -> TestResult {
    let placed = scenario.place(Added::Nothing);
    let catalog = catalog();
    verdict(placed.cuts.iter().try_for_each(|cut| {
        let source = cut.segment.source;
        let active: Vec<SpanId> = placed
            .spans
            .iter()
            .filter(|(_, record)| {
                record.opened <= cut.from
                    && record.closed.is_none_or(|closed| closed >= cut.to)
                    && record.selection.matches(source, &catalog.descriptions[&source])
            })
            .map(|(&id, _)| id)
            .collect();
        let mut expected: Vec<SpanId> =
            active.iter().copied().filter(|&span| !active.iter().any(|&other| placed.is_descendant(other, span))).collect();
        if expected.is_empty() {
            expected.push(SpanId::ROOT);
        }
        expected.sort();
        let mut claimants = cut.segment.claimants.clone();
        claimants.sort();
        if claimants == expected { Ok(()) } else { Err(format!("{:?}: claimed by {claimants:?}, expected {expected:?}", cut.segment)) }
    }))
}

/// AC/TST-11, AC/PROF-19
#[test]
fn a_segment_is_claimed_by_the_innermost_active_spans_that_selected_it() {
    check(claims_by_the_rule as fn(Scenario) -> TestResult);
}

/// A span over the GPU alone, started anywhere, changes no segment on the
/// CPU or the RAM.
fn other_sources_untouched(scenario: Scenario, after: usize) -> TestResult {
    let after = after % (scenario.steps.len() + 1);
    let (without, with) = (scenario.place(Added::Nothing), scenario.place(Added::GpuSpan(after)));
    verdict([cpu(), ram()].into_iter().try_for_each(|source| {
        let segments = |placed: &Placed| placed.on(source).into_iter().map(|cut| cut.segment.clone()).collect::<Vec<_>>();
        if segments(&without) == segments(&with) { Ok(()) } else { Err(format!("{source:?} changed")) }
    }))
}

/// AC/TST-12, AC/PROF-20
#[test]
fn a_span_on_other_sources_changes_no_segment_on_this_one() {
    check(other_sources_untouched as fn(Scenario, usize) -> TestResult);
}

/// Marks only split segments: each segment is the run of the new ones up to
/// the next cut that is not an added mark, all claimed as it was.
fn marks_only_split(scenario: Scenario) -> TestResult {
    let (without, with) = (scenario.place(Added::Nothing), scenario.place(Added::Marks));
    verdict(power_sources().into_iter().try_for_each(|source| {
        let mut pieces = with.on(source).into_iter();
        for whole in without.on(source) {
            let mut from = None;
            loop {
                let Some(piece) = pieces.next() else { return Err(format!("{source:?}: no piece of {:?}", whole.segment)) };
                if piece.segment.claimants != whole.segment.claimants {
                    return Err(format!("{source:?}: {:?} within {:?}", piece.segment, whole.segment));
                }
                from.get_or_insert(piece.segment.from);
                if !piece.added {
                    if (from, piece.segment.to) != (Some(whole.segment.from), whole.segment.to) {
                        return Err(format!("{source:?}: pieces {from:?}..{} of {:?}", piece.segment.to, whole.segment));
                    }
                    break;
                }
            }
        }
        Ok(())
    }))
}

/// AC/TST-13, AC/PROF-9
#[test]
fn adding_marks_only_splits_segments() {
    check(marks_only_split as fn(Scenario) -> TestResult);
}


/// AC/TST-23
#[test]
fn every_ignored_test_says_why_it_is_known_to_fail() {
    fn sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("a source directory").flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    for dir in ["src", "tests"] {
        sources(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(dir), &mut found);
    }
    let marker = ["#[", "ignore"].concat();
    for path in found {
        let text = std::fs::read_to_string(&path).expect("readable");
        for (n, line) in text.lines().enumerate() {
            if let Some(at) = line.find(&marker) {
                let rest = &line[at + marker.len()..];
                assert!(rest.starts_with(" = \"known: "), "{}:{}: an ignored test names what is known to fail", path.display(), n + 1);
            }
        }
    }
}
