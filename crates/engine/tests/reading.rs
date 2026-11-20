//! How the engine reads its providers on real threads, through the public
//! API: reads that hang, panic or return out of order, on worker threads of
//! their own. What depends only on timing (policies, cooldowns, the watchdog,
//! how long a finish waits) is checked exactly in the crate's `tests::engine`;
//! the bounds here leave seconds of slack, so a busy host cannot break them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use tokio::sync::oneshot;

use carbontracker::{
    SubscriptionEnded, ConfigError, Cooldown, Failure, FetchPolicy, Fidelity, GridLocation, Observation, PowerDomain,
    PowerScope, Provider, ProviderError, ProviderId, ProviderResponse, Sample, SampleKind,
    SourceDescription, SourceId, SourceSelection, Sources, Subscription, TrackerConfig,
    TrackerEvent, TrackerHandle, TrackerQuery, Work, start, start_subscribed,
};

/// Long enough that no read times out on a busy host; tests about hung
/// reads set their own.
const TIMEOUT: Duration = Duration::from_secs(5);
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
    /// Fail every read with `Unavailable`.
    Fail,
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
            Behaviour::Fail => return Err(ProviderError::Unavailable("the driver is busy".to_owned())),
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

/// A config whose reads time out after `timeout`.
fn timing_out(timeout: Duration) -> TrackerConfig {
    TrackerConfig { fetch: FetchPolicy { timeout, ..config().fetch }, ..config() }
}

fn sources_of(power_provider: Test, grid_provider: Test) -> Sources {
    let mut sources = Sources::new();
    sources
        .add_provider(POWER, power_provider, [(source(POWER), power())])
        .unwrap();
    sources
        .add_provider(GRID, grid_provider, [(source(GRID), grid())])
        .unwrap();
    sources
}

/// A tracker over `power` and `grid`.
fn tracker(power_provider: Test, grid_provider: Test) -> TrackerHandle {
    tracker_with(power_provider, grid_provider, config())
}

fn tracker_with(power_provider: Test, grid_provider: Test, config: TrackerConfig) -> TrackerHandle {
    start(sources_of(power_provider, grid_provider), config).expect("a valid config")
}

