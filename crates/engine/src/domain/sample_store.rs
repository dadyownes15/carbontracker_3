//! [SampleStore]: stores every sample each source has reported, read as functions of
//! time with exact `∫f dt` and `∫f·g dt` over any interval.
use std::collections::HashMap;

use crate::SourceDescription;
use crate::domain::units::{GramsCo2e, Joules};
use crate::domain::{SourceId, SourceKind, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Sample {
    observed_at: Timestamp,
    value: f64,
}

impl Sample {
    pub fn new(observed_at: Timestamp, value: f64) -> Self {
        Self { observed_at, value }
    }
    pub fn observed_at(&self) -> &Timestamp {
        &self.observed_at
    }
    pub fn value(&self) -> f64 {
        self.value
    }
}

/// What a source's sample means. The store fills the gaps between samples
/// from it; a provider never chooses how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum SampleKind {
    /// The draw at one moment, in watts: a straight line between samples.
    Instant,
    /// A value holding from its time until the next, e.g. grid intensity.
    Held,
    /// A running total in joules since an origin the provider chooses; the
    /// power across each gap is its slope.
    Total,
}

/// Every sample each source has reported, one series per [SourceId], each
/// kept sorted by `observed_at`.
///
/// Readouts run in parallel and land in any order, so a sample older than a
/// series' newest is inserted where it belongs rather than refused. Between
/// and past its samples a series is filled in as its [SampleKind] says,
/// holding the edge values beyond its ends.
///
/// Each series knows its [SourceKind], so power is only read as energy and
/// intensity only as intensity.
#[derive(Debug)]
pub struct SampleStore {
    series: HashMap<SourceId, Series>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SampleStoreError {
    #[error("unknown source key")]
    UnknownSourceKey,

    /// A sample breaks its kind's rule; nothing of its batch was stored.
    #[error("{0}")]
    Invalid(&'static str),

    #[error("source key registered twice: {0:?}")]
    DuplicateSourceKey(SourceId),
}

impl SampleStore {
    /// A store with one empty series per `(source, kind, sample kind)`.
    /// Samples accumulate, kept in time order however they arrive.
    pub fn new(
        series: impl IntoIterator<Item = (SourceId, SourceKind, SampleKind)>,
        series_capacity: usize,
    ) -> Result<SampleStore, SampleStoreError> {
        // Reject duplicate keys instead of silently overwriting.
        let mut map = HashMap::new();
        for (key, kind, sample_kind) in series {
            let entry = Series {
                kind,
                sample_kind,
                points: Vec::with_capacity(series_capacity),
            };
            if map.insert(key, entry).is_some() {
                return Err(SampleStoreError::DuplicateSourceKey(key));
            }
        }
        Ok(SampleStore { series: map })
    }

    pub fn new_from_source(
        descriptions: &HashMap<SourceId, SourceDescription>,
        series_capacity: usize,
    ) -> Result<SampleStore, SampleStoreError> {
        let series = descriptions
            .iter()
            .map(|(&id, description)| (id, description.kind(), description.sample_kind()));
        SampleStore::new(series, series_capacity)
    }

    /// The energy power source `power` used over `interval`. `None` if it is
    /// not a power source, has no samples to read, or they integrate to
    /// nothing valid.
    pub fn energy(&self, power: SourceId, interval: &TimeInterval) -> Option<Joules> {
        let integral = self.signal(power, interval, is_power)?.integral(interval)?;
        Joules::new(integral).ok()
    }

    /// The emissions of `power`'s energy over `interval`, against grid
    /// intensity series `intensity`. `None` if they are not a power and an
    /// intensity source, or either has no samples to read.
    pub fn emissions(
        &self,
        power: SourceId,
        intensity: SourceId,
        interval: &TimeInterval,
    ) -> Option<GramsCo2e> {
        let grid = self.signal(intensity, interval, is_intensity)?;
        let integral = self
            .signal(power, interval, is_power)?
            .integral_of_product(&grid, interval)?;
        GramsCo2e::from_power_intensity_integral(integral).ok()
    }

    /// The time-weighted mean of intensity series `intensity` over `interval`.
    /// `None` if it is not an intensity source or has no samples to read.
    #[cfg(test)]
    pub fn mean_intensity(
        &self,
        intensity: SourceId,
        interval: &TimeInterval,
    ) -> Option<crate::domain::units::GramsCo2ePerKwh> {
        let integral = self
            .signal(intensity, interval, is_intensity)?
            .integral(interval)?;
        crate::domain::units::GramsCo2ePerKwh::new(integral / interval.duration().as_secs_f64())
            .ok()
    }

    /// How many samples of `source` fall in `[start, end)`, so adjacent
    /// intervals count each sample once.
    pub fn samples_within(&self, source: SourceId, interval: &TimeInterval) -> usize {
        self.series.get(&source).map_or(0, |series| {
            let from = series
                .points
                .partition_point(|sample| sample.observed_at() < interval.start());
            let to = series
                .points
                .partition_point(|sample| sample.observed_at() < interval.end());
            to - from
        })
    }

    /// How many samples are held, across every series.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.series.values().map(|series| series.points.len()).sum()
    }

    /// Adds `samples` to the series.
    pub fn store(&mut self, key: SourceId, samples: Vec<Sample>) -> Result<(), SampleStoreError> {
        let series = self
            .series
            .get_mut(&key)
            .ok_or(SampleStoreError::UnknownSourceKey)?;

        series.check(&samples)?;
        series.insert(samples);
        Ok(())
    }

    /// [SampleStore::delete_before_each] with `cutoff` for every series, or
    /// none.
    #[cfg(test)]
    pub fn delete_before(&mut self, cutoff: Option<&Timestamp>) {
        let cutoffs = cutoff
            .map(|cutoff| self.series.keys().map(|&id| (id, *cutoff)).collect())
            .unwrap_or_default();
        self.delete_before_each(&cutoffs);
    }

    /// Drop each series' samples older than its cutoff in `cutoffs`, keeping
    /// those its kind reads before it; a series with none keeps only its latest.
    pub fn delete_before_each(&mut self, cutoffs: &HashMap<SourceId, Timestamp>) {
        for (source, series) in &mut self.series {
            let beyond = edge_samples(series.sample_kind);
            let points = &mut series.points;
            if points.is_empty() {
                continue;
            }
            let retain_from = match cutoffs.get(source) {
                Some(cutoff) => points.partition_point(|sample| sample.observed_at() < cutoff).saturating_sub(beyond),
                None => points.len() - 1,
            };
            points.drain(..retain_from.min(points.len() - 1));
        }
    }

    /// `source`'s samples over `interval`, plus those either side its edges
    /// are read from. `None` if its kind is not `wanted`, or there are too
    /// few samples to read as its [SampleKind].
    fn signal(
        &self,
        source: SourceId,
        interval: &TimeInterval,
        wanted: fn(SourceKind) -> bool,
    ) -> Option<Signal<'_>> {
        let series = self
            .series
            .get(&source)
            .filter(|series| wanted(series.kind))?;
        let samples = series.window(interval, edge_samples(series.sample_kind));
        let needed = match series.sample_kind {
            SampleKind::Instant | SampleKind::Held => 1,
            SampleKind::Total => 2,
        };
        (samples.len() >= needed).then_some(Signal {
            samples,
            sample_kind: series.sample_kind,
        })
    }
}

