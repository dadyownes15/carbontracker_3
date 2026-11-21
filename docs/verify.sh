#!/bin/sh
# Everything offline and deterministic, fast first. --live adds the tests that
# need network and API keys. See docs/TEST.md.
set -eu
# This script lives in docs/; it runs from the repository root.
cd "$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"

# The repo's own virtualenv, if there is one, holds maturin and pytest.
if [ -d .venv/bin ]; then PATH="$PWD/.venv/bin:$PATH"; fi

live=no
[ "${1:-}" = "--live" ] && live=yes

say() { printf '\n\033[1m== %s\033[0m\n' "$1"; }

if [ -n "${CARBONTRACKER_BLESS:-}" ]; then
    echo "CARBONTRACKER_BLESS is set: format tests would rewrite their own expectations." >&2
    exit 2
fi

say "lints"
# Not `cargo fmt --check`: parts of the tree are deliberately hand-formatted
# (the clap attributes in crates/cli/src/args.rs), so rustfmt is not clean at
# any max_width. Decide that before CI; see docs/TEST.md.
cargo clippy --workspace --all-targets -- -D warnings

say "specifications"
sh docs/check-specifications.sh
sh docs/check-conformance.sh

say "rust: L1-L6"
cargo test --workspace

say "quickcheck: L3 in Haskell"
if command -v cabal >/dev/null 2>&1 && [ -d quickcheck ]; then
    cargo build -q -p carbontracker --features replay --bin replay-pure 2>/dev/null \
        || echo "replay binary did not build: trace properties will skip"
    (cd quickcheck && QUICKCHECK_CASES="${QUICKCHECK_CASES:-200}" cabal test -v0 --test-show-details=direct)
else
    echo "quickcheck: skipped (no cabal)"
fi

say "python: L6"
if command -v maturin >/dev/null 2>&1; then
    maturin develop -m crates/python/Cargo.toml --quiet
    pytest -q crates/python/tests
else
    echo "maturin not installed: skipped (see docs/TEST.md)"
fi

say "L7: printouts for a person to read"
cargo review-printouts >/dev/null
echo "read: target/review/printouts.html"

if [ "$live" = yes ]; then
    say "L8: live"
    cargo test -p carbontracker-providers --test live_location -- --ignored
    if [ -n "${CARBONTRACKER_EM_FREE_KEY:-}" ]; then
        cargo test -p carbontracker-providers --test live_intensity -- --ignored free_key
    else
        echo "CARBONTRACKER_EM_FREE_KEY unset: free_key skipped"
    fi
    if [ -n "${CARBONTRACKER_EM_PAID_KEY:-}" ]; then
        cargo test -p carbontracker-providers --test live_intensity -- --ignored paid_key
    else
        echo "CARBONTRACKER_EM_PAID_KEY unset: paid_key skipped"
    fi
else
    say "L8: live"
    echo "skipped. docs/verify.sh --live to run them."
fi

printf '\n\033[1mok\033[0m\n'
