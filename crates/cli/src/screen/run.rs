//! Feeds the check view: one run with no log and no command, whose items
//! reach the view through a channel until the view stops it.

use std::io;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;

use carbontracker::TrackerEvent;
use carbontracker_providers::ProbeProgress;
use carbontracker_run::{Outcome, RunConfig, Sink, SourceSelection, Started, Work};

use super::live::{Msg, Source};
use crate::args::Args;

/// Hands the run's items to the view.
struct ChannelSink(Sender<Msg>);

impl Sink for ChannelSink {
    fn probing(&mut self, step: &ProbeProgress) -> io::Result<()> {
        let msg = match step {
            ProbeProgress::Probing { candidate } => Msg::Probing(candidate),
            ProbeProgress::Decided { candidate, verdict } => Msg::Decided(candidate, verdict.clone()),
        };
        self.send(msg)
    }

    fn start(&mut self, started: &Started) -> io::Result<()> {
        let mut sources: Vec<Source> = started
            .catalog
            .sources
            .iter()
            .map(|(id, description)| Source {
                id: *id,
                description: description.clone(),
                served_by: started.served_by.get(id).copied().unwrap_or("unknown"),
            })
            .collect();
        sources.sort_by_key(|source| (source.id.provider().0, source.id.slot()));
        self.send(Msg::Started { sources, forecast: started.forecast.clone() })
    }

    fn event(&mut self, event: &TrackerEvent) -> io::Result<()> {
        match event {
            TrackerEvent::ReadoutLanded { outcomes, .. } => self.send(Msg::Readout(outcomes.clone())),
            _ => Ok(()),
        }
    }

    fn error(&mut self, text: &str) -> io::Result<()> {
        self.send(Msg::Failed(text.to_owned()))
    }

    fn name(&self) -> &str {
        "check view"
    }
}

impl ChannelSink {
    /// A closed view ends the sink.
    fn send(&self, msg: Msg) -> io::Result<()> {
        self.0.send(msg).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the view closed"))
    }
}

/// The running feed; dropping it stops the run without waiting.
pub struct Feed {
    pub rx: Receiver<Msg>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Feed {
    /// A feed whose messages come from `rx`, with no run behind it.
    #[cfg(test)]
    pub fn fake(rx: Receiver<Msg>) -> Self {
        Self { rx, stop: None, thread: None }
    }

    /// Stops the run, and waits for it to finish once it has `started`.
    /// While still probing it is left to end with the process.
    pub fn stop(mut self, started: bool) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if started && let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts a run for `args` on a thread of its own, as `track` would but with
/// no log and no command, and one span open so every source is read.
pub fn start(args: &Args) -> Result<Feed, String> {
    let probe = args.probe(&args.keys()?)?;
    let tracker = args.tracker()?;
    let (tx, rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let failed = tx.clone();
        let config = RunConfig { probe, tracker, frontend: "cli", project: None, name: None, command: crate::args::hide_keys(std::env::args()), sinks: vec![Box::new(ChannelSink(tx))] };
        let run = match carbontracker_run::start(config, |_, _| ()) {
            Ok(run) => run,
            Err(error) => {
                let _ = failed.send(Msg::Failed(error.to_string()));
                return;
            }
        };
        let span = run.start_span("check", None, SourceSelection::All);
        let _ = stop_rx.recv();
        run.stop_span(span, Work::default());
        let _ = run.finish(Outcome::Interrupted { epochs_done: None });
    });
    Ok(Feed { rx, stop: Some(stop_tx), thread: Some(thread) })
}