/// How many samples beyond an interval its edges are read from: the one
/// either side, or for a counter the gap either side, so a counter holds its
/// edge rate past its ends as other series hold their edge values.
fn edge_samples(sample_kind: SampleKind) -> usize {
    match sample_kind {
        SampleKind::Instant | SampleKind::Held => 1,
        SampleKind::Total => 2,
    }
}

fn is_power(kind: SourceKind) -> bool {
    kind == SourceKind::Power
}

fn is_intensity(kind: SourceKind) -> bool {
    kind == SourceKind::Intensity
}

#[derive(Debug)]
struct Series {
    kind: SourceKind,
    sample_kind: SampleKind,
    points: Vec<Sample>,
}

impl Series {
    /// Refuse `samples` if any breaks its kind's rule: a value that is not
    /// finite, a negative draw or intensity, or a total that goes backwards
    /// against the batch or the totals stored either side.
    fn check(&self, samples: &[Sample]) -> Result<(), SampleStoreError> {
        if samples.iter().any(|sample| !sample.value().is_finite()) {
            return Err(SampleStoreError::Invalid(
                "a value that is not a finite number",
            ));
        }
        match self.sample_kind {
            SampleKind::Instant | SampleKind::Held => {
                if samples.iter().any(|sample| sample.value() < 0.0) {
                    return Err(SampleStoreError::Invalid("a negative value"));
                }
            }
            SampleKind::Total => {
                let mut merged: Vec<(Timestamp, f64)> = self
                    .points
                    .iter()
                    .chain(samples)
                    .map(|sample| (*sample.observed_at(), sample.value()))
                    .collect();
                merged.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                if merged.windows(2).any(|pair| pair[1].1 < pair[0].1) {
                    return Err(SampleStoreError::Invalid("a total that decreases"));
                }
            }
        }
        Ok(())
    }

