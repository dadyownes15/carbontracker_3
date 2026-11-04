//! The native tracker: a run with carbontracker 2.4.7's console and its
//! epoch prediction.
//!
//! Span starts, stops and marks never wait, so they keep the GIL. Starting
//! and stopping release it: the run's writer takes it to print. In profiler
//! mode a span's start and stop also wait for reads, outside the GIL.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbontracker::{
    GramsCo2ePerKwh, GridLocation, PowerUsageEffectiveness, SourceKind, Timestamp, TrackerConfig, Watts,
};
use carbontracker_providers::{
    config_file, process_tree, Coordinates, CountryCode, GridConfig, Measure, Pinned, ProbeConfig,
};
use carbontracker_run::{
    presentation, standard_sinks, LatestForecast, Console, Folder, Layout, Outcome, Predictor, RunConfig,
    RunHandle, SpanId, Verbosity, Work,
};
use pyo3::exceptions::{PyOSError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyDict;

use crate::convert;
use crate::console_writer::PyWriter;
use crate::profiler::{self, Epochs, Freshness, Paced, Projected};

#[pyclass(module = "carbontracker._native")]
pub struct Tracker {
    run: Mutex<Option<RunHandle<Projected>>>,
    freshness: Arc<Freshness>,
    /// The sources a profiled span waits for, with their cooldowns.
    paced: Paced,
    fetch_timeout: Duration,
    epochs: Arc<Epochs>,
    /// The longest an ended epoch takes to settle: the next readout, and its fetch.
    settle_within: Duration,
    config_warning: Option<String>,
    location: Option<String>,
    started_at: f64,
}

fn invalid(message: impl ToString) -> PyErr {
    PyValueError::new_err(message.to_string())
}

fn stopped(error: impl ToString) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

#[pymethods]
impl Tracker {
    /// Probe this host and start tracking. The console's lines go to
    /// `write(line)`. Every invalid argument is a `ValueError`, raised before
    /// anything is probed or started.
    #[new]
    #[pyo3(signature = (
        *, zone, country, coordinates, global_, power, cpu_power, gpu_power, intensity, components,
        interval, pue, api_keys, devices_by_pid, log_dir, log_file_prefix, epochs, epochs_before_pred, monitor_epochs,
        verbose, decimal_precision, interpretable, profiler, write,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        py: Python<'_>,
        zone: Option<String>,
        country: Option<String>,
        coordinates: Option<(f64, f64)>,
        global_: bool,
        power: Option<f64>,
        cpu_power: Option<f64>,
        gpu_power: Option<f64>,
        intensity: Option<f64>,
        components: &str,
        interval: f64,
        pue: f64,
        api_keys: Option<HashMap<String, String>>,
        devices_by_pid: bool,
        log_dir: Option<PathBuf>,
        log_file_prefix: &str,
        epochs: Option<usize>,
        epochs_before_pred: i64,
        monitor_epochs: u64,
        verbose: i64,
        decimal_precision: usize,
        interpretable: bool,
        profiler: bool,
        write: Py<PyAny>,
    ) -> PyResult<Self> {
        // A profiler contacts no service, so it reads no key.
        let (keys, readable) = if profiler {
            Default::default()
        } else {
            config_file::keys(api_keys.unwrap_or_default()).map_err(|error| invalid(format!("api_keys: {error}")))?
        };
        let config_warning = readable.map(|path| presentation::config_readable(&path));
        if power.is_some() && (cpu_power.is_some() || gpu_power.is_some()) {
            return Err(invalid("power is the whole machine's draw, so it can't be given with cpu_power or gpu_power"));
        }
        let measure = components.parse::<Measure>().map_err(|error| invalid(format!("components: {error}")))?;
        let measure = if profiler { profiler::measure(measure) } else { measure };
        let probe = ProbeConfig {
            measure,
            grid: GridConfig {
                location: location(zone, country, coordinates, global_)?,
                api_keys: keys,
                ..GridConfig::default()
            },
            pinned: Pinned {
                power: watts("power", power)?,
                cpu_power: watts("cpu_power", cpu_power)?,
                gpu_power: watts("gpu_power", gpu_power)?,
                intensity: intensity
                    .map(|value| GramsCo2ePerKwh::new(value).map_err(|error| invalid(format!("intensity: {error}"))))
                    .transpose()?,
            },
            // This process and its children, as they are now.
            processes: devices_by_pid.then(|| process_tree(std::process::id())),
        };
        probe.validate().map_err(invalid)?;
        if !(interval.is_finite() && interval > 0.0) {
            return Err(invalid("update_interval must be a number of seconds above zero"));
        }
        let tracker = TrackerConfig {
            readout_interval: Duration::from_secs_f64(interval),
            pue: PowerUsageEffectiveness::new(pue).ok_or_else(|| invalid("pue must be at least 1"))?,
            ..TrackerConfig::default()
        };

        let folder = log_dir
            .map(|dir| Folder::claim(&dir, Some(log_file_prefix)))
            .transpose()
            .map_err(|error| PyOSError::new_err(format!("cannot write logs: {error}")))?;
        let console = Console {
            out: Box::new(PyWriter::new(write)),
            // A profiler prints its own table.
            verbosity: if verbose > 0 && !profiler { Verbosity::Normal } else { Verbosity::Quiet },
            layout: Layout::Legacy { decimals: decimal_precision, equivalents: interpretable },
        };
        let sinks = standard_sinks(console, folder.as_ref()).map_err(|error| PyOSError::new_err(error.to_string()))?;
        let predict_after = usize::try_from(epochs_before_pred).ok().filter(|&after| after > 0);
        let monitor = usize::try_from(monitor_epochs).ok().filter(|&monitor| monitor > 0);
        let freshness = Arc::new(Freshness::default());
        let settled_epochs = Arc::new(Epochs::default());
        let projected = |_: &_, forecaster: &_| Projected {
            predictor: Predictor::new(epochs, predict_after, monitor, LatestForecast::new(forecaster)),
            freshness: Arc::clone(&freshness),
            epochs: Arc::clone(&settled_epochs),
            settled: Vec::new(),
        };

        let started_at = convert::seconds(&chrono::Utc::now());
        // The writer takes the GIL to print, even while this waits.
        let run = py
            .detach(|| carbontracker_run::start(RunConfig { probe, tracker, frontend: "python", sinks }, projected))
            .map_err(stopped)?;
        let started = run.started();
        let location = started.header.config.location.as_ref().map(presentation::location);
        let power = |source| {
            let description = started.catalog.sources.get(&source)?;
            (description.kind() == SourceKind::Power).then(|| description.sample_kind())
        };
        let paced = profiler::paced(&started.cooldowns, power);
        let fetch_timeout = started.fetch_timeout;
        let settle_within = Duration::from_secs_f64(interval) + fetch_timeout;
        Ok(Self {
            run: Mutex::new(Some(run)),
            freshness,
            paced,
            fetch_timeout,
            epochs: settled_epochs,
            settle_within,
            config_warning,
            location,
            started_at,
        })
    }

    /// A warning about the config file, if others can read it.
    fn config_warning(&self) -> Option<String> {
        self.config_warning.clone()
    }

    /// Open a span. Never waits.
    #[pyo3(signature = (name, parent=None))]
    fn start_span(&self, name: &str, parent: Option<usize>) -> PyResult<usize> {
        self.with_run(|run| run.start_span(name, parent.map(SpanId::new)).get())
    }

    /// Report work on an open span. Never waits.
    fn mark(&self, span: usize, kind: &str, amount: f64) -> PyResult<()> {
        self.with_run(|run| run.mark(SpanId::new(span), Work::of(kind, amount)))
    }

    /// Stop a span with the work it did. Never waits.
    fn stop_span(&self, span: usize, work: Vec<(String, f64)>) -> PyResult<()> {
        let work: Work = work.into_iter().collect();
        self.with_run(|run| run.stop_span(SpanId::new(span), work))
    }

    /// Open a span on a read: wait until every paced source is fresh or
    /// ready, start, then wait for the reads the start asked for. Returns the
    /// span, the seconds waited before and after its start, and the sources
    /// whose read came late.
    #[pyo3(signature = (name, parent=None))]
    fn start_span_paced(
        &self,
        py: Python<'_>,
        name: &str,
        parent: Option<usize>,
    ) -> PyResult<(usize, f64, f64, Vec<String>)> {
        let began = Instant::now();
        let plan = profiler::start(&self.paced, &self.freshness.newest(), chrono::Utc::now());
        py.detach(|| sleep_until(plan.at));
        let after = chrono::Utc::now();
        let before = began.elapsed().as_secs_f64();
        let cut = Instant::now();
        let span = self.with_run(|run| run.start_span(name, parent.map(SpanId::new)).get())?;
        let read: Paced = self.paced.iter().filter(|(source, ..)| plan.read.contains(source)).copied().collect();
        let deadline = Instant::now() + self.fetch_timeout;
        let late = py.detach(|| self.await_fresh(span, &read, after, deadline));
        Ok((span, before, cut.elapsed().as_secs_f64(), late.iter().map(ToString::to_string).collect()))
    }

    /// Stop a span on a read: cut it now, keep it open until every paced
    /// source has landed a sample stamped after the cut, then stop it.
    /// Returns the seconds waited and the sources whose read came late.
    fn stop_span_paced(&self, py: Python<'_>, span: usize, work: Vec<(String, f64)>) -> PyResult<(f64, Vec<String>)> {
        let began = Instant::now();
        let mut late = Vec::new();
        if !self.paced.is_empty() {
            let after = chrono::Utc::now();
            self.with_run(|run| run.mark(SpanId::new(span), Work::default()))?;
            let due = profiler::ready(&self.paced, &self.freshness.newest(), after);
            let until_due = due.signed_duration_since(after).to_std().unwrap_or_default();
            let deadline = Instant::now() + until_due + self.fetch_timeout;
            late = py.detach(|| self.await_fresh(span, &self.paced, after, deadline));
        }
        let work: Work = work.into_iter().collect();
        self.with_run(|run| run.stop_span(SpanId::new(span), work))?;
        Ok((began.elapsed().as_secs_f64(), late.iter().map(ToString::to_string).collect()))
    }

    /// The whole run's cost: the `ended` epochs, scaled up to `epochs` of
    /// them, at the forecast's mean intensity. Waits for the ended epochs to
    /// settle, at most until the next readout and its fetch.
    fn predict<'py>(&self, py: Python<'py>, epochs: usize, ended: usize) -> PyResult<Bound<'py, PyDict>> {
        let deadline = Instant::now() + self.settle_within;
        py.detach(|| self.epochs.wait(ended, deadline));
        // Copied out under the view's lock, converted after.
        let prediction = self.with_run(|run| run.view().read(|projected| projected.predictor.predict(epochs)))?;
        convert::prediction(py, &prediction)
    }

    /// Stop tracking: say how the run `ended` (`completed`, `monitored`,
    /// `interrupted` or `failed`) after `epochs_done` epochs. Returns once
    /// everything is written, with the session and the prediction made.
    #[pyo3(signature = (ended, epochs_done=None))]
    fn stop<'py>(&self, py: Python<'py>, ended: &str, epochs_done: Option<u64>) -> PyResult<Bound<'py, PyDict>> {
        let outcome = match ended {
            "completed" => Outcome::Completed { epochs_done },
            "monitored" => Outcome::Monitored { epochs_done: epochs_done.unwrap_or_default() },
            "interrupted" => Outcome::Interrupted { epochs_done },
            "failed" => Outcome::Failed { epochs_done },
            other => return Err(invalid(format!("no ending named {other}"))),
        };
        let run = self.run.lock().expect("not poisoned").take().ok_or_else(|| stopped("tracking has stopped"))?;
        // The writer takes the GIL to print the closing block.
        let (report, view) = py.detach(|| run.finish(outcome)).map_err(stopped)?;
        let (prediction, settled) =
            view.read(|projected| (projected.predictor.prediction().cloned(), projected.settled.clone()));

        let session = convert::session(py, &report)?;
        session.set_item("started_at", self.started_at)?;
        session.set_item("location", self.location.clone())?;
        let paced: Vec<_> = self.paced.iter().map(|&(source, ..)| source).collect();
        let spans = settled.iter().map(|report| convert::span(py, report, &paced)).collect::<PyResult<Vec<_>>>()?;
        session.set_item("spans", spans)?;
        session.set_item("prediction", prediction.map(|prediction| convert::prediction(py, &prediction)).transpose()?)?;
        Ok(session)
    }
}

