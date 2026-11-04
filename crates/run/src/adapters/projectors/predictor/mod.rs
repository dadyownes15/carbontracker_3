//! [Predictor]: carbontracker 2.4.7's prediction, as a projection. Counts
//! the epochs as they settle and, once enough have, predicts the whole run
//! ([prediction]) over the grid's forecast, which it asks a
//! [Forecaster] for ([LatestForecast]).

pub mod prediction;

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::Arc;

use carbontracker::{Sample, SpanId, SpanReport, TrackerEvent, Usage};
use carbontracker_providers::{ForecastReport, Forecaster};

use crate::domain::item::{Block, Item, Layout};
use crate::domain::presentation::{self, PREFIX};
use crate::domain::record;
use crate::ports::Projection;
use prediction::{Measured, Prediction};

/// The name of each epoch's span: carbontracker 2.4.7's epochs are the
/// top-level spans named this.
pub const EPOCH: &str = "epoch";

/// Reads the grid's latest forecast: `None` until one has been fetched,
/// `Some(None)` if there is none. Never waits.
pub struct LatestForecast(Box<dyn Fn() -> Option<Option<ForecastReport>> + Send>);

impl LatestForecast {
    /// Reads `forecaster`'s.
    pub fn new(forecaster: &Forecaster) -> Self {
        let forecaster = forecaster.clone();
        Self(Box::new(move || forecaster.latest()))
    }

    /// Reads `latest` instead of a [Forecaster], e.g. a fixed forecast.
    pub fn from_fn(latest: impl Fn() -> Option<Option<ForecastReport>> + Send + 'static) -> Self {
        Self(Box::new(latest))
    }
}

impl fmt::Debug for LatestForecast {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LatestForecast")
    }
}

/// The epochs planned, and what the epochs so far add up to. Only while the
/// run is active: it ignores how the run ends.
#[derive(Debug)]
pub struct Predictor {
    /// The epochs planned; `None` prints no prediction, and
    /// [Predictor::predict] still works.
    epochs: Option<usize>,
    /// The epochs to measure before predicting; `None` for no prediction.
    predict_after: Option<usize>,
    /// The epochs monitored, for the record.
    monitor: Option<usize>,
    latest: LatestForecast,
    /// Whether a forecast has been fetched, curve or not.
    fetched: bool,
    /// The latest forecast curve.
    curve: Vec<Sample>,
    /// The forecast is the current intensity held flat.
    projected: bool,
    /// Epochs started and not yet settled.
    open: HashSet<SpanId>,
    /// Settled epochs, in the order they started.
    settled: BTreeMap<SpanId, Arc<SpanReport>>,
    /// `predict_after` epochs have settled: the prediction is made once the
    /// forecast is answered.
    due: bool,
    prediction: Option<Prediction>,
}

impl Predictor {
    pub fn new(
        epochs: Option<usize>,
        predict_after: Option<usize>,
        monitor: Option<usize>,
        latest: LatestForecast,
    ) -> Self {
        Self {
            epochs,
            predict_after,
            monitor,
            latest,
            fetched: false,
            curve: Vec::new(),
            projected: false,
            open: HashSet::new(),
            settled: BTreeMap::new(),
            due: false,
            prediction: None,
        }
    }

    /// The prediction made once `predict_after` epochs settled.
    pub fn prediction(&self) -> Option<&Prediction> {
        self.prediction.as_ref()
    }

    /// The whole run scaled to `planned` epochs, from the epochs settled so
    /// far.
    pub fn predict(&self, planned: usize) -> Prediction {
        let curve = (!self.curve.is_empty()).then_some(self.curve.as_slice());
        prediction::predict(
            &self.measured(),
            planned as f64,
            curve,
            self.projected,
            chrono::Utc::now(),
        )
    }