    /// Put each sample where its time belongs, however late it arrives.
    /// Samples at equal times are ordered by value, so arrival order never
    /// changes the series.
    fn insert(&mut self, samples: Vec<Sample>) {
        for sample in samples {
            let at = self.points.partition_point(|point| {
                (point.observed_at(), point.value()) <= (sample.observed_at(), sample.value())
            });
            self.points.insert(at, sample);
        }
    }

    /// Points in `interval`, both edges inclusive, plus up to `beyond` more
    /// on each side to read its edges from.
    fn window(&self, interval: &TimeInterval, beyond: usize) -> &[Sample] {
        let points = &self.points;
        let start = points.partition_point(|sample| sample.observed_at() < interval.start());
        let end = points.partition_point(|sample| sample.observed_at() <= interval.end());
        &points[start.saturating_sub(beyond)..end.saturating_add(beyond).min(points.len())]
    }
}

/// A non-empty, sorted run of one series' samples, read as a function of time.
#[derive(Debug, Clone, Copy)]
struct Signal<'s> {
    samples: &'s [Sample],
    sample_kind: SampleKind,
}

impl<'s> Signal<'s> {
    /// `∫f dt` over `interval`, with `dt` in seconds.
    fn integral(&self, interval: &TimeInterval) -> Option<f64> {
        finite(
            self.pieces(interval)
                .map(|p| (p.from + p.to) / 2.0 * secs_between(p.start, p.end))
                .sum(),
        )
    }

    /// `∫f·g dt` over `interval`, exact for every pair of readings.
    fn integral_of_product(&self, other: &Signal<'_>, interval: &TimeInterval) -> Option<f64> {
        let mut lhs = self.pieces(interval).peekable();
        let mut rhs = other.pieces(interval).peekable();
        let mut cursor = *interval.start();
        let mut total = 0.0;

        while let (Some(&f), Some(&g)) = (lhs.peek(), rhs.peek()) {
            let end = f.end.min(g.end);
            let width = secs_between(cursor, end);
            if width > 0.0 {
                let (f0, f1, g0, g1) = (f.at(cursor), f.at(end), g.at(cursor), g.at(end));
                total += width / 6.0 * (2.0 * f0 * g0 + f0 * g1 + f1 * g0 + 2.0 * f1 * g1);
            }
            cursor = end;
            if f.end == end {
                lhs.next();
            }
            if g.end == end {
                rhs.next();
            }
        }

        finite(total)
    }

    /// Linear pieces covering `interval` in order, holding the edge values
    /// before the first sample and after the last.
    fn pieces(&self, interval: &TimeInterval) -> impl Iterator<Item = Piece> + use<'s> {
        let (start, end) = (*interval.start(), *interval.end());
        let Signal {
            samples,
            sample_kind,
        } = *self;

        let (first, last) = (samples[0], samples[samples.len() - 1]);
        let (first_value, last_value) = edge_values(samples, sample_kind);

