//! [Readers]: every provider, each behind its [Gate], read on threads of
//! their own. A request goes to one provider and lands as one readout once
//! that provider has answered it.

use std::collections::{HashMap, VecDeque};
use std::panic::{self, AssertUnwindSafe};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::domain::asks::{Asked, Asks, GateState};
use crate::domain::gate::{Answer, Asker, Effect, Gate, Returned};
use crate::domain::reading::{ActionWhenUnready, Cooldown, Failure, UnreadyActions};
use crate::domain::session::RequestId;
use crate::domain::{Observation, ProviderError, ProviderId, ReadoutId, ReadoutResponse, SourceId, Timestamp};
use crate::ports::outbound::{Provider, ProviderResponse, Reading};

/// What a read thread sends back: the provider, unless it panicked, and what
/// the read came back with.
pub(crate) struct ReadReturn {
    provider: ProviderId,
    back: Option<Box<dyn Provider>>,
    result: Result<Result<ProviderResponse, ProviderError>, String>,
}

/// Where read threads send what they read.
pub(crate) type Returns = mpsc::UnboundedReceiver<ReadReturn>;

/// What one provider's gate asks of the readers.
type Effects = Vec<Effect<RequestId, Vec<(SourceId, Observation)>>>;

/// One provider, as the engine holds it.
pub(crate) struct Reader {
    /// `None` while lent to a read, and once retired.
    provider: Option<Box<dyn Provider>>,
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
    /// Whether its policy queues: only then is a failed cut request sent again.
    queues: bool,
    /// How long a cut's request stays open after a failed read; `None` never.
    retry_for: Option<Duration>,
    /// When each open cut request was first sent.
    sent_at: HashMap<RequestId, Instant>,
    /// Cut requests a read failed, waiting to be sent again, oldest first,
    /// each with what its last read gave.
    open: VecDeque<(RequestId, Vec<(SourceId, Observation)>)>,
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
            serves,
            gate: Gate::new(cooldown, policy, timeout),
            asks: if cooldown == Cooldown::Never { Asks::never() } else { Asks::default() },
            askers: HashMap::new(),
            watchdog_at: None,
            watchdog_due: false,
            queues: policy.on_cooldown == ActionWhenUnready::Queue || policy.on_busy == ActionWhenUnready::Queue,
            retry_for: None,
            sent_at: HashMap::new(),
            open: VecDeque::new(),
        }
    }

    /// Send a cut's request again after a failed read, for up to `retry_for`
    /// after it was first sent; zero never does.
    pub(crate) fn retrying(mut self, retry_for: Duration) -> Self {
        self.retry_for = (!retry_for.is_zero()).then_some(retry_for);
        self
    }

    /// Whether a failed read should leave `request`, from `asker`, open.
    fn keeps_open(&self, request: RequestId, asker: Asker, answer: &Answer<Vec<(SourceId, Observation)>>, now: Instant) -> bool {
        let failed = match answer {
            // Only samples answer it: a source unchanged or failed leaves it open.
            Answer::Read(observations) => observations.iter().any(|(_, observation)| !matches!(observation, Observation::Samples(_))),
            Answer::Done => false,
            // A panic ends the retry.
            Answer::Failed(Failure::Panicked(_) | Failure::Retired) => false,
            Answer::Failed(_) => true,
            // Skipped behind the queue: the queued read answers what it would (08).
            Answer::Skipped(_) => false,
        };
        let within = |retry_for: Duration| self.sent_at.get(&request).is_some_and(|&sent| now < sent + retry_for);
        failed && asker == Asker::Cut && self.queues && self.retry_for.is_some_and(within)
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
    sender: mpsc::UnboundedSender<ReadReturn>,
}

impl Readers {
    /// Readers over `readers`, each asked by the watchdog after `interval`
    /// without a request; read threads send back on the returned [Returns].
    pub(crate) fn new(readers: HashMap<ProviderId, Reader>, interval: Duration) -> (Self, Returns) {
        let (sender, returns) = mpsc::unbounded_channel();
        (Self { readers, interval, next_request: 1, sender }, returns)
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

    /// A read came back at `now`. Returns every readout that has landed.
    pub(crate) fn returned(&mut self, read: ReadReturn, now: Instant, wall: Timestamp) -> Vec<ReadoutResponse> {
        let mut landed = Vec::new();
        let Some(reader) = self.readers.get_mut(&read.provider) else {
            return landed;
        };
        reader.provider = read.back;
        reader.asks.transition();
        let returned = match read.result {
            Ok(result) => {
                let (observations, complete) = observe(&reader.serves, result);
                Returned::Read { answer: observations, complete }
            }
            Err(message) => Returned::Panicked(message),
        };
        let effects = reader.gate.returned(returned, now);
        self.apply(read.provider, effects, now, wall, &mut landed);
        self.resend(read.provider, now, wall, &mut landed);
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
            self.apply(provider, effects, now, wall, &mut landed);
            self.resend(provider, now, wall, &mut landed);
            self.watchdog(provider, now, wall, &mut landed);
        }
        landed
    }

