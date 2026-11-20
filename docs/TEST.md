# Testing carbontracker

```sh
docs/verify.sh            # everything offline and deterministic
docs/verify.sh --live     # also the tests that need network and API keys
```

Everything below `--live` is offline: power and intensity are pinned, the
config directory is a temporary one, and `CARBONTRACKER_EM_KEY` is removed, so
no test touches the network and your own key never leaks into one.

## The levels

A test belongs at the highest level that can still fail for exactly one reason.

| | Level | Checks | Run just this |
|---|---|---|---|
| L1 | unit | Domain types: units, interpolation, timeline arithmetic | `cargo test --workspace --lib` |
| L2 | model | The accounting model over random sessions | `cargo test -p carbontracker --lib properties` |
| L3 | simulation | A whole session against generated realistic worlds | `cargo test -p carbontracker --lib tests::` |
| L4 | contract | The public API, pinned so values are exact | `cargo test -p carbontracker --test pinned_run` |
| L5 | format | Every record fixture still reads | `cargo test -p carbontracker-run --test record` |
| L6 | end to end | The real binary and the real Python package | `cargo test -p carbontracker-cli --test cli` |
| L7 | review | Wording and layout, by a person | `cargo review-printouts` |
| L8 | live | That a real API still answers the shape we parse | see below |

## L7 — reading the printouts

```sh
cargo review-printouts && open target/review/printouts.html
```

Every standard printout, in the terminal's font and colours, a tab per
frontend and per command. **Nothing here checks the wording** — it is for a
person to read. Look at it whenever you change what carbontracker says.

## L8 — the live tests

Ignored by default: they need network, and the Electricity Maps tests need a
key. Plans reach different endpoints, so they come in two sets.

```sh
CARBONTRACKER_EM_FREE_KEY=... \
  cargo test -p carbontracker-providers --test live_intensity -- --ignored free_key

CARBONTRACKER_EM_PAID_KEY=... \
  cargo test -p carbontracker-providers --test live_intensity -- --ignored paid_key

cargo test -p carbontracker-providers --test live_location -- --ignored
```

`live_location` sends this host's IP to ipinfo.io.

Each asserts the *shape* of an answer, never a value, and makes one fetch per
endpoint to stay inside the rate limit. **When one fails on shape, re-record
the fixtures** the offline tests use (`crates/providers/src/application/tests/`).

## Python

```sh
maturin develop -m crates/python/Cargo.toml && pytest crates/python/tests
```

The extension must be rebuilt after any Rust change, or pytest tests the last
build.

## Blessing a record

The golden files in `crates/run/tests/fixtures/records/v<N>/` are the record
format's contract: every release must read every one of them.

```sh
CARBONTRACKER_BLESS=1 cargo test -p carbontracker-run --test record
```

When the format changes on purpose, bless the current file **and keep the old
one under a new name**, so it is still read. Never set `CARBONTRACKER_BLESS` in
an automated run: it turns every format test into a tautology.

## Ignored tests that are meant to stay ignored

Two properties in `crates/engine/src/domain/properties.rs` are `#[ignore]`d with
`known:` on the attribute. They document accepted trade-offs in how empty and
out-of-order readouts are billed. They are a record of a decision, not a broken
test — don't delete them, and un-ignoring one is a decision, not a fix.

## Saved proptest seeds

`crates/engine/proptest-regressions/` holds seeds of cases that have failed
before. Proptest writes them **automatically** on a failure and replays them
before any new case, so a committed seed keeps failing until the defect is
fixed. That is the point — but it means a red proptest is permanently red, not
flaky, and `cargo test` will not go green by rerunning it.

When a saved case turns out to be an accepted trade-off rather than a bug,
move it to an `#[ignore = "known: …"]` test and delete its seed. Leaving it in
the regressions file keeps the suite red and teaches everyone to ignore a
failure, which is the one habit that makes a suite worthless.

## Writing a test

- **Name the test as a sentence.** `the_exit_code_is_the_commands_and_the_closing_block_follows`.
- **Never write an oracle that reimplements the code.** Expected values are
  known by construction — see `Truth::integral_of_product` in
  `crates/engine/src/tests/sample_store.rs`. A test that computes the answer the way
  the code does proves only that the code agrees with itself.
- **Pin power and intensity** in anything above L3, so expected values are
  exact (`WATTS * duration` joules) and nothing is probed.
