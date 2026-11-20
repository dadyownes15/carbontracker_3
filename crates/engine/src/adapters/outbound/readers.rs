//! [Readers]: every provider, each behind its [Gate], read on a worker
//! thread of its own. A request goes to one provider and lands as one readout once
//! that provider has answered it.

use std::collections::HashMap;
use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::domain::asks::{Asked, Asks, GateState};
use crate::domain::gate::{Answer, Asker, Effect, Gate, Returned};
use crate::domain::reading::{Cooldown, Failure, UnreadyActions};
use crate::domain::session::RequestId;
use crate::domain::{Observation, ProviderError, ProviderId, ReadoutId, ReadoutResponse, SourceId, Timestamp};
use crate::ports::outbound::{Provider, ProviderResponse, Reading};

/// What a worker sends back: what the read came back with, or the panic
/// that ended the worker, and when the read returned.
pub(crate) struct ReadReturn {
    provider: ProviderId,
    result: Result<Result<ProviderResponse, ProviderError>, String>,
    returned_at: Instant,
}

impl ReadReturn {
    pub(crate) fn new(
        provider: ProviderId,
        result: Result<Result<ProviderResponse, ProviderError>, String>,
        returned_at: Instant,
    ) -> Self {
        Self { provider, result, returned_at }
    }
}

/// Where workers send what they read.
pub(crate) type Returns = mpsc::UnboundedReceiver<ReadReturn>;

/// What one provider's gate asks of the readers.
type Effects = Vec<Effect<RequestId, Vec<(SourceId, Observation)>>>;

/// One provider, as the engine holds it.
pub(crate) struct Reader {
    /// The provider until its worker starts.
    provider: Option<Box<dyn Provider>>,
    /// Its worker: one job per read. `None` if none could be started.
    worker: Option<std::sync::mpsc::Sender<()>>,
    /// Every source it serves, sorted.
    serves: Vec<SourceId>,
    gate: Gate<RequestId, Vec<(SourceId, Observation)>>,
    asks: Asks,
    /// Who sent each request not yet answered.
    askers: HashMap<RequestId, Asker>,
    /// When the watchdog next asks it.
    watchdog_at: Option<Instant>,
    /// The watchdog came due while the provider was unready: it asks as
    /// soon as the provider is ready.
    watchdog_due: bool,
}

impl Reader {
    /// `provider`, serving `serves`, read under `policy`, with its cooldown
    /// asked now.
    pub(crate) fn new(
        provider: Box<dyn Provider>,
        mut serves: Vec<SourceId>,
        policy: UnreadyActions,
        timeout: Duration,
    ) -> Self {
        serves.sort();
        let cooldown: Cooldown = provider.cooldown();
        Self {
            provider: Some(provider),
            worker: None,
            serves,
            gate: Gate::new(cooldown, policy, timeout),
            asks: if cooldown == Cooldown::Never { Asks::never() } else { Asks::default() },
            askers: HashMap::new(),
            watchdog_at: None,
            watchdog_due: false,
        }
    }

    /// How long the watchdog leaves it unasked: `readout_interval`, and never
    /// inside its cooldown.
    fn watchdog_period(&self, interval: Duration) -> Duration {
        match self.gate.cooldown() {
            Cooldown::After(cooldown) => interval.max(cooldown),
            Cooldown::Never => interval,
        }
    }
}

/// Every provider a tracker reads.
pub(crate) struct Readers {
    readers: HashMap<ProviderId, Reader>,
    /// How long the watchdog leaves a provider unasked, at least.
    interval: Duration,
    next_request: usize,
}

impl Readers {
    /// Readers over `readers`, each asked by the watchdog after `interval`
    /// without a request; read threads send back on the returned [Returns].
    pub(crate) fn new(readers: HashMap<ProviderId, Reader>, interval: Duration) -> (Self, Returns) {
        let (sender, returns) = mpsc::unbounded_channel();
        let mut readers = readers;
        for (&id, reader) in &mut readers {
            let provider = reader.provider.take().expect("a new reader holds its provider");
            reader.worker = spawn_worker(id, provider, sender.clone());
        }
        (Self { readers, interval, next_request: 1 }, returns)
    }

