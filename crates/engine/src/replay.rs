//! The pure replay: one case in, what the session did out. No threads, no
//! clock and no providers. The case says what each cut relies on and when
//! each readout lands with which samples; the replay hands both to a
//! [Session] and reports what it accounted and settled.
//!
//! Times are microseconds from the session's opening. Sources are keyed
//! `provider.slot`; a cut's `relies` is keyed by provider id.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::TimeDelta;
use serde::{Deserialize, Serialize};

use crate::domain::session::{Relying, RequestId};
use crate::domain::source::{GridLocation, SourceCatalog, SourceDescription};
use crate::domain::{
    Fidelity, PowerDomain, PowerScope, PowerUsageEffectiveness, ProviderId, ReadoutId, ReportWarning, Sample, SampleKind,
    SampleStore,
    Session, SourceId, SourceSelection, SpanId, Timestamp, Usage,
};

/// One case.
#[derive(Debug, Deserialize)]
pub struct Input {
    pub pue: f64,
    pub sources: Vec<SourceInput>,
    /// Each power source's intensity source.
    #[serde(default)]
    pub pairs: BTreeMap<String, String>,
    /// In the order the session sees them.
    pub events: Vec<Event>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SourceInput {
    Power { id: String, domain: Domain, sample: Kind },
    Intensity { id: String },
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Domain {
    Cpu,
    Gpu,
    Ram,
    Ane,
    System,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Instant,
    Held,
    Total,
}

#[derive(Debug, Deserialize)]
pub struct Event {
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
        #[serde(default)]
        relies: BTreeMap<String, usize>,
    },
    Mark {
        span: usize,
        #[serde(default)]
        relies: BTreeMap<String, usize>,
    },
    Stop {
        span: usize,
        #[serde(default)]
        relies: BTreeMap<String, usize>,
    },
    Finish {
        #[serde(default)]
        relies: BTreeMap<String, usize>,
    },
    /// A readout lands: its samples are stored, then the session is told.
    /// A source left out of `samples` had nothing new.
    Land {
        request: usize,
        #[serde(default)]
        samples: BTreeMap<String, Vec<(u64, f64)>>,
    },
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
pub enum Selection {
    #[default]
    #[serde(skip)]
    Unset,
    Word(String),
    Domains { domains: Vec<Domain> },
    Sources { sources: Vec<String> },
}

/// What the session did.
#[derive(Debug, Default, Serialize)]
pub struct Output {
    /// Every segment accounted, in the order accounted.
    pub segments: Vec<SegmentOutput>,
    pub settled: Vec<SettledOutput>,
    /// Each settled span's report, as it settled.
    pub spans: Vec<SpanOutput>,
    /// Per span (0 is the root), per source: what it was billed itself.
    pub ledgers: BTreeMap<usize, BTreeMap<String, UsageOutput>>,
    pub session: SessionOutput,
    pub refused: Vec<RefusedOutput>,
}

#[derive(Debug, Serialize)]
pub struct SegmentOutput {
    pub source: String,
    pub from_us: u64,
    pub to_us: u64,
    /// 0 is the root.
    pub claimants: Vec<usize>,
    /// The time of the event that accounted it.
    pub accounted_us: u64,
    /// Over every claimant, PUE included.
    pub energy_j: f64,
    pub emissions_g: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct SettledOutput {
    pub span: usize,
    pub at_us: u64,
}

#[derive(Debug, Serialize)]
pub struct SpanOutput {
    pub span: usize,
    /// Per selected power source, the samples it had inside the span.
    pub samples: BTreeMap<String, usize>,
    /// The kinds of warning the report gives, e.g. `thin_coverage`.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct UsageOutput {
    pub energy_j: f64,
    pub emissions_g: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub struct SessionOutput {
    pub total_energy_j: f64,
    pub total_emissions_g: Option<f64>,
    /// Whether every segment was accounted once `finish` had been sent.
    pub drained: bool,
}

#[derive(Debug, Serialize)]
pub struct RefusedOutput {
    pub event: usize,
    pub error: String,
}

/// Run one case given as JSON; the output as JSON.
///
/// # Errors
///
/// A message if the case is not valid JSON or names an unknown source.
pub fn run_json(case: &str) -> Result<String, String> {
    let input: Input = serde_json::from_str(case).map_err(|error| format!("invalid case: {error}"))?;
    let output = run(&input)?;
    serde_json::to_string(&output).map_err(|error| error.to_string())
}

/// Run one case.
///
/// # Errors
///
/// A message if the case names a source it does not declare.
pub fn run(input: &Input) -> Result<Output, String> {
    let catalog = catalog(input)?;
    let mut store = SampleStore::new_from_source(&catalog.descriptions, 1 << 20).map_err(|error| error.to_string())?;
    let pue = PowerUsageEffectiveness::new(input.pue).ok_or_else(|| format!("PUE {} is below 1", input.pue))?;
    let mut session = Session::new(catalog, pue, stamp(0));
    let mut output = Output::default();
    let mut answered = HashSet::new();
    let mut finished = false;

    for (index, event) in input.events.iter().enumerate() {
        let at = stamp(event.at_us);
        let result = match &event.body {
            Body::Start { span, parent, selection, relies } => session.start(
                SpanId::new(*span),
                format!("span {span}"),
                parent.map(SpanId::new),
                selection_of(selection)?,
                at,
                Relies::new(relies, &answered)?,
            ),
            Body::Mark { span, relies } => session.mark(SpanId::new(*span), at, Relies::new(relies, &answered)?),
            Body::Stop { span, relies } => session.stop(SpanId::new(*span), at, Relies::new(relies, &answered)?),
            Body::Finish { relies } => {
                session.finish(at, Relies::new(relies, &answered)?);
                finished = true;
                Ok(())
            }
            Body::Land { request, samples } => {
                for (key, points) in samples {
                    let source = parse(key)?;
                    if !session.catalog().descriptions.contains_key(&source) {
                        return Err(format!("the case gives samples of {key}, which it does not declare"));
                    }
                    let points = points.iter().map(|&(t, value)| Sample::new(stamp(t), value)).collect();
                    // A sample its series refuses is dropped, as the tracker does.
                    let _ = store.store(source, points);
                }
                answered.insert(ReadoutId(*request));
                for span in session.land(ReadoutId(*request), &store) {
                    settle(&session, &mut output, span, at, event.at_us);
                }
                Ok(())
            }
        };
        if let Err(error) = result {
            output.refused.push(RefusedOutput { event: index, error: error.to_string() });
        }
        // A cut can account at once: everything it relies on has landed.
        for span in session.account_ready(&store) {
            settle(&session, &mut output, span, at, event.at_us);
        }
        log_accounted(&mut session, &mut output, event.at_us);
    }

    let mut total = UsageOutput { energy_j: 0.0, emissions_g: Some(0.0) };
    for segment in &output.segments {
        total.energy_j += segment.energy_j;
        total.emissions_g = total.emissions_g.zip(segment.emissions_g).map(|(sum, grams)| sum + grams);
    }
    output.session = SessionOutput {
        total_energy_j: total.energy_j,
        total_emissions_g: total.emissions_g,
        drained: finished && session.is_drained(),
    };
    Ok(output)
}

/// A cut's requests, one per provider.
struct Relies<'a> {
    requests: HashMap<ProviderId, RequestId>,
    answered: &'a HashSet<RequestId>,
}

impl<'a> Relies<'a> {
    fn new(relies: &BTreeMap<String, usize>, answered: &'a HashSet<RequestId>) -> Result<Self, String> {
        let requests = relies
            .iter()
            .map(|(provider, &request)| {
                let provider = provider.parse().map_err(|_| format!("provider id {provider:?} is not a number"))?;
                Ok((ProviderId(provider), ReadoutId(request)))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self { requests, answered })
    }
}

impl Relying for Relies<'_> {
    fn relied(&mut self, source: SourceId, _: Timestamp) -> Option<RequestId> {
        self.requests.get(&source.provider()).copied()
    }

    fn is_answered(&self, request: RequestId) -> bool {
        self.answered.contains(&request)
    }
}

/// Log that `span` settled, with its report.
fn settle(session: &Session, output: &mut Output, span: SpanId, at: Timestamp, at_us: u64) {
    output.settled.push(SettledOutput { span: span.get(), at_us });
    if let Ok(report) = session.span_report(span, at) {
        let warnings = report
            .warnings
            .iter()
            .map(|warning| {
                match warning {
                    ReportWarning::ThinCoverage { .. } => "thin_coverage",
                    ReportWarning::NoIntensity { .. } => "no_intensity",
                    ReportWarning::HeldIntensity { .. } => "held_intensity",
                    ReportWarning::Estimated { .. } => "estimated",
                    ReportWarning::NoSelectedSources => "no_selected_sources",
                }
                .to_owned()
            })
            .collect();
        let samples = report.samples.iter().map(|(id, &n)| (id.to_string(), n)).collect();
        output.spans.push(SpanOutput { span: span.get(), samples, warnings });
    }
}

fn log_accounted(session: &mut Session, output: &mut Output, at_us: u64) {
    for (segment, billed) in session.take_accounted() {
        let source = segment.source.to_string();
        let mut whole = UsageOutput { energy_j: 0.0, emissions_g: Some(0.0) };
        for (span, usage) in billed {
            add(&mut whole, usage);
            let ledger = output.ledgers.entry(span.get()).or_default();
            add(ledger.entry(source.clone()).or_insert(UsageOutput { energy_j: 0.0, emissions_g: Some(0.0) }), usage);
        }
        output.segments.push(SegmentOutput {
            source,
            from_us: micros(segment.from),
            to_us: micros(segment.to),
            claimants: segment.claimants.iter().map(SpanId::get).collect(),
            accounted_us: at_us,
            energy_j: whole.energy_j,
            emissions_g: whole.emissions_g,
        });
    }
}

fn add(into: &mut UsageOutput, usage: Usage) {
    into.energy_j += usage.energy.get();
    into.emissions_g = match (into.emissions_g, usage.emissions) {
        (Some(sum), Some(grams)) => Some(sum + grams.get()),
        _ => None,
    };
}

fn catalog(input: &Input) -> Result<SourceCatalog, String> {
    let mut descriptions = HashMap::new();
    for source in &input.sources {
        let (id, description) = match source {
            SourceInput::Power { id, domain, sample } => (
                id,
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
            SourceInput::Intensity { id } => (
                id,
                SourceDescription::Intensity {
                    location: GridLocation::Zone { zone_id: format!("replay {id}") },
                    fidelity: Fidelity::Measured,
                },
            ),
        };
        descriptions.insert(parse(id)?, description);
    }
    let pairs = input
        .pairs
        .iter()
        .map(|(power, intensity)| Ok((parse(power)?, parse(intensity)?)))
        .collect::<Result<_, String>>()?;
    Ok(SourceCatalog { descriptions, pairs })
}

fn selection_of(selection: &Selection) -> Result<SourceSelection, String> {
    Ok(match selection {
        Selection::Unset => SourceSelection::All,
        Selection::Word(word) if word == "all" => SourceSelection::All,
        Selection::Word(word) => return Err(format!("unknown selection {word:?}")),
        Selection::Domains { domains } => SourceSelection::Domains(domains.iter().copied().map(power_domain).collect()),
        Selection::Sources { sources } => SourceSelection::Sources(sources.iter().map(|key| parse(key)).collect::<Result<_, _>>()?),
    })
}

fn power_domain(domain: Domain) -> PowerDomain {
    match domain {
        Domain::Cpu => PowerDomain::Cpu,
        Domain::Gpu => PowerDomain::Gpu,
        Domain::Ram => PowerDomain::Ram,
        Domain::Ane => PowerDomain::Ane,
        Domain::System => PowerDomain::System,
    }
}

fn parse(key: &str) -> Result<SourceId, String> {
    let bad = || format!("source id {key:?} is not provider.slot");
    let (provider, slot) = key.split_once('.').ok_or_else(bad)?;
    Ok(SourceId::new(ProviderId(provider.parse().map_err(|_| bad())?), slot.parse().map_err(|_| bad())?))
}

/// `us` microseconds after the session opened.
fn stamp(us: u64) -> Timestamp {
    chrono::DateTime::UNIX_EPOCH + TimeDelta::microseconds(us as i64)
}

fn micros(at: Timestamp) -> u64 {
    (at - chrono::DateTime::UNIX_EPOCH).num_microseconds().unwrap_or(0).max(0) as u64
}

#[cfg(test)]
mod tests;
