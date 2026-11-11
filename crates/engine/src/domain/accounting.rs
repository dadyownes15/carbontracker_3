//! Accounting a finished segment: measure what each source used over it, then
//! bill it to spans. Pure functions; the [Session](super::Session) adds up
//! what they return.

use std::collections::HashMap;

use super::sample_store::{SampleStore, TimeInterval};
use super::timeline::Segment;
use super::units::{GramsCo2e, Joules};
use super::{SourceCatalog, SourceId, SpanId};

/// Energy and the emissions it caused, for one source or summed over several.
/// Emissions are absent when energy was drawn with no intensity to bill it by.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Usage {
    pub energy: Joules,
    pub emissions: Option<GramsCo2e>,
}

impl Usage {
    /// Nothing used, with present zero emissions.
    pub const ZERO: Self = Self { energy: Joules::ZERO, emissions: Some(GramsCo2e::ZERO) };
}

impl Default for Usage {
    fn default() -> Self {
        Self::ZERO
    }
}

impl std::ops::AddAssign for Usage {
    /// Absent emissions make the sum absent only if they came with energy.
    fn add_assign(&mut self, other: Self) {
        let absent = |usage: &Self| usage.emissions.is_none() && usage.energy.get() > 0.0;
        self.emissions = if absent(self) || absent(&other) {
            None
        } else {
            let grams = |usage: &Self| usage.emissions.unwrap_or_default();
            Some(grams(self) + grams(&other))
        };
        self.energy += other.energy;
    }
}

impl std::ops::Mul<f64> for Usage {
    type Output = Self;

    fn mul(self, factor: f64) -> Self {
        Self {
            energy: self.energy * factor,
            emissions: self.emissions.map(|grams| grams * factor),
        }
    }
}

/// How much the whole facility draws per watt the machine draws: cooling,
/// power conversion and the like. A fixed multiplier on every power source's
/// energy and emissions. 1.0 means no overhead.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PowerUsageEffectiveness(f64);

impl PowerUsageEffectiveness {
    /// Uptime Institute's 2023 global average (Daniel Bizo, 2023, Global Data
    /// Center Survey).
    pub const UPTIME_2023: Self = Self(1.58);

    /// `None` unless `factor` is finite and at least 1: a facility never
    /// draws less than the machine in it.
    pub fn new(factor: f64) -> Option<Self> {
        (factor.is_finite() && factor >= 1.0).then_some(Self(factor))
    }

    pub const fn get(&self) -> f64 {
        self.0
    }
}

impl Default for PowerUsageEffectiveness {
    fn default() -> Self {
        Self(1.0)
    }
}

/// What `segment`'s source used over it: its energy, and its emissions against
/// its own intensity. Emissions are absent with no paired intensity, or none
/// with a sample to read. A zero-width segment uses nothing.
pub fn measure(catalog: &SourceCatalog, sample_store: &SampleStore, segment: &Segment) -> Usage {
    let intensity = catalog.intensity_of(segment.source);
    let Some(interval) = TimeInterval::new(segment.from, segment.to).ok().filter(|_| segment.from < segment.to) else {
        return Usage { energy: Joules::ZERO, emissions: intensity.map(|_| GramsCo2e::ZERO) };
    };
    Usage {
        energy: sample_store.energy(segment.source, &interval).unwrap_or_default(),
        emissions: intensity.and_then(|grid| sample_store.emissions(segment.source, grid, &interval)),
    }
}

/// Each claimant's share of `segment`: even, summing to one.
pub fn share(segment: &Segment) -> Vec<(SpanId, f64)> {
    let share = 1.0 / segment.claimants.len() as f64;
    segment.claimants.iter().map(|&span| (span, share)).collect()
}

/// What each claimant is billed for `segment`: its share of the usage, scaled
/// by `pue`.
pub fn bill(
    catalog: &SourceCatalog,
    sample_store: &SampleStore,
    segment: &Segment,
    pue: PowerUsageEffectiveness,
) -> Vec<(SpanId, Usage)> {
    if segment.from < segment.to
        && TimeInterval::new(segment.from, segment.to).is_ok_and(|interval| sample_store.energy(segment.source, &interval).is_none())
    {
        return Vec::new();
    }
    let whole = measure(catalog, sample_store, segment) * pue.get();
    share(segment).into_iter().map(|(span, share)| (span, whole * share)).collect()
}

