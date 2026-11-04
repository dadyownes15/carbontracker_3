//! [Forecaster]: the grid's intensity forecast, fetched when asked, on a
//! thread of its own.

use std::fmt;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use carbontracker::{Fidelity, GridLocation, Sample};

/// How long a fetch may take before the askers are answered without a curve.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a fetched curve answers later asks without fetching again.
const REUSE_FOR: Duration = Duration::from_secs(60);

/// The grid's intensity forecast as it stood when fetched.
#[derive(Debug, Clone, PartialEq)]
pub struct ForecastReport {
    /// The grid forecast, always the one the run is billed against.
    pub location: GridLocation,
    pub fidelity: Fidelity,
    /// How far ahead the curve reaches, which can be shorter than asked for.
    pub horizon: Duration,
    /// gCO2e/kWh over time, held from each point to the next.
    pub curve: Vec<Sample>,
}

/// Fetches one curve. Blocks; runs on a thread of the forecaster's own.
pub(crate) type Fetch = Arc<dyn Fn() -> Option<Vec<Sample>> + Send + Sync>;

type Reply = Box<dyn FnOnce(Option<ForecastReport>) + Send>;

/// The forecast for the grid a run is billed against. Cheap to clone: clones
/// share one cache.
#[derive(Clone)]
pub struct Forecaster {
    inner: Arc<Inner>,
}

struct Inner {
    location: GridLocation,
    fidelity: Fidelity,
    horizon: Duration,
    served_by: &'static str,
    fetch: Fetch,
    timeout: Duration,
    reuse_for: Duration,
    state: Mutex<State>,
    /// What the last fetch to find a curve found, or the first fetch's
    /// `None`; `None` before any fetch has finished.
    last: Mutex<Option<Option<Vec<Sample>>>>,
}

enum State {
    Idle,
    /// A fetch is running; these askers wait on it.
    Fetching(Vec<Reply>),
    /// The last fetch's answer, and when it came.
    Fetched { at: Instant, curve: Option<Vec<Sample>> },
}

impl Forecaster {
    pub(crate) fn new(
        location: GridLocation,
        fidelity: Fidelity,
        horizon: Duration,
        served_by: &'static str,
        fetch: Fetch,
    ) -> Self {
        let inner = Inner {
            location,
            fidelity,
            horizon,
            served_by,
            fetch,
            timeout: FETCH_TIMEOUT,
            reuse_for: REUSE_FOR,
            state: Mutex::new(State::Idle),
            last: Mutex::new(None),
        };
        Self { inner: Arc::new(inner) }
    }

    /// Answer the next asks within a minute with `curve`, as if just fetched.
    pub(crate) fn primed(self, curve: Vec<Sample>) -> Self {
        *self.inner.last.lock().unwrap() = Some(Some(curve.clone()));
        *self.inner.state.lock().unwrap() = State::Fetched { at: Instant::now(), curve: Some(curve) };
        self
    }

    #[cfg(test)]
    pub(crate) fn with_timing(self, timeout: Duration, reuse_for: Duration) -> Self {
        let inner = Arc::into_inner(self.inner).expect("not yet cloned");
        Self { inner: Arc::new(Inner { timeout, reuse_for, ..inner }) }
    }

    /// The grid forecast, always the one the run is billed against.
    pub fn location(&self) -> &GridLocation {
        &self.inner.location
    }

    /// `Measured` for a published forecast, `Modelled` for a projection,
    /// `Stated` when pinned, `Assumed` for the global average held flat.
    pub fn fidelity(&self) -> Fidelity {
        self.inner.fidelity
    }

    /// How far ahead the curve reaches.
    pub fn horizon(&self) -> Duration {
        self.inner.horizon
    }

    /// The probe candidate behind the forecast, e.g. `"projection"`.
    pub fn served_by(&self) -> &'static str {
        self.inner.served_by
    }

    /// The latest forecast, at once: `None` before the first fetch has
    /// finished, `Some(None)` if it found no curve. A missing or stale curve
    /// is fetched again in the background.
    pub fn latest(&self) -> Option<Option<ForecastReport>> {
        self.ask(|_| {});
        let last = self.inner.last.lock().unwrap().clone()?;
        Some(self.inner.report(last))
    }

    /// Fetch the curve; the answer arrives on `reply`, `None` if the fetch
    /// failed or outlived its timeout. Never blocks: a fetched curve answers
    /// asks within a minute of it at once, and a fetch runs on a thread of
    /// its own.
    pub fn ask(&self, reply: impl FnOnce(Option<ForecastReport>) + Send + 'static) {
        let mut state = self.inner.state.lock().unwrap();
        match &mut *state {
            State::Fetched { at, curve } if at.elapsed() < self.inner.reuse_for => {
                let report = self.inner.report(curve.clone());
                drop(state);
                reply(report);
            }
            State::Fetching(waiting) => waiting.push(Box::new(reply)),
            _ => {
                *state = State::Fetching(vec![Box::new(reply)]);
                drop(state);
                let inner = Arc::clone(&self.inner);
                let started = thread::Builder::new()
                    .name("carbontracker-forecast".to_owned())
                    .spawn(move || inner.fetch_and_answer());
                if started.is_err() {
                    self.inner.answer(None);
                }
            }
        }
    }
}