    /// Send `provider`'s oldest open request again if the provider can take
    /// it, or let it land with its last read once `retry_for` has passed.
    /// Not while a read is out past the timeout: it waits for that read.
    fn resend(&mut self, provider: ProviderId, now: Instant, wall: Timestamp, landed: &mut Vec<ReadoutResponse>) {
        loop {
            let reader = self.readers.get_mut(&provider).expect("a known provider");
            let Some((request, _)) = reader.open.front() else { return };
            let request = *request;
            let expired = match (reader.sent_at.get(&request), reader.retry_for) {
                (Some(&sent), Some(retry_for)) => now >= sent + retry_for,
                _ => true,
            };
            if expired {
                let (_, last) = reader.open.pop_front().expect("checked");
                reader.askers.remove(&request);
                reader.sent_at.remove(&request);
                landed.push(response_of(request, last));
                continue;
            }
            if reader.gate.state_at(now) == GateState::Failing && !reader.gate.is_final() {
                return;
            }
            if reader.gate.queued().is_some() {
                return;
            }
            reader.open.pop_front();
            let effects = reader.gate.request(request, Asker::Cut, now);
            self.apply(provider, effects, now, wall, landed);
            return;
        }
    }

    /// Whether `request` was sent and has been answered.
    pub(crate) fn is_answered(&self, request: RequestId) -> bool {
        request.0 < self.next_request && self.readers.values().all(|reader| !reader.askers.contains_key(&request))
    }

    /// When [Readers::advance] next has something to do, if ever.
    pub(crate) fn wake_at(&self) -> Option<Instant> {
        self.readers
            .values()
            .flat_map(|reader| {
                let give_up = reader
                    .open
                    .front()
                    .and_then(|(request, _)| Some(*reader.sent_at.get(request)? + reader.retry_for?));
                [reader.gate.wake_at(), reader.watchdog_at, give_up]
            })
            .flatten()
            .min()
    }