/// How many samples `segment`'s source and its intensity have in `[from, to)`,
/// so adjacent segments count each sample once.
pub fn count_samples(
    catalog: &SourceCatalog,
    sample_store: &SampleStore,
    segment: &Segment,
) -> HashMap<SourceId, usize> {
    let Ok(interval) = TimeInterval::new(segment.from, segment.to) else {
        return HashMap::new();
    };
    std::iter::once(segment.source)
        .chain(catalog.intensity_of(segment.source))
        .map(|id| (id, sample_store.samples_within(id, &interval)))
        .collect()
}

#[cfg(test)]
mod segment_tests {
    use super::*;
    use crate::domain::{
        Fidelity, GridLocation, PowerDomain, PowerScope, ProviderId, Sample, SampleKind, SourceDescription,
        Timestamp,
    };

    /// `ms` milliseconds after the epoch.
    fn at(ms: i64) -> Timestamp {
        chrono::DateTime::UNIX_EPOCH + chrono::TimeDelta::milliseconds(ms)
    }

    fn cpu() -> SourceId { SourceId::new(ProviderId(1), 0) }
    fn gpu() -> SourceId { SourceId::new(ProviderId(2), 0) }
    fn ram() -> SourceId { SourceId::new(ProviderId(1), 1) }
    fn dk1() -> SourceId { SourceId::new(ProviderId(3), 0) }
    fn se3() -> SourceId { SourceId::new(ProviderId(4), 0) }

    fn span(n: usize) -> SpanId { SpanId::new(n) }

