//! The record format, through its public seam: what [JsonlSink] writes.
//!
//! The golden files in `fixtures/records/v<N>/` are the format's contract:
//! every release must read every one of them. `python_monitored.jsonl` is
//! also what this release writes for a fixed run; when that changes on
//! purpose, rewrite it with `CARBONTRACKER_BLESS=1 cargo test` and keep the
//! old file under a new name, so it is still read.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbontracker::{
    Failure, Skip, CatalogReport, Fidelity, GramsCo2e, GridLocation, SampleKind, Joules, Observation, PowerDomain,
    PowerScope, ProviderError, ProviderId, ReadoutId, ReportWarning, Sample, SessionReport,
    SourceDescription, SourceId, SourceSelection, SpanId, SpanReport, Timestamp, TrackerError,
    TrackerEvent, Usage, Work,
};
use carbontracker_run::record;
use carbontracker_run::{JsonlSink, Outcome, Sink, Started};
use chrono::{DateTime, TimeDelta};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/records");

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

fn t(seconds: i64) -> Timestamp {
    DateTime::from_timestamp(1_761_379_923, 0).unwrap() + TimeDelta::seconds(seconds)
}

fn cpu() -> SourceId {
    SourceId::new(ProviderId(3), 0)
}

fn grid() -> SourceId {
    SourceId::new(ProviderId(7), 0)
}

/// Paired with [north].
fn gpu() -> SourceId {
    SourceId::new(ProviderId(4), 0)
}

fn north() -> SourceId {
    SourceId::new(ProviderId(8), 0)
}

/// Paired with no intensity source.
fn ram() -> SourceId {
    SourceId::new(ProviderId(5), 0)
}

fn unbilled(kwh: f64) -> Usage {
    Usage {
        energy: Joules::new(kwh * 3_600_000.0).unwrap(),
        emissions: None,
    }
}

fn power(device: &str, domain: PowerDomain) -> SourceDescription {
    SourceDescription::Power {
        device_name: device.to_owned(),
        domain,
        scope: PowerScope::DeviceTotal,
        sample_kind: SampleKind::Instant,
        fidelity: Fidelity::Measured,
    }
}

fn usage(kwh: f64, grams: f64) -> Usage {
    Usage {
        energy: Joules::new(kwh * 3_600_000.0).unwrap(),
        emissions: Some(GramsCo2e::new(grams).unwrap()),
    }
}

fn catalog() -> CatalogReport {
    let mut catalog = CatalogReport::default();
    catalog.sources.insert(cpu(), power("Apple M2 Pro", PowerDomain::Cpu));
    catalog.sources.insert(gpu(), power("NVIDIA A100", PowerDomain::Gpu));
    catalog.sources.insert(ram(), power("Apple M2 Pro", PowerDomain::Ram));
    let dk2 = SourceDescription::Intensity {
        location: GridLocation::Zone {
            zone_id: "DK-DK2".to_owned(),
        },
        fidelity: Fidelity::Measured,
    };
    catalog.sources.insert(grid(), dk2);
    let se3 = SourceDescription::Intensity {
        location: GridLocation::Zone {
            zone_id: "SE-SE3".to_owned(),
        },
        fidelity: Fidelity::Measured,
    };
    catalog.sources.insert(north(), se3);
    catalog.pairs = HashMap::from([(cpu(), grid()), (gpu(), north())]);
    catalog
}

fn epoch(id: usize, kwh: f64, grams: f64) -> TrackerEvent {
    TrackerEvent::SpanSettled {
        report: Arc::new(SpanReport {
            span_id: SpanId::new(id),
            span_name: "epoch".to_owned(),
            per_source: HashMap::from([(cpu(), usage(kwh, grams))]),
            sources: HashMap::new(),
            total: usage(kwh, grams),
            exclusive: usage(kwh, grams),
            intensities: HashMap::new(),
            duration: Duration::from_secs(60),
            samples: HashMap::from([(cpu(), 60)]),
            warnings: Vec::new(),
            selection: SourceSelection::All,
        }),
    }
}

