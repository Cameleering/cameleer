#!/bin/bash
# proxy.sh <async|sync> <fast|sleepN> <conns> <secs>: loadgen (CPUs 6-11) -> cameleer-bench serve http-proxy
# (CPUs 0-3) -> raw backend (CPUs 4-5). Prints throughput, latency, and the Cameleer process's CPU and
# context switches per request over the middle of the measured window.
MODE=$1; BACK=$2; CONNS=$3; SECS=$4
BIN=$HOME/async-target/release/cameleer-bench
BP=18081; PP=18080
pkill -f "cameleer-bench (raw-http|serve)" 2>/dev/null; sleep 0.3
if [ "$BACK" = fast ]; then taskset -c 4-5 $BIN raw-http $BP poll & else taskset -c 4-5 $BIN raw-http $BP $BACK & fi
BPID=$!
ASYNC=true; [ "$MODE" = sync ] && ASYNC=false
CAMEL_ASYNC_ROUTING=$ASYNC BENCH_PROXY_TARGET=127.0.0.1:$BP taskset -c 0-3 $BIN serve http-proxy $PP $((SECS + 20)) > /tmp/proxy-serve.out 2>&1 &
SPID=$!
for i in $(seq 100); do grep -q READY /tmp/proxy-serve.out && break; sleep 0.1; done
TCK=$(getconf CLK_TCK)
cpu() { awk '{print $14+$15}' /proc/$SPID/stat; }
ctx() { cat /proc/$SPID/task/*/status 2>/dev/null | awk '/ctxt_switches/ {s+=$2} END {print s}'; }
thr() { ls /proc/$SPID/task | wc -l; }
taskset -c 6-11 $BIN loadgen-remote 127.0.0.1 $PP http-proxy $CONNS 2 $SECS 1 > /tmp/proxy-load.out 2>&1 &
LPID=$!
sleep 2.5
C0=$(cpu); X0=$(ctx); T0=$(date +%s.%N)
sleep $(echo "$SECS - 1" | bc)
C1=$(cpu); X1=$(ctx); T1=$(date +%s.%N); TH=$(thr)
wait $LPID
LINE=$(grep RESULT /tmp/proxy-load.out)
TPUT=$(echo "$LINE" | sed 's/.*tput=\([0-9]*\).*/\1/')
P50=$(echo "$LINE" | sed 's/.*p50_us=\([0-9.]*\).*/\1/'); P99=$(echo "$LINE" | sed 's/.*p99_us=\([0-9.]*\).*/\1/')
ERR=$(grep -o "errors=[0-9]*" /tmp/proxy-load.out | head -1)
kill $SPID $BPID 2>/dev/null; wait 2>/dev/null
awk -v m=$MODE -v b=$BACK -v n=$CONNS -v t=$TPUT -v c0=$C0 -v c1=$C1 -v x0=$X0 -v x1=$X1 -v t0=$T0 -v t1=$T1 -v tck=$TCK -v p50=$P50 -v p99=$P99 -v th=$TH -v e="$ERR" 'BEGIN {
  dt = t1 - t0; req = t * dt;
  printf "mode=%s backend=%s conns=%d tput=%d p50_us=%s p99_us=%s cpu_us_per_req=%.1f ctx_per_req=%.2f threads=%d cpus_busy=%.2f %s\n", m, b, n, t, p50, p99, (c1-c0)/tck*1e6/req, (x1-x0)/req, th, (c1-c0)/tck/dt, e }'
