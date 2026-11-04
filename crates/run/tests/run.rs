//! A run through its public seam: [start], spans, [RunHandle::finish], over
//! pinned sources from the default registry: no hardware, no network.

use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbontracker::{GramsCo2ePerKwh, GridLocation, TrackerConfig, TrackerEvent, Watts};
use carbontracker_providers::{GridConfig, Pinned, ProbeConfig};
use carbontracker_run::{
    LatestForecast, Console, Folder, Layout, Outcome, Predictor, RunConfig, Sink, Started, Verbosity,
    Work, standard_sinks, start,
};

/// A writer the test reads back.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Shared {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

fn probe() -> ProbeConfig {
    ProbeConfig {
        grid: GridConfig {
            location: Some(GridLocation::Global),
            ..GridConfig::default()
        },
        pinned: Pinned {
            power: Some(Watts::new(10.0).unwrap()),
            intensity: Some(GramsCo2ePerKwh::new(100.0).unwrap()),
            ..Pinned::default()
        },
        ..ProbeConfig::default()
    }
}

fn tracker() -> TrackerConfig {
    TrackerConfig {
        readout_interval: Duration::from_millis(20),
        ..TrackerConfig::default()
    }
}

fn config(sinks: Vec<Box<dyn Sink>>) -> RunConfig {
    RunConfig {
        probe: probe(),
        tracker: tracker(),
        frontend: "test",
        sinks,
    }
}

fn console(out: &Shared, verbosity: Verbosity) -> Console {
    Console {
        out: Box::new(out.clone()),
        verbosity,
        layout: Layout::Summary {
            title: "work".to_owned(),
        },
    }
}

fn legacy(out: &Shared) -> Console {
    Console {
        out: Box::new(out.clone()),
        verbosity: Verbosity::Normal,
        layout: Layout::Legacy {
            decimals: 6,
            equivalents: true,
        },
    }
}

/// One span of `work`, then finish.
fn one_span(config: RunConfig) {
    let run = start(config, |_, _| ()).unwrap();
    let span = run.start_span("work", None);
    std::thread::sleep(Duration::from_millis(50));
    run.stop_span(span, Work::default());
    run.finish(Outcome::Exited { code: 0 }).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn record(folder: &Folder) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(folder.record()).unwrap();
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// AC/RUN-15, AC/RUN-22, AC/FE-48
#[test]
fn the_folder_holds_the_record_and_the_consoles_text() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let out = Shared::default();
    one_span(config(
        standard_sinks(console(&out, Verbosity::Normal), Some(&folder)).unwrap(),
    ));

    let record_name = folder
        .record()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(names(folder.path()), ["output.log".to_owned(), record_name]);
    let output = std::fs::read_to_string(folder.output_log()).unwrap();
    assert_eq!(
        output,
        out.text(),
        "output.log is the console at normal verbosity"
    );
    assert!(
        output.contains("CarbonTracker: ready. Starting work\n"),
        "{output}"
    );
    assert!(output.contains("work summary:\n"), "{output}");
    assert!(
        output.contains(&format!("Logs:       {}", folder.path().display())),
        "{output}"
    );
}

/// AC/RUN-15, AC/FE-48
#[test]
fn a_quiet_console_still_leaves_a_full_output_log() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let out = Shared::default();
    one_span(config(
        standard_sinks(console(&out, Verbosity::Quiet), Some(&folder)).unwrap(),
    ));

    assert_eq!(out.text(), "");
    assert!(
        std::fs::read_to_string(folder.output_log())
            .unwrap()
            .contains("work summary:")
    );
}

/// AC/RUN-15, AC/FE-48
#[test]
fn everything_shows_each_event_but_output_log_keeps_to_normal() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let out = Shared::default();
    one_span(config(
        standard_sinks(console(&out, Verbosity::Everything), Some(&folder)).unwrap(),
    ));

    let console = out.text();
    assert!(
        console.contains("  work started\n") && console.contains("  work settled: "),
        "{console}"
    );
    let output = std::fs::read_to_string(folder.output_log()).unwrap();
    assert!(!output.contains("work started"), "{output}");
}

