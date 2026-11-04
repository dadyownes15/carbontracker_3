//! What a whole run is expected to cost, from the spans already measured.
//!
//! Rebuilt from the engine's parked prediction (`docs/parked/prediction/`) on
//! what the engine streams: settled span reports and the forecast curve.

use std::fmt;
use std::time::Duration;

use carbontracker::{GramsCo2e, GramsCo2ePerKwh, Joules, Sample, Timestamp, Usage};

/// The shortest stretch the forecast is averaged over.
const MIN_WINDOW: Duration = Duration::from_secs(60);

/// What a run is expected to cost. Apart from a span report, so a
/// prediction can't be mistaken for a measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct Prediction {
    /// Energy the whole run is expected to use, measured part included.
    pub energy: Joules,
    /// That energy at the mean intensity over the remaining time.
    pub emissions: GramsCo2e,
    /// How long the whole run is expected to take.
    pub duration: Duration,
    /// `duration` less what the measured spans took.
    pub remaining: Duration,
    /// Mean intensity over the remaining time.
    pub intensity: GramsCo2ePerKwh,
    /// How many measured spans were averaged over.
    pub measured: usize,
    /// The multiplier, e.g. the number of epochs.
    pub total: f64,
    pub warnings: Vec<PredictionWarning>,
}

/// Why a prediction might be wrong, beyond being a prediction.
#[derive(Debug, Clone, PartialEq)]
pub enum PredictionWarning {
    /// Nothing was measured to extrapolate from.
    NothingMeasured,
    /// Extrapolated from one span, so variance between spans is invisible.
    ThinBasis { measured: usize },
    /// The forecast is the current intensity held flat.
    ProjectedIntensity,
    /// The forecast covers only this fraction of the remaining time; the rest
    /// is filled with the curve's mean.
    BeyondForecast { covered: f64 },
    /// No forecast at all: the measured spans' own intensity is held forward.
    NoForecast,
}

impl fmt::Display for PredictionWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NothingMeasured => {
                formatter.write_str("no epoch was measured, so this prediction rests on no data")
            }
            Self::ThinBasis { measured } => write!(
                formatter,
                "predicted from {measured} epoch, so it can't see how much epochs vary; epochs_before_pred=3 gives a \
                 steadier estimate"
            ),
            Self::ProjectedIntensity => formatter.write_str(
                "no forecast is published for this grid, so the current intensity is held forward",
            ),
            Self::BeyondForecast { covered } => {
                write!(
                    formatter,
                    "the forecast covers {:.0} % of the remaining time; the rest uses its mean",
                    covered * 100.0
                )
            }
            Self::NoForecast => formatter.write_str(
                "no forecast resolved, so the measured epochs' intensity is held forward",
            ),
        }
    }
}

/// The spans measured so far, summed: how many, what they used, how long they
/// took. A running total, so it stays this size however long the run.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Measured {
    pub count: usize,
    pub usage: Usage,
    pub duration: Duration,
}

/// The `measured` spans scaled up to `total` of them, with emissions at the
/// forecast's mean intensity over the time still to run from `now`.
/// `projected` says the forecast is the current intensity held flat.
pub fn predict(
    measured: &Measured,
    total: f64,
    forecast: Option<&[Sample]>,
    projected: bool,
    now: Timestamp,
) -> Prediction {
    let mut warnings = Vec::new();
    match measured.count {
        0 => warnings.push(PredictionWarning::NothingMeasured),
        1 => warnings.push(PredictionWarning::ThinBasis { measured: 1 }),
        _ => {}
    }

    let count = measured.count.max(1) as f64;
    let total = total.max(0.0);
    let energy = measured.usage.energy.get();
    let elapsed = measured.duration;
    let duration =
        Duration::try_from_secs_f64(elapsed.as_secs_f64() / count * total).unwrap_or_default();
    let remaining = duration.saturating_sub(elapsed);
    let energy = Joules::new(energy / count * total).unwrap_or_default();

    let window = remaining.max(MIN_WINDOW);
    let intensity = match forecast.and_then(|curve| mean_over(curve, now, window)) {
        Some((mean, covered)) => {
            if projected {
                warnings.push(PredictionWarning::ProjectedIntensity);
            }
            if covered < 1.0 - 1e-9 {
                warnings.push(PredictionWarning::BeyondForecast { covered });
            }
            mean
        }
        None => {
            warnings.push(PredictionWarning::NoForecast);
            measured_intensity(measured)
        }
    };
    let intensity = GramsCo2ePerKwh::new(intensity).unwrap_or(GramsCo2ePerKwh::ZERO);

    Prediction {
        energy,
        emissions: energy
            .to_kilowatt_hours()
            .emissions_at(intensity)
            .unwrap_or_default(),
        duration,
        remaining,
        intensity,
        measured: measured.count,
        total,
        warnings,
    }
}

