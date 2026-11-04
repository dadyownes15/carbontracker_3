# Profiling — the timeline, attribution and settling

How measured usage becomes a span's report: where cuts fall, who claims each
stretch, and when a report is final. How usage is measured is
[08](08-measurements.md).

## The timeline

![Spans P, B, C and A over 1.25 s. The CPU and the GPU are each cut only by the spans that selected them, and NVML answers three cuts with one read.](figures/timeline.svg)

**A cut says where the bill splits; a read says what was drawn.** The two are
not paired. One read can answer many cuts, and a read can land with no cut
near it. How cuts ask for reads is [08](08-measurements.md). <!-- AC/PROF-1 -->

**Each power source has its own timeline.** A start, stop or mark on a span
cuts every power source the span selects, and no other. `finish()` cuts every
source. The session opens with a cut on every source, which asks every
provider for a read, as any cut does. That read is each source's first sample,
so the root is billed from the session's start. <!-- AC/PROF-2 -->

> A server starts a session and calls only `finish()`, an hour later. On each
> source the root has one segment, billed between the opening read and the
> read that answers `finish()`.

**A segment is the stretch of one source between two adjacent cuts on it.**
It is the unit a source is measured and billed in, and it has no readout of
its own. On each source, segments cover the session with no gap and no
overlap. A segment is half-open, `[from, to)`: a sample on a shared cut
belongs to the later segment. <!-- AC/PROF-3 -->

**Cuts called together split the bill exactly where they were stamped**, and
the same reads answer them. A zero-width segment bills nothing; a narrow one
bills its width, and waits for nothing of its own. <!-- AC/PROF-4 -->

> B stops at 3.30 and C starts at 3.31. On the CPU the 10 ms between is A's.
> On the GPU it is P's, since A never selected the GPU. Each is billed 10 ms
> of draw, and nothing waits on either.

**A span that selects nothing on this host cuts nothing** and asks no
provider. Its report is zero and warns `NoSelectedSources`. <!-- AC/PROF-5 -->

**The caller's clock stamps the cut.** `start_span`, `mark` and `stop_span`
read the time when called, on the caller's thread, so tracker latency never
moves a cut. All sources share one clock. Cuts never go backwards: one stamped
before the latest cut on any source is moved up to it, since two threads can
read the clock in one order and send in the other. <!-- AC/PROF-6 -->

**A span is active from its start cut to its stop cut.** A start cuts, then
opens the span; a stop cuts, then closes it. So the span is active in every
segment between the two on the sources it selected, and in no other. <!-- AC/PROF-7 -->

**Stopping a span closes its open descendants at the same cut.** `finish()`
places the session's last cut and closes every open span. <!-- AC/PROF-8 -->

**A mark is a cut that opens and closes nothing.** It is there for
granularity: it breaks a span's time into more segments, so the span's report
is current at that moment and there are finer stretches to attribute over.
A mark asks for reads as any cut does. The `Work` passed to a mark or a stop goes out in
`Marked` or `SpanStopped` and is not kept; under the even policy it weighs
nothing. <!-- AC/PROF-9 -->

**The root span holds what no span claims.** It opens with the session, never
closes, selects every source, and is the parent of every top-level span.
Callers cannot name it: `SpanId` 0 is refused as `UnknownSpan`. <!-- AC/PROF-10 -->

### Refusals

A refused command places no cut, asks no provider, and is announced as
`CommandRefused`. <!-- AC/PROF-11 -->

| Command | Refused when | As |
|---|---|---|
| Start | The parent was never started | `UnknownSpan` | <!-- AC/PROF-12 -->
| Start | The parent has stopped | `ParentClosed` | <!-- AC/PROF-13 -->
| Start | The selection names a source the catalog lacks | `UnknownSource` | <!-- AC/PROF-14 -->
| Stop, mark | The span has stopped | `AlreadyClosed` | <!-- AC/PROF-15 -->
| Stop, mark | The id was never handed out | `UnknownSpan` | <!-- AC/PROF-16 -->
| Any | `finish()` has been called | `Closed` | <!-- AC/PROF-17 -->

A refused start's id still counts as handed out, so later stops and marks on
it are refused as `AlreadyClosed`. <!-- AC/PROF-18 -->

## Attribution

**Each source of a segment is claimed by the innermost running spans that
selected it.** A span claims a source in a segment when it is active in the
segment, selected the source, and has no active descendant that also selected
it. With no such span, the root claims it. <!-- AC/PROF-19 -->

**A span that did not select a source has no effect on it.** Its cuts do not
split that source, and its running does not change who claims it. <!-- AC/PROF-20 -->

