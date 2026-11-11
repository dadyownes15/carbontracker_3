# Measurements — readouts, reading providers, the watchdog and the sample store

How the engine gets its numbers: when it reads, how it reads a provider, what
it keeps, and how a series becomes joules and grams. What a provider is and
what its answers mean is "The provider port" in [02](02-engine.md). Who the
numbers are billed to is [09](09-profiling.md).

## Readouts

**A readout is one request to one provider,** under a `ReadoutId` unique
within the session. A cut or the watchdog sends it. Each lands as one
`ReadoutLanded`, once the provider has answered, with an `Observation` for
each source the provider serves. Every request sent lands one, whether it was
read, skipped or failed. A cut that relies on an earlier request announces
nothing of its own. <!-- AC/MEAS-1 -->

**A refused command sends no request.** Nor does a subscription or a query. <!-- AC/MEAS-2 -->

**Readouts run concurrently and land in any order.** The engine never waits for
one readout before sending the next. <!-- AC/MEAS-3 -->

### Cuts and requests

**A cut asks the providers of the sources it cuts, and of their intensity
sources.** From each it needs one read that starts at or after it. Where cuts
fall is [09](09-profiling.md). The session's opening cut cuts every source and
asks every provider, as any cut does. <!-- AC/MEAS-4 -->

**A cut asks each provider once, and sends a request only when no request
already sent can answer it.** The first row that fits decides. <!-- AC/MEAS-5 -->

| When the cut asks, the provider | The cut relies on |
|---|---|
| 1. Has a read out or landed that started at or after the cut | That read | <!-- AC/MEAS-6 -->
| 2. Is ready | A request sent now, answered by a read started now | <!-- AC/MEAS-7 -->
| 3. Has a request queued | The queued request | <!-- AC/MEAS-8 -->
| 4. Has stayed unready since a cut's request to it was answered without a read | That answer | <!-- AC/MEAS-9 -->
| 5. Is out on a read past the timeout, or retired | A request sent now, answered `Failed` at once | <!-- AC/MEAS-10 -->
| 6. Is unready otherwise | A request sent now, which its policy queues or skips | <!-- AC/MEAS-11 -->

Row 4 covers any answer without a read, `Skipped` or `Failed`, while the
provider stays unready. **Its stretch ends at any transition:** a read
starting, returning or timing out ends it, and so does the provider being
ready. A watchdog answer without a read is never relied on. <!-- AC/MEAS-12 -->

**A cut is answered when the request it relies on is.** A read answers it with
that read's observations. A request answered without a read answers it on what
the store already holds. <!-- AC/MEAS-13 -->

**While a provider stays unready, cuts send it at most one request**, however
many arrive. A provider's work is bounded by its cooldown, never by how many
cuts there are. <!-- AC/MEAS-14 -->

**A provider whose cooldown is `Never` answers every cut at once once it has
answered with samples.** A cut sends it nothing. Until then the table applies,
except that a read out within its timeout answers the cut whenever it
started. <!-- AC/MEAS-15 -->

> NVML, cooldown 1 s, `Queue`. Read 1 starts at 3.00. Stop B at 3.30 falls
> inside the cooldown and queues a request. Start C at 3.31 and mark C at 3.90
> rely on it and send nothing. Read 2 starts at 4.00 and answers all three.

> DK-DK1, cooldown 1 min, `Skip`. Start A at 3.05 is answered `Skipped` at
> once. The provider stays unready, so every later cut in the figure relies on
> that answer, and emissions rest on the held value.

## Observations

**A provider says what it read; the engine says what became of each source.**
A provider answers with `Reading`s and `ProviderError`s, and cannot claim that
it was skipped or timed out. The engine announces `Observation`s, and never
claims that a provider had nothing new. <!-- AC/MEAS-16 -->

| `Observation` | Means | Decided by | The provider was called |
|---|---|---|---|
| `Samples(samples)` | The provider's fresh samples | Provider | Yes | <!-- AC/MEAS-17 -->
| `Unchanged` | The provider had nothing new | Provider | Yes | <!-- AC/MEAS-18 -->
| `Skipped(Skip::Cooldown)` | Not read: inside the provider's cooldown | Engine | No | <!-- AC/MEAS-19 -->
| `Skipped(Skip::Busy)` | Not read: a read is still running, inside the timeout | Engine | No | <!-- AC/MEAS-20 -->
| `Failed(Failure)` | Not read, or not readable, and something is wrong | Either | Either | <!-- AC/MEAS-21 -->

