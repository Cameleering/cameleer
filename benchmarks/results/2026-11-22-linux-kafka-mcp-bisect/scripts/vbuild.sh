#!/usr/bin/env bash
# On a server droplet: build cameleer-bench for each named tarball in /root/v/tars into /root/v/bin/<name>,
# one shared target directory (registry crates are reused between refs). usage: vbuild.sh name...
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH CARGO_TARGET_DIR=/root/v/target
mkdir -p /root/v/bin
for n in "$@"; do
  rm -rf /root/v/src && mkdir -p /root/v/src && tar -xzf "/root/v/tars/$n.tgz" -C /root/v/src
  find /root/v/src -type f -exec touch {} +
  echo "building $n $(date -u +%H:%M:%S)" >&2
  (cd /root/v/src && cargo build -q --release -p cameleer-bench)
  cp /root/v/target/release/cameleer-bench "/root/v/bin/$n"
  ls -la "/root/v/bin/$n" >&2
done
