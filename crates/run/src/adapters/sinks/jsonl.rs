//! [JsonlSink]: writes the run's [record](crate::domain::record).

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;

use carbontracker::{Timestamp, TrackerEvent};

use crate::domain::encode::Encoder;
use crate::domain::item::{Outcome, Started};
use crate::domain::record::{Line, Record};
use crate::ports::Sink;

/// Writes a run's record: its header, the probe and the catalog, then a line
/// per event, gap and outcome. Skips probe steps, warnings and blocks: the
/// record holds what was measured.
pub struct JsonlSink {
    out: Option<LineWriter<Box<dyn Write + Send>>>,
    /// Created at [Sink::start], so a run that never starts leaves no record.
    path: Option<PathBuf>,
    encoder: Encoder,
    now: Box<dyn FnMut() -> Timestamp + Send>,
}

impl JsonlSink {
    /// Writes to `out`, from the run's start.
    pub fn new(out: impl Write + Send + 'static) -> Self {
        Self {
            out: Some(LineWriter::new(Box::new(out))),
            path: None,
            encoder: Encoder::default(),
            now: Box::new(chrono::Utc::now),
        }
    }

    /// Writes to a new file at `path`, created when the run starts.
    pub fn create(path: PathBuf) -> Self {
        Self {
            out: None,
            path: Some(path),
            encoder: Encoder::default(),
            now: Box::new(chrono::Utc::now),
        }
    }

    /// Uses `now` for lines without their own timestamp, e.g. in tests.
    pub fn with_clock(mut self, now: impl FnMut() -> Timestamp + Send + 'static) -> Self {
        self.now = Box::new(now);
        self
    }

    fn write(&mut self, at: Option<Timestamp>, line: Line) -> io::Result<()> {
        let at = at.unwrap_or_else(|| (self.now)());
        match &mut self.out {
            Some(out) => out.write(&Record { at, line }),
            None => Ok(()),
        }
    }
}

impl Sink for JsonlSink {
    fn start(&mut self, started: &Started) -> io::Result<()> {
        if let Some(path) = self.path.take() {
            self.out = Some(LineWriter::new(Box::new(BufWriter::new(File::create_new(
                path,
            )?))));
        }
        self.write(None, Line::Run(started.header.clone()))?;
        self.write(None, Line::Probe(started.probe.clone()))?;
        let catalog = self.encoder.catalog(&started.catalog);
        self.write(None, catalog)
    }

    fn event(&mut self, event: &TrackerEvent) -> io::Result<()> {
        let (at, line) = self.encoder.event(event);
        self.write(at, line)
    }

    fn gap(&mut self) -> io::Result<()> {
        self.write(None, Line::Gap)
    }

    fn outcome(&mut self, outcome: &Outcome) -> io::Result<()> {
        let line = self.encoder.outcome(outcome);
        self.write(None, line)
    }

    fn finish(&mut self) -> io::Result<()> {
        match &mut self.out {
            Some(out) => out.flush(),
            None => Ok(()),
        }
    }

    fn name(&self) -> &str {
        "record"
    }
}

/// Writes [Record]s, one JSON object per line.
struct LineWriter<W> {
    out: W,
}

impl<W: Write> LineWriter<W> {
    fn new(out: W) -> Self {
        Self { out }
    }

    fn write(&mut self, record: &Record) -> io::Result<()> {
        serde_json::to_writer(&mut self.out, record)?;
        self.out.write_all(b"\n")
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
