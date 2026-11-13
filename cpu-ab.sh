#!/usr/bin/env bash
# CPU per request and thread counts for one scenario: cpu-ab.sh <scenario>
B=$HOME/simoom-spike-target/release/cameleer-bench
sc=${1:-mcp-session}
ticks() { awk '{print $14+$15}' /proc/$1/stat; }
for eng in classic simoom simoom+inline; do
  port=$((20000 + RANDOM % 5000)); inl=; [ "${eng#*+}" = inline ] && inl=1
  env CAMEL_HTTP_ENGINE=${eng%%+*} ${inl:+CAMEL_SIMOOM_INLINE=1} taskset -c 0-3 $B serve $sc $port 40 >/tmp/srv.$$ 2>&1 &
  pid=$!
  for _ in $(seq 50); do grep -q READY /tmp/srv.$$ && break; sleep 0.2; done
  taskset -c 4-11 $B loadgen-remote 127.0.0.1 $port $sc 64 5 5 3 >/tmp/lg.$$ 2>&1 &
  lg=$!
  sleep 6; a=$(ticks $pid); sleep 14; b=$(ticks $pid); thr=$(ls /proc/$pid/task | wc -l)
  wait $lg; kill $pid 2>/dev/null; wait $pid 2>/dev/null
  msgs=$(grep -ao 'msgs=[0-9]*' /tmp/lg.$$ | cut -d= -f2); tput=$(grep -ao 'tput=[0-9]*' /tmp/lg.$$ | head -1)
  echo "$eng $sc $tput threads=$thr cpu_ticks_14s=$((b-a)) (100 ticks = 1 cpu-second; 4 cpus = 5600)"
done
