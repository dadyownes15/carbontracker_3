# The Python package — `carbontracker`

What users call. `CarbonTracker` keeps carbontracker 2.4.7's interface, so its
example loop runs unchanged, and adds spans, `predict`, `track` and a returned
`Session`. Like the CLI ([06](06-cli.md)), it works only through
`carbontracker-run`; it never restarts itself, exits the process or waits on
input.

```python
from carbontracker import CarbonTracker            # or carbontracker.tracker, as in 2.4.7

tracker = CarbonTracker(epochs=max_epochs)
for epoch in range(max_epochs):
    tracker.epoch_start()
    with tracker.span("eval") as span:
        span.mark("samples", 64)
    tracker.epoch_end()
session = tracker.stop()
```

**`from carbontracker import X` gives `CarbonTracker`, `Session`, `Span`,
`RunningSpan`, `Source`, `Usage`, `Prediction`, `ProfilerSpanReport`, `Report`,
`TrackingError`, `report` and `track`**, and `carbontracker.tracker.CarbonTracker` is the same
class. <!-- AC/PY-1 -->

## Constructing

**2.4.7's fourteen arguments keep their names, positions and defaults**:
`epochs`, `epochs_before_pred`, `monitor_epochs`, `update_interval`,
`interpretable`, `stop_and_confirm`, `ignore_errors`, `components`,
`devices_by_pid`, `log_dir`, `log_file_prefix`, `verbose`,
`decimal_precision`, `api_keys`. Every argument added after them is
keyword-only. <!-- AC/PY-2 -->

| New argument | Means | Default |
|---|---|---|
| `zone`, `country`, `coordinates`, `global_` | Where the grid is — at most one | estimated from this IP |
| `power`, `cpu_power`, `gpu_power` | A stated draw in watts | measured |
| `intensity` | A fixed grid intensity in gCO2eq/kWh | fetched |
| `pue` | Power usage effectiveness | 1.58 |

**Every invalid or conflicting argument raises `ValueError` before anything is
probed or started, whatever `ignore_errors` says.** Refused: two locations; a
zone that is empty; a country that is not two letters; coordinates outside
±90, ±180; `power` with `cpu_power` or `gpu_power`; a negative draw; an
intensity not above zero; `pue` below 1; an `update_interval` not above zero;
`components` other than `all`, `cpu`, `gpu`, `cpu,gpu`; a negative `epochs`
or `decimal_precision`; `api_keys` that is not a dict of strings, or names a
service it does not know; `monitor_epochs` of zero or below
`epochs_before_pred`. <!-- AC/PY-3 -->

**2.4.7's removed arguments raise `TypeError` naming what to use instead**:
`sim_cpu`, `sim_cpu_tdp`, `sim_cpu_util` point to `cpu_power`; `sim_gpu`,
`sim_gpu_watts`, `sim_gpu_util` to `gpu_power`; `stop_and_confirm=True` says
it waited on `input()`. `stop_and_confirm=False` is accepted. <!-- AC/PY-4 -->

**An `api_keys` key wins over `CARBONTRACKER_EM_KEY`, which wins over the
config file `carbontracker init` writes.** <!-- AC/PY-5 -->

**A config file others can read gets a warning on stderr**, and tracking goes
on. <!-- AC/PY-6 -->

## Errors

**A tracking failure raises `TrackingError`**, a `RuntimeError`, from the call
that hit it, with the cause chained. <!-- AC/PY-7 -->

**With `ignore_errors=True`, a tracking failure prints the error and
`Continued training without monitoring`, stops tracking, and returns.** Later
`epoch_start()` and `epoch_end()` do nothing, and `stop()` returns the session
up to the failure. <!-- AC/PY-8 -->

## Epochs

**2.4.7's example loop prints what 2.4.7 printed**: what is measured, the
predicted consumption for the planned epochs after `epochs_before_pred` of
them, the actual consumption when the loop ends, and `Finished monitoring.` <!-- AC/PY-9 -->

**`epochs_before_pred=0` predicts nothing.** <!-- AC/PY-10 -->

**After `monitor_epochs` epochs, tracking stops by itself** and prints the
actual consumption; later epoch calls do nothing. <!-- AC/PY-11 -->

**`stop()` before all epochs says the training was interrupted**, and an epoch
still open is not counted as done. <!-- AC/PY-12 -->

**`epoch_start()` on a tracker made without `epochs` raises `ValueError`.** <!-- AC/PY-13 -->

**`epoch_end()` with no epoch open is a tracking failure naming
`epoch_start()`**, raised or ignored as any other. <!-- AC/PY-44 -->

**`predict(epochs=None)` returns a `Prediction` of the whole run** from the
epochs ended so far, scaled to `epochs` or the planned ones; it waits for
the last ended epoch to settle, at most one readout and its fetch. With no
epoch measured yet it returns `None`, never a prediction of zeros. Without
`epochs` here or on the tracker it raises `ValueError`; after tracking stops,
`RuntimeError`. <!-- AC/PY-14 -->

**`session.prediction` is the prediction printed after `epochs_before_pred`
epochs**, not one made at the end. <!-- AC/PY-15 -->