**`Skipped` is healthy; `Failed` is not.** A source that is skipped is working
as asked. A source that failed needs someone's attention.

| `Failure` | Means | Decided by |
|---|---|---|
| `Provider(ProviderError)` | The provider failed this source, or the whole read | Provider | <!-- AC/MEAS-22 -->
| `Unanswered` | The provider's response left this source out | Engine | <!-- AC/MEAS-23 -->
| `TimedOut` | The read answering this request outlived the timeout | Engine | <!-- AC/MEAS-24 -->
| `Stalled` | Not read: an earlier read is still running past the timeout | Engine | <!-- AC/MEAS-25 -->
| `Panicked(message)` | The read answering this request panicked; the provider is retired | Engine | <!-- AC/MEAS-26 -->
| `Retired` | Not read: the provider panicked in an earlier read | Engine | <!-- AC/MEAS-27 -->

**The store treats every observation but `Samples` the same:** the series is
not updated, and it fills the gap as its `SampleKind` says. Which one it was
matters to the people reading the record, not to the accounting. <!-- AC/MEAS-28 -->

## Reading providers

**The engine enforces how a provider is read.** A provider set states each
provider's cooldown and builds nothing else: no pacing, no timeout and no
thread. No provider set can break how the engine schedules reads. <!-- AC/MEAS-29 -->

**Provider code never runs on the tracker thread:** not its read, and not its
drop, also when a thread for a read cannot be started. A slow, blocking or hung read never delays commands, queries or event
delivery, and a read never delays another provider's read. Each read runs on a
thread of its own. A provider's cooldown is asked once, in `start`, before the
tracker runs. <!-- AC/MEAS-30 -->

**A provider is in one of five states,** which its gate decides at each
moment:

| State | Means | A cut |
|---|---|---|
| Ready | Idle, its cooldown passed | Sends; a read starts now | <!-- AC/MEAS-31 -->
| Unready | Cooling down, or a read out within its timeout | Rows 1, 3, 4, then its policy | <!-- AC/MEAS-32 -->
| Failing | A read out past its timeout | Sends; the request fails at once | <!-- AC/MEAS-33 -->
| Retired | A read panicked | Sends; the request fails at once | <!-- AC/MEAS-34 -->
| Done | A `Never` provider that has answered with samples | Asks nothing | <!-- AC/MEAS-35 -->

A read starting makes it Unready; the read outliving its timeout makes it
Failing; the read returning makes it Ready, or Unready while it still cools
down. A panic retires it, and a `Never` provider is Done once it has answered
with samples. Retired and Done are final. <!-- AC/MEAS-36 -->

**A provider starts idle, with its cooldown passed.** A read made while
probing does not count. A provider that kept the probe's answer reports it on
its first read. <!-- AC/MEAS-37 -->

### The fetch policy

**A provider is unready while it is inside its cooldown or out on a read.**
`TrackerConfig::fetch`, a `FetchPolicy`, sets the `timeout` every read is held
to (10 s by default; above zero), and, per provider, what the engine does with
a request to it while it is unready: `UnreadyActions { on_cooldown, on_busy }`,
each an `ActionWhenUnready` of `Skip` or `Queue`. A provider left out queues
on both if it serves power, and skips on both if it serves the intensity.
Providers never see the policy. <!-- AC/MEAS-38 -->

**Every request to a provider follows that provider's policy,** whichever of
its sources the request was for. <!-- AC/MEAS-39 -->

**A watchdog request never queues.** It only adds samples between cuts, so it
bounds nothing. <!-- AC/MEAS-40 -->

**The policy trades accuracy for settling speed, per provider.** Under
`Skip`, a cut inside the cooldown is answered at once, on the value or rate
held from before. Under `Queue`, a cut waits for a read that starts at or
after it, so the stretch up to it is billed from that read. For a
counter, or a reading taken at the read, that sample is stamped after the
cut. An average over a window can be stamped before the cut, so for those
sources the edge is still filled in. By default a power provider queues, and
the intensity's provider skips: the intensity holds as a step, and queuing
could hold every cut for up to a minute. <!-- AC/MEAS-41 -->

![One GPU energy counter and span S, 10.2 to 10.6 s, inside the cooldown. Under Queue, S settles at 11.02 and is billed 59 J. Under Skip, S settles at 10.60 and is billed 24 J. The actual draw was 112 J.](figures/skip-vs-queue.svg)

### Requests

