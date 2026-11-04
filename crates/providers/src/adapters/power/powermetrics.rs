//! powermetrics (macOS): declines unless sudo is already cached.

use std::time::Duration;

use chrono::Utc;

use crate::adapters::system::cpu::device_name;
use crate::domain::window::Window;
use crate::ports::Outcome;
use carbontracker::{Cooldown, Provider, ProviderError, ProviderId, ProviderResponse, Reading, SourceId, Timestamp};
use carbontracker::{Fidelity, PowerDomain, PowerScope, SourceDescription};
use carbontracker::SampleKind;

const POWERMETRICS_SAMPLERS: &str = "cpu_power,gpu_power,ane_power";

const POWERMETRICS_SAMPLE_MS: &str = "1000";
const WINDOW: Duration = Duration::from_secs(1);

pub(crate) const POWERMETRICS_DOMAINS: [PowerDomain; 3] =
    [PowerDomain::Cpu, PowerDomain::Gpu, PowerDomain::Ane];

/// One window of `powermetrics`, blocking for about 1.5 s. `sudo -n` fails
/// fast when no credential is cached instead of prompting and hanging.
fn run_powermetrics() -> Result<String, ProviderError> {
    run_window("sudo", &POWERMETRICS_ARGS, CHILD_TIMEOUT)
}

const POWERMETRICS_ARGS: [&str; 9] =
    ["-n", "powermetrics", "-n1", "--samplers", POWERMETRICS_SAMPLERS, "-i", POWERMETRICS_SAMPLE_MS, "--format", "text"];

/// How long one window may take before the read gives up on it.
const CHILD_TIMEOUT: Duration = Duration::from_secs(5);

/// Run `program` with `args` and return what it wrote, giving up after
/// `timeout`. A failing exit is `Denied`: `sudo -n` without a cached
/// credential.
fn run_window(program: &str, args: &[&str], timeout: Duration) -> Result<String, ProviderError> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let unavailable = |detail: String| ProviderError::Unavailable(format!("powermetrics: {detail}"));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| unavailable(error.to_string()))?;

    // Drained on a thread of its own, so a full pipe never stalls the child.
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let drained = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });

    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(unavailable(format!("no answer within {timeout:?}")));
            }
            Err(error) => return Err(unavailable(error.to_string())),
        }
    };
    let bytes = drained.join().ok().and_then(Result::ok).unwrap_or_default();

    if !status.success() {
        return Err(ProviderError::Denied("sudo powermetrics needs a cached credential".to_owned()));
    }
    String::from_utf8(bytes).map_err(|_| ProviderError::Invalid("powermetrics wrote non-UTF-8".to_owned()))
}

pub(crate) async fn probe_powermetrics(id: ProviderId) -> Option<Outcome> {
    // A real sample checks sudo and reveals which domains are reported.
    let report = tokio::task::spawn_blocking(run_powermetrics).await.ok()?;
    outcome(id, report)
}

/// The sources a probe's `report` shows, each domain it has a figure for;
/// none if the window could not be run.
fn outcome(id: ProviderId, report: Result<String, ProviderError>) -> Option<Outcome> {
    let sample = PowermetricsSample::parse(&report.ok()?);

    let device_name = device_name();
    let mut descriptions = Vec::new();
    let mut domains = Vec::new();

    for (slot, domain) in POWERMETRICS_DOMAINS.into_iter().enumerate() {
        if sample.watts_for(domain).is_none() { continue; }
        let sid = SourceId::new(id, slot as u16);
        descriptions.push((
            sid,
            SourceDescription::Power {
                device_name: device_name.clone(),
                domain,
                scope: PowerScope::DeviceTotal,
                sample_kind: SampleKind::Instant,
                fidelity: Fidelity::Measured,
            },
        ));
        domains.push((domain, sid));
    }

    if descriptions.is_empty() { return None; }

    Some(Outcome { descriptions, provider: Box::new(Powermetrics { domains }) })
}

#[derive(Debug, Default)]
struct PowermetricsSample {
    cpu_mw: Option<f64>,
    gpu_mw: Option<f64>,
    ane_mw: Option<f64>,
}

impl PowermetricsSample {
    fn parse(report: &str) -> Self {
        Self {
            cpu_mw: first_mw(report, "CPU Power: "),
            gpu_mw: first_mw(report, "GPU Power: "),
            ane_mw: first_mw(report, "ANE Power: "),
        }
    }

    fn watts_for(&self, domain: PowerDomain) -> Option<f64> {
        let mw = match domain {
            PowerDomain::Cpu => self.cpu_mw?,
            PowerDomain::Gpu => self.gpu_mw?,
            PowerDomain::Ane => self.ane_mw?,
            // powermetrics' samplers cover no such domain.
            PowerDomain::Ram | PowerDomain::System => return None,
        };
        Some(mw / 1000.0)
    }
}

