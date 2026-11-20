//! The stateful replay: one case in, what the engine did out. The real
//! [Engine] runs one input at a time with no threads: each provider's reads
//! follow the case's script, and every input is given its time. A busy host
//! is a stall: the engine handles nothing until it ends, then takes what
//! became ready meanwhile in the real loop's order. A run is a pure function
//! of its case, so a failure replays exactly.
//!
//! Times are microseconds from the session's opening. Sources are keyed
//! `provider.slot`.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

use super::{Domain, Kind, Selection, SegmentOutput, SettledOutput, UsageOutput, add, micros, parse, power_domain, selection_of, stamp};
use crate::adapters::outbound::readers::{Logged, ReadReturn, Reader, Readers};
use crate::application::runtime::{Engine, Input as EngineInput};
use crate::application::subscribers::subscription;
use crate::application::tracker::Tracker;
use crate::config::Announcements;
use crate::domain::reading::UnreadyActions;
use crate::domain::source::{GridLocation, SourceCatalog, SourceDescription};
use crate::domain::{
    Cooldown, Fidelity, PowerScope, PowerUsageEffectiveness, ProviderError, ProviderId, Sample, SampleKind, SourceId,
    SpanId, TrackerCommand, TrackerEvent, TrackerMessage, Work,
};
use crate::ports::outbound::{Provider, ProviderResponse, Reading};

/// One case: the providers and their scripted reads, the world, and the program.
#[derive(Debug, Deserialize)]
pub struct Input {
    pub pue: f64,
    pub readout_interval_us: u64,
    pub timeout_us: u64,
    pub providers: Vec<ProviderInput>,
    /// Each power source's intensity source.
    #[serde(default)]
    pub pairs: BTreeMap<String, String>,
    /// Each source's true value over time: piecewise linear, flat beyond its ends.
    #[serde(default)]
    pub truth: BTreeMap<String, Vec<(u64, f64)>>,
    pub program: Vec<Op>,
    /// Stretches `[from, until)` in which the engine handles nothing, as on a
    /// busy host.
    #[serde(default)]
    pub stalls_us: Vec<(u64, u64)>,
}

#[derive(Debug, Deserialize)]
pub struct ProviderInput {
    pub id: usize,
    /// `None` is `Never`.
    pub cooldown_us: Option<u64>,
    pub policy: Policy,
    /// The provider's reads in order; the last repeats.
    pub reads: Vec<ReadInput>,
    pub sources: Vec<SourceInput>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    Queue,
    Skip,
}

/// One read: returns after `latency_us`, with nothing new if `empty`; or
/// panics after `panic_after_us`; or never returns, with `hang`.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct ReadInput {
    pub latency_us: Option<u64>,
    #[serde(default)]
    pub empty: bool,
    pub panic_after_us: Option<u64>,
    #[serde(default)]
    pub hang: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SourceInput {
    Power { slot: u16, domain: Domain, sample: Kind },
    Intensity { slot: u16 },
}

