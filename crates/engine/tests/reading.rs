//! How the engine reads its providers, through the public API: reads that
//! hang, panic or are slow, on threads of their own, under the fetch policy.
//! Timeouts and cooldowns are tens of milliseconds; every check of time is an
//! upper bound with slack, never an exact time.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use carbontracker::{
    SubscriptionEnded, UnreadyActions, ConfigError, Cooldown, Failure, FetchPolicy, Fidelity, GridLocation, Observation, PowerDomain,
    PowerScope, Provider, ProviderError, ProviderId, ProviderResponse, Sample, SampleKind, Skip,
    SourceDescription, SourceId, SourceSelection, Sources, Subscription, TrackerConfig,
    TrackerEvent, TrackerHandle, TrackerQuery, Work, start, start_subscribed,
};

const TIMEOUT: Duration = Duration::from_millis(100);
const WAIT: Duration = Duration::from_secs(5);
const POWER: ProviderId = ProviderId(1);
const GRID: ProviderId = ProviderId(2);

fn source(provider: ProviderId) -> SourceId {
    SourceId::new(provider, 0)
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
    SourceDescription::Intensity {
        location: GridLocation::Global,
        fidelity: Fidelity::Stated,
    }
}

/// What a test provider does on each read.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Answer at once.
    Answer,
    /// Wait for the test to release it.
    Block,
    Panic,
}

