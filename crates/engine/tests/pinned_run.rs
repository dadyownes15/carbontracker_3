//! End-to-end tests through the real actor with power and intensity pinned,
//! so expected values are exact: `WATTS * duration` joules. The sources are
//! built by hand through the public provider port, as any registry would.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::oneshot;

use carbontracker::{
    Announcements, Fidelity, GridLocation, PowerDomain, PowerScope, Provider, ProviderError,
    ProviderId, ProviderResponse, ReportWarning, Sample, SampleKind, SourceDescription, SourceId,
    SourceSelection, Sources, SpanId, SpanReport, Subscription, TrackerConfig, TrackerEvent,
    TrackerHandle, TrackerQuery, Work, start,
};

const WATTS: f64 = 10.0;
const G_CO2_PER_KWH: f64 = 100.0;
/// Its one source reads `value`, stamped at each read.
struct Constant {
    source: SourceId,
    value: f64,
}

impl Provider for Constant {
    fn read(&mut self) -> Result<ProviderResponse, ProviderError> {
        let mut response = ProviderResponse::new();
        response.samples(
            self.source,
            vec![Sample::new(chrono::Utc::now(), self.value)],
        );
        Ok(response)
    }
}

/// The whole machine drawing [WATTS], against a grid pinned at
/// [G_CO2_PER_KWH]: one provider each.
fn pinned_sources() -> Sources {
    let mut sources = Sources::new();
    let mut add = |n: usize, value: f64, description: SourceDescription| {
        let provider = ProviderId(n);
        let service = Constant {
            source: SourceId::new(provider, 0),
            value,
        };
        sources
            .add_provider(
                provider,
                service,
                [(SourceId::new(provider, 0), description)],
            )
            .expect("one power source, one grid");
    };
    add(
        0,
        WATTS,
        SourceDescription::Power {
            device_name: "pinned".to_owned(),
            domain: PowerDomain::System,
            scope: PowerScope::DeviceTotal,
            sample_kind: SampleKind::Instant,
            // Read at every interval, as a meter is.
            fidelity: Fidelity::Measured,
        },
    );
    add(
        1,
        G_CO2_PER_KWH,
        SourceDescription::Intensity {
            location: GridLocation::Global,
            fidelity: Fidelity::Stated,
        },
    );
    sources
}

/// Readouts every 100 ms.
fn config() -> TrackerConfig {
    TrackerConfig {
        readout_interval: Duration::from_millis(100),
        ..TrackerConfig::default()
    }
}

/// A tracker over [pinned_sources].
fn pinned() -> TrackerHandle {
    start(pinned_sources(), config()).expect("the config is valid")
}

/// The reports spans settled with, read off the event stream. Make it before
/// starting the spans it should see.
struct Settled {
    events: Subscription,
    reports: HashMap<SpanId, Arc<SpanReport>>,
}

impl Settled {
    fn on(tracker: &TrackerHandle) -> Self {
        Self {
            events: tracker.subscribe(),
            reports: HashMap::new(),
        }
    }

    /// `span`'s final report, waiting for it to settle.
    async fn report(&mut self, span: SpanId) -> Arc<SpanReport> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(report) = self.reports.get(&span) {
                    return report.clone();
                }
                match self.events.recv().await {
                    Ok(TrackerEvent::SpanSettled { report }) => {
                        self.reports.insert(report.span_id, report);
                    }
                    Ok(_) => {}
                    Err(ended) => panic!("event stream ended: {ended}"),
                }
            }
        })
        .await
        .expect("a stopped span settles")
    }
}

/// AC/MEAS-76
#[tokio::test]
async fn energy_is_power_times_the_span_duration() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let span = tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(600)).await;
    tracker.stop_span(span, Work::default());

    let profile = settled.report(span).await;

    let expected = WATTS * profile.duration.as_secs_f64();
    let error = (profile.total.energy.get() - expected).abs();
    assert!(
        error < 0.05,
        "expected ~{expected:.4} J over {:.3} s, got {:.4} J",
        profile.duration.as_secs_f64(),
        profile.total.energy.get()
    );
}

