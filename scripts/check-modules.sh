#!/bin/sh
# Fails if a source file is never declared as a module.
#
# Rust says nothing about a .rs file that no `mod` statement mentions: it is
# simply never compiled. So a whole feature can be written, the build stay
# green, every test pass, and none of it run. That happened here: a histogram
# module was written and wired up in three places, and did nothing, because one
# `mod histogram;` line was missing.
#
# The declaration must be in the file's OWN parent module, not just somewhere
# in the workspace: two crates can each have a `histogram.rs`, and finding one
# says nothing about the other.
#
# Usage: scripts/check-modules.sh
# Exit status: 0 clean, 1 an orphan was found.

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
status=0
checked=0

# Where a module living in `dir` would have to be declared.
parent_of() {
    dir=$1
    if [ -f "$dir/mod.rs" ]; then
        echo "$dir/mod.rs"
    elif [ -f "$dir/lib.rs" ]; then
        echo "$dir/lib.rs"
    elif [ -f "$dir/main.rs" ]; then
        echo "$dir/main.rs"
    else
        echo ""
    fi
}

for file in $(find "$root/crates" -name '*.rs' -type f | sort); do
    case "$file" in
        */benches/*|*/tests/*|*/examples/*) continue ;;
    esac
    name=$(basename "$file" .rs)
    dir=$(dirname "$file")

    case "$name" in
        lib|main) continue ;;
        # A directory module is declared in ITS parent, by the directory name.
        mod)
            name=$(basename "$dir")
            dir=$(dirname "$dir")
            ;;
    esac

    parent=$(parent_of "$dir")
    if [ -z "$parent" ]; then
        echo "ORPHAN: ${file#"$root/"} has no parent module file" >&2
        status=1
        continue
    fi

    checked=$((checked + 1))
    if ! grep -qE "^[[:space:]]*(pub |pub\\(crate\\) )?mod ${name};" "$parent"; then
        echo "ORPHAN: ${file#"$root/"} is not declared in ${parent#"$root/"}," >&2
        echo "        so it is never compiled and does nothing" >&2
        status=1
    fi
done

if [ "$status" -eq 0 ]; then
    echo "module check: clean ($checked modules)"
fi
exit "$status"
