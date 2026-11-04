//! [Gate]: when one provider is read. Every rule about the cooldown, a read
//! in flight, the one queued request, the timeout and a panic lives here, as
//! a state machine driven by explicit times. It runs no threads and reads no
//! clock; the runtime does both.

use std::time::{Duration, Instant};

use crate::domain::asks::GateState;
use crate::domain::reading::{ActionWhenUnready, Cooldown, Failure, Skip, UnreadyActions};

/// Who sent a request: a cut, or the watchdog. A watchdog request never
/// queues: it only adds samples between cuts, so it bounds nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Asker {
    Cut,
    Watchdog,
}

/// How a request is answered.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Answer<T> {
    /// What the read made for it returned.
    Read(T),
    /// Not read, while all is well.
    Skipped(Skip),
    /// Not read, or not readable, and something is wrong.
    Failed(Failure),
    /// The provider has already answered everything it ever will: its
    /// sources are left out of the readout.
    Done,
}

/// What the runtime must do.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Effect<R, T> {
    /// Start a read now, answering `R` when it returns.
    Start(R),
    /// Answer `R`.
    Answer(R, Answer<T>),
}

/// What a read came back with.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Returned<T> {
    /// It returned `T`; `complete` if it answered every source with samples.
    Read { answer: T, complete: bool },
    /// It panicked, with this message.
    Panicked(String),
}

/// When one provider is read: requests `R` in, effects out, answers `T`.
pub(crate) struct Gate<R, T> {
    cooldown: Cooldown,
    policy: UnreadyActions,
    timeout: Duration,
    state: State<R>,
    /// When the last read began; `None` before the first.
    last_start: Option<Instant>,
    /// The one request waiting for the provider to be ready.
    queued: Option<R>,
    _answers: std::marker::PhantomData<fn() -> T>,
}

enum State<R> {
    /// Not reading. With `done`, a `Never` provider that has answered every
    /// source it serves.
    Idle { done: bool },
    /// A read began at `started`, answering `request`; `None` once it has
    /// been abandoned past the timeout.
    Reading {
        request: Option<R>,
        started: Instant,
    },
    /// A read panicked: never read again.
    Retired,
}

impl<R: Clone, T> Gate<R, T> {
    /// A provider that is idle, with its cooldown passed.
    pub(crate) fn new(cooldown: Cooldown, policy: UnreadyActions, timeout: Duration) -> Self {
        Self {
            cooldown,
            policy,
            timeout,
            state: State::Idle { done: false },
            last_start: None,
            queued: None,
            _answers: std::marker::PhantomData,
        }
    }

    /// A request to read the provider, arriving at `now`.
    pub(crate) fn request(&mut self, request: R, asker: Asker, now: Instant) -> Vec<Effect<R, T>> {
        let mut effects = self.advance(now);
        let (action, skip) = match self.state {
            State::Retired => return answered(effects, request, Answer::Failed(Failure::Retired)),
            State::Idle { done: true } => return answered(effects, request, Answer::Done),
            State::Reading { request: None, .. } => {
                return answered(effects, request, Answer::Failed(Failure::Stalled));
            }
            State::Idle { done: false } if self.cooled(now) => {
                effects.push(self.start(request, now));
                return effects;
            }
            State::Idle { done: false } => (self.policy.on_cooldown, Skip::Cooldown),
            State::Reading {
                request: Some(_), ..
            } => (self.policy.on_busy, Skip::Busy),
        };
        if action == ActionWhenUnready::Queue && asker == Asker::Cut && self.queued.is_none() {
            self.queued = Some(request);
            return effects;
        }
        answered(effects, request, Answer::Skipped(skip))
    }

    /// The read in flight returned at `now`.
    pub(crate) fn returned(&mut self, returned: Returned<T>, now: Instant) -> Vec<Effect<R, T>> {
        let mut effects = Vec::new();
        let State::Reading { request, .. } =
            std::mem::replace(&mut self.state, State::Idle { done: false })
        else {
            return effects;
        };
        match returned {
            Returned::Panicked(message) => {
                self.state = State::Retired;
                for request in request.into_iter().chain(self.queued.take()) {
                    effects.push(Effect::Answer(
                        request,
                        Answer::Failed(Failure::Panicked(message.clone())),
                    ));
                }
                return effects;
            }
            Returned::Read { answer, complete } => {
                let done = complete && self.cooldown == Cooldown::Never;
                self.state = State::Idle { done };
                // An abandoned read's answer is dropped.
                if let Some(request) = request {
                    effects.push(Effect::Answer(request, Answer::Read(answer)));
                }
            }
        }
        effects.extend(self.advance(now));
        effects
    }