/// AC/MEAS-77, AC/PROF-23
#[tokio::test]
async fn emissions_are_energy_against_the_pinned_grid() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let span = tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(600)).await;
    tracker.stop_span(span, Work::default());

    let profile = settled.report(span).await;

    let kwh = profile.total.energy.get() / 3_600_000.0;
    let expected = kwh * G_CO2_PER_KWH;
    let error = (profile.total.emissions.expect("a pinned grid bills emissions").get() - expected).abs();
    assert!(
        error < 1e-9,
        "expected ~{expected:.9} gCO2e, got {:.9}",
        profile.total.emissions.expect("a pinned grid bills emissions").get()
    );
    assert!(
        profile.total.emissions.expect("a pinned grid bills emissions").get() > 0.0,
        "emissions must reach the profile"
    );
}

/// AC/ENG-15
#[tokio::test]
async fn a_settled_span_is_never_empty() {
    let tracker = pinned();
    let mut events = tracker.subscribe();

    let span = tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(300)).await;
    tracker.stop_span(span, Work::default());

    let report = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events.recv().await {
                Ok(TrackerEvent::SpanSettled { report }) => return report,
                Ok(_) => {}
                Err(ended) => panic!("event stream ended: {ended}"),
            }
        }
    })
    .await
    .expect("a span was stopped, so it must settle");

    assert_eq!(report.span_id, span);
    assert!(
        !report.per_source.is_empty(),
        "SpanSettled announced an empty report"
    );
    assert!(report.per_source.values().all(|u| u.energy.get() > 0.0));
}

/// Span identity is the id, not the name.
/// AC/ENG-4
#[tokio::test]
async fn two_spans_may_share_a_name() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let first = tracker.start_span("epoch", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(400)).await;
    tracker.stop_span(first, Work::default());

    let second = tracker.start_span("epoch", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(100)).await;
    tracker.stop_span(second, Work::default());

    assert_ne!(first, second, "same name must not mean same span");

    tokio::time::sleep(Duration::from_millis(300)).await;

    let long = settled.report(first).await;
    let short = settled.report(second).await;

    assert_eq!(long.span_name, "epoch");
    assert_eq!(short.span_name, "epoch");
    assert!(
        long.total.energy.get() > short.total.energy.get(),
        "the longer span must carry more energy: {:.4} J vs {:.4} J",
        long.total.energy.get(),
        short.total.energy.get()
    );
}

/// A parent contains its child; they must not split the overlap as peers.
/// AC/PROF-21, AC/PROF-39
#[tokio::test]
async fn a_nested_span_owns_its_own_time() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    // outer: 0..600ms, inner: 200..400ms.
    let outer = tracker.start_span("outer", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let inner = tracker.start_span("inner", Some(outer), SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(200)).await;
    tracker.stop_span(inner, Work::default());
    tokio::time::sleep(Duration::from_millis(200)).await;
    tracker.stop_span(outer, Work::default());

    tokio::time::sleep(Duration::from_millis(300)).await;

    let outer = settled.report(outer).await;
    let inner = settled.report(inner).await;

    let expected_inner = WATTS * inner.duration.as_secs_f64();
    assert!(
        (inner.total.energy.get() - expected_inner).abs() < 0.05,
        "inner should own its whole stretch: expected ~{expected_inner:.4} J, got {:.4} J",
        inner.total.energy.get()
    );

    let expected_outer = WATTS * outer.duration.as_secs_f64();
    assert!(
        (outer.total.energy.get() - expected_outer).abs() < 0.05,
        "outer should report the whole run: expected ~{expected_outer:.4} J, got {:.4} J",
        outer.total.energy.get()
    );

    let expected_exclusive = expected_outer - expected_inner;
    assert!(
        (outer.exclusive.energy.get() - expected_exclusive).abs() < 0.05,
        "outer exclusive should be the run minus its child: expected ~{expected_exclusive:.4} J, got {:.4} J",
        outer.exclusive.energy.get()
    );

    assert!((inner.exclusive.energy.get() - inner.total.energy.get()).abs() < 1e-9);
}

