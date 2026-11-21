#!/usr/bin/env bash
# Runs the QuickCheck suite and, when it passes, writes quickcheck/passed: a
# hash of everything the suite depends on, and the number of cases. On a
# pull request, CI skips its own run when the hash matches the code it checks
# out, so commit the stamp after your last change. Any later change breaks
# the match and CI runs the suite again. A push to main or staging and the nightly run
# ignore the stamp. AC/TST-38
#
#   quickcheck/run.sh          run the suite and write the stamp
#   quickcheck/run.sh hash     print the hash of the current tree
#
# QUICKCHECK_CASES sets the cases per property (default 200).
set -euo pipefail

root="$(git -C "$(dirname "$0")" rev-parse --show-toplevel)"
cd "$root"

# What the suite depends on: the engine, the suite, and the locked
# dependencies. The stamp itself is left out.
hash_inputs() {
  git ls-files -z -- crates/engine quickcheck Cargo.lock \
    | grep -zv '^quickcheck/passed$' \
    | while IFS= read -r -d '' path; do
        printf '%s %s\n' "$(git hash-object -- "$path")" "$path"
      done \
    | shasum -a 256 | cut -d' ' -f1
}

if [ "${1:-}" = "hash" ]; then
  hash_inputs
  exit 0
fi

cases="${QUICKCHECK_CASES:-200}"
before="$(hash_inputs)"
cargo build -p carbontracker --features replay --bins
(cd quickcheck && QUICKCHECK_CASES="$cases" cabal test -v0 --test-show-details=direct)
if [ "$(hash_inputs)" != "$before" ]; then
  echo "quickcheck/run.sh: files changed during the run; no stamp written" >&2
  exit 1
fi
printf '%s\n%s\n' "$before" "$cases" > quickcheck/passed
echo "quickcheck/run.sh: passed at $cases cases; commit quickcheck/passed"