    /// Readers whose jobs go to the returned channels rather than to worker
    /// threads: nothing is read, and the caller hands back each result.
    #[cfg(test)]
    pub(crate) fn detached(
        readers: HashMap<ProviderId, Reader>,
        interval: Duration,
    ) -> (Self, HashMap<ProviderId, std::sync::mpsc::Receiver<()>>) {
        let mut readers = readers;
        let mut jobs = HashMap::new();
        for (&id, reader) in &mut readers {
            reader.provider = None;
            let (sender, receiver) = std::sync::mpsc::channel();
            reader.worker = Some(sender);
            jobs.insert(id, receiver);
        }
        (Self { readers, interval, next_request: 1 }, jobs)
    }

    /// Every provider, in id order.
    pub(crate) fn providers(&self) -> Vec<ProviderId> {
        let mut providers: Vec<ProviderId> = self.readers.keys().copied().collect();
        providers.sort();
        providers
    }

    /// A cut stamped `stamp` asks `provider` at `now` (`wall` on the stamps'
    /// clock): the request it relies on, if any, and the readouts that land.
    pub(crate) fn ask(
        &mut self,
        provider: ProviderId,
        stamp: Timestamp,
        now: Instant,
        wall: Timestamp,
    ) -> (Option<RequestId>, Vec<ReadoutResponse>) {
        let mut landed = Vec::new();
        let Some(reader) = self.readers.get_mut(&provider) else {
            return (None, landed);
        };
        let state = reader.gate.state_at(now);
        if state == GateState::Ready {
            reader.asks.transition();
        }
        match reader.asks.cut(stamp, state, reader.gate.queued()) {
            Asked::Relies(request) => (Some(request), landed),
            Asked::Nothing => (None, landed),
            Asked::Send => {
                let request = self.send(provider, Asker::Cut, now, wall, &mut landed);
                (Some(request), landed)
            }
        }
    }

    /// A read came back, handled at `now`. Returns every readout that has landed.
    pub(crate) fn returned(&mut self, read: ReadReturn, now: Instant, wall: Timestamp) -> Vec<ReadoutResponse> {
        let mut landed = Vec::new();
        let Some(reader) = self.readers.get_mut(&read.provider) else {
            return landed;
        };
        reader.asks.transition();
        let returned = match read.result {
            Ok(result) => {
                let (observations, complete) = observe(&reader.serves, result);
                Returned::Read { answer: observations, complete }
            }
            Err(message) => Returned::Panicked(message),
        };
        let effects = reader.gate.returned(returned, read.returned_at, now);
        self.apply(read.provider, effects, wall, &mut landed);
        self.watchdog(read.provider, now, wall, &mut landed);
        landed
    }

    /// Ask `provider` for the watchdog if it is due and ready. Due but
    /// unready, it asks once the cooldown passes or the read returns; it
    /// never asks a provider that is done or retired.
    fn watchdog(&mut self, provider: ProviderId, now: Instant, wall: Timestamp, landed: &mut Vec<ReadoutResponse>) {
        let reader = self.readers.get_mut(&provider).expect("a known provider");
        let due = reader.watchdog_due || reader.watchdog_at.is_some_and(|at| now >= at);
        if !due {
            return;
        }
        if reader.gate.is_final() {
            (reader.watchdog_at, reader.watchdog_due) = (None, false);
        } else if reader.gate.state_at(now) == GateState::Ready {
            // Its request never queues, so it never asks inside a cooldown.
            self.send(provider, Asker::Watchdog, now, wall, landed);
        } else {
            reader.watchdog_due = true;
            reader.watchdog_at = reader.gate.cooling_until().filter(|&at| at > now);
        }
    }

    /// Time has reached `now`: reads may have timed out, queued requests may
    /// start, and the watchdog asks every provider it is due to. Returns
    /// every readout that has landed.
    pub(crate) fn advance(&mut self, now: Instant, wall: Timestamp) -> Vec<ReadoutResponse> {
        let mut landed = Vec::new();
        for provider in self.providers() {
            let effects = self.readers.get_mut(&provider).expect("listed above").gate.advance(now);
            self.apply(provider, effects, wall, &mut landed);
            self.watchdog(provider, now, wall, &mut landed);
        }
        landed
    }

