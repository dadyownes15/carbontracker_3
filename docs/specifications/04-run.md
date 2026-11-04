# A run — `carbontracker-run`

Everything between a frontend and the engine: probe, start, forward, write.
Every frontend uses it, so none of them talks to the engine directly.

## Starting

`start(config, projection)` probes this host, starts the engine and returns a
`RunHandle`. That handle is the frontend's **only** way into the engine. <!-- AC/RUN-1 -->

**The run hands the forecaster to the projections that want it.**
`projection` is built once the engine is up, from a weak handle on the engine
and the run's `Forecaster`. A projection that wants the forecast reads it through
a `LatestForecast` built from the `Forecaster`'s `latest()`, never from the
engine. Reading never blocks; until a fetch has finished there is no forecast
yet. <!-- AC/RUN-2 -->

**A grid that falls to the world average without being asked for says why**,
in a warning just after `Started`: no location could be estimated, or nothing
serves the one there is, because Electricity Maps has no key (or declined it)
and the country table needs a country it lists. An asked-for world average
(`global`, profiler mode) or a stated intensity warns nothing. <!-- AC/RUN-23 -->

## The item stream

Everything a run does, from the first probe step to the session's end, reaches
every sink as an `Item`, in one order. <!-- AC/RUN-3 -->

| Item | Is |
|---|---|
| `Probing` | One probe step | <!-- AC/RUN-4 -->
| `Started` | The run's header, the probe, the catalog, and what the forecast is, before the first event | <!-- AC/RUN-5 -->
| `Event` | One engine event, in order | <!-- AC/RUN-6 -->
| `Gap` | Events were lost here: the subscription was cut off, and no engine event follows it | <!-- AC/RUN-7 -->
| `Outcome` | How the run ended | <!-- AC/RUN-8 -->
| `Warning` / `Error` | Text for the user | <!-- AC/RUN-9 -->
| `Block` | A rendered block from the projection | <!-- AC/RUN-10 -->

Two threads carry it. The **forwarder** hands each item to the `Projection` and
passes it on; it does no I/O, so it never falls behind the engine. The
**writer** hands each item to every sink and may block. <!-- AC/RUN-11 -->

## `Sink`

The port every output implements. Called on the writer thread, never the
engine's or the caller's, so a sink may block on I/O. Only `start`, `event` and
`name` must be implemented; the rest default to doing nothing. <!-- AC/RUN-12 --> <!-- AC/RUN-13 -->

`deliver(sink, item)` hands an item to the sink method for its kind. It is not
part of the trait, so no sink can route items around its own methods.

**A sink that errs is finished and dropped; the others go on.** A failing sink
never fails the run, and the user is told which sink stopped. <!-- AC/RUN-14 -->

`ConsoleSink` writes the human-readable lines. `JsonlSink` writes the record.
`standard_sinks` is the pair every frontend uses. <!-- AC/RUN-15 -->

## `Projection`

**The engine keeps no running totals for anyone else.** It announces what
happened and forgets it, so a frontend that needs to know more than the last
event told it has nowhere to look. A `Projection` is that place: somewhere to
accumulate state from the run's items as they go past, without the engine
having to hold it or know who wanted it. <!-- AC/RUN-16 -->

**Every frontend that remembers anything does it here.** Python's prediction is
the one that exists: it keeps the epochs that have settled so far and
extrapolates them to a whole run at the forecast's mean intensity. The same
shape serves what hasn't been built yet — a guard that stops a run once it has
emitted too much, or a queue that waits for the grid to get cleaner — because
all of them are the same problem: watch the stream, keep a little state, answer
a question about it. <!-- AC/RUN-17 -->

```rust
trait Projection {
    /// One item has arrived. Update, and return anything to print.
    fn project(&mut self, item: &Item) -> Vec<Box<dyn Block>>;

    /// Add to the record's header, once, before the run starts.
    fn describe(&self, header: &mut record::Header) {}
}
```

**It is updated on the forwarder thread and read by the frontend**, through a
`View` that shares it under a lock. The frontend asks whenever it likes; the
projection is as current as the last item forwarded. <!-- AC/RUN-18 -->

**It must do no I/O.** The forwarder is what keeps the run's subscriber from
falling behind the engine and being cut off, and the frontend may be waiting on
the same lock. Printing is not an exception: a projection returns blocks for
the console rather than writing them, and the writer thread prints them just
before the item they came from. <!-- AC/RUN-19 -->

**A run that needs no state leaves it out.** `()` implements `Projection` and
does nothing, so the CLI pays for none of this. <!-- AC/RUN-20 -->

## Ending

`finish(outcome)` returns the `SessionReport` and the final `View`. The outcome
says how the run ended: `Completed`, `Monitored`, `Interrupted`, `Failed`,
`Exited { code }` or `Signalled { signal }`. <!-- AC/RUN-21 -->

## The run folder

With a log directory, each run gets a folder in it holding the run's record and
the command's output. `--no-log` writes nothing. <!-- AC/RUN-22 -->

## Why

- **The forwarder does no I/O** so that a slow disk or terminal can never make
  the run's subscriber fall behind and be cut off by the engine.
- **A failing sink is dropped, not fatal**, because losing the log is not a
  reason to lose the computation being measured.

## Anchors

```anchor
carbontracker_run::start
carbontracker_run::RunHandle
carbontracker_run::RunConfig
carbontracker_run::RunError
carbontracker_run::View
carbontracker_run::Sink
carbontracker_run::deliver
carbontracker_run::Projection
carbontracker_run::Item
carbontracker_run::Block
carbontracker_run::Layout
carbontracker_run::Outcome
carbontracker_run::Started
carbontracker_run::ForecastSource
carbontracker_run::LatestForecast
carbontracker_run::ConsoleSink
carbontracker_run::JsonlSink
carbontracker_run::standard_sinks
carbontracker_run::Predictor
carbontracker_run::Folder
carbontracker_run::Verbosity
carbontracker_run::VERSION
carbontracker_run::record
carbontracker_run::presentation
```
