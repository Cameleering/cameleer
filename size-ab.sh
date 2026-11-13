#!/usr/bin/env bash
# Stripped binary size of cameleer-main (dist profile), with and without the simoom engine.
cd /mnt/e/Work/DataHQ/Code/Rust-camel/.claude/worktrees/simoom-spike || exit 1
export CARGO_TARGET_DIR=$HOME/simoom-size-target CARGO_INCREMENTAL=0
for cfg in "core|--no-default-features --features core" "full|"; do
  name=${cfg%%|*}; flags=${cfg#*|}
  for eng in classic simoom; do
    extra=""; [ $eng = simoom ] && extra="--features cameleer-http/simoom"
    cargo build -q --profile dist -p cameleer-main $flags $extra 2>&1 | tail -2
    strip -o /tmp/cameleer-$name-$eng target-none 2>/dev/null
    strip -o /tmp/cameleer-$name-$eng "$CARGO_TARGET_DIR/dist/cameleer" && echo "$name $eng $(stat -c %s /tmp/cameleer-$name-$eng)"
  done
done