#[derive(Debug, Deserialize)]
pub struct Op {
    pub at_us: u64,
    #[serde(flatten)]
    pub body: Body,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum Body {
    Start {
        span: usize,
        #[serde(default)]
        parent: Option<usize>,
        #[serde(default)]
        selection: Selection,
    },
    Mark {
        span: usize,
    },
    Stop {
        span: usize,
    },
    Finish,
}

/// What the engine did.
#[derive(Debug, Default, Serialize)]
pub struct Output {
    pub requests: Vec<RequestOutput>,
    pub cuts: Vec<CutOutput>,
    /// Every segment accounted, in the order accounted.
    pub segments: Vec<SegmentOutput>,
    pub settled: Vec<SettledOutput>,
    /// Per span (0 is the root), per source: what it was billed itself.
    pub ledgers: BTreeMap<String, BTreeMap<String, UsageOutput>>,
    pub session: SessionOutput,
    pub refused: Vec<RefusedOutput>,
}

#[derive(Debug, Serialize)]
pub struct RequestOutput {
    pub id: usize,
    pub provider: usize,
    /// `cut` or `watchdog`.
    pub asker: &'static str,
    pub sent_us: u64,
    pub started_us: Option<u64>,
    pub answered_us: Option<u64>,
    pub answer: Option<&'static str>,
    /// It waited for the provider to be ready before its read started.
    pub queued: bool,
    pub failure: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CutOutput {
    pub at_us: u64,
    pub op_index: usize,
    /// What each source cut relied on; `null` if nothing.
    pub relied: BTreeMap<String, Option<usize>>,
}

#[derive(Debug, Default, Serialize)]
pub struct SessionOutput {
    pub total_energy_j: f64,
    pub total_emissions_g: Option<f64>,
    pub ended_us: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct RefusedOutput {
    pub op_index: usize,
    pub error: String,
}

/// Run one case given as JSON; the output as JSON.
///
/// # Errors
///
/// A message if the case is not valid JSON or cannot be set up.
pub fn run_json(case: &str) -> Result<String, String> {
    let input: Input = serde_json::from_str(case).map_err(|error| format!("invalid case: {error}"))?;
    let output = run(&input)?;
    serde_json::to_string(&output).map_err(|error| error.to_string())
}

/// A provider the replay never reads: it only states its cooldown.
struct Scripted(Cooldown);

impl Provider for Scripted {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        unreachable!("the replay reads nothing itself")
    }