    /// Drop every provider the engine holds, on a thread of its own: those
    /// at home, and those whose reads came back unheard on `returns`. A
    /// provider still out on a read is dropped by that read's thread once it
    /// returns, since nobody is listening any more.
    pub(crate) fn close(self, mut returns: Returns) {
        returns.close();
        let mut providers: Vec<Box<dyn Provider>> =
            self.readers.into_values().filter_map(|reader| reader.provider).collect();
        while let Ok(read) = returns.try_recv() {
            providers.extend(read.back);
        }
        if !providers.is_empty() {
            let _ = std::thread::Builder::new().name("carbontracker-drop".to_owned()).spawn(move || {
                let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(providers)));
            });
        }
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
        if asker == Asker::Cut {
            reader.sent_at.insert(request, now);
        }
        reader.watchdog_at = Some(now + reader.watchdog_period(interval));
        reader.watchdog_due = false;
        let effects = reader.gate.request(request, asker, now);
        self.apply(provider, effects, now, wall, landed);
        request
    }

    /// Carry out `provider`'s gate `effects`, collecting readouts that land.
    fn apply(&mut self, provider: ProviderId, effects: Effects, now: Instant, wall: Timestamp, landed: &mut Vec<ReadoutResponse>) {
        for effect in effects {
            match effect {
                Effect::Start(request) => {
                    let asks = &mut self.readers.get_mut(&provider).expect("known").asks;
                    asks.transition();
                    asks.started(request, wall);
                    if let Some(effects) = self.start(provider) {
                        self.apply(provider, effects, now, wall, landed);
                    }
                }
                Effect::Answer(request, answer) => {
                    let fresh = ReadoutId(self.next_request);
                    let reader = self.readers.get_mut(&provider).expect("known");
                    let Some(asker) = reader.askers.remove(&request) else {
                        debug_assert!(false, "{request:?} answered twice, or never sent");
                        continue;
                    };
                    let keep_open = reader.keeps_open(request, asker, &answer, now);
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
                    if keep_open {
                        // The failure lands under an id of its own; the request
                        // stays open and is sent again once the provider is ready.
                        reader.askers.insert(request, asker);
                        reader.open.push_back((request, observations.clone()));
                        self.next_request += 1;
                        landed.push(response_of(fresh, observations));
                    } else {
                        reader.sent_at.remove(&request);
                        landed.push(response_of(request, observations));
                    }
                }
            }
        }
    }

    /// Lend `provider` to a thread of its own for one read. If no thread can
    /// be started, the read panicked as far as the gate is concerned: returns
    /// the gate's effects.
    fn start(&mut self, provider: ProviderId) -> Option<Effects> {
        let reader = self.readers.get_mut(&provider).expect("only a known provider starts");
        let Some(lent) = reader.provider.take() else {
            unreachable!("the gate starts a read only while the provider is home");
        };
        // Shared, so a thread that never starts leaves the provider here.
        let lent = std::sync::Arc::new(std::sync::Mutex::new(Some(lent)));
        let taken = std::sync::Arc::clone(&lent);
        let sender = self.sender.clone();
        let started = std::thread::Builder::new().name(format!("carbontracker-read-{}", provider.0)).spawn(move || {
            let Some(mut lent) = taken.lock().ok().and_then(|mut slot| slot.take()) else { return };
            drop(taken);
            let result = panic::catch_unwind(AssertUnwindSafe(|| lent.read())).map_err(panic_message);
            let back = if result.is_ok() {
                Some(lent)
            } else {
                // A provider that panicked is never read again: drop it here.
                let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(lent)));
                None
            };
            if let Err(unheard) = sender.send(ReadReturn { provider, back, result }) {
                // The session has ended: drop the provider here.
                let _ = panic::catch_unwind(AssertUnwindSafe(move || drop(unheard.0)));
            }
        });
        started.is_err().then(|| {
            // Kept at home, so [Readers::close] drops it off this thread.
            reader.provider = lent.lock().ok().and_then(|mut slot| slot.take());
            let failed = Returned::Panicked("no thread could be started for the read".to_owned());
            reader.gate.returned(failed, Instant::now())
        })
    }
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

    /// Fails its first `fails` reads with `Unavailable`, then reads 1 W.
    struct Flaky {
        fails: usize,
        reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Provider for Flaky {
        fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
            let n = self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n < self.fails {
                return Err(ProviderError::Unavailable("the driver is busy".to_owned()));
            }
            let mut response = ProviderResponse::new();
            response.samples(source(0), vec![sample()]);
            Ok(response)
        }

        fn cooldown(&self) -> Cooldown {
            Cooldown::After(Duration::ZERO)
        }
    }

    fn flaky(fails: usize, policy: UnreadyActions, retry_for: Duration) -> (Readers, Returns) {
        let provider = Flaky { fails, reads: std::sync::Arc::default() };
        let reader = Reader::new(Box::new(provider), vec![source(0)], policy, TIMEOUT).retrying(retry_for);
        Readers::new(HashMap::from([(ProviderId(1), reader)]), Duration::from_secs(60))
    }

    /// Hand back reads as they return, at `at` ms, until `request` lands.
    fn until_landed(readers: &mut Readers, returns: &mut Returns, t0: Instant, request: RequestId) -> (ReadoutResponse, Vec<ReadoutResponse>) {
        let mut others = Vec::new();
        for at in 1..200 {
            let read = returns.blocking_recv().expect("a read returns");
            for response in readers.returned(read, after(t0, at), stamp(at as i64)) {
                if response.id == request {
                    return (response, others);
                }
                others.push(response);
            }
        }
        panic!("request {request:?} never landed");
    }

    fn samples_of(response: &ReadoutResponse) -> bool {
        response.samples().count() == 1
    }

    /// AC/MEAS-92
    #[test]
    fn under_queue_a_failed_read_does_not_answer_the_request_and_it_is_sent_again() {
        let (mut readers, mut returns) = flaky(2, UnreadyActions::QUEUE, Duration::from_secs(30));
        let t0 = Instant::now();
        let (request, landed) = readers.ask(ProviderId(1), stamp(0), t0, stamp(0));
        let request = request.expect("a request");
        assert!(landed.is_empty());
        let (answer, failures) = until_landed(&mut readers, &mut returns, t0, request);
        assert!(samples_of(&answer), "answered by the read with samples: {answer:?}");
        assert_eq!(failures.len(), 2, "each failure lands under an id of its own");
        assert!(failures.iter().all(|failure| failure.id != request && !samples_of(failure)));
    }

    /// AC/MEAS-94
    #[test]
    fn an_open_request_gives_up_after_retry_for() {
        let (mut readers, mut returns) = flaky(usize::MAX, UnreadyActions::QUEUE, Duration::from_millis(5));
        let t0 = Instant::now();
        let request = readers.ask(ProviderId(1), stamp(0), t0, stamp(0)).0.expect("a request");
        let (answer, _) = until_landed(&mut readers, &mut returns, t0, request);
        assert!(!samples_of(&answer), "it lands with the last failure");
    }

    /// AC/MEAS-96
    #[test]
    fn under_skip_a_failed_read_answers_its_request() {
        let (mut readers, mut returns) = flaky(1, UnreadyActions::SKIP, Duration::from_secs(30));
        let t0 = Instant::now();
        let request = readers.ask(ProviderId(1), stamp(0), t0, stamp(0)).0.expect("a request");
        let read = returns.blocking_recv().expect("the read returns");
        let landed = readers.returned(read, after(t0, 1), stamp(1));
        assert_eq!(landed.iter().map(|response| response.id).collect::<Vec<_>>(), vec![request]);
    }

    /// AC/MEAS-95
    #[test]
    fn a_panic_ends_the_retry() {
        let provider = Scripted { quiet: 0, panics: true, reads: std::sync::Arc::default(), cooldown: Cooldown::After(Duration::ZERO) };
        let reader = Reader::new(Box::new(provider), vec![source(0)], UnreadyActions::QUEUE, TIMEOUT).retrying(Duration::from_secs(30));
        let (mut readers, mut returns) = Readers::new(HashMap::from([(ProviderId(1), reader)]), Duration::from_secs(60));
        let t0 = Instant::now();
        let request = readers.ask(ProviderId(1), stamp(0), t0, stamp(0)).0.expect("a request");
        let read = returns.blocking_recv().expect("the read returns");
        let landed = readers.returned(read, after(t0, 1), stamp(1));
        let answer = landed.into_iter().find(|response| response.id == request).expect("answered at once");
        assert!(answer.into_outcomes().all(|(_, o)| matches!(o, Observation::Failed(Failure::Panicked(_)))));
    }

    /// AC/MEAS-93
    #[test]
    fn an_open_request_waits_for_a_hung_read_to_return_then_is_sent_again() {
        let (release, wait) = std::sync::mpsc::channel();
        let reads = std::sync::Arc::default();
        let provider = Hanging { release: Some(wait), reads: std::sync::Arc::clone(&reads), cooldown: Cooldown::After(Duration::ZERO) };
        let reader = Reader::new(Box::new(provider), vec![source(0)], UnreadyActions::QUEUE, TIMEOUT).retrying(Duration::from_secs(30));
        let (mut readers, mut returns) = Readers::new(HashMap::from([(ProviderId(1), reader)]), Duration::from_secs(60));
        let t0 = Instant::now();
        let request = readers.ask(ProviderId(1), stamp(0), t0, stamp(0)).0.expect("a request");
        while reads_of(&reads) == 0 {
            std::thread::yield_now();
        }
        // Past the timeout: the request is not answered, and not sent again.
        for at in [2100u64, 2200, 3000] {
            let landed = readers.advance(after(t0, at), stamp(at as i64));
            assert!(landed.iter().all(|response| response.id != request), "{landed:?}");
        }
        assert_eq!(reads_of(&reads), 1, "no read while the hung one is out");
        release.send(()).unwrap();
        let (answer, _) = until_landed(&mut readers, &mut returns, t0, request);
        assert!(samples_of(&answer));
        assert_eq!(reads_of(&reads), 2);
    }

    /// AC/MEAS-92
    #[test]
    fn under_queue_a_read_with_nothing_new_leaves_the_request_open_too() {
        let reads = std::sync::Arc::default();
        let provider = Scripted { quiet: 2, panics: false, reads: std::sync::Arc::clone(&reads), cooldown: Cooldown::After(Duration::ZERO) };
        let reader = Reader::new(Box::new(provider), vec![source(0)], UnreadyActions::QUEUE, TIMEOUT).retrying(Duration::from_secs(30));
        let (mut readers, mut returns) = Readers::new(HashMap::from([(ProviderId(1), reader)]), Duration::from_secs(60));
        let t0 = Instant::now();
        let request = readers.ask(ProviderId(1), stamp(0), t0, stamp(0)).0.expect("a request");
        let (answer, quiet) = until_landed(&mut readers, &mut returns, t0, request);
        assert!(samples_of(&answer), "answered by the first read with samples");
        assert_eq!(quiet.len(), 2);
        assert_eq!(reads_of(&reads), 3);
    }
}
