# The profiler — `CarbonTracker(mode="profiler")`

`CarbonTracker(mode="profiler")` measures the electricity of short spans that
run in order, such as the stages of a pipeline. It reports joules and watts,
contacts no network service, and reports no emissions. It is built in the
Python package ([10](10-python.md)): the engine and the providers are the
same as in tracker mode. <!-- AC/PRF-1 -->

## Which to use

**A span lasting much longer than its slowest source's counter period is
measured well without waiting.** Only the counter windows at its two ends
blur into its neighbours. <!-- AC/PRF-2 live-only -->

| You measure | Use | Costs |
|---|---|---|
| Epochs, evaluation, a whole run | Tracker mode | Nothing |
| One stage that can run alone | Tracker mode, the stage repeated inside one span | The repeats |
| Stages that must run in order | Profiler mode | Up to one counter period per span |

> A stage takes 0.4 s and NVML's energy counter is read at most once a
> second. Repeated 15 times inside one span, the span lasts 6 s, and only the
> windows at its two ends blur. Divided by 15, that is one call's energy.

## Waiting for reads

![Two spans on one GPU counter, cooldown 1 s, idle 60 W. In tracker mode one read window covers both, and both are billed its mean: tokenize 23 J against 16.5 J, forward 62 J against 112 J. In profiler mode each span stays open until a read lands, and the idle draw over its window is subtracted: 16.5 J and 112 J gross, in 2.0 s instead of 0.55 s.](figures/profiler-wait.svg)

**The profiler knows each source's cooldown.** Every provider declares how
soon after a read it can be read again; the run passes that cooldown on for
each source, and the profiler waits by it. The user configures no cooldowns:
about 10 ms on RAPL, 100 ms on macmon, 1 s on NVML's energy counter.
<!-- AC/PRF-3 -->

**A source is ready one cooldown after its newest sample.** The profiler
takes the newest sample's stamp as the last read, which is never earlier
than the read's start, so it never cuts while a source is still cooling down.
<!-- AC/PRF-4 -->

**A profiled span stops on a read.** When a span closes, the frontend
synchronizes the accelerator streams, notes the time the work ended, and cuts
the span to ask every measured power source it selects for a read. It keeps
the span open until each of them has landed as many samples stamped after the
work ended as it needs, then stops it. The span's window then holds the read window its work
fell in, so no neighbour shares it. <!-- AC/PRF-5 -->

**A span that overlaps a sibling neither waits nor stalls it.** When another
span is open that is neither its ancestor nor its descendant, such as a
second request in a server, a span starts and stops at once, as in tracker
mode, and its boundary inside the overlap blurs. Spans in order and nested
spans still wait. <!-- AC/PRF-35 -->

**A profiled span starts on a read.** If the newest sample of every measured
power source the span selects is younger than 1 % of that source's cooldown,
the span starts at once: this is a span that follows another, and the read
that ended one starts the next. Otherwise the frontend waits until every such
source is ready, starts the span, and returns once the read its start asked
for has landed. <!-- AC/PRF-6 -->

> forward starts at 1.0 s, on the read that ended tokenize, and waits for
> nothing. A span started 0.5 s after unprofiled code waits until 1.0 s, when
> NVML is ready, so its start is read at once and that code's draw stays with
> the parent.

**A counter needs one sample after a cut; any other source two.** A
counter's sample is the total at its read, so the first one after a cut
closes the stretch before it. A window's figure is stamped inside its window,
and a straight line runs between samples, so the first sample after a cut can
still hold work from before it; the second does not. <!-- AC/PRF-7 -->

> On macmon's 100 ms windows, heavy 0.15 s stages waiting for one sample were
> billed 24 % low and 30 % high in two runs, their energy landing in the next
> stage; waiting for two, within 1 % of the stage repeated in one span.

**While a source is late, it is asked again.** A source past its cooldown
with no fresh sample has no read queued, so the frontend cuts the span again
every 10 ms until the samples it needs land. <!-- AC/PRF-8 -->

**Only sources that can be read again are waited for.** A stated, modelled
or assumed source is read once and holds its value; it has no fresher sample
to wait for. A run with no such power source never waits. <!-- AC/PRF-9 -->

**A late read is known, not waited out.** A read the profiler waits for is
due by the time its source is ready plus the run's fetch timeout. Past that
the span stops anyway, and its report warns that it was not isolated, naming
the source. <!-- AC/PRF-10 -->

