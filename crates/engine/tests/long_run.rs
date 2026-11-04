//! A long run, counted by the allocator: what the engine holds levels off.
//! Its own binary, so the count sees only this test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use tokio::sync::oneshot;

use carbontracker::{
    Fidelity, GridLocation, PowerDomain, PowerScope, Provider, ProviderError, ProviderId, ProviderResponse, Sample,
    SampleKind, SourceDescription, SourceId, SourceSelection, Sources, TrackerConfig, TrackerHandle, TrackerQuery,
    Work, start_subscribed,
};

/// Bytes allocated and not yet freed, on every thread.
static LIVE: AtomicIsize = AtomicIsize::new(0);

struct Counting;

// SAFETY: forwards to the system allocator, only counting.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's layout, passed on.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: allocated above with this layout.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Power samples a run delivers: nearly six days of two devices sampled at 1 Hz.
const SAMPLES: usize = 1_000_000;
/// Samples a sampler buffers between reads.
const BURST: usize = 200;

/// A sampler that buffers [BURST] samples between reads, spread over the gap
/// since the last read: watts for an `Instant` source, a joule counter for a
/// `Total` one.
struct Sampler {
    source: SourceId,
    kind: SampleKind,
    watts: f64,
    joules: f64,
    last: DateTime<Utc>,
    delivered: Arc<AtomicUsize>,
}

impl Sampler {
    fn new(source: SourceId, kind: SampleKind, watts: f64, delivered: Arc<AtomicUsize>) -> Self {
        Self { source, kind, watts, joules: 0.0, last: Utc::now(), delivered }
    }
}

impl Provider for Sampler {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let now = Utc::now();
        let step = (now - self.last) / BURST as i32;
        let mut samples = Vec::with_capacity(BURST);
        for n in 1..=BURST {
            let at = self.last + step * n as i32;
            let value = match self.kind {
                SampleKind::Total => {
                    self.joules += self.watts * step.as_seconds_f64();
                    self.joules
                }
                _ => self.watts,
            };
            samples.push(Sample::new(at, value));
        }
        self.last = now;
        self.delivered.fetch_add(BURST, Ordering::Relaxed);
        let mut response = ProviderResponse::new();
        response.samples(self.source, samples);
        Ok(response)
    }
}

/// A grid that publishes a new intensity every so many reads.
struct Grid {
    source: SourceId,
    reads: usize,
}

impl Provider for Grid {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        self.reads += 1;
        let mut response = ProviderResponse::new();
        let samples = if self.reads % 50 == 1 {
            vec![Sample::new(Utc::now(), 150.0 + (self.reads % 7) as f64 * 20.0)]
        } else {
            Vec::new()
        };
        response.samples(self.source, samples);
        Ok(response)
    }
}

/// A GPU workstation: a CPU sampled for watts, a GPU energy counter, and the grid.
fn workstation(delivered: &Arc<AtomicUsize>) -> Sources {
    let power = |device: &str, domain, sample_kind| SourceDescription::Power {
        device_name: device.to_owned(),
        domain,
        scope: PowerScope::DeviceTotal,
        sample_kind,
        fidelity: Fidelity::Stated,
    };
    let [cpu, gpu, grid] = [1, 2, 3].map(|n| SourceId::new(ProviderId(n), 0));
    let mut sources = Sources::new();
    let sampler = Sampler::new(cpu, SampleKind::Instant, 65.0, delivered.clone());
    sources.add_provider(ProviderId(1), sampler, [(cpu, power("cpu", PowerDomain::Cpu, SampleKind::Instant))]).unwrap();
    let sampler = Sampler::new(gpu, SampleKind::Total, 300.0, delivered.clone());
    sources.add_provider(ProviderId(2), sampler, [(gpu, power("gpu0", PowerDomain::Gpu, SampleKind::Total))]).unwrap();
    let intensity = SourceDescription::Intensity { location: GridLocation::Global, fidelity: Fidelity::Stated };
    sources.add_provider(ProviderId(3), Grid { source: grid, reads: 0 }, [(grid, intensity)]).unwrap();
    sources
}

/// Wait until the tracker has handled everything sent before.
async fn caught_up(tracker: &TrackerHandle) {
    let (reply, answer) = oneshot::channel();
    tracker.query(TrackerQuery::Catalog { reply });
    answer.await.expect("running").expect("answered");
}

/// AC/ENG-29, AC/ENG-30, AC/ENG-32
#[tokio::test]
async fn a_long_training_run_levels_off() {
    let began = Instant::now();
    let delivered = Arc::new(AtomicUsize::new(0));
    let config = TrackerConfig { readout_interval: Duration::from_secs(1), ..TrackerConfig::default() };
    let (tracker, mut events) = start_subscribed(workstation(&delivered), config).expect("a valid config");
    // A sink that keeps up, keeping nothing.
    let sink = tokio::spawn(async move {
        let mut seen = 0usize;
        while events.recv().await.is_ok() {
            seen += 1;
        }
        seen
    });

    // Epochs of steps, one span per step, a mark halfway through each.
    let run = tracker.start_span("train", None, SourceSelection::All);
    let mut checkpoints = Vec::new();
    let mut epoch = tracker.start_span("epoch", Some(run), SourceSelection::All);
    let mut steps = 0usize;
    while delivered.load(Ordering::Relaxed) < SAMPLES {
        let step = tracker.start_span("step", Some(epoch), SourceSelection::All);
        tracker.mark(step, Work::of("batches", 1.0));
        tracker.stop_span(step, Work::of("samples", 256.0));
        steps += 1;
        if steps.is_multiple_of(1_000) {
            tracker.stop_span(epoch, Work::of("epochs", 1.0));
            epoch = tracker.start_span("epoch", Some(run), SourceSelection::All);
        }
        caught_up(&tracker).await;
        let so_far = delivered.load(Ordering::Relaxed);
        if so_far / (SAMPLES / 20) > checkpoints.len() {
            checkpoints.push((so_far, LIVE.load(Ordering::Relaxed)));
        }
    }
    tracker.stop_span(run, Work::of("epochs", 0.0));
    let report = tracker.finish().await.expect("finishes");
    let seen = sink.await.expect("the sink ran");

    for (samples, live) in &checkpoints {
        eprintln!("{samples:>9} samples: {live:>10} bytes live");
    }
    eprintln!("{steps} steps, {seen} events, {:?}", began.elapsed());
    assert!(report.total.energy.as_microjoules() > 0.0, "the run was billed");

    // After the first fifth, live bytes stay within 1 MB of where they were;
    // the rest of the run delivered 800k samples.
    let (_, settled) = checkpoints[3];
    let peak = checkpoints[3..].iter().map(|&(_, live)| live).max().unwrap();
    assert!(peak - settled < 1_000_000, "live bytes grew from {settled} to {peak}");
}
