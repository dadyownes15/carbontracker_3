//! What the engine holds, counted by the allocator: work is sent, not kept.
//! Its own binary, so the count sees only this test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Duration;

use tokio::sync::oneshot;

use carbontracker::{
    Fidelity, PowerDomain, PowerScope, Provider, ProviderError, ProviderId, ProviderResponse,
    Sample, SampleKind, SourceDescription, SourceId, SourceSelection, Sources, TrackerConfig,
    TrackerHandle, TrackerQuery, Work, start,
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

/// A CPU drawing 10 W.
struct Cpu;

impl Provider for Cpu {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let mut response = ProviderResponse::new();
        response.samples(SourceId::new(ProviderId(1), 0), vec![Sample::new(chrono::Utc::now(), 10.0)]);
        Ok(response)
    }
}

/// Wait until the tracker has handled everything sent before.
async fn caught_up(tracker: &TrackerHandle) {
    let (reply, answer) = oneshot::channel();
    tracker.query(TrackerQuery::Catalog { reply });
    answer.await.expect("running").expect("answered");
}

/// AC/ENG-33, AC/PROF-9
#[tokio::test]
async fn marks_and_stops_keep_none_of_their_work() {
    let mut sources = Sources::new();
    let cpu = SourceDescription::Power {
        device_name: "cpu".to_owned(),
        domain: PowerDomain::Cpu,
        scope: PowerScope::DeviceTotal,
        sample_kind: SampleKind::Instant,
        fidelity: Fidelity::Stated,
    };
    sources.add_provider(ProviderId(1), Cpu, [(SourceId::new(ProviderId(1), 0), cpu)]).unwrap();
    let config = TrackerConfig { readout_interval: Duration::from_secs(60), ..TrackerConfig::default() };
    let tracker = start(sources, config).expect("a valid config");
    // A span that cuts nothing here, so only its work could be kept.
    let nothing = SourceSelection::Domains(vec![PowerDomain::Ane]);
    let span = tracker.start_span("work", None, nothing.clone());
    caught_up(&tracker).await;

    let before = LIVE.load(Ordering::Relaxed);
    let kind = "k".repeat(10_000);
    for _ in 0..2_000 {
        tracker.mark(span, Work::of(&kind, 1.0));
    }
    for _ in 0..200 {
        let stopped = tracker.start_span("step", None, nothing.clone());
        tracker.stop_span(stopped, Work::of(&kind, 1.0));
    }
    caught_up(&tracker).await;
    let grown = LIVE.load(Ordering::Relaxed) - before;

    // 22 MB of work went in.
    assert!(grown < 2_000_000, "the tracker holds {grown} bytes more");
}
