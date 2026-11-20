//! The engine driven one input at a time, with no threads and with time
//! given to each step: when a read starts, when it returns, when the engine
//! sees it. What these tests check holds exactly on any host, however busy.

use std::collections::HashMap;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use chrono::TimeDelta;
use tokio::sync::oneshot;

use crate::adapters::outbound::readers::{ReadReturn, Readers};
use crate::application::runtime::{Engine, Input};
use crate::application::subscribers::{Subscription, subscription};
use crate::application::tracker::Tracker;
use crate::config::{Announcements, FetchPolicy};
use crate::domain::{
    Cooldown, Failure, Fidelity, GridLocation, Observation, PowerDomain, PowerScope, PowerUsageEffectiveness,
    ProviderError, ProviderId, Sample, SampleKind, SessionReport, Skip, SourceDescription, SourceId,
    SourceSelection, SpanId, Timestamp, TrackerCommand, TrackerError, TrackerEvent, TrackerMessage,
    UnreadyActions, Work,
};
use crate::ports::outbound::{Provider, ProviderResponse};
use crate::sources::Sources;

const POWER: ProviderId = ProviderId(1);
const GRID: ProviderId = ProviderId(2);
const TIMEOUT: Duration = Duration::from_secs(1);
/// Long enough that the watchdog stays quiet unless a test sets its own.
const QUIET: Duration = Duration::from_secs(600);
const WATTS: f64 = 10.0;

/// A provider the driver never reads: it only states its cooldown.
struct Unread(Cooldown);

impl Provider for Unread {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        unreachable!("the driver reads nothing")
    }

    fn cooldown(&self) -> Cooldown {
        self.0
    }
}

fn after(cooldown_ms: u64) -> Cooldown {
    Cooldown::After(Duration::from_millis(cooldown_ms))
}

fn power() -> SourceDescription {
    SourceDescription::Power {
        device_name: "test".to_owned(),
        domain: PowerDomain::System,
        scope: PowerScope::DeviceTotal,
        sample_kind: SampleKind::Instant,
        fidelity: Fidelity::Stated,
    }
}

fn grid() -> SourceDescription {
    SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Stated }
}

/// What a test sets up: each provider's cooldown and policy, how many power
/// sources the power provider serves, and the watchdog's period.
struct Setup {
    power: Cooldown,
    power_sources: u16,
    grid: Cooldown,
    policies: Vec<(ProviderId, UnreadyActions)>,
    interval: Duration,
}

impl Default for Setup {
    fn default() -> Self {
        Self { power: after(0), power_sources: 1, grid: after(0), policies: Vec::new(), interval: QUIET }
    }
}

/// The engine, the jobs it sends each provider, and every event it announces.
struct Driver {
    engine: Engine,
    jobs: HashMap<ProviderId, Receiver<()>>,
    reads: HashMap<ProviderId, usize>,
    serves: HashMap<ProviderId, Vec<SourceId>>,
    events: Subscription,
    seen: Vec<TrackerEvent>,
    t0: Instant,
    wall0: Timestamp,
    next_span: usize,
}

impl Driver {
    /// The engine over `setup`, opened at 0 ms: every provider has been sent its opening read.
    fn new(setup: Setup) -> Self {
        let mut sources = Sources::new();
        let power_sources: Vec<SourceId> = (0..setup.power_sources).map(|slot| SourceId::new(POWER, slot)).collect();
        sources
            .add_provider(POWER, Unread(setup.power), power_sources.iter().map(|&source| (source, power())))
            .unwrap();
        sources.add_provider(GRID, Unread(setup.grid), [(SourceId::new(GRID, 0), grid())]).unwrap();
        let fetch = FetchPolicy {
            timeout: TIMEOUT,
            providers: setup.policies.into_iter().collect(),
        };
        let (readers, catalog) = sources.split(&fetch).unwrap();

        let (t0, wall0) = (Instant::now(), chrono::DateTime::UNIX_EPOCH);
        let mut tracker = Tracker::new(catalog, PowerUsageEffectiveness::default(), Announcements::default(), wall0);
        let (subscriber, events) = subscription(1 << 16);
        tracker.subscribe(subscriber);
        let (readers, jobs) = Readers::detached(readers, setup.interval);
        let mut engine = Engine::new(tracker, readers);
        engine.open(wall0, t0, wall0);
        let serves = HashMap::from([(POWER, power_sources), (GRID, vec![SourceId::new(GRID, 0)])]);
        Self { engine, jobs, reads: HashMap::new(), serves, events, seen: Vec::new(), t0, wall0, next_span: 1 }
    }

