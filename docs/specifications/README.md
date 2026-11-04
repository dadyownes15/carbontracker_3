# Specifications

How carbontracker's interfaces must behave.

**A specification states the wished behaviour, not what the code happens to
do.** Where the code disagrees with a specification, the code is wrong: the
gap is a defect, kept as an `#[ignore = "known: …"]` test until it is fixed.
Never edit a specification to match the code; change it only when the wished
behaviour changes.

## Why the 3.0.0 specifications drifted

The first 3.0.0 specifications drifted and were deleted. They drifted for
three reasons, and this directory is shaped to avoid each one.

**They were change plans wearing a specification's clothes.** `3.0.0.md` was a
checklist: `- [x] Narrow the public API`, `- [ ] Grid location`. A checkbox is
answered the moment the work merges, and the document that holds it expires
with it. Nothing in a ticked list says how the interface must behave.

**They mixed three lifetimes in one file.** The standing specification lives
as long as the interface. The change plan dies on merge. The rationale is
history and never needs updating. Kept together, the whole file is as stale as
its shortest-lived part.

**They described structure, not promises.** `3.0.0.md` specified
`carbontracker-sinks` with `LogSink, JsonlSink, CsvSink`. That crate is now
`carbontracker-run`, with `ConsoleSink`, `JsonlSink` and a `Projection` port.
The promise barely changed; the spec could not follow because it had written
down the furniture rather than the promise.

## The rules

1. **Present tense, indicative.** "The engine announces every accepted cut."
   Never "will", "becomes", "should" or "is renamed to". The wished behaviour
   is stated as fact; where the code falls short, that is a defect.
2. **No checkboxes, no status, no dates.** Work in progress belongs
   elsewhere. A specification describes the interface, not the project.
3. **Promises, not structure.** Name the promise and the symbols that carry
   it. Do not name private modules, file layout or internal types: those move
   without the promise changing.
4. **One document per public seam**, not per crate. A seam is something an
   outside caller can hold on to and we are unwilling to break quietly.
5. **Anchored.** Every specification lists the exported symbols it governs in
   an `anchor` block. A check script fails if a listed symbol is no longer
   exported.
6. **Rationale is one line.** "Why" notes exist to stop a
   future reader undoing a deliberate decision. Anything longer is history.

## Where other things go

| Kind | Lives in | Lifetime |
|---|---|---|
| Standing specification | `docs/specifications/` | As long as the interface |
| Proposed specification, under review | `docs/specifications/drafts/` | Until adopted into a specification, or dropped |
| Known defects: code that breaks a specification | `#[ignore = "known: …"]` on the test | Until fixed |

## The documents

| | Seam | Governs |
|---|---|---|
| [01](01-system.md) | The system | Crates, principles, what depends on what |
| [02](02-engine.md) | `carbontracker` | The handle, events, queries, provider port |
| [03](03-providers.md) | `carbontracker-providers` | Probing, the chains, resolution, the forecast, the default providers |
| [04](04-run.md) | `carbontracker-run` | `RunHandle`, `Sink`, `Projection`, items |
| [05](05-record-v1.md) | The `.ct` file | The on-disk format, and its versioning |
| [06](06-cli.md) | `carbontracker`, the command | What users type: tracking, `check`, `init`, `report`, the run folder |
| [07](07-testing.md) | The suite | What is verified, where, and how |
| [08](08-measurements.md) | Readouts | Readouts, reading providers, the watchdog, the sample store, the cutoff |
| [09](09-profiling.md) | Spans | The timeline, attribution, settling, reports |
| [10](10-python.md) | `carbontracker`, the package | What users call: `CarbonTracker`, spans, `track`, `report` |
| [11](11-profiler.md) | `carbontracker`, the package in profiler mode | Waiting for reads, the baseline, the profiler's settings and report |