**Every request to read a provider is answered; none is dropped.** When a
request arrives, the provider is in one of these states. <!-- AC/MEAS-42 -->

| The provider is | The request is |
|---|---|
| Idle, with its cooldown passed | Answered by a read started now | <!-- AC/MEAS-43 -->
| Idle, inside its cooldown, nothing queued | `Skipped(Cooldown)` at once, or queued | <!-- AC/MEAS-44 -->
| Out on a read inside the timeout, nothing queued | `Skipped(Busy)` at once, or queued | <!-- AC/MEAS-45 -->
| Unready, with a request already queued | `Skipped(Cooldown)` or `Skipped(Busy)` at once, whatever the policy | <!-- AC/MEAS-46 -->
| Out on a read past the timeout | `Failed(Stalled)` at once | <!-- AC/MEAS-47 -->
| Retired | `Failed(Retired)` at once | <!-- AC/MEAS-48 -->

The cooldown is measured from the start of the provider's previous read. <!-- AC/MEAS-49 -->

**A provider has at most one read in flight.** <!-- AC/MEAS-50 -->

**At most one request is queued, and only under `Queue`.** A request to an
unready provider is queued only when that provider's policy says `Queue` for
its state, cooldown or busy, and nothing is queued yet. The queued request is
answered by a read of its own, which starts once the provider is ready. A
request that arrives while one is queued is skipped, whatever the policy:
`Busy` if a read is in flight, `Cooldown` otherwise. <!-- AC/MEAS-51 -->

**Each read answers one request.** Its samples land in that request's readout
and in no other. Many cuts can rely on that request. <!-- AC/MEAS-52 -->

**Skipping behind the queue loses nothing.** The queued read starts as soon
as the provider is ready, so while requests arrive the provider is read as
often as its cooldown allows. A request skipped behind the queue could not
have been read sooner, and its read could have said nothing the queued read
does not. <!-- AC/MEAS-53 -->

> 500 requests reach NVML within one second while a read is in flight, and
> NVML's policy is `Queue`. The read in flight answers the request that started
> it. The next request is queued, and answered by the read that starts once
> NVML is ready. The other 498 are skipped.

**A queued request is answered within `max(cooldown, timeout) + timeout` of
arriving.** The read ahead of it ends within the timeout, the cooldown counts
from that read's start, and its own read ends within the timeout. <!-- AC/MEAS-54 -->

**Skipping and queuing cost no work.** A request that is skipped, queued, or
answered at once starts no thread and runs no provider code. <!-- AC/MEAS-55 -->

**Every read ends within the timeout.** When the timeout passes, the request
the read answers is answered `Failed(TimedOut)`. The read is abandoned: its
result is discarded when it returns, and the provider stays out until then. A
request queued behind it is answered `Failed(Stalled)`. <!-- AC/MEAS-56 -->

**A panicking provider is retired, alone.** The request the panicking read
answers is answered `Failed(Panicked)`, with the panic's message, and so is a
request queued behind it. Every later request is answered `Failed(Retired)`,
and the provider is never read again. Every other provider is unaffected. <!-- AC/MEAS-57 -->

### When a read fails

**Under `Queue`, a cut's request is answered only by a read with samples.**
A read that fails, times out or leaves a source unanswered does not answer
it. That read lands as a readout of its own, under a fresh id no segment
waits on, so its failure is still announced. The request stays open and is
sent again as soon as the provider is ready, until a read answers every
source it serves with samples. A segment is never billed from a store its
provider simply had not answered yet. <!-- AC/MEAS-92 -->

> NVML, `Queue`. A stop at 3.30 relies on request 7. The read for request 7
> fails with `Unavailable`: readout 8 announces the failure, and request 7 is
> sent again once NVML is ready. That read returns samples, and request 7
> lands with them; the stop's segment is billed from them.

**An open request is not sent again while the provider's read is out past
the timeout.** It is sent once that read returns. A hung read cannot make
the request fail over and over. <!-- AC/MEAS-93 -->

**An open request gives up `retry_for` after it was first sent**, 30 s by
default: it lands with what the provider's last read gave, and its segments
are billed on what the store holds, as under `Skip`. A provider that never
answers cannot hold a session open. `retry_for` is a setting of the fetch
policy; zero turns the retry off. <!-- AC/MEAS-94 -->

**A panic ends the retry.** The open request lands `Failed(Panicked)` and the
provider is retired, as above. <!-- AC/MEAS-95 -->

