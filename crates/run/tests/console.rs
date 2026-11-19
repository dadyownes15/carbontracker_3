//! The [ConsoleSink] through its public seam: scripted items in, the lines it
//! prints out, at each verbosity.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbontracker::{
    CatalogReport, Failure, Fidelity, Observation, PowerDomain, PowerScope, ProviderError, ProviderId, ReadoutId,
    Sample, SampleKind, Skip, SourceDescription, SourceId, SourceSelection, SpanId, Timestamp, TrackerError,
    TrackerEvent, Work,
};
use carbontracker_run::{ConsoleSink, Item, Layout, Started, Verbosity, deliver, record};

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

const VERBOSITIES: [Verbosity; 3] = [Verbosity::Quiet, Verbosity::Normal, Verbosity::Everything];

fn nvml() -> SourceId {
    SourceId::new(ProviderId(1), 0)
}

fn t(seconds: i64) -> Timestamp {
    chrono::DateTime::from_timestamp(1_761_659_000 + seconds, 0).unwrap()
}

/// A console at `verbosity`, and what it prints.
fn console(verbosity: Verbosity) -> (ConsoleSink, Shared) {
    let out = Shared::default();
    let sink = ConsoleSink::new(out.clone(), verbosity, Layout::Summary { title: "train".to_owned() }, None)
        .with_clock(|| "14:03:12".to_owned());
    (sink, out)
}

/// A start with one GPU, served by NVML.
fn started() -> Item {
    let mut catalog = CatalogReport::default();
    catalog.sources.insert(
        nvml(),
        SourceDescription::Power {
            device_name: "NVIDIA A100".to_owned(),
            domain: PowerDomain::Gpu,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Measured,
        },
    );
    Item::Started(Arc::new(Started {
        header: record::Header {
            format: record::FORMAT,
            carbontracker: "3.0.0".to_owned(),
            frontend: "test".to_owned(),
            command: Vec::new(),
            config: record::Config { interval_s: 1.0, pue: 1.0, components: None, location: None },
            epochs: None,
        },
        probe: record::Probe { candidates: Vec::new() },
        catalog,
        served_by: HashMap::from([(nvml(), "nvml")]),
        cooldowns: HashMap::new(),
        fetch_timeout: Duration::from_secs(10),
        forecast: None,
    }))
}

fn readout(id: usize, observation: Observation) -> Item {
    Item::Event(TrackerEvent::ReadoutLanded {
        readout: ReadoutId(id),
        outcomes: Arc::new(HashMap::from([(nvml(), observation)])),
    })
}

/// NVML's readouts: samples, three failures, then an unchanged and a skipped
/// read.
fn nvml_readouts() -> Vec<Item> {
    let unavailable = || Observation::Failed(Failure::Provider(ProviderError::Unavailable("driver gone".to_owned())));
    vec![
        readout(1, Observation::Samples(vec![Sample::new(t(1), 210.0)])),
        readout(2, unavailable()),
        readout(3, unavailable()),
        readout(4, unavailable()),
        readout(5, Observation::Unchanged),
        readout(6, Observation::Skipped(Skip::Cooldown)),
    ]
}

/// The warning lines in `text`.
fn warnings(text: &str) -> Vec<&str> {
    text.lines().filter(|line| line.contains("WARNING - ")).collect()
}

/// AC/OUT-3, AC/OUT-7, AC/OUT-11, AC/OUT-15, AC/OUT-20, AC/OUT-30
#[test]
fn every_failed_read_is_a_warning_at_every_verbosity_and_nothing_else_is() {
    for verbosity in VERBOSITIES {
        let (mut sink, out) = console(verbosity);
        deliver(&mut sink, &started()).unwrap();
        for item in nvml_readouts() {
            deliver(&mut sink, &item).unwrap();
        }
        let text = out.text();
        let warnings = warnings(&text);
        assert_eq!(warnings.len(), 3, "{verbosity:?}: {text}");
        for warning in warnings {
            assert!(warning.contains("nvml") && warning.contains("unavailable"), "{verbosity:?}: {warning}");
        }
    }
}

/// AC/OUT-13
#[test]
fn each_failed_read_is_written_before_the_next_readout() {
    let (mut sink, out) = console(Verbosity::Quiet);
    deliver(&mut sink, &started()).unwrap();
    let mut seen = Vec::new();
    for item in nvml_readouts() {
        deliver(&mut sink, &item).unwrap();
        seen.push(warnings(&out.text()).len());
    }
    assert_eq!(seen, [0, 1, 2, 3, 3, 3]);
}

/// AC/OUT-25
#[test]
fn a_refused_command_is_a_warning_naming_the_span() {
    let inner = SpanId::new(2);
    for verbosity in VERBOSITIES {
        let (mut sink, out) = console(verbosity);
        deliver(&mut sink, &started()).unwrap();
        let events = [
            TrackerEvent::SpanStarted {
                span: inner,
                name: "inner".to_owned(),
                parent: None,
                selection: SourceSelection::All,
                at: t(1),
            },
            TrackerEvent::SpanStopped { span: inner, work: Work::default(), at: t(2) },
            TrackerEvent::CommandRefused { span: inner, error: TrackerError::AlreadyClosed(inner) },
        ];
        for event in events {
            deliver(&mut sink, &Item::Event(event)).unwrap();
        }
        let text = out.text();
        let [warning] = warnings(&text)[..] else { panic!("{verbosity:?}: {text}") };
        assert!(warning.contains("inner") && warning.contains("refused"), "{verbosity:?}: {warning}");
    }
}

/// AC/OUT-2, AC/OUT-11, AC/OUT-23
#[test]
fn errors_and_lost_events_print_at_quiet() {
    let (mut sink, out) = console(Verbosity::Quiet);
    deliver(&mut sink, &started()).unwrap();
    deliver(&mut sink, &Item::Gap).unwrap();
    deliver(&mut sink, &Item::Error("the engine stopped".to_owned())).unwrap();
    assert_eq!(
        out.text(),
        "CarbonTracker: WARNING - events were lost here: the record is incomplete\n\
         CarbonTracker: ERROR - the engine stopped\n"
    );
}
