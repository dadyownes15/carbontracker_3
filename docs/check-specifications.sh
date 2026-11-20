#!/bin/sh
# Fails if a symbol named in a docs/specifications/ `anchor` block is no longer
# exported. Catches renames and removals, not promises that quietly stopped
# being true.
#
#     sh docs/check-specifications.sh

set -u
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
specs="$root/docs/specifications"
[ -d "$specs" ] || { echo "no docs/specifications: nothing to check"; exit 0; }

crate_dir() {
    case "$1" in
        carbontracker) echo "$root/crates/engine" ;;
        carbontracker_providers) echo "$root/crates/providers" ;;
        carbontracker_run) echo "$root/crates/run" ;;
        carbontracker_cli) echo "$root/crates/cli" ;;
        carbontracker_py) echo "$root/crates/python" ;;
        *) echo "" ;;
    esac
}

missing=0
checked=0

for doc in "$specs"/*.md; do
    # Lines inside ```anchor … ``` blocks.
    awk '/^```anchor$/{inside=1;next} /^```$/{inside=0} inside' "$doc" |
    while IFS= read -r path; do
        [ -n "$path" ] || continue
        crate=${path%%::*}
        symbol=${path##*::}
        dir=$(crate_dir "$crate")
        if [ -z "$dir" ]; then
            echo "$(basename "$doc"): unknown crate '$crate' in '$path'"
            echo x >> "$specs/.missing"
            continue
        fi
        # A pub definition or a re-export of that exact name, anywhere in the crate.
        if ! grep -rqE "pub( +(fn|struct|enum|trait|type|const|mod|use))? +.*\b${symbol}\b" "$dir/src" 2>/dev/null; then
            echo "$(basename "$doc"): $path is no longer exported"
            echo x >> "$specs/.missing"
        fi
    done
done

[ -f "$specs/.missing" ] && missing=$(wc -l < "$specs/.missing" | tr -d ' ')
rm -f "$specs/.missing"
checked=$(awk '/^```anchor$/{inside=1;next} /^```$/{inside=0} inside' "$specs"/*.md | grep -c . )

if [ "${missing:-0}" -gt 0 ]; then
    echo
    echo "$missing of $checked anchored symbols are gone. Update the specification or the code."
    exit 1
fi
echo "specifications: $checked anchored symbols, all exported"
