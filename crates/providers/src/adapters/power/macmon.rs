//! macmon (macOS): IOReport private API, no sudo needed.

use std::time::Duration;

use chrono::Utc;

use crate::adapters::system::cpu::device_name;
use crate::domain::window::Window;
use crate::ports::Outcome;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, SourceId, Timestamp};
use carbontracker::{Fidelity, PowerDomain, PowerScope, SourceDescription};
use carbontracker::{Sample, SampleKind};

/// Back-to-back windows this long: a 10 ms sampler adds about 0.3–0.4 W of
/// CPU draw, a 100 ms one at most about 0.05 W (one M2 Pro, macOS 26.2).
const MACMON_SAMPLE_INTERVAL_MS: u32 = 100;
const WINDOW: Duration = Duration::from_millis(MACMON_SAMPLE_INTERVAL_MS as u64);

pub(crate) const MACMON_DOMAINS: [PowerDomain; 5] = [
    PowerDomain::Cpu,
    PowerDomain::Gpu,
    PowerDomain::Ane,
    PowerDomain::Ram,
    PowerDomain::System,
];

pub(crate) async fn probe_macmon(id: ProviderId) -> Option<Outcome> {
    // Intel Macs have no IOReport power channel and fail here.
    let (ready, readings) = spawn_macmon_thread(MACMON_SAMPLE_INTERVAL_MS);
    if !ready.await.unwrap_or(false) { return None; }

    let descriptions = describe(id, &device_name());
    let domains = MACMON_DOMAINS.into_iter().zip(descriptions.iter().map(|(sid, _)| *sid)).collect();
    // Self-paced: a background thread samples, and a read only drains it.
    Some(Outcome { descriptions, provider: Box::new(Macmon { readings, domains }) })
}

/// A source for each of [MACMON_DOMAINS] on `device_name`, in that order.
fn describe(id: ProviderId, device_name: &str) -> Vec<(SourceId, SourceDescription)> {
    MACMON_DOMAINS
        .into_iter()
        .enumerate()
        .map(|(slot, domain)| {
            let description = SourceDescription::Power {
                device_name: device_name.to_owned(),
                domain,
                scope: PowerScope::DeviceTotal,
                sample_kind: SampleKind::Instant,
                fidelity: Fidelity::Measured,
            };
            (SourceId::new(id, slot as u16), description)
        })
        .collect()
}

/// One sampler window: when it ended, and each domain's watts over it.
#[derive(Debug, Clone)]
struct MacmonWindow {
    ended: Timestamp,
    watts: [(PowerDomain, f64); 5],
}

impl MacmonWindow {
    fn from_metrics(ended: Timestamp, metrics: &::macmon::Metrics) -> Self {
        Self {
            ended,
            watts: [
                (PowerDomain::Cpu, f64::from(metrics.cpu_power)),
                (PowerDomain::Gpu, f64::from(metrics.gpu_power)),
                (PowerDomain::Ane, f64::from(metrics.ane_power)),
                (PowerDomain::Ram, f64::from(metrics.ram_power)),
                (PowerDomain::System, f64::from(metrics.sys_power)),
            ],
        }
    }

    /// `domain`'s figure, stamped where it belongs: CPU, GPU, ANE and RAM are
    /// energy over the window, so its middle; system is the SMC's reading as
    /// the window ends.
    fn sample(&self, domain: PowerDomain) -> Option<Sample> {
        let (_, watts) = self.watts.iter().find(|(of, _)| *of == domain)?;
        let window = match domain {
            PowerDomain::System => Window::instant(self.ended),
            _ => Window::ending(self.ended, WINDOW),
        };
        Some(window.sample(*watts))
    }
}

type MacmonReadings = std::sync::mpsc::Receiver<Result<MacmonWindow, ProviderError>>;

/// Runs the sampler on its own OS thread: `get_metrics` blocks and `Sampler` is
/// `!Send`. The thread exits when the receiver is dropped.
fn spawn_macmon_thread(interval_ms: u32)
    -> (tokio::sync::oneshot::Receiver<bool>, MacmonReadings)
{
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let Ok(mut sampler) = ::macmon::Sampler::new() else {
            let _ = ready_tx.send(false);
            return;
        };

        // Signal ready only after the first reading, so spans have no unmeasured head.
        let mut ready_tx = Some(ready_tx);

        loop {
            // Stamped as the window ends; it may sit in the channel a while.
            let reading = sampler
                .get_metrics(interval_ms)
                .map(|metrics| MacmonWindow::from_metrics(Utc::now(), &metrics))
                .map_err(|error| ProviderError::Unavailable(format!("IOReport: {error}")));

            if let Some(ready_tx) = ready_tx.take() {
                let _ = ready_tx.send(reading.is_ok());
            }

            if tx.send(reading).is_err() { break; }
        }
    });

    (ready_rx, rx)
}