    fn cooldown(&self) -> Cooldown {
        self.0
    }
}

/// A read's result, waiting for the engine to see it.
struct Pending {
    provider: ProviderId,
    result: Result<Result<ProviderResponse, ProviderError>, String>,
    returned_us: u64,
}

/// Run one case.
///
/// # Errors
///
/// A message if the case names a source it does not declare, or never ends.
pub fn run(input: &Input) -> Result<Output, String> {
    let catalog = catalog(input)?;
    let pue = PowerUsageEffectiveness::new(input.pue).ok_or_else(|| format!("PUE {} is below 1", input.pue))?;
    let timeout = Duration::from_micros(input.timeout_us);
    let mut readers = HashMap::new();
    let mut serves: HashMap<ProviderId, Vec<(SourceId, Option<Kind>)>> = HashMap::new();
    for provider in &input.providers {
        let id = ProviderId(provider.id);
        let cooldown = provider.cooldown_us.map_or(Cooldown::Never, |us| Cooldown::After(Duration::from_micros(us)));
        let policy = match provider.policy {
            Policy::Queue => UnreadyActions::QUEUE,
            Policy::Skip => UnreadyActions::SKIP,
        };
        let sources: Vec<(SourceId, Option<Kind>)> = provider
            .sources
            .iter()
            .map(|source| match source {
                SourceInput::Power { slot, sample, .. } => (SourceId::new(id, *slot), Some(*sample)),
                SourceInput::Intensity { slot } => (SourceId::new(id, *slot), None),
            })
            .collect();
        let ids = sources.iter().map(|(source, _)| *source).collect();
        readers.insert(id, Reader::new(Box::new(Scripted(cooldown)), ids, policy, timeout));
        serves.insert(id, sources);
    }
    let script: HashMap<ProviderId, &ProviderInput> =
        input.providers.iter().map(|provider| (ProviderId(provider.id), provider)).collect();

    let t0 = Instant::now();
    let mut tracker = Tracker::new(catalog, pue, Announcements::default(), stamp(0));
    let (subscriber, mut events) = subscription(1 << 22);
    tracker.subscribe(subscriber);
    let (readers, jobs) = Readers::detached(readers, Duration::from_micros(input.readout_interval_us));
    let mut engine = Engine::new(tracker, readers);

    let mut replay = Replay {
        input,
        jobs,
        serves,
        script,
        started: HashMap::new(),
        pending: BinaryHeap::new(),
        results: HashMap::new(),
        next_result: 0,
        requests: BTreeMap::new(),
        output: Output::default(),
        replies: Vec::new(),
    };

    engine.open(stamp(0), t0, stamp(0));
    replay.after_step(&mut engine, 0, None, &mut events);

    let (mut next_op, mut now_us) = (0, 0);
    for _ in 0..10_000_000 {
        if engine.is_done() {
            break;
        }
        let op_at = input.program.get(next_op).map(|op| op.at_us);
        let returned_at = replay.pending.peek().map(|Reverse((at, _))| *at);
        let wake = engine.wake_at().map(|at| ceil_micros(at.saturating_duration_since(t0)));
        let Some(next) = [op_at, returned_at, wake].into_iter().flatten().min() else { break };
        now_us = now_us.max(next);
        while let Some(&(_, until)) = input.stalls_us.iter().find(|&&(from, until)| from <= now_us && now_us < until) {
            now_us = until;
        }
        let (now, wall) = (t0 + Duration::from_micros(now_us), stamp(now_us));

        // As the real loop takes what is ready: a result that has come back,
        // then a command, then the deadline.
        if returned_at.is_some_and(|at| at <= now_us) {
            let Reverse((_, n)) = replay.pending.pop().expect("peeked");
            let pending = replay.results.remove(&n).expect("scheduled");
            let read = ReadReturn::new(pending.provider, pending.result, t0 + Duration::from_micros(pending.returned_us));
            engine.step(EngineInput::Returned(read), now, wall);
            replay.after_step(&mut engine, now_us, None, &mut events);
        } else if op_at.is_some_and(|at| at <= now_us) {
            let index = next_op;
            next_op += 1;
            let message = replay.message(index)?;
            engine.step(EngineInput::Command(message), now, wall);
            replay.after_step(&mut engine, now_us, Some(index), &mut events);
        } else {
            engine.step(EngineInput::Tick, now, wall);
            replay.after_step(&mut engine, now_us, None, &mut events);
        }
    }
    if !engine.is_done() && input.program.iter().any(|op| matches!(op.body, Body::Finish)) {
        return Err("the session never ended".to_owned());
    }
    Ok(replay.finish())
}

/// The replay's state between steps.
struct Replay<'a> {
    input: &'a Input,
    jobs: HashMap<ProviderId, Receiver<()>>,
    serves: HashMap<ProviderId, Vec<(SourceId, Option<Kind>)>>,
    script: HashMap<ProviderId, &'a ProviderInput>,
    /// How many reads each provider has started.
    started: HashMap<ProviderId, usize>,
    /// Results by when they returned, then by when they were scheduled.
    pending: BinaryHeap<Reverse<(u64, usize)>>,
    results: HashMap<usize, Pending>,
    next_result: usize,
    requests: BTreeMap<usize, RequestOutput>,
    output: Output,
    /// Kept so a finish's reply has somewhere to go.
    replies: Vec<oneshot::Receiver<Result<crate::domain::SessionReport, crate::domain::TrackerError>>>,
}

impl Replay<'_> {
    /// The message for op `index`, stamped when the caller made it.
    fn message(&mut self, index: usize) -> Result<TrackerMessage, String> {
        let at = stamp(self.input.program[index].at_us);
        Ok(match &self.input.program[index].body {
            Body::Start { span, parent, selection } => TrackerMessage::Command(TrackerCommand::Start {
                id: SpanId::new(*span),
                name: format!("span {span}"),
                parent: parent.map(SpanId::new),
                sources: selection_of(selection)?,
                at,
            }),
            Body::Mark { span } => TrackerMessage::Command(TrackerCommand::Mark { id: SpanId::new(*span), work: Work::default(), at }),
            Body::Stop { span } => TrackerMessage::Command(TrackerCommand::Stop { id: SpanId::new(*span), work: Work::default(), at }),
            Body::Finish => {
                let (reply, report) = oneshot::channel();
                self.replies.push(report);
                TrackerMessage::Finish { at, reply }
            }
        })
    }

