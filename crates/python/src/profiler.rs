//! Profiler mode's waits: when a span may start or stop, judged by each
//! source's cooldown and the newest sample it landed.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use carbontracker::{Cooldown, Observation, SampleKind, SourceId, SpanReport, Timestamp, TrackerEvent};
use carbontracker_providers::Measure;
use carbontracker_run::{Block, Item, Predictor, Projection};

/// A source is fresh while its newest sample is younger than this share of
/// its cooldown: a span starting then leaks at most that share of a window.
const FRESH_SHARE: u32 = 50;

/// The sources a profiled span waits for: those that can be read again, each
/// with its cooldown and how many samples after a cut close the stretch
/// before it.
pub type Paced = Vec<(SourceId, Duration, usize)>;

/// The power sources among `cooldowns` that can be read again; `kind` is each
/// power source's sample kind, `None` for other sources. A counter's sample
/// is the total at its read, so one after a cut closes the stretch before it.
/// A window's figure is stamped inside the window, so the first sample after
/// a cut can still hold work from before it, and the second closes it.
pub fn paced(cooldowns: &HashMap<SourceId, Cooldown>, kind: impl Fn(SourceId) -> Option<SampleKind>) -> Paced {
    let mut paced: Paced = cooldowns
        .iter()
        .filter_map(|(&source, cooldown)| {
            let samples = match kind(source)? {
                SampleKind::Total => 1,
                SampleKind::Instant | SampleKind::Held => 2,
            };
            match cooldown {
                Cooldown::After(after) => Some((source, *after, samples)),
                Cooldown::Never => None,
            }
        })
        .collect();
    paced.sort_by_key(|&(source, ..)| source);
    paced
}

/// When a span may start, and which sources its start cut reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Start {
    /// The first time every source is fresh or ready; `now` if it already is.
    pub at: Timestamp,
    /// The sources ready at `at`, which the start cut reads: the span waits
    /// for their samples before its work begins.
    pub read: Vec<SourceId>,
}

/// When a span selecting `paced` may start, given each source's newest
/// sample: the first time from `now` at which every source is either fresh,
/// so the read that ended the span before serves, or ready, so the start cut
/// is read at once.
pub fn start(paced: &Paced, newest: &HashMap<SourceId, Timestamp>, now: Timestamp) -> Start {
    let ready_at = |source: SourceId, cooldown: Duration| newest.get(&source).map(|&at| at + cooldown);
    let mut candidates: Vec<Timestamp> = paced
        .iter()
        .filter_map(|&(source, cooldown, _)| ready_at(source, cooldown))
        .filter(|&at| at > now)
        .collect();
    candidates.push(now);
    candidates.sort();
    let is_ready = |source, cooldown, at| ready_at(source, cooldown).is_none_or(|ready| ready <= at);
    let is_fresh = |source: SourceId, cooldown: Duration, at: Timestamp| {
        newest.get(&source).is_some_and(|&newest| {
            at.signed_duration_since(newest).to_std().unwrap_or_default() < cooldown / FRESH_SHARE
        })
    };
    let at = candidates
        .into_iter()
        .find(|&at| paced.iter().all(|&(source, cooldown, _)| is_fresh(source, cooldown, at) || is_ready(source, cooldown, at)))
        .expect("every source is ready at the latest candidate");
    let read = paced
        .iter()
        .filter(|&&(source, cooldown, _)| !is_fresh(source, cooldown, at))
        .map(|&(source, ..)| source)
        .collect();
    Start { at, read }
}

/// When every source in `paced` can be read again: one cooldown after its
/// newest sample, and never before `now`.
pub fn ready(paced: &Paced, newest: &HashMap<SourceId, Timestamp>, now: Timestamp) -> Timestamp {
    paced
        .iter()
        .filter_map(|&(source, cooldown, _)| Some(*newest.get(&source)? + cooldown))
        .fold(now, Timestamp::max)
}

/// How often a wait asks a late source again.
pub const ASK_EVERY: Duration = Duration::from_millis(10);

/// Whether a wait should cut again: some `late` source is past its cooldown,
/// so no read of it is queued. A source whose sampler sets the pace, with a
/// cooldown of zero, answers a read with nothing new until its window ends,
/// and is asked until it has.
pub fn ask_again(paced: &Paced, late: &[SourceId], newest: &HashMap<SourceId, Timestamp>, now: Timestamp) -> bool {
    paced
        .iter()
        .filter(|(source, ..)| late.contains(source))
        .any(|&(source, cooldown, _)| newest.get(&source).is_none_or(|&at| at + cooldown <= now))
}