    /// Whether `request` was sent and has been answered.
    pub(crate) fn is_answered(&self, request: RequestId) -> bool {
        request.0 < self.next_request && self.readers.values().all(|reader| !reader.askers.contains_key(&request))
    }

    /// When [Readers::advance] next has something to do, if ever.
    pub(crate) fn wake_at(&self) -> Option<Instant> {
        self.readers
            .values()
            .flat_map(|reader| [reader.gate.wake_at(), reader.watchdog_at])
            .flatten()
            .min()
    }

    /// Stop every worker: each drops its provider on its own thread, once
    /// any read it is on returns.
    pub(crate) fn close(self, mut returns: Returns) {
        returns.close();
        drop(self.readers);
    }

    /// Send `provider` a fresh request from `asker`, and reset its watchdog.
    fn send(
        &mut self,
        provider: ProviderId,
        asker: Asker,
        now: Instant,
        wall: Timestamp,
        landed: &mut Vec<ReadoutResponse>,
    ) -> RequestId {
        let request = ReadoutId(self.next_request);
        self.next_request += 1;
        let interval = self.interval;
        let reader = self.readers.get_mut(&provider).expect("only a known provider is asked");
        reader.askers.insert(request, asker);
        reader.watchdog_at = Some(now + reader.watchdog_period(interval));
        reader.watchdog_due = false;
        let effects = reader.gate.request(request, asker, now);
        self.apply(provider, effects, wall, landed);
        request
    }

    /// Carry out `provider`'s gate `effects`, collecting readouts that land.
    fn apply(&mut self, provider: ProviderId, effects: Effects, wall: Timestamp, landed: &mut Vec<ReadoutResponse>) {
        for effect in effects {
            match effect {
                Effect::Start(request) => {
                    let asks = &mut self.readers.get_mut(&provider).expect("known").asks;
                    asks.transition();
                    asks.started(request, wall);
                    if let Some(effects) = self.start(provider) {
                        self.apply(provider, effects, wall, landed);
                    }
                }
                Effect::Answer(request, answer) => {
                    let reader = self.readers.get_mut(&provider).expect("known");
                    let Some(asker) = reader.askers.remove(&request) else {
                        debug_assert!(false, "{request:?} answered twice, or never sent");
                        continue;
                    };
                    if matches!(answer, Answer::Failed(Failure::TimedOut)) {
                        reader.asks.transition();
                    }
                    reader.asks.answered(request, asker, matches!(answer, Answer::Read(_)));
                    // A provider that is done or retired is never asked again.
                    if matches!(answer, Answer::Done | Answer::Failed(Failure::Panicked(_) | Failure::Retired)) {
                        reader.watchdog_at = None;
                    }
                    let observations: Vec<(SourceId, Observation)> = match answer {
                        Answer::Read(observations) => observations,
                        Answer::Skipped(skip) => {
                            reader.serves.iter().map(|&source| (source, Observation::Skipped(skip))).collect()
                        }
                        Answer::Failed(failure) => reader
                            .serves
                            .iter()
                            .map(|&source| (source, Observation::Failed(failure.clone())))
                            .collect(),
                        Answer::Done => Vec::new(),
                    };
                    landed.push(response_of(request, observations));
                }
            }
        }
    }

    /// Hand `provider`'s worker one read. If it has no worker, the read
    /// panicked as far as the gate is concerned: returns the gate's effects.
    fn start(&mut self, provider: ProviderId) -> Option<Effects> {
        let reader = self.readers.get_mut(&provider).expect("only a known provider starts");
        let sent = reader.worker.as_ref().is_some_and(|worker| worker.send(()).is_ok());
        (!sent).then(|| {
            let failed = Returned::Panicked("no thread could be started for the provider".to_owned());
            let now = Instant::now();
            reader.gate.returned(failed, now, now)
        })
    }
}

