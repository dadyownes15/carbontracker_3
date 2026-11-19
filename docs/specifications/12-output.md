# Output — what a run prints at each verbosity

What every frontend prints to the console while it tracks, and which lines
each verbosity keeps. The command ([06](06-cli.md)) and the package
([10](10-python.md)) choose a verbosity; the run's console sink
([04](04-run.md)) decides what each one prints. <!-- AC/OUT-1 unbuilt -->

Each example below is a test: the setup, what the console is given, and what
it prints, with the rules it checks.

## Kinds of line

Every line the console prints is one of four kinds.

| Kind | Is |
|---|---|
| Error | The session, or part of it, could not go on | <!-- AC/OUT-2 unbuilt -->
| Warning | A reported figure is less trustworthy than it looks, or something the user asked for did not happen | <!-- AC/OUT-3 -->
| Info | What is measured, and the closing block | <!-- AC/OUT-4 unbuilt -->
| Debug | One probe step, event or readout, with its time | <!-- AC/OUT-5 unbuilt -->

**A message that could make a reported figure wrong is a warning, never an
info or debug line.** <!-- AC/OUT-7 -->

## Verbosity

| Verbosity | Prints | Command | Package |
|---|---|---|---|
| Quiet | Errors and warnings | — | `verbose=0` | <!-- AC/OUT-8 unbuilt -->
| Normal | Quiet, plus info lines | default | `verbose>=1` | <!-- AC/OUT-9 unbuilt -->
| Everything | Normal, plus debug lines | `--verbose` | — | <!-- AC/OUT-10 unbuilt -->

**Errors and warnings print at every verbosity.** No setting silences
them. <!-- AC/OUT-11 -->

**A higher verbosity only adds lines.** Every line printed at a lower
verbosity is printed at a higher one, with the same text. <!-- AC/OUT-12 unbuilt -->

**A warning prints when its cause is known, not at the end.** A warning raised
during the session prints during the session. <!-- AC/OUT-13 -->

**`output.log` holds what Normal prints, whatever the verbosity.** <!-- AC/OUT-14 unbuilt -->

> A stated CPU at 10 W and a stated intensity of 100 g/kWh, tracking
> `sleep 1`, at each verbosity. Quiet prints no line. Normal prints the setup
> lines and the closing block. Everything prints the same lines in the same
> order, with timed debug lines between them. (OUT-8, OUT-9, OUT-10, OUT-12)

> The same session with `--verbose` and a log folder. `output.log` equals
> what the session prints without `--verbose`. (OUT-14)

## Failed reads

Under `ignore_errors` a failed read does not stop the session
([08](08-measurements.md)), so the warning is how the user learns of it.

**Every failed read is a warning**, naming the source and the failure.
<!-- AC/OUT-15 -->

**A skipped or unchanged read is never a warning.** Both are healthy.
<!-- AC/OUT-20 -->

> The console at Quiet is given NVML's readouts in order: samples,
> `Failed(Provider(Unavailable))` three times, `Unchanged`, then
> `Skipped(Cooldown)`. It prints three warnings, each naming NVML and
> `Unavailable`, and nothing for the other readouts. At Normal and Everything
> the same three warnings appear. (OUT-11, OUT-15, OUT-20, OUT-30)

> In the same stream, each warning is written before the console is given
> the next readout. (OUT-13)

## Other warnings

Each of these is a warning, at every verbosity:

| Warning | Spec | |
|---|---|---|
| Each of the session report's warnings: thin coverage, no intensity, held intensity, an estimated total, no selected sources | [09](09-profiling.md) | <!-- AC/OUT-21 unbuilt -->
| A source that is not measured: modelled or assumed, and not asked for | [03](03-providers.md), [04](04-run.md) | <!-- AC/OUT-22 -->
| Events were lost: the record is incomplete | [04](04-run.md) | <!-- AC/OUT-23 unbuilt -->
| A sink failed and stopped | [04](04-run.md) | <!-- AC/OUT-24 unbuilt -->
| A command was refused: a mark, start or stop the engine did not accept | [02](02-engine.md) | <!-- AC/OUT-25 unbuilt -->

**Only a measured or stated source warns nothing.** A source whose fidelity
is modelled or assumed is a warning when the session starts, naming the
source and its fidelity. A figure the user asked for (a stated power or
intensity, `global`, profiler mode) is not warned about. <!-- AC/OUT-32 -->

> A session started at Quiet with a modelled CPU and the world-average grid,
> not asked for. Two warnings print before the first span: the CPU is
> modelled, the grid is assumed. The same session with `cpu_power=10` and
> `global=True` prints no line. (OUT-22, OUT-32)

**The session report's warnings print apart from the closing block**, so
Quiet prints them without it. <!-- AC/OUT-28 unbuilt -->

> A package session at `verbose=0`, `update_interval=60`, stopped at once. No
> sample lands, so the report warns of thin coverage. The warning prints and
> the closing block does not. (OUT-21, OUT-28)

> A package session at `verbose=0` that stops the same span twice. The second
> stop is refused, and a warning names the span. (OUT-25)

## What it never does

**It never prints a warning for healthy behaviour**: a skipped read, an
unchanged read, or a cooldown. <!-- AC/OUT-30 -->

**It never prints to stdout during a command run.** All output is on stderr
([06](06-cli.md)). <!-- AC/OUT-31 unbuilt -->

## Why

- **Warnings print at every verbosity** so that `verbose=0` can never hide a
  wrong figure.
- **Every failed read is a warning** so that a figure resting on a failed
  source is never reported silently. How a sink lays out repeated warnings is
  presentation, not verbosity.

## Anchors

```anchor
carbontracker_run::Verbosity
carbontracker_run::ConsoleSink
carbontracker_run::standard_sinks
```
