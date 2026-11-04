//! The read gate over generated streams of requests: bursts of cuts,
//! watchdog ticks, and reads that are quick, slow, hang or panic.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use proptest::collection::vec;
use proptest::prelude::*;

use crate::domain::asks::{Asked, Asks, GateState};
use crate::domain::gate::{Answer, Asker, Effect, Gate, Returned};
use crate::domain::{ReadoutId, Timestamp};
use crate::domain::reading::{ActionWhenUnready, Cooldown, UnreadyActions};

const TIMEOUT: Duration = Duration::from_millis(100);

/// How one read behaves.
#[derive(Clone, Copy, Debug)]
enum Read {
    /// Returns after this long; `complete` if it answered every source.
    Returns {
        after: Duration,
        complete: bool,
    },
    Panics {
        after: Duration,
    },
    /// Never returns.
    Hangs,
}

fn read() -> impl Strategy<Value = Read> {
    let after = prop_oneof![
        3 => (0u64..20).prop_map(Duration::from_millis),
        1 => (20u64..300).prop_map(Duration::from_millis),
    ];
    prop_oneof![
        12 => (after.clone(), any::<bool>()).prop_map(|(after, complete)| Read::Returns { after, complete }),
        1 => after.prop_map(|after| Read::Panics { after }),
        1 => Just(Read::Hangs),
    ]
}

fn cooldown() -> impl Strategy<Value = Cooldown> {
    prop_oneof![
        Just(Cooldown::After(Duration::ZERO)),
        Just(Cooldown::After(Duration::from_millis(10))),
        Just(Cooldown::After(Duration::from_millis(250))),
        Just(Cooldown::Never),
    ]
}

fn action() -> impl Strategy<Value = ActionWhenUnready> {
    prop_oneof![
        Just(ActionWhenUnready::Skip),
        Just(ActionWhenUnready::Queue)
    ]
}

/// Requests as (gap since the last, who asked): often bursts of zero gaps.
fn requests() -> impl Strategy<Value = Vec<(Duration, Asker)>> {
    let gap = prop_oneof![
        3 => Just(Duration::ZERO),
        3 => (0u64..5).prop_map(Duration::from_millis),
        2 => (5u64..400).prop_map(Duration::from_millis),
    ];
    let asker = prop_oneof![4 => Just(Asker::Cut), 1 => Just(Asker::Watchdog)];
    vec((gap, asker), 1..200)
}

