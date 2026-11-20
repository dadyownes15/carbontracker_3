# External usage — what a span spent on hardware not measured here

External usage is energy and emissions a span caused on hardware this
carbontracker does not measure: a model behind an API, a remote service, a job
another meter measured. It is recorded on a span beside the usage measured
here, kept apart from it in every report, and added to it only in a total that
says how much of it is external. <!-- AC/EXT-1 unbuilt -->

It lives in the run ([04](../04-run.md)), not the engine: an entry needs no
cut and no read, so the engine still only measures and attributes
([01](../01-system.md), principle 1).

![One document over 5 s. embed, llm_call and score cut the CPU and the GPU and are billed what was measured. At 3.8 s the LLM call's response brings 3 100 input and 420 output tokens; the estimator makes them 2.03 Wh, which lands on llm_call without cutting either source. The bill keeps local and external apart and adds them only in the total.](figures/external.svg)

## An entry

**An entry is work, and the usage known or estimated from it.** <!-- AC/EXT-2 unbuilt -->

| Field | Is | Given |
|---|---|---|
| `span` | The span it is billed to | Always; the session when none is named |
| `label` | Where the work ran, for people: `anthropic/claude-sonnet-4-5`, `ocr.example.com`, `hendrix job 812734` | Always |
| `work` | Quantities by kind: `input_tokens`, `cache_write_tokens`, `cache_read_tokens`, `output_tokens`, `requests`, `gpu_hours`, … | When known |
| `energy_kwh` | The energy | When known |
| `emissions_g` | The emissions | When known |
| `intensity_g_per_kwh` | The grid where it ran | When known |
| `fidelity` | `measured`, `modelled` or `stated` | Always |
| `estimator` | `name@version` | When modelled |
| `range` | Low and high energy | When modelled |

**An entry needs a label and at least one of work, energy or emissions.**
Anything less is refused: `ValueError` in Python, exit 2 from the CLI. <!-- AC/EXT-3 unbuilt -->

**Every value is finite and at least zero**, or the entry is refused. <!-- AC/EXT-4 unbuilt -->

**Given energy and an intensity, the emissions are their product.** <!-- AC/EXT-5 unbuilt -->

**Energy with no intensity has absent emissions, never this host's.** The grid
the work ran on is not this machine's. <!-- AC/EXT-6 unbuilt -->

**A `zone` given with an entry prices its energy at that zone's country
average**, from the table the package ships, as `modelled`. Nothing is
fetched. <!-- AC/EXT-7 unbuilt -->

**Emissions given without energy count as emissions, with energy absent.** <!-- AC/EXT-8 unbuilt -->

**An entry keeps the fidelity it was made with**: `measured` when a meter
elsewhere read it (DCGM on a cluster node, a service that meters each call),
`modelled` when an estimator derived it from work, `stated` when it was given
with no account of how. <!-- AC/EXT-9 unbuilt -->

## Estimators

**An estimator turns work into usage, and does nothing else.** It is a pure
function of the work and its factor table: the same work and the same table
version give the same usage. It calls no network service. <!-- AC/EXT-10 unbuilt -->

**Every factor names its source**: the publication, its date, and a low and
high value beside the central one. <!-- AC/EXT-11 unbuilt -->

**Each kind of work is priced by its own factor.** Input, cache-write,
cache-read and output tokens are four kinds. <!-- AC/EXT-12 unbuilt -->

**Work with no factor is recorded unpriced**: its usage is absent and a warning
names the label and the kind, such as `no factor for
anthropic/claude-opus-5-5 cache_read_tokens`. It is never priced as another
model or another kind. <!-- AC/EXT-13 unbuilt -->

**The record keeps the work and the estimator's version**, so a later table can
price the same run again. <!-- AC/EXT-14 unbuilt -->

**An estimator is chosen by name, and the record names the one used.** The
package ships `llm-tokens`, the default: per model and per kind of token, its
table versioned by the month it was compiled (`llm-tokens@2026.11`). <!-- AC/EXT-33 unbuilt -->

## Recording

```python
with tracker.span("llm_call") as span:
    reply = client.messages.create(model=MODEL, messages=messages, max_tokens=1024)
    span.external(f"anthropic/{reply.model}", work={
        "input_tokens": reply.usage.input_tokens,
        "output_tokens": reply.usage.output_tokens,
    })
```

| Call | Does |
|---|---|
| `RunningSpan.external(label, *, work=None, energy_kwh=None, emissions_g=None, intensity_g_per_kwh=None, zone=None, fidelity=None)` | Records an entry on the span. With work and neither energy nor emissions, `llm-tokens` prices the work | <!-- AC/EXT-15 unbuilt -->
| `CarbonTracker.external(...)` | The same, on the session | <!-- AC/EXT-16 unbuilt -->
| `carbontracker external LABEL [--work KIND=N]... [--energy-kwh X] [--emissions-g X] [--intensity X] [--zone Z] [--fidelity F] [--span ID]` | The same, from inside a tracked command, on the run that tracks it ([agents](agents.md) says how the command reaches its run) | <!-- AC/EXT-17 unbuilt -->

**Recording cuts nothing, asks no provider and never waits.** <!-- AC/EXT-18 unbuilt -->

**An entry may name any span of the run, open or closed, until the run
finishes.** Usage is often known only after the work ends: a provider reports
it late, a meter elsewhere settles later. After the run finishes, an entry is
refused as `Closed`. <!-- AC/EXT-19 unbuilt -->

**Recording holds no memory per entry or per span.** Each entry is written as
it comes; the run keeps only the session's sums. <!-- AC/EXT-20 unbuilt -->

**External usage is never multiplied by this host's PUE.** An estimator
includes the overheads of where the work ran. <!-- AC/EXT-21 unbuilt -->

**External usage is never shared out.** It belongs to the span it was recorded
on, and counts towards that span's ancestors as a child's usage does.
Concurrent spans never split it. <!-- AC/EXT-22 unbuilt -->

## In the record

These are additions under [05](../05-record-v1.md)'s rules. A reader older than
them skips the lines it does not know, and reports local usage only.

**Each entry is an `external` line**: the span, its time, and the entry's
fields. <!-- AC/EXT-23 unbuilt -->

**The `session` line carries `external` beside the local usage**: energy and
emissions, the number of entries, how many are unpriced, and energy and
emissions per fidelity. The local usage keeps its format 1 name and
meaning. <!-- AC/EXT-24 unbuilt -->

## In reports

**A total that includes external usage says how much of it is external, and how
that part was arrived at.** <!-- AC/EXT-25 unbuilt -->

```
CarbonTracker: agent summary:
	Energy:     7.01 Wh, of which 6.60 Wh (2.2–19.8) external, modelled by llm-tokens@2026.11
```

**`Session.total` is local plus external; `Session.local` and
`Session.external` are the two parts.** Its emissions sum what is known, and
`Session.external.unpriced` counts the entries without. <!-- AC/EXT-26 unbuilt -->

**`report` shows local and external apart in every table and CSV**, the runs
table, `--spans`, and the CSV's `local_*` and `external_*` columns. <!-- AC/EXT-27 unbuilt -->

**The paper sentence names the external part**: "… estimated to use 12.9 kWh
of electricity, of which 0.41 kWh is estimated for work done by remote
services (llm-tokens@2026.11), contributing to …" <!-- AC/EXT-28 unbuilt -->

**A modelled part is shown with its range.** <!-- AC/EXT-29 unbuilt -->

**`predict()` scales the external usage of the epochs measured as it scales
the local**, and shows the two apart. <!-- AC/EXT-30 unbuilt -->

## What it never does

**It never derives energy from price.** A cost in dollars, which some agents
report, is never turned into energy or emissions. <!-- AC/EXT-31 unbuilt -->

**It never fetches a factor or an intensity during a run.** <!-- AC/EXT-32 unbuilt -->

## Why

- **Apart, and added only in a labelled total**, so that a measured number is
  never quietly diluted by an estimate, and a paper's reader can tell which is
  which.
- **No local intensity for remote energy**, because the remote grid is not
  ours. The carbontracker-tui prototype priced it locally, and the number was
  wrong in a way no reader could see.
- **In the run, not the engine**, because an entry needs no cut and no read.
- **Entries may name closed spans** because providers and meters report after
  the work ends.
- **Work is kept** because published factors are young, and keep improving.
- **Never from price**, because price follows the market, not the joules.
- **Every kind of token priced**, because a coding agent's tokens are mostly
  cache reads, and EcoLogits' methodology gives input tokens no GPU energy.
- **Estimators named and swappable**, because published cache-read factors
  differ fourfold, from 9.5 to 39 Wh per million tokens.

## Anchors

```anchor
carbontracker_run::External
carbontracker_run::Estimator
carbontracker_run::RunHandle
carbontracker_run::record::External
carbontracker_py::Tracker
```