/// Where each test provider's code ran, by thread name.
#[derive(Clone, Default)]
struct Threads(Arc<Mutex<Vec<(&'static str, String)>>>);

impl Threads {
    fn record(&self, what: &'static str) {
        let name = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_owned();
        self.0.lock().unwrap().push((what, name));
    }

    fn of(&self, what: &'static str) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter(|(done, _)| *done == what)
            .map(|(_, name)| name.clone())
            .collect()
    }

    /// Wait until `what` was recorded.
    fn wait_for(&self, what: &'static str) -> Vec<String> {
        let deadline = Instant::now() + WAIT;
        while self.of(what).is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.of(what)
    }
}

/// A provider of one source, reading `value`.
struct Test {
    source: SourceId,
    value: f64,
    behaviour: Behaviour,
    cooldown: Cooldown,
    release: Option<Mutex<mpsc::Receiver<()>>>,
    reads: Arc<AtomicUsize>,
    threads: Threads,
}

impl Test {
    fn new(provider: ProviderId, behaviour: Behaviour) -> Self {
        Self {
            source: source(provider),
            value: 10.0,
            behaviour,
            cooldown: Cooldown::After(Duration::ZERO),
            release: None,
            reads: Arc::default(),
            threads: Threads::default(),
        }
    }

    fn cooling(mut self, cooldown: Cooldown) -> Self {
        self.cooldown = cooldown;
        self
    }

    /// Blocks each read until the returned sender sends, or is dropped.
    fn blocking(mut self) -> (Self, mpsc::Sender<()>) {
        let (release, wait) = mpsc::channel();
        self.behaviour = Behaviour::Block;
        self.release = Some(Mutex::new(wait));
        (self, release)
    }
}

impl Provider for Test {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        self.threads.record("read");
        self.reads.fetch_add(1, Ordering::SeqCst);
        match self.behaviour {
            Behaviour::Answer => {}
            Behaviour::Block => {
                let _ = self
                    .release
                    .as_ref()
                    .expect("a blocking provider")
                    .lock()
                    .unwrap()
                    .recv();
            }
            Behaviour::Panic => panic!("the driver exploded"),
        }
        let mut response = ProviderResponse::new();
        response.samples(
            self.source,
            vec![Sample::new(chrono::Utc::now(), self.value)],
        );
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        self.threads.record("cooldown");
        self.cooldown
    }
}

impl Drop for Test {
    fn drop(&mut self) {
        self.threads.record("drop");
    }
}

fn config() -> TrackerConfig {
    TrackerConfig {
        readout_interval: Duration::from_secs(60),
        fetch: FetchPolicy {
            timeout: TIMEOUT,
            ..FetchPolicy::default()
        },
        ..TrackerConfig::default()
    }
}

/// A tracker over `power` and `grid`.
fn tracker(power_provider: Test, grid_provider: Test) -> TrackerHandle {
    tracker_with(power_provider, grid_provider, config())
}

fn tracker_with(power_provider: Test, grid_provider: Test, config: TrackerConfig) -> TrackerHandle {
    let mut sources = Sources::new();
    sources
        .add_provider(POWER, power_provider, [(source(POWER), power())])
        .unwrap();
    sources
        .add_provider(GRID, grid_provider, [(source(GRID), grid())])
        .unwrap();
    start(sources, config).expect("a valid config")
}

/// Each source's observations, in the order their readouts landed, until
/// the session ends.
async fn observed(events: &mut Subscription) -> BTreeMap<SourceId, Vec<Observation>> {
    tokio::time::timeout(WAIT, async {
        let mut observed: BTreeMap<SourceId, Vec<Observation>> = BTreeMap::new();
        while let Ok(event) = events.recv().await {
            match event {
                TrackerEvent::ReadoutLanded { outcomes, .. } => {
                    for (id, observation) in outcomes.iter() {
                        observed.entry(*id).or_default().push(observation.clone());
                    }
                }
                TrackerEvent::SessionEnded { .. } => break,
                _ => {}
            }
        }
        observed
    })
    .await
    .expect("the session ends")
}

/// AC/MEAS-3, AC/MEAS-24, AC/MEAS-25, AC/MEAS-30, AC/ENG-37
#[tokio::test]
async fn a_read_that_hangs_delays_nothing_else() {
    let (hanging, _release) = Test::new(POWER, Behaviour::Answer).blocking();
    let grid_provider = Test::new(GRID, Behaviour::Answer);
    // Open requests wait for the hung read up to retry_for, then give up.
    let retry_for = TIMEOUT * 3;
    let config = TrackerConfig { fetch: FetchPolicy { retry_for, ..config().fetch }, ..config() };
    let tracker = tracker_with(hanging, grid_provider, config);
    let mut events = tracker.subscribe();
    let mut cuts = tracker.subscribe();

    let began = Instant::now();
    let span = tracker.start_span("work", None, SourceSelection::All);
    let (reply, answer) = oneshot::channel();
    tracker.query(TrackerQuery::Catalog { reply });
    answer.await.unwrap().expect("a query is answered while the read hangs");
    assert!(began.elapsed() < TIMEOUT, "the query did not wait on the read");
    tracker.mark(span, Work::default());
    while !matches!(cuts.recv().await, Ok(TrackerEvent::Marked { .. })) {}
    assert!(began.elapsed() < TIMEOUT, "events were delivered while the read hangs");

    tracker.stop_span(span, Work::default());
    tracker.finish().await.expect("the session finishes despite the hung read");
    assert!(began.elapsed() < retry_for + TIMEOUT * 5, "nothing waited past retry_for");

    let observed = observed(&mut events).await;
    let power = &observed[&source(POWER)];
    assert_eq!(power[0], Observation::Failed(Failure::TimedOut), "the opening read timed out");
    // Each failed read lands as it happens; each open request gives up with
    // its last at retry_for. Nothing was read while the read hung.
    assert!(
        power[1..].iter().all(|o| matches!(o, Observation::Failed(Failure::Stalled | Failure::TimedOut))),
        "the hung read is still out: {power:?}"
    );
    assert!(
        observed[&source(GRID)].iter().any(|o| matches!(o, Observation::Samples(_))),
        "the grid was read meanwhile"
    );
}

/// AC/MEAS-26, AC/MEAS-27, AC/MEAS-57
#[tokio::test]
async fn a_panicking_provider_is_retired_alone() {
    let tracker = tracker(Test::new(POWER, Behaviour::Panic), Test::new(GRID, Behaviour::Answer));
    let mut events = tracker.subscribe();

    let span = tracker.start_span("work", None, SourceSelection::All);
    tracker.stop_span(span, Work::default());
    tracker.finish().await.expect("the session finishes");

    let observed = observed(&mut events).await;
    let power = &observed[&source(POWER)];
    let panicked = Observation::Failed(Failure::Panicked("the driver exploded".to_owned()));
    // Requests out when the read panicked fail with it; later ones are retired.
    assert!(!power.is_empty());
    assert!(
        power.iter().all(|o| *o == panicked || *o == Observation::Failed(Failure::Retired)),
        "{power:?}"
    );
    assert!(
        observed[&source(GRID)].iter().all(|o| matches!(o, Observation::Samples(_))),
        "the other provider is unaffected"
    );
}

/// AC/MEAS-30, AC/MEAS-60, AC/ENG-41, AC/ENG-44
#[tokio::test]
async fn no_provider_code_runs_on_the_tracker_thread() {
    let power_provider = Test::new(POWER, Behaviour::Answer);
    let threads = power_provider.threads.clone();
    let tracker = tracker(power_provider, Test::new(GRID, Behaviour::Answer));
    let span = tracker.start_span("work", None, SourceSelection::All);
    tracker.stop_span(span, Work::default());
    tracker.finish().await.expect("the session finishes");

    let dropped = threads.wait_for("drop");
    assert_eq!(threads.of("cooldown").len(), 1, "the cooldown is asked once");
    for (what, names) in [
        ("cooldown", threads.of("cooldown")),
        ("read", threads.of("read")),
        ("drop", dropped),
    ] {
        assert!(!names.is_empty(), "{what} ran");
        assert!(
            names.iter().all(|name| name != "carbontracker"),
            "{what} ran on the tracker thread: {names:?}"
        );
    }
}

/// AC/MEAS-56, AC/MEAS-60, AC/ENG-37, AC/ENG-41
#[tokio::test]
async fn a_provider_out_on_an_abandoned_read_is_dropped_when_it_returns() {
    let (hanging, release) = Test::new(POWER, Behaviour::Answer).blocking();
    let threads = hanging.threads.clone();
    let tracker = tracker(hanging, Test::new(GRID, Behaviour::Answer));
    let span = tracker.start_span("work", None, SourceSelection::All);
    tracker.stop_span(span, Work::default());
    tracker
        .finish()
        .await
        .expect("the session finishes despite the hung read");
    assert!(threads.of("drop").is_empty(), "still out on its read");

    release.send(()).unwrap();
    let dropped = threads.wait_for("drop");
    assert!(
        dropped
            .iter()
            .all(|name| name.starts_with("carbontracker-read")),
        "dropped by its read: {dropped:?}"
    );
}

/// AC/ENG-27, AC/MEAS-54
#[tokio::test]
async fn finishing_waits_no_longer_than_the_slowest_queued_request() {
    let cooldown = Duration::from_millis(300);
    let power_provider = Test::new(POWER, Behaviour::Answer).cooling(Cooldown::After(cooldown));
    // The grid skips, so its minute-long cooldown never holds the finish.
    let grid_provider =
        Test::new(GRID, Behaviour::Answer).cooling(Cooldown::After(Duration::from_secs(60)));
    let tracker = tracker(power_provider, grid_provider);

    for _ in 0..20 {
        let span = tracker.start_span("burst", None, SourceSelection::All);
        tracker.stop_span(span, Work::default());
    }
    let began = Instant::now();
    tracker.finish().await.expect("the session finishes");
    let bound = cooldown.max(TIMEOUT) + TIMEOUT;
    assert!(
        began.elapsed() < bound + Duration::from_millis(200),
        "{:?} against {bound:?}",
        began.elapsed()
    );
}

/// AC/ENG-2, AC/MEAS-38
#[test]
fn a_zero_timeout_or_a_policy_for_an_unknown_provider_is_refused() {
    let zero = TrackerConfig {
        fetch: FetchPolicy {
            timeout: Duration::ZERO,
            ..FetchPolicy::default()
        },
        ..config()
    };
    assert_eq!(zero.validate(), Err(ConfigError::ZeroFetchTimeout));
    assert_eq!(FetchPolicy::default().timeout, Duration::from_secs(10));
    // AC/MEAS-94
    assert_eq!(FetchPolicy::default().retry_for, Duration::from_secs(30));

    let no_interval = TrackerConfig { readout_interval: Duration::ZERO, ..config() };
    assert_eq!(no_interval.validate(), Err(ConfigError::ZeroReadoutInterval));
    let mut sources = Sources::new();
    sources.add_provider(POWER, Test::new(POWER, Behaviour::Answer), [(source(POWER), power())]).unwrap();
    assert_eq!(start(sources, no_interval).err(), Some(ConfigError::ZeroReadoutInterval));

    let mut unknown = config();
    unknown
        .fetch
        .providers
        .insert(ProviderId(99), carbontracker::UnreadyActions::SKIP);
    let mut sources = Sources::new();
    sources
        .add_provider(
            POWER,
            Test::new(POWER, Behaviour::Answer),
            [(source(POWER), power())],
        )
        .unwrap();
    assert_eq!(
        start(sources, unknown).err(),
        Some(ConfigError::UnknownProvider(ProviderId(99)))
    );
}

/// AC/MEAS-38, AC/MEAS-41, AC/ENG-10
#[tokio::test]
async fn by_default_power_queues_and_the_intensity_skips() {
    let cooldown = Cooldown::After(Duration::from_millis(300));
    let tracker = tracker(
        Test::new(POWER, Behaviour::Answer).cooling(cooldown),
        Test::new(GRID, Behaviour::Answer).cooling(cooldown),
    );
    let mut events = tracker.subscribe();

    // Inside both cooldowns, after the opening reads have started.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let span = tracker.start_span("work", None, SourceSelection::All);
    tracker.stop_span(span, Work::default());
    tracker.finish().await.expect("the session finishes");

    let observed = observed(&mut events).await;
    assert!(
        observed[&source(POWER)].iter().all(|o| matches!(o, Observation::Samples(_))),
        "power waited for its cooldown"
    );
    assert!(
        observed[&source(GRID)]
            .iter()
            .any(|o| matches!(o, Observation::Skipped(Skip::Cooldown | Skip::Busy))),
        "the intensity skipped"
    );
}

/// AC/MEAS-15, AC/MEAS-35, AC/MEAS-58
#[tokio::test]
async fn a_provider_read_once_is_never_read_again() {
    let fixed = Test::new(GRID, Behaviour::Answer).cooling(Cooldown::Never);
    let reads = fixed.reads.clone();
    let tracker = tracker(Test::new(POWER, Behaviour::Answer), fixed);
    let mut events = tracker.subscribe();

    for _ in 0..5 {
        let span = tracker.start_span("later", None, SourceSelection::All);
        tracker.stop_span(span, Work::default());
    }
    tracker.finish().await.expect("the session finishes");

    // Its opening read answers it; later cuts send it nothing.
    let observed = observed(&mut events).await;
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert!(observed.get(&source(GRID)).is_none_or(|grid| grid.len() <= 1));
}

/// AC/MEAS-4, AC/PROF-2
#[tokio::test]
async fn the_opening_cut_reads_every_provider() {
    let power_provider = Test::new(POWER, Behaviour::Answer);
    let grid_provider = Test::new(GRID, Behaviour::Answer);
    let (power_reads, grid_reads) = (power_provider.reads.clone(), grid_provider.reads.clone());
    let _tracker = tracker(power_provider, grid_provider);

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(power_reads.load(Ordering::SeqCst), 1, "read before any span");
    assert_eq!(grid_reads.load(Ordering::SeqCst), 1);
}

/// AC/MEAS-29, AC/MEAS-61, AC/MEAS-64, AC/MEAS-67, AC/ENG-43, AC/ENG-45
#[tokio::test]
async fn the_watchdog_asks_each_provider_on_its_own_period() {
    let power_provider = Test::new(POWER, Behaviour::Answer).cooling(Cooldown::After(Duration::from_millis(10)));
    let grid_provider = Test::new(GRID, Behaviour::Answer).cooling(Cooldown::After(Duration::from_millis(300)));
    let (power_reads, grid_reads) = (power_provider.reads.clone(), grid_provider.reads.clone());
    let config = TrackerConfig { readout_interval: Duration::from_millis(100), ..config() };
    let _tracker = tracker_with(power_provider, grid_provider, config);

    // 1 opening read each, then power every 100 ms and the grid every 300 ms.
    tokio::time::sleep(Duration::from_millis(720)).await;
    let (power, grid) = (power_reads.load(Ordering::SeqCst), grid_reads.load(Ordering::SeqCst));
    assert!((6..=9).contains(&power), "power read {power} times");
    assert!((2..=4).contains(&grid), "the grid read {grid} times, never inside its cooldown");
}

/// AC/PROF-2, AC/PROF-44, AC/MEAS-84
#[tokio::test]
async fn a_session_with_only_finish_bills_the_root_between_the_opening_and_finishing_reads() {
    let power_provider = Test::new(POWER, Behaviour::Answer);
    let reads = power_provider.reads.clone();
    let tracker = tracker(power_provider, Test::new(GRID, Behaviour::Answer));

    tokio::time::sleep(Duration::from_millis(200)).await;
    let report = tracker.finish().await.expect("the session finishes");

    assert_eq!(reads.load(Ordering::SeqCst), 2, "the opening read and the finishing read");
    let expected = 10.0 * report.duration.as_secs_f64() * report_pue();
    let energy = report.total.energy.get();
    assert!((energy - expected).abs() < 0.05 * expected, "{energy} J against {expected} J");
}

fn report_pue() -> f64 {
    TrackerConfig::default().pue.get()
}

/// AC/ENG-28
#[tokio::test]
async fn dropping_every_handle_ends_the_session_with_no_report() {
    let power_provider = Test::new(POWER, Behaviour::Answer);
    let threads = power_provider.threads.clone();
    let tracker = tracker(power_provider, Test::new(GRID, Behaviour::Answer));
    let mut events = tracker.subscribe();
    tracker.start_span("work", None, SourceSelection::All);
    drop(tracker);

    let ended = tokio::time::timeout(WAIT, async {
        loop {
            match events.recv().await {
                Ok(TrackerEvent::SessionEnded { .. }) => panic!("no session report without finish"),
                Ok(_) => {}
                Err(ended) => break ended,
            }
        }
    })
    .await
    .expect("the stream ends");
    assert_eq!(ended, SubscriptionEnded::Closed);
    assert!(!threads.wait_for("drop").is_empty(), "the provider is dropped");
}

/// AC/ENG-27
#[tokio::test]
async fn finishing_waits_no_longer_than_the_timeout_for_a_provider_that_skips() {
    let (hanging, _release) = Test::new(POWER, Behaviour::Answer).blocking();
    let mut config = config();
    config.fetch.providers.insert(POWER, UnreadyActions::SKIP);
    let tracker = tracker_with(hanging, Test::new(GRID, Behaviour::Answer), config);

    let span = tracker.start_span("work", None, SourceSelection::All);
    tracker.stop_span(span, Work::default());
    let began = Instant::now();
    tracker.finish().await.expect("the session finishes despite the hung read");
    assert!(began.elapsed() < TIMEOUT + Duration::from_millis(50), "{:?}", began.elapsed());
}

/// The provider each `ReadoutLanded` came from, in the order they landed,
/// with the sources it observed, until the session ends.
async fn landed(events: &mut Subscription) -> Vec<(ProviderId, carbontracker::ReadoutId, Vec<SourceId>)> {
    tokio::time::timeout(WAIT, async {
        let mut landed = Vec::new();
        while let Ok(event) = events.recv().await {
            match event {
                TrackerEvent::ReadoutLanded { readout, outcomes } => {
                    let mut sources: Vec<SourceId> = outcomes.keys().copied().collect();
                    sources.sort();
                    landed.push((sources[0].provider(), readout, sources));
                }
                TrackerEvent::SessionEnded { .. } => break,
                _ => {}
            }
        }
        landed
    })
    .await
    .expect("the session ends")
}

/// AC/MEAS-3
#[tokio::test]
async fn readouts_land_in_the_order_their_reads_return() {
    let (power_provider, release_power) = Test::new(POWER, Behaviour::Answer).blocking();
    let (grid_provider, release_grid) = Test::new(GRID, Behaviour::Answer).blocking();
    let (power_threads, grid_threads) = (power_provider.threads.clone(), grid_provider.threads.clone());
    let config = TrackerConfig { fetch: FetchPolicy { timeout: Duration::from_secs(2), ..FetchPolicy::default() }, ..config() };
    let tracker = tracker_with(power_provider, grid_provider, config);
    let mut events = tracker.subscribe();

    assert!(!power_threads.wait_for("read").is_empty() && !grid_threads.wait_for("read").is_empty(), "both reads out at once");
    // Dropped, each sender releases every read from then on.
    drop(release_grid);
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(release_power);
    tracker.finish().await.expect("the session finishes");

    let order: Vec<ProviderId> = landed(&mut events).await.into_iter().map(|(provider, ..)| provider).collect();
    assert_eq!(order[..2], [GRID, POWER], "the grid returned first: {order:?}");
}

/// A provider of two power sources, slots 0 and 1, counting its reads.
struct Pair {
    reads: Arc<AtomicUsize>,
    cooldown: Cooldown,
}

impl Provider for Pair {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut response = ProviderResponse::new();
        for slot in 0..2 {
            response.samples(SourceId::new(POWER, slot), vec![Sample::new(chrono::Utc::now(), 10.0)]);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        self.cooldown
    }
}

/// A tracker over a [Pair] and the grid, and the pair's read count.
fn pair_tracker(cooldown: Cooldown, config: TrackerConfig) -> (TrackerHandle, Arc<AtomicUsize>) {
    let reads = Arc::<AtomicUsize>::default();
    let mut sources = Sources::new();
    let pair = Pair { reads: reads.clone(), cooldown };
    sources
        .add_provider(POWER, pair, [(SourceId::new(POWER, 0), power()), (SourceId::new(POWER, 1), power())])
        .unwrap();
    sources.add_provider(GRID, Test::new(GRID, Behaviour::Answer), [(source(GRID), grid())]).unwrap();
    (start(sources, config).expect("a valid config"), reads)
}

/// AC/MEAS-5
#[tokio::test]
async fn a_cut_reads_a_provider_once_however_many_of_its_sources_it_cuts() {
    let (tracker, reads) = pair_tracker(Cooldown::After(Duration::ZERO), config());
    let settle = Duration::from_millis(50);
    tokio::time::sleep(settle).await;
    let span = tracker.start_span("both", None, SourceSelection::All);
    tokio::time::sleep(settle).await;
    tracker.stop_span(span, Work::default());
    tokio::time::sleep(settle).await;
    tracker.finish().await.expect("the session finishes");

    assert_eq!(reads.load(Ordering::SeqCst), 4, "the opening cut, the start, the stop and the finish");
}

/// AC/MEAS-39, AC/MEAS-55
#[tokio::test]
async fn every_request_follows_its_provider_s_policy_and_a_skip_costs_no_read() {
    let mut config = config();
    config.fetch.providers.insert(POWER, UnreadyActions::SKIP);
    let (tracker, reads) = pair_tracker(Cooldown::After(Duration::from_secs(60)), config);
    let mut events = tracker.subscribe();

    tokio::time::sleep(Duration::from_millis(50)).await;
    for slot in [0, 1].repeat(25) {
        let span = tracker.start_span("one", None, SourceSelection::Sources(vec![SourceId::new(POWER, slot)]));
        tracker.stop_span(span, Work::default());
    }
    tracker.finish().await.expect("the session finishes");

    assert_eq!(reads.load(Ordering::SeqCst), 1, "only the opening read: every cut is inside the cooldown");
    let observed = observed(&mut events).await;
    for slot in [0, 1] {
        let seen = &observed[&SourceId::new(POWER, slot)];
        // The opening read's samples, if they landed after subscribing, then every cut skipped.
        let skipped: Vec<_> = seen.iter().filter(|o| !matches!(o, Observation::Samples(_))).collect();
        assert!(seen.len() - skipped.len() <= 1, "slot {slot}: {seen:?}");
        assert!(!skipped.is_empty() && skipped.iter().all(|o| **o == Observation::Skipped(Skip::Cooldown)), "slot {slot}: {seen:?}");
    }
}

/// AC/MEAS-37
#[tokio::test]
async fn a_read_made_while_probing_does_not_hold_the_first_read() {
    let mut probed = Test::new(POWER, Behaviour::Answer).cooling(Cooldown::After(Duration::from_secs(60)));
    probed.read().expect("the probe reads it");
    let reads = probed.reads.clone();
    let _tracker = tracker(probed, Test::new(GRID, Behaviour::Answer));

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(reads.load(Ordering::SeqCst), 2, "read at the opening cut, inside the probe's cooldown");
}

/// AC/MEAS-41
#[tokio::test]
async fn queue_waits_for_the_next_read_and_skip_answers_on_the_held_value() {
    let cooldown = Duration::from_millis(300);
    let settles_after_stop = |action: UnreadyActions| async move {
        let mut config = config();
        config.fetch.providers.insert(POWER, action);
        let tracker = tracker_with(
            Test::new(POWER, Behaviour::Answer).cooling(Cooldown::After(cooldown)),
            Test::new(GRID, Behaviour::Answer).cooling(Cooldown::After(Duration::from_secs(60))),
            config,
        );
        let mut events = tracker.subscribe();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let span = tracker.start_span("work", None, SourceSelection::All);
        tracker.stop_span(span, Work::default());
        let stopped = Instant::now();
        tokio::time::timeout(WAIT, async {
            loop {
                if let Ok(TrackerEvent::SpanSettled { report }) = events.recv().await
                    && report.span_id == span
                {
                    break;
                }
            }
        })
        .await
        .expect("the span settles");
        stopped.elapsed()
    };

    let queued = settles_after_stop(UnreadyActions::QUEUE).await;
    let skipped = settles_after_stop(UnreadyActions::SKIP).await;
    assert!(queued >= Duration::from_millis(150), "Queue waits for the read after the cooldown: {queued:?}");
    assert!(skipped < Duration::from_millis(100), "Skip answers at once: {skipped:?}");
}

/// AC/MEAS-1, AC/MEAS-14
#[tokio::test]
async fn each_request_lands_once_and_cuts_inside_a_cooldown_send_one() {
    let tracker = tracker(
        Test::new(POWER, Behaviour::Answer).cooling(Cooldown::After(Duration::from_millis(300))),
        Test::new(GRID, Behaviour::Answer).cooling(Cooldown::After(Duration::from_secs(60))),
    );
    let mut events = tracker.subscribe();
    for _ in 0..20 {
        let span = tracker.start_span("burst", None, SourceSelection::All);
        tracker.stop_span(span, Work::default());
    }
    tracker.finish().await.expect("the session finishes");

    let landed = landed(&mut events).await;
    let mut ids: Vec<_> = landed.iter().map(|(_, id, _)| *id).collect();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), landed.len(), "no request lands twice");
    for (provider, _, sources) in &landed {
        assert_eq!(*sources, vec![source(*provider)], "an observation for each source it serves");
    }
    let power = landed.iter().filter(|(provider, ..)| *provider == POWER).count();
    assert!(power <= 2, "40 cuts sent the power provider {power} requests after the opening one");
}

