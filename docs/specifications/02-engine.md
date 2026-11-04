# The engine — `carbontracker`

A closed core with two outputs: **events** pushed to subscribers, and
**reports** pulled by queries.

## Starting

`start(sources, config)` takes the sources to measure and a validated
`TrackerConfig`, puts a tracker on its own runtime, and returns a
`TrackerHandle`. The engine never probes; building `Sources` is a registry's
job. <!-- AC/ENG-1 -->

`TrackerConfig` is `readout_interval` (the longest gap between readouts; must
be above zero), `pue` (scales every span's energy and emissions), `announce`,
and `fetch`, the `FetchPolicy` providers are read under (see
[08](08-measurements.md)). `validate()` decides whether a config is usable;
`start` also refuses a fetch policy that names a provider the `Sources` do not
have. <!-- AC/ENG-2 -->

`Announcements` is the engine's verbosity: which kinds of event it announces —
`readouts`, `cuts`, `settled`, `refusals`. All are on by default. Turning one
off saves the work rather than hiding it: the payload is never built. <!-- AC/ENG-3 -->

## Cutting the timeline

| Call | Answers | Refusal |
|---|---|---|
| `start_span(name, parent, sources)` | A `SpanId`, at once | Announced as `CommandRefused` | <!-- AC/ENG-4 -->
| `mark(span, work)` | Nothing | Announced as `CommandRefused` | <!-- AC/ENG-5 -->
| `stop_span(span, work)` | Nothing | Announced as `CommandRefused` | <!-- AC/ENG-6 -->

All three are fire-and-forget: they take their own timestamp so tracker latency
never skews a cut, and they never block the caller. <!-- AC/ENG-7 -->

A span's `SourceSelection` is fixed when it opens. Stopping a span stops its
open descendants with it. <!-- AC/ENG-8 -->

## Events

Every subscriber gets its own copy of each event; large payloads are shared
rather than copied. <!-- AC/ENG-9 -->

| Event | Carries |
|---|---|
| `ReadoutLanded` | One request to one provider: per source it serves, its `Observation`: samples, nothing new, skipped, or failed | <!-- AC/ENG-10 -->
| `SpanStarted` | The span, its name, parent, selection and time | <!-- AC/ENG-11 -->
| `SpanStopped` | The span, the work it reported, and the time | <!-- AC/ENG-12 -->
| `Marked` | Work reported on an open span | <!-- AC/ENG-13 -->
| `CommandRefused` | The span and the `TrackerError` | <!-- AC/ENG-14 -->
| `SpanSettled` | The span's **final** report | <!-- AC/ENG-15 -->
| `SessionEnded` | The session report; the last event, the stream ends after it | <!-- AC/ENG-16 -->

`SpanSettled` is the only place a closed span's report exists. A frontend that
wants one subscribes before it stops the span and matches on span id. <!-- AC/ENG-17 -->

### Delivery

Each subscriber gets its own bounded queue, sized by the subscriber
(`subscribe_with_capacity`, or `subscribe` for the default). The engine sends
without ever waiting. **A subscriber whose queue fills is cut off**, and its
stream ends with a signal it can tell apart from the session ending. It may
subscribe again, and receives events from the next one on, with no replay. <!-- AC/ENG-18 -->

**A subscription taken at start sees every event.** `start_subscribed`
returns the handle together with a subscription that exists before the
engine reads anything, so it hears the opening readouts too. A subscription
taken later with `subscribe` hears what comes after it, which can be after
the opening readouts have landed. <!-- AC/ENG-65 -->

Keeping up is the subscriber's job. A cut-off subscriber has lost those events
for good: they cannot be asked for again. A sink that must be complete drains
its queue into a buffer of its own.

## Queries

Every query is about the present. `query(TrackerQuery)` carries the reply
channel. <!-- AC/ENG-19 -->

| Query | Answers |
|---|---|
| `SpanReport` | An **open** span's report so far, as of its latest accounted cut | <!-- AC/ENG-20 -->
| `SessionStats` | The session's running totals | <!-- AC/ENG-21 -->
| `Catalog` | The sources measured, and `pairs`: each power source's intensity source | <!-- AC/ENG-22 -->

A closed span is refused as `AlreadyClosed`, settled or not — its report is in
its event. Only an id never handed out is `UnknownSpan`. There is no query for
samples; samples are only streamed. <!-- AC/ENG-23 -->

**The catalog pairs each power source with one intensity source, or none,
before the tracker runs.** Several power sources can share one intensity
source. The pairing never changes: the catalog has no setter. <!-- AC/ENG-24 -->

**The engine has no forecast.** It reads, stores and bills power and
intensity. A forecast is never billed, so it is not a source: a run asks a
`Forecaster` for it (see [03](03-providers.md)). <!-- AC/ENG-25 -->

## Ending

`finish()` places one final cut, stops every open span, waits for every span to
settle, sends `SessionEnded`, and returns the `SessionReport`. A second
`finish()` that arrives while finishing waits for the same report; after the
tracker has stopped, every call is refused as `Closed`. The handle keeps no
copy of the report. <!-- AC/ENG-26 -->

**`finish()` waits no longer than its final cut's requests can take:**
`max(cooldown, timeout) + timeout` for the slowest provider that queues, and
`timeout` for the others. A read still running after the session ends is
abandoned. <!-- AC/ENG-27 -->

Dropping every handle without `finish()` shuts the engine down with no report
and no `SessionEnded`. <!-- AC/ENG-28 -->

## Memory

The engine's footprint does not grow with the length of a session or the number
of spans that have run. <!-- AC/ENG-29 -->

- Samples older than the earliest point any segment still needs are dropped,
  keeping one sample before that point so the next segment can interpolate.
  The latest sample of every series is always kept. <!-- AC/ENG-30 -->
- Facts a report needs from samples are recorded as a segment is accounted, not
  read back later. <!-- AC/ENG-31 -->
- A settled span's usage folds into its parent's descendants total; the span is
  then forgotten. An open parent's report reads its own ledger plus that total. <!-- AC/ENG-32 -->
- Marks and stop work are sent in their events and not stored. <!-- AC/ENG-33 -->
- The root span is never forgotten; its ledger is bounded by the source count. <!-- AC/ENG-34 -->

**Settling costs no more while many closed spans wait on readouts.** Each
pass checks only the spans just closed or just fully accounted, and their
ancestors, so a long epoch of short spans settles in time linear in its spans. <!-- AC/ENG-66 -->

`held()` reports what the tracker is holding, for tests.

## Serialization

The `serde` feature turns on `Serialize` for every exported type. Serialize
only: nothing outside the engine builds engine types from JSON, and
deserializing units would skip their validation. A `SourceId` serializes as
`provider.slot`, so maps keyed by source become JSON objects. Unit newtypes
serialize as bare numbers. <!-- AC/ENG-35 -->

## The provider port

`Sources` has a public, checked constructor: `add_provider(id, provider,
sources)` adds a provider with the sources it serves, and roles follow from
descriptions. A registry builds `Sources`; the engine reads it. When a
provider is read is [08](08-measurements.md); this section says what a
provider is and what its answers mean. <!-- AC/ENG-36 -->

### What a provider is

**A provider reads its sources.** It knows how to get a value from a device,
a file or a service, and nothing about when it will be asked.

```rust
pub trait Provider: Send + 'static {
    /// Read every source this provider serves. May block.
    fn read(&mut self) -> Result<ProviderResponse, ProviderError>;

    /// How long a reading stays current. Asked once, when the tracker starts.
    fn cooldown(&self) -> Cooldown { Cooldown::After(Duration::ZERO) }
}

pub enum Cooldown {
    /// Read again once this long has passed since the last read began.
    After(Duration),
    /// Read once: no later read can say anything new.
    Never,
}
```

**A provider's read may block.** The engine lends each read a thread, for that
read alone. A provider that waits on a network or a process blocks; one that
wants async code runs it on a runtime it owns. <!-- AC/ENG-37 -->

**A provider bounds its own waits.** It gives an HTTP client or a child
process a timeout of its own, of a few seconds, so that its read returns
rather than being abandoned by the engine. <!-- AC/ENG-38 -->

**A provider owns everything that outlives a read:** connections, sampler
threads, driver handles, and any runtime it runs on. That is what lets a
registry build it while probing, and hand it to the engine whole. A panic on
a thread the provider owns is the provider's to handle: a sampler that dies
makes later reads fail, not hang. <!-- AC/ENG-39 -->

**A provider serves exactly the sources it was handed over with,** all of one
kind: power, or intensity. Every read answers every one of them. Sources that
need different rules to be measured correctly, such as a different cooldown,
are served by separate providers. <!-- AC/ENG-40 -->

**A provider is dropped when the session ends,** so it can close what it owns. <!-- AC/ENG-41 -->

### What the engine promises a provider

- **A provider is never read concurrently.** `&mut self` makes this a fact of
  the type. <!-- AC/ENG-42 -->
- **A provider is never read sooner than its cooldown allows.** The cooldown
  is measured from the start of its previous read. <!-- AC/ENG-43 -->
- **A provider is never read, asked or dropped on the tracker thread.** It may
  block in any of them. <!-- AC/ENG-44 -->
- **A provider is asked at least once per `max(readout_interval, cooldown)`**
  by the watchdog, and read unless it is unready. <!-- AC/ENG-45 -->

### What a read returns

**Every read answers every source the provider serves, with exactly one
`Reading` each,** in a `ProviderResponse`. A read that fails as a whole
returns one `ProviderError`, and that fails every source the provider serves.
A response that leaves a source out, or names one the provider does not
serve, is a defect in the provider. <!-- AC/ENG-46 -->

| `Reading` | Means |
|---|---|
| `Samples(samples)` | New observations of the source since this provider's last read; never empty | <!-- AC/ENG-47 -->
| `Unchanged` | Read, with no new observation since the last read | <!-- AC/ENG-48 -->
| `Failed(ProviderError)` | This source could not be read, or what it read cannot be used | <!-- AC/ENG-49 -->

**A provider reports failure as a `ProviderError`, never by panicking.** <!-- AC/ENG-50 -->

### What a sample means

**A sample's meaning is fixed by its source's `SampleKind`.** The store fills
the gaps between samples from that meaning ([08](08-measurements.md)). <!-- AC/ENG-51 -->

| `SampleKind` | Stamped at | Must hold |
|---|---|---|
| `Instant`: power | The moment the draw describes; a reading averaged over a window is stamped at the window's middle | Finite, at least zero | <!-- AC/ENG-52 -->
| `Total`: energy | When the counter was looked at | Finite, never decreasing, exact across every gap between samples | <!-- AC/ENG-53 -->
| `Held`: intensity | The time from which the value holds, as the publisher states it | Finite, at least zero | <!-- AC/ENG-54 -->

**Only the difference between two samples of a total means anything.** The
origin is the provider's choice. <!-- AC/ENG-55 -->

**A counter is read at the time of the read, and kept exact across wraps.**
The read takes the hardware counter as it is then, adds it to the provider's
own running total, accounting for every wrap since it was last looked at, and
reports that total. The provider relies on the watchdog to look often enough
to see every wrap. <!-- AC/ENG-56 -->

**Energy a counter cannot account for is reported, never hidden.** When the
provider cannot know how much energy passed between two looks, as after a
reset or a wrap of unknown size, that read answers `Failed(Invalid)` with a
detail that says why. Its next read reports samples again, from a total that
continues past the last one reported, so the series never decreases. The
store fills that gap like any other: it bills the energy counted from then on,
an undercount the record shows. <!-- AC/ENG-57 -->

**A sample is a new observation, not a new value.** Each sample records a look
at the source that no earlier sample recorded. Reading a counter is always a
new observation; a published value is new only when its time is. <!-- AC/ENG-58 -->

### Why a provider could not read

| `ProviderError` | Means | What a user does | For example |
|---|---|---|---|
| `Unavailable(detail)` | The device, file or service could not be reached | Wait, or check the host | Network down, driver unloaded, counter file gone | <!-- AC/ENG-59 -->
| `Denied(detail)` | Reached, and access was refused | Fix access | No root for powermetrics, a rejected API key | <!-- AC/ENG-60 -->
| `RateLimited(detail)` | The service refused because it was asked too often | Wait | Electricity Maps answers 429 | <!-- AC/ENG-61 -->
| `Invalid(detail)` | It answered, with something that cannot be used | Report it | Unparseable output, a negative reading, a counter that reset | <!-- AC/ENG-62 -->

**Every variant carries a detail for people.** The variant is for programs and
the record; the detail is what a frontend shows. <!-- AC/ENG-63 -->

### What a provider states about itself

**A cooldown is how long a reading stays current.** Within it, a new read
cannot say anything honest that the last one did not. It is no shorter than
the gap at which the source is measured to be accurate, meaning the energy
over a run stays within 2% of what long gaps between reads give. Each default
provider's cooldown, and the evidence for it, is in [03](03-providers.md). <!-- AC/ENG-64 live-only -->

## Why

- **Queries are about the present only** so that what the engine must remember
  is bounded by what is open, not by what has happened. The same holds for
  requests and answers: one is kept only while a segment still waits on it.
- **A full queue cuts the subscriber off** rather than dropping its oldest
  events, because a silent gap in a record is worse than a known end.
- **The provider port says nothing about when** so that a provider's
  correctness never depends on how often it is read.
- **A read may block** because a thread per read is cheap for a handful of
  providers, and a blocking read can always be abandoned.
- **Every read answers every source** so that no source can starve.
- **Averages are stamped at the middle of their window** because stamping them
  at the end bills half a window's draw to the wrong side of a cut.
- **An unaccounted gap fails the read** because silently lost energy is worse
  than a known gap in the record. It is an ordinary failure, not one of its
  own, so the engine needs no rule for a case this rare.
- **The forecast is not the engine's** because it is never billed.
- **`unnameable_types` is on**, so a public type that cannot be named fails the
  lint and cannot leak into the interface by accident.

## Anchors

```anchor
carbontracker::start
carbontracker::start_subscribed
carbontracker::TrackerHandle
carbontracker::WeakTrackerHandle
carbontracker::Subscription
carbontracker::SubscriptionEnded
carbontracker::TrackerConfig
carbontracker::FetchPolicy
carbontracker::Announcements
carbontracker::ConfigError
carbontracker::TrackerEvent
carbontracker::TrackerQuery
carbontracker::TrackerError
carbontracker::Reply
carbontracker::SpanId
carbontracker::SpanReport
carbontracker::SessionReport
carbontracker::CatalogReport
carbontracker::SourceSelection
carbontracker::SourceId
carbontracker::SourceDescription
carbontracker::Sources
carbontracker::Provider
carbontracker::Cooldown
carbontracker::ProviderResponse
carbontracker::Reading
carbontracker::ProviderError
carbontracker::Sample
carbontracker::SampleKind
carbontracker::Work
carbontracker::WorkEntry
carbontracker::Usage
```
