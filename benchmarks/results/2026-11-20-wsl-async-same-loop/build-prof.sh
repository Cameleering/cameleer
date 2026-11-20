#!/bin/bash
# build-prof.sh: profiling build of cameleer-bench on simoom rt-same-loop (frame pointers, line tables)
export PATH=$HOME/.cargo/bin:$PATH
export CARGO_TARGET_DIR=$HOME/async-target-prof
export RUSTFLAGS="-C force-frame-pointers=yes"
export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
cd /mnt/e/Work/DataHQ/Code/Rust-camel/.claude/worktrees/affectionate-dijkstra-0f0dea
S=/mnt/e/Work/DataHQ/Code/Rust-camel/target-h2/simoom-rsl/crates
ARGS=()
for c in simoom simoom-client simoom-h2 simoom-hpack simoom-http1 simoom-rt simoom-sys simoom-tls; do ARGS+=(--config "patch.crates-io.$c.path=\"$S/$c\""); done
cargo build --release -p cameleer-bench "${ARGS[@]}" 2>&1 | grep -E "^(warning|error)|Finished"