    /// The epochs settled so far.
    pub fn measured(&self) -> Measured {
        self.settled
            .values()
            .fold(Measured::default(), |mut measured, report| {
                measured.count += 1;
                measured.usage += report.total;
                measured.duration += report.duration;
                measured
            })
    }

    /// Takes the latest forecast, if one has been fetched. Never waits.
    fn refresh_forecast(&mut self) {
        if let Some(report) = (self.latest.0)() {
            self.fetched = true;
            self.curve = report.map(|forecast| forecast.curve).unwrap_or_default();
        }
    }
}

impl Projection for Predictor {
    fn project(&mut self, item: &Item) -> Vec<Box<dyn Block>> {
        self.refresh_forecast();
        match item {
            Item::Started(started) => {
                self.projected = started.probe.candidates.iter().any(|candidate| {
                    candidate.name == "projection" && candidate.verdict == "serving"
                });
            }
            Item::Event(TrackerEvent::SpanStarted {
                span,
                name,
                parent: None,
                ..
            }) if name == EPOCH => {
                self.open.insert(*span);
            }
            Item::Event(TrackerEvent::SpanSettled { report })
                if self.open.remove(&report.span_id) =>
            {
                self.settled.insert(report.span_id, Arc::clone(report));
                if self.epochs.is_some()
                    && self.prediction.is_none()
                    && Some(self.settled.len()) == self.predict_after
                {
                    self.due = true;
                }
            }
            _ => {}
        }
        // Once a forecast has been fetched; at the outcome at the latest, with
        // what there is, so the run's end never loses the prediction.
        let (Some(epochs), true) = (self.epochs, self.due) else {
            return Vec::new();
        };
        if !self.fetched && !matches!(item, Item::Outcome(_)) {
            return Vec::new();
        }
        self.due = false;
        let prediction = self.predict(epochs);
        self.prediction = Some(prediction.clone());
        vec![Box::new(Predicted { epochs, prediction })]
    }

    fn describe(&self, header: &mut record::Header) {
        if let Some(planned) = self.epochs {
            header.epochs = Some(record::Epochs {
                planned: planned as u64,
                monitor: self.monitor.unwrap_or(planned) as u64,
                before_prediction: self.predict_after.map_or(0, |after| after as i64),
            });
        }
    }
}

/// carbontracker 2.4.7's "Predicted consumption" block, then a note per
/// warning.
#[derive(Debug)]
struct Predicted {
    epochs: usize,
    prediction: Prediction,
}