    fn at(&self, ms: u64) -> (Instant, Timestamp) {
        (self.t0 + Duration::from_millis(ms), self.wall0 + TimeDelta::milliseconds(ms as i64))
    }

    fn command(&mut self, ms: u64, message: TrackerMessage) {
        let (now, wall) = self.at(ms);
        self.engine.step(Input::Command(message), now, wall);
    }

    fn start(&mut self, ms: u64, sources: SourceSelection) -> SpanId {
        let id = SpanId::new(self.next_span);
        self.next_span += 1;
        let at = self.at(ms).1;
        self.command(ms, TrackerMessage::Command(TrackerCommand::Start { id, name: "work".to_owned(), parent: None, sources, at }));
        id
    }

    fn stop(&mut self, ms: u64, id: SpanId) {
        let at = self.at(ms).1;
        self.command(ms, TrackerMessage::Command(TrackerCommand::Stop { id, work: Work::default(), at }));
    }

    fn finish(&mut self, ms: u64) -> oneshot::Receiver<Result<SessionReport, TrackerError>> {
        let (reply, report) = oneshot::channel();
        let at = self.at(ms).1;
        self.command(ms, TrackerMessage::Finish { at, reply });
        report
    }

    fn tick(&mut self, ms: u64) {
        let (now, wall) = self.at(ms);
        self.engine.step(Input::Tick, now, wall);
    }

    /// When the engine next wants a tick, in ms from the opening.
    fn wake_at(&self) -> Option<u64> {
        self.engine.wake_at().map(|at| at.saturating_duration_since(self.t0).as_millis() as u64)
    }

    /// How many reads `provider` has been sent so far.
    fn reads(&mut self, provider: ProviderId) -> usize {
        let count = self.reads.entry(provider).or_default();
        while self.jobs[&provider].try_recv().is_ok() {
            *count += 1;
        }
        *count
    }

    /// `provider`'s read in flight returned at `returned` ms with [WATTS] on
    /// each source, and the engine sees it at `seen` ms.
    fn returns(&mut self, provider: ProviderId, returned: u64, seen: u64) {
        let mut response = ProviderResponse::new();
        for &source in &self.serves[&provider] {
            response.samples(source, vec![Sample::new(self.at(returned).1, WATTS)]);
        }
        self.hand_back(provider, Ok(response), returned, seen);
    }

    /// `provider`'s read in flight failed at `returned` ms, seen at `seen` ms.
    fn fails(&mut self, provider: ProviderId, returned: u64, seen: u64) {
        self.hand_back(provider, Err(ProviderError::Unavailable("busy".to_owned())), returned, seen);
    }

    /// `provider`'s read in flight panicked at `returned` ms, seen at `seen` ms.
    fn panics(&mut self, provider: ProviderId, returned: u64, seen: u64) {
        let read = ReadReturn::new(provider, Err("the provider panicked".to_owned()), self.at(returned).0);
        let (now, wall) = self.at(seen);
        self.engine.step(Input::Returned(read), now, wall);
    }

    fn hand_back(&mut self, provider: ProviderId, result: Result<ProviderResponse, ProviderError>, returned: u64, seen: u64) {
        let read = ReadReturn::new(provider, Ok(result), self.at(returned).0);
        let (now, wall) = self.at(seen);
        self.engine.step(Input::Returned(read), now, wall);
    }

    /// Runs the engine until `until` ms: each tick when it is due, and each
    /// read sent returning `takes` ms after it was sent.
    fn run_until(&mut self, until: u64, takes: u64) {
        let mut out: Vec<(u64, ProviderId)> = Vec::new();
        let mut now = 0;
        loop {
            for provider in [POWER, GRID] {
                let before = self.reads.get(&provider).copied().unwrap_or(0);
                for _ in before..self.reads(provider) {
                    out.push((now + takes, provider));
                }
            }
            out.sort_by_key(|&(at, provider)| (at, provider));
            let next_return = out.first().map(|&(at, _)| at);
            let next = [next_return, self.wake_at()].into_iter().flatten().min();
            let Some(next) = next.filter(|&next| next <= until) else { break };
            now = next.max(now);
            if next_return == Some(next) {
                let (at, provider) = out.remove(0);
                self.returns(provider, at, at);
            } else {
                self.tick(now);
            }
        }
    }

    /// Every event announced so far.
    fn events(&mut self) -> &[TrackerEvent] {
        while let Ok(Some(event)) = self.events.try_recv() {
            self.seen.push(event);
        }
        &self.seen
    }