        let head = (start < *first.observed_at()).then(|| Piece {
            start,
            end: end.min(*first.observed_at()),
            from: first_value,
            to: first_value,
        });

        let tail = (end > *last.observed_at()).then(|| Piece {
            start: start.max(*last.observed_at()),
            end,
            from: last_value,
            to: last_value,
        });

        let gaps = samples.windows(2).filter_map(move |pair| {
            let (a, b) = (pair[0], pair[1]);
            let (from, to) = (start.max(*a.observed_at()), end.min(*b.observed_at()));
            if from >= to {
                return None;
            }
            let (at_from, at_to) = match sample_kind {
                SampleKind::Instant => {
                    let line = Piece {
                        start: *a.observed_at(),
                        end: *b.observed_at(),
                        from: a.value(),
                        to: b.value(),
                    };
                    (line.at(from), line.at(to))
                }
                SampleKind::Held => (a.value(), a.value()),
                SampleKind::Total => {
                    let r = gap_rate(&a, &b);
                    (r, r)
                }
            };
            Some(Piece {
                start: from,
                end: to,
                from: at_from,
                to: at_to,
            })
        });

        head.into_iter().chain(gaps).chain(tail)
    }
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    start: Timestamp,
    end: Timestamp,
    from: f64,
    to: f64,
}

impl Piece {
    fn at(&self, t: Timestamp) -> f64 {
        let width = secs_between(self.start, self.end);
        if width <= 0.0 {
            return self.from;
        }
        self.from + secs_between(self.start, t) / width * (self.to - self.from)
    }
}

/// The values a signal holds before its first sample and after its last.
fn edge_values(samples: &[Sample], sample_kind: SampleKind) -> (f64, f64) {
    let (first, last) = (&samples[0], &samples[samples.len() - 1]);
    match sample_kind {
        SampleKind::Instant | SampleKind::Held => (first.value(), last.value()),
        SampleKind::Total => (
            gap_rate(&samples[0], &samples[1]),
            gap_rate(&samples[samples.len() - 2], last),
        ),
    }
}