/// What a profiler measures: every component, never a whole-system total. A
/// system total also carries the display, disks and other processes, whose
/// noise swamps a short span's dynamic energy.
pub fn measure(chosen: Measure) -> Measure {
    use carbontracker::PowerDomain::{Ane, Cpu, Gpu, Ram};
    match chosen {
        Measure::All => Measure::Only(vec![Cpu, Gpu, Ram, Ane]),
        only => only,
    }
}

/// How many epochs have settled, which a caller can wait on.
#[derive(Default)]
pub struct Epochs {
    count: Mutex<usize>,
    changed: Condvar,
}

impl Epochs {
    pub fn settled(&self, count: usize) {
        *self.count.lock().expect("not poisoned") = count;
        self.changed.notify_all();
    }

    /// Waits until `count` epochs have settled, or until `deadline`. Returns
    /// whether they have.
    pub fn wait(&self, count: usize, deadline: Instant) -> bool {
        let mut settled = self.count.lock().expect("not poisoned");
        loop {
            let now = Instant::now();
            if *settled >= count || now >= deadline {
                return *settled >= count;
            }
            settled = self.changed.wait_timeout(settled, deadline - now).expect("not poisoned").0;
        }
    }
}

/// Each source's newest landed sample, which a caller can wait on.
#[derive(Default)]
pub struct Freshness {
    /// Each source's most recent sample stamps, oldest first.
    stamps: Mutex<HashMap<SourceId, Vec<Timestamp>>>,
    landed: Condvar,
}

/// Stamps kept per source: more than any wait counts.
const KEPT: usize = 8;

impl Freshness {
    /// A sample of `source` stamped `at` has landed.
    pub fn landed(&self, source: SourceId, at: Timestamp) {
        let mut stamps = self.stamps.lock().expect("not poisoned");
        let kept = stamps.entry(source).or_default();
        let place = kept.partition_point(|&stamp| stamp <= at);
        kept.insert(place, at);
        if kept.len() > KEPT {
            kept.remove(0);
        }
        self.landed.notify_all();
    }

    /// Each source's newest sample so far.
    pub fn newest(&self) -> HashMap<SourceId, Timestamp> {
        let stamps = self.stamps.lock().expect("not poisoned");
        stamps.iter().filter_map(|(&source, kept)| Some((source, *kept.last()?))).collect()
    }

    /// Waits until every source in `paced` has landed as many samples stamped
    /// after `after` as it needs, or until `deadline`. Returns the sources
    /// still late.
    pub fn wait(&self, paced: &Paced, after: Timestamp, deadline: Instant) -> Vec<SourceId> {
        let late = |stamps: &HashMap<SourceId, Vec<Timestamp>>| -> Vec<SourceId> {
            paced
                .iter()
                .filter(|&&(source, _, needed)| {
                    stamps.get(&source).map_or(0, |kept| kept.iter().filter(|&&at| at > after).count()) < needed
                })
                .map(|&(source, ..)| source)
                .collect()
        };
        let mut stamps = self.stamps.lock().expect("not poisoned");
        loop {
            let still = late(&stamps);
            let now = Instant::now();
            if still.is_empty() || now >= deadline {
                return still;
            }
            stamps = self.landed.wait_timeout(stamps, deadline - now).expect("not poisoned").0;
        }
    }
}

/// The binding's projection: the prediction, each source's newest sample, and
/// every settled span's report.
pub struct Projected {
    pub predictor: Predictor,
    pub freshness: Arc<Freshness>,
    pub epochs: Arc<Epochs>,
    pub settled: Vec<Arc<SpanReport>>,
}

impl Projection for Projected {
    fn project(&mut self, item: &Item) -> Vec<Box<dyn Block>> {
        match item {
            Item::Event(TrackerEvent::ReadoutLanded { outcomes, .. }) => {
                for (&source, observation) in outcomes.iter() {
                    if let Observation::Samples(samples) = observation
                        && let Some(at) = samples.iter().map(|sample| *sample.observed_at()).max()
                    {
                        self.freshness.landed(source, at);
                    }
                }
            }
            Item::Event(TrackerEvent::SpanSettled { report }) => self.settled.push(Arc::clone(report)),
            _ => {}
        }
        let blocks = self.predictor.project(item);
        if let Item::Event(TrackerEvent::SpanSettled { .. }) = item {
            self.epochs.settled(self.predictor.measured().count);
        }
        blocks
    }

