# Span sources — choosing what a Python span bills

A Python span names the power sources it bills, so a span that runs on the
GPU is cut and billed on the GPU alone. The selection is the engine's
`SourceSelection` ([09](../09-profiling.md#selection)); the package passes it
through `carbontracker-run` and adds no attribution rules of its own. A span
that names no sources bills every source, as today. <!-- AC/PY-45 unbuilt -->

These rules extend the Python package ([10](../10-python.md)) and join it on
adoption, keeping their numbers.

```python
tracker = CarbonTracker(epochs=max_epochs)

with tracker.span("decode", sources="gpu"):
    ...

with tracker.span("tokenize", sources=["cpu", "ram"]):
    ...

gpus = CarbonTracker(components="gpu")             # tracker.sources: the GPUs alone
for source in gpus.sources.values():
    with gpus.span(f"shard {source.label}", sources=source):
        ...
```

## Naming sources

| Call | Does |
|---|---|
| `tracker.span(name, parent=None, *, sources=None)` | `sources` is keyword-only; `None` selects every power source | <!-- AC/PY-46 unbuilt -->
| `sources="gpu"` or `sources=["cpu", "gpu"]` | Selects every power source of those domains: `cpu`, `gpu`, `ram`, `ane`, `system` | <!-- AC/PY-47 unbuilt -->
| `sources=source` or `sources=[source, ...]` | Selects exactly those `Source`s, taken from `tracker.sources` | <!-- AC/PY-48 unbuilt -->

**`tracker.sources` gives the power sources this tracker measures, as
`components` leaves them, as `Source`s by id**, from construction on, with
the ids `session.sources` uses. A `Source` from it is how a span names one
source: the caller never writes an id. <!-- AC/PY-49 unbuilt -->

**`Source.domain` is the power source's domain**, one of the five names
above, and `None` for an intensity source. <!-- AC/PY-50 unbuilt -->

**A `Span` keeps its selection**: each `with`, decorated call or `start()` of
it opens a span with the same sources. <!-- AC/PY-51 unbuilt -->

## Refusals

**An invalid `sources` raises `ValueError` from `tracker.span(...)`, before
any span starts, whatever `ignore_errors` says.** Refused: a name that is not
one of the five domains; a source id given as a string; an empty iterable;
domain names and `Source`s mixed in one selection; an intensity `Source`; a
`Source` of another tracker; any other type. The message lists the five
domain names. <!-- AC/PY-52 unbuilt -->

**A domain the tracker does not measure is not an error**, whether the host
lacks it or `components` left it out. The span runs, cuts nothing, and its
report in the record warns `NoSelectedSources` (AC/PROF-5), so one script
runs on a GPU host and on a CPU-only one. <!-- AC/PY-53 unbuilt -->

## What a selection changes

**The record's `span_started` line bills what `sources` selected**
(AC/REC-10): the domains, the source ids, or every source. <!-- AC/PY-54 unbuilt -->

**A span's report covers only the power sources it selected**, each with the
intensity source it is paired with. A CPU-only span has no GPU usage in
`per_source`. <!-- AC/PY-55 unbuilt -->

**A child's sources are its own**, neither narrowed nor widened by its
parent's: a GPU span under a CPU-only parent bills the GPU, and the parent
has no part in it (AC/PROF-20). <!-- AC/PY-56 unbuilt -->

**Epochs select every power source.** <!-- AC/PY-57 unbuilt -->

**In profiler mode a span waits only for the paced sources it selects**
([11](../11-profiler.md)), at its start and at its stop. <!-- AC/PY-58 unbuilt -->

## What it never does

**`sources` never names an intensity source.** Each selected power source
brings the intensity source the catalog paired it with (AC/PROF-28). <!-- AC/PY-59 unbuilt -->

**A span never selects a source the tracker does not measure**: no `Source`
exists for one, and a domain name outside `components` selects nothing
(AC/PY-53). <!-- AC/PY-60 unbuilt -->

## Why

- **A `Source` object is the id**, taken from `tracker.sources`, so a caller
  selects only what the tracker measures, and a mistyped id string is a
  `ValueError` rather than an empty selection.
- **`tracker.sources` is what `components` left**, so the sources a span can
  name and the sources being tracked are one set.
- **An empty iterable is refused** because selecting nothing is never what
  the caller meant; a domain the host lacks is how a script selects nothing
  on purpose.
- **A missing domain is not an error** so that the same code runs on hosts
  with and without the device.
- **`sources` is keyword-only** so that `span(name, parent)` keeps its
  positions.
- **The parent does not narrow its child** because the engine selects per
  span, and a narrowing rule would hide a GPU child's usage under a CPU-only
  parent.

## Anchors

```anchor
carbontracker_py::Tracker
carbontracker::SourceSelection
carbontracker::PowerDomain
```
