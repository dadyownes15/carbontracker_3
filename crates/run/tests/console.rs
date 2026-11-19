//! The [ConsoleSink] through its public seam: scripted items in, the lines it
//! prints out, at each verbosity.

mod common;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use carbontracker::{
    CatalogReport, Failure, Fidelity, GridLocation, Joules, Observation, PowerDomain, PowerScope, ProviderError,
    ProviderId, ReadoutId, ReportWarning, Sample, SampleKind, SessionReport, Skip, SourceDescription, SourceId,
    SourceSelection, SpanId, Timestamp, TrackerError, TrackerEvent, Usage, Work,
};
use carbontracker_run::{ConsoleSink, Item, Layout, Started, Verbosity, deliver, record};
use common::Shared;

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

/// A start with one GPU, served by NVML, on the world-average grid nobody
/// asked for.
fn started() -> Item {
    started_on(None)
}

/// A start with one GPU, served by NVML, and the world-average grid, with the
/// grid's `location` as recorded.
fn started_on(location: Option<record::Location>) -> Item {
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
    catalog.sources.insert(
        SourceId::new(ProviderId(2), 0),
        SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Assumed },
    );
    Item::Started(Arc::new(Started {
        header: record::Header {
            format: record::FORMAT,
            carbontracker: "3.0.0".to_owned(),
            frontend: "test".to_owned(),
            command: Vec::new(),
            config: record::Config { interval_s: 1.0, pue: 1.0, components: None, location },
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

/// AC/OUT-3, AC/OUT-7, AC/OUT-11, AC/OUT-12, AC/OUT-15, AC/OUT-20, AC/OUT-30
#[test]
fn every_failed_read_is_a_warning_at_every_verbosity_and_nothing_else_is() {
    let mut printed = Vec::new();
    for verbosity in VERBOSITIES {
        let (mut sink, out) = console(verbosity);
        deliver(&mut sink, &started()).unwrap();
        for item in nvml_readouts() {
            deliver(&mut sink, &item).unwrap();
        }
        let text = out.text();
        let warnings: Vec<String> = warnings(&text).into_iter().map(str::to_owned).collect();
        assert_eq!(warnings.len(), 3, "{verbosity:?}: {text}");
        for warning in &warnings {
            assert!(warning.contains("nvml") && warning.contains("unavailable"), "{verbosity:?}: {warning}");
        }
        printed.push(warnings);
    }
    // The same text at every verbosity.
    assert_eq!(printed[0], printed[1]);
    assert_eq!(printed[0], printed[2]);
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

/// A session that ended with `warnings`.
fn ended(warnings: Vec<ReportWarning>) -> Item {
    let none = Usage { energy: Joules::new(0.0).unwrap(), emissions: None };
    Item::Event(TrackerEvent::SessionEnded {
        report: Arc::new(SessionReport {
            per_source: HashMap::new(),
            sources: HashMap::new(),
            total: none,
            claimed: none,
            unclaimed: none,
            intensities: HashMap::new(),
            duration: Duration::from_secs(1),
            samples: HashMap::new(),
            warnings,
        }),
    })
}

/// AC/OUT-21, AC/OUT-32
#[test]
fn an_estimate_leaves_out_the_world_average_only_when_it_was_asked_for() {
    let estimated = || {
        ended(vec![ReportWarning::Estimated {
            sources: vec!["Intel Xeon (cpu) modelled".to_owned(), "assumed world-average grid".to_owned()],
        }])
    };
    let printed = |location| {
        let (mut sink, out) = console(Verbosity::Quiet);
        deliver(&mut sink, &started_on(location)).unwrap();
        deliver(&mut sink, &estimated()).unwrap();
        out.text()
    };
    let prefix = "CarbonTracker: WARNING - part of this total is an estimate, not measured on this machine: ";
    assert_eq!(printed(None), format!("{prefix}Intel Xeon (cpu) modelled, assumed world-average grid\n"));
    let global = Some(record::Location::Stated(record::Grid::Global));
    assert_eq!(printed(global), format!("{prefix}Intel Xeon (cpu) modelled\n"));
}