/// Start `provider`'s worker: it reads once per job, sends what it read to
/// `returns`, and drops the provider on its own thread when the jobs end or
/// a read panics. `None` if no thread could be started; the provider is
/// then leaked, never dropped on the tracker thread.
fn spawn_worker(
    id: ProviderId,
    provider: Box<dyn Provider>,
    returns: mpsc::UnboundedSender<ReadReturn>,
) -> Option<std::sync::mpsc::Sender<()>> {
    let (jobs, inbox) = std::sync::mpsc::channel::<()>();
    // Shared, so a thread that never starts leaves the provider here.
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(provider)));
    let taken = std::sync::Arc::clone(&slot);
    let started = std::thread::Builder::new().name(format!("carbontracker-read-{}", id.0)).spawn(move || {
        let Some(mut provider) = taken.lock().ok().and_then(|mut slot| slot.take()) else { return };
        drop(taken);
        while inbox.recv().is_ok() {
            let result = panic::catch_unwind(AssertUnwindSafe(|| provider.read())).map_err(panic_message);
            // Stamped here, so the engine judges the read by when it returned.
            let returned_at = Instant::now();
            if result.is_err() {
                // A provider that panicked is never read again.
                let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(provider)));
                let _ = returns.send(ReadReturn::new(id, result, returned_at));
                return;
            }
            if returns.send(ReadReturn::new(id, result, returned_at)).is_err() {
                break;
            }
        }
        let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(provider)));
    });
    if started.is_err() {
        std::mem::forget(slot.lock().ok().and_then(|mut slot| slot.take()));
        return None;
    }
    Some(jobs)
}

/// A readout for `request` carrying `observations`.
fn response_of(request: RequestId, observations: Vec<(SourceId, Observation)>) -> ReadoutResponse {
    let mut response = ReadoutResponse::new(request);
    for (source, observation) in observations {
        response.observe(source, observation);
    }
    response
}

/// The observation for each source `serves` lists, from what a read
/// returned, and whether it answered every one of them with samples. A
/// source left out is [Failure::Unanswered]; one it does not serve is
/// dropped; a read that failed as a whole fails every source.
pub(crate) fn observe(
    serves: &[SourceId],
    result: Result<ProviderResponse, ProviderError>,
) -> (Vec<(SourceId, Observation)>, bool) {
    let response = match result {
        Ok(response) => response,
        Err(error) => {
            let failed = serves.iter().map(|&source| {
                (
                    source,
                    Observation::Failed(Failure::Provider(error.clone())),
                )
            });
            return (failed.collect(), false);
        }
    };
    let observations: Vec<(SourceId, Observation)> = serves
        .iter()
        .map(|&source| {
            let observation = match response.get(source) {
                Some(Reading::Samples(samples)) if !samples.is_empty() => {
                    Observation::Samples(samples.clone())
                }
                Some(Reading::Samples(_) | Reading::Unchanged) => Observation::Unchanged,
                Some(Reading::Failed(error)) => {
                    Observation::Failed(Failure::Provider(error.clone()))
                }
                None => Observation::Failed(Failure::Unanswered),
            };
            (source, observation)
        })
        .collect();
    let complete = observations
        .iter()
        .all(|(_, observation)| matches!(observation, Observation::Samples(_)));
    (observations, complete)
}

