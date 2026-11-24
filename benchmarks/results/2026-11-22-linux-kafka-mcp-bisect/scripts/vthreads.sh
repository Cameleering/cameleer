#!/usr/bin/env bash
# CPU time per thread (by name) of one served scenario over 10 s of load. usage: vthreads.sh <name> <scenario>
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"; OUT="$ROOT/target/cloud-bench"; STATE="$OUT/droplets.txt"; KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
B=$(field cameleer-bench-b 2); BP=$(field cameleer-bench-b 3); L=$(field cameleer-bench-load 2); LP=$(field cameleer-bench-load 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
name=$1; sc=$2
on "$B" "BENCH_KAFKA=$LP:9092 BENCH_PG=$LP VENV='' /root/v/vserve.sh $name $sc 8080 70" >/dev/null
on "$L" "/root/cameleer/target/release/cameleer-bench loadgen-remote $BP 8080 $sc 64 15 5 4" > "$OUT/vthreads-load.txt" 2>/dev/null &
sleep 25
on "$B" 'pid=$(pgrep -x '$name'); snap() { for t in /proc/$pid/task/*; do echo "$(cat $t/comm) $(awk "{print \$14+\$15}" $t/stat 2>/dev/null)"; done; }
 snap > /tmp/s1; sleep 10; snap > /tmp/s2
 paste /tmp/s1 /tmp/s2 | awk "{d=\$4-\$2; n=\$1; sub(/-[0-9]+\$/,\"\",n); c[n]+=d; k[n]++} END {for (n in c) printf \"%-24s threads=%-3d cpu_ticks=%d\n\", n, k[n], c[n]}" | sort -k3 -t= -n -r | head -12'
wait
grep RESULT "$OUT/vthreads-load.txt" | cut -c1-160
on "$B" '/root/v/vserve.sh wait' >/dev/null 2>&1