/// Overlapping top-level spans are peers and split the shared stretch.
/// AC/PROF-22, AC/PROF-29
#[tokio::test]
async fn concurrent_sibling_spans_split_the_overlap() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let first = tracker.start_span("tenant-a", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = tracker.start_span("tenant-b", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(400)).await;
    tracker.stop_span(second, Work::default());
    tracker.stop_span(first, Work::default());

    tokio::time::sleep(Duration::from_millis(300)).await;

    let first = settled.report(first).await;
    let second = settled.report(second).await;

    let second_j = second.total.energy.get();
    assert!(second_j > 0.0, "second span got nothing");
    assert!(
        second_j < WATTS * second.duration.as_secs_f64(),
        "a shared stretch must be split: {second_j:.4} J over {:.3} s",
        second.duration.as_secs_f64()
    );

    let total = first.total.energy.get() + second_j;
    assert!(
        total <= WATTS * first.duration.as_secs_f64() + 0.05,
        "attribution created energy: {total:.4} J over {:.3} s",
        first.duration.as_secs_f64()
    );
}

/// AC/PROF-43
#[tokio::test]
async fn a_well_sampled_span_carries_no_warnings() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let span = tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(600)).await;
    tracker.stop_span(span, Work::default());

    let profile = settled.report(span).await;

    assert!(
        profile.samples.values().all(|&n| n >= 2),
        "expected several samples, got {:?}",
        profile.samples
    );
    assert!(
        profile.warnings.is_empty(),
        "unexpected warnings: {:?}",
        profile.warnings
    );
}

/// AC/PROF-47
#[tokio::test]
async fn a_span_shorter_than_the_interval_warns_about_its_coverage() {
    // A long interval, so no watchdog read lands inside the span however
    // long a loaded machine stretches it.
    let config = TrackerConfig { readout_interval: Duration::from_secs(10), ..config() };
    let tracker = start(pinned_sources(), config).expect("the config is valid");
    let mut settled = Settled::on(&tracker);

    // Far shorter than the interval.
    let span = tracker.start_span("blink", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(5)).await;
    tracker.stop_span(span, Work::default());

    let profile = settled.report(span).await;

    assert!(
        profile.samples.values().any(|&n| n < 2),
        "expected thin coverage, got {:?} samples",
        profile.samples
    );
    assert!(
        profile
            .warnings
            .iter()
            .any(|w| matches!(w, carbontracker::ReportWarning::ThinCoverage { .. })),
        "a span this short must warn, got {:?}",
        profile.warnings
    );
    let text = profile.warnings[0].to_string();
    assert!(text.contains("sample"), "unhelpful warning text: {text}");
}

/// The pinned source is `System`, so selecting it matches the default.
/// AC/PROF-27
#[tokio::test]
async fn selecting_the_resolved_domain_reports_what_the_default_does() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let selected = tracker.start_span(
        "work",
        None,
        SourceSelection::Domains(vec![PowerDomain::System]),
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    tracker.stop_span(selected, Work::default());

    let profile = settled.report(selected).await;

    let expected = WATTS * profile.duration.as_secs_f64();
    let error = (profile.total.energy.get() - expected).abs();
    assert!(
        error < 0.05,
        "expected ~{expected:.4} J over {:.3} s, got {:.4} J",
        profile.duration.as_secs_f64(),
        profile.total.energy.get()
    );
    assert!(
        profile.warnings.is_empty(),
        "unexpected warnings: {:?}",
        profile.warnings
    );
}