/// AC/REC-1, AC/REC-6, AC/REC-7
#[test]
fn the_record_starts_with_the_run_the_probe_and_the_catalog() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    one_span(config(
        standard_sinks(console(&Shared::default(), Verbosity::Quiet), Some(&folder)).unwrap(),
    ));

    let lines = record(&folder);
    let kinds: Vec<&str> = lines
        .iter()
        .map(|line| line["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds[..3], ["run", "probe", "catalog"]);
    assert_eq!(kinds[kinds.len() - 2..], ["outcome", "session"]);

    let run = &lines[0];
    let command: Vec<String> = std::env::args().collect();
    assert_eq!(
        (&run["frontend"], &run["command"]),
        (&"test".into(), &serde_json::json!(command))
    );
    assert_eq!(run["epochs"], serde_json::Value::Null);
    let candidates = lines[1]["candidates"].as_array().unwrap();
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate["verdict"] == "serving"),
        "{candidates:?}"
    );
    assert!(
        !lines[1].to_string().contains("error"),
        "no error text in the probe line"
    );
}

/// Every item a sink gets, by kind.
#[derive(Clone, Default)]
struct Kinds(Arc<Mutex<Vec<&'static str>>>);

impl Sink for Kinds {
    fn probing(&mut self, _: &carbontracker_providers::ProbeProgress) -> io::Result<()> {
        self.0.lock().unwrap().push("probing");
        Ok(())
    }
    fn start(&mut self, _: &Started) -> io::Result<()> {
        self.0.lock().unwrap().push("started");
        Ok(())
    }
    fn event(&mut self, event: &TrackerEvent) -> io::Result<()> {
        let kind = match event {
            TrackerEvent::SessionEnded { .. } => "session",
            TrackerEvent::SpanSettled { .. } => "settled",
            _ => "event",
        };
        self.0.lock().unwrap().push(kind);
        Ok(())
    }
    fn outcome(&mut self, _: &Outcome) -> io::Result<()> {
        self.0.lock().unwrap().push("outcome");
        Ok(())
    }
    fn name(&self) -> &str {
        "kinds"
    }
}

/// AC/RUN-3, AC/RUN-20
#[test]
fn a_sink_of_its_own_gets_every_item_in_order() {
    let kinds = Kinds::default();
    one_span(config(vec![Box::new(kinds.clone())]));

    let seen = kinds.0.lock().unwrap().clone();
    let started = seen.iter().position(|kind| *kind == "started").unwrap();
    assert!(
        seen[..started].iter().all(|kind| *kind == "probing"),
        "{seen:?}"
    );
    assert!(seen.contains(&"settled"), "{seen:?}");
    assert_eq!(seen[seen.len() - 2..], ["outcome", "session"]);
}

/// Fails on the first event. Implements only what a sink must.
///
/// AC/RUN-13
struct Failing;

impl Sink for Failing {
    fn start(&mut self, _: &Started) -> io::Result<()> {
        Ok(())
    }
    fn event(&mut self, _: &TrackerEvent) -> io::Result<()> {
        Err(io::Error::other("disk full"))
    }
    fn name(&self) -> &str {
        "failing"
    }
}

/// AC/RUN-9, AC/RUN-14
#[test]
fn a_failing_sink_is_named_after_the_summary_and_the_others_go_on() {
    let out = Shared::default();
    let mut sinks = vec![Box::new(Failing) as Box<dyn Sink>];
    sinks.extend(standard_sinks(console(&out, Verbosity::Normal), None).unwrap());
    one_span(config(sinks));

    let console = out.text();
    let summary = console.find("work summary:").expect("the summary");
    let warning = console
        .find("the failing sink failed and stopped: disk full")
        .expect("the warning");
    assert!(summary < warning, "{console}");
}

/// AC/RUN-9
#[test]
fn a_run_that_cant_start_says_why_and_leaves_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let out = Shared::default();
    let mut config =
        config(standard_sinks(console(&out, Verbosity::Normal), Some(&folder)).unwrap());
    config.tracker.readout_interval = Duration::ZERO;

    assert!(start(config, |_, _| ()).is_err());
    assert_eq!(names(folder.path()), ["output.log"]);
    let output = std::fs::read_to_string(folder.output_log()).unwrap();
    assert!(output.contains("CarbonTracker: ERROR - "), "{output}");
    assert_eq!(output, out.text());
}