    /// CPU and RAM read as rates, the GPU as a counter; CPU on `dk1`, GPU on
    /// `se3`, RAM unpaired.
    fn catalog() -> SourceCatalog {
        let power = |domain, sample_kind| SourceDescription::Power {
            device_name: format!("{domain:?}"),
            domain,
            scope: PowerScope::DeviceTotal,
            sample_kind,
            fidelity: Fidelity::Stated,
        };
        let grid = SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Stated };
        SourceCatalog {
            descriptions: HashMap::from([
                (cpu(), power(PowerDomain::Cpu, SampleKind::Instant)),
                (gpu(), power(PowerDomain::Gpu, SampleKind::Total)),
                (ram(), power(PowerDomain::Ram, SampleKind::Instant)),
                (dk1(), grid.clone()),
                (se3(), grid),
            ]),
            pairs: HashMap::from([(cpu(), dk1()), (gpu(), se3())]),
        }
    }

    fn store(series: &[(SourceId, &[(i64, f64)])]) -> SampleStore {
        let mut store = SampleStore::new_from_source(&catalog().descriptions, 64).unwrap();
        for &(source, samples) in series {
            store.store(source, samples.iter().map(|&(ms, value)| Sample::new(at(ms), value)).collect()).unwrap();
        }
        store
    }

    /// CPU 10 W → 30 W over 0–2 s, RAM 5 W, GPU 100 W as a counter, both
    /// grids steady.
    fn steady() -> SampleStore {
        store(&[
            (cpu(), &[(0, 10.0), (2000, 30.0)]),
            (ram(), &[(0, 5.0), (2000, 5.0)]),
            (gpu(), &[(0, 0.0), (2000, 200.0)]),
            (dk1(), &[(0, 100.0)]),
            (se3(), &[(0, 40.0)]),
        ])
    }

    fn segment(source: SourceId, from: i64, to: i64, claimants: &[usize]) -> Segment {
        let claimants: Vec<SpanId> = claimants.iter().map(|&n| span(n)).collect();
        Segment { source, from: at(from), to: at(to), active: claimants.clone(), claimants }
    }

    fn pue(factor: f64) -> PowerUsageEffectiveness {
        PowerUsageEffectiveness::new(factor).unwrap()
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1.0)
    }

    fn grams(usage: Usage) -> f64 {
        usage.emissions.expect("emissions present").get()
    }

    /// AC/PROF-22, AC/PROF-24, AC/TST-3
    #[test]
    fn shares_are_even_sum_to_one_and_name_only_claimants() {
        let shares = share(&segment(cpu(), 0, 1000, &[1, 2, 3]));
        assert_eq!(shares.iter().map(|&(s, _)| s).collect::<Vec<_>>(), vec![span(1), span(2), span(3)]);
        assert!(shares.iter().all(|&(_, share)| close(share, 1.0 / 3.0)));
        assert!(close(shares.iter().map(|&(_, share)| share).sum(), 1.0));
    }

    /// AC/ENG-2, AC/PROF-25, AC/PROF-30, AC/TST-4
    #[test]
    fn a_segments_bills_sum_to_its_usage_times_the_pue() {
        let (catalog, store) = (catalog(), steady());
        for source in [cpu(), gpu(), ram()] {
            let segment = segment(source, 300, 1700, &[1, 2]);
            let usage = measure(&catalog, &store, &segment);
            let bills = bill(&catalog, &store, &segment, pue(1.5));
            let billed: f64 = bills.iter().map(|(_, u)| u.energy.get()).sum();
            assert!(close(billed, usage.energy.get() * 1.5), "{source:?}");
            assert!(bills.iter().all(|(_, u)| close(u.energy.get(), usage.energy.get() * 1.5 / 2.0)), "{source:?}");
        }
    }

    /// AC/MEAS-79, AC/PROF-4, AC/TST-5
    #[test]
    fn a_zero_width_segment_bills_nothing() {
        let (catalog, store) = (catalog(), steady());
        for (source, emissions) in [(cpu(), Some(0.0)), (ram(), None)] {
            for (_, usage) in bill(&catalog, &store, &segment(source, 500, 500, &[1]), pue(1.0)) {
                assert_eq!(usage.energy.get(), 0.0);
                assert_eq!(usage.emissions.map(|g| g.get()), emissions, "{source:?}");
            }
        }
    }

    /// AC/TST-6
    #[test]
    fn splitting_a_segment_in_two_bills_the_same() {
        let (catalog, store) = (catalog(), steady());
        for source in [cpu(), gpu()] {
            let whole = measure(&catalog, &store, &segment(source, 200, 1800, &[1]));
            for middle in [201, 700, 1000, 1799] {
                let mut halves = measure(&catalog, &store, &segment(source, 200, middle, &[1]));
                halves += measure(&catalog, &store, &segment(source, middle, 1800, &[1]));
                assert!(close(halves.energy.get(), whole.energy.get()));
                assert!(close(grams(halves), grams(whole)));
            }
        }
    }

    /// AC/PROF-23, AC/TST-7
    #[test]
    fn scaling_the_intensity_by_k_scales_emissions_by_k_and_not_energy() {
        let catalog = catalog();
        let cpu_samples: &[(i64, f64)] = &[(0, 10.0), (2000, 30.0)];
        let base = measure(&catalog, &store(&[(cpu(), cpu_samples), (dk1(), &[(0, 100.0), (1000, 300.0)])]), &segment(cpu(), 0, 2000, &[1]));
        for k in [0.1, 2.5, 10.0] {
            let scaled_store = store(&[(cpu(), cpu_samples), (dk1(), &[(0, 100.0 * k), (1000, 300.0 * k)])]);
            let scaled = measure(&catalog, &scaled_store, &segment(cpu(), 0, 2000, &[1]));
            assert_eq!(scaled.energy, base.energy);
            assert!(close(grams(scaled), grams(base) * k));
        }
    }

    /// AC/PROF-23, AC/PROF-28, AC/TST-8
    #[test]
    fn changing_one_intensity_changes_nothing_paired_with_another() {
        let catalog = catalog();
        let gpu_samples: &[(i64, f64)] = &[(0, 0.0), (2000, 200.0)];
        let before = store(&[(gpu(), gpu_samples), (se3(), &[(0, 40.0)]), (dk1(), &[(0, 100.0)])]);
        let after = store(&[(gpu(), gpu_samples), (se3(), &[(0, 40.0)]), (dk1(), &[(0, 900.0)])]);
        let segment = segment(gpu(), 0, 2000, &[1]);
        assert_eq!(measure(&catalog, &before, &segment), measure(&catalog, &after, &segment));
    }

    /// AC/MEAS-80, AC/PROF-45, AC/TST-9
    #[test]
    fn a_source_with_no_intensity_bills_absent_emissions_never_zero() {
        let usage = measure(&catalog(), &steady(), &segment(ram(), 0, 1000, &[1]));
        assert!(close(usage.energy.get(), 5.0));
        assert_eq!(usage.emissions, None);
    }

    /// AC/MEAS-80, AC/PROF-45
    #[test]
    fn a_segment_whose_intensity_has_no_sample_bills_absent_emissions() {
        let store = store(&[(cpu(), &[(0, 10.0), (1000, 10.0)])]);
        let usage = measure(&catalog(), &store, &segment(cpu(), 0, 1000, &[1]));
        assert!(close(usage.energy.get(), 10.0));
        assert_eq!(usage.emissions, None);
    }

    /// AC/MEAS-76, AC/MEAS-77, AC/PROF-23
    #[test]
    fn emissions_are_the_integral_of_power_times_its_own_intensity() {
        // ∫(10 + 10t) W × 100 g/kWh over 2 s = 4000 W·s·g/kWh = 4000 / 3.6e6 g.
        let usage = measure(&catalog(), &steady(), &segment(cpu(), 0, 2000, &[1]));
        assert!(close(usage.energy.get(), 40.0));
        assert!(close(grams(usage), 4000.0 / 3.6e6));
    }

    /// AC/PROF-26
    #[test]
    fn a_bill_depends_only_on_the_segment_and_the_store() {
        let catalog = catalog();
        let segment = segment(cpu(), 0, 1000, &[1, 2]);
        let cpu_samples: &[(i64, f64)] = &[(0, 10.0), (1000, 20.0)];
        let alone = store(&[(cpu(), cpu_samples), (dk1(), &[(0, 100.0)])]);
        let crowded = store(&[
            (cpu(), cpu_samples),
            (dk1(), &[(0, 100.0)]),
            (gpu(), &[(0, 0.0), (1000, 500.0)]),
            (se3(), &[(0, 900.0)]),
        ]);
        assert_eq!(bill(&catalog, &alone, &segment, pue(1.2)), bill(&catalog, &crowded, &segment, pue(1.2)));
    }

    /// AC/MEAS-41, AC/MEAS-73, AC/MEAS-74
    #[test]
    fn queue_bills_s_from_the_slope_between_reads_and_skip_from_the_held_rate() {
        let catalog = catalog();
        let s = segment(gpu(), 10_200, 10_600, &[1]);
        // Queue: reads at 10.0 s and 11.0 s, 148 J apart.
        let queued = store(&[(gpu(), &[(10_000, 1000.0), (11_000, 1148.0)])]);
        assert!(close(measure(&catalog, &queued, &s).energy.get(), 59.2));
        // Skip: 60 W up to 10.0 s, nothing after.
        let skipped = store(&[(gpu(), &[(9_000, 940.0), (10_000, 1000.0)])]);
        assert!(close(measure(&catalog, &skipped, &s).energy.get(), 24.0));
    }

    /// AC/PROF-4
    #[test]
    fn a_sliver_bills_its_width() {
        let store = store(&[(cpu(), &[(0, 20.0), (5000, 20.0)]), (gpu(), &[(0, 0.0), (5000, 500.0)])]);
        assert!(close(measure(&catalog(), &store, &segment(cpu(), 3300, 3310, &[1])).energy.get(), 0.2));
        assert!(close(measure(&catalog(), &store, &segment(gpu(), 3300, 3310, &[1])).energy.get(), 1.0));
    }

    /// AC/PROF-3, AC/PROF-41
    #[test]
    fn count_samples_counts_the_source_and_its_intensity_inside_the_segment() {
        let store = store(&[
            (cpu(), &[(3000, 1.0), (3100, 1.0), (3200, 1.0), (3500, 1.0)]),
            (dk1(), &[(3060, 100.0)]),
            (gpu(), &[(3100, 0.0)]),
        ]);
        let counts = count_samples(&catalog(), &store, &segment(cpu(), 3050, 3250, &[1]));
        assert_eq!(counts, HashMap::from([(cpu(), 2), (dk1(), 1)]));
    }

    /// AC/MEAS-78
    #[test]
    fn time_is_counted_to_the_nanosecond() {
        let store = store(&[(cpu(), &[(0, 20.0), (5000, 20.0)])]);
        let from = at(3300);
        let segment = Segment { source: cpu(), from, to: from + chrono::TimeDelta::nanoseconds(1), active: vec![span(1)], claimants: vec![span(1)] };
        let joules = measure(&catalog(), &store, &segment).energy.get();
        assert!((joules - 2e-8).abs() < 1e-15, "20 W for 1 ns, got {joules}");
    }
}