    /// Record what the step at `now_us` did, and start the reads it asked for.
    fn after_step(&mut self, engine: &mut Engine, now_us: u64, op: Option<usize>, events: &mut crate::application::subscribers::Subscription) {
        for logged in engine.take_requests() {
            match logged {
                Logged::Sent { request, provider, watchdog } => {
                    self.requests.insert(request.0, RequestOutput {
                        id: request.0,
                        provider: provider.0,
                        asker: if watchdog { "watchdog" } else { "cut" },
                        sent_us: now_us,
                        started_us: None,
                        answered_us: None,
                        answer: None,
                        queued: false,
                        failure: None,
                    });
                }
                Logged::Started { request } => {
                    if let Some(record) = self.requests.get_mut(&request.0) {
                        record.started_us = Some(now_us);
                        record.queued = now_us > record.sent_us;
                    }
                }
                Logged::Answered { request, answer, failure } => {
                    if let Some(record) = self.requests.get_mut(&request.0) {
                        record.answered_us = Some(now_us);
                        record.answer = Some(answer);
                        record.failure = failure;
                    }
                }
            }
        }

        let relied = engine.take_relied();
        if let Some(index) = op {
            let mut by_source = BTreeMap::new();
            for (source, request) in relied {
                by_source.insert(source.to_string(), request.map(|request| request.0));
            }
            if !by_source.is_empty() {
                self.output.cuts.push(CutOutput { at_us: self.input.program[index].at_us, op_index: index, relied: by_source });
            }
        }

        for (segment, billed) in engine.session_mut().take_accounted() {
            let source = segment.source.to_string();
            let mut whole = UsageOutput { energy_j: 0.0, emissions_g: Some(0.0) };
            for (span, usage) in billed {
                add(&mut whole, usage);
                let ledger = self.output.ledgers.entry(span.get().to_string()).or_default();
                add(ledger.entry(source.clone()).or_insert(UsageOutput { energy_j: 0.0, emissions_g: Some(0.0) }), usage);
            }
            self.output.segments.push(SegmentOutput {
                source,
                from_us: micros(segment.from),
                to_us: micros(segment.to),
                claimants: segment.claimants.iter().map(SpanId::get).collect(),
                accounted_us: now_us,
                energy_j: whole.energy_j,
                emissions_g: whole.emissions_g,
            });
        }

        while let Ok(Some(event)) = events.try_recv() {
            match event {
                TrackerEvent::SpanSettled { report } => {
                    self.output.settled.push(SettledOutput { span: report.span_id.get(), at_us: now_us });
                }
                TrackerEvent::CommandRefused { error, .. } => {
                    self.output.refused.push(RefusedOutput { op_index: op.unwrap_or(usize::MAX), error: error.to_string() });
                }
                TrackerEvent::SessionEnded { .. } => self.output.session.ended_us = Some(now_us),
                _ => {}
            }
        }

        let providers: Vec<ProviderId> = self.jobs.keys().copied().collect();
        for provider in providers {
            while self.jobs[&provider].try_recv().is_ok() {
                self.start_read(provider, now_us);
            }
        }
    }

    /// `provider` starts its next scripted read at `now_us`.
    fn start_read(&mut self, provider: ProviderId, now_us: u64) {
        let script = self.script[&provider];
        let k = self.started.entry(provider).or_default();
        let read = nth(&script.reads, *k).unwrap_or(ReadInput { latency_us: Some(0), ..ReadInput::default() });
        *k += 1;
        if read.hang {
            return;
        }
        let (latency, result) = match read.panic_after_us {
            Some(after) => (after, Err("the provider panicked".to_owned())),
            None => (read.latency_us.unwrap_or(0), Ok(Ok(self.response(provider, now_us, read.empty)))),
        };
        let returned_us = now_us + latency;
        let n = self.next_result;
        self.next_result += 1;
        self.results.insert(n, Pending { provider, result, returned_us });
        self.pending.push(Reverse((returned_us, n)));
    }

