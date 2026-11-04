# The suite

How the specifications in this directory are verified, and what each kind of
test is for.

## Acceptance criteria

A specification states a promise. A criterion makes it checkable. A test names the
criterion.

> **Promise** (02-engine): a subscriber whose queue fills is cut off, and its
> stream ends with a signal it can tell apart from the session ending.
>
> **AC/ENG-21.1** A subscriber whose bounded queue fills receives no further
> events.
> **AC/ENG-21.2** Its stream ends with a signal distinguishable from the
> session ending.
> **AC/ENG-21.3** Delivery to a full queue completes without blocking.
> **AC/ENG-21.4** It may subscribe again and receives events from the next one
> on, with no replay.

```rust
/// AC/ENG-21.2
#[test]
fn a_subscriber_whose_queue_fills_is_told_it_was_cut_off() { … }
```

Rules:

- **Write criteria at the granularity of a behaviour someone could observe**,
  never of an internal type. `the ledger map is cleared` is a change detector,
  not a test.
- **Not every promise needs a test.** Some are verified by the compiler or a
  lint. Say so: a specification whose verification is *"the `unnameable_types`
  lint"* is complete; one that silently has no test is a hole nobody can tell
  from a decision.
- **Criteria you choose not to test go in the specification's "Why"**,
  with the reason. Silence is the thing to avoid.

## Levels

Each level owns one seam. A test belongs at the **highest** level that can
still fail for exactly one reason.

| | Level | Owns | Lives in |
|---|---|---|---|
| L1 | unit | Domain types: units, sample kinds, timeline arithmetic, the read gate | `crates/*/src/**` in-module `#[cfg(test)]` |
| L2 | model | The accounting model over random sessions: conservation, settle-once, billing only what was selected | `crates/engine/src/domain/properties.rs` |
| L3 | simulation | A whole session over generated programs, hosts and worlds; the read gate against generated streams of requests | `quickcheck/` against `replay-pure`; `crates/engine/src/tests/{gate,sample_store}.rs` |
| L4 | contract | Everything a downstream crate sees, through the public API, pinned so values are exact; how providers are read, with real threads | `crates/engine/tests/{pinned_run,reading,long_run}.rs`, `crates/providers/src/application/tests/` |
| L5 | format | Every v1 fixture still reads; this release writes what it says | `crates/run/tests/record.rs` |
| L6 | end to end | The real binary and the real Python package, pinned, offline | `crates/cli/tests/`, `crates/python/tests/` |
| L7 | review | Wording and layout, by a person | `crates/cli/tests/review_printouts/` |
| L8 | live | That a real API still answers the shape we parse | `crates/providers/tests/live_*.rs` |

**L1–L7 are offline and deterministic.** Power and intensity are pinned, the
config directory is a temporary one, and `CARBONTRACKER_EM_KEY` is removed, so
nothing touches the network and the user's own key never leaks into a test. <!-- AC/TST-1 -->

**L8 is `#[ignore]`d** and runs on demand with its own keys. It asserts the
*shape* of an answer, never a value, and makes one fetch per endpoint per test
to stay inside the rate limit. When one fails on shape, re-record the fixtures
the offline levels use. <!-- AC/TST-2 -->

## Segments and billing

**Each property of segments is checked where it is decided.** The rules are
[08](08-measurements.md) and [09](09-profiling.md).

**Billing, one segment at a time (L1).** No session and no timing: a segment
and a store in, bills out.

| Property | Checked by |
|---|---|
| Shares sum to one and name only claimants | Invariant | <!-- AC/TST-3 -->
| A segment's bills sum to its measured usage × PUE | Invariant | <!-- AC/TST-4 -->
| A zero-width segment bills nothing | Invariant | <!-- AC/TST-5 -->
| Splitting a segment in two, same claimants, bills the same | Metamorphic | <!-- AC/TST-6 -->
| Scaling the source's intensity by `k` scales emissions by `k`, energy unchanged | Metamorphic | <!-- AC/TST-7 -->
| Changing one intensity source changes nothing paired with another | Metamorphic | <!-- AC/TST-8 -->
| No intensity source leaves emissions absent, never zero | Invariant | <!-- AC/TST-9 -->

**The timeline (L2).** Commands in, segments out; no store.

| Property | Checked by |
|---|---|
| On each source, segments tile the session with no gap or overlap | Invariant | <!-- AC/TST-10 -->
| A segment's claimants selected its source and have no running descendant that did | Invariant | <!-- AC/TST-11 -->
| Adding a span on other sources changes no segment on this one | Metamorphic | <!-- AC/TST-12 -->
| Adding marks only splits segments, never changes their claimants | Metamorphic | <!-- AC/TST-13 -->

**The session over generated worlds (L3).** The world, the readouts and the
timing are all generated.

