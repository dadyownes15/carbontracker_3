//! Run printouts: a run's items handed to the real [ConsoleSink], through
//! the real [Predictor] where the frontend has one, in the order the
//! forwarder hands them over.

use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use carbontracker::{
    Failure, CatalogReport, Fidelity, GramsCo2e, GridLocation, SampleKind, Joules, Observation,
    PowerDomain, PowerScope, ProviderError, ProviderId, ReadoutId, ReportWarning, Sample, SessionReport,
    SourceDescription, SourceId, SourceSelection, SpanId, SpanReport, Timestamp, TrackerError, TrackerEvent,
    Usage, Work,
};
use carbontracker_providers::{ForecastReport, ProbeProgress, Verdict};
use carbontracker_run::{
    LatestForecast, ConsoleSink, ForecastSource, Item, Layout, Outcome, Predictor, Projection, Sink, Started,
    Verbosity, deliver, presentation, record,
};
use chrono::{DateTime, TimeDelta, Utc};

use crate::page::Case;

const LOGS: &str = "carbontracker/run-20261028_140312-a1b2c3d4";

/// `carbontracker <command>`: the summary layout, on stderr.
pub fn cli() -> Vec<Case> {
    let summary = || Layout::Summary { title: "python train.py".to_owned() };
    vec![
        Case::new(
            "Mac, live grid",
            "Apple silicon through macmon, Electricity Maps for the grid and a projected forecast.",
            {
                let mut run = Run::new(Verbosity::Normal, summary(), Some(LOGS));
                run.probe(&mac_probe()).start(mac(), "cli");
                run.epochs(3, Duration::from_secs(40), 0.0021, 0.31);
                run.end(Outcome::Exited { code: 0 }, session(&mac(), 128, 0.0063, 0.93, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Linux GPU server, no API key",
            "RAPL and two A100s; the country average stands in for the grid. No run folder.",
            {
                let mut run = Run::new(Verbosity::Normal, summary(), None);
                run.probe(&gpu_probe()).start(gpu_server(), "cli");
                run.end(Outcome::Exited { code: 0 }, session(&gpu_server(), 5412, 1.84, 266.0, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Nothing found, no location",
            "No power source on this host and no location: generic CPU and the world average.",
            {
                let mut run = Run::new(Verbosity::Normal, summary(), Some(LOGS));
                run.probe(&nothing_found_probe()).start(nothing_found(), "cli");
                run.end(Outcome::Exited { code: 0 }, session(&nothing_found(), 61, 0.00071, 0.34, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Stated values",
            "carbontracker --power 300 --intensity 120 <command>: no probing.",
            {
                let mut run = Run::new(Verbosity::Normal, summary(), Some(LOGS));
                run.probe(&stated_probe()).start(stated(), "cli");
                run.end(Outcome::Exited { code: 0 }, session(&stated(), 3600, 0.3, 36.0, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "A command too short to sample",
            "Finished inside one readout interval, so no energy and the session's warnings.",
            {
                let mut run = Run::new(Verbosity::Normal, summary(), Some(LOGS));
                run.start(mac(), "cli");
                let cpu = vec!["Apple M2 Pro (cpu)".to_owned()];
                let warnings = vec![
                    ReportWarning::ThinCoverage { samples: 0, sources: cpu.clone() },
                    ReportWarning::NoIntensity { sources: cpu },
                ];
                run.end(Outcome::Exited { code: 0 }, session(&mac(), 0, 0.0, 0.0, warnings));
                run.text()
            },
        ),
        Case::new(
            "The command failed",
            "The command exits with 2; the summary is the same.",
            {
                let mut run = Run::new(Verbosity::Normal, summary(), Some(LOGS));
                run.start(mac(), "cli");
                run.end(Outcome::Exited { code: 2 }, session(&mac(), 9, 0.00004, 0.0059, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Verbose (-v)",
            "Every probe step and engine event, stamped, then the summary.",
            {
                let mut run = Run::new(Verbosity::Everything, summary(), Some(LOGS));
                run.probe(&mac_probe()).start(mac(), "cli");
                run.verbose_events();
                run.feed(Item::Gap);
                run.end(Outcome::Signalled { signal: 15 }, session(&mac(), 128, 0.0063, 0.93, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Quiet (-q)",
            "Only warnings and errors: a gap, a sink that failed, and an error.",
            {
                let mut run = Run::new(Verbosity::Quiet, summary(), Some(LOGS));
                run.probe(&mac_probe()).start(mac(), "cli");
                run.epochs(2, Duration::from_secs(40), 0.0021, 0.31);
                run.feed(Item::Gap);
                let failed = io::Error::new(io::ErrorKind::StorageFull, "No space left on device");
                run.feed(Item::Warning(presentation::sink_failed("record", &failed)));
                run.feed(Item::Error("the session didn't finish: the engine stopped".to_owned()));
                run.end(Outcome::Exited { code: 0 }, session(&mac(), 128, 0.0063, 0.93, Vec::new()));
                run.text()
            },
        ),
    ]
}

/// `CarbonTracker(epochs=…)`: carbontracker 2.4.7's layout, with the
/// prediction.
pub fn python() -> Vec<Case> {
    let legacy = |decimals| Layout::Legacy { decimals, equivalents: true };
    vec![
        Case::new(
            "Defaults",
            "epochs=10, epochs_before_pred=1, monitor_epochs=1: predicts after the first epoch over the \
             published forecast, then stops tracking.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), Some(LOGS))
                    .predicting(10, 1, Some(1), Forecast::Published { hours: 24 });
                run.probe(&mac_probe_published()).start(mac_published(), "python");
                run.epochs(1, Duration::from_secs(52), 0.0027, 0.39);
                run.end(Outcome::Monitored { epochs_done: 1 }, session(&mac_published(), 54, 0.0028, 0.41, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "All epochs monitored",
            "epochs=5, epochs_before_pred=3, monitor_epochs=-1: a projected forecast, every epoch tracked.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), Some(LOGS))
                    .predicting(5, 3, None, Forecast::Projected);
                run.probe(&mac_probe()).start(mac(), "python");
                run.epochs(5, Duration::from_secs(52), 0.0027, 0.39);
                run.end(Outcome::Completed { epochs_done: Some(5) }, session(&mac(), 262, 0.0136, 1.97, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "A run longer than the forecast",
            "epochs=100 of 40 minutes each: the 24 h forecast covers part of what remains.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), Some(LOGS))
                    .predicting(100, 2, Some(2), Forecast::Published { hours: 24 });
                run.probe(&gpu_probe()).start(gpu_server_published(), "python");
                run.epochs(2, Duration::from_secs(40 * 60), 0.61, 89.0);
                run.end(Outcome::Monitored { epochs_done: 2 }, session(&gpu_server_published(), 4805, 1.22, 178.0, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "No forecast",
            "The country average serves the grid and no forecast resolves.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), Some(LOGS))
                    .predicting(10, 2, Some(2), Forecast::None);
                run.probe(&gpu_probe()).start(gpu_server(), "python");
                run.epochs(2, Duration::from_secs(600), 0.15, 22.0);
                run.end(Outcome::Monitored { epochs_done: 2 }, session(&gpu_server(), 1203, 0.3, 44.0, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Interrupted",
            "epochs=10, monitor_epochs=5: training stops after three epochs.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), Some(LOGS))
                    .predicting(10, 1, Some(5), Forecast::Projected);
                run.probe(&mac_probe()).start(mac(), "python");
                run.epochs(3, Duration::from_secs(52), 0.0027, 0.39);
                run.end(
                    Outcome::Interrupted { epochs_done: Some(3) },
                    session(&mac(), 158, 0.0082, 1.18, Vec::new()),
                );
                run.text()
            },
        ),
        Case::new(
            "interpretable=False, decimal_precision=3",
            "No equivalents, three decimals.",
            {
                let layout = Layout::Legacy { decimals: 3, equivalents: false };
                let mut run = Run::new(Verbosity::Normal, layout, Some(LOGS))
                    .predicting(10, 1, Some(1), Forecast::Projected);
                run.probe(&gpu_probe()).start(gpu_server(), "python");
                run.epochs(1, Duration::from_secs(600), 0.15, 22.0);
                run.end(Outcome::Monitored { epochs_done: 1 }, session(&gpu_server(), 601, 0.15, 22.0, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "No log_dir, epochs_before_pred=0",
            "No prediction and no logs line.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), None).predicting_never(10, Some(1));
                run.probe(&mac_probe()).start(mac(), "python");
                run.epochs(1, Duration::from_secs(52), 0.0027, 0.39);
                run.end(Outcome::Monitored { epochs_done: 1 }, session(&mac(), 54, 0.0028, 0.41, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "Stated values",
            "power=300, intensity=120: no probing.",
            {
                let mut run = Run::new(Verbosity::Normal, legacy(12), Some(LOGS))
                    .predicting(10, 1, Some(1), Forecast::None);
                run.probe(&stated_probe()).start(stated(), "python");
                run.epochs(1, Duration::from_secs(360), 0.03, 3.6);
                run.end(Outcome::Monitored { epochs_done: 1 }, session(&stated(), 361, 0.03, 3.6, Vec::new()));
                run.text()
            },
        ),
        Case::new(
            "verbose=0",
            "Quiet: only warnings and errors.",
            {
                let mut run = Run::new(Verbosity::Quiet, legacy(12), Some(LOGS))
                    .predicting(10, 1, Some(1), Forecast::Projected);
                run.probe(&mac_probe()).start(mac(), "python");
                run.epochs(1, Duration::from_secs(52), 0.0027, 0.39);
                run.feed(Item::Gap);
                run.end(Outcome::Monitored { epochs_done: 1 }, session(&mac(), 54, 0.0028, 0.41, Vec::new()));
                run.text()
            },
        ),
    ]
}

/// What the writer prints when finishing a sink fails and none is left to
/// tell.
pub fn sink_failed_at_end() -> Case {
    let failed = io::Error::new(io::ErrorKind::StorageFull, "No space left on device");
    Case::new(
        "A sink failed at the end",
        "The writer, when finishing a sink fails and none is left to tell.",
        presentation::prefixed(&presentation::warning(&presentation::sink_failed("record", &failed))),
    )
}

/// What Python prints when tracking fails and training goes on.
pub fn ignored_error() -> Case {
    Case::new(
        "ignore_errors=True",
        "When tracking fails and training goes on.",
        presentation::ignored_error(
            "Traceback (most recent call last):\n  File \"train.py\", line 12, in <module>\nRuntimeError: the engine stopped\n",
        ),
    )
}

/// What the predictor's forecast question gets back.
#[derive(Clone, Copy)]
enum Forecast {
    /// A curve from now on, hourly.
    Published { hours: i64 },
    /// The current intensity held flat, as the `projection` candidate does.
    Projected,
    None,
}

impl Forecast {
    fn ask(self) -> LatestForecast {
        LatestForecast::from_fn(move || {
            let now = Utc::now();
            let hourly = |hours: i64, value: &dyn Fn(i64) -> f64| -> Vec<Sample> {
                (0..=hours).map(|hour| Sample::new(now + TimeDelta::hours(hour), value(hour))).collect()
            };
            let curve = match self {
                Self::Published { hours } => hourly(hours, &|hour| 140.0 + 40.0 * ((hour as f64) / 4.0).sin()),
                Self::Projected => hourly(24, &|_| 148.0),
                Self::None => return Some(None),
            };
            let report = ForecastReport {
                location: zone("DK"),
                fidelity: Fidelity::Modelled,
                horizon: Duration::from_secs(24 * 3600),
                curve,
            };
            Some(Some(report))
        })
    }
}

/// One run, from the console's side: the sink and the frontend's
/// projection.
struct Run {
    out: Shared,
    sink: ConsoleSink,
    predictor: Option<Predictor>,
    /// Spans opened so far.
    spans: usize,
    /// Seconds since the run began, for event times.
    at: i64,
}

impl Run {
    fn new(verbosity: Verbosity, layout: Layout, logs: Option<&str>) -> Self {
        let out = Shared::default();
        let mut second = 11;
        let sink = ConsoleSink::new(out.clone(), verbosity, layout, logs.map(str::to_owned)).with_clock(move || {
            second += 1;
            format!("14:{:02}:{:02}", 3 + second / 60, second % 60)
        });
        Self { out, sink, predictor: None, spans: 0, at: 0 }
    }

    fn predicting(mut self, epochs: usize, predict_after: usize, monitor: Option<usize>, forecast: Forecast) -> Self {
        self.predictor = Some(Predictor::new(Some(epochs), Some(predict_after), monitor, forecast.ask()));
        self
    }

    fn predicting_never(mut self, epochs: usize, monitor: Option<usize>) -> Self {
        self.predictor = Some(Predictor::new(Some(epochs), None, monitor, Forecast::None.ask()));
        self
    }

    /// Hands `item` over as the forwarder does: the blocks it projects
    /// first, then the item.
    fn feed(&mut self, item: Item) -> &mut Self {
        if let Some(predictor) = &mut self.predictor {
            for block in predictor.project(&item) {
                deliver(&mut self.sink, &Item::Block(Arc::from(block))).unwrap();
            }
        }
        deliver(&mut self.sink, &item).unwrap();
        self
    }

    fn probe(&mut self, steps: &[(&'static str, Verdict)]) -> &mut Self {
        for (candidate, verdict) in steps {
            deliver(&mut self.sink, &Item::Probing(ProbeProgress::Probing { candidate })).unwrap();
            let decided = ProbeProgress::Decided { candidate, verdict: verdict.clone() };
            deliver(&mut self.sink, &Item::Probing(decided)).unwrap();
        }
        self
    }

    fn start(&mut self, host: Host, frontend: &str) -> &mut Self {
        let mut header = record::Header {
            format: record::FORMAT,
            carbontracker: "3.0.0".to_owned(),
            frontend: frontend.to_owned(),
            command: vec!["python".to_owned(), "train.py".to_owned()],
            config: record::Config { interval_s: 1.0, pue: 1.58, components: None, location: None },
            epochs: None,
        };
        if let Some(predictor) = &self.predictor {
            predictor.describe(&mut header);
        }
        let probe = record::Probe {
            candidates: host
                .served_by
                .values()
                .map(|name| record::Candidate {
                    name: (*name).to_owned(),
                    verdict: "serving".to_owned(),
                    series: Vec::new(),
                })
                .collect(),
        };
        self.feed(Item::Started(Arc::new(Started { header, probe, catalog: host.catalog, served_by: host.served_by, cooldowns: Default::default(), fetch_timeout: std::time::Duration::from_secs(10), forecast: host.forecast })))
    }

    fn event(&mut self, event: TrackerEvent) -> &mut Self {
        self.feed(Item::Event(event))
    }

    fn next_span(&mut self) -> SpanId {
        self.spans += 1;
        SpanId::new(self.spans)
    }

    /// `count` top-level epochs, each `duration` long, using `kwh` and
    /// `grams`.
    fn epochs(&mut self, count: usize, duration: Duration, kwh: f64, grams: f64) {
        for _ in 0..count {
            let span = self.next_span();
            self.event(TrackerEvent::SpanStarted {
                span,
                name: "epoch".to_owned(),
                parent: None,
                selection: SourceSelection::All,
                at: t(self.at),
            });
            self.at += duration.as_secs() as i64;
            self.event(TrackerEvent::SpanStopped { span, work: Work::default(), at: t(self.at) });
            self.event(settled(span, "epoch", duration, kwh, grams, Vec::new()));
        }
    }

    /// One of each event the verbose console shows.
    fn verbose_events(&mut self) {
        let (epoch, eval) = (self.next_span(), self.next_span());
        self.event(TrackerEvent::SpanStarted {
            span: epoch,
            name: "epoch".to_owned(),
            parent: None,
            selection: SourceSelection::All,
            at: t(0),
        });
        self.event(TrackerEvent::ReadoutLanded {
            readout: ReadoutId(1),
            outcomes: Arc::new(HashMap::from([
                (id(1, 0), Observation::Samples(vec![Sample::new(t(1), 3.21)])),
                (id(1, 1), Observation::Samples(vec![Sample::new(t(1), 0.412)])),
                (id(1, 2), Observation::Samples(vec![Sample::new(t(1), 0.0)])),
                (id(1, 3), Observation::Samples(vec![Sample::new(t(1), 0.731)])),
                (id(1, 4), Observation::Samples(vec![Sample::new(t(1), 6.02)])),
                (id(7, 0), Observation::Samples(vec![Sample::new(t(1), 148.0)])),
            ])),
        });
        self.event(TrackerEvent::ReadoutLanded {
            readout: ReadoutId(2),
            outcomes: Arc::new(HashMap::from([
                (id(1, 0), Observation::Samples(vec![Sample::new(t(2), 3.37)])),
                (id(1, 1), Observation::Unchanged),
                (id(7, 0), Observation::Failed(Failure::Provider(ProviderError::Unavailable("network down".to_owned())))),
            ])),
        });
        self.event(TrackerEvent::SpanStarted {
            span: eval,
            name: "eval".to_owned(),
            parent: Some(epoch),
            selection: SourceSelection::Domains(vec![PowerDomain::Cpu]),
            at: t(30),
        });
        self.event(TrackerEvent::Marked {
            span: eval,
            work: Work::of("samples", 64.0),
            at: t(31),
        });
        self.event(TrackerEvent::SpanStopped { span: eval, work: Work::default(), at: t(31) });
        let thin = vec![ReportWarning::ThinCoverage { samples: 1, sources: vec!["GPU".to_owned()] }];
        self.event(settled(eval, "eval", Duration::from_millis(800), 0.0000008, 0.00012, thin));
        self.event(TrackerEvent::SpanStopped { span: epoch, work: Work::default(), at: t(40) });
        self.event(settled(epoch, "epoch", Duration::from_secs(40), 0.0021, 0.31, Vec::new()));
        self.event(TrackerEvent::CommandRefused { span: eval, error: TrackerError::AlreadyClosed(eval) });
    }

    /// How the run ended, then the session.
    fn end(&mut self, outcome: Outcome, session: SessionReport) {
        self.feed(Item::Outcome(outcome));
        self.event(TrackerEvent::SessionEnded { report: Arc::new(session) });
    }

    fn text(mut self) -> String {
        self.sink.finish().unwrap();
        String::from_utf8(self.out.0.lock().unwrap().clone()).unwrap()
    }
}

/// What a host resolved to: the engine's catalog, the candidate serving
/// each source, and the forecast.
struct Host {
    catalog: CatalogReport,
    served_by: HashMap<SourceId, &'static str>,
    forecast: Option<ForecastSource>,
}

impl Host {
    fn new(sources: Vec<(SourceId, &'static str, SourceDescription)>) -> Self {
        let mut host = Self { catalog: CatalogReport::default(), served_by: HashMap::new(), forecast: None };
        let mut intensity = None;
        for (id, candidate, description) in sources {
            if let SourceDescription::Intensity { .. } = description {
                intensity = Some(id);
            }
            host.catalog.sources.insert(id, description);
            host.served_by.insert(id, candidate);
        }
        if let Some(intensity) = intensity {
            host.catalog.pairs = host
                .catalog
                .sources
                .iter()
                .filter(|(_, description)| matches!(description, SourceDescription::Power { .. }))
                .map(|(&id, _)| (id, intensity))
                .collect();
        }
        host
    }

    /// The day-long forecast `served_by` serves for `location`.
    fn forecasting(mut self, served_by: &'static str, location: GridLocation, fidelity: Fidelity) -> Self {
        let horizon = Duration::from_secs(24 * 3600);
        self.forecast = Some(ForecastSource { location, horizon, fidelity, served_by });
        self
    }
}

fn mac_power() -> Vec<(SourceId, &'static str, SourceDescription)> {
    [PowerDomain::Cpu, PowerDomain::Gpu, PowerDomain::Ane, PowerDomain::Ram, PowerDomain::System]
        .into_iter()
        .enumerate()
        .map(|(slot, domain)| (id(1, slot as u16), "macmon", power("Apple M2 Pro", domain, Fidelity::Measured)))
        .collect()
}

fn mac() -> Host {
    let mut sources = mac_power();
    sources.push((id(7, 0), "electricity-maps", intensity(zone("DK"), Fidelity::Measured)));
    Host::new(sources).forecasting("projection", zone("DK"), Fidelity::Modelled)
}

fn mac_published() -> Host {
    let mut sources = mac_power();
    sources.push((id(7, 0), "electricity-maps", intensity(zone("DK-DK2"), Fidelity::Measured)));
    Host::new(sources).forecasting("electricity-maps-forecast", zone("DK-DK2"), Fidelity::Measured)
}

fn gpu_power() -> Vec<(SourceId, &'static str, SourceDescription)> {
    vec![
        (id(2, 0), "rapl", power("Intel Xeon Gold 6338", PowerDomain::Cpu, Fidelity::Measured)),
        (id(2, 1), "rapl", power("Intel Xeon Gold 6338", PowerDomain::Ram, Fidelity::Measured)),
        (id(3, 0), "nvml-energy", power("NVIDIA A100-SXM4-40GB", PowerDomain::Gpu, Fidelity::Measured)),
        (id(3, 1), "nvml-energy", power("NVIDIA A100-SXM4-40GB", PowerDomain::Gpu, Fidelity::Measured)),
    ]
}

fn gpu_server() -> Host {
    let mut sources = gpu_power();
    let dk = GridLocation::Country { country_code: "dk".to_owned() };
    sources.push((id(10, 0), "country-average", intensity(dk, Fidelity::Modelled)));
    Host::new(sources)
}

fn gpu_server_published() -> Host {
    let mut sources = gpu_power();
    sources.push((id(7, 0), "electricity-maps", intensity(zone("DK-DK2"), Fidelity::Measured)));
    Host::new(sources).forecasting("electricity-maps-forecast", zone("DK-DK2"), Fidelity::Measured)
}

fn nothing_found() -> Host {
    Host::new(vec![
        (id(4, 0), "generic-cpu", power("Apple M2 Pro", PowerDomain::Cpu, Fidelity::Modelled)),
        (id(11, 0), "global-average", intensity(GridLocation::Global, Fidelity::Modelled)),
    ])
}

fn stated() -> Host {
    Host::new(vec![
        (id(5, 0), "static", power("static", PowerDomain::System, Fidelity::Stated)),
        (id(12, 0), "static", intensity(GridLocation::Global, Fidelity::Stated)),
    ])
}

fn mac_probe() -> Vec<(&'static str, Verdict)> {
    vec![
        ("nvml-energy", Verdict::Declined),
        ("nvml-power", Verdict::Declined),
        ("macmon", Verdict::Serving(vec!["Apple M2 Pro (Cpu/DeviceTotal, measured)".to_owned()])),
        ("powermetrics", Verdict::Covered),
        ("generic-cpu", Verdict::Covered),
        ("ip-geolocation", Verdict::Serving(vec!["DK (55.68, 12.57)".to_owned()])),
        ("electricity-maps", Verdict::Serving(vec!["intensity (DK, measured)".to_owned()])),
        ("country-average", Verdict::Covered),
        ("global-average", Verdict::Covered),
        ("electricity-maps-forecast", Verdict::Declined),
        ("projection", Verdict::Serving(vec!["intensity forecast (DK, modelled)".to_owned()])),
    ]
}

fn mac_probe_published() -> Vec<(&'static str, Verdict)> {
    let mut steps = mac_probe();
    steps.truncate(8);
    steps.push(("electricity-maps-forecast", Verdict::Serving(vec!["intensity forecast (DK-DK2, measured)".to_owned()])));
    steps.push(("projection", Verdict::Covered));
    steps
}

fn nothing_found_probe() -> Vec<(&'static str, Verdict)> {
    vec![
        ("nvml-energy", Verdict::Declined),
        ("nvml-power", Verdict::Declined),
        ("macmon", Verdict::Declined),
        ("powermetrics", Verdict::Declined),
        ("generic-cpu", Verdict::Serving(vec!["Apple M2 Pro (Cpu/DeviceTotal, modelled)".to_owned()])),
        ("ip-geolocation", Verdict::Declined),
        ("electricity-maps", Verdict::Declined),
        ("country-average", Verdict::Declined),
        ("global-average", Verdict::Serving(vec!["intensity (world average, modelled)".to_owned()])),
        ("electricity-maps-forecast", Verdict::Declined),
        ("projection", Verdict::NotWanted),
    ]
}

fn stated_probe() -> Vec<(&'static str, Verdict)> {
    vec![
        ("static", Verdict::Serving(vec!["static (System/DeviceTotal, stated)".to_owned()])),
        ("static", Verdict::Serving(vec!["intensity (world average, stated)".to_owned()])),
    ]
}

fn gpu_probe() -> Vec<(&'static str, Verdict)> {
    vec![
        ("nvml-energy", Verdict::Serving(vec!["NVIDIA A100-SXM4-40GB (Gpu/DeviceTotal, measured)".to_owned()])),
        ("nvml-power", Verdict::Declined),
        ("rapl", Verdict::Serving(vec!["Intel Xeon Gold 6338 (Cpu/DeviceTotal, measured)".to_owned()])),
        ("generic-cpu", Verdict::Covered),
        ("ip-geolocation", Verdict::Serving(vec!["DK".to_owned()])),
        ("electricity-maps", Verdict::Declined),
        ("country-average", Verdict::Serving(vec!["intensity (DK, modelled)".to_owned()])),
        ("global-average", Verdict::Covered),
        ("electricity-maps-forecast", Verdict::Declined),
        ("projection", Verdict::NotWanted),
    ]
}

/// The session over `host`'s sources, `seconds` long.
fn session(host: &Host, seconds: u64, kwh: f64, grams: f64, warnings: Vec<ReportWarning>) -> SessionReport {
    let intensities = host
        .catalog
        .pairs
        .values()
        .filter_map(|&id| Some((id, host.catalog.sources.get(&id)?.clone())))
        .collect();
    SessionReport {
        per_source: HashMap::new(),
        sources: host.catalog.sources.clone(),
        total: usage(kwh, grams),
        claimed: usage(kwh, grams),
        unclaimed: usage(0.0, 0.0),
        intensities,
        duration: Duration::from_secs(seconds),
        samples: host.catalog.sources.keys().map(|&id| (id, seconds as usize)).collect(),
        warnings,
    }
}

fn settled(span: SpanId, name: &str, duration: Duration, kwh: f64, grams: f64, warnings: Vec<ReportWarning>) -> TrackerEvent {
    TrackerEvent::SpanSettled {
        report: Arc::new(SpanReport {
            span_id: span,
            span_name: name.to_owned(),
            per_source: HashMap::new(),
            sources: HashMap::new(),
            total: usage(kwh, grams),
            exclusive: usage(kwh, grams),
            intensities: HashMap::new(),
            duration,
            samples: HashMap::new(),
            warnings,
            selection: SourceSelection::All,
        }),
    }
}

fn id(provider: usize, slot: u16) -> SourceId {
    SourceId::new(ProviderId(provider), slot)
}

fn t(seconds: i64) -> Timestamp {
    DateTime::from_timestamp(1_761_659_000, 0).unwrap() + TimeDelta::seconds(seconds)
}

fn usage(kwh: f64, grams: f64) -> Usage {
    Usage { energy: Joules::new(kwh * 3_600_000.0).unwrap(), emissions: Some(GramsCo2e::new(grams).unwrap()) }
}

fn zone(zone_id: &str) -> GridLocation {
    GridLocation::Zone { zone_id: zone_id.to_owned() }
}

fn power(device: &str, domain: PowerDomain, fidelity: Fidelity) -> SourceDescription {
    let sample_kind = if device.starts_with("Intel") { SampleKind::Total } else { SampleKind::Instant };
    SourceDescription::Power {
        device_name: device.to_owned(),
        domain,
        scope: PowerScope::DeviceTotal,
        sample_kind,
        fidelity,
    }
}

fn intensity(location: GridLocation, fidelity: Fidelity) -> SourceDescription {
    SourceDescription::Intensity { location, fidelity }
}

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