impl Tracker {
    /// Waits until every source in `paced` has landed a sample stamped after
    /// `after`, cutting `span` again while a late source has no read queued.
    /// Returns the sources still late at `deadline`.
    fn await_fresh(&self, span: usize, paced: &Paced, after: Timestamp, deadline: Instant) -> Vec<carbontracker::SourceId> {
        loop {
            let slice = deadline.min(Instant::now() + profiler::ASK_EVERY);
            let late = self.freshness.wait(paced, after, slice);
            if late.is_empty() || Instant::now() >= deadline {
                return late;
            }
            if profiler::ask_again(paced, &late, &self.freshness.newest(), chrono::Utc::now()) {
                // A refused mark changes nothing: the wait ends at the deadline.
                let _ = self.with_run(|run| run.mark(SpanId::new(span), Work::default()));
            }
        }
    }

    fn with_run<R>(&self, call: impl FnOnce(&RunHandle<Projected>) -> R) -> PyResult<R> {
        let run = self.run.lock().expect("not poisoned");
        run.as_ref().map(call).ok_or_else(|| stopped("tracking has stopped"))
    }
}

/// At most one of the four ways to say where the grid is; `None` estimates.
fn location(
    zone: Option<String>,
    country: Option<String>,
    coordinates: Option<(f64, f64)>,
    global: bool,
) -> PyResult<Option<GridLocation>> {
    let given = [zone.is_some(), country.is_some(), coordinates.is_some(), global].iter().filter(|&&given| given).count();
    if given > 1 {
        return Err(invalid("give at most one of zone, country, coordinates and global_"));
    }
    if let Some(zone) = zone {
        let zone = zone.trim();
        if zone.is_empty() {
            return Err(invalid("zone must not be empty, e.g. DK-DK2"));
        }
        return Ok(Some(GridLocation::Zone { zone_id: zone.to_owned() }));
    }
    if let Some(country) = country {
        let code = CountryCode::new(country.trim()).ok_or_else(|| invalid("country must be two letters, e.g. DK"))?;
        return Ok(Some(GridLocation::Country { country_code: code.as_str().to_owned() }));
    }
    if let Some((latitude, longitude)) = coordinates {
        let at = Coordinates::new(latitude, longitude)
            .ok_or_else(|| invalid("coordinates must be (latitude, longitude) within ±90 and ±180"))?;
        return Ok(Some(GridLocation::Coordinates { latitude: at.latitude(), longitude: at.longitude() }));
    }
    Ok(global.then_some(GridLocation::Global))
}

/// A stated draw, named for its error.
fn watts(name: &str, value: Option<f64>) -> PyResult<Option<Watts>> {
    value.map(|value| Watts::new(value).map_err(|error| invalid(format!("{name}: {error}")))).transpose()
}

/// Sleeps until `at`; returns at once if it has passed.
fn sleep_until(at: Timestamp) {
    if let Ok(left) = at.signed_duration_since(chrono::Utc::now()).to_std() {
        std::thread::sleep(left);
    }
}
