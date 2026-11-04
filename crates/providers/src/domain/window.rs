//! [Window]: the stretch of time a power figure describes.

use std::time::Duration;

use carbontracker::{Sample, Timestamp};

/// The stretch of time a power figure describes: an average over a window,
/// or a draw at one moment, a window of no length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    pub(crate) ended: Timestamp,
    pub(crate) length: Duration,
}

impl Window {
    /// A window of `length` that ended at `ended`.
    pub(crate) fn ending(ended: Timestamp, length: Duration) -> Self {
        Self { ended, length }
    }

    /// The draw at one moment.
    pub(crate) fn instant(at: Timestamp) -> Self {
        Self { ended: at, length: Duration::ZERO }
    }

    /// `watts` over this window, stamped at its middle: an average stamped at
    /// its end would bill half a window's draw to the wrong side of a cut.
    pub(crate) fn sample(&self, watts: f64) -> Sample {
        let half = chrono::Duration::from_std(self.length / 2).unwrap_or(chrono::Duration::zero());
        Sample::new(self.ended - half, watts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AC/ENG-52
    #[test]
    fn an_average_is_stamped_at_its_windows_middle_and_an_instant_where_it_was() {
        let ended = chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(10);
        let averaged = Window::ending(ended, Duration::from_secs(1)).sample(5.0);
        assert_eq!(*averaged.observed_at(), ended - chrono::Duration::milliseconds(500));
        assert_eq!(*Window::instant(ended).sample(5.0).observed_at(), ended);
    }
}
