//! [NvmlPower]: GPUs without an energy counter, read by their power
//! reading.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use super::Board;
use crate::domain::window::Window;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderResponse, Reading, SourceId};

/// Pascal and Volta update the reading about every 20 ms, Turing and newer
/// about every 100 ms (Yang et al., arXiv 2312.02741).
const COOLDOWN: Duration = Duration::from_millis(100);

/// The window an averaged reading covers.
const AVERAGED_OVER: Duration = Duration::from_secs(1);

pub(crate) struct NvmlPower {
    board: Arc<dyn Board>,
    /// Each GPU's index, source, and whether its reading averages the last second.
    gpus: Vec<(u32, SourceId, bool)>,
}

impl NvmlPower {
    pub(crate) fn new(board: Arc<dyn Board>, gpus: Vec<(u32, SourceId, bool)>) -> Self {
        Self { board, gpus }
    }
}

impl Provider for NvmlPower {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let mut response = ProviderResponse::new();
        for &(index, source, averaged) in &self.gpus {
            let reading = match self.board.power_mw(index) {
                Ok(milliwatts) => {
                    let now = Utc::now();
                    let window = if averaged { Window::ending(now, AVERAGED_OVER) } else { Window::instant(now) };
                    Reading::Samples(vec![window.sample(f64::from(milliwatts) / 1000.0)])
                }
                Err(error) => Reading::Failed(ProviderError::Unavailable(error)),
            };
            response.insert(source, reading);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        Cooldown::After(COOLDOWN)
    }
}

#[cfg(test)]
mod tests {
    use carbontracker::ProviderId;

    use super::super::fake::FakeBoard;
    use super::*;

    /// AC/PROV-39 AC/ENG-52
    #[test]
    fn an_averaged_reading_is_stamped_half_a_second_back() {
        let board = Arc::new(FakeBoard::default());
        board.power_mw.lock().unwrap().extend([(0, Ok(250_000)), (1, Ok(70_000))]);
        let (averaged, instant) = (SourceId::new(ProviderId(15), 0), SourceId::new(ProviderId(15), 1));
        let mut provider = NvmlPower::new(board, vec![(0, averaged, true), (1, instant, false)]);

        let before = Utc::now();
        let response = provider.read().unwrap();
        let after = Utc::now();
        let stamp = |source| match response.get(source) {
            Some(Reading::Samples(samples)) => (*samples[0].observed_at(), samples[0].value()),
            other => panic!("{other:?}"),
        };
        let (averaged_at, watts) = stamp(averaged);
        assert_eq!(watts, 250.0);
        let half = chrono::Duration::milliseconds(500);
        assert!(before - half <= averaged_at && averaged_at <= after - half, "the middle of the last second");
        let instant_at = stamp(instant).0;
        assert!(before <= instant_at && instant_at <= after, "an instantaneous reading is stamped at the read");
    }
}