/// The curve's mean over `[from, from + window)`, each point held until the
/// next, and the fraction of the window the curve covers. Outside the curve
/// the curve's own mean is used. `None` for an empty curve.
fn mean_over(curve: &[Sample], from: Timestamp, window: Duration) -> Option<(f64, f64)> {
    let mut points: Vec<(f64, f64)> = curve
        .iter()
        .map(|sample| (seconds(sample.observed_at()), sample.value()))
        .collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    let (first, last) = (points.first()?.0, points.last()?.0);

    // The curve's own mean, over the time it spans.
    let pieces = points
        .windows(2)
        .map(|pair| (pair[0].0, pair[1].0, pair[0].1));
    let span = last - first;
    let curve_mean = if span > 0.0 {
        pieces
            .clone()
            .map(|(start, end, value)| (end - start) * value)
            .sum::<f64>()
            / span
    } else {
        points[0].1
    };

    let (from, to) = (seconds(&from), seconds(&from) + window.as_secs_f64());
    let overlap = |start: f64, end: f64| (end.min(to) - start.max(from)).max(0.0);
    let covered_area: f64 = pieces
        .map(|(start, end, value)| overlap(start, end) * value)
        .sum();
    let covered = overlap(first, last);
    let length = to - from;

    Some((
        (covered_area + (length - covered) * curve_mean) / length,
        covered / length,
    ))
}

/// The measured spans' own grams per kWh; zero if they used nothing or
/// had no intensity to bill it against.
fn measured_intensity(measured: &Measured) -> f64 {
    let kwh = measured.usage.energy.to_kilowatt_hours().get();
    match measured.usage.emissions {
        Some(grams) if kwh > 0.0 => grams.get() / kwh,
        _ => 0.0,
    }
}