/// A caught panic's message.
fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| (*message).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the provider panicked".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::sample_store::Sample;

    fn source(slot: u16) -> SourceId {
        SourceId::new(ProviderId(1), slot)
    }

    fn sample() -> Sample {
        Sample::new(chrono::DateTime::UNIX_EPOCH, 1.0)
    }

    /// AC/MEAS-23, AC/MEAS-59, AC/ENG-40, AC/ENG-46
    #[test]
    fn a_source_left_out_is_unanswered_and_a_foreign_one_is_dropped() {
        let mut response = ProviderResponse::new();
        response.samples(source(0), vec![sample()]);
        response.samples(SourceId::new(ProviderId(9), 0), vec![sample()]);

        let (observations, complete) = observe(&[source(0), source(1)], Ok(response));
        assert_eq!(
            observations,
            vec![
                (source(0), Observation::Samples(vec![sample()])),
                (source(1), Observation::Failed(Failure::Unanswered))
            ]
        );
        assert!(!complete);
    }

    /// AC/MEAS-59, AC/ENG-46
    #[test]
    fn a_read_that_failed_as_a_whole_fails_every_source() {
        let error = ProviderError::Unavailable("driver unloaded".to_owned());
        let (observations, complete) = observe(&[source(0), source(1)], Err(error.clone()));
        assert!(observations.iter().all(|(_, observation)| *observation
            == Observation::Failed(Failure::Provider(error.clone()))));
        assert_eq!(observations.len(), 2);
        assert!(!complete);
    }

    /// AC/MEAS-17, AC/MEAS-18, AC/ENG-47
    #[test]
    fn a_read_with_samples_for_every_source_is_complete() {
        let mut response = ProviderResponse::new();
        response
            .samples(source(0), vec![sample()])
            .insert(source(1), Reading::Unchanged);
        assert!(!observe(&[source(0), source(1)], Ok(response.clone())).1);
        response.samples(source(1), vec![sample()]);
        assert!(observe(&[source(0), source(1)], Ok(response)).1);
    }

    /// A provider whose first read blocks until released; later reads answer
    /// at once. Counts its reads.
    struct Hanging {
        release: Option<std::sync::mpsc::Receiver<()>>,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        cooldown: Cooldown,
    }

    impl Provider for Hanging {
        fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Some(release) = self.release.take() {
                let _ = release.recv();
            }
            let mut response = ProviderResponse::new();
            response.samples(source(0), vec![sample()]);
            Ok(response)
        }

        fn cooldown(&self) -> Cooldown {
            self.cooldown
        }
    }

    const TIMEOUT: Duration = Duration::from_secs(2);

    /// One hanging provider under `Queue`, with `cooldown`, the watchdog
    /// every `interval`.
    fn hanging(
        cooldown: Duration,
        interval: Duration,
    ) -> (Readers, Returns, std::sync::mpsc::Sender<()>, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let (release, wait) = std::sync::mpsc::channel();
        let reads = std::sync::Arc::default();
        let provider =
            Hanging { release: Some(wait), reads: std::sync::Arc::clone(&reads), cooldown: Cooldown::After(cooldown) };
        let reader = Reader::new(Box::new(provider), vec![source(0)], UnreadyActions::QUEUE, TIMEOUT);
        let (readers, returns) = Readers::new(HashMap::from([(ProviderId(1), reader)]), interval);
        (readers, returns, release, reads)
    }

    fn stamp(ms: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::milliseconds(ms)
    }

    fn after(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// AC/MEAS-12
    #[test]
    fn a_cut_after_a_timeout_does_not_rely_on_the_stalled_answer() {
        let (mut readers, mut returns, release, _) = hanging(Duration::from_secs(4), Duration::from_secs(60));
        let t0 = Instant::now();
        let provider = ProviderId(1);
        readers.ask(provider, stamp(0), t0, stamp(0));
        // Queued behind the hung read, then stalled when it times out.
        let (queued, _) = readers.ask(provider, stamp(500), after(t0, 500), stamp(500));
        let landed = readers.advance(after(t0, 2100), stamp(2100));
        assert!(landed.iter().any(|response| Some(response.id) == queued), "the queued request stalled");

        // The hung read returns inside its cooldown: unready again, but a
        // transition has passed since the stalled answer.
        release.send(()).unwrap();
        let read = returns.blocking_recv().expect("the read returns");
        readers.returned(read, after(t0, 2500), stamp(2500));
        let (relied, _) = readers.ask(provider, stamp(2600), after(t0, 2600), stamp(2600));
        assert_ne!(relied, queued, "a fresh request, queued for the cooldown's end");
    }

    /// AC/MEAS-62, AC/ENG-45
    #[test]
    fn the_watchdog_asks_again_once_a_hung_read_returns() {
        let (mut readers, mut returns, release, reads) = hanging(Duration::ZERO, Duration::from_secs(1));
        let t0 = Instant::now();
        readers.ask(ProviderId(1), stamp(0), t0, stamp(0));
        // The watchdog is due while the read is out past its timeout.
        readers.advance(after(t0, 2100), stamp(2100));
        release.send(()).unwrap();
        let read = returns.blocking_recv().expect("the read returns");
        readers.returned(read, after(t0, 2500), stamp(2500));

        readers.advance(after(t0, 3600), stamp(3600));
        let deadline = Instant::now() + Duration::from_secs(5);
        while reads.load(std::sync::atomic::Ordering::SeqCst) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2, "the watchdog read it again");
    }

    /// AC/MEAS-62
    #[test]
    fn a_watchdog_due_while_busy_asks_as_soon_as_the_read_returns() {
        let (mut readers, mut returns, release, reads) = hanging(Duration::ZERO, Duration::from_secs(1));
        let t0 = Instant::now();
        readers.ask(ProviderId(1), stamp(0), t0, stamp(0));
        // Due at 1 s while the read is out, inside its timeout.
        readers.advance(after(t0, 1000), stamp(1000));
        release.send(()).unwrap();
        let read = returns.blocking_recv().expect("the read returns");
        readers.returned(read, after(t0, 1100), stamp(1100));
        readers.advance(after(t0, 1100), stamp(1100));

        let deadline = Instant::now() + Duration::from_secs(5);
        while reads.load(std::sync::atomic::Ordering::SeqCst) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(reads.load(std::sync::atomic::Ordering::SeqCst), 2, "asked at 1.1 s, not a period later");
    }

    /// AC/MEAS-22
    #[test]
    fn a_source_the_provider_failed_fails_alone() {
        let error = ProviderError::Denied("no root".to_owned());
        let mut response = ProviderResponse::new();
        response.samples(source(0), vec![sample()]).insert(source(1), Reading::Failed(error.clone()));

        let (observations, complete) = observe(&[source(0), source(1)], Ok(response));
        assert_eq!(
            observations,
            vec![
                (source(0), Observation::Samples(vec![sample()])),
                (source(1), Observation::Failed(Failure::Provider(error)))
            ]
        );
        assert!(!complete);
    }

    /// AC/MEAS-16
    #[test]
    fn a_provider_says_what_it_read_and_the_engine_what_became_of_it() {
        // A provider can only say samples, nothing new, or failed: never skipped or timed out.
        for reading in [Reading::Samples(vec![sample()]), Reading::Unchanged, Reading::Failed(ProviderError::Invalid("x".to_owned()))] {
            let said = match &reading {
                Reading::Samples(_) => "samples",
                Reading::Unchanged => "unchanged",
                Reading::Failed(_) => "failed",
            };
            let mut response = ProviderResponse::new();
            response.insert(source(0), reading);
            let (observations, _) = observe(&[source(0)], Ok(response));
            let became = match &observations[0].1 {
                Observation::Samples(_) => "samples",
                Observation::Unchanged => "unchanged",
                Observation::Failed(Failure::Provider(_)) => "failed",
                other => panic!("the engine claimed {other:?} for a provider that said {said}"),
            };
            assert_eq!(said, became);
        }
    }

    /// Answers `Unchanged` to its first `quiet` reads, then samples; or
    /// panics on every read. Counts its reads.
    struct Scripted {
        quiet: usize,
        panics: bool,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        cooldown: Cooldown,
    }

    impl Provider for Scripted {
        fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
            let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert!(!self.panics, "the driver exploded");
            let mut response = ProviderResponse::new();
            if n < self.quiet {
                response.insert(source(0), Reading::Unchanged);
            } else {
                response.samples(source(0), vec![sample()]);
            }
            Ok(response)
        }

        fn cooldown(&self) -> Cooldown {
            self.cooldown
        }
    }

    /// One [Scripted] provider under `Queue`, the watchdog every second.
    fn scripted(provider: Scripted) -> (Readers, Returns) {
        let reader = Reader::new(Box::new(provider), vec![source(0)], UnreadyActions::QUEUE, TIMEOUT);
        Readers::new(HashMap::from([(ProviderId(1), reader)]), Duration::from_secs(1))
    }

    /// Wait for the read in flight to return, and hand it back at `at`.
    fn read_returns(readers: &mut Readers, returns: &mut Returns, t0: Instant, at: u64) {
        let read = returns.blocking_recv().expect("the read returns");
        readers.returned(read, after(t0, at), stamp(at as i64));
    }

    fn reads_of(reads: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
        reads.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// AC/MEAS-62
    #[test]
    fn the_watchdog_never_asks_a_retired_provider() {
        let reads = std::sync::Arc::default();
        let provider = Scripted { quiet: 0, panics: true, reads: std::sync::Arc::clone(&reads), cooldown: Cooldown::After(Duration::ZERO) };
        let (mut readers, mut returns) = scripted(provider);
        let t0 = Instant::now();
        readers.ask(ProviderId(1), stamp(0), t0, stamp(0));
        read_returns(&mut readers, &mut returns, t0, 10);

        assert_eq!(readers.wake_at(), None, "nothing left to ask");
        for at in [1000, 5000, 60_000] {
            assert!(readers.advance(after(t0, at), stamp(at as i64)).is_empty());
        }
        assert_eq!(reads_of(&reads), 1);
    }

    /// AC/MEAS-63
    #[test]
    fn a_missed_watchdog_tick_is_skipped_not_caught_up() {
        let reads = std::sync::Arc::default();
        let provider = Scripted { quiet: 0, panics: false, reads: std::sync::Arc::clone(&reads), cooldown: Cooldown::After(Duration::ZERO) };
        let (mut readers, mut returns) = scripted(provider);
        let t0 = Instant::now();
        readers.ask(ProviderId(1), stamp(0), t0, stamp(0));
        read_returns(&mut readers, &mut returns, t0, 10);

        // Ticks were due at 1 s to 5 s; time only reaches 5.5 s.
        readers.advance(after(t0, 5500), stamp(5500));
        read_returns(&mut readers, &mut returns, t0, 5510);
        assert_eq!(reads_of(&reads), 2, "one watchdog read for five missed ticks");
        assert_eq!(readers.wake_at(), Some(after(t0, 6500)), "the next a period after it");
    }

    /// AC/MEAS-65
    #[test]
    fn the_watchdog_asks_a_never_provider_until_it_has_answered() {
        let reads = std::sync::Arc::default();
        let provider = Scripted { quiet: 2, panics: false, reads: std::sync::Arc::clone(&reads), cooldown: Cooldown::Never };
        let (mut readers, mut returns) = scripted(provider);
        let t0 = Instant::now();
        readers.ask(ProviderId(1), stamp(0), t0, stamp(0));
        read_returns(&mut readers, &mut returns, t0, 10);
        for at in [1000, 2000] {
            readers.advance(after(t0, at), stamp(at as i64));
            read_returns(&mut readers, &mut returns, t0, at + 10);
        }
        assert_eq!(reads_of(&reads), 3, "two quiet reads, then samples");

        for at in [3000, 4000, 60_000] {
            readers.advance(after(t0, at), stamp(at as i64));
        }
        assert_eq!(readers.wake_at(), None);
        assert_eq!(reads_of(&reads), 3, "never asked again");
    }

    /// Fails every read with `Unavailable`.
    struct Failing;

    impl Provider for Failing {
        fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
            Err(ProviderError::Unavailable("the driver is busy".to_owned()))
        }

        fn cooldown(&self) -> Cooldown {
            Cooldown::After(Duration::ZERO)
        }
    }

    /// One reader under `policy` over a provider whose every read fails, the
    /// watchdog every minute.
    fn failing_reader(policy: UnreadyActions) -> (Readers, Returns) {
        let reader = Reader::new(Box::new(Failing), vec![source(0)], policy, TIMEOUT);
        Readers::new(HashMap::from([(ProviderId(1), reader)]), Duration::from_secs(60))
    }

    /// AC/MEAS-96
    #[test]
    fn under_either_policy_a_failed_read_answers_its_request() {
        for policy in [UnreadyActions::QUEUE, UnreadyActions::SKIP] {
            let (mut readers, mut returns) = failing_reader(policy);
            let t0 = Instant::now();
            let request = readers.ask(ProviderId(1), stamp(0), t0, stamp(0)).0.expect("a request");
            let read = returns.blocking_recv().expect("the read returns");
            let landed = readers.returned(read, after(t0, 1), stamp(1));
            assert_eq!(landed.iter().map(|response| response.id).collect::<Vec<_>>(), vec![request]);
            assert!(landed[0].samples().next().is_none(), "it lands with the failure");
            assert!(readers.advance(after(t0, 100), stamp(100)).is_empty(), "nothing is sent again");
        }
    }
}