/// AC/RUN-2, AC/RUN-10, AC/RUN-17, AC/RUN-18
#[test]
fn the_predictor_prints_its_block_and_the_view_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let out = Shared::default();
    let predictor = |_: &_, forecaster: &_| Predictor::new(Some(10), Some(2), None, LatestForecast::new(forecaster));
    let run = start(
        config(standard_sinks(legacy(&out), Some(&folder)).unwrap()),
        predictor,
    )
    .unwrap();
    for _ in 0..3 {
        let epoch = run.start_span("epoch", None);
        std::thread::sleep(Duration::from_millis(30));
        run.stop_span(epoch, Work::default());
    }
    let (_, view) = run
        .finish(Outcome::Completed {
            epochs_done: Some(3),
        })
        .unwrap();

    let prediction = view
        .read(|predictor| predictor.prediction().cloned())
        .expect("a prediction");
    assert_eq!((prediction.measured, prediction.total), (2, 10.0));
    // The pinned grid's forecast reached it: its intensity, and no fallback.
    assert_eq!(prediction.intensity.get(), 100.0);
    assert!(
        prediction
            .warnings
            .iter()
            .all(|warning| !warning.to_string().contains("no forecast")),
        "{:?}",
        prediction.warnings
    );
    assert_eq!(view.read(|predictor| predictor.measured().count), 3);

    let console = out.text();
    let predicted = console
        .find("Predicted consumption for 10 epoch(s):")
        .expect("the prediction block");
    let actual = console
        .find("Actual consumption:\n")
        .expect("the closing block");
    assert!(predicted < actual, "{console}");
    assert!(
        console.ends_with("CarbonTracker: Finished monitoring.\n"),
        "{console}"
    );

    let lines = record(&folder);
    assert_eq!(
        lines[0]["epochs"],
        serde_json::json!({"planned": 10, "monitor": 10, "before_prediction": 2})
    );
    assert!(
        lines.iter().all(|line| line["kind"] != "prediction"),
        "the record holds what was measured"
    );
}

/// Every item a sink gets, as text, and whether each call came on the
/// writer thread.
#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<String>>>,
    started: Arc<Mutex<Option<Started>>>,
    off_the_writer: Arc<Mutex<Vec<String>>>,
}