    /// Time has reached `now`: a read may have outlived the timeout, or the
    /// cooldown before a queued request may have passed.
    pub(crate) fn advance(&mut self, now: Instant) -> Vec<Effect<R, T>> {
        let mut effects = Vec::new();
        let cooled = self.cooled(now);
        match &mut self.state {
            State::Reading { request, started }
                if now >= *started + self.timeout && request.is_some() =>
            {
                let timed_out = request.take().expect("checked above");
                effects.push(Effect::Answer(timed_out, Answer::Failed(Failure::TimedOut)));
                if let Some(queued) = self.queued.take() {
                    effects.push(Effect::Answer(queued, Answer::Failed(Failure::Stalled)));
                }
            }
            State::Idle { done: true } => {
                if let Some(queued) = self.queued.take() {
                    effects.push(Effect::Answer(queued, Answer::Done));
                }
            }
            State::Idle { done: false } if cooled => {
                if let Some(queued) = self.queued.take() {
                    effects.push(self.start(queued, now));
                }
            }
            _ => {}
        }
        effects
    }

    /// What the gate is doing at `now`, as a cut sees it.
    pub(crate) fn state_at(&self, now: Instant) -> GateState {
        match self.state {
            State::Retired | State::Reading { request: None, .. } => GateState::Failing,
            State::Reading { started, .. } if now >= started + self.timeout => GateState::Failing,
            State::Reading { .. } => GateState::Unready,
            State::Idle { done: true } => GateState::Done,
            State::Idle { done: false } if self.cooled(now) => GateState::Ready,
            State::Idle { done: false } => GateState::Unready,
        }
    }

    /// Whether the provider will never be read again: retired, or done.
    pub(crate) fn is_final(&self) -> bool {
        matches!(self.state, State::Retired | State::Idle { done: true })
    }

    /// The one request waiting for the provider to be ready.
    pub(crate) fn queued(&self) -> Option<R> {
        self.queued.clone()
    }

    /// When an idle provider's cooldown passes; `None` while it reads, is
    /// done or is retired.
    pub(crate) fn cooling_until(&self) -> Option<Instant> {
        match self.state {
            State::Idle { done: false } => self.cooled_at(),
            _ => None,
        }
    }

    /// The provider's cooldown.
    pub(crate) fn cooldown(&self) -> Cooldown {
        self.cooldown
    }

    /// When [Gate::advance] next has something to do, if ever.
    pub(crate) fn wake_at(&self) -> Option<Instant> {
        match self.state {
            State::Reading {
                request: Some(_),
                started,
            } => Some(started + self.timeout),
            State::Idle { done: false } if self.queued.is_some() => self.cooled_at(),
            _ => None,
        }
    }

    /// When the cooldown since the last read's start passes; `None` before
    /// the first read, which is never held back.
    fn cooled_at(&self) -> Option<Instant> {
        let start = self.last_start?;
        Some(match self.cooldown {
            Cooldown::After(cooldown) => start + cooldown,
            // `Never` holds no read back until the provider has answered.
            Cooldown::Never => start,
        })
    }

    fn cooled(&self, now: Instant) -> bool {
        self.cooled_at().is_none_or(|at| now >= at)
    }

    fn start(&mut self, request: R, now: Instant) -> Effect<R, T> {
        self.state = State::Reading {
            request: Some(request.clone()),
            started: now,
        };
        self.last_start = Some(now);
        Effect::Start(request)
    }
}

