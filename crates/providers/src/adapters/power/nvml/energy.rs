//! [NvmlEnergy]: GPUs with an energy counter (Volta and newer), read as a
//! running total.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;

use super::Board;
use crate::domain::counter::Accumulator;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderResponse, Reading, SourceId};

/// The counter is measured never to update faster than about every 100 ms;
/// read 1 s apart it is within 2% (Dauner et al., HotCarbon 2026).
const COOLDOWN: Duration = Duration::from_secs(1);

pub(crate) struct NvmlEnergy {
    board: Arc<dyn Board>,
    /// Each GPU's index, source and running total.
    gpus: Vec<(u32, SourceId, Accumulator)>,
}

impl NvmlEnergy {
    pub(crate) fn new(board: Arc<dyn Board>, gpus: Vec<(u32, SourceId, Accumulator)>) -> Self {
        Self { board, gpus }
    }
}

impl Provider for NvmlEnergy {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let mut response = ProviderResponse::new();
        for (index, source, total) in &mut self.gpus {
            // The counter never wraps: a decrease is a restart, as after a driver reload.
            let reading = match self.board.energy_mj(*index) {
                Ok(millijoules) => total.reading(millijoules, Utc::now(), 1e-3),
                Err(error) => Reading::Failed(ProviderError::Unavailable(error)),
            };
            response.insert(*source, reading);
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

    fn source() -> SourceId {
        SourceId::new(ProviderId(2), 0)
    }

    fn joules(provider: &mut NvmlEnergy) -> Reading {
        provider.read().unwrap().get(source()).cloned().unwrap()
    }

    /// AC/PROV-38 AC/PROV-33 AC/ENG-57
    #[test]
    fn each_gpu_reports_its_energy_since_the_first_read_and_a_restart_fails_once() {
        let board = Arc::new(FakeBoard::default());
        let mut provider = NvmlEnergy::new(board.clone(), vec![(0, source(), Accumulator::new(None))]);
        let set = |millijoules: u64| {
            board.energy_mj.lock().unwrap().insert(0, Ok(millijoules));
        };

        set(5_000_000);
        assert!(matches!(joules(&mut provider), Reading::Samples(samples) if samples[0].value() == 0.0));
        set(5_002_500);
        assert!(matches!(joules(&mut provider), Reading::Samples(samples) if samples[0].value() == 2.5));
        set(100);
        assert!(matches!(joules(&mut provider), Reading::Failed(ProviderError::Invalid(_))), "the driver reloaded");
        set(1_100);
        assert!(matches!(joules(&mut provider), Reading::Samples(samples) if samples[0].value() == 3.5));
    }

    /// AC/ENG-59
    #[test]
    fn a_gpu_the_driver_lost_is_unavailable() {
        let mut provider = NvmlEnergy::new(Arc::new(FakeBoard::default()), vec![(0, source(), Accumulator::new(None))]);
        assert!(matches!(joules(&mut provider), Reading::Failed(ProviderError::Unavailable(_))));
    }
}