impl Recorder {
    fn push(&self, text: String) {
        let thread = std::thread::current().name().unwrap_or("").to_owned();
        if thread != "carbontracker-writer" {
            self.off_the_writer.lock().unwrap().push(format!("{text} on {thread:?}"));
        }
        self.seen.lock().unwrap().push(text);
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

fn event_text(event: &TrackerEvent) -> String {
    match event {
        TrackerEvent::ReadoutLanded { .. } => "readout".to_owned(),
        TrackerEvent::SpanStarted { span, .. } => format!("span_started {}", span.get()),
        TrackerEvent::SpanStopped { span, .. } => format!("span_stopped {}", span.get()),
        TrackerEvent::SpanSettled { report } => format!("span_settled {}", report.span_id.get()),
        TrackerEvent::Marked { span, .. } => format!("marked {}", span.get()),
        TrackerEvent::CommandRefused { span, .. } => format!("refused {}", span.get()),
        TrackerEvent::SessionEnded { .. } => "session".to_owned(),
    }
}

impl Sink for Recorder {
    fn probing(&mut self, _: &carbontracker_providers::ProbeProgress) -> io::Result<()> {
        self.push("probing".to_owned());
        Ok(())
    }
    fn start(&mut self, started: &Started) -> io::Result<()> {
        *self.started.lock().unwrap() = Some(started.clone());
        self.push("started".to_owned());
        Ok(())
    }
    fn event(&mut self, event: &TrackerEvent) -> io::Result<()> {
        self.push(event_text(event));
        Ok(())
    }
    fn gap(&mut self) -> io::Result<()> {
        self.push("gap".to_owned());
        Ok(())
    }
    fn outcome(&mut self, outcome: &Outcome) -> io::Result<()> {
        self.push(format!("outcome {outcome:?}"));
        Ok(())
    }
    fn warning(&mut self, text: &str) -> io::Result<()> {
        self.push(format!("warning {text}"));
        Ok(())
    }
    fn error(&mut self, text: &str) -> io::Result<()> {
        self.push(format!("error {text}"));
        Ok(())
    }
    fn block(&mut self, block: &dyn carbontracker_run::Block) -> io::Result<()> {
        let lines = block.lines(&Layout::Summary { title: String::new() });
        self.push(format!("block {}", lines.join(" ")));
        Ok(())
    }
    fn finish(&mut self) -> io::Result<()> {
        self.push("finish".to_owned());
        Ok(())
    }
    fn name(&self) -> &str {
        "recorder"
    }
}

/// Two nested spans, one marked, then finish.
fn nested(config: RunConfig) {
    let run = start(config, |_, _| ()).unwrap();
    let outer = run.start_span("outer", None);
    let inner = run.start_span("inner", Some(outer));
    run.mark(inner, Work::of("samples", 8.0));
    std::thread::sleep(Duration::from_millis(30));
    run.stop_span(inner, Work::default());
    run.stop_span(outer, Work::default());
    run.finish(Outcome::Exited { code: 0 }).unwrap();
}

fn position(seen: &[String], text: &str) -> usize {
    seen.iter().position(|item| item == text).unwrap_or_else(|| panic!("no {text}: {seen:?}"))
}

/// AC/RUN-3, AC/RUN-4, AC/RUN-6, AC/RUN-8
#[test]
fn every_sink_gets_every_item_in_one_order() {
    let (first, second) = (Recorder::default(), Recorder::default());
    nested(config(vec![Box::new(first.clone()), Box::new(second.clone())]));

    let seen = first.seen();
    assert_eq!(seen, second.seen(), "both sinks get the same items in the same order");
    let started = position(&seen, "started");
    assert!(started > 0 && seen[..started].iter().all(|item| item == "probing"), "{seen:?}");
    for span in [1, 2] {
        let opened = position(&seen, &format!("span_started {span}"));
        let stopped = position(&seen, &format!("span_stopped {span}"));
        let settled = position(&seen, &format!("span_settled {span}"));
        assert!(started < opened && opened < stopped && stopped < settled, "{seen:?}");
    }
    let marked = position(&seen, "marked 2");
    assert!(position(&seen, "span_started 2") < marked && marked < position(&seen, "span_stopped 2"), "{seen:?}");
    assert_eq!(seen[seen.len() - 3..], ["outcome Exited { code: 0 }", "session", "finish"]);
}

/// AC/RUN-5
#[test]
fn started_comes_before_any_event_and_says_what_the_forecast_is() {
    let recorder = Recorder::default();
    nested(config(vec![Box::new(recorder.clone())]));

    let started = recorder.started.lock().unwrap().clone().expect("started");
    assert_eq!(started.header.frontend, "test");
    assert_eq!(started.header.format, carbontracker_run::record::FORMAT);
    assert!(started.probe.candidates.iter().any(|candidate| candidate.verdict == "serving"));
    let kinds: Vec<_> = started.catalog.sources.values().map(|source| source.kind()).collect();
    assert_eq!(kinds.len(), 2, "{:?}", started.catalog.sources);
    assert_eq!(started.catalog.pairs.len(), 1);
    let forecast = started.forecast.expect("the pinned grid has a forecast");
    assert_eq!(
        (forecast.location, forecast.fidelity, forecast.served_by),
        (GridLocation::Global, carbontracker::Fidelity::Stated, "static")
    );
    let seen = recorder.seen();
    let first_event = seen.iter().position(|item| item.starts_with("span_") || item == "readout").unwrap();
    assert!(position(&seen, "started") < first_event, "{seen:?}");
}

/// AC/RUN-12
#[test]
fn every_sink_call_is_on_the_writer_thread() {
    let recorder = Recorder::default();
    nested(config(vec![Box::new(recorder.clone())]));
    assert!(recorder.seen().len() > 5);
    assert_eq!(*recorder.off_the_writer.lock().unwrap(), Vec::<String>::new());
}

/// Holds `release` shut until the test opens it.
#[derive(Clone, Default)]
struct Gate(Arc<std::sync::atomic::AtomicBool>);

impl Gate {
    fn wait(&self) {
        while !self.0.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn open(&self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// More marks than the engine's subscriber queue holds (1024).
const BURST: usize = 3000;

/// A burst of marks on one span, sent while `gate` is shut, then finish.
fn burst(run: carbontracker_run::RunHandle<impl carbontracker_run::Projection>, gate: &Gate) {
    let span = run.start_span("burst", None);
    for _ in 0..BURST {
        run.mark(span, Work::of("steps", 1.0));
    }
    // Time for the engine to announce them all.
    std::thread::sleep(Duration::from_millis(300));
    gate.open();
    run.stop_span(span, Work::default());
    run.finish(Outcome::Exited { code: 0 }).unwrap();
}

/// A sink that blocks on its first mark until the gate opens.
struct Slow {
    gate: Gate,
    recorder: Recorder,
}

impl Sink for Slow {
    fn start(&mut self, started: &Started) -> io::Result<()> {
        self.recorder.start(started)
    }
    fn event(&mut self, event: &TrackerEvent) -> io::Result<()> {
        if matches!(event, TrackerEvent::Marked { .. }) {
            self.gate.wait();
        }
        self.recorder.event(event)
    }
    fn gap(&mut self) -> io::Result<()> {
        self.recorder.gap()
    }
    fn name(&self) -> &str {
        "slow"
    }
}

/// AC/RUN-11
#[test]
fn a_sink_that_blocks_loses_no_events() {
    let (gate, recorder) = (Gate::default(), Recorder::default());
    let slow = Slow { gate: gate.clone(), recorder: recorder.clone() };
    let run = start(config(vec![Box::new(slow)]), |_, _| ()).unwrap();
    burst(run, &gate);

    let seen = recorder.seen();
    assert!(!seen.contains(&"gap".to_owned()), "the forwarder kept up");
    assert_eq!(seen.iter().filter(|item| item.starts_with("marked")).count(), BURST);
    assert_eq!(seen.last().map(String::as_str), Some("session"));
}

/// A projection that blocks on its first mark until the gate opens: a
/// planted fault, since a projection must never wait.
struct Stuck(Gate);

impl carbontracker_run::Projection for Stuck {
    fn project(&mut self, item: &carbontracker_run::Item) -> Vec<Box<dyn carbontracker_run::Block>> {
        if let carbontracker_run::Item::Event(TrackerEvent::Marked { .. }) = item {
            self.0.wait();
        }
        Vec::new()
    }
}

/// AC/RUN-7
#[test]
fn a_projection_that_falls_behind_gets_a_gap_and_no_event_after_it() {
    let (gate, recorder) = (Gate::default(), Recorder::default());
    let stuck = gate.clone();
    let run = start(config(vec![Box::new(recorder.clone())]), move |_, _| Stuck(stuck)).unwrap();
    burst(run, &gate);

    let seen = recorder.seen();
    let gap = position(&seen, "gap");
    assert!(seen.iter().filter(|item| item.starts_with("marked")).count() < BURST);
    assert_eq!(seen[gap + 1..], ["outcome Exited { code: 0 }", "finish"], "no event follows the gap");
}

/// Fails on its first event, and counts what it is asked after that.
#[derive(Clone, Default)]
struct Counting {
    failed: Arc<Mutex<bool>>,
    calls_after: Arc<Mutex<usize>>,
    finishes: Arc<Mutex<usize>>,
}

impl Sink for Counting {
    fn start(&mut self, _: &Started) -> io::Result<()> {
        Ok(())
    }
    fn event(&mut self, _: &TrackerEvent) -> io::Result<()> {
        let mut failed = self.failed.lock().unwrap();
        if *failed {
            *self.calls_after.lock().unwrap() += 1;
            return Ok(());
        }
        *failed = true;
        Err(io::Error::other("disk full"))
    }
    fn outcome(&mut self, _: &Outcome) -> io::Result<()> {
        if *self.failed.lock().unwrap() {
            *self.calls_after.lock().unwrap() += 1;
        }
        Ok(())
    }
    fn warning(&mut self, _: &str) -> io::Result<()> {
        *self.calls_after.lock().unwrap() += 1;
        Ok(())
    }
    fn finish(&mut self) -> io::Result<()> {
        *self.finishes.lock().unwrap() += 1;
        Ok(())
    }
    fn name(&self) -> &str {
        "counting"
    }
}

/// AC/RUN-14
#[test]
fn a_sink_that_errs_is_finished_once_and_dropped_while_the_others_go_on() {
    let (failing, other) = (Counting::default(), Recorder::default());
    nested(config(vec![Box::new(failing.clone()), Box::new(other.clone())]));

    assert_eq!(*failing.calls_after.lock().unwrap(), 0, "nothing after its error");
    assert_eq!(*failing.finishes.lock().unwrap(), 1);
    let seen = other.seen();
    assert_eq!(
        seen[seen.len() - 4..],
        [
            "outcome Exited { code: 0 }".to_owned(),
            "session".to_owned(),
            "warning the counting sink failed and stopped: disk full".to_owned(),
            "finish".to_owned()
        ]
    );
}

/// Counts the items it sees, sums the settled spans' energy, and notes the
/// threads it ran on.
#[derive(Default)]
struct Tally {
    items: usize,
    settled_kwh: f64,
    threads: std::collections::BTreeSet<String>,
}

impl carbontracker_run::Projection for Tally {
    fn project(&mut self, item: &carbontracker_run::Item) -> Vec<Box<dyn carbontracker_run::Block>> {
        self.items += 1;
        self.threads.insert(std::thread::current().name().unwrap_or("").to_owned());
        if let carbontracker_run::Item::Event(TrackerEvent::SpanSettled { report }) = item {
            self.settled_kwh += report.total.energy.to_kilowatt_hours().get();
        }
        Vec::new()
    }
}

/// AC/RUN-16, AC/RUN-17, AC/RUN-18
#[test]
fn a_projection_keeps_what_the_engine_forgets_and_the_view_reads_it() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let recorder = Recorder::default();
    let mut sinks = standard_sinks(console(&Shared::default(), Verbosity::Quiet), Some(&folder)).unwrap();
    sinks.push(Box::new(recorder.clone()));
    let run = start(config(sinks), |_, _| Tally::default()).unwrap();
    for _ in 0..3 {
        let span = run.start_span("step", None);
        std::thread::sleep(Duration::from_millis(30));
        run.stop_span(span, Work::default());
    }
    // Readable mid-run, and never ahead of the run.
    let midway = run.view().read(|tally| tally.items);
    let (_, view) = run.finish(Outcome::Exited { code: 0 }).unwrap();

    let (items, settled_kwh, threads) = view.read(|tally| (tally.items, tally.settled_kwh, tally.threads.clone()));
    assert!(midway <= items);
    // Every item from the start on; probe steps come before it exists.
    let seen = recorder.seen();
    let projected = seen.iter().filter(|item| *item != "probing" && *item != "finish").count();
    assert_eq!(items, projected, "{seen:?}");
    assert_eq!(threads.into_iter().collect::<Vec<_>>(), ["carbontracker-forwarder"]);
    let recorded: f64 = record(&folder)
        .iter()
        .filter(|line| line["kind"] == "span_settled")
        .map(|line| line["energy_kwh"].as_f64().unwrap())
        .sum();
    assert!(settled_kwh > 0.0 && (settled_kwh - recorded).abs() < 1e-15, "{settled_kwh} vs {recorded}");
}

/// Console text naming the item it came from.
#[derive(Debug)]
struct Tag(String);

impl carbontracker_run::Block for Tag {
    fn lines(&self, _: &Layout) -> Vec<String> {
        vec![self.0.clone()]
    }
}

/// Returns one block per event, naming it.
struct Tagger;

impl carbontracker_run::Projection for Tagger {
    fn project(&mut self, item: &carbontracker_run::Item) -> Vec<Box<dyn carbontracker_run::Block>> {
        match item {
            carbontracker_run::Item::Event(event) => vec![Box::new(Tag(event_text(event)))],
            _ => Vec::new(),
        }
    }
}

/// AC/RUN-10, AC/RUN-19
#[test]
fn each_block_comes_just_before_the_item_it_came_from() {
    let recorder = Recorder::default();
    let run = start(config(vec![Box::new(recorder.clone())]), |_, _| Tagger).unwrap();
    let span = run.start_span("work", None);
    run.mark(span, Work::of("steps", 1.0));
    run.stop_span(span, Work::default());
    run.finish(Outcome::Exited { code: 0 }).unwrap();

    let seen = recorder.seen();
    let events: Vec<usize> = (0..seen.len())
        .filter(|&at| seen[at].starts_with("span_") || seen[at].starts_with("marked") || seen[at] == "readout" || seen[at] == "session")
        .collect();
    assert!(events.len() >= 4, "{seen:?}");
    for at in events {
        assert_eq!(seen[at - 1], format!("block {}", seen[at]), "{seen:?}");
    }
}

/// AC/RUN-21, AC/REC-15
#[test]
fn every_outcome_reaches_the_record() {
    for (outcome, expected) in [
        (Outcome::Completed { epochs_done: Some(3) }, serde_json::json!({"ended": "completed", "epochs_done": 3})),
        (Outcome::Monitored { epochs_done: 2 }, serde_json::json!({"ended": "monitored", "epochs_done": 2})),
        (Outcome::Interrupted { epochs_done: None }, serde_json::json!({"ended": "interrupted"})),
        (Outcome::Failed { epochs_done: Some(1) }, serde_json::json!({"ended": "failed", "epochs_done": 1})),
        (Outcome::Exited { code: 4 }, serde_json::json!({"ended": "exited", "code": 4})),
        (Outcome::Signalled { signal: 9 }, serde_json::json!({"ended": "signalled", "signal": 9})),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let folder = Folder::claim(dir.path(), None).unwrap();
        let run = start(
            config(standard_sinks(console(&Shared::default(), Verbosity::Quiet), Some(&folder)).unwrap()),
            |_, _| (),
        )
        .unwrap();
        let (session, view) = run.finish(outcome).unwrap();
        view.read(|()| ());
        let lines = record(&folder);
        let mut line = lines.iter().find(|line| line["kind"] == "outcome").expect("an outcome line").clone();
        let object = line.as_object_mut().unwrap();
        object.remove("at");
        object.remove("kind");
        assert_eq!(line, expected, "{outcome:?}");
        let last = lines.last().unwrap();
        assert_eq!(last["kind"], "session");
        assert_eq!(last["duration_s"].as_f64().unwrap(), session.duration.as_secs_f64());
    }
}

/// AC/REC-18, AC/REC-19
#[test]
fn a_source_that_is_never_read_again_is_in_one_readout_line() {
    let dir = tempfile::tempdir().unwrap();
    let folder = Folder::claim(dir.path(), None).unwrap();
    let run = start(
        config(standard_sinks(console(&Shared::default(), Verbosity::Quiet), Some(&folder)).unwrap()),
        |_, _| (),
    )
    .unwrap();
    // Pinned sources are never read again; many cuts over many intervals.
    for _ in 0..5 {
        let span = run.start_span("step", None);
        std::thread::sleep(Duration::from_millis(30));
        run.stop_span(span, Work::default());
    }
    run.finish(Outcome::Exited { code: 0 }).unwrap();

    let lines = record(&folder);
    let catalog = lines.iter().find(|line| line["kind"] == "catalog").unwrap();
    let sources: Vec<&str> = catalog["sources"].as_array().unwrap().iter().map(|source| source["id"].as_str().unwrap()).collect();
    assert_eq!(sources.len(), 2);
    let readouts: Vec<Vec<String>> = lines
        .iter()
        .filter(|line| line["kind"] == "readout")
        .map(|line| {
            let mut ids = Vec::new();
            for observation in ["samples", "unchanged", "skipped", "failed"] {
                match &line[observation] {
                    serde_json::Value::Object(map) => ids.extend(map.keys().cloned()),
                    serde_json::Value::Array(list) => ids.extend(list.iter().map(|id| id.as_str().unwrap().to_owned())),
                    _ => {}
                }
            }
            ids
        })
        .collect();
    for ids in &readouts {
        let provider = |id: &String| id.split('.').next().unwrap().to_owned();
        assert!(ids.iter().all(|id| provider(id) == provider(&ids[0])), "one provider per readout line: {ids:?}");
    }
    for source in sources {
        let lines = readouts.iter().filter(|ids| ids.iter().any(|id| id == source)).count();
        assert_eq!(lines, 1, "{source} in {readouts:?}");
    }
}

/// Every Rust file under `dir`, with its text.
fn sources(dir: &Path) -> Vec<(std::path::PathBuf, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text = std::fs::read_to_string(&path).unwrap();
            found.push((path, text));
        }
    }
    found
}

/// AC/RUN-1
#[test]
fn no_frontend_reaches_the_engine_but_through_a_run() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    for frontend in ["cli", "python"] {
        for (path, text) in sources(&crates.join(frontend).join("src")) {
            for name in ["carbontracker::start", "TrackerHandle", "TrackerQuery", ".subscribe(", "subscribe_with_capacity"] {
                assert!(!text.contains(name), "{} names {name}", path.display());
            }
        }
    }
}

/// AC/PRF-3
#[test]
fn started_gives_each_source_its_cooldown_and_the_fetch_timeout() {
    let run = start(config(Vec::new()), |_, _| ()).expect("started");
    let started = run.started();

    // Stated draws and intensities are read once.
    assert!(!started.catalog.sources.is_empty());
    for source in started.catalog.sources.keys() {
        assert_eq!(started.cooldowns.get(source), Some(&carbontracker::Cooldown::Never), "{source}");
    }
    assert_eq!(started.fetch_timeout, tracker().fetch.timeout);
    run.finish(Outcome::Completed { epochs_done: None }).expect("finished");
}

/// The console's text for a run probed with `grid` and a stated draw only.
fn console_for(grid: GridConfig) -> String {
    let out = Shared::default();
    let mut config = config(standard_sinks(console(&out, Verbosity::Normal), None).unwrap());
    config.probe.grid = grid;
    config.probe.pinned.intensity = None;
    one_span(config);
    out.text()
}

/// AC/RUN-23
#[test]
fn a_world_average_nobody_asked_for_says_why() {
    let coordinates = GridLocation::Coordinates { latitude: 55.68, longitude: 12.57 };
    let text = console_for(GridConfig { location: Some(coordinates), ..GridConfig::default() });
    let warning = text.lines().find(|line| line.contains("WARNING - the world-average grid")).unwrap_or_else(|| panic!("{text}"));
    assert!(warning.contains("(55.68, 12.57)") && warning.contains("no key"), "{warning}");
    assert!(warning.contains("country table"), "{warning}");
    assert!(text.find("ready.").unwrap() < text.find(warning).unwrap(), "{text}");

    let asked = console_for(GridConfig { location: Some(GridLocation::Global), ..GridConfig::default() });
    assert!(!asked.contains("WARNING - the world-average grid"), "{asked}");
}
