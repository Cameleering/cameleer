#!/usr/bin/env bash
# Runs on a server droplet of the cloud benchmark, called by run.sh over SSH. Starts one side
# (rust = Cameleer, java = Apache Camel) the same way for both and prints the records of
# docs/BENCHMARKS.md (RESULT, PROC, VERSION), tagged with the side.
#
#   bench-node.sh inproc <side> <warmup> <iter> <iters>      in-process scenarios and startup
#   bench-node.sh stream <side> <warmup> <iter> <iters>      Kafka stream processing
#   bench-node.sh serve <side> <scenario> <port> <secs>      start a served scenario, return at READY
#   bench-node.sh served-wait                                wait for it to exit, print its PROC record
#
# BENCH_KAFKA, BENCH_PG and BENCH_RUN are passed through from run.sh.
set -euo pipefail

SRC=/root/cameleer
BENCH="$SRC/target/release/cameleer-bench"
JAVA="$(ls -d /root/.cache/cameleer-bench/jdk-*)/bin/java"
JDIR="$SRC/benchmarks/java-camel"
ulimit -n 65536
# the commands below are split into words unquoted; `lib/*` must reach java as one classpath entry
set -f

cmd_for() { # the benchmark command of a side
  case "$1" in
    rust) echo "$BENCH" ;;
    java) echo "$JAVA -cp $JDIR/target/classes:$JDIR/target/lib/* bench.Bench" ;;
    *) echo "unknown side $1" >&2; exit 1 ;;
  esac
}

case "$1" in
  inproc)
    side=$2; warm=$3; iter=$4; iters=$5
    # shellcheck disable=SC2046
    for s in direct seda seda-inout cbr splitagg csv; do
      "$BENCH" measure --side "$side" --scenario "$s" -- $(cmd_for "$side") run "$s" "$warm" "$iter" "$iters"
    done
    "$BENCH" measure --side "$side" --scenario startup --repeat 5 -- $(cmd_for "$side") startup
    ;;
  stream)
    side=$2
    export BENCH_SIDE="$side"
    "$BENCH" measure --side "$side" --scenario kafka-stream -- $(cmd_for "$side") run kafka-stream "$3" "$4" "$5"
    ;;
  serve)
    side=$2; scenario=$3; port=$4; secs=$5
    export BENCH_SIDE="$side"
    rm -f /root/served.out /root/served.time
    # /usr/bin/time reports the peak RSS and the CPU share of the whole run, the same for both sides
    # shellcheck disable=SC2046
    nohup /usr/bin/time -o /root/served.time -f "PROC side=$side scenario=$scenario peak_rss_kb=%M cpu_pct=%P user_s=%U sys_s=%S" \
      $(cmd_for "$side") serve "$scenario" "$port" "$secs" >/root/served.out 2>/root/served.err &
    echo $! > /root/served.pid
    for _ in $(seq 120); do
      grep -q '^READY' /root/served.out 2>/dev/null && { grep '^VERSION' /root/served.out | sed "s/^VERSION /VERSION side=$side /"; exit 0; }
      kill -0 "$(cat /root/served.pid)" 2>/dev/null || { cat /root/served.err >&2; exit 1; }
      sleep 0.5
    done
    echo "server did not get ready" >&2; exit 1
    ;;
  served-wait)
    while kill -0 "$(cat /root/served.pid)" 2>/dev/null; do sleep 0.5; done
    cat /root/served.time
    ;;
  *) echo "usage: see the header of $0" >&2; exit 1 ;;
esac