impl Block for Predicted {
    fn lines(&self, layout: &Layout) -> Vec<String> {
        let usage = Usage {
            energy: self.prediction.energy,
            emissions: Some(self.prediction.emissions),
        };
        let title = format!("Predicted consumption for {} epoch(s):", self.epochs);
        let block = match layout {
            Layout::Legacy {
                decimals,
                equivalents,
            } => {
                let layout = presentation::Layout::Legacy {
                    decimals: *decimals,
                };
                presentation::consumption(
                    &title,
                    self.prediction.duration,
                    usage,
                    layout,
                    *equivalents,
                )
            }
            Layout::Summary { .. } => {
                let layout = presentation::Layout::Summary {
                    intensity: None,
                    logs: None,
                };
                presentation::consumption(&title, self.prediction.duration, usage, layout, true)
            }
        };
        let mut lines = vec![format!("{PREFIX}\n{block}")];
        lines.extend(
            self.prediction
                .warnings
                .iter()
                .map(|warning| format!("{PREFIX}{}", presentation::note(&warning.to_string()))),
        );
        lines
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use std::collections::HashMap;
    use std::sync::Mutex;

    use carbontracker::{
        CatalogReport, Fidelity, GramsCo2e, GridLocation, Joules,
        SourceSelection,
    };

    use super::prediction::PredictionWarning;
    use super::*;
    use crate::domain::item::{Outcome, Started};

    /// A flat forecast at `g_per_kwh`, a day long.
    fn forecast(g_per_kwh: f64) -> ForecastReport {
        let now = chrono::Utc::now();
        ForecastReport {
            location: GridLocation::Global,
            horizon: Duration::from_secs(24 * 3600),
            fidelity: Fidelity::Stated,
            curve: vec![
                Sample::new(now, g_per_kwh),
                Sample::new(now + chrono::TimeDelta::days(1), g_per_kwh),
            ],
        }
    }

    /// Has `forecast` from the start.
    fn answering(forecast: ForecastReport) -> LatestForecast {
        LatestForecast::from_fn(move || Some(Some(forecast.clone())))
    }

    /// What a held forecast reads, for the test to set later.
    type Held = Arc<Mutex<Option<Option<ForecastReport>>>>;

    /// Nothing fetched until the test sets it.
    fn held() -> (LatestForecast, Held) {
        let latest = Arc::new(Mutex::new(None));
        let read = Arc::clone(&latest);
        (LatestForecast::from_fn(move || read.lock().unwrap().clone()), latest)
    }

    fn predictor(
        epochs: usize,
        predict_after: Option<usize>,
        latest: LatestForecast,
    ) -> Predictor {
        Predictor::new(Some(epochs), predict_after, None, latest)
    }

    fn started() -> Item {
        Item::Started(Arc::new(Started {
            header: record::Header {
                format: record::FORMAT,
                carbontracker: "3".to_owned(),
                frontend: "python".to_owned(),
                command: Vec::new(),
                config: record::Config {
                    interval_s: 1.0,
                    pue: 1.0,
                    components: None,
                    location: None,
                },
                epochs: None,
            },
            probe: record::Probe {
                candidates: Vec::new(),
            },
            catalog: CatalogReport::default(),
            served_by: HashMap::new(),
            cooldowns: HashMap::new(),
            fetch_timeout: std::time::Duration::from_secs(10),
            forecast: None,
        }))
    }

    fn epoch_started(id: usize) -> Item {
        Item::Event(TrackerEvent::SpanStarted {
            span: SpanId::new(id),
            name: EPOCH.to_owned(),
            parent: None,
            selection: SourceSelection::All,
            at: chrono::Utc::now(),
        })
    }

    fn settled(id: usize, kwh: f64) -> Item {
        let usage = Usage {
            energy: Joules::new(kwh * 3_600_000.0).unwrap(),
            emissions: Some(GramsCo2e::new(kwh * 100.0).unwrap()),
        };
        Item::Event(TrackerEvent::SpanSettled {
            report: Arc::new(SpanReport {
                span_id: SpanId::new(id),
                span_name: EPOCH.to_owned(),
                per_source: HashMap::new(),
                sources: HashMap::new(),
                total: usage,
                exclusive: usage,
                intensities: HashMap::new(),
                duration: Duration::from_secs(60),
                samples: HashMap::new(),
                warnings: Vec::new(),
                selection: SourceSelection::All,
            }),
        })
    }

    fn project(predictor: &mut Predictor, items: &[Item]) -> usize {
        items.iter().map(|item| predictor.project(item).len()).sum()
    }

    /// AC/RUN-17

    #[test]
    fn the_prediction_comes_once_predict_after_epochs_settled() {
        let mut predictor = predictor(10, Some(2), answering(forecast(100.0)));
        assert_eq!(
            project(
                &mut predictor,
                &[
                    started(),
                    epoch_started(1),
                    settled(1, 1.0),
                    epoch_started(2)
                ]
            ),
            0
        );
        assert!(predictor.prediction().is_none());

        assert_eq!(
            predictor.project(&settled(2, 3.0)).len(),
            1,
            "the prediction block"
        );
        let prediction = predictor.prediction().unwrap();
        assert_eq!((prediction.measured, prediction.total), (2, 10.0));
        assert_eq!(
            prediction.energy.to_kilowatt_hours().get(),
            20.0,
            "2 kWh an epoch, ten epochs"
        );

        assert_eq!(
            project(&mut predictor, &[epoch_started(3), settled(3, 1.0)]),
            0,
            "only one prediction"
        );
        assert_eq!(predictor.measured().count, 3);
        assert_eq!(
            predictor.predict(4).measured,
            3,
            "asked for, from every epoch settled"
        );
    }

    /// AC/RUN-2, AC/RUN-17

    #[test]
    fn the_prediction_uses_the_latest_forecast() {
        let mut predictor = predictor(10, Some(1), answering(forecast(250.0)));
        project(
            &mut predictor,
            &[started(), epoch_started(1), settled(1, 1.0)],
        );

        let prediction = predictor.prediction().expect("a prediction");
        assert_eq!(
            prediction.intensity.get(),
            250.0,
            "the forecast's, not the epochs' own"
        );
        assert!(
            !prediction.warnings.contains(&PredictionWarning::NoForecast),
            "{:?}",
            prediction.warnings
        );
    }

    /// AC/RUN-2, AC/RUN-17

    #[test]
    fn a_prediction_due_before_the_forecast_waits_for_it() {
        let (ask_forecast, replies) = held();
        let mut predictor = predictor(10, Some(1), ask_forecast);
        assert_eq!(
            project(
                &mut predictor,
                &[started(), epoch_started(1), settled(1, 1.0)]
            ),
            0,
            "not before the forecast"
        );

        *replies.lock().unwrap() = Some(Some(forecast(250.0)));
        assert_eq!(
            predictor.project(&epoch_started(2)).len(),
            1,
            "on the next item, once it is answered"
        );
        assert_eq!(predictor.prediction().unwrap().intensity.get(), 250.0);
    }

    /// AC/RUN-17

    #[test]
    fn a_prediction_still_due_at_the_outcome_is_made_with_what_there_is() {
        let (ask_forecast, _replies) = held();
        let mut predictor = predictor(10, Some(1), ask_forecast);
        project(
            &mut predictor,
            &[started(), epoch_started(1), settled(1, 1.0)],
        );

        assert_eq!(
            predictor
                .project(&Item::Outcome(Outcome::Completed {
                    epochs_done: Some(1)
                }))
                .len(),
            1
        );
        assert!(
            predictor
                .prediction()
                .unwrap()
                .warnings
                .contains(&PredictionWarning::NoForecast)
        );
    }

    /// AC/RUN-17

    #[test]
    fn only_top_level_epochs_count() {
        let mut predictor = predictor(3, Some(1), answering(forecast(100.0)));
        let nested = Item::Event(TrackerEvent::SpanStarted {
            span: SpanId::new(2),
            name: EPOCH.to_owned(),
            parent: Some(SpanId::new(1)),
            selection: SourceSelection::All,
            at: chrono::Utc::now(),
        });
        assert_eq!(
            project(&mut predictor, &[started(), nested, settled(2, 1.0)]),
            0
        );
        assert_eq!(predictor.measured().count, 0);
    }

    #[test]
    fn the_end_of_the_run_adds_nothing_without_a_prediction_due() {
        let mut predictor = predictor(3, None, answering(forecast(100.0)));
        let items = [
            started(),
            epoch_started(1),
            settled(1, 1.0),
            Item::Outcome(Outcome::Completed {
                epochs_done: Some(1),
            }),
        ];
        assert_eq!(project(&mut predictor, &items), 0);
        assert!(predictor.prediction().is_none());
    }

    #[test]
    fn the_epochs_go_into_the_run_line() {
        let mut header = match started() {
            Item::Started(started) => started.header.clone(),
            _ => unreachable!(),
        };
        Predictor::new(Some(5), Some(1), Some(2), answering(forecast(100.0))).describe(&mut header);
        assert_eq!(
            header.epochs,
            Some(record::Epochs {
                planned: 5,
                monitor: 2,
                before_prediction: 1
            })
        );
    }
}
