#!/usr/bin/env bash
# Per-2-second trace (threads, context switches, cpu) of one served scenario under load, to see what happens in a
# throughput dip. usage: vtrace.sh <name> <scenario>  -> $OUT/vtrace-<name>-<n>.txt (server side) and the load records
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"; OUT="$ROOT/target/cloud-bench"; STATE="$OUT/droplets.txt"; KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
B=$(field cameleer-bench-b 2); BP=$(field cameleer-bench-b 3); L=$(field cameleer-bench-load 2); LP=$(field cameleer-bench-load 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
name=$1; sc=$2; tag=${3:-1}
on "$B" "BENCH_KAFKA=$LP:9092 BENCH_PG=$LP VENV='' /root/v/vserve.sh $name $sc 8080 60" >/dev/null
on "$L" "/root/cameleer/target/release/cameleer-bench loadgen-remote $BP 8080 $sc 64 5 1 20" > "$OUT/vtrace-load-$name-$tag.txt" 2>/dev/null &
on "$B" 'sleep 6; pid=$(pgrep -x '$name'); echo "pid=$pid threads=$(ls /proc/$pid/task | wc -l)"; perf stat -I 1000 -p $pid -e context-switches,cpu-migrations,task-clock -- sleep 16 2>&1 | grep -E "context-switches|task-clock"' > "$OUT/vtrace-$name-$tag.txt" 2>&1
wait
grep RESULT "$OUT/vtrace-load-$name-$tag.txt" | cut -c1-200
on "$B" '/root/v/vserve.sh wait' >/dev/null 2>&1