    /// Each source's observations, in the order their readouts landed.
    fn observed(&mut self) -> HashMap<SourceId, Vec<Observation>> {
        let mut observed: HashMap<SourceId, Vec<Observation>> = HashMap::new();
        for event in self.events() {
            if let TrackerEvent::ReadoutLanded { outcomes, .. } = event {
                for (source, observation) in outcomes.iter() {
                    observed.entry(*source).or_default().push(observation.clone());
                }
            }
        }
        observed
    }

    fn settled(&mut self, span: SpanId) -> bool {
        self.events().iter().any(|event| matches!(event, TrackerEvent::SpanSettled { report } if report.span_id == span))
    }
}

fn power_source() -> SourceId {
    SourceId::new(POWER, 0)
}

fn grid_source() -> SourceId {
    SourceId::new(GRID, 0)
}

fn samples(observation: &Observation) -> bool {
    matches!(observation, Observation::Samples(_))
}

/// AC/MEAS-4, AC/PROF-2, AC/MEAS-37
#[test]
fn the_opening_cut_reads_every_provider_at_once() {
    // A cooldown of a minute does not hold the first read, whatever was read while probing.
    let mut driver = Driver::new(Setup { power: after(60_000), grid: after(60_000), ..Setup::default() });
    assert_eq!(driver.reads(POWER), 1, "read before any span");
    assert_eq!(driver.reads(GRID), 1);
}

/// AC/MEAS-98
#[test]
fn a_read_that_returned_in_time_lands_its_samples_however_late_the_engine_sees_it() {
    let mut driver = Driver::new(Setup::default());
    driver.returns(POWER, 950, 1_100);
    driver.tick(1_100);
    assert!(driver.observed()[&power_source()].iter().all(samples), "{:?}", driver.observed());
}

/// AC/MEAS-56, AC/MEAS-98
#[test]
fn a_read_that_returned_past_its_deadline_times_out_however_soon_the_engine_sees_it() {
    let mut driver = Driver::new(Setup::default());
    driver.returns(POWER, 1_050, 1_050);
    assert_eq!(driver.observed()[&power_source()], [Observation::Failed(Failure::TimedOut)]);
}

/// AC/MEAS-38, AC/MEAS-41, AC/ENG-10
#[test]
fn by_default_power_queues_and_the_intensity_skips() {
    let mut driver = Driver::new(Setup { power: after(300), grid: after(300), ..Setup::default() });
    driver.returns(POWER, 5, 5);
    driver.returns(GRID, 5, 5);

    // Inside both cooldowns.
    let span = driver.start(50, SourceSelection::All);
    driver.stop(60, span);
    assert_eq!(driver.reads(POWER), 1, "power queues one request for the cooldown's end");
    driver.run_until(400, 5);

    let observed = driver.observed();
    assert!(observed[&power_source()].iter().all(samples), "power waited for its cooldown: {observed:?}");
    assert!(
        observed[&grid_source()].contains(&Observation::Skipped(Skip::Cooldown)),
        "the intensity skipped: {observed:?}"
    );
}

/// AC/MEAS-39, AC/MEAS-55
#[test]
fn every_request_follows_its_provider_s_policy_and_a_skip_costs_no_read() {
    let mut driver = Driver::new(Setup {
        power: after(60_000),
        power_sources: 2,
        policies: vec![(POWER, UnreadyActions::SKIP)],
        ..Setup::default()
    });
    driver.returns(POWER, 5, 5);
    for (n, slot) in [0, 1].repeat(25).into_iter().enumerate() {
        let at = 10 + 2 * n as u64;
        let span = driver.start(at, SourceSelection::Sources(vec![SourceId::new(POWER, slot)]));
        driver.stop(at + 1, span);
    }

    assert_eq!(driver.reads(POWER), 1, "only the opening read: every cut is inside the cooldown");
    let observed = driver.observed();
    for slot in [0, 1] {
        let seen = &observed[&SourceId::new(POWER, slot)];
        assert!(samples(&seen[0]), "slot {slot}: the opening read first: {seen:?}");
        assert!(seen[1..].iter().all(|o| *o == Observation::Skipped(Skip::Cooldown)), "slot {slot}: {seen:?}");
    }
}