/// AC/PROF-5, AC/PROF-51
#[tokio::test]
async fn selecting_a_domain_this_host_does_not_measure_says_so() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let span = tracker.start_span(
        "work",
        None,
        SourceSelection::Domains(vec![PowerDomain::Gpu]),
    );
    tokio::time::sleep(Duration::from_millis(600)).await;
    tracker.stop_span(span, Work::default());

    let profile = settled.report(span).await;

    assert_eq!(profile.total.energy.get(), 0.0);
    assert!(
        profile.warnings.contains(&ReportWarning::NoSelectedSources),
        "expected the empty selection to be reported, got {:?}",
        profile.warnings
    );
}

/// AC/PROF-36
#[tokio::test]
async fn a_narrow_child_does_not_narrow_its_parent() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);

    let parent = tracker.start_span("outer", None, SourceSelection::All);
    let child = tracker.start_span(
        "inner",
        Some(parent),
        SourceSelection::Domains(vec![PowerDomain::Gpu]),
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    tracker.stop_span(child, Work::default());
    tokio::time::sleep(Duration::from_millis(200)).await;
    tracker.stop_span(parent, Work::default());

    let child_profile = settled.report(child).await;
    let parent_profile = settled.report(parent).await;

    assert_eq!(
        child_profile.total.energy.get(),
        0.0,
        "the child asked about a GPU"
    );
    let expected = WATTS * parent_profile.duration.as_secs_f64();
    let error = (parent_profile.total.energy.get() - expected).abs();
    assert!(
        error < 0.05,
        "the parent still measured the machine: expected ~{expected:.4} J, got {:.4} J",
        parent_profile.total.energy.get()
    );
}

/// AC/ENG-22
#[tokio::test]
async fn the_catalog_names_the_intensity() {
    let tracker = pinned();
    let (reply, answer) = oneshot::channel();
    tracker.query(TrackerQuery::Catalog { reply });
    let catalog = answer.await.unwrap().expect("tracker is running");

    let intensity = *catalog.pairs.values().next().expect("a pinned grid serves intensity");
    assert!(matches!(
        catalog.sources[&intensity],
        SourceDescription::Intensity { .. }
    ));
}

/// An open span can be asked about; a closed one's report is its settle event.
/// AC/ENG-17, AC/ENG-20, AC/ENG-23
#[tokio::test]
async fn an_open_span_reports_so_far_and_a_stopped_one_is_refused() {
    let tracker = pinned();
    let span = tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let profile = |span| {
        let (reply, answer) = oneshot::channel();
        tracker.query(TrackerQuery::SpanReport {
            span,
            at: chrono::Utc::now(),
            reply,
        });
        answer
    };
    let so_far = profile(span)
        .await
        .unwrap()
        .expect("an open span is answered");
    assert_eq!(so_far.span_id, span);

    tracker.stop_span(span, Work::default());
    assert!(matches!(
        profile(span).await.unwrap(),
        Err(carbontracker::TrackerError::AlreadyClosed(_))
    ));
}

/// A hot loop that never yields must not cost the tracker its messages: the
/// tracker runs on its own thread, not on the caller's runtime. `#[tokio::test]`
/// is single-threaded, so a tracker sharing it would not run until the
/// wait below, by which time a bounded queue would have dropped the stop.
/// AC/ENG-1, AC/ENG-7
#[tokio::test]
async fn a_loop_that_never_yields_loses_no_marks_or_stops() {
    // Only settle events: a thousand marks would otherwise fill the queue
    // while this thread sleeps, and cut the subscriber off.
    let announce = Announcements {
        readouts: false,
        cuts: false,
        ..Announcements::default()
    };
    let tracker = start(
        pinned_sources(),
        TrackerConfig {
            announce,
            ..config()
        },
    )
    .expect("the config is valid");
    let mut settled = Settled::on(&tracker);
    let span = tracker.start_span("train", None, SourceSelection::All);

    for _ in 0..1_000 {
        tracker.mark(span, Work::of("steps", 1.0));
    }
    std::thread::sleep(Duration::from_millis(200));
    tracker.stop_span(span, Work::default());

    let report = settled.report(span).await;
    assert!(
        report.total.energy.get() > 0.0,
        "the stop arrived, so the span settled"
    );
}