/// AC/ENG-65
#[tokio::test]
async fn a_subscription_taken_at_start_sees_the_opening_readouts_however_late_it_is_read() {
    let mut sources = Sources::new();
    let power_provider = Test::new(POWER, Behaviour::Answer).cooling(Cooldown::Never);
    let grid_provider = Test::new(GRID, Behaviour::Answer).cooling(Cooldown::Never);
    sources.add_provider(POWER, power_provider, [(source(POWER), power())]).unwrap();
    sources.add_provider(GRID, grid_provider, [(source(GRID), grid())]).unwrap();
    let (tracker, mut events) = start_subscribed(sources, config()).expect("a valid config");
    // Both opening reads return long before the caller looks.
    tokio::time::sleep(Duration::from_millis(50)).await;
    tracker.finish().await.expect("the session finishes");
    let providers: Vec<ProviderId> = landed(&mut events).await.into_iter().map(|(provider, ..)| provider).collect();
    assert!(providers.contains(&POWER) && providers.contains(&GRID), "landed {providers:?}");
}

/// AC/MEAS-1, AC/MEAS-4
#[tokio::test]
async fn a_subscriber_from_the_start_sees_the_opening_readouts() {
    // Read once each: a missed opening readout is never made up for.
    for run in 0..200 {
        let tracker = tracker(
            Test::new(POWER, Behaviour::Answer).cooling(Cooldown::Never),
            Test::new(GRID, Behaviour::Answer).cooling(Cooldown::Never),
        );
        let mut events = tracker.subscribe();
        tracker.finish().await.expect("the session finishes");
        let providers: Vec<ProviderId> = landed(&mut events).await.into_iter().map(|(provider, ..)| provider).collect();
        assert!(providers.contains(&POWER) && providers.contains(&GRID), "run {run} landed {providers:?}");
    }
}
