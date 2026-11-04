//! [ConsoleSink]: a run for people, on a terminal or in `output.log`.

use std::collections::HashMap;
use std::io::{self, Write};

use carbontracker::{Observation, SessionReport, SourceDescription, SourceId, SpanId, TrackerEvent};
use carbontracker_providers::ProbeProgress;

use super::Verbosity;
use crate::domain::item::{Block, Layout, Outcome, Started};
use crate::domain::presentation::{self, PREFIX, Ready, events};
use crate::ports::Sink;

/// What is measured, the closing block, and at [Verbosity::Everything] every
/// probe step and event, stamped with the time of day.
pub struct ConsoleSink {
    out: Box<dyn Write + Send>,
    verbosity: Verbosity,
    layout: Layout,
    /// The run folder, for display.
    logs: Option<String>,
    /// Said "gathering sources…".
    gathering: bool,
    /// The epochs monitored, for 2.4.7's interrupted line.
    monitor: Option<u64>,
    now: Box<dyn FnMut() -> String + Send>,
    /// For naming spans in later events, and sources in failed readouts.
    spans: HashMap<SpanId, String>,
    sources: HashMap<SourceId, SourceDescription>,
}

impl ConsoleSink {
    pub fn new(
        out: impl Write + Send + 'static,
        verbosity: Verbosity,
        layout: Layout,
        logs: Option<String>,
    ) -> Self {
        Self {
            out: Box::new(out),
            verbosity,
            layout,
            logs,
            gathering: false,
            monitor: None,
            now: Box::new(|| chrono::Local::now().format("%H:%M:%S").to_string()),
            spans: HashMap::new(),
            sources: HashMap::new(),
        }
    }

    /// Stamps event lines with `now` instead of the time of day, e.g. in
    /// tests.
    pub fn with_clock(mut self, now: impl FnMut() -> String + Send + 'static) -> Self {
        self.now = Box::new(now);
        self
    }

    fn normal(&self) -> bool {
        self.verbosity >= Verbosity::Normal
    }

    fn everything(&self) -> bool {
        self.verbosity >= Verbosity::Everything
    }

    /// Each line of `text`, prefixed.
    fn say(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.out, "{}", presentation::prefixed(text))
    }

    /// `text` as printed whole: the prefix on its first line only.
    fn print(&mut self, text: &str) -> io::Result<()> {
        writeln!(self.out, "{PREFIX}{text}")
    }

    /// An event line, stamped.
    fn log(&mut self, text: &str) -> io::Result<()> {
        let stamp = (self.now)();
        for line in text.lines() {
            writeln!(self.out, "{PREFIX}{stamp}  {line}")?;
        }
        Ok(())
    }

    fn span_name(&self, span: SpanId) -> String {
        self.spans
            .get(&span)
            .cloned()
            .unwrap_or_else(|| format!("span {}", span.get()))
    }

    fn label(&self, source: SourceId) -> (Option<&SourceDescription>, String) {
        let description = self.sources.get(&source);
        (
            description,
            description
                .map(presentation::source)
                .unwrap_or_else(|| source.to_string()),
        )
    }

    /// The session's closing block, in the layout.
    fn closing(&mut self, report: &SessionReport) -> io::Result<()> {
        match self.layout.clone() {
            Layout::Summary { title } => {
                writeln!(self.out)?;
                self.print(&presentation::summary(
                    &title,
                    report,
                    self.logs.as_deref(),
                    true,
                ))?;
            }
            Layout::Legacy {
                decimals,
                equivalents,
            } => {
                let layout = presentation::Layout::Legacy { decimals };
                let block = presentation::consumption(
                    "Actual consumption:",
                    report.duration,
                    report.total,
                    layout,
                    equivalents,
                );
                self.print(&format!("\n{block}"))?;
                if let Some(grid) = presentation::grid_line(report) {
                    self.say(&grid)?;
                }
                if let Some(logs) = self.logs.clone() {
                    self.say(&presentation::logs_line(&logs))?;
                }
            }
        }
        for note in presentation::notes(report) {
            self.say(&note)?;
        }
        if matches!(self.layout, Layout::Legacy { .. }) {
            self.say(presentation::FINISHED)?;
        }
        Ok(())
    }
}