impl Inner {
    fn report(&self, curve: Option<Vec<Sample>>) -> Option<ForecastReport> {
        Some(ForecastReport { location: self.location.clone(), fidelity: self.fidelity, horizon: self.horizon, curve: curve? })
    }

    /// Fetch on a thread of its own, waiting no longer than the timeout, and
    /// answer everyone waiting. A fetch that outlives the timeout is
    /// abandoned: its answer is dropped when it comes.
    fn fetch_and_answer(self: Arc<Self>) {
        let (sender, answer) = mpsc::channel();
        let fetch = Arc::clone(&self.fetch);
        let fetching = thread::Builder::new().name("carbontracker-forecast-fetch".to_owned()).spawn(move || {
            let _ = sender.send(fetch());
        });
        let curve = match fetching {
            Ok(_) => answer.recv_timeout(self.timeout).ok().flatten(),
            Err(_) => None,
        };
        self.answer(curve);
    }

    /// Keep `curve` as the last fetch's answer, and give it to everyone waiting.
    fn answer(&self, curve: Option<Vec<Sample>>) {
        let mut last = self.last.lock().unwrap();
        if curve.is_some() || last.is_none() {
            *last = Some(curve.clone());
        }
        drop(last);
        let previous = std::mem::replace(
            &mut *self.state.lock().unwrap(),
            State::Fetched { at: Instant::now(), curve: curve.clone() },
        );
        let State::Fetching(waiting) = previous else { return };
        for reply in waiting {
            reply(self.report(curve.clone()));
        }
    }
}

impl fmt::Debug for Forecaster {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Forecaster")
            .field("location", &self.inner.location)
            .field("fidelity", &self.inner.fidelity)
            .field("horizon", &self.inner.horizon)
            .field("served_by", &self.inner.served_by)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    use chrono::Utc;

    use super::*;

    const WAIT: Duration = Duration::from_secs(5);

    fn curve(value: f64) -> Vec<Sample> {
        vec![Sample::new(Utc::now(), value)]
    }

