#!/bin/bash
# build-rsl.sh: cameleer-bench against the exported simoom rt-same-loop, as ~/async-target-rsl/release/cameleer-bench
export PATH=$HOME/.cargo/bin:$PATH
cd /mnt/e/Work/DataHQ/Code/Rust-camel/.claude/worktrees/affectionate-dijkstra-0f0dea
S=/mnt/e/Work/DataHQ/Code/Rust-camel/target-h2/simoom-rsl/crates
ARGS=()
for c in simoom simoom-client simoom-h2 simoom-hpack simoom-http1 simoom-rt simoom-sys simoom-tls; do ARGS+=(--config "patch.crates-io.$c.path=\"$S/$c\""); done
CARGO_TARGET_DIR=$HOME/async-target-rsl cargo build --release -p cameleer-bench "${ARGS[@]}" 2>&1 | grep -E "^(warning|error)|Finished"
