# The system

carbontracker measures a computation's energy use and attributes it to named
spans of work.

## Principles

1. **The engine measures and attributes. Nothing else.** It never prints,
   writes files or formats for people.
2. **Two outputs only.** Events are pushed; queries are pulled. Streams go out
   as events, snapshots come back as query answers.
3. **The engine forgets what it has sent.** A settled span goes out whole in
   its event and is then forgotten. Samples are dropped once every segment
   that reads them is accounted. What stays is open spans, unaccounted
   segments, the samples those read, and running totals.
4. **The engine never probes.** It starts over the `Sources` it is given. What
   to measure is a provider registry's business.
5. **The engine produces, the frontend shows.** The engine's verbosity is
   which event kinds it announces. How much of what it receives a frontend
   shows, and the layout and wording, are the frontend's.
6. **A frontend never calls a provider.** Downstream estimates take intensity
   from the events the engine already sent.

## Crates

| Crate | Is | Depends on |
|---|---|---|
| `carbontracker` | The engine: measuring, attribution, events, queries, and the provider port | — |
| `carbontracker-providers` | The default provider set: probe this host | engine |
| `carbontracker-run` | A run: probe, start, forward items to sinks, write the record | engine, providers |
| `carbontracker-cli` | `carbontracker <command>`: tracks a whole run | engine, providers, run |
| `carbontracker-py` | Python bindings and the `carbontracker` command | engine, providers, run, cli |

Dependencies run one way, left to right. The engine depends on nothing in this
table, and nothing in this table reaches around `carbontracker-run` to the
engine except by the types that crate re-exports.

## Threads

A tracker runs on its own runtime, never the caller's. `start_span`, `mark` and
`stop_span` are fire-and-forget and never block the caller, whatever the engine
is doing.

A run adds two more threads. The **forwarder** takes engine events, hands each
item to the run's `Projection`, and passes it on; it does no I/O, so it never
falls behind the engine. The **writer** hands each item to every sink, and may
block on I/O.

## Provider sets

Each use case brings one provider set, written against the public provider
port. The default set (`carbontracker-providers`) deduces what to measure from
the host. A Slurm or container tracker brings its own, so job-scoped and
device-total sources are never mixed in one session. This is why providers are
a crate and the port is public: a new use case adds a provider set without
touching the engine or the other sets.

## Anchors

```anchor
carbontracker::start
carbontracker::Sources
carbontracker::Provider
carbontracker_providers::ProviderRegistry
carbontracker_run::start
carbontracker_run::RunHandle
```