    /// A forecaster whose fetch counts its calls and answers `value`.
    fn counting(value: f64) -> (Forecaster, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetch: Fetch = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(curve(value))
        });
        (Forecaster::new(GridLocation::Global, Fidelity::Modelled, Duration::from_secs(3600), "test", fetch), calls)
    }

    /// Ask, and wait for the answer.
    fn ask(forecaster: &Forecaster) -> Option<ForecastReport> {
        let (reply, answer) = mpsc::channel();
        forecaster.ask(move |report| {
            let _ = reply.send(report);
        });
        answer.recv_timeout(WAIT).expect("every ask is answered")
    }

    /// AC/PROV-22 AC/PROV-23
    #[test]
    fn asking_returns_at_once_and_the_curve_arrives_on_the_reply() {
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let fetch: Fetch = Arc::new(move || {
            let _ = gate.lock().unwrap().recv();
            Some(curve(300.0))
        });
        let forecaster =
            Forecaster::new(GridLocation::Global, Fidelity::Measured, Duration::from_secs(3600), "test", fetch);

        let (reply, answer) = mpsc::channel();
        forecaster.ask(move |report| {
            let _ = reply.send(report);
        });
        assert!(answer.try_recv().is_err(), "the fetch is still blocked, and ask has returned");

        release.send(()).unwrap();
        let report = answer.recv_timeout(WAIT).unwrap().expect("fetched");
        assert_eq!(report.curve[0].value(), 300.0);
        assert_eq!((report.location, report.fidelity), (GridLocation::Global, Fidelity::Measured));
    }

    /// AC/PROV-21
    #[test]
    fn latest_returns_at_once_and_a_stale_curve_is_refreshed_in_the_background() {
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let fetch: Fetch = Arc::new(move || {
            let _ = gate.lock().unwrap().recv();
            Some(curve(300.0))
        });
        let forecaster =
            Forecaster::new(GridLocation::Global, Fidelity::Measured, Duration::from_secs(3600), "test", fetch);

        assert_eq!(forecaster.latest(), None, "nothing fetched yet, and latest has returned");
        release.send(()).unwrap();
        let deadline = Instant::now() + WAIT;
        let report = loop {
            if let Some(report) = forecaster.latest() {
                break report.expect("a curve");
            }
            assert!(Instant::now() < deadline, "the curve arrives");
            thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(report.curve[0].value(), 300.0);
    }

    /// AC/PROV-24
    #[test]
    fn asking_again_within_a_minute_answers_the_last_curve() {
        let (forecaster, calls) = counting(250.0);
        let first = ask(&forecaster).expect("fetched");
        let second = ask(&forecaster).expect("reused");

        assert_eq!(first.curve, second.curve);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// AC/PROV-24
    #[test]
    fn a_curve_older_than_its_reuse_is_fetched_again() {
        let (forecaster, calls) = counting(250.0);
        let forecaster = forecaster.with_timing(WAIT, Duration::ZERO);
        ask(&forecaster);
        ask(&forecaster);

        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// AC/PROV-24
    #[test]
    fn a_primed_forecaster_answers_its_first_ask_without_fetching() {
        let (forecaster, calls) = counting(250.0);
        let forecaster = forecaster.primed(curve(301.0));

        assert_eq!(ask(&forecaster).expect("primed").curve[0].value(), 301.0);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    /// AC/PROV-23
    #[test]
    fn a_fetch_that_hangs_is_answered_after_the_timeout() {
        let (_never, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let fetch: Fetch = Arc::new(move || {
            let _ = gate.lock().unwrap().recv();
            Some(curve(1.0))
        });
        let forecaster =
            Forecaster::new(GridLocation::Global, Fidelity::Measured, Duration::from_secs(3600), "test", fetch)
                .with_timing(Duration::from_millis(50), REUSE_FOR);

        let started = Instant::now();
        assert_eq!(ask(&forecaster), None);
        assert!(started.elapsed() < WAIT);
    }

    /// AC/PROV-24
    #[test]
    fn asks_during_a_fetch_wait_on_it() {
        let (release, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetch: Fetch = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            let _ = gate.lock().unwrap().recv();
            Some(curve(2.0))
        });
        let forecaster =
            Forecaster::new(GridLocation::Global, Fidelity::Measured, Duration::from_secs(3600), "test", fetch);

        let (reply, answers) = mpsc::channel();
        for _ in 0..3 {
            let reply = reply.clone();
            forecaster.ask(move |report| {
                let _ = reply.send(report);
            });
        }
        release.send(()).unwrap();
        for _ in 0..3 {
            assert!(answers.recv_timeout(WAIT).unwrap().is_some());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    /// AC/PROV-22
    #[test]
    fn a_failed_fetch_is_answered_with_no_curve() {
        let fetch: Fetch = Arc::new(|| None);
        let forecaster =
            Forecaster::new(GridLocation::Global, Fidelity::Measured, Duration::from_secs(3600), "test", fetch);
        assert_eq!(ask(&forecaster), None);
    }

    /// `latest()` once the fetch it started has finished.
    fn settled(forecaster: &Forecaster, calls: &AtomicUsize, fetches: usize) -> Option<Option<ForecastReport>> {
        let deadline = Instant::now() + WAIT;
        while calls.load(Ordering::SeqCst) < fetches || matches!(*forecaster.inner.state.lock().unwrap(), State::Fetching(_)) {
            assert!(Instant::now() < deadline, "the fetch finishes");
            thread::sleep(Duration::from_millis(5));
        }
        forecaster.inner.last.lock().unwrap().clone().map(|last| forecaster.inner.report(last))
    }

    /// AC/PROV-21
    #[test]
    fn latest_is_none_until_a_fetch_finishes_and_a_later_failure_keeps_the_last_curve() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        // Fails, then finds 250, then fails again.
        let fetch: Fetch = Arc::new(move || (counted.fetch_add(1, Ordering::SeqCst) == 1).then(|| curve(250.0)));
        let forecaster = Forecaster::new(GridLocation::Global, Fidelity::Measured, Duration::from_secs(3600), "test", fetch)
            .with_timing(WAIT, Duration::ZERO);

        assert_eq!(forecaster.latest(), None, "before any fetch has finished");
        assert_eq!(settled(&forecaster, &calls, 1), Some(None), "no fetch has found a curve yet");

        forecaster.latest();
        let found = settled(&forecaster, &calls, 2).flatten().expect("the second fetch found one");
        assert_eq!(found.curve[0].value(), 250.0);

        let stale = forecaster.latest().flatten().expect("answered at once");
        assert_eq!(stale.curve, found.curve);
        let kept = settled(&forecaster, &calls, 3).flatten().expect("a failed fetch keeps the last curve");
        assert_eq!(kept.curve, found.curve);
    }

    /// AC/PROV-23 AC/PROV-24
    #[test]
    fn a_fetch_is_bounded_by_ten_seconds_and_a_curve_reused_for_a_minute() {
        let (forecaster, _) = counting(1.0);
        assert_eq!(forecaster.inner.timeout, Duration::from_secs(10));
        assert_eq!(forecaster.inner.reuse_for, Duration::from_secs(60));
    }
}
