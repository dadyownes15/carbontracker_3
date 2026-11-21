#!/bin/sh
# Fails unless every rule in docs/specifications/ is cited by a test.
#
# A rule is marked `<!-- AC/MEAS-3 -->` at the end of its paragraph. A test
# cites it by naming `AC/MEAS-3` in a comment. A rule marked
# `<!-- AC/MEAS-3 unbuilt -->` or `<!-- AC/MEAS-3 live-only -->` needs no test
# yet, and is listed instead.
#
#     sh docs/check-conformance.sh

set -u
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
specs="$root/docs/specifications"
[ -d "$specs" ] || { echo "no docs/specifications: nothing to check"; exit 0; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Every marked rule: "<id> <mark>", mark empty for a rule that needs a test.
grep -ohE '<!-- AC/[A-Z]+-[0-9]+( [a-z-]+)? -->' "$specs"/*.md |
    sed -E 's/<!-- (AC\/[A-Z]+-[0-9]+) ?([a-z-]*) -->/\1 \2/' > "$work/rules"
cut -d' ' -f1 "$work/rules" | sort > "$work/ids"
sort -u "$work/ids" > "$work/unique"
awk '$2 == "" { print $1 }' "$work/rules" | sort -u > "$work/tested"
awk '$2 != "" { print $1 " (" $2 ")" }' "$work/rules" | sort -u > "$work/exempt"

# Every id a test cites.
find "$root/crates" "$root/quickcheck" \( -path '*/target' -o -path '*/dist-newstyle' \) -prune -o \( -name '*.rs' -o -name '*.py' -o -name '*.hs' \) -print |
    xargs grep -ohE 'AC/[A-Z]+-[0-9]+' 2>/dev/null | sort -u > "$work/cited"

failed=0
duplicates=$(sort "$work/ids" | uniq -d)
if [ -n "$duplicates" ]; then
    echo "rules marked twice:"; echo "$duplicates" | sed 's/^/  /'; failed=1
fi
untested=$(comm -23 "$work/tested" "$work/cited")
if [ -n "$untested" ]; then
    echo "rules no test cites:"; echo "$untested" | sed 's/^/  /'; failed=1
fi
unknown=$(comm -13 "$work/unique" "$work/cited")
if [ -n "$unknown" ]; then
    echo "tests citing a rule no specification has:"; echo "$unknown" | sed 's/^/  /'; failed=1
fi

rules=$(wc -l < "$work/unique" | tr -d ' ')
exempt=$(wc -l < "$work/exempt" | tr -d ' ')
if [ "$exempt" -gt 0 ]; then
    echo "rules with no test yet, by mark:"; sed 's/^/  /' "$work/exempt"
fi
[ "$failed" -eq 0 ] || exit 1
echo "conformance: $rules rules, $((rules - exempt)) cited by a test, $exempt marked"