/// AC/MEAS-41
#[test]
fn queue_waits_for_the_next_read_and_skip_answers_on_the_held_value() {
    for (policy, settles_at_once) in [(UnreadyActions::QUEUE, false), (UnreadyActions::SKIP, true)] {
        // The grid skips inside its cooldown, so only power decides when the span settles.
        let mut driver = Driver::new(Setup {
            power: after(300),
            grid: after(60_000),
            policies: vec![(POWER, policy)],
            ..Setup::default()
        });
        driver.returns(POWER, 5, 5);
        driver.returns(GRID, 5, 5);
        let span = driver.start(50, SourceSelection::All);
        driver.stop(60, span);
        assert_eq!(driver.settled(span), settles_at_once, "{policy:?} at the stop");

        driver.run_until(299, 5);
        assert_eq!(driver.settled(span), settles_at_once, "{policy:?} before the cooldown ends");
        driver.run_until(400, 5);
        assert!(driver.settled(span), "{policy:?} once the read after the cooldown returned");
    }
}

/// AC/MEAS-5
#[test]
fn a_cut_reads_a_provider_once_however_many_of_its_sources_it_cuts() {
    let mut driver = Driver::new(Setup { power_sources: 2, ..Setup::default() });
    driver.returns(POWER, 5, 5);
    let span = driver.start(50, SourceSelection::All);
    assert_eq!(driver.reads(POWER), 2);
    driver.returns(POWER, 55, 55);
    driver.stop(100, span);
    assert_eq!(driver.reads(POWER), 3);
    driver.returns(POWER, 105, 105);
    driver.finish(150);
    assert_eq!(driver.reads(POWER), 4, "the opening cut, the start, the stop and the finish");
}

/// AC/MEAS-29, AC/MEAS-61, AC/MEAS-64, AC/MEAS-67, AC/ENG-43, AC/ENG-45
#[test]
fn the_watchdog_asks_each_provider_on_its_own_period() {
    let mut driver = Driver::new(Setup {
        power: after(10),
        grid: after(300),
        interval: Duration::from_millis(100),
        ..Setup::default()
    });
    // Power every 100 ms; the grid every 300 ms, never inside its cooldown.
    driver.run_until(720, 1);
    assert_eq!(driver.reads(POWER), 8, "at 0, 100, ..., 700 ms");
    assert_eq!(driver.reads(GRID), 3, "at 0, 300 and 600 ms");
}

/// AC/MEAS-15, AC/MEAS-35, AC/MEAS-58
#[test]
fn a_provider_read_once_is_never_read_again() {
    let mut driver = Driver::new(Setup { grid: Cooldown::Never, ..Setup::default() });
    driver.returns(GRID, 5, 5);
    driver.returns(POWER, 5, 5);
    for n in 0..5 {
        let span = driver.start(10 + 20 * n, SourceSelection::All);
        driver.stop(20 + 20 * n, span);
    }
    driver.finish(200);
    assert_eq!(driver.reads(GRID), 1, "its opening read answers it; later cuts send it nothing");
}

/// AC/PROF-2, AC/PROF-44, AC/MEAS-84
#[test]
fn a_session_with_only_finish_bills_the_root_between_the_opening_and_finishing_reads() {
    let mut driver = Driver::new(Setup::default());
    driver.returns(POWER, 5, 5);
    driver.returns(GRID, 5, 5);
    let mut report = driver.finish(200);
    assert_eq!(driver.reads(POWER), 2, "the opening read and the finishing read");
    driver.returns(POWER, 205, 205);
    driver.returns(GRID, 205, 205);

    let report = report.try_recv().expect("finished").expect("a report");
    let expected = WATTS * 0.2 * PowerUsageEffectiveness::default().get();
    let energy = report.total.energy.get();
    assert!((energy - expected).abs() < 1e-9, "{energy} J against {expected} J");
}

/// When `finish` is answered, in ms, with every read returning `takes` ms
/// after it was sent.
fn finished_at(driver: &mut Driver, mut report: oneshot::Receiver<Result<SessionReport, TrackerError>>, takes: u64) -> u64 {
    for until in (0..10_000).step_by(1) {
        driver.run_until(until, takes);
        if report.try_recv().is_ok() {
            return until;
        }
    }
    panic!("finish was never answered");
}

/// AC/ENG-27, AC/MEAS-54
#[test]
fn finishing_waits_no_longer_than_the_slowest_queued_request() {
    let mut driver = Driver::new(Setup { power: after(300), grid: after(60_000), ..Setup::default() });
    driver.returns(POWER, 1, 1);
    driver.returns(GRID, 1, 1);
    for n in 0..20 {
        let span = driver.start(10 + n, SourceSelection::All);
        driver.stop(10 + n, span);
    }
    let report = driver.finish(40);
    let bound = 40 + 300_u64.max(TIMEOUT.as_millis() as u64) + TIMEOUT.as_millis() as u64;
    let answered = finished_at(&mut driver, report, 5);
    assert!(answered <= bound, "answered at {answered} ms against {bound} ms");
}

