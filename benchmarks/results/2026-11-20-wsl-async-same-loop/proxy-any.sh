#!/bin/bash
# proxy-any.sh <phase0|rsl> <proxy.sh args>
K=$1; shift
[ "$K" = rsl ] && export BIN=$HOME/async-target-rsl/release/cameleer-bench
[ "$K" = fix ] && export BIN=$HOME/async-target-fix/release/cameleer-bench
bash /mnt/e/Work/DataHQ/Code/Rust-camel/target-h2/async/proxy.sh "$@" | sed "s/^/build=$K /"
