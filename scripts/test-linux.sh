#!/bin/sh
# Runs the whole test suite on Linux, in a container.
#
# The tests cover things that differ between systems — trash, renaming, file
# times, read-only volumes, path separators — and with continuous integration
# unavailable they would otherwise only ever run on the Mac the work is done on.
#
# The container matches this machine's processor rather than the x86-64 the
# Linux release is built for: what differs between systems is the system, and
# emulating another processor would turn three minutes into thirty. Its build
# and download caches live in named volumes, so the Mac's own `target` is left
# alone and later runs start warm.
#
# Usage: scripts/test-linux.sh [cargo test arguments]

set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)

exec docker run --rm \
    -v "$root":/src \
    -v fitsview-linux-target:/target \
    -v fitsview-linux-cargo:/usr/local/cargo/registry \
    -e CARGO_TARGET_DIR=/target \
    -w /src \
    rust:1-bookworm \
    cargo test --workspace --all-features "$@"