/// AC/ENG-27
#[test]
fn finishing_waits_no_longer_than_the_timeout_for_a_hung_read() {
    for (policy, bound) in [(UnreadyActions::SKIP, TIMEOUT), (UnreadyActions::QUEUE, 2 * TIMEOUT)] {
        let mut driver = Driver::new(Setup { policies: vec![(POWER, policy)], ..Setup::default() });
        driver.returns(GRID, 1, 1);
        // The opening power read never returns.
        let span = driver.start(10, SourceSelection::All);
        driver.stop(20, span);
        let mut report = driver.finish(30);
        while let Some(at) = driver.wake_at() {
            driver.tick(at);
            if report.try_recv().is_ok() {
                assert!(at <= 30 + bound.as_millis() as u64, "{policy:?}: answered at {at} ms");
                break;
            }
        }
        assert!(driver.engine.is_done(), "{policy:?}: the session ended");
    }
}

/// AC/MEAS-96
#[test]
fn under_queue_a_failed_read_answers_its_request_and_is_not_sent_again() {
    let mut driver = Driver::new(Setup::default());
    driver.fails(POWER, 5, 5);
    driver.returns(GRID, 5, 5);
    let span = driver.start(10, SourceSelection::All);
    driver.fails(POWER, 15, 15);
    driver.returns(GRID, 15, 15);
    driver.stop(20, span);
    driver.fails(POWER, 25, 25);
    driver.returns(GRID, 25, 25);
    assert!(driver.settled(span), "settled on the failed reads, with no wait");
    assert_eq!(driver.reads(POWER), 3, "one read per cut, none sent again");
    let power = &driver.observed()[&power_source()];
    assert!(power.iter().all(|o| matches!(o, Observation::Failed(Failure::Provider(_)))), "{power:?}");
}

/// AC/MEAS-1, AC/MEAS-14
#[test]
fn each_request_lands_once_and_cuts_inside_a_cooldown_send_one() {
    let mut driver = Driver::new(Setup { power: after(300), grid: after(60_000), ..Setup::default() });
    driver.returns(POWER, 1, 1);
    driver.returns(GRID, 1, 1);
    for n in 0..20 {
        let span = driver.start(10 + 2 * n, SourceSelection::All);
        driver.stop(11 + 2 * n, span);
    }
    driver.run_until(600, 5);
    assert_eq!(driver.reads(POWER), 2, "40 cuts in one cooldown send one request after the opening one");

    let mut ids: Vec<_> = driver
        .events()
        .iter()
        .filter_map(|event| match event {
            TrackerEvent::ReadoutLanded { readout, .. } => Some(*readout),
            _ => None,
        })
        .collect();
    let landed = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), landed, "no request lands twice");
}

/// How many of `source`'s observations failed as `failure` says.
fn failed(driver: &mut Driver, source: SourceId, failure: impl Fn(&Failure) -> bool) -> usize {
    driver.observed().get(&source).map_or(0, |observations| {
        observations.iter().filter(|o| matches!(o, Observation::Failed(f) if failure(f))).count()
    })
}

/// AC/MEAS-9, AC/MEAS-14, AC/MEAS-34
#[test]
fn cuts_to_a_retired_provider_send_it_one_request() {
    let mut driver = Driver::new(Setup::default());
    driver.panics(POWER, 5, 5);
    driver.returns(GRID, 5, 5);
    for n in 0..5 {
        let span = driver.start(10 + 10 * n, SourceSelection::All);
        driver.stop(10 + 10 * n, span);
    }
    driver.run_until(100, 1);
    assert_eq!(failed(&mut driver, power_source(), |f| matches!(f, Failure::Retired)), 1, "{:?}", driver.observed()[&power_source()]);
}

/// AC/MEAS-12, AC/MEAS-98
#[test]
fn a_cut_handled_after_a_timeout_it_has_not_seen_relies_on_no_answer_from_before_it() {
    let mut driver = Driver::new(Setup { policies: vec![(POWER, UnreadyActions::SKIP)], ..Setup::default() });
    driver.returns(GRID, 1, 1);
    // Busy: the cut's request is skipped, and relied on while the read is out.
    let span = driver.start(100, SourceSelection::All);
    assert_eq!(driver.observed()[&power_source()].len(), 1, "skipped");
    // The read times out at 1000 ms; the engine handles nothing until 1500 ms.
    driver.stop(1500, span);
    let power = driver.observed()[&power_source()].clone();
    assert!(matches!(power.as_slice(), [Observation::Skipped(_), Observation::Failed(Failure::TimedOut), Observation::Failed(Failure::Stalled)]), "{power:?}");
}