/// Finishing settles every open span, ends the stream, and stops the tracker.
/// AC/ENG-16, AC/ENG-26, AC/PROF-8, AC/PROF-33
#[tokio::test]
async fn finishing_settles_everything_then_stops() {
    let tracker = pinned();
    let mut events = tracker.subscribe();
    let outer = tracker.start_span("outer", None, SourceSelection::All);
    tracker.start_span("inner", Some(outer), SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(300)).await;

    let report = tracker.finish().await.expect("the session finishes");
    assert!(report.total.energy.get() > 0.0);

    let mut seen = Vec::new();
    let ended = loop {
        match events.recv().await {
            Ok(TrackerEvent::SpanSettled { report }) => seen.push(report.span_name.clone()),
            Ok(TrackerEvent::SessionEnded { .. }) => seen.push("ended".to_owned()),
            Ok(_) => {}
            Err(ended) => break ended,
        }
    };
    assert_eq!(
        seen,
        ["inner", "outer", "ended"],
        "children, parents, then the end"
    );
    assert_eq!(ended, carbontracker::SubscriptionEnded::Closed);

    assert!(matches!(
        tracker.finish().await,
        Err(carbontracker::TrackerError::Closed)
    ));
}

/// A start can't be refused to its caller, which already has the id: it is
/// announced, naming that id.
/// AC/ENG-4, AC/ENG-14, AC/PROF-13
#[tokio::test]
async fn a_refused_start_is_announced_with_its_own_id() {
    let tracker = pinned();
    let mut settled = Settled::on(&tracker);
    let parent = tracker.start_span("outer", None, SourceSelection::All);
    tracker.stop_span(parent, Work::default());
    settled.report(parent).await;

    let child = tracker.start_span("inner", Some(parent), SourceSelection::All);
    let refused = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match settled.events.recv().await {
                Ok(TrackerEvent::CommandRefused { span, error }) => break (span, error),
                Ok(_) => {}
                Err(ended) => panic!("event stream ended: {ended}"),
            }
        }
    })
    .await
    .expect("the refusal is announced");
    assert_eq!(refused.0, child);
    assert!(matches!(refused.1, carbontracker::TrackerError::ParentClosed(id) if id == parent));
}

/// Starting a span never waits on the tracker either: a loop of starts and
/// stops that never yields loses none of them.
/// AC/ENG-1, AC/ENG-4, AC/ENG-7
#[tokio::test]
async fn a_loop_that_never_yields_loses_no_starts_or_stops() {
    let announce = Announcements {
        readouts: false,
        cuts: false,
        ..Announcements::default()
    };
    let tracker = start(
        pinned_sources(),
        TrackerConfig {
            announce,
            ..config()
        },
    )
    .expect("the config is valid");
    let mut settled = Settled {
        events: tracker.subscribe_with_capacity(4096),
        reports: HashMap::new(),
    };

    let spans: Vec<SpanId> = (0..1_000)
        .map(|_| {
            let span = tracker.start_span("step", None, SourceSelection::All);
            tracker.stop_span(span, Work::default());
            span
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));

    for span in spans {
        settled.report(span).await;
    }
}