**Under `Skip`, a failed read answers its request.** Its segments are billed
on what the store holds, and a later cut asks the provider again. <!-- AC/MEAS-96 -->

**With `ignore_errors` off, the first failed read stops the session** with an
error the frontend raises, as 2.4.7 does. <!-- AC/MEAS-97 unbuilt -->

**A provider whose cooldown is `Never` is read until it has answered.** Its
value cannot change, so a cut while its read is out, within the timeout,
relies on that read whenever it started, and never queues. Once a read has
answered every source it serves with samples, cuts ask it nothing. <!-- AC/MEAS-58 -->

**A response that names a source the provider does not serve has that entry
dropped,** and a source it serves but left out is `Failed(Unanswered)`. A read
that fails as a whole fails every source the provider serves. <!-- AC/MEAS-59 -->

**The engine drops every provider when the session ends, off the tracker
thread.** A provider still out on an abandoned read is dropped by that read's
thread when it returns. <!-- AC/MEAS-60 -->

## The watchdog

**The watchdog is per provider.** It sends a provider a request when
`max(readout_interval, cooldown)` passes since the last request sent to it, so
it never asks inside a cooldown. <!-- AC/MEAS-61 -->

- It asks only a ready provider, and its request never queues. While the
  provider is Unready or Failing it waits, and asks once the provider is
  Ready again and the period has passed. Only Retired and Done end it. <!-- AC/MEAS-62 -->
- A missed tick is skipped, not caught up. <!-- AC/MEAS-63 -->
- It reads whether or not a span is open, so the stretches the root takes are
  measured too. <!-- AC/MEAS-64 -->
- A provider whose cooldown is `Never` is asked every `readout_interval` until
  it has answered with samples, then never. <!-- AC/MEAS-65 -->

**A watchdog readout ends no segment.** It only adds samples between cuts, so a
long segment is integrated over a curve rather than between two end points. A
cut can rely on a watchdog read. <!-- AC/MEAS-66 -->

**Providers may rely on it.** A provider is asked at least once per
`max(readout_interval, cooldown)`, and reads it unless it is unready. A counter that wraps
no faster than every few minutes is never read too rarely to see each wrap. <!-- AC/MEAS-67 -->

## The sample store

One series per source, sorted by observation time.

- Series accumulate. A sample older than a series' newest is inserted where it
  belongs, not refused; samples at the same time are ordered by value, so
  arrival order never changes a series. <!-- AC/MEAS-68 -->
- Only `Samples` are stored. Every other observation is announced and nothing
  more. <!-- AC/MEAS-69 -->
- **A sample that breaks its kind's rule is never stored.** A value that is not
  finite, a negative power or intensity, or a total that goes down against the
  totals either side of it refuses the source's whole batch, and the source is
  announced `Failed(Provider(Invalid))` in that readout. <!-- AC/MEAS-70 -->

### Sample kinds

Each source's `SourceDescription` fixes its `SampleKind`, and the store fills
its series from it; a provider never chooses how.

| `SampleKind` | The value | Between samples | Before the first, after the last |
|---|---|---|---|
| `Instant`: power | The draw at one moment, in watts | A straight line | The edge value, held | <!-- AC/MEAS-71 -->
| `Held`: intensity | gCO2e/kWh, holding from its time until the next | Each value held until the next | The edge value, held | <!-- AC/MEAS-72 -->
| `Total`: energy | A running total in joules since an origin the provider chooses | Power is the slope of each gap | The edge gap's rate, held | <!-- AC/MEAS-73 -->

**Beyond its samples a series is held, never extrapolated.** A segment that
ends after a source's latest sample is billed that source's last value, or
last rate, up to its end. <!-- AC/MEAS-74 -->

An `Instant` or `Held` series can be read once it has one sample, a `Total`
once it has two. A source that cannot be read over a segment measures nothing
there, and nothing is billed for it, not even to the root. <!-- AC/MEAS-75 -->

### Integration

Over a segment from `from` to `to`:

- A power source's energy is `∫P dt`, in joules, exact for its sample kind. <!-- AC/MEAS-76 -->
- Its emissions are `∫P·I dt` against its own intensity source's series,
  exact for every pair of readings: not mean power times mean intensity. <!-- AC/MEAS-77 -->
- Time is counted to the nanosecond. <!-- AC/MEAS-78 -->
- A zero-width segment measures nothing. <!-- AC/MEAS-79 -->

