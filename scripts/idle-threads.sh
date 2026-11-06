#!/usr/bin/env bash
# Build and run the `idle_threads` example (perf-review finding 16: timer and supervising
# controller threads, XQuery/XSLT latency per evaluation, local status file writes) on Linux,
# e.g. under WSL2.
#
#   scripts/idle-threads.sh <source>      # <source>: a checkout directory or a `git archive` tar
#
# The sources are copied to $IDLE_THREADS_DIR (default ~/idle-threads) so the build runs on the
# Linux file system; build output goes to $IDLE_THREADS_DIR/target.
set -euo pipefail

src="${1:?usage: $0 <checkout directory or git archive tar>}"
work="${IDLE_THREADS_DIR:-$HOME/idle-threads}"
name="$(basename "$src")"
dest="$work/src-${name%.tar}"
rm -rf "$dest"
mkdir -p "$dest"
if [ -f "$src" ]; then
    tar -xf "$src" -C "$dest"
else
    tar -C "$src" --exclude=./target --exclude=./.git -cf - . | tar -xf - -C "$dest"
fi
cd "$dest"
export CARGO_TARGET_DIR="$work/target"
cargo build --release -p cameleer-main --example idle_threads 2>&1 | grep -v '^ *Compiling' || true
"$CARGO_TARGET_DIR/release/examples/idle_threads"