    /// What `provider` reads at `at_us`: each source's true value then, or
    /// nothing new.
    fn response(&self, provider: ProviderId, at_us: u64, empty: bool) -> ProviderResponse {
        let mut response = ProviderResponse::new();
        for &(source, kind) in &self.serves[&provider] {
            if empty {
                response.insert(source, Reading::Unchanged);
                continue;
            }
            let curve = self.input.truth.get(&source.to_string()).map_or(&[][..], Vec::as_slice);
            let value = match kind {
                // A counter reads the energy since the opening, in J.
                Some(Kind::Total) => integral(curve, 0, at_us) * 1e-6,
                _ => value_at(curve, at_us),
            };
            response.samples(source, vec![Sample::new(stamp(at_us), value)]);
        }
        response
    }

    fn finish(mut self) -> Output {
        self.output.requests = self.requests.into_values().collect();
        let mut total = UsageOutput { energy_j: 0.0, emissions_g: Some(0.0) };
        for segment in &self.output.segments {
            total.energy_j += segment.energy_j;
            total.emissions_g = total.emissions_g.zip(segment.emissions_g).map(|(sum, grams)| sum + grams);
        }
        self.output.session.total_energy_j = total.energy_j;
        self.output.session.total_emissions_g = total.emissions_g;
        self.output
    }
}

/// Item `k` of `items`, the last repeating; `None` if there are none.
fn nth<T: Copy>(items: &[T], k: usize) -> Option<T> {
    items.get(k.min(items.len().saturating_sub(1))).copied()
}

fn ceil_micros(duration: Duration) -> u64 {
    duration.as_nanos().div_ceil(1_000) as u64
}

/// The curve's value at `t`: linear between its points, flat beyond its ends.
fn value_at(curve: &[(u64, f64)], t: u64) -> f64 {
    let Some(&(first_t, first_v)) = curve.first() else { return 0.0 };
    if t <= first_t {
        return first_v;
    }
    for pair in curve.windows(2) {
        let ((ta, va), (tb, vb)) = (pair[0], pair[1]);
        if t <= tb {
            return if tb == ta { vb } else { va + (vb - va) * (t - ta) as f64 / (tb - ta) as f64 };
        }
    }
    curve.last().map_or(0.0, |&(_, v)| v)
}

/// The exact integral of the curve from `a` to `b`, in value × µs.
fn integral(curve: &[(u64, f64)], a: u64, b: u64) -> f64 {
    if b <= a {
        return 0.0;
    }
    let mut points = vec![a];
    points.extend(curve.iter().map(|&(t, _)| t).filter(|&t| t > a && t < b));
    points.push(b);
    points.windows(2).map(|w| (value_at(curve, w[0]) + value_at(curve, w[1])) / 2.0 * (w[1] - w[0]) as f64).sum()
}

fn catalog(input: &Input) -> Result<SourceCatalog, String> {
    let mut descriptions = HashMap::new();
    for provider in &input.providers {
        for source in &provider.sources {
            let (slot, description) = match source {
                SourceInput::Power { slot, domain, sample } => (
                    *slot,
                    SourceDescription::Power {
                        device_name: format!("{domain:?}"),
                        domain: power_domain(*domain),
                        scope: PowerScope::DeviceTotal,
                        sample_kind: match sample {
                            Kind::Instant => SampleKind::Instant,
                            Kind::Held => SampleKind::Held,
                            Kind::Total => SampleKind::Total,
                        },
                        fidelity: Fidelity::Measured,
                    },
                ),
                SourceInput::Intensity { slot } => (
                    *slot,
                    SourceDescription::Intensity {
                        location: GridLocation::Zone { zone_id: format!("replay {}.{slot}", provider.id) },
                        fidelity: Fidelity::Measured,
                    },
                ),
            };
            descriptions.insert(SourceId::new(ProviderId(provider.id), slot), description);
        }
    }
    let pairs = input
        .pairs
        .iter()
        .map(|(power, intensity)| Ok((parse(power)?, parse(intensity)?)))
        .collect::<Result<_, String>>()?;
    Ok(SourceCatalog { descriptions, pairs })
}