## Spans

| Call | Does |
|---|---|
| `tracker.span(name, parent=None)` | A `Span`; each `with`, decorated call or `start()` opens a new one, without waiting | <!-- AC/PY-16 -->
| `RunningSpan.mark(kind, amount=1.0)` | Reports work on the open span, without waiting; after it stops, nothing | <!-- AC/PY-17 -->
| `RunningSpan.stop(work=None)` | Closes the span with its work, without waiting; a second stop does nothing | <!-- AC/PY-18 -->

**A span nests under `parent` if given, else under the innermost span running
in this thread or task, else under the open epoch, else under the session.** <!-- AC/PY-19 -->

**A span closes when its `with` block or decorated call ends, also on an
exception**, which is not swallowed. <!-- AC/PY-20 -->

**`@tracker.span(name)` works on plain and `async` functions**, and keeps the
function's name and docstring. <!-- AC/PY-21 -->

**`"epoch"` is the epochs' name; `span("epoch")` raises `ValueError`.** <!-- AC/PY-22 -->

**Once tracking stops, `start()` raises `RuntimeError` and a decorated function
runs untracked.** <!-- AC/PY-23 -->

**A long run of spans holds no memory per span.** <!-- AC/PY-24 -->

## Ending

**`with CarbonTracker(...) as tracker:` stops at the end of the block, also on
an exception**, which is not swallowed. <!-- AC/PY-25 -->

**`stop()` returns once everything is written, with the `Session`.** Calling
it again returns the same object. <!-- AC/PY-26 -->

**A `Session` is plain frozen data**: it pickles, `dataclasses.asdict` and
`json.dumps(..., default=str)` take it, and it outlives the tracker. Energy is
in kWh, emissions in grams, durations in seconds, times are timezone-aware UTC. <!-- AC/PY-27 -->

**Absent emissions are `None`, never zero.** <!-- AC/PY-28 -->

**A tracker never stopped is stopped at interpreter exit**, so its files are
complete. <!-- AC/PY-29 -->

**A forked child tracks nothing**: its spans and decorated functions run
untracked and it writes nothing, while the parent goes on tracking. <!-- AC/PY-30 -->

## `track`

**`@track` and `@track(**arguments)` make each call of a function one whole
session**, on plain and `async` functions; the arguments are
`CarbonTracker`'s. The last call's `Session` is on the function's `session`. <!-- AC/PY-31 -->

## Output

**Output goes through `print`, or `tqdm.write` when tqdm is imported**, so it
reaches a notebook cell and keeps progress bars whole. <!-- AC/PY-32 -->

**`verbose=0` prints only warnings and errors**; any `verbose` of 1 or more
prints everything, as 1. <!-- AC/PY-33 -->

**`log_dir=None` writes nothing.** <!-- AC/PY-34 -->

**With a `log_dir`, each tracker gets a folder of its own:
`<log_dir>/<prefix>_run-<YYYYMMDD_HHMMSS>-<id>/`** (no `<prefix>_` without
`log_file_prefix`), holding `run-<id>.ct` and `output.log`. Trackers started
together never share one. <!-- AC/PY-35 -->

**`output.log` holds the whole output, whatever `verbose` says.** <!-- AC/PY-36 -->

**A `log_dir` that cannot be written is a tracking failure**, raised as
`TrackingError` unless `ignore_errors`. <!-- AC/PY-37 -->

## Reading runs back

**`report(*paths)` returns the `Report` that `carbontracker report` prints**:
its `runs`, `spans`, `total`, `incomplete` and `unreadable`, by the same rules
as [06](06-cli.md). <!-- AC/PY-38 -->

**`str(report)` is the CLI's runs table; `report.table(spans=True)` its spans
table; `report.csv(spans=False)` its CSV.** <!-- AC/PY-39 -->

**`report` on a path that does not exist raises `FileNotFoundError`.** <!-- AC/PY-40 -->

**`carbontracker.parser.aggregate_consumption(log_dir)` and
`print_aggregate(log_dir)` keep 2.4.7's names and sentence**, over the same
total as `report`. <!-- AC/PY-41 -->

## The command

**Installing the package installs `carbontracker`, the CLI of [06](06-cli.md)**,
with the same arguments, output and exit codes. <!-- AC/PY-42 -->

## What it never does

**It never calls `sys.exit`, reads `input()`, or restarts the tracker** to
change a key. 2.4.7's interface is kept; its hacks are not. <!-- AC/PY-43 -->

## Why

- **`ValueError` before probing** so that a typo costs nothing and never
  half-starts a run, even under `ignore_errors`.
- **Removed arguments are a `TypeError` naming the replacement** so that a
  2.4.7 script fails with the fix in the message.
- **Spans nest through context variables** so that threads and asyncio tasks
  each nest as their own code does.
- **A forked child tracks nothing** because it has the tracker but not its
  threads; sending to them would hang it.
- **`verbose=0` still writes `output.log`** so that a quiet job still leaves
  its log.

## Anchors

```anchor
carbontracker_py::Tracker
carbontracker_cli::run
carbontracker_run::Report
carbontracker_run::read_report
```