/// Two monitored epochs of ten, an eval span inside the first, three
/// readouts, a refused stop, a gap; then the outcome and the session. Of
/// three power sources, the CPU and GPU are each paired with a grid of their
/// own, and the RAM with none.
fn python_monitored() -> String {
    let run = record::Header {
        format: record::FORMAT,
        carbontracker: "3.0.0".to_owned(),
        frontend: "python".to_owned(),
        command: vec!["train.py".to_owned(), "--lr".to_owned(), "0.1".to_owned()],
        config: record::Config {
            interval_s: 1.0,
            pue: 1.58,
            components: None,
            location: Some(record::Location::Estimated {
                country: "DK".to_owned(),
                coordinates: Some([55.68, 12.57]),
                by: "ip-geolocation".to_owned(),
            }),
        },
        epochs: Some(record::Epochs {
            planned: 10,
            monitor: 2,
            before_prediction: 1,
        }),
    };
    let out = Shared::default();
    let mut clock = 0;
    let mut sink = JsonlSink::new(out.clone()).with_clock(move || {
        clock += 1;
        t(clock * 10)
    });
    let probe = record::Probe {
        candidates: vec![
            record::Candidate {
                name: "macmon".to_owned(),
                verdict: "serving".to_owned(),
                series: vec!["cpu".to_owned()],
            },
            record::Candidate {
                name: "powermetrics".to_owned(),
                verdict: "covered".to_owned(),
                series: Vec::new(),
            },
            record::Candidate {
                name: "electricity-maps".to_owned(),
                verdict: "serving".to_owned(),
                series: vec!["DK-DK2".to_owned()],
            },
        ],
    };
    let (first, eval, second) = (SpanId::new(1), SpanId::new(2), SpanId::new(3));
    sink.start(&Started {
        header: run,
        probe,
        catalog: catalog(),
        served_by: HashMap::new(),
        cooldowns: HashMap::new(),
        fetch_timeout: Duration::from_secs(10),
        forecast: None,
    })
    .unwrap();
    for event in [
        TrackerEvent::SpanStarted {
            span: first,
            name: "epoch".to_owned(),
            parent: None,
            selection: SourceSelection::All,
            at: t(1),
        },
        TrackerEvent::ReadoutLanded {
            readout: ReadoutId(1),
            outcomes: Arc::new(HashMap::from([
                (cpu(), Observation::Samples(vec![Sample::new(t(2), 3.21)])),
                (grid(), Observation::Failed(Failure::Provider(ProviderError::Unavailable("network down".to_owned())))),
            ])),
        },
        TrackerEvent::ReadoutLanded {
            readout: ReadoutId(2),
            outcomes: Arc::new(HashMap::from([
                (cpu(), Observation::Unchanged),
                (grid(), Observation::Skipped(Skip::Cooldown)),
            ])),
        },
        TrackerEvent::ReadoutLanded {
            readout: ReadoutId(3),
            outcomes: Arc::new(HashMap::from([
                (cpu(), Observation::Failed(Failure::Panicked("the driver exploded".to_owned()))),
                (grid(), Observation::Failed(Failure::TimedOut)),
            ])),
        },
        TrackerEvent::SpanStarted {
            span: eval,
            name: "eval".to_owned(),
            parent: Some(first),
            selection: SourceSelection::Domains(vec![PowerDomain::Cpu]),
            at: t(30),
        },
        TrackerEvent::Marked {
            span: eval,
            work: Work::of("samples", 64.0),
            at: t(31),
        },
        TrackerEvent::SpanStopped {
            span: eval,
            work: Work::of("batches", 2.0),
            at: t(40),
        },
        TrackerEvent::SpanStopped {
            span: first,
            work: Work::default(),
            at: t(61),
        },
        epoch(1, 0.0101, 1.52),
        TrackerEvent::CommandRefused {
            span: eval,
            error: TrackerError::AlreadyClosed(eval),
        },
        TrackerEvent::SpanStarted {
            span: second,
            name: "epoch".to_owned(),
            parent: None,
            selection: SourceSelection::All,
            at: t(62),
        },
        TrackerEvent::SpanStopped {
            span: second,
            work: Work::default(),
            at: t(122),
        },
        epoch(3, 0.0103, 1.55),
    ] {
        sink.event(&event).unwrap();
    }
    sink.gap().unwrap();
    sink.outcome(&Outcome::Monitored { epochs_done: 2 })
        .unwrap();
    let mut session = SessionReport {
        per_source: HashMap::from([(cpu(), usage(0.0204, 3.04)), (gpu(), usage(0.004, 0.16)), (ram(), unbilled(0.0005))]),
        sources: catalog().sources,
        total: usage(0.0249, 3.20),
        claimed: usage(0.0247, 3.17),
        unclaimed: usage(0.0002, 0.03),
        intensities: [grid(), north()].into_iter().map(|id| (id, catalog().sources[&id].clone())).collect(),
        duration: Duration::from_secs(123),
        samples: HashMap::from([(cpu(), 123), (gpu(), 120), (ram(), 61)]),
        warnings: Vec::new(),
    };
    session.warnings.push(ReportWarning::NoIntensity { sources: vec!["Apple M2 Pro (ram)".to_owned()] });
    sink.event(&TrackerEvent::SessionEnded {
        report: Arc::new(session),
    })
    .unwrap();
    sink.finish().unwrap();
    String::from_utf8(out.0.lock().unwrap().clone()).unwrap()
}

