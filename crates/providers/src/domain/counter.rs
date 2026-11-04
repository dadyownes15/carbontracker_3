//! [Accumulator]: a hardware energy counter turned into a running total.

use carbontracker::{ProviderError, Reading, Sample, Timestamp};

/// Keeps a running total from raw counts of a hardware energy counter. The
/// total starts at zero at the first look and never decreases.
///
/// A counter that goes down either wrapped or restarted. With a known
/// `range`, it wrapped, and the wrap is counted. Without one, the energy
/// since the last look is unknown: that look fails, and the total carries on
/// from the new count.
#[derive(Debug, Clone)]
pub(crate) struct Accumulator {
    /// The count at which the counter wraps to zero, if known.
    range: Option<u64>,
    last: Option<u64>,
    total: u128,
}

impl Accumulator {
    /// A counter that wraps at `range`, or, with `None`, one whose decrease
    /// cannot be accounted for.
    pub(crate) fn new(range: Option<u64>) -> Self {
        Self { range, last: None, total: 0 }
    }

    /// The running total after a look that saw `raw`; `None` if the energy
    /// since the last look is unknown.
    pub(crate) fn advance(&mut self, raw: u64) -> Option<u128> {
        let Some(last) = self.last.replace(raw) else { return Some(0) };
        let delta = if raw >= last {
            u128::from(raw - last)
        } else {
            u128::from(self.range?.saturating_sub(last)) + u128::from(raw)
        };
        self.total += delta;
        Some(self.total)
    }

    /// The [Reading] for a look at `at` that saw `raw`, in joules, given the
    /// joules each count is worth.
    pub(crate) fn reading(&mut self, raw: u64, at: Timestamp, joules_per_count: f64) -> Reading {
        match self.advance(raw) {
            Some(total) => Reading::Samples(vec![Sample::new(at, total as f64 * joules_per_count)]),
            None => Reading::Failed(ProviderError::Invalid(
                "the energy counter went down: energy since the last read is unknown".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_total_starts_at_zero_at_the_first_look() {
        let mut counter = Accumulator::new(None);
        assert_eq!(counter.advance(5_000), Some(0));
        assert_eq!(counter.advance(7_500), Some(2_500));
    }

    #[test]
    fn a_count_that_has_not_moved_adds_nothing() {
        let mut counter = Accumulator::new(None);
        counter.advance(1_000);
        assert_eq!(counter.advance(1_000), Some(0));
    }

    /// AC/PROV-33 AC/ENG-56
    #[test]
    fn a_wrap_within_a_known_range_is_counted() {
        let mut counter = Accumulator::new(Some(1_000));
        counter.advance(900);
        assert_eq!(counter.advance(100), Some(200), "100 to the top, then 100 more");
    }

    /// AC/PROV-33 AC/ENG-57
    #[test]
    fn a_decrease_with_no_range_is_unknown_and_the_total_carries_on() {
        let mut counter = Accumulator::new(None);
        counter.advance(500);
        assert_eq!(counter.advance(800), Some(300));
        assert_eq!(counter.advance(100), None);
        assert_eq!(counter.advance(150), Some(350), "the unknown gap adds nothing, and the total never decreases");
    }

    /// AC/PROV-33 AC/ENG-57
    #[test]
    fn a_reading_is_the_total_in_joules_or_an_invalid_failure() {
        let at = chrono::DateTime::UNIX_EPOCH;
        let mut counter = Accumulator::new(None);
        counter.reading(1_000, at, 1e-3);
        assert!(matches!(counter.reading(3_000, at, 1e-3), Reading::Samples(samples) if samples[0].value() == 2.0));
        assert!(matches!(counter.reading(0, at, 1e-3), Reading::Failed(ProviderError::Invalid(_))));
    }
}
