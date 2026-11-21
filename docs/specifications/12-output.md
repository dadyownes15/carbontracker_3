# Output — what a run prints at each verbosity

What every frontend prints to the console while it tracks, and which lines
each verbosity keeps. The command ([06](06-cli.md)) and the package
([10](10-python.md)) choose a verbosity; the run's console sink
([04](04-run.md)) decides what each one prints. <!-- AC/OUT-1 -->

Each example below is a test: the setup, what the console is given, and what
it prints, with the rules it checks.

## Kinds of line

Every line the console prints is one of four kinds.

| Kind | Is |
|---|---|
| Error | The session, or part of it, could not go on | <!-- AC/OUT-2 -->
| Warning | A reported figure is less trustworthy than it looks, or something the user asked for did not happen | <!-- AC/OUT-3 -->
| Info | What is measured, and the closing block | <!-- AC/OUT-4 -->
| Debug | One probe step, event or readout, with its time | <!-- AC/OUT-5 -->

**A message that could make a reported figure wrong is a warning, never an
info or debug line.** <!-- AC/OUT-7 -->

## Verbosity

| Verbosity | Prints | Command | Package |
|---|---|---|---|
| Quiet | Errors and warnings | — | `verbose=0` | <!-- AC/OUT-8 -->
| Normal | Quiet, plus info lines | default | `verbose>=1` | <!-- AC/OUT-9 -->
| Everything | Normal, plus debug lines | `--verbose` | — | <!-- AC/OUT-10 -->

**Errors and warnings print at every verbosity.** No setting silences
them. <!-- AC/OUT-11 -->

**A higher verbosity only adds lines.** Every line printed at a lower
verbosity is printed at a higher one, with the same text. <!-- AC/OUT-12 -->

**A warning prints when its cause is known, not at the end.** A warning raised
during the session prints during the session. <!-- AC/OUT-13 -->

**`output.log` holds what Normal prints, whatever the verbosity.** <!-- AC/OUT-14 -->

> A run with a stated draw of 10 W and a stated intensity of 100 g/kWh on the
> world-average grid, as asked, and a log folder. It tracks one span and
> hands every line to three consoles at once: Quiet, Normal and Everything.
> Quiet prints no line. Normal prints the setup lines, the ready line and the
> closing block. Everything prints the same lines in the same order, with
> timed debug lines between them. `output.log` equals what Normal printed.
> (OUT-8, OUT-9, OUT-10, OUT-12, OUT-14)

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
| Each of the session report's warnings: thin coverage, no intensity, held intensity, an estimated total, no selected sources | [09](09-profiling.md) | <!-- AC/OUT-21 -->
| Each of a span report's warnings, naming the span, when the span settles | [09](09-profiling.md) | <!-- AC/OUT-33 -->
| Each of the prediction's warnings: nothing measured, a thin basis, a held or partial forecast, no forecast | [10](10-python.md) | <!-- AC/OUT-34 -->
| A source that is not measured: modelled or assumed, and not asked for | [03](03-providers.md), [04](04-run.md) | <!-- AC/OUT-22 -->
| Events were lost: the record is incomplete | [04](04-run.md) | <!-- AC/OUT-23 -->
| A sink failed and stopped, printed by the other sinks as soon as it fails | [04](04-run.md) | <!-- AC/OUT-24 -->
| A command was refused: a mark, start or stop the engine did not accept | [02](02-engine.md) | <!-- AC/OUT-25 -->

**Only a measured or stated source warns nothing.** A source whose fidelity
is modelled or assumed is a warning when the session starts, naming the
source and its fidelity. A figure the user asked for (a stated power or
intensity, `global`, profiler mode) is not warned about. <!-- AC/OUT-32 -->

**A figure the user asked for is not warned about in a report either.** Where
a report's warning names it, the warning names only the sources not asked
for, and is not printed when every source it names was asked for. This rule
wins over OUT-21 and OUT-33. <!-- AC/OUT-35 -->

> A run at Quiet with a stated draw on Denmark's grid, read from the country
> table, which is modelled. One warning prints, naming DK and that it is
> modelled. At Everything it is the line right after the ready line, before
> the span starts. The same run with a stated intensity on the world-average
> grid, as asked, prints no line. (OUT-13, OUT-22, OUT-32)

> The console at Quiet is given a session report whose estimate names a
> modelled CPU and the world-average grid. When the world average was not
> asked for, the warning names both; when it was, it names only the CPU.
> (OUT-21, OUT-35)

**The report's and the prediction's warnings print apart from their blocks**,
so Quiet prints them without the closing or prediction block. <!-- AC/OUT-28 -->

> A package session at `verbose=0` with `power=10` on Denmark's grid, stopped
> at once. The country table's figure is modelled, so the report warns that
> part of the total is an estimate. The warning prints and the closing block
> does not. The same session with `global_=True` and `intensity=100` prints
> nothing. (OUT-21, OUT-28, OUT-35)

**A span report's warning prints once per run.** A later span whose warning
has the same text, span name included, prints nothing more. <!-- AC/OUT-36 -->

> The console at Quiet is given a span report for `epoch` with no samples
> inside it. It prints one warning, naming `epoch` and the thin coverage, and
> the same line at Normal and Everything. (OUT-11, OUT-12, OUT-33)

> The console is then given two more such reports for `epoch` and one for
> `eval`. It prints one more warning, for `eval`. (OUT-36)

> A package session at `verbose=0` with stated figures, `epochs=2` and
> `epochs_before_pred=1`, stopped after one epoch. It prints one warning: the
> prediction rests on one epoch. It prints no prediction block. (OUT-28,
> OUT-34)

> A package session at `verbose=0` starts an inner span under an outer one,
> stops the outer span, which stops the inner with it, then stops the inner
> span. The engine refuses that stop, and a warning names the inner span.
> (OUT-25)

> A run at Normal whose first sink fails on the first event. The console
> prints that the sink failed and stopped before the closing block. (OUT-13,
> OUT-24)

## What it never does

**It never prints a warning for healthy behaviour**: a skipped read, an
unchanged read, or a cooldown. <!-- AC/OUT-30 -->

**It never prints to stdout during a command run.** All output is on stderr
([06](06-cli.md)). <!-- AC/OUT-31 -->

## Why

- **Warnings print at every verbosity** so that `verbose=0` can never hide a
  wrong figure.
- **Every failed read is a warning** so that a figure resting on a failed
  source is never reported silently. How a sink lays out repeated warnings is
  presentation, not verbosity.
- **A figure the user asked for is never warned about**, in a report either,
  so that a warning always tells the user something they did not choose.
- **A span's warning prints once per run** because a training loop settles
  hundreds of spans with the same name, and the same cause, such as a
  modelled grid, would otherwise repeat on every one.

## Anchors

```anchor
carbontracker_run::Verbosity
carbontracker_run::ConsoleSink
carbontracker_run::standard_sinks
```