A power source with energy and no intensity source, or an intensity series
with no sample by the time the segment is accounted, bills its joules with
absent emissions, never zero ([09](09-profiling.md)). <!-- AC/MEAS-80 -->

## Accounting

![The GPU and DK-DK1 at 13.05 s. Two segments are accounted, one waits for an intensity read still out, and two wait behind it.](figures/accounting.svg)

**A segment is accounted once its source and that source's intensity source
have both answered the cut that ends it, and every earlier segment on its
source has been accounted.** A power source with no intensity source waits only
for itself. The segment is measured and billed once, on what the store holds
by then, and never revisited. <!-- AC/MEAS-81 -->

**Under `Queue`, no segment is billed from an older sample because a read
landed late.** A provider has one read in flight, so every read of it that
started before a segment's end has ended before the read that answers that
end. Under `Skip`, a segment can be accounted while an earlier read is still
out, and what that read brings changes nothing already accounted. <!-- AC/MEAS-82 -->

**Under `Queue`, a segment whose reads succeed is accounted within
`max(cooldown, timeout) + timeout` of its end**, the larger over its two
providers; one whose read fails, within `retry_for` plus that bound. A `Skip` side
answers at once. Accounting in order does not stretch this: a segment ahead of
it ended earlier, under the same bound. <!-- AC/MEAS-83 quickcheck -->

**A source no span cuts is accounted at `finish()`.** Until then its store
holds every sample since its last cut. <!-- AC/MEAS-84 -->

## The cutoff

**A series keeps only what an unaccounted segment reads.** After each readout
lands, samples before the cutoff are dropped. A power series keeps from the
start of its own oldest unaccounted segment, or its latest cut when every
segment on it is accounted. An intensity series keeps from the oldest start
among the unaccounted segments of the power sources paired with it. <!-- AC/MEAS-85 -->

- One sample before the cutoff is kept, two for a `Total` on any series, so the
  next segment can read its edge. <!-- AC/MEAS-86 -->
- Every series' latest sample is always kept. <!-- AC/MEAS-87 -->

So the store grows with the longest stretch still waiting on a readout, not
with the length of the session. <!-- AC/MEAS-88 -->

## Held values

**A segment is accounted as soon as its cut is answered, on whatever the store
holds by then.** A source that came back without samples is held at its
last value past it, and the segment is never revisited when a later sample
shows the draw had changed. <!-- AC/MEAS-89 -->

Under `Skip`, two consequences are known and kept in the suite as
`#[ignore = "known: …"]`:

- When reads are skipped, ledgers do not sum to what the samples later imply.
- A segment accounted before an earlier read lands bills its first sliver from
  an older sample, so ledgers can depend on the order readouts land in. <!-- AC/MEAS-90 -->

Under `Queue`, ledgers do not depend on the order readouts land in, as long
as each provider's readouts land in the order its requests were sent. <!-- AC/MEAS-91 -->

## Why

- **Values are held rather than awaited** because waiting for a real sample past
  every segment's end would delay every report by up to the slowest provider's
  cooldown.
- **The engine enforces the cooldown and the timeout** because a readout that
  never lands holds spans unsettled forever. The guarantee that every readout
  lands should not depend on each provider set remembering to provide it.
- **The fetch policy is the tracker's** because it changes how cuts are
  attributed, not what a source measures.
- **At most one request is queued** so that the work a provider does is bounded
  by time, not by how many cuts arrive, and each read's samples land in one
  readout.
- **Every read runs on a thread of its own** so that a blocking or hung read
  can always be abandoned, and never stalls another provider's read.
- **A provider returns to the engine only when its read does** so that one read
  in flight is a fact of ownership, not a limit someone has to apply.
- **`Reading` and `Observation` are separate types** so that each outcome has
  one owner, and neither side can report the other's decision.
- **A cut asks only the providers of what it cuts**, so a span never waits on
  a read of a source it did not select.

## Anchors

```anchor
carbontracker::TrackerConfig
carbontracker::FetchPolicy
carbontracker::UnreadyActions
carbontracker::ActionWhenUnready
carbontracker::TrackerQuery
carbontracker::TrackerEvent
carbontracker::ReadoutId
carbontracker::Observation
carbontracker::Skip
carbontracker::Failure
carbontracker::ProviderError
carbontracker::Provider
carbontracker::ProviderResponse
carbontracker::Sample
carbontracker::SampleKind
carbontracker::SourceKind
carbontracker::SourceDescription
carbontracker::SourceSelection
```