/// The longest a queued request waits: the read ahead of it ends within the
/// timeout, the cooldown counts from that read's start, and its own read ends
/// within the timeout.
fn bound(cooldown: Cooldown) -> Duration {
    let cooldown = match cooldown {
        Cooldown::After(cooldown) => cooldown,
        Cooldown::Never => Duration::ZERO,
    };
    cooldown.max(TIMEOUT) + TIMEOUT
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// AC/MEAS-40, AC/MEAS-42, AC/MEAS-49, AC/MEAS-50, AC/MEAS-51, AC/MEAS-53, AC/MEAS-54, AC/ENG-42, AC/ENG-43
    #[test]
    fn every_request_is_answered_once_and_the_provider_is_read_by_the_rules(
        cooldown in cooldown(),
        on_cooldown in action(),
        on_busy in action(),
        requests in requests(),
        reads in vec(read(), 1..16),
    ) {
        let t0 = Instant::now();
        let mut gate: Gate<usize, ()> = Gate::new(cooldown, UnreadyActions { on_cooldown, on_busy }, TIMEOUT);

        let mut arrivals = Vec::new();
        let mut at = t0;
        for (index, (gap, asker)) in requests.iter().enumerate() {
            at += *gap;
            arrivals.push((at, index, *asker));
        }

        let mut answered: BTreeMap<usize, Instant> = BTreeMap::new();
        let mut arrived: BTreeMap<usize, Instant> = BTreeMap::new();
        // The read in flight, abandoned or not: when it returns, and how.
        let mut in_flight: Option<(Option<Instant>, Read)> = None;
        let mut reading: Option<usize> = None;
        let mut last_start: Option<Instant> = None;
        let mut last_return: Option<Instant> = None;
        let mut reads_made = 0;
        let mut next = 0;

        loop {
            let returns_at = in_flight.and_then(|(at, _)| at);
            let candidates = [arrivals.get(next).map(|(at, ..)| *at), returns_at, gate.wake_at()];
            let Some(now) = candidates.into_iter().flatten().min() else { break };

            let effects = if returns_at == Some(now) {
                let (_, read) = in_flight.take().expect("a read returns");
                reading = None;
                last_return = Some(now);
                let returned = match read {
                    Read::Returns { complete, .. } => Returned::Read { answer: (), complete },
                    Read::Panics { .. } => Returned::Panicked("boom".to_owned()),
                    Read::Hangs => unreachable!("a hung read never returns"),
                };
                gate.returned(returned, now)
            } else if arrivals.get(next).map(|(at, ..)| *at) == Some(now) {
                let (_, index, asker) = arrivals[next];
                next += 1;
                arrived.insert(index, now);
                let effects = gate.request(index, asker, now);
                // A watchdog request is answered or read at once: never queued.
                if asker == Asker::Watchdog {
                    let handled = effects.iter().any(|effect| match effect {
                        Effect::Start(request) | Effect::Answer(request, _) => *request == index,
                    });
                    prop_assert!(handled, "watchdog request {} was queued", index);
                }
                effects
            } else {
                gate.advance(now)
            };

            for effect in effects {
                match effect {
                    Effect::Start(request) => {
                        prop_assert!(in_flight.is_none(), "two reads in flight at {:?}", now - t0);
                        if let (Some(last), Cooldown::After(cooldown)) = (last_start, cooldown) {
                            prop_assert!(now >= last + cooldown, "read inside the cooldown at {:?}", now - t0);
                        }
                        // A queued request starts as soon as the provider is ready.
                        if arrived.get(&request).is_some_and(|&at| at < now) {
                            let cooled = match (last_start, cooldown) {
                                (Some(last), Cooldown::After(cooldown)) => last + cooldown,
                                (Some(last), Cooldown::Never) => last,
                                (None, _) => t0,
                            };
                            let ready = cooled.max(last_return.unwrap_or(t0));
                            prop_assert_eq!(now, ready, "queued request {} started late", request);
                        }
                        last_start = Some(now);
                        let read = reads[reads_made % reads.len()];
                        reads_made += 1;
                        let returns = match read {
                            Read::Returns { after, .. } | Read::Panics { after } => Some(now + after),
                            Read::Hangs => None,
                        };
                        in_flight = Some((returns, read));
                        reading = Some(request);
                    }
                    Effect::Answer(request, answer) => {
                        prop_assert!(answered.insert(request, now).is_none(), "request {} answered twice", request);
                        if matches!(answer, Answer::Read(())) {
                            prop_assert_eq!(reading, None, "a read answers once it has returned");
                        }
                    }
                }
            }

            // At most one request waits that is neither answered nor being read.
            let waiting = arrived
                .keys()
                .filter(|request| !answered.contains_key(request) && reading != Some(**request))
                .count();
            prop_assert!(waiting <= 1, "{} requests queued at {:?}", waiting, now - t0);
        }

        prop_assert_eq!(answered.len(), requests.len(), "every request is answered");
        for (request, at) in &answered {
            let waited = *at - arrived[request];
            prop_assert!(waited <= bound(cooldown), "request {} waited {:?}", request, waited);
        }
    }

    /// Cuts asking through [Asks], then the gate. Stamps and the
    /// gate's clock are one clock.
    /// AC/MEAS-11, AC/MEAS-14, AC/PROF-1, AC/TST-18
    #[test]
    fn cuts_rely_on_requests_by_the_rules(
        cooldown in cooldown(),
        on_cooldown in action(),
        on_busy in action(),
        gaps in vec(prop_oneof![3 => Just(0u64), 3 => 0u64..5, 2 => 5u64..400], 1..150),
        reads in vec(read(), 1..16),
    ) {
        let queue = on_cooldown == ActionWhenUnready::Queue && on_busy == ActionWhenUnready::Queue;
        let t0 = Instant::now();
        let stamp = |at: Instant| -> Timestamp {
            chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::from_std(at - t0).expect("small")
        };
        let mut gate: Gate<ReadoutId, ()> = Gate::new(cooldown, UnreadyActions { on_cooldown, on_busy }, TIMEOUT);
        let mut asks = if cooldown == Cooldown::Never { Asks::never() } else { Asks::default() };

        let mut cuts_at = Vec::new();
        let mut at = t0;
        for gap in &gaps {
            at += Duration::from_millis(*gap);
            cuts_at.push(at);
        }

        // Per cut: when, and the request it relies on.
        let mut relied: Vec<(Instant, Option<ReadoutId>)> = Vec::new();
        let mut started: BTreeMap<ReadoutId, Instant> = BTreeMap::new();
        // Per request: when it was answered, and whether by a read.
        let mut answers: BTreeMap<ReadoutId, (Instant, bool, bool)> = BTreeMap::new();
        let mut in_flight: Option<(Option<Instant>, Read)> = None;
        let mut reads_made = 0;
        let mut next_id = 1;
        let mut next = 0;
        // Requests cuts sent since the provider last changed state, while unready.
        let mut sent_while_unready = 0;

        loop {
            let returns_at = in_flight.and_then(|(at, _)| at);
            let candidates = [cuts_at.get(next).copied(), returns_at, gate.wake_at()];
            let Some(now) = candidates.into_iter().flatten().min() else { break };

            let effects = if returns_at == Some(now) {
                let (_, read) = in_flight.take().expect("a read returns");
                asks.transition();
                sent_while_unready = 0;
                let returned = match read {
                    Read::Returns { complete, .. } => Returned::Read { answer: (), complete },
                    Read::Panics { .. } => Returned::Panicked("boom".to_owned()),
                    Read::Hangs => unreachable!("a hung read never returns"),
                };
                gate.returned(returned, now)
            } else if cuts_at.get(next) == Some(&now) {
                next += 1;
                let state = gate.state_at(now);
                if state == GateState::Ready {
                    asks.transition();
                    sent_while_unready = 0;
                }
                match asks.cut(stamp(now), state, gate.queued()) {
                    Asked::Relies(request) => {
                        relied.push((now, Some(request)));
                        Vec::new()
                    }
                    Asked::Nothing => {
                        relied.push((now, None));
                        Vec::new()
                    }
                    Asked::Send => {
                        let request = ReadoutId(next_id);
                        next_id += 1;
                        if state == GateState::Unready {
                            sent_while_unready += 1;
                            prop_assert!(sent_while_unready <= 1, "two requests sent in one unready stretch at {:?}", now - t0);
                        }
                        relied.push((now, Some(request)));
                        gate.request(request, Asker::Cut, now)
                    }
                }
            } else {
                gate.advance(now)
            };

            for effect in effects {
                match effect {
                    Effect::Start(request) => {
                        asks.transition();
                        sent_while_unready = 0;
                        asks.started(request, stamp(now));
                        started.insert(request, now);
                        let read = reads[reads_made % reads.len()];
                        reads_made += 1;
                        let returns = match read {
                            Read::Returns { after, .. } | Read::Panics { after } => Some(now + after),
                            Read::Hangs => None,
                        };
                        in_flight = Some((returns, read));
                    }
                    Effect::Answer(request, answer) => {
                        let read = matches!(answer, Answer::Read(()));
                        if matches!(answer, Answer::Failed(crate::domain::reading::Failure::TimedOut)) {
                            asks.transition();
                            sent_while_unready = 0;
                        }
                        // Failed, or Done: a done provider answers every cut at once.
                        let failed = matches!(answer, Answer::Failed(_) | Answer::Done);
                        asks.answered(request, Asker::Cut, read);
                        answers.insert(request, (now, read, failed));
                    }
                }
            }
        }

        for (cut, request) in &relied {
            let Some(request) = request else { continue };
            let (answered_at, read, failed) = answers[request];
            if queue {
                prop_assert!(
                    failed || (read && (started[request] >= *cut || cooldown == Cooldown::Never)),
                    "under Queue the cut at {:?} relied on request {:?}, read {}, failed {}", *cut - t0, request, read, failed
                );
                prop_assert!(answered_at.saturating_duration_since(*cut) <= bound(cooldown), "cut at {:?} waited too long", *cut - t0);
            }
        }
    }
}
