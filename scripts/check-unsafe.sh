#!/bin/sh
# Guard against `unsafe` creeping into our own crates.
#
# The real guarantee is `#![forbid(unsafe_code)]` in each crate root, which the
# compiler enforces. This script is a second, cheaper line of defence that also
# catches someone deleting that attribute. It runs in CI and can be run locally.
#
# Usage: scripts/check-unsafe.sh
# Exit status: 0 clean, 1 violation found.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
status=0

# 1. Every crate root must carry the forbid attribute.
for root_file in "$root"/crates/*/src/lib.rs "$root"/crates/*/src/main.rs; do
    [ -f "$root_file" ] || continue
    if ! grep -q '^#!\[forbid(unsafe_code)\]' "$root_file"; then
        echo "MISSING: $root_file has no #![forbid(unsafe_code)]" >&2
        status=1
    fi
done

# 2. No `unsafe` keyword anywhere in our sources.
#
# Strip the permitted mentions before searching:
#   - the forbid attribute itself
#   - line comments, which discuss unsafety in prose
# Then look for `unsafe` as a whole word. This over-approximates (a string
# literal containing the word would trip it) which is the safe direction for a
# guard to err in.
found=$(
    find "$root/crates" -name '*.rs' -type f -print \
    | while IFS= read -r f; do
        sed -e 's://.*::' -e 's:#!\[forbid(unsafe_code)\]::' "$f" \
        | grep -n '[^A-Za-z0-9_]unsafe[^A-Za-z0-9_]\|^unsafe[^A-Za-z0-9_]\|[^A-Za-z0-9_]unsafe$' \
        | sed "s|^|${f#"$root/"}:|"
    done
)

if [ -n "$found" ]; then
    echo 'FOUND unsafe keyword in crate sources:' >&2
    echo "$found" >&2
    status=1
fi

if [ "$status" -eq 0 ]; then
    echo "unsafe guard: clean"
fi
exit "$status"