**A parent with a running child claims nothing.** Spans nest like a call
stack: while a child runs, the parent is taken to be waiting on it, so the
stretch is the child's. The parent's report still includes it through its
descendants; only `exclusive` leaves it out. <!-- AC/PROF-21 -->

A parent that works alongside its child, such as one that spawns a task and
polls it, opens a span of its own for that work. The two are siblings and
share the stretch.

> P (all sources) runs children A (CPU only) and B (all) in parallel, over a
> segment with 40 J of CPU and 100 J of GPU:
>
> | Source | Used | Claimed by | Billed |
> |---|---|---|---|
> | CPU | 40 J | A, B | A 20 J, B 20 J |
> | GPU | 100 J | B | B 100 J |
>
> P is billed nothing itself. A never selected the GPU, so it has no part in it.

### The attribution policy

**A policy decides each claimant's share of a source in a segment.** Shares
sum to one. There is one policy, **even**: every claimant gets an equal share,
and reported `Work` weighs nothing. <!-- AC/PROF-22 -->

### Billing a segment

**A segment is billed in three steps, each a function of what it is given.**

| Step | From | To |
|---|---|---|
| Measure | The segment and the store | Its source's energy `∫P dt`, and emissions `∫P·I dt` against the source's own intensity ([08](08-measurements.md)) | <!-- AC/PROF-23 -->
| Share | The segment's claimants | A share for each, summing to one | <!-- AC/PROF-24 -->
| Bill | The usage, the shares and the PUE | Each claimant's part: usage × share × PUE | <!-- AC/PROF-25 -->

**A segment's bill depends on nothing but the segment and the store it is
accounted on.** Not on other segments, other sources, the order commands
arrived in, or when it was accounted. When to bill is the session's to decide;
how much is decided from the segment alone. <!-- AC/PROF-26 -->

### Selection

**What a selection matches.** `All` matches every power source;
`Domains` the power sources of those domains; `Sources` exactly those sources.
A selection names power sources only. <!-- AC/PROF-27 -->

**Each power source is billed against its own intensity source, or none.**
The catalog pairs them before the tracker runs ([02](02-engine.md)), and
several power sources can share one intensity source. An intensity source is
never cut and never claimed. <!-- AC/PROF-28 -->

**A span is billed the whole draw of what it selected.** Nothing is subtracted
for idle: two spans sharing a GPU each take half of all it drew. <!-- AC/PROF-29 -->

**Every amount is scaled by the PUE**, energy and emissions alike. <!-- AC/PROF-30 -->

