# The record — format 1

A run's `.ct` file: JSON lines, one `Record` per line, the `run` header first. <!-- AC/REC-1 -->
This is carbontracker's longest-lived specification, because the files outlive every
version that wrote them.

## Versioning

- A reader accepts every format up to `FORMAT` and **rejects newer ones**. <!-- AC/REC-2 -->
- Within a format, only **new kinds and new fields** may be added. A reader
  skips kinds it does not know (`Line::Unknown`). <!-- AC/REC-3 -->
- Removing a field, renaming one, or changing its unit or meaning **requires a
  new format**. <!-- AC/REC-4 -->
- Every format has fixtures in `crates/run/tests/fixtures/records/v<N>/`. Every
  later release must still read every one of them. <!-- AC/REC-5 -->

Until 3.0.0 ships, format 1 may be amended in place and the fixtures
re-blessed. After it ships, amendment is additive only, by the rules above.

## Lines

| `kind` | Is |
|---|---|
| `run` | The header: version, config, location, grid, epochs | <!-- AC/REC-6 -->
| `probe` | What was tried and what each candidate decided | <!-- AC/REC-7 -->
| `catalog` | The sources measured, with description and fidelity, and `pairs`: each power source's intensity source | <!-- AC/REC-8 -->
| `readout` | One request to one provider: each source it serves, under what became of it | <!-- AC/REC-9 -->
| `span_started` | A span opened, with its name, parent and what it bills | <!-- AC/REC-10 -->
| `span_stopped` | A span closed, with its work | <!-- AC/REC-11 -->
| `marked` | Work on an open span | <!-- AC/REC-12 -->
| `refused` | A command the tracker refused | <!-- AC/REC-13 -->
| `span_settled` | A span's final usage | <!-- AC/REC-14 -->
| `outcome` | How the run ended | <!-- AC/REC-15 -->
| `session` | The session's totals | <!-- AC/REC-16 -->

## Readouts

**The record keeps each source's observation, by name.** For each readout, a
source appears under the observation it had: `samples` with its points,
`unchanged`, `skipped` with `cooldown` or `busy`, or `failed` with its
`failure` (`provider`, `unanswered`, `timed_out`, `stalled`, `panicked` or
`retired`). A provider's failure also carries its `error` (`unavailable`,
`denied`, `rate_limited` or `invalid`) and its `detail`; a panic, its message
as the `detail`. A reader of the record can tell a source that was skipped
from one that had nothing new and from one that failed, and say whose
decision it was. <!-- AC/REC-17 -->

**A source that can never be read again is recorded once.** A source whose
provider's cooldown is `Never` appears in the readout that read it, and in none
made after it. <!-- AC/REC-18 -->

**A `readout` line is one request to one provider,** sent by a cut or by the
watchdog. A cut that relies on an earlier request writes no line of its own. <!-- AC/REC-19 -->

## Usage

**`emissions_g` is null where no intensity was known**, and only on usage with
energy above zero. **`samples` is a map from source id to count**, per power
source. A `session` line's `intensities` is a list, one entry per intensity
source: its `source`, `label`, `fidelity`, and `mean_g_per_kwh`, the mean over
the power sources paired with it. <!-- AC/REC-20 -->

## Types

The record's types are **independent of the engine's**. They are not re-exports
and must never become re-exports: the engine is free to change its own types,
and the file format is not. <!-- AC/REC-21 -->

## Units

Energy in kWh, emissions in gCO2e, intensity in gCO2e/kWh, times as RFC 3339.
Units never change meaning within a format. <!-- AC/REC-22 -->

## Why

- **The types are independent of the engine's** so that an engine refactor can
  never silently change a file that users already have on disk.
- **Readers reject newer formats** rather than guessing, so a wrong reading is
  never mistaken for a right one.

## Anchors

```anchor
carbontracker_run::record::FORMAT
carbontracker_run::record::Record
carbontracker_run::record::Line
carbontracker_run::record::Header
carbontracker_run::record::Catalog
carbontracker_run::record::Readout
carbontracker_run::record::Failed
carbontracker_run::record::SpanStarted
carbontracker_run::record::SpanStopped
carbontracker_run::record::SpanSettled
carbontracker_run::record::Session
carbontracker_run::record::Outcome
```