**Waits happen on the caller's thread.** Starting, stopping and marking stay
fire-and-forget for the engine; only the frontend's `with` blocks. <!-- AC/PRF-11 untested -->

**The profiler adds no spans of its own.** The wait before a start is billed
to the span's parent, as any time outside the span is. <!-- AC/PRF-12 -->

**A profiler measures components, never a whole-system total.** With
`components="all"` it measures CPU, GPU, RAM and the neural engine; a source
of the whole system, such as macmon's, is left out. A system total carries
the display, disks and other processes, and that noise swamps a short span's
dynamic energy. <!-- AC/PRF-13 -->

## The baseline

**The baseline is measured once, at the start,** unless `baseline_w` states
it. The frontend runs one profiled, idle span of `baseline_s` (1 s) before the
tracker returns. The baseline power
`P_base` is that span's energy over its window, across the measured power
sources the session selects. <!-- AC/PRF-14 -->

**Every profiled span has its baseline subtracted,** unless
`subtract_baseline=False`. For a span with window
`W`, from its start cut to its stop cut, and measured energy `E`, the engine's
total for the span: <!-- AC/PRF-15 -->

| Value | Is |
|---|---|
| Work time `t` | `W` less the waits inside the span, its own and its descendants' |
| Share `s` | The part of `W` that is the span's own: at each moment, the part of the innermost open spans that are it or its descendants, as the engine splits energy. 1 when nothing overlaps it |
| Dynamic energy | `E − P_base × W × s` |
| Gross energy | Dynamic energy `+ P_base × t × s` |
| Dynamic power | Dynamic energy `/ t` |
| Gross power | Gross energy `/ t` |

> tokenize: `W` = 1.0 s, `E` = 67.5 J, `t` = 0.15 s, `P_base` = 60 W.
> Dynamic 67.5 − 60 = 7.5 J. Gross 7.5 + 9 = 16.5 J. Dynamic power 50 W.

**Dynamic energy is reported as measured, never clamped.** Noise can make it
slightly negative for a span that drew nothing above idle. <!-- AC/PRF-16 untested -->

**The numbers come from the engine's span reports.** The frontend subtracts
and sums; it invents no energy. The spans' measured energy and the unclaimed
time outside them add up to the session's total, as the engine bills them.
<!-- AC/PRF-17 untested -->

## The Python package

```python
with CarbonTracker(mode="profiler") as tracker:
    for item in batch:
        with tracker.span("tokenize"):
            tokens = tokenizer(item)
        with tracker.span("forward"):
            out = model(tokens)
```

**The code is not refactored.** The user writes no sleeps and no device
synchronizations. <!-- AC/PRF-18 untested -->

| Argument | Default | Means |
|---|---|---|
| `mode` | `"tracker"` | `"tracker"` or `"profiler"`; anything else is a `ValueError` | <!-- AC/PRF-19 -->
| `components`, `power`, `cpu_power`, `gpu_power`, `pue`, `log_dir`, `log_file_prefix`, `verbose`, `ignore_errors` | As in tracker mode | As in tracker mode | <!-- AC/PRF-20 untested -->

#### Measuring

Each setting trades time for accuracy. The defaults isolate every span; each
one can be loosened when the run is too slow, or tightened when the numbers
are too noisy. <!-- AC/PRF-21 untested -->