/// AC/REC-4, AC/REC-6, AC/REC-7, AC/REC-8, AC/REC-9, AC/REC-10, AC/REC-11,
/// AC/REC-12, AC/REC-13, AC/REC-14, AC/REC-15, AC/REC-16, AC/REC-17,
/// AC/REC-22
#[test]
// AC/TST-26: rewritten only with CARBONTRACKER_BLESS set.
fn a_record_is_what_this_release_writes_for_a_fixed_run() {
    let path = Path::new(FIXTURES).join("v1/python_monitored.jsonl");
    let written = python_monitored();
    if std::env::var_os("CARBONTRACKER_BLESS").is_some() {
        std::fs::write(&path, &written).unwrap();
    }
    let golden = std::fs::read_to_string(&path).expect("the fixture exists");
    assert_eq!(
        written, golden,
        "the record changed: see this file's header"
    );
}

/// AC/REC-1
#[test]
fn a_record_names_its_writer_first_then_what_it_measures() {
    let written = python_monitored();
    let kinds: Vec<String> = written
        .lines()
        .map(|line| {
            serde_json::from_str::<serde_json::Value>(line).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    assert_eq!(kinds[..3], ["run", "probe", "catalog"]);
    assert_eq!(kinds.last().map(String::as_str), Some("session"));
    assert!(
        !kinds.iter().any(|kind| kind == "prediction"),
        "a record holds only what was measured"
    );
}

fn lines(text: &str) -> Vec<serde_json::Value> {
    text.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// AC/REC-20, AC/REC-22
#[test]
fn absent_emissions_are_null_and_each_intensity_is_the_mean_of_its_own_pairs() {
    let lines = lines(&python_monitored());
    let session = lines.last().unwrap();
    let per_source = &session["per_source"];
    assert_eq!(per_source["5.0"], serde_json::json!({"energy_kwh": 0.0005, "emissions_g": null}));
    assert_eq!(per_source["4.0"]["emissions_g"], serde_json::json!(0.16));
    assert_eq!(session["samples"], serde_json::json!({"3.0": 123, "4.0": 120, "5.0": 61}));
    let intensities = session["intensities"].as_array().unwrap();
    let means: Vec<(&str, f64)> =
        intensities.iter().map(|entry| (entry["source"].as_str().unwrap(), entry["mean_g_per_kwh"].as_f64().unwrap())).collect();
    // By hand: 3.04 g over 0.0204 kWh for the CPU's grid, 0.16 over 0.004 for the GPU's.
    assert_eq!(means.len(), 2);
    assert!(means[0].0 == "7.0" && close(means[0].1, 3.04 / 0.0204), "{means:?}");
    assert!(means[1].0 == "8.0" && close(means[1].1, 40.0), "{means:?}");
    for entry in intensities {
        assert!(entry["label"].is_string() && entry["fidelity"] == "measured", "{entry}");
    }
    // 0.0204 kWh written as kWh, not J.
    assert_eq!(per_source["3.0"]["energy_kwh"], serde_json::json!(0.0204));
}

/// The fixture files of every format, by path.
fn fixtures() -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for format in std::fs::read_dir(FIXTURES).unwrap() {
        let format = format.unwrap().path();
        assert!(format.file_name().unwrap().to_string_lossy().starts_with('v'), "{}", format.display());
        for file in std::fs::read_dir(&format).unwrap() {
            found.push(file.unwrap().path());
        }
    }
    found.sort();
    found
}

/// AC/REC-5, AC/REC-3
#[test]
fn every_fixture_of_every_format_still_reads_whole() {
    let fixtures = fixtures();
    assert!(!fixtures.is_empty());
    for path in fixtures {
        let text = std::fs::read_to_string(&path).unwrap();
        let records = record::read(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(records.len(), text.lines().count(), "{}", path.display());
        for (record, line) in records.iter().zip(text.lines()) {
            // A fixture is in a format this release knows: every kind is known
            // and nothing is lost on the way back.
            assert!(!matches!(record.line, record::Line::Unknown), "{}: {line}", path.display());
            let again = serde_json::to_value(record).unwrap();
            assert_eq!(again, serde_json::from_str::<serde_json::Value>(line).unwrap(), "{}", path.display());
        }
    }
}

/// AC/REC-2
#[test]
fn a_reader_takes_every_format_up_to_its_own_and_rejects_newer() {
    let header = |format: u32| {
        format!(
            "{{\"at\":\"2026-10-25T10:00:00Z\",\"kind\":\"run\",\"format\":{format},\"carbontracker\":\"3.0.0\",\
             \"frontend\":\"cli\",\"command\":[],\"config\":{{\"interval_s\":1.0,\"pue\":1.0,\"components\":null,\
             \"location\":null}},\"epochs\":null}}\n"
        )
    };
    for format in 1..=record::FORMAT {
        assert!(record::read(&header(format)).is_ok(), "format {format}");
    }
    let error = record::read(&header(record::FORMAT + 1)).unwrap_err();
    assert!(error.contains(&format!("format {}", record::FORMAT + 1)), "{error}");
}

/// AC/REC-3
#[test]
fn a_kind_or_field_from_a_later_version_is_skipped() {
    let future: record::Record =
        serde_json::from_str(r#"{"at":"2026-10-25T10:00:00Z","kind":"from_the_future","x":1}"#).unwrap();
    assert_eq!(future.line, record::Line::Unknown);
    let marked: record::Record =
        serde_json::from_str(r#"{"at":"2026-10-25T10:00:00Z","kind":"marked","span":2,"work":[],"later":true}"#).unwrap();
    assert_eq!(marked.line, record::Line::Marked(record::Marked { span: 2, work: Vec::new() }));
}

/// AC/REC-21
#[test]
fn the_record_types_name_nothing_of_the_engine() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/domain/record");
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for word in ["carbontracker::", "use carbontracker", "carbontracker_providers"] {
            assert!(!text.contains(word), "{} names {word}", path.display());
        }
    }
}
