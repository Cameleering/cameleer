#!/usr/bin/env bash
# Context switches and a CPU profile of one served scenario under load. usage: vperf.sh <name> <scenario> [venv]
# (run on your machine after droplets.sh up and vsetup.sh; perf comes from apt on the server)
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"; OUT="$ROOT/target/cloud-bench"; STATE="$OUT/droplets.txt"; KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
B=$(field cameleer-bench-b 2); BP=$(field cameleer-bench-b 3); L=$(field cameleer-bench-load 2); LP=$(field cameleer-bench-load 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
name=$1; sc=$2; venv=${3:-}
on "$B" "BENCH_KAFKA=$LP:9092 BENCH_PG=$LP VENV='$venv' /root/v/vserve.sh $name $sc 8080 70" >/dev/null
on "$L" "/root/cameleer/target/release/cameleer-bench loadgen-remote $BP 8080 $sc 64 15 5 4" > "$OUT/vperf-load.txt" 2>/dev/null &
sleep 25
on "$B" 'pid=$(pgrep -x '$name'); echo "threads=$(ls /proc/$pid/task | wc -l)";
 perf stat -p $pid -e syscalls:sys_enter_clone3,syscalls:sys_enter_munmap,syscalls:sys_enter_madvise,syscalls:sys_enter_mmap,syscalls:sys_enter_futex,sched:sched_process_exit -- sleep 10 2>&1 | grep -E "syscalls|sched";
 true'
wait
grep RESULT "$OUT/vperf-load.txt" | cut -c1-200
on "$B" '/root/v/vserve.sh wait' >/dev/null 2>&1