    fn describe(&self, header: &mut carbontracker_run::record::Header) {
        self.predictor.describe(header);
    }
}

#[cfg(test)]
mod tests {
    use std::thread;

    use carbontracker::ProviderId;
    use chrono::{TimeZone, Utc};

    use carbontracker::SampleKind;

    use super::*;

    #[allow(non_snake_case)]
    fn NVML() -> SourceId {
        SourceId::new(ProviderId(1), 0)
    }
    #[allow(non_snake_case)]
    fn RAPL() -> SourceId {
        SourceId::new(ProviderId(2), 0)
    }
    #[allow(non_snake_case)]
    fn STATED() -> SourceId {
        SourceId::new(ProviderId(3), 0)
    }

    fn at(seconds: f64) -> Timestamp {
        Utc.timestamp_opt(1_000_000, 0).unwrap() + Duration::from_secs_f64(seconds)
    }

    fn both() -> Paced {
        vec![(NVML(), Duration::from_secs(1), 1), (RAPL(), Duration::from_millis(10), 1)]
    }

    /// AC/PRF-3 AC/PRF-9
    #[test]
    fn only_sources_that_can_be_read_again_are_paced() {
        let cooldowns = HashMap::from([
            (NVML(), Cooldown::After(Duration::from_secs(1))),
            (STATED(), Cooldown::Never),
            (RAPL(), Cooldown::After(Duration::from_millis(10))),
        ]);
        let counter = |_| Some(SampleKind::Total);
        assert_eq!(paced(&cooldowns, counter), both());
        let not_rapl = |source| (source != RAPL()).then_some(SampleKind::Total);
        assert_eq!(paced(&cooldowns, not_rapl), vec![(NVML(), Duration::from_secs(1), 1)]);
    }

    /// AC/PRF-7
    #[test]
    fn a_window_average_waits_for_two_samples_and_a_counter_for_one() {
        // A window's figure is stamped inside it, so the first sample after a
        // cut may still hold work from before it.
        let cooldowns = HashMap::from([(NVML(), Cooldown::After(Duration::ZERO)), (RAPL(), Cooldown::After(Duration::ZERO))]);
        let kind = |source| Some(if source == NVML() { SampleKind::Instant } else { SampleKind::Total });
        assert_eq!(paced(&cooldowns, kind), vec![(NVML(), Duration::ZERO, 2), (RAPL(), Duration::ZERO, 1)]);
    }

    /// AC/PRF-6
    #[test]
    fn a_span_right_after_a_read_starts_at_once() {
        let newest = HashMap::from([(NVML(), at(1.0)), (RAPL(), at(1.0))]);
        assert_eq!(start(&both(), &newest, at(1.0001)), Start { at: at(1.0001), read: vec![] });
    }

    /// AC/PRF-6
    #[test]
    fn a_short_cooldown_is_waited_out_while_a_long_one_stays_fresh() {
        // RAPL is ready at 1.01; NVML, read at 1.0, is still fresh then.
        let newest = HashMap::from([(NVML(), at(1.0)), (RAPL(), at(1.0))]);
        assert_eq!(start(&both(), &newest, at(1.001)), Start { at: at(1.01), read: vec![RAPL()] });
    }

    /// AC/PRF-6
    #[test]
    fn a_span_after_unprofiled_code_waits_until_every_source_is_ready() {
        // NVML's newest is 0.5 s old: ready at 1.0. RAPL is ready already.
        let newest = HashMap::from([(NVML(), at(0.0)), (RAPL(), at(0.4))]);
        assert_eq!(start(&both(), &newest, at(0.5)), Start { at: at(1.0), read: vec![NVML(), RAPL()] });
    }

    /// AC/PRF-6
    #[test]
    fn ready_sources_are_read_now() {
        let newest = HashMap::from([(NVML(), at(0.0)), (RAPL(), at(0.0))]);
        assert_eq!(start(&both(), &newest, at(3.0)), Start { at: at(3.0), read: vec![NVML(), RAPL()] });
    }

    /// AC/PRF-6
    #[test]
    fn a_source_never_read_is_ready_now() {
        assert_eq!(start(&both(), &HashMap::new(), at(2.0)), Start { at: at(2.0), read: vec![NVML(), RAPL()] });
    }

    /// AC/PRF-9
    #[test]
    fn nothing_to_pace_starts_at_once() {
        assert_eq!(start(&Vec::new(), &HashMap::new(), at(2.0)), Start { at: at(2.0), read: vec![] });
    }