/// Sinks write reports and events as JSON, so sources must key maps as strings.
/// AC/ENG-13, AC/ENG-35
#[cfg(feature = "serde")]
#[tokio::test]
async fn reports_and_events_serialize_to_json() {
    let tracker = pinned();
    let mut events = tracker.subscribe();
    let span = tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(300)).await;
    tracker.mark(span, Work::of("steps", 2.0));
    tracker.stop_span(span, Work::default());

    let mut kinds = std::collections::HashSet::new();
    let report = loop {
        let event = events.recv().await.expect("the stream stays open");
        let json = serde_json::to_value(&event).expect("an event serializes");
        kinds.extend(
            json.as_object()
                .and_then(|tagged| tagged.keys().next().cloned()),
        );
        if let TrackerEvent::SpanSettled { report } = event {
            break report;
        }
    };

    let json = serde_json::to_value(&report).expect("a report serializes");
    assert_eq!(json["span_name"], "work");
    assert!(
        json["total"]["energy"]
            .as_f64()
            .is_some_and(|joules| joules > 0.0)
    );
    let per_source = json["per_source"].as_object().expect("sources key a map");
    assert!(per_source.contains_key("0.0"), "a source is keyed provider.slot: {per_source:?}");
    assert!(per_source["0.0"]["energy"].is_f64(), "a unit is a bare number");

    for kind in [
        "SpanStarted",
        "SpanStopped",
        "Marked",
        "ReadoutLanded",
        "SpanSettled",
    ] {
        assert!(kinds.contains(kind), "no {kind} among {kinds:?}");
    }

    // Work is a list on the wire, however many kinds it names.
    let marked = serde_json::to_value(TrackerEvent::Marked {
        span,
        work: Work::of("steps", 2.0),
        at: chrono::Utc::now(),
    })
    .expect("an event serializes");
    assert_eq!(
        marked["Marked"]["work"],
        serde_json::json!([{ "kind": "steps", "amount": 2.0 }])
    );
}

/// AC/ENG-7, AC/PROF-6
#[tokio::test]
async fn a_cut_is_stamped_when_called_not_when_the_tracker_handles_it() {
    let tracker = pinned();
    let mut events = tracker.subscribe_with_capacity(65_536);
    // Marks on a span that cuts nothing keep the tracker busy and cost no read.
    let busy = tracker.start_span("busy", None, SourceSelection::Domains(vec![PowerDomain::Ane]));
    for _ in 0..20_000 {
        tracker.mark(busy, Work::default());
    }
    let before = chrono::Utc::now();
    let span = tracker.start_span("work", None, SourceSelection::All);
    let after = chrono::Utc::now();

    let at = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(TrackerEvent::SpanStarted { span: started, at, .. }) = events.recv().await
                && started == span
            {
                return at;
            }
        }
    })
    .await
    .expect("the start is announced");
    assert!(before <= at && at <= after, "stamped at {at}, called between {before} and {after}");
}

/// AC/ENG-18
#[tokio::test]
async fn a_cut_off_subscriber_may_subscribe_again_and_gets_no_replay() {
    let announce = Announcements { readouts: false, ..Announcements::default() };
    let tracker = start(pinned_sources(), TrackerConfig { announce, ..config() }).expect("the config is valid");
    let mut first = tracker.subscribe_with_capacity(1);
    let early = tracker.start_span("early", None, SourceSelection::All);
    tracker.mark(early, Work::default());
    tracker.mark(early, Work::default());
    // Not read meanwhile, so the queue of one fills.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Err(ended) = first.recv().await {
                return ended;
            }
        }
    })
    .await
    .expect("the full queue is cut off");
    assert_eq!(ended, carbontracker::SubscriptionEnded::CutOff);

    let mut second = tracker.subscribe();
    let late = tracker.start_span("late", None, SourceSelection::All);
    match second.recv().await {
        Ok(TrackerEvent::SpanStarted { span, .. }) => assert_eq!(span, late, "nothing from before"),
        other => panic!("expected the late span's start first, got {other:?}"),
    }
}

/// AC/ENG-26
#[tokio::test]
async fn a_finish_while_finishing_waits_for_the_same_report() {
    let tracker = pinned();
    let mut events = tracker.subscribe();
    tracker.start_span("work", None, SourceSelection::All);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (first, second) = tokio::join!(tracker.finish(), tracker.finish());
    let (first, second) = (first.expect("finished"), second.expect("finished too"));
    assert_eq!(first.total.energy.get(), second.total.energy.get());
    assert_eq!(first.duration, second.duration);

    let mut ended = 0;
    while let Ok(event) = events.recv().await {
        ended += usize::from(matches!(event, TrackerEvent::SessionEnded { .. }));
    }
    assert_eq!(ended, 1, "one SessionEnded");
    assert!(matches!(tracker.finish().await, Err(carbontracker::TrackerError::Closed)));
}