| Property | Checked by |
|---|---|
| Each segment is billed once, in order per source | Invariant | <!-- AC/TST-14 -->
| On each source, bills to spans plus the root's equal PUE × `∫P` on the store they were billed from | Invariant | <!-- AC/TST-15 -->
| A child settles before or with its parent, and every closed span settles once | Invariant | <!-- AC/TST-16 -->
| A span settles with no read landed for a source it did not select | Liveness | <!-- AC/TST-17 -->
| While a provider stays unready, cuts send it at most one request | Bound | <!-- AC/TST-18 -->
| Under `Queue`, a segment is accounted within `max(cooldown, timeout) + timeout` of its end | Bound | <!-- AC/TST-19 quickcheck -->
| Under `Queue`, no segment's first sliver is billed from an older sample | Invariant | <!-- AC/TST-20 quickcheck -->
| With every read landed before billing, adding marks changes no bill | Metamorphic | <!-- AC/TST-21 quickcheck -->
| A span is billed only for sources it selected | Invariant | <!-- AC/TST-28 -->
| With a constant intensity, emissions are energy × intensity, and absent with none | Invariant | <!-- AC/TST-29 -->
| A span counts the samples that landed inside it, and warns of thin coverage below two | Invariant | <!-- AC/TST-30 -->

## The QuickCheck suite

**A session is written once and run by several interpreters.** In
`quickcheck/`, a caller's program is a `Plan`: first-order data in which a
span is a block, so every plan is well-formed and shrinking cannot break
one. A plan runs as a `Session`, a free monad of caller operations whose
continuations receive span handles; programs never name span ids. One
interpreter schedules it into timed ops, a model of the providers turns
those into the readouts a session sees, and `replay-pure` runs them through
the real `Session`, store and catalog with no threads and no clock. A
stateful replay through the public `Tracker` will run the same programs with
real threads; it is not built yet. <!-- AC/TST-31 -->

**Input is real use, at real size.** Hosts are those carbontracker runs on
(an Intel laptop, Apple Silicon, a GPU server with up to eight GPUs, a live
grid and a static fallback) or any mix of providers. Programs are shapes of
real work: training epochs with one span per GPU per step, sweeps,
concurrent serving, notebook cells, and spans far shorter than the readout
interval. Sizes grow with QuickCheck's size parameter, so small cases come
first. <!-- AC/TST-32 -->

**A property names the cases that make it non-vacuous, and fails without
them.** Each run demands a minimum share of cases with queued requests,
answers with no read, timeouts, panics, watchdog reads, children on sources
their parents did not select, ten or more sources, and spans on both sides
of the thin-coverage boundary. A property that stops seeing them fails as
too trivial rather than passing. <!-- AC/TST-33 -->

**The provider model is input, not an oracle.** It follows 08's rules
closely enough to be realistic; no property relies on it being exact. The
expected values come from the program, the world and the trace. <!-- AC/TST-34 -->

**A failing case is written to `quickcheck/failures/` as the JSON
`replay-pure` reads**, so it can be run again by hand. GHC may be absent:
the suite then reports itself skipped, never passed. <!-- AC/TST-35 -->

## Generators and oracles

**A generator produces realistic input; an oracle says what the answer should
be. The oracle must never be a second implementation.** <!-- AC/TST-22 -->

`quickcheck/src/Quickcheck/World.hs` is the pattern to follow. A world is
written in its own types, so the engine can change without rewriting the
worlds, and the expected answer is known **by construction**: on a constant
world each source draws a known number of watts, so its bill is PUE × watts
× session length. It does not reimplement the engine's accounting and then
check the two agree.

A test that computes the expected value the way the code does proves only that
the code is self-consistent.

## Known defects

A property that does not hold stays in the suite, `#[ignore]`d, with the reason
on the attribute: <!-- AC/TST-23 -->

```rust
#[ignore = "known: a segment closed by an empty readout is billed a held value
            that later samples contradict"]
```

Under `Skip`, the order-dependence and held-value non-conservation in
[08](08-measurements.md) are accepted trade-offs, kept this way. <!-- AC/TST-24 -->

**Under `Queue`, the ledgers do not depend on the order answers land in,**
as long as each provider's answers land in the order its requests were sent.
`under_queue_answer_order_does_not_change_ledgers` checks this over every
interleaving of providers. The version that shuffles whole readouts across
providers stays `#[ignore]`d: it is the `Skip` trade-off above. <!-- AC/TST-25 -->

An accepted trade-off is a record, not a broken test. Deleting it
loses the knowledge; un-ignoring it is a decision, not a fix.

## Blessing

Golden records are rewritten with `CARBONTRACKER_BLESS=1`, never automatically.
When the format changes on purpose, bless the current file **and keep the old
one under a new name**, so it is still read. The bless flag must never be set
in any automated run: it turns every format test into a tautology. <!-- AC/TST-26 -->

## Drift

A check script reads every `anchor` block in `docs/specifications/` and
fails if a listed symbol is no longer exported. It is the only mechanical link
between these documents and the code: a specification whose symbols are gone breaks it. <!-- AC/TST-27 script -->

It is not a substitute for reading. It catches renames and removals, not a
promise that quietly stopped being true.

## Not verified

Stated so the gaps are decisions, not oversights:

- **Cross-frontend equivalence.** Nothing asserts the CLI and the Python
  package produce the same record for the same pinned world and the same
  spans. A real class of bug; deferred as a piece of work of its own.
- **Overhead in wall-clock time.** What the engine holds is bounded by
  `held()`, asserted by unit tests in `tracker.rs`, and live bytes level off
  over a million-sample run (`long_run.rs`). Per-readout cost is not
  checked: a timing budget is the flaky kind of test and buys less than it
  costs.
- **Timing under load.** `replay-pure` takes its timing from the case, so a
  fault that appears only under real thread scheduling is invisible to L3
  until the stateful replay exists.
- **Live sets on a cadence.** They run only when someone runs them, so a
  provider's response shape can change without anyone finding out.
