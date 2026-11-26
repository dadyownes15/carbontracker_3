//! [ConsoleSink]: a run for people, on a terminal or in `output.log`.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write};

use carbontracker::{
    GridLocation, Observation, ReportWarning, SessionReport, SourceDescription, SourceId, SpanId, TrackerEvent,
};
use carbontracker_providers::ProbeProgress;

use super::Verbosity;
use crate::domain::item::{Block, Layout, Outcome, Started};
use crate::domain::presentation::{self, PREFIX, Ready, events, paint};
use crate::ports::Sink;

/// Warnings and errors at every verbosity; from [Verbosity::Normal] what is
/// measured and the closing block; at [Verbosity::Everything] every probe
/// step, event and readout, stamped with the time of day.
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
    /// For naming spans in later events, and sources in failed reads.
    spans: HashMap<SpanId, String>,
    sources: HashMap<SourceId, SourceDescription>,
    served_by: HashMap<SourceId, &'static str>,
    /// Whether the user asked for the world-average grid.
    global: bool,
    /// The span warnings printed so far, each printed once per run.
    span_warnings: HashSet<String>,
    /// The warnings themselves, without their span, so the session's own
    /// copy is not printed again.
    warned: HashSet<String>,
    /// Paint lines for a terminal.
    colour: bool,
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
            span_warnings: HashSet::new(),
            warned: HashSet::new(),
            colour: false,
            sources: HashMap::new(),
            served_by: HashMap::new(),
            global: false,
        }
    }

    /// Stamps event lines with `now` instead of the time of day, e.g. in
    /// tests.
    pub fn with_clock(mut self, now: impl FnMut() -> String + Send + 'static) -> Self {
        self.now = Box::new(now);
        self
    }

    /// Paints lines for a terminal when `colour`.
    pub fn with_colour(mut self, colour: bool) -> Self {
        self.colour = colour;
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
        self.say_with(text, paint::line)
    }

    /// Each line of `text`, prefixed, painted by `painter` on a terminal.
    fn say_with(&mut self, text: &str, painter: fn(&str) -> String) -> io::Result<()> {
        if !self.colour {
            return writeln!(self.out, "{}", presentation::prefixed(text));
        }
        for line in text.lines() {
            writeln!(self.out, "{}{}", paint::prefix(), painter(line))?;
        }
        Ok(())
    }

    /// `text` as printed whole: the prefix on its first line only.
    fn print(&mut self, text: &str) -> io::Result<()> {
        self.print_with(text, str::to_owned)
    }

    /// `text` as printed whole, painted by `painter` on a terminal.
    fn print_with(&mut self, text: &str, painter: fn(&str) -> String) -> io::Result<()> {
        match self.colour {
            true => writeln!(self.out, "{}{}", paint::prefix(), painter(text)),
            false => writeln!(self.out, "{PREFIX}{text}"),
        }
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

    /// `warning` without the sources the user asked for; `None` when every
    /// source in it was asked for.
    fn without_asked(&self, warning: &ReportWarning) -> Option<ReportWarning> {
        let ReportWarning::Estimated { sources } = warning else {
            return Some(warning.clone());
        };
        if !self.global {
            return Some(warning.clone());
        }
        // The engine labels an intensity source by its description.
        let world: Vec<String> = self
            .sources
            .values()
            .filter(|source| matches!(source, SourceDescription::Intensity { location: GridLocation::Global, .. }))
            .map(ToString::to_string)
            .collect();
        let sources: Vec<String> = sources.iter().filter(|source| !world.contains(source)).cloned().collect();
        (!sources.is_empty()).then_some(ReportWarning::Estimated { sources })
    }

    /// The session's closing block, in the layout.
    fn closing(&mut self, report: &SessionReport) -> io::Result<()> {
        match self.layout.clone() {
            Layout::Summary { title } => {
                writeln!(self.out)?;
                let block = presentation::summary(&title, report, self.logs.as_deref(), true);
                let text = if self.colour { paint::summary(&block) } else { block.plain() };
                self.print_with(&text, str::to_owned)?;
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
        self.served_by = started.served_by.clone();
        self.global = started.header.config.global();
        self.monitor = started.header.epochs.as_ref().map(|epochs| epochs.monitor);
        if !self.normal() {
            return Ok(());
        }
        // Only a prediction reads the forecast.
        let predicts = started.header.epochs.as_ref().is_some_and(|epochs| epochs.before_prediction > 0);
        let forecast = started.forecast.as_ref().filter(|_| predicts);
        for line in presentation::source_lines(&started.catalog, &started.served_by, forecast) {
            let text = if self.colour { paint::source_line(&line) } else { line.plain() };
            self.say_with(&text, str::to_owned)?;
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
            TrackerEvent::ReadoutLanded { outcomes, .. } => {
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
                        // Healthy: the last value still stands.
                        Observation::Unchanged | Observation::Skipped(_) => {}
                        Observation::Failed(error) => {
                            let provider = self.served_by.get(source).copied();
                            let read = description.map_or_else(|| label.clone(), presentation::part);
                            self.warning(&presentation::failed_read(&read, provider, &error.to_string()))?;
                        }
                    }
                }
                if everything && !figures.is_empty() {
                    self.log(&events::readout(&figures))?;
                }
            }
            TrackerEvent::CommandRefused { span, error } => {
                let name = self.span_name(*span);
                self.warning(&presentation::refused(&name, &error.to_string()))?;
            }
            TrackerEvent::SpanSettled { report } => {
                if everything {
                    self.log(&events::settled(report))?;
                }
                let warnings: Vec<_> =
                    report.warnings.iter().filter_map(|warning| self.without_asked(warning)).collect();
                for warning in warnings {
                    self.warned.insert(warning.to_string());
                    let text = presentation::span_warning(&report.span_name, &warning.to_string());
                    if self.span_warnings.insert(text.clone()) {
                        self.warning(&text)?;
                    }
                }
                self.spans.remove(&report.span_id);
            }
            TrackerEvent::SessionEnded { report } => {
                let warnings: Vec<_> =
                    report.warnings.iter().filter_map(|warning| self.without_asked(warning)).collect();
                // A warning already printed for a span is not printed again.
                let fresh: Vec<String> =
                    warnings.iter().map(ToString::to_string).filter(|text| !self.warned.contains(text)).collect();
                for text in fresh {
                    self.warning(&text)?;
                }
                if self.normal() {
                    self.closing(report)?;
                }
            }
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
        if self.normal() {
            for line in block.lines(&self.layout) {
                writeln!(self.out, "{line}")?;
            }
        }
        for warning in block.warnings() {
            self.warning(&warning)?;
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