fn seconds(at: &Timestamp) -> f64 {
    at.timestamp_micros() as f64 / 1e6
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta};

    use super::*;

    fn t(secs: i64) -> Timestamp {
        DateTime::UNIX_EPOCH + TimeDelta::seconds(secs)
    }

    fn span(kwh: f64, grams: f64, secs: u64) -> Measured {
        Measured {
            count: 1,
            usage: Usage {
                energy: Joules::new(kwh * 3_600_000.0).unwrap(),
                emissions: Some(GramsCo2e::new(grams).unwrap()),
            },
            duration: Duration::from_secs(secs),
        }
    }

    /// The running total of `spans`, as the binding keeps it.
    fn sum(spans: &[Measured]) -> Measured {
        spans.iter().fold(Measured::default(), |mut total, span| {
            total.count += span.count;
            total.usage += span.usage;
            total.duration += span.duration;
            total
        })
    }

    fn flat(value: f64, from: i64, to: i64) -> Vec<Sample> {
        vec![Sample::new(t(from), value), Sample::new(t(to), value)]
    }

    #[test]
    fn the_mean_span_is_scaled_to_the_whole_run() {
        let spans = [span(1.0, 100.0, 60), span(3.0, 300.0, 180)];
        let curve = flat(200.0, 0, 100_000);
        let prediction = predict(&sum(&spans), 10.0, Some(&curve), false, t(0));

        assert_eq!(
            prediction.energy.to_kilowatt_hours().get(),
            20.0,
            "2 kWh a span, ten spans"
        );
        assert_eq!(prediction.duration, Duration::from_secs(1200));
        assert_eq!(
            prediction.remaining,
            Duration::from_secs(960),
            "less the 240 s measured"
        );
        assert_eq!(prediction.intensity.get(), 200.0);
        assert_eq!(prediction.emissions.get(), 4000.0);
        assert!(prediction.warnings.is_empty(), "{:?}", prediction.warnings);
    }

    #[test]
    fn thin_or_missing_measurements_say_so() {
        let curve = flat(200.0, 0, 100_000);
        let one = predict(&sum(&[span(1.0, 1.0, 60)]), 5.0, Some(&curve), false, t(0));
        assert_eq!(one.warnings, [PredictionWarning::ThinBasis { measured: 1 }]);

        let none = predict(&Measured::default(), 5.0, Some(&curve), false, t(0));
        assert_eq!(none.warnings, [PredictionWarning::NothingMeasured]);
        assert_eq!(none.energy, Joules::ZERO);
    }

    #[test]
    fn the_intensity_is_the_curves_mean_over_the_remaining_time() {
        // 100 g for the first 500 s, 300 g for the next 500 s.
        let curve = vec![
            Sample::new(t(0), 100.0),
            Sample::new(t(500), 300.0),
            Sample::new(t(1000), 300.0),
        ];
        let spans = [span(1.0, 1.0, 100), span(1.0, 1.0, 100)];
        // Ten spans of 100 s: 1000 s in all, 800 s still to run.
        let prediction = predict(&sum(&spans), 10.0, Some(&curve), false, t(0));
        let expected = (500.0 * 100.0 + 300.0 * 300.0) / 800.0;
        assert!(
            (prediction.intensity.get() - expected).abs() < 1e-9,
            "{}",
            prediction.intensity.get()
        );
    }

    #[test]
    fn past_the_curve_the_rest_uses_its_mean() {
        // Covers 300 of the 600 s still to run, at 100 g; its mean is 100 g.
        let curve = vec![Sample::new(t(0), 100.0), Sample::new(t(300), 500.0)];
        let spans = [span(1.0, 1.0, 100), span(1.0, 1.0, 100)];
        let prediction = predict(&sum(&spans), 8.0, Some(&curve), false, t(0));

        assert_eq!(prediction.remaining, Duration::from_secs(600));
        assert!((prediction.intensity.get() - 100.0).abs() < 1e-9);
        assert_eq!(
            prediction.warnings,
            [PredictionWarning::BeyondForecast { covered: 0.5 }]
        );
    }

    #[test]
    fn a_projection_or_no_forecast_is_named() {
        let spans = [span(2.0, 300.0, 60), span(2.0, 300.0, 60)];
        let projected = predict(
            &sum(&spans),
            4.0,
            Some(&flat(150.0, 0, 100_000)),
            true,
            t(0),
        );
        assert_eq!(projected.warnings, [PredictionWarning::ProjectedIntensity]);

        let none = predict(&sum(&spans), 4.0, None, false, t(0));
        assert_eq!(none.warnings, [PredictionWarning::NoForecast]);
        assert_eq!(
            none.intensity.get(),
            150.0,
            "the measured spans' own intensity"
        );
    }

    #[test]
    fn warnings_speak_of_epochs() {
        let thin = PredictionWarning::ThinBasis { measured: 1 }.to_string();
        assert!(
            thin.starts_with("predicted from 1 epoch,") && thin.contains("epochs_before_pred=3"),
            "{thin}"
        );
    }

    #[test]
    fn at_least_a_minute_of_forecast_is_averaged() {
        // Nothing remains, so the next minute stands in: 0..30 s at 100 g,
        // then the curve's mean (100 g).
        let spans = [span(1.0, 1.0, 60), span(1.0, 1.0, 60)];
        let prediction = predict(&sum(&spans), 2.0, Some(&flat(100.0, 0, 30)), false, t(0));
        assert_eq!(prediction.remaining, Duration::ZERO);
        assert_eq!(
            prediction.warnings,
            [PredictionWarning::BeyondForecast { covered: 0.5 }]
        );
    }
}
