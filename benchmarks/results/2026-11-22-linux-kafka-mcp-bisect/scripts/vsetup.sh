#!/usr/bin/env bash
# After droplets.sh up: ship the tarballs (TARS=dir with <name>.tgz), prepare the load node from main.tgz and build
# every variant on both servers. usage: vsetup.sh <names...>   (main must be one of them)
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
OUT="$ROOT/target/cloud-bench"; STATE="$OUT/droplets.txt"; KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
A=$(field cameleer-bench-a 2); B=$(field cameleer-bench-b 2); L=$(field cameleer-bench-load 2); LP=$(field cameleer-bench-load 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
for h in "$A" "$B" "$L"; do
  on "$h" 'mkdir -p /root/v/tars'
  scp -q "${SSH_OPTS[@]}" "$ROOT/benchmarks/results/2026-11-22-linux-kafka-mcp-bisect/scripts/vserve.sh" "$ROOT/benchmarks/results/2026-11-22-linux-kafka-mcp-bisect/scripts/vbuild.sh" "root@$h:/root/v/"
  scp -q "${SSH_OPTS[@]}" "$TARS/main.tgz" "root@$h:/root/v/tars/"
  on "$h" 'chmod +x /root/v/*.sh; rm -rf /root/cameleer && mkdir -p /root/cameleer && tar -xzf /root/v/tars/main.tgz -C /root/cameleer && chmod +x /root/cameleer/scripts/*.sh /root/cameleer/scripts/cloud-bench/*.sh'
done
for h in "$A" "$B"; do for n in "$@"; do [ "$n" = main ] || scp -q "${SSH_OPTS[@]}" "$TARS/$n.tgz" "root@$h:/root/v/tars/"; done; done
on "$L" "/root/cameleer/scripts/cloud-bench/setup-node.sh load $LP" >"$OUT/vsetup-load.log" 2>&1 &
for h in "$A" "$B"; do
  on "$h" "/root/cameleer/scripts/cloud-bench/setup-node.sh base && nohup /root/v/vbuild.sh $* >/root/v/build.log 2>&1; echo \$? > /root/v/build.rc" >"$OUT/vsetup-$h.log" 2>&1 &
done
wait
echo "setup done"; for h in "$A" "$B"; do on "$h" 'cat /root/v/build.rc; ls /root/v/bin'; done