struct Macmon {
    readings: MacmonReadings,
    domains: Vec<(PowerDomain, SourceId)>,
}

impl Provider for Macmon {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        // Every window queued since the last read, not just the newest.
        let mut batch = Vec::new();
        loop {
            match self.readings.try_recv() {
                Ok(window) => batch.push(window?),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    return Err(ProviderError::Unavailable("the macmon sampler stopped".to_owned()));
                }
            }
        }

        let mut response = ProviderResponse::new();
        for &(domain, sid) in &self.domains {
            let samples = batch.iter().filter_map(|window| window.sample(domain)).collect();
            // No window since the last read: Unchanged.
            response.samples(sid, samples);
        }
        Ok(response)
    }

    fn cooldown(&self) -> Cooldown {
        // A reading stays current until the sampler's next window ends.
        Cooldown::After(WINDOW)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use carbontracker::Reading;

    use super::*;

    fn window(ended_ms: i64, watts: f64) -> MacmonWindow {
        let ended = chrono::DateTime::UNIX_EPOCH + chrono::Duration::milliseconds(ended_ms);
        MacmonWindow { ended, watts: MACMON_DOMAINS.map(|domain| (domain, watts)) }
    }

    fn macmon() -> (Macmon, mpsc::Sender<Result<MacmonWindow, ProviderError>>, SourceId, SourceId) {
        let (send, readings) = mpsc::channel();
        let (cpu, system) = (SourceId::new(ProviderId(3), 0), SourceId::new(ProviderId(3), 4));
        (Macmon { readings, domains: vec![(PowerDomain::Cpu, cpu), (PowerDomain::System, system)] }, send, cpu, system)
    }

    fn stamps(response: &ProviderResponse, source: SourceId) -> Vec<i64> {
        match response.get(source) {
            Some(Reading::Samples(samples)) => samples.iter().map(|sample| sample.observed_at().timestamp_millis()).collect(),
            other => panic!("{other:?}"),
        }
    }

    /// AC/PROV-40 AC/PROV-41 AC/ENG-52
    #[test]
    fn a_read_takes_every_window_since_the_last_stamped_where_each_figure_belongs() {
        let (mut macmon, send, cpu, system) = macmon();
        send.send(Ok(window(100, 3.0))).unwrap();
        send.send(Ok(window(200, 4.0))).unwrap();

        let response = macmon.read().unwrap();
        assert_eq!(stamps(&response, cpu), [50, 150], "energy over a window: its middle");
        assert_eq!(stamps(&response, system), [100, 200], "the SMC's reading: as the window ends");
    }

    /// AC/PROV-40 AC/ENG-48
    #[test]
    fn with_no_window_since_the_last_read_nothing_is_new() {
        let (mut macmon, _send, cpu, _) = macmon();
        assert_eq!(macmon.read().unwrap().get(cpu), Some(&Reading::Unchanged));
    }

    /// AC/ENG-39
    #[test]
    fn a_sampler_that_stopped_fails_the_read() {
        let (mut macmon, send, ..) = macmon();
        drop(send);
        assert!(matches!(macmon.read(), Err(ProviderError::Unavailable(_))));
    }

    /// AC/PROV-29 AC/PROV-42
    #[test]
    fn it_serves_five_domains_as_instants_paced_by_100_ms_windows() {
        let descriptions = describe(ProviderId(3), "Apple M2 Pro");
        let served: Vec<(PowerDomain, SampleKind)> = descriptions
            .iter()
            .map(|(_, description)| match description {
                SourceDescription::Power { domain, sample_kind, .. } => (*domain, *sample_kind),
                SourceDescription::Intensity { .. } => panic!("a power provider"),
            })
            .collect();
        let instant = SampleKind::Instant;
        assert_eq!(
            served,
            [
                (PowerDomain::Cpu, instant),
                (PowerDomain::Gpu, instant),
                (PowerDomain::Ane, instant),
                (PowerDomain::Ram, instant),
                (PowerDomain::System, instant),
            ]
        );

        let (macmon, ..) = macmon();
        assert_eq!(macmon.cooldown(), Cooldown::After(WINDOW), "a reading stays current for one sampler window");
        assert_eq!(WINDOW, Duration::from_millis(100));
    }
}