fn gap_rate(a: &Sample, b: &Sample) -> f64 {
    let width = secs_between(*a.observed_at(), *b.observed_at());
    if width > 0.0 {
        (b.value() - a.value()) / width
    } else {
        0.0
    }
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

/// Seconds at nanosecond resolution, the clock's own: truncating each piece
/// to a coarser unit loses time wherever a reading splits one. Falls back to
/// microseconds past ~292 years.
fn secs_between(from: Timestamp, to: Timestamp) -> f64 {
    let delta = to - from;
    match (delta.num_nanoseconds(), delta.num_microseconds()) {
        (Some(nanoseconds), _) => nanoseconds as f64 * 1e-9,
        (None, Some(microseconds)) => microseconds as f64 * 1e-6,
        (None, None) => delta.num_milliseconds() as f64 * 1e-3,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimeInterval {
    start: Timestamp,
    end: Timestamp,
}

impl TimeInterval {
    pub fn new(start: Timestamp, end: Timestamp) -> Result<Self, TimeIntervalError> {
        if end <= start {
            return Err(TimeIntervalError::EndNotAfterStart { start, end });
        }

        Ok(Self { start, end })
    }

    pub fn start(&self) -> &Timestamp {
        &self.start
    }

    pub fn end(&self) -> &Timestamp {
        &self.end
    }

    #[cfg(test)]
    pub fn duration(&self) -> std::time::Duration {
        self.end
            .signed_duration_since(self.start)
            .to_std()
            .expect("validated interval must have a nonnegative duration")
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TimeIntervalError {
    #[error("interval must have end after start: {start}..{end}")]
    EndNotAfterStart { start: Timestamp, end: Timestamp },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Fidelity, GridLocation, PowerDomain, PowerScope, ProviderId, SourceCatalog,
    };

    /// `secs` seconds after the epoch.
    fn t(secs: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(secs)
    }

    fn cpu() -> SourceId {
        SourceId::new(ProviderId(1), 0)
    }

    fn gpu() -> SourceId {
        SourceId::new(ProviderId(1), 1)
    }

    /// [cpu] and [gpu], read linearly, with no intensity series.
    fn catalog() -> SourceCatalog {
        let power = |domain| SourceDescription::Power {
            device_name: format!("{domain:?}"),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Stated,
        };
        SourceCatalog {
            descriptions: HashMap::from([
                (cpu(), power(PowerDomain::Cpu)),
                (gpu(), power(PowerDomain::Gpu)),
            ]),
            ..SourceCatalog::default()
        }
    }

    /// Empty, with one series per source in `catalog`.
    fn sample_store(catalog: &SourceCatalog) -> SampleStore {
        SampleStore::new_from_source(&catalog.descriptions, 64)
            .expect("the keys of a map are distinct")
    }

    fn interval(from: i64, to: i64) -> TimeInterval {
        TimeInterval::new(t(from), t(to)).unwrap()
    }

    /// One counter series, [cpu].
    fn counter_store() -> SampleStore {
        SampleStore::new([(cpu(), SourceKind::Power, SampleKind::Total)], 16).unwrap()
    }

    fn refused(result: Result<(), SampleStoreError>) -> bool {
        matches!(result, Err(SampleStoreError::Invalid(_)))
    }

    /// AC/ENG-52, AC/MEAS-70
    #[test]
    fn a_draw_that_is_not_finite_or_is_negative_is_refused_with_its_batch() {
        let mut store = sample_store(&catalog());
        for bad in [f64::NAN, f64::INFINITY, -1.0] {
            assert!(
                refused(store.store(cpu(), vec![Sample::new(t(0), 10.0), Sample::new(t(1), bad)])),
                "{bad}"
            );
        }
        assert_eq!(
            store.samples_within(cpu(), &interval(0, 10)),
            0,
            "nothing of a refused batch is stored"
        );
    }

    /// AC/ENG-53, AC/MEAS-70
    #[test]
    fn a_total_that_goes_backwards_is_refused() {
        let mut store = counter_store();
        store
            .store(
                cpu(),
                vec![Sample::new(t(0), 100.0), Sample::new(t(10), 200.0)],
            )
            .unwrap();

        assert!(refused(store.store(
            cpu(),
            vec![Sample::new(t(20), 300.0), Sample::new(t(30), 250.0)]
        )));
        assert!(
            refused(store.store(cpu(), vec![Sample::new(t(5), 250.0)])),
            "above the total after it"
        );
        assert!(
            refused(store.store(cpu(), vec![Sample::new(t(20), 150.0)])),
            "below the total before it"
        );
        store
            .store(cpu(), vec![Sample::new(t(5), 150.0)])
            .expect("a late total between its neighbours");
        assert_eq!(store.samples_within(cpu(), &interval(0, 40)), 3);
    }

    /// AC/ENG-53
    #[test]
    fn a_total_is_exact_across_any_gap() {
        let mut store = counter_store();
        store
            .store(
                cpu(),
                vec![
                    Sample::new(t(0), 0.0),
                    Sample::new(t(10), 100.0),
                    Sample::new(t(3610), 36100.0),
                ],
            )
            .unwrap();
        assert_eq!(
            store.energy(cpu(), &interval(0, 3610)).unwrap().get(),
            36100.0
        );
    }

    /// AC/MEAS-68
    #[test]
    fn a_late_sample_is_inserted_where_it_belongs() {
        let mut sample_store = sample_store(&catalog());
        sample_store
            .store(cpu(), vec![Sample::new(t(11), 20.0)])
            .unwrap();
        sample_store
            .store(cpu(), vec![Sample::new(t(1), 0.0)])
            .unwrap();
        sample_store
            .store(cpu(), vec![Sample::new(t(6), 10.0)])
            .unwrap();

        // 0 W ramping to 20 W over 10 s is 100 J.
        assert_eq!(
            sample_store.energy(cpu(), &interval(1, 11)).unwrap().get(),
            100.0
        );
        // The sample at 11 s starts the next interval.
        assert_eq!(sample_store.samples_within(cpu(), &interval(1, 11)), 2);
    }

    /// AC/MEAS-75
    #[test]
    fn a_series_with_no_samples_answers_nothing() {
        let sample_store = sample_store(&catalog());
        assert_eq!(sample_store.energy(cpu(), &interval(1, 11)), None);
        assert_eq!(sample_store.samples_within(cpu(), &interval(1, 11)), 0);
    }

    /// AC/ENG-51, AC/MEAS-71, AC/MEAS-74
    #[test]
    fn sampled_power_reads_as_a_line_and_holds_its_edge_values_past_the_ends() {
        let mut sample_store = sample_store(&catalog());
        sample_store
            .store(
                cpu(),
                vec![Sample::new(t(10), 10.0), Sample::new(t(20), 20.0)],
            )
            .unwrap();

        // 10 W for 5 s before the first sample, 10 W ramping to 20 W over
        // 10 s, then 20 W for 5 s after the last.
        assert_eq!(
            sample_store.energy(cpu(), &interval(5, 25)).unwrap().get(),
            50.0 + 150.0 + 100.0
        );
    }

    /// AC/ENG-51, AC/MEAS-73, AC/MEAS-74
    #[test]
    fn a_cumulative_counter_reads_as_its_slope_and_holds_it_past_the_ends() {
        let mut sample_store =
            SampleStore::new([(cpu(), SourceKind::Power, SampleKind::Total)], 4).unwrap();
        sample_store
            .store(
                cpu(),
                vec![Sample::new(t(10), 0.0), Sample::new(t(20), 50.0)],
            )
            .unwrap();

        // 5 W throughout, held past both ends: 5 W over 20 s.
        assert_eq!(
            sample_store.energy(cpu(), &interval(5, 25)).unwrap().get(),
            100.0
        );
    }

    /// AC/MEAS-72, AC/MEAS-77
    #[test]
    fn emissions_integrate_power_against_intensity() {
        let mut sample_store = SampleStore::new(
            [
                (cpu(), SourceKind::Power, SampleKind::Instant),
                (gpu(), SourceKind::Intensity, SampleKind::Held),
            ],
            4,
        )
        .unwrap();
        sample_store
            .store(
                cpu(),
                vec![Sample::new(t(0), 1000.0), Sample::new(t(3600), 1000.0)],
            )
            .unwrap();
        sample_store
            .store(gpu(), vec![Sample::new(t(0), 100.0)])
            .unwrap();

        // 1 kWh at 100 g/kWh.
        let grams = sample_store
            .emissions(cpu(), gpu(), &interval(0, 3600))
            .unwrap()
            .get();
        assert!((grams - 100.0).abs() < 1e-9, "got {grams}");
    }

    /// AC/MEAS-77, AC/MEAS-78
    #[test]
    fn readings_between_microseconds_lose_no_time() {
        let ns = |nanos| chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::nanoseconds(nanos);
        let mut sample_store = SampleStore::new(
            [
                (cpu(), SourceKind::Power, SampleKind::Instant),
                (gpu(), SourceKind::Intensity, SampleKind::Held),
            ],
            4,
        )
        .unwrap();
        sample_store
            .store(cpu(), vec![Sample::new(ns(1_500), 10.0)])
            .unwrap();
        sample_store
            .store(gpu(), vec![Sample::new(ns(3_100), 100.0)])
            .unwrap();
        let interval = TimeInterval::new(ns(0), ns(4_700)).unwrap();

        // 10 W over 4.7 µs, all of it at 100 g/kWh.
        let joules = sample_store.energy(cpu(), &interval).unwrap().get();
        let grams = sample_store
            .emissions(cpu(), gpu(), &interval)
            .unwrap()
            .get();
        assert!((joules - 4.7e-5).abs() < 1e-15, "got {joules}");
        assert!(
            (grams - joules / 3_600_000.0 * 100.0).abs() < 1e-20,
            "got {grams}"
        );
    }

    fn grid() -> SourceId {
        SourceId::new(ProviderId(1), 2)
    }

    /// [cpu] and [grid], each sampled at 0 s and 10 s, built from their
    /// descriptions so the store knows which is power and which intensity.
    fn power_and_grid() -> SampleStore {
        let mut descriptions = catalog().descriptions;
        descriptions.insert(
            grid(),
            SourceDescription::Intensity {
                location: GridLocation::Global,
                fidelity: Fidelity::Stated,
            },
        );
        let mut sample_store = SampleStore::new_from_source(&descriptions, 4).unwrap();
        for source in [cpu(), grid()] {
            sample_store
                .store(
                    source,
                    vec![Sample::new(t(0), 100.0), Sample::new(t(10), 100.0)],
                )
                .unwrap();
        }
        sample_store
    }

    #[test]
    fn a_power_source_has_no_mean_intensity() {
        let sample_store = power_and_grid();
        assert!(
            sample_store
                .mean_intensity(grid(), &interval(0, 10))
                .is_some()
        );
        assert_eq!(sample_store.mean_intensity(cpu(), &interval(0, 10)), None);
    }

    #[test]
    fn an_intensity_source_has_no_energy() {
        let sample_store = power_and_grid();
        assert!(sample_store.energy(cpu(), &interval(0, 10)).is_some());
        assert_eq!(sample_store.energy(grid(), &interval(0, 10)), None);
    }

    #[test]
    fn emissions_need_a_power_source_against_an_intensity_source() {
        let sample_store = power_and_grid();
        assert!(
            sample_store
                .emissions(cpu(), grid(), &interval(0, 10))
                .is_some()
        );
        assert_eq!(
            sample_store.emissions(grid(), cpu(), &interval(0, 10)),
            None,
            "swapped"
        );
        assert_eq!(
            sample_store.emissions(cpu(), cpu(), &interval(0, 10)),
            None,
            "power against power"
        );
        assert_eq!(
            sample_store.emissions(grid(), grid(), &interval(0, 10)),
            None,
            "intensity against intensity"
        );
    }

    /// AC/ENG-30, AC/MEAS-86, AC/MEAS-87
    #[test]
    fn each_series_is_pruned_at_its_own_cutoff_keeping_what_its_kind_reads_before_it() {
        let mut store = SampleStore::new(
            [
                (SourceId::for_test(1, 0), SourceKind::Power, SampleKind::Instant),
                (SourceId::for_test(2, 0), SourceKind::Power, SampleKind::Total),
                (SourceId::for_test(3, 0), SourceKind::Intensity, SampleKind::Held),
            ],
            16,
        )
        .unwrap();
        let at = |s: i64| chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::seconds(s);
        for id in [SourceId::for_test(1, 0), SourceId::for_test(2, 0), SourceId::for_test(3, 0)] {
            store.store(id, (1..=10).map(|s| Sample::new(at(s), s as f64)).collect()).unwrap();
        }
        let cutoffs = HashMap::from([(SourceId::for_test(1, 0), at(6)), (SourceId::for_test(2, 0), at(6))]);
        store.delete_before_each(&cutoffs);
        let first = |id| store.series[&id].points.first().unwrap().observed_at().to_owned();
        assert_eq!(first(SourceId::for_test(1, 0)), at(5), "one before the cutoff for a rate");
        assert_eq!(first(SourceId::for_test(2, 0)), at(4), "two before it for a total");
        assert_eq!(store.series[&SourceId::for_test(3, 0)].points.len(), 1, "with no cutoff only the latest is kept");
    }

    /// AC/ENG-54, AC/MEAS-70
    #[test]
    fn an_intensity_that_is_not_finite_or_is_negative_is_refused_with_its_batch() {
        let mut store = SampleStore::new([(grid(), SourceKind::Intensity, SampleKind::Held)], 4).unwrap();
        for bad in [f64::NAN, f64::NEG_INFINITY, -1.0] {
            assert!(refused(store.store(grid(), vec![Sample::new(t(0), 100.0), Sample::new(t(1), bad)])), "{bad}");
        }
        assert_eq!(store.samples_within(grid(), &interval(0, 10)), 0);
        store.store(grid(), vec![Sample::new(t(0), 0.0)]).expect("zero is a valid intensity");
    }
}