/// [tracker_with], subscribed before the engine reads anything.
fn subscribed(power_provider: Test, grid_provider: Test, config: TrackerConfig) -> (TrackerHandle, Subscription) {
    start_subscribed(sources_of(power_provider, grid_provider), config).expect("a valid config")
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
    // Waiting on the hung read would take the whole timeout; the bounds are a third of it.
    let timeout = Duration::from_secs(3);
    let (hanging, _release) = Test::new(POWER, Behaviour::Answer).blocking();
    let (tracker, mut events) = subscribed(hanging, Test::new(GRID, Behaviour::Answer), timing_out(timeout));
    let mut cuts = tracker.subscribe();

    let began = Instant::now();
    let span = tracker.start_span("work", None, SourceSelection::All);
    let (reply, answer) = oneshot::channel();
    tracker.query(TrackerQuery::Catalog { reply });
    answer.await.unwrap().expect("a query is answered while the read hangs");
    assert!(began.elapsed() < timeout / 3, "the query did not wait on the read");
    tracker.mark(span, Work::default());
    tokio::time::timeout(WAIT, async {
        while !matches!(cuts.recv().await.expect("the stream is open"), TrackerEvent::Marked { .. }) {}
    })
    .await
    .expect("the mark is announced");
    assert!(began.elapsed() < timeout / 3, "events were delivered while the read hangs");

    tracker.stop_span(span, Work::default());
    tracker.finish().await.expect("the session finishes despite the hung read");
    assert!(began.elapsed() < 2 * timeout + Duration::from_secs(1), "nothing waited past AC/ENG-27's bound");

    let observed = observed(&mut events).await;
    let power = &observed[&source(POWER)];
    assert_eq!(power[0], Observation::Failed(Failure::TimedOut), "the opening read timed out");
    // Every request after the hung one fails with it. Nothing was read while
    // the read hung.
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
    let (tracker, mut events) = subscribed(Test::new(POWER, Behaviour::Panic), Test::new(GRID, Behaviour::Answer), config());

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
    // The grid skips, so a request that finds its read still out on a slow
    // host is `Skipped`. Nothing of the panic reaches it.
    let grid = &observed[&source(GRID)];
    assert!(grid.iter().any(|o| matches!(o, Observation::Samples(_))), "the grid was read: {grid:?}");
    assert!(
        !grid.iter().any(|o| matches!(o, Observation::Failed(Failure::Panicked(_) | Failure::Retired))),
        "the other provider is unaffected: {grid:?}"
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
    let tracker = tracker_with(hanging, Test::new(GRID, Behaviour::Answer), timing_out(Duration::from_millis(100)));
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

/// AC/MEAS-96
#[tokio::test]
async fn under_queue_a_failed_read_answers_its_request() {
    let failing = Test::new(POWER, Behaviour::Fail);
    let reads = Arc::clone(&failing.reads);
    let mut sources = Sources::new();
    sources.add_provider(POWER, failing, [(source(POWER), power())]).unwrap();
    sources.add_provider(GRID, Test::new(GRID, Behaviour::Answer), [(source(GRID), grid())]).unwrap();
    // A timeout no busy host reaches, so every read answers for itself.
    let config = TrackerConfig { fetch: FetchPolicy { timeout: Duration::from_secs(5), ..config().fetch }, ..config() };
    let (tracker, mut events) = start_subscribed(sources, config).expect("a valid config");

    let span = tracker.start_span("work", None, SourceSelection::All);
    tracker.stop_span(span, Work::default());
    let began = Instant::now();
    tracker.finish().await.expect("the session finishes");
    // A retried request would hold finish for its retry window.
    assert!(began.elapsed() < Duration::from_secs(1), "nothing waited on a failed read: {:?}", began.elapsed());

    // The opening cut, the start, the stop and the finish each send at most one.
    assert!(reads.load(Ordering::SeqCst) <= 4, "no request was sent again: {} reads", reads.load(Ordering::SeqCst));
    let power = &observed(&mut events).await[&source(POWER)];
    assert!(power.iter().all(|o| matches!(o, Observation::Failed(Failure::Provider(_)))), "{power:?}");
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
    let first = tokio::time::timeout(WAIT, async {
        loop {
            if let TrackerEvent::ReadoutLanded { outcomes, .. } = events.recv().await.expect("the stream is open") {
                break outcomes.keys().next().expect("an observation").provider();
            }
        }
    })
    .await
    .expect("the grid's readout lands");
    drop(release_power);
    tracker.finish().await.expect("the session finishes");

    let then: Vec<ProviderId> = landed(&mut events).await.into_iter().map(|(provider, ..)| provider).collect();
    assert_eq!((first, then[0]), (GRID, POWER), "the grid returned first, then power: {then:?}");
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
    // Subscribed before the engine reads anything: a later `subscribe` can
    // miss them (AC/ENG-65).
    for run in 0..200 {
        let mut sources = Sources::new();
        let power_provider = Test::new(POWER, Behaviour::Answer).cooling(Cooldown::Never);
        let grid_provider = Test::new(GRID, Behaviour::Answer).cooling(Cooldown::Never);
        sources.add_provider(POWER, power_provider, [(source(POWER), power())]).unwrap();
        sources.add_provider(GRID, grid_provider, [(source(GRID), grid())]).unwrap();
        let (tracker, mut events) = start_subscribed(sources, config()).expect("a valid config");
        tracker.finish().await.expect("the session finishes");
        let providers: Vec<ProviderId> = landed(&mut events).await.into_iter().map(|(provider, ..)| provider).collect();
        assert!(providers.contains(&POWER) && providers.contains(&GRID), "run {run} landed {providers:?}");
    }
}