/// `effects`, then `request` answered with `answer`.
fn answered<R, T>(
    mut effects: Vec<Effect<R, T>>,
    request: R,
    answer: Answer<T>,
) -> Vec<Effect<R, T>> {
    effects.push(Effect::Answer(request, answer));
    effects
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(10);
    const COOLDOWN: Duration = Duration::from_secs(1);

    type TestGate = Gate<u32, &'static str>;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn gate(cooldown: Cooldown, policy: UnreadyActions) -> (TestGate, Instant) {
        (Gate::new(cooldown, policy, TIMEOUT), Instant::now())
    }

    fn queuing() -> (TestGate, Instant) {
        gate(Cooldown::After(COOLDOWN), UnreadyActions::QUEUE)
    }

    fn skipping() -> (TestGate, Instant) {
        gate(Cooldown::After(COOLDOWN), UnreadyActions::SKIP)
    }

    fn read(answer: &'static str) -> Returned<&'static str> {
        Returned::Read {
            answer,
            complete: true,
        }
    }

    fn start(request: u32) -> Vec<Effect<u32, &'static str>> {
        vec![Effect::Start(request)]
    }

    fn answer(request: u32, answer: Answer<&'static str>) -> Vec<Effect<u32, &'static str>> {
        vec![Effect::Answer(request, answer)]
    }

    /// AC/MEAS-31, AC/MEAS-37, AC/MEAS-43, AC/MEAS-49, AC/ENG-43
    #[test]
    fn an_idle_provider_with_its_cooldown_passed_is_read_at_once() {
        let (mut gate, t0) = queuing();
        assert_eq!(
            gate.request(1, Asker::Cut, t0),
            start(1),
            "a provider starts with its cooldown passed"
        );
        assert_eq!(
            gate.returned(read("a"), t0 + ms(5)),
            answer(1, Answer::Read("a"))
        );
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + COOLDOWN),
            start(2),
            "the cooldown runs from the read's start"
        );
    }

    /// AC/MEAS-19, AC/MEAS-44, AC/MEAS-51, AC/MEAS-52
    #[test]
    fn inside_the_cooldown_a_request_is_skipped_or_queued() {
        let (mut skip, t0) = skipping();
        skip.request(1, Asker::Cut, t0);
        skip.returned(read("a"), t0 + ms(5));
        assert_eq!(
            skip.request(2, Asker::Cut, t0 + ms(500)),
            answer(2, Answer::Skipped(Skip::Cooldown))
        );

        let (mut queue, t0) = queuing();
        queue.request(1, Asker::Cut, t0);
        queue.returned(read("a"), t0 + ms(5));
        assert_eq!(queue.request(2, Asker::Cut, t0 + ms(500)), vec![]);
        assert_eq!(queue.wake_at(), Some(t0 + COOLDOWN));
        assert_eq!(
            queue.advance(t0 + COOLDOWN),
            start(2),
            "the queued request gets a read of its own"
        );
    }

    /// AC/MEAS-20, AC/MEAS-32, AC/MEAS-45, AC/MEAS-53
    #[test]
    fn during_a_read_a_request_is_skipped_or_queued() {
        let (mut skip, t0) = skipping();
        skip.request(1, Asker::Cut, t0);
        assert_eq!(
            skip.request(2, Asker::Cut, t0 + ms(5)),
            answer(2, Answer::Skipped(Skip::Busy))
        );

        let (mut queue, t0) = queuing();
        queue.request(1, Asker::Cut, t0);
        assert_eq!(queue.request(2, Asker::Cut, t0 + ms(5)), vec![]);
        // The read outlives the cooldown, so the queued one starts as it returns.
        let mut effects = queue.returned(read("a"), t0 + COOLDOWN + ms(1));
        assert_eq!(effects.remove(0), Effect::Answer(1, Answer::Read("a")));
        assert_eq!(effects, start(2));
    }

    /// Each policy is per state.
    /// AC/MEAS-38, AC/MEAS-44, AC/MEAS-45
    #[test]
    fn the_policy_for_cooldown_and_busy_are_apart() {
        let policy = UnreadyActions {
            on_cooldown: ActionWhenUnready::Skip,
            on_busy: ActionWhenUnready::Queue,
        };
        let (mut gate, t0) = gate(Cooldown::After(COOLDOWN), policy);
        gate.request(1, Asker::Cut, t0);
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + ms(5)),
            vec![],
            "busy queues"
        );

        let (mut gate, t0) = self::gate(Cooldown::After(COOLDOWN), policy);
        gate.request(1, Asker::Cut, t0);
        gate.returned(read("a"), t0 + ms(5));
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + ms(10)),
            answer(2, Answer::Skipped(Skip::Cooldown)),
            "cooling skips"
        );
    }

    /// AC/MEAS-46, AC/MEAS-51
    #[test]
    fn behind_a_queued_request_every_request_is_skipped() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        gate.request(2, Asker::Cut, t0 + ms(1));
        assert_eq!(
            gate.request(3, Asker::Cut, t0 + ms(2)),
            answer(3, Answer::Skipped(Skip::Busy))
        );

        gate.returned(read("a"), t0 + ms(5));
        assert_eq!(
            gate.request(4, Asker::Cut, t0 + ms(6)),
            answer(4, Answer::Skipped(Skip::Cooldown))
        );
    }

    /// AC/MEAS-40
    #[test]
    fn a_watchdog_request_never_queues() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        assert_eq!(
            gate.request(2, Asker::Watchdog, t0 + ms(1)),
            answer(2, Answer::Skipped(Skip::Busy))
        );
        gate.returned(read("a"), t0 + ms(5));
        assert_eq!(
            gate.request(3, Asker::Watchdog, t0 + ms(6)),
            answer(3, Answer::Skipped(Skip::Cooldown))
        );
    }

    /// AC/MEAS-24, AC/MEAS-25, AC/MEAS-33, AC/MEAS-47, AC/MEAS-56
    #[test]
    fn a_read_past_the_timeout_is_abandoned_and_stalls_what_follows() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        gate.request(2, Asker::Cut, t0 + ms(1));
        assert_eq!(gate.wake_at(), Some(t0 + TIMEOUT));
        assert_eq!(
            gate.advance(t0 + TIMEOUT),
            vec![
                Effect::Answer(1, Answer::Failed(Failure::TimedOut)),
                Effect::Answer(2, Answer::Failed(Failure::Stalled))
            ]
        );
        assert_eq!(
            gate.request(3, Asker::Cut, t0 + TIMEOUT + ms(1)),
            answer(3, Answer::Failed(Failure::Stalled))
        );
        assert_eq!(gate.wake_at(), None, "nothing to do until the read returns");
    }

    /// A request after the deadline stalls even before the runtime advanced.
    /// AC/MEAS-47, AC/MEAS-56
    #[test]
    fn a_request_after_the_deadline_times_the_read_out_first() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + TIMEOUT),
            vec![
                Effect::Answer(1, Answer::Failed(Failure::TimedOut)),
                Effect::Answer(2, Answer::Failed(Failure::Stalled))
            ]
        );
    }

    /// AC/MEAS-36, AC/MEAS-56
    #[test]
    fn an_abandoned_read_is_discarded_when_it_returns() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        gate.advance(t0 + TIMEOUT);
        assert_eq!(
            gate.returned(read("late"), t0 + TIMEOUT + ms(1)),
            vec![],
            "its answer is discarded"
        );
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + TIMEOUT + ms(2)),
            start(2),
            "the provider is idle again"
        );
    }

    /// AC/MEAS-10, AC/MEAS-26, AC/MEAS-27, AC/MEAS-34, AC/MEAS-48, AC/MEAS-57
    #[test]
    fn a_panicking_provider_is_retired() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        gate.request(2, Asker::Cut, t0 + ms(1));
        let panicked = Answer::Failed(Failure::Panicked("boom".to_owned()));
        assert_eq!(
            gate.returned(Returned::Panicked("boom".to_owned()), t0 + ms(5)),
            vec![
                Effect::Answer(1, panicked.clone()),
                Effect::Answer(2, panicked)
            ]
        );
        assert_eq!(
            gate.request(3, Asker::Cut, t0 + COOLDOWN * 5),
            answer(3, Answer::Failed(Failure::Retired))
        );
        assert_eq!(gate.wake_at(), None);
    }

    /// A panic in an abandoned read still retires the provider.
    /// AC/MEAS-27, AC/MEAS-36, AC/MEAS-57
    #[test]
    fn a_panic_after_the_timeout_retires_without_answering_again() {
        let (mut gate, t0) = queuing();
        gate.request(1, Asker::Cut, t0);
        gate.advance(t0 + TIMEOUT);
        assert_eq!(
            gate.returned(Returned::Panicked("late".to_owned()), t0 + TIMEOUT + ms(1)),
            vec![]
        );
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + TIMEOUT + ms(2)),
            answer(2, Answer::Failed(Failure::Retired))
        );
    }

    /// AC/MEAS-35, AC/MEAS-36, AC/MEAS-58
    #[test]
    fn a_never_provider_is_read_until_it_has_answered() {
        let (mut gate, t0) = gate(Cooldown::Never, UnreadyActions::QUEUE);
        gate.request(1, Asker::Cut, t0);
        gate.returned(
            Returned::Read {
                answer: "partial",
                complete: false,
            },
            t0 + ms(1),
        );
        assert_eq!(
            gate.request(2, Asker::Cut, t0 + ms(2)),
            start(2),
            "not yet answered: no cooldown holds it"
        );
        gate.returned(read("all"), t0 + ms(3));
        assert_eq!(
            gate.request(3, Asker::Cut, t0 + ms(4)),
            answer(3, Answer::Done)
        );
        assert_eq!(
            gate.request(4, Asker::Watchdog, t0 + ms(5)),
            answer(4, Answer::Done)
        );
    }

    /// A request queued behind the read that completed is answered with nothing.
    /// AC/MEAS-35
    #[test]
    fn a_request_queued_behind_a_never_providers_last_read_is_done() {
        let (mut gate, t0) = gate(Cooldown::Never, UnreadyActions::QUEUE);
        gate.request(1, Asker::Cut, t0);
        gate.request(2, Asker::Cut, t0 + ms(1));
        assert_eq!(
            gate.returned(read("all"), t0 + ms(2)),
            vec![
                Effect::Answer(1, Answer::Read("all")),
                Effect::Answer(2, Answer::Done)
            ]
        );
    }
}
