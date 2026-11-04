//! Runs read back from their records: what each measured, summed across
//! runs, and their settled spans grouped by path.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

use super::record::{self, Line, Outcome, Session};

/// The runs read so far, and the files that could not be read.
#[derive(Debug, Clone, Default)]
pub struct Report {
    /// In the order they started.
    pub runs: Vec<Run>,
    pub unreadable: Vec<Unreadable>,
    spans: Vec<SpanGroup>,
    index: HashMap<String, usize>,
}

/// One run's record.
#[derive(Debug, Clone)]
pub struct Run {
    /// Where the record was read from.
    pub file: String,
    pub started: DateTime<Utc>,
    pub frontend: String,
    pub command: Vec<String>,
    /// The name of the first span opened with no parent: for the CLI, the
    /// command it tracked.
    pub first_root_span: Option<String>,
    pub outcome: Option<Outcome>,
    /// `None` if the run never ended normally.
    pub session: Option<Session>,
}

impl Run {
    /// How the run ended, e.g. `completed`; `None` if that was not recorded.
    pub fn ended(&self) -> Option<String> {
        self.outcome.as_ref().map(|outcome| outcome.ended.name().to_owned())
    }

    /// Whether the run wrote its `session` line.
    pub fn is_complete(&self) -> bool {
        self.session.is_some()
    }
}

/// A file that could not be read, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct Unreadable {
    pub file: String,
    pub reason: String,
}

/// The complete runs, summed.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Total {
    pub runs: usize,
    pub duration_s: f64,
    pub energy_kwh: f64,
    /// Over the runs with emissions; `None` if none had any.
    pub emissions_g: Option<f64>,
    /// Runs whose energy had no intensity to bill it against.
    pub without_emissions: usize,
}

/// The settled spans at one path, across runs.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanGroup {
    /// The names from the root down, e.g. `epoch/eval`.
    pub path: String,
    pub count: u64,
    pub energy_kwh: f64,
    pub min_kwh: f64,
    pub max_kwh: f64,
    /// Over the spans with emissions; `None` if none had any.
    pub emissions_g: Option<f64>,
    /// Spans whose energy had no intensity to bill it against.
    pub without_emissions: u64,
}

impl SpanGroup {
    pub fn mean_kwh(&self) -> f64 {
        self.energy_kwh / self.count as f64
    }
}

impl Report {
    /// Adds the record read from `file`, or names it as unreadable.
    pub fn add(&mut self, file: &str, text: &str) {
        let records = match record::read(text) {
            Ok(records) => records,
            Err(reason) => {
                self.unreadable.push(Unreadable { file: file.to_owned(), reason });
                return;
            }
        };
        let mut lines = records.into_iter();
        let Some(record::Record { at: started, line: Line::Run(header) }) = lines.next() else {
            unreachable!("read checks for the header");
        };
        let mut run = Run {
            file: file.to_owned(),
            started,
            frontend: header.frontend,
            command: header.command,
            first_root_span: None,
            outcome: None,
            session: None,
        };
        let mut started_spans: HashMap<u64, (String, Option<u64>)> = HashMap::new();
        for record in lines {
            match record.line {
                Line::SpanStarted(span) => {
                    if span.parent.is_none() && run.first_root_span.is_none() {
                        run.first_root_span = Some(span.name.clone());
                    }
                    started_spans.insert(span.span, (span.name, span.parent));
                }
                Line::SpanSettled(span) => {
                    let path = path(&started_spans, span.span, &span.name);
                    self.settle(path, span.energy_kwh, span.emissions_g);
                }
                Line::Outcome(outcome) => run.outcome = Some(outcome),
                Line::Session(session) => run.session = Some(session),
                _ => {}
            }
        }
        let at = self.runs.partition_point(|other| other.started <= run.started);
        self.runs.insert(at, run);
    }

    /// The runs without a `session` line, left out of [Report::total].
    pub fn incomplete(&self) -> impl Iterator<Item = &Run> {
        self.runs.iter().filter(|run| !run.is_complete())
    }

    /// The complete runs' `session` lines, summed.
    pub fn total(&self) -> Total {
        let mut total = Total::default();
        for session in self.runs.iter().filter_map(|run| run.session.as_ref()) {
            total.runs += 1;
            total.duration_s += session.duration_s;
            total.energy_kwh += session.energy_kwh;
            match session.emissions_g {
                Some(grams) => *total.emissions_g.get_or_insert(0.0) += grams,
                None => total.without_emissions += 1,
            }
        }
        total
    }

    /// The span groups, in the order their paths were first seen.
    pub fn spans(&self) -> &[SpanGroup] {
        &self.spans
    }

    fn settle(&mut self, path: String, kwh: f64, grams: Option<f64>) {
        let spans = &mut self.spans;
        let index = *self.index.entry(path.clone()).or_insert_with(|| {
            spans.push(SpanGroup {
                path,
                count: 0,
                energy_kwh: 0.0,
                min_kwh: f64::INFINITY,
                max_kwh: f64::NEG_INFINITY,
                emissions_g: None,
                without_emissions: 0,
            });
            spans.len() - 1
        });
        let group = &mut self.spans[index];
        group.count += 1;
        group.energy_kwh += kwh;
        group.min_kwh = group.min_kwh.min(kwh);
        group.max_kwh = group.max_kwh.max(kwh);
        match grams {
            Some(grams) => *group.emissions_g.get_or_insert(0.0) += grams,
            None => group.without_emissions += 1,
        }
    }
}

/// The names from the root down to `span`, joined by `/`.
fn path(started: &HashMap<u64, (String, Option<u64>)>, span: u64, name: &str) -> String {
    let mut names = vec![name];
    let mut parent = started.get(&span).and_then(|(_, parent)| *parent);
    while let Some(id) = parent
        && let Some((name, next)) = started.get(&id)
        && names.len() <= started.len()
    {
        names.push(name);
        parent = *next;
    }
    names.reverse();
    names.join("/")
}