impl Sink for ConsoleSink {
    fn probing(&mut self, step: &ProbeProgress) -> io::Result<()> {
        if self.normal() && !self.gathering {
            self.gathering = true;
            self.say("gathering sources…")?;
        }
        if self.everything() {
            self.log(&step.to_string())?;
        }
        Ok(())
    }

    fn start(&mut self, started: &Started) -> io::Result<()> {
        self.sources = started.catalog.sources.clone();
        self.monitor = started.header.epochs.as_ref().map(|epochs| epochs.monitor);
        if !self.normal() {
            return Ok(());
        }
        for line in presentation::sources(&started.catalog, &started.served_by, started.forecast.as_ref()) {
            self.say(&line)?;
        }
        let ready = match &self.layout {
            Layout::Summary { title } => presentation::ready(Ready::Command(title)),
            Layout::Legacy { .. } => presentation::ready(Ready::Tracking),
        };
        self.say(&ready)
    }

    fn event(&mut self, event: &TrackerEvent) -> io::Result<()> {
        let everything = self.everything();
        match event {
            TrackerEvent::SpanStarted {
                span, name, parent, ..
            } => {
                self.spans.insert(*span, name.clone());
                if everything {
                    let parent = parent.map(|parent| self.span_name(parent));
                    self.log(&events::started(name, parent.as_deref()))?;
                }
            }
            TrackerEvent::SpanStopped { span, .. } if everything => {
                self.log(&events::stopped(&self.span_name(*span)))?;
            }
            TrackerEvent::Marked { span, work, .. } if everything => {
                let name = self.span_name(*span);
                for entry in work.entries() {
                    self.log(&events::marked(&name, &entry.kind, entry.amount))?;
                }
            }
            TrackerEvent::ReadoutLanded { outcomes, .. } if everything => {
                let mut outcomes: Vec<_> = outcomes.iter().collect();
                outcomes.sort_by_key(|(source, _)| **source);
                let mut figures = Vec::new();
                for (source, outcome) in outcomes {
                    let (description, label) = self.label(*source);
                    match outcome {
                        Observation::Samples(samples) => figures.extend(
                            events::figure(description, samples)
                                .map(|figure| format!("{label} {figure}")),
                        ),
                        // Nothing new to show; the last value still stands.
                        Observation::Unchanged | Observation::Skipped(_) => {}
                        Observation::Failed(error) => {
                            self.log(&events::source_failed(&label, &error.to_string()))?
                        }
                    }
                }
                if !figures.is_empty() {
                    self.log(&events::readout(&figures))?;
                }
            }
            TrackerEvent::CommandRefused { span, error } if everything => {
                let name = self.span_name(*span);
                self.log(&events::refused(&name, &error.to_string()))?;
            }
            TrackerEvent::SpanSettled { report } => {
                if everything {
                    for line in events::settled(report) {
                        self.log(&line)?;
                    }
                }
                self.spans.remove(&report.span_id);
            }
            TrackerEvent::SessionEnded { report } if self.normal() => self.closing(report)?,
            _ => {}
        }
        Ok(())
    }

    fn gap(&mut self) -> io::Result<()> {
        self.say(&presentation::warning(events::GAP))
    }

    fn outcome(&mut self, outcome: &Outcome) -> io::Result<()> {
        if self.everything() {
            self.log(&presentation::outcome(outcome))?;
        }
        // carbontracker 2.4.7's line when training stops early.
        if let (
            true,
            Layout::Legacy { .. },
            Outcome::Interrupted {
                epochs_done: Some(_),
            },
            Some(monitor),
        ) = (self.normal(), &self.layout, outcome, self.monitor)
        {
            self.say(&presentation::interrupted(monitor as usize))?;
        }
        Ok(())
    }

    fn warning(&mut self, text: &str) -> io::Result<()> {
        self.say(&presentation::warning(text))
    }

    fn error(&mut self, text: &str) -> io::Result<()> {
        self.say(&presentation::error(text))
    }

    fn block(&mut self, block: &dyn Block) -> io::Result<()> {
        if !self.normal() {
            return Ok(());
        }
        for line in block.lines(&self.layout) {
            writeln!(self.out, "{line}")?;
        }
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        self.out.flush()
    }

    fn name(&self) -> &str {
        "console"
    }
}
