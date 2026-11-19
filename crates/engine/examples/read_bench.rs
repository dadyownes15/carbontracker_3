//! Drives the engine as hard as the gate allows and counts reads.
//!
//! Usage: read_bench <providers> <seconds> <read_micros> <pace_micros>
//! Each provider sleeps `read_micros` per read and has no cooldown; the
//! caller marks in a tight loop, so each provider is read back to back.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use carbontracker::{
    Fidelity, PowerDomain, PowerScope, Provider, ProviderError, ProviderId, ProviderResponse, Sample, SampleKind,
    SourceDescription, SourceId, SourceSelection, Sources, TrackerConfig, Work, start,
};

struct Counted {
    source: SourceId,
    sleep: Duration,
    reads: Arc<AtomicUsize>,
    threads: Arc<Mutex<HashSet<std::thread::ThreadId>>>,
}

impl Provider for Counted {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        self.threads.lock().unwrap().insert(std::thread::current().id());
        if !self.sleep.is_zero() {
            std::thread::sleep(self.sleep);
        }
        let mut response = ProviderResponse::new();
        response.samples(self.source, vec![Sample::new(chrono::Utc::now(), 1.0)]);
        Ok(response)
    }
}

fn main() {
    let args: Vec<u64> = std::env::args().skip(1).map(|a| a.parse().expect("a number")).collect();
    let (providers, seconds, micros, pace) = (args[0] as usize, args[1], args[2], args[3]);

    let reads = Arc::new(AtomicUsize::new(0));
    let threads = Arc::new(Mutex::new(HashSet::new()));
    let mut sources = Sources::new();
    for n in 0..providers {
        let id = ProviderId(n);
        let source = SourceId::new(id, 0);
        let provider =
            Counted { source, sleep: Duration::from_micros(micros), reads: reads.clone(), threads: threads.clone() };
        let description = SourceDescription::Power {
            device_name: format!("bench-{n}"),
            domain: PowerDomain::System,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            fidelity: Fidelity::Measured,
        };
        sources.add_provider(id, provider, [(source, description)]).expect("distinct providers");
    }
    let config = TrackerConfig { readout_interval: Duration::from_millis(100), ..TrackerConfig::default() };
    let tracker = start(sources, config).expect("valid config");

    let span = tracker.start_span("bench", None, SourceSelection::All);
    let started = Instant::now();
    let mut marks = 0u64;
    while started.elapsed() < Duration::from_secs(seconds) {
        // Paced so the engine, not the queue, is what is measured.
        tracker.mark(span, Work::default());
        marks += 1;
        std::thread::sleep(Duration::from_micros(pace));
    }
    tracker.stop_span(span, Work::default());
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(tracker.finish()).expect("finished");
    let elapsed = started.elapsed().as_secs_f64();
    let reads = reads.load(Ordering::Relaxed);
    println!(
        "providers={providers} read_us={micros} marks={marks} reads={reads} reads_per_s_per_provider={:.0} distinct_read_threads={}",
        reads as f64 / elapsed / providers as f64,
        threads.lock().unwrap().len()
    );
}
