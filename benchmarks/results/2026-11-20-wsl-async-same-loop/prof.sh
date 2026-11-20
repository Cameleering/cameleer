#!/bin/bash
# prof.sh <secs>: perf record the Cameleer process of the proxy scenario (fast backend, 64 connections)
P=/usr/lib/linux-tools/6.8.0-149-generic/perf
BIN=$HOME/async-target-prof/release/cameleer-bench
BP=18081; PP=18080
pkill -f "cameleer-bench (raw-http|serve)" 2>/dev/null; sleep 0.3
taskset -c 4-5 $BIN raw-http $BP poll & BPID=$!
BENCH_PROXY_TARGET=127.0.0.1:$BP taskset -c 0-3 $BIN serve http-proxy $PP 60 > /tmp/prof-serve.out 2>&1 & SPID=$!
for i in $(seq 100); do grep -q READY /tmp/prof-serve.out && break; sleep 0.1; done
taskset -c 6-11 $BIN loadgen-remote 127.0.0.1 $PP http-proxy 64 2 $(( $1 + 4 )) 1 > /tmp/prof-load.out 2>&1 & LPID=$!
sleep 3
$P record -F 2999 -g -p $SPID -o /tmp/prof.data -- sleep $1 2>&1 | tail -1
$P stat -e task-clock,context-switches,cycles,instructions,cycles:k,instructions:k -p $SPID -- sleep 2 2>&1 | grep -E "task-clock|context|cycles|instr"
wait $LPID; grep RESULT /tmp/prof-load.out
kill $SPID $BPID 2>/dev/null; wait 2>/dev/null