**Every joule measured is billed exactly once**, to one span or to the root,
times the PUE, measured on the store each segment was accounted on. Under
`Skip` a segment can be accounted on a held value that a later sample
contradicts; that later sample is not billed again. The trade-off is in
[08](08-measurements.md#held-values). <!-- AC/PROF-31 -->

## Settling

How and when a segment is accounted is [08](08-measurements.md): once its
source and that source's intensity source have both answered the cut that ends
it, in order per source.

**A span is settled once every segment it was active in, on the sources it
selected, has been accounted.** So a span that stopped waits only for the
providers of the sources it selected, and of their intensity sources, to
answer its stop. A CPU-only span never waits on a GPU read. It also waits for
its children to settle, since each child's usage is part of its own. <!-- AC/PROF-32 -->

> In the timeline figure, A stops at 3.75 and settles at once: RAPL reads at
> the stop, and DK-DK1 answers on its held value. B stops at 3.30 and settles
> at 4.02, when NVML's read 2 lands.

**A closed span settles once.** It is announced in `SpanSettled` with its final
report, its ledger never changes after, and it is forgotten: what it and its
descendants were billed folds into its parent. A child settles before, or with,
its parent, and is announced first. <!-- AC/PROF-33 -->

**An open span is settled up to its latest cut, and never announced.** A
`SpanReport` query on it is answered once every segment it was active in that
has ended, on the sources it selected, has been accounted, and no later. <!-- AC/PROF-34 -->

**`SessionStats` answers at once** with what has been accounted so far, since
every cut touches the root and waiting for it to settle could wait forever. It
holds each source up to its latest accounted segment. `finish()` answers only once every segment
is accounted. <!-- AC/PROF-35 -->

## Reports

A `SpanReport` covers the span and its descendants unless a field says
otherwise. The span's selection is applied when the report is built. <!-- AC/PROF-36 -->

| Field | Unit | Is |
|---|---|---|
| `per_source` | J and gCO2e | Usage per selected source | <!-- AC/PROF-37 -->
| `total` | J and gCO2e | Usage over the sources no other selected source subsumes | <!-- AC/PROF-38 -->
| `exclusive` | J and gCO2e | What the span was billed itself, descendants excluded | <!-- AC/PROF-39 -->
| `duration` | s | From its start cut to its stop cut, or to the query's time | <!-- AC/PROF-40 -->
| `samples` | count | Per selected power source, the samples it had inside the segments the span was active in | <!-- AC/PROF-41 -->
| `intensities` | — | The intensity sources the selected power sources are paired with; empty when none is | <!-- AC/PROF-42 -->
| `warnings` | — | What the numbers cannot be read without | <!-- AC/PROF-43 -->

Every `Usage` is `energy` in joules and `emissions` in grams of CO2e, both
after the PUE. <!-- AC/PROF-44 -->

**Emissions are absent, not zero, where no intensity is known.** A segment's
emissions are absent when its source has no intensity source, or its intensity
series has no sample by the time it is accounted. `Usage.emissions` is an
`Option`, absent only from usage with energy above zero: zero energy has zero
emissions. A sum of usages has absent emissions if any part with energy does. <!-- AC/PROF-45 -->

**`total` is not the sum of `per_source`.** A whole-device `System` rail
already contains the CPU, GPU, ANE and RAM, so a subsumed source is left out
of the total. <!-- AC/PROF-46 -->

| `ReportWarning` | When |
|---|---|
| `ThinCoverage` | Names each selected `Measured` power source with fewer than two samples inside the span; a fixed figure holds one level and is never thin | <!-- AC/PROF-47 -->
| `NoIntensity` | Names each power source that drew energy with no intensity, so emissions are absent | <!-- AC/PROF-48 -->
| `HeldIntensity` | Names each intensity source that landed no sample inside the span, so emissions rest on an earlier one | <!-- AC/PROF-49 -->
| `Estimated` | A counted source, or its intensity source, is `Modelled` or `Assumed` | <!-- AC/PROF-50 -->
| `NoSelectedSources` | The selection is not `All` and nothing on this host matched it | <!-- AC/PROF-51 -->

Inside the span means the union of its ended segments on its selected sources,
each `[from, to)`.

A `SessionReport` is the root's report. `total` is everything measured,
`claimed` the part billed to spans, and `unclaimed` the part the root took:
time with no span running, and sources no running span selected. `claimed`
plus `unclaimed` is `total`. <!-- AC/PROF-52 -->

## Why

- **Leaves claim, not every active span**, so a parent and its child are never
  billed the same joule. Concurrent work in a parent is a sibling span, not a
  rule of its own.
- **A span has no part in a source it did not select**, so selecting less
  never moves usage onto a parent that is only waiting.
- **Cuts are stamped by the caller**, so a busy tracker delays a report but
  never changes it.
- **Cuts and reads are separate**, so cuts called together cost one read per
  provider, and a sliver has nothing of its own to wait for.
- **A cut relies on a request already sent**, so a provider's work is bounded
  by its cooldown, never by how many cuts arrive.
- **A segment's bill depends only on the segment and the store**, so every
  billing property is checked on one segment, with no session and no timing.
- **Segments on a source are accounted in order**, so no segment is billed on
  a held intensity that an earlier, still-out read would replace.
- **Each power source has its own intensity source**, so it is billed against
  the grid it draws from.

## To do: attribution by work

**Not implemented, and not specified yet.** It relies on the per-source
claimants above. What is decided so far:

- A span opts in by naming a work kind when it starts, such as `tokens`.
- A mark reports the work since the span's previous mark, or its start,
  spread over the span's segments since then in proportion to their length.
  Work passed to a stop is announced only, never weighed.
- Of n claimants on a source, each that reports no work gets 1/n; those that
  report one kind split the rest by the work each reported over the segment.
  Mixed kinds fall back to even and warn `MixedWork`.
- A segment waits only when two or more claimants report one kind, until each
  has marked past its end or stopped. No timeout.
- A mark at the same moment as its span's previous mark, or its start,
  reports work over no time. Where that work goes is open.
- One work amount covers every source a span selected; uneven work across
  devices means one child span per device.

Also open, not adopted:

- **A watchdog read cuts the sources it reads**, as a mark does, so a source
  no span cuts is accounted before `finish()` and counted in `SessionStats`.
- **A held sample states until when it holds.** With an `until` from the
  publisher, a cut inside the slot is answered at once and correctly, with no
  request to the intensity's provider.

## Anchors

```anchor
carbontracker::TrackerHandle
carbontracker::SpanId
carbontracker::SourceSelection
carbontracker::PowerDomain
carbontracker::Work
carbontracker::WorkEntry
carbontracker::PowerUsageEffectiveness
carbontracker::Usage
carbontracker::SpanReport
carbontracker::SessionReport
carbontracker::ReportWarning
carbontracker::TrackerError
carbontracker::TrackerEvent
carbontracker::TrackerQuery
```