    /// AC/PRF-4
    #[test]
    fn a_stop_is_due_once_every_source_is_ready() {
        let newest = HashMap::from([(NVML(), at(1.0)), (RAPL(), at(1.3))]);
        assert_eq!(ready(&both(), &newest, at(1.4)), at(2.0));
        assert_eq!(ready(&both(), &newest, at(2.5)), at(2.5));
    }

    /// AC/PRF-8
    #[test]
    fn a_late_source_past_its_cooldown_is_asked_again() {
        // macmon: cooldown zero, its sampler sets the pace.
        let macmon = vec![(NVML(), Duration::ZERO, 2)];
        let newest = HashMap::from([(NVML(), at(1.0))]);
        assert!(ask_again(&macmon, &[NVML()], &newest, at(1.02)));
    }

    /// AC/PRF-8
    #[test]
    fn a_late_source_inside_its_cooldown_waits_for_its_queued_read() {
        let newest = HashMap::from([(NVML(), at(1.0))]);
        let nvml = vec![(NVML(), Duration::from_secs(1), 1)];
        assert!(!ask_again(&nvml, &[NVML()], &newest, at(1.5)));
        assert!(ask_again(&nvml, &[NVML()], &newest, at(2.1)), "its read came back with nothing new");
    }

    /// AC/PRF-8
    #[test]
    fn a_source_on_time_is_not_asked_again() {
        let macmon = vec![(NVML(), Duration::ZERO, 2)];
        assert!(!ask_again(&macmon, &[], &HashMap::new(), at(1.0)));
    }

    /// AC/PRF-5
    #[test]
    fn a_sample_after_the_cut_ends_the_wait() {
        let freshness = Arc::new(Freshness::default());
        freshness.landed(NVML(), at(0.0));
        let landing = Arc::clone(&freshness);
        let lands = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            landing.landed(NVML(), at(1.0));
        });

        let paced = vec![(NVML(), Duration::from_secs(1), 1)];
        let late = freshness.wait(&paced, at(0.5), Instant::now() + Duration::from_secs(5));
        lands.join().unwrap();
        assert!(late.is_empty());
    }

    /// AC/PRF-5 AC/PRF-10
    #[test]
    fn a_sample_before_the_cut_does_not_end_the_wait() {
        let freshness = Freshness::default();
        freshness.landed(NVML(), at(0.4));
        let paced = vec![(NVML(), Duration::from_secs(1), 1)];
        let late = freshness.wait(&paced, at(0.5), Instant::now() + Duration::from_millis(20));
        assert_eq!(late, vec![NVML()]);
    }

    /// AC/PRF-7
    #[test]
    fn a_window_average_wait_ends_on_its_second_sample_after_the_cut() {
        let freshness = Freshness::default();
        let paced = vec![(NVML(), Duration::ZERO, 2)];
        freshness.landed(NVML(), at(0.55));
        let soon = || Instant::now() + Duration::from_millis(20);
        assert_eq!(freshness.wait(&paced, at(0.5), soon()), vec![NVML()], "one window after the cut is not enough");
        freshness.landed(NVML(), at(0.65));
        assert!(freshness.wait(&paced, at(0.5), soon()).is_empty());
    }

    /// AC/PRF-4
    #[test]
    fn the_newest_sample_is_kept_whatever_order_they_land_in() {
        let freshness = Freshness::default();
        freshness.landed(NVML(), at(2.0));
        freshness.landed(NVML(), at(1.0));
        assert_eq!(freshness.newest()[&NVML()], at(2.0));
    }

    #[test]
    fn a_settled_epoch_wakes_a_waiter() {
        let epochs = Arc::new(Epochs::default());
        let settling = Arc::clone(&epochs);
        let settles = thread::spawn(move || {
            thread::sleep(Duration::from_millis(20));
            settling.settled(1);
        });
        assert!(epochs.wait(1, Instant::now() + Duration::from_secs(5)));
        settles.join().unwrap();
    }

    #[test]
    fn too_few_settled_epochs_wait_until_the_deadline() {
        let epochs = Epochs::default();
        epochs.settled(1);
        assert!(!epochs.wait(2, Instant::now() + Duration::from_millis(20)));
    }

    /// AC/PRF-13
    #[test]
    fn a_profiler_measures_components_not_the_system_total() {
        use carbontracker::PowerDomain::{Ane, Cpu, Gpu, Ram};
        assert_eq!(measure(Measure::All), Measure::Only(vec![Cpu, Gpu, Ram, Ane]));
        assert_eq!(measure(Measure::Only(vec![Gpu])), Measure::Only(vec![Gpu]));
    }
}
