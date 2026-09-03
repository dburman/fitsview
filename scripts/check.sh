#!/bin/sh
# Runs everything continuous integration runs, and fails if any of it fails.
#
# Written as a script rather than left as a command to paste because the
# obvious one-liner is easy to get wrong. Summarising `cargo test` output by
# adding up the "N passed" numbers looks like it works and silently ignores
# failures, which is how a broken test survived two phases of this project.
# Exit status is the only summary worth trusting.
#
# Usage: scripts/check.sh

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

status=0
run() {
    name=$1
    shift
    printf '%-28s' "$name"
    if output=$("$@" 2>&1); then
        echo "ok"
    else
        echo "FAILED"
        echo "$output" | tail -30 | sed 's/^/    /'
        status=1
    fi
}

run "formatting" cargo fmt --all --check
run "lints" cargo clippy --workspace --all-targets --all-features -- -D warnings
run "tests" cargo test --workspace --all-features
run "unsafe guard" ./scripts/check-unsafe.sh

# Optional extras: skipped rather than failed when the toolchain or target for
# them is not installed, since they are not needed to work on the project.
msrv=$(grep -m1 '^rust-version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
if rustup toolchain list 2>/dev/null | grep -q "^${msrv}"; then
    run "minimum rust ($msrv)" cargo "+$msrv" check --workspace --all-features
else
    printf '%-28s%s\n' "minimum rust ($msrv)" "skipped (rustup toolchain install $msrv)"
fi

if rustup target list --installed 2>/dev/null | grep -q x86_64-pc-windows-msvc; then
    run "windows build" cargo check --target x86_64-pc-windows-msvc --workspace --all-features
else
    printf '%-28s%s\n' "windows build" "skipped (rustup target add x86_64-pc-windows-msvc)"
fi

if [ "$status" -eq 0 ]; then
    echo "all checks passed"
else
    echo "some checks failed" >&2
fi
exit "$status"