fn first_mw(report: &str, label: &str) -> Option<f64> {
    for line in report.lines() {
        let Some(at) = line.find(label) else { continue };
        let rest = &line[at + label.len()..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if digits.is_empty() || !rest[digits.len()..].starts_with(" mW") { continue; }
        return digits.parse().ok();
    }
    None
}

/// Runs one `powermetrics` window per read.
struct Powermetrics {
    domains: Vec<(PowerDomain, SourceId)>,
}

impl Powermetrics {
    /// What a `report` of the window that ended at `ended` says of each domain.
    fn answer(&self, report: &str, ended: Timestamp) -> ProviderResponse {
        // The figures average the window: stamp its middle.
        let window = Window::ending(ended, WINDOW);
        let sample = PowermetricsSample::parse(report);
        let mut response = ProviderResponse::new();
        for (domain, sid) in &self.domains {
            let reading = match sample.watts_for(*domain) {
                Some(watts) => Reading::Samples(vec![window.sample(watts)]),
                None => Reading::Failed(ProviderError::Invalid(format!("no {domain:?} power in the report"))),
            };
            response.insert(*sid, reading);
        }
        response
    }
}

impl Provider for Powermetrics {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let report = run_powermetrics()?;
        Ok(self.answer(&report, Utc::now()))
    }

    fn cooldown(&self) -> Cooldown {
        // Each read is a whole window.
        Cooldown::After(Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    const REPORT: &str = "**** Processor usage ****\nCPU Power: 1234 mW\nGPU Power: 56 mW\n";

    fn sources() -> (SourceId, SourceId, SourceId) {
        (SourceId::new(ProviderId(4), 0), SourceId::new(ProviderId(4), 1), SourceId::new(ProviderId(4), 2))
    }

    /// AC/PROV-43 AC/ENG-52
    #[test]
    fn a_report_gives_each_domain_its_watts_at_the_windows_middle() {
        let (cpu, gpu, ane) = sources();
        let provider = Powermetrics { domains: vec![(PowerDomain::Cpu, cpu), (PowerDomain::Gpu, gpu), (PowerDomain::Ane, ane)] };
        let ended = chrono::DateTime::UNIX_EPOCH + chrono::Duration::seconds(10);

        let response = provider.answer(REPORT, ended);
        let Some(Reading::Samples(samples)) = response.get(cpu) else { panic!("{response:?}") };
        assert_eq!(samples[0].value(), 1.234);
        assert_eq!(*samples[0].observed_at(), ended - chrono::Duration::milliseconds(500));
        assert!(matches!(response.get(ane), Some(Reading::Failed(ProviderError::Invalid(_)))), "not in the report");
    }

    /// AC/PROV-30
    #[test]
    fn it_serves_cpu_gpu_and_ane_as_instants_with_no_cooldown() {
        assert_eq!(POWERMETRICS_DOMAINS, [PowerDomain::Cpu, PowerDomain::Gpu, PowerDomain::Ane]);
        let report = "CPU Power: 1 mW\nGPU Power: 2 mW\nANE Power: 3 mW\n";
        let outcome = outcome(ProviderId(4), Ok(report.to_owned())).expect("a report with figures serves");

        let served: Vec<(PowerDomain, SampleKind)> = outcome
            .descriptions
            .iter()
            .map(|(_, description)| match description {
                SourceDescription::Power { domain, sample_kind, .. } => (*domain, *sample_kind),
                SourceDescription::Intensity { .. } => panic!("a power provider"),
            })
            .collect();
        let instant = SampleKind::Instant;
        assert_eq!(served, [(PowerDomain::Cpu, instant), (PowerDomain::Gpu, instant), (PowerDomain::Ane, instant)]);
        assert_eq!(outcome.provider.cooldown(), Cooldown::After(Duration::ZERO));
    }

    /// AC/PROV-43
    #[test]
    fn it_asks_sudo_never_to_prompt_and_reads_a_one_second_window() {
        assert_eq!(POWERMETRICS_ARGS[..2], ["-n", "powermetrics"]);
        let interval = POWERMETRICS_ARGS.windows(2).find(|pair| pair[0] == "-i").map(|pair| pair[1]);
        assert_eq!(interval, Some("1000"));
        assert_eq!(WINDOW, Duration::from_secs(1));
    }

    /// AC/PROV-43 AC/ENG-60
    #[test]
    fn a_refused_sudo_declines_the_probe_and_denies_a_read() {
        let refused = run_window("sh", &["-c", "exit 1"], CHILD_TIMEOUT);
        assert!(matches!(refused, Err(ProviderError::Denied(_))), "{refused:?}");
        assert!(outcome(ProviderId(4), refused).is_none());
    }

    /// AC/ENG-38
    #[test]
    fn a_window_that_hangs_is_given_up_on_its_own_timeout() {
        let started = Instant::now();
        let hung = run_window("sleep", &["30"], Duration::from_millis(200));
        assert!(matches!(hung, Err(ProviderError::Unavailable(_))), "{hung:?}");
        assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
        assert!(CHILD_TIMEOUT <= Duration::from_secs(10), "a few seconds");
    }

    /// AC/ENG-50 AC/ENG-62
    #[test]
    fn a_report_that_does_not_parse_is_invalid_and_never_panics() {
        let (cpu, gpu, _) = sources();
        let provider = Powermetrics { domains: vec![(PowerDomain::Cpu, cpu), (PowerDomain::Gpu, gpu)] };
        for garbage in ["", "CPU Power: lots mW", "\u{0}\u{ff}GPU Power: -5 mW"] {
            let response = std::panic::catch_unwind(|| provider.answer(garbage, Utc::now())).expect("no panic");
            for source in [cpu, gpu] {
                assert!(
                    matches!(response.get(source), Some(Reading::Failed(ProviderError::Invalid(_)))),
                    "{garbage:?}: {response:?}"
                );
            }
        }
        assert!(outcome(ProviderId(4), Ok("no figures".to_owned())).is_none(), "nothing to serve declines");
    }
}