| Argument | Default | Means |
|---|---|---|
| `sync_streams` | `True` | Before a span stops, `torch.cuda.synchronize()` when PyTorch is loaded and CUDA is available | <!-- AC/PRF-22 -->
| `wait` | `True` | Wait for reads at span boundaries ([Waiting for reads](#waiting-for-reads)). `False` never waits: spans shorter than a counter period blur, as in tracker mode | <!-- AC/PRF-23 untested -->
| `settle_s` | `0.0` | Extra idle time before each span starts, so the device drops back to idle clocks and every call starts from the same state. The span then starts on a read of its own; the idle time is billed to its parent | <!-- AC/PRF-24 -->
| `subtract_baseline` | `True` | Subtract `P_base` ([The baseline](#the-baseline)). `False` reports the window's measured energy, idle and waits included, as both dynamic and gross, and both powers over the window | <!-- AC/PRF-25 -->
| `baseline_w` | `None` | A known idle draw in watts. Given, nothing is measured at the start | <!-- AC/PRF-26 -->
| `baseline_s` | `1.0` | How long the idle span that measures `P_base` lasts. Longer is steadier and slower to start | <!-- AC/PRF-27 untested -->

> On NVIDIA, a pipeline of ten 50 ms stages takes about 10 s with the
> defaults. `wait=False` takes 0.5 s and blurs the stages together. With
> `settle_s=0.5` each stage starts from idle clocks, and needs a read of its
> own before it starts: about 20 s.

**Every setting is checked before anything is probed or started.** A
negative or non-finite time, or a `baseline_w` below zero, is a
`ValueError`. So is `baseline_w` or `baseline_s` with
`subtract_baseline=False`. <!-- AC/PRF-28 -->

**Every argument profiler mode cannot use is a `ValueError`,** raised before
anything is probed or started, and naming profiler mode: `epochs`,
`epochs_before_pred`, `monitor_epochs`, `zone`, `country`, `coordinates`,
`global_`, `intensity` and `api_keys`. Every measuring setting above, given in
tracker mode, is one too. <!-- AC/PRF-29 -->

**`epoch_start()` and `predict()` are `ValueError`s in profiler mode.**
<!-- AC/PRF-30 -->

**`@track(mode="profiler")` profiles each call as one span**, named after the
function. <!-- AC/PRF-31 -->

**Profiler mode reads no API key and contacts no network service.** No
geolocation, no Electricity Maps, no forecast. <!-- AC/PRF-32 untested -->

### The session

`stop()` returns a `Session` as in tracker mode, with two fields set:
<!-- AC/PRF-33 -->

| Field | Holds |
|---|---|
| `spans` | Per span name, a `ProfilerSpanReport` summed over every call of that name; empty in tracker mode |
| `baseline_w` | `P_base`; `None` in tracker mode |

```python
@dataclass(frozen=True)
class ProfilerSpanReport:
    name: str
    calls: int
    duration_s: float        # work time, summed
    waited_s: float          # waits inside the span, summed
    dynamic_energy_j: float
    gross_energy_j: float
    dynamic_power_w: float   # over duration_s; over the window without a baseline
    gross_power_w: float     # likewise
    samples: int             # fewest samples any measured source had in one call
    warnings: List[str]
```

### Console

**At `verbose=1`, `stop()` prints one table, and nothing about carbon.**
`verbose=0` prints nothing. <!-- AC/PRF-34 -->

**The table shows every call, under its parent,** in the order the calls
started. Siblings that share a name are numbered `#1`, `#2`, … in that order;
a name unique among its siblings is shown bare. `session.spans` still sums
the calls per name. <!-- AC/PRF-36 -->

```text
CarbonTracker: profiler   NVIDIA H100 80GB HBM3 (gpu) measured
CarbonTracker: baseline   60.0 W

  Span        Calls   Duration     Waited   Dynamic Energy   Gross Energy   Dynamic Power
  ---------------------------------------------------------------------------------------
  tokenize        1    150.0 ms   850.0 ms           7.50 J        16.50 J          50.0 W
  forward         1    400.0 ms   600.0 ms          88.00 J       112.00 J         220.0 W
```

## What it does not do

- **It does not run inside a tracker-mode run.** Waits and a baseline inside
  a training run would bill idle time to its epochs; profile a step in a
  separate profiler-mode run.
- **The CLI has no profiler mode.** A tracked command has no spans to wait at.
- **It does not repeat code for the user.** Repetition is the user's loop,
  inside one span, in tracker mode.

## Why

- **Waiting and subtracting are both on by default.** Waiting without
  subtracting bills the idle wait to the span, which is worse than not
  waiting; subtracting without waiting cannot separate spans that share a
  read window. Each can be turned off to see its effect.
- **Every trade of time for accuracy is a setting** with a default that
  isolates spans, so a user can loosen it for speed without editing code.
- **The wait is inside the span.** Stopped before the wait, the span gets a
  time share of the window's mean and loses most of a burst.
- **Built in the frontend** because every piece of it is a sequence of the
  engine's own cuts and reads; the engine stays one model for both modes.
- **Cooldowns come from the providers,** so the profiler waits exactly as
  long as each source needs and no cooldown is written down twice.
- **No guard spans:** waiting out the cooldown lets the start cut be read at
  once, so nothing but the user's spans reaches the record.

## Anchors

```anchor
carbontracker_py::Tracker
```
