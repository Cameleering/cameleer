#!/usr/bin/env bash
B=$HOME/simoom-spike-target/release/cameleer-bench
sc=mcp-session; port=21999
env CAMEL_HTTP_ENGINE=simoom CAMEL_SIMOOM_INLINE=1 taskset -c 0-3 $B serve $sc $port 40 >/tmp/srv.$$ 2>&1 &
pid=$!
for _ in $(seq 50); do grep -q READY /tmp/srv.$$ && break; sleep 0.2; done
taskset -c 4-11 $B loadgen-remote 127.0.0.1 $port $sc 64 5 5 3 >/dev/null 2>&1 &
sleep 8
for i in 1 2 3 4 5; do
  for t in /proc/$pid/task/*; do echo "$(cat $t/comm) $(awk '{print $3}' $t/stat) $(cat $t/wchan 2>/dev/null)"; done
  sleep 0.5
done | sort | uniq -c | sort -rn | head -25
kill $pid; wait 2>/dev/null
