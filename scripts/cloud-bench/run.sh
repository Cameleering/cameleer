#!/usr/bin/env bash
# Cloud benchmark orchestrator: runs on your machine and drives the droplets made by
# droplets.sh over SSH. Results go to target/cloud-bench/.
#
#   scripts/cloud-bench/run.sh setup      copy the source to the droplets and prepare them
#   scripts/cloud-bench/run.sh prefill    fill the Kafka input topic of the stream scenario
#   scripts/cloud-bench/run.sh bench      run every scenario, both sides, on both servers
#   scripts/cloud-bench/run.sh test       run the hermetic test suite on a server droplet
#   scripts/cloud-bench/run.sh all        setup, prefill, bench
#
# Layout: two identical servers (a, b) and a load node with Kafka, PostgreSQL and the load
# generator. Phase 1 runs Apache Camel (JVM and Quarkus native) on a and Cameleer on b, phase 2
# swaps them, so a difference between the two machines cancels out. Only one side runs at a time.
# BENCH_SIDES limits the sides (default: java rust native).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$ROOT/target/cloud-bench"
STATE="$OUT/droplets.txt"
KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
WARMUP="${BENCH_WARMUP:-15}"; ITER="${BENCH_ITER:-5}"; ITERS="${BENCH_ITERS:-5}"
CONNS="${BENCH_CONNS:-64}"
SIDES="${BENCH_SIDES:-java rust native}"
STREAM_RECORDS="${BENCH_STREAM_RECORDS:-30000000}"
SERVED="${BENCH_SERVED:-http rest-json http-kafka http-sql}"
PORT=8080

[ -f "$STATE" ] || { echo "no $STATE: run droplets.sh up first" >&2; exit 1; }
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
A_PUB=$(field cameleer-bench-a 2); A_PRIV=$(field cameleer-bench-a 3)
B_PUB=$(field cameleer-bench-b 2); B_PRIV=$(field cameleer-bench-b 3)
L_PUB=$(field cameleer-bench-load 2); L_PRIV=$(field cameleer-bench-load 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }

setup() {
  mkdir -p "$OUT"
  log "packing the source at $(git -C "$ROOT" rev-parse --short HEAD)"
  git -C "$ROOT" archive --format=tar.gz -o "$OUT/src.tar.gz" HEAD
  for h in "$A_PUB" "$B_PUB" "$L_PUB"; do
    scp -q "${SSH_OPTS[@]}" "$OUT/src.tar.gz" "root@$h:/root/src.tar.gz"
    on "$h" 'rm -rf /root/cameleer && mkdir -p /root/cameleer && tar -xzf /root/src.tar.gz -C /root/cameleer && chmod +x /root/cameleer/scripts/*.sh /root/cameleer/scripts/cloud-bench/*.sh'
  done
  log "preparing the nodes in parallel (logs in $OUT/setup-*.log)"
  on "$A_PUB" /root/cameleer/scripts/cloud-bench/setup-node.sh server >"$OUT/setup-a.log" 2>&1 &
  on "$B_PUB" /root/cameleer/scripts/cloud-bench/setup-node.sh server >"$OUT/setup-b.log" 2>&1 &
  on "$L_PUB" /root/cameleer/scripts/cloud-bench/setup-node.sh load "$L_PRIV" >"$OUT/setup-load.log" 2>&1 &
  local failed=0
  for j in $(jobs -p); do wait "$j" || failed=1; done
  for f in "$OUT"/setup-*.log; do tail -n 3 "$f"; done
  [ "$failed" = 0 ] || { echo "setup failed, see $OUT/setup-*.log" >&2; exit 1; }
}

prefill() {
  log "filling stream-in with $STREAM_RECORDS records"
  on "$L_PUB" "STREAM_RECORDS=$STREAM_RECORDS bash -s" <<'EOF'
set -euo pipefail
export JAVA_HOME="$(ls -d /root/.cache/cameleer-bench/jdk-*)"
python3 - <<'PY' > /root/stream-payloads.txt
for i in range(100000):
    print('{"orderId":%d,"customer":"ACME","amount":%d.50}' % (i, 5 + (i % 10) * 20))
PY
/opt/kafka/bin/kafka-producer-perf-test.sh --topic stream-in --num-records "$STREAM_RECORDS" --throughput -1 \
  --payload-file /root/stream-payloads.txt --producer-props bootstrap.servers=127.0.0.1:9092 acks=1 linger.ms=20 batch.size=524288 \
  | tail -1
EOF
}

machine() {
  local h=$1
  on "$h" 'echo "CPU: $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2 | sed "s/^ *//")"; echo "vCPUs: $(nproc)"; echo "RAM: $(awk "/MemTotal/ {printf \"%.1f GiB\", \$2/1048576}" /proc/meminfo)"; echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME") $(uname -r)"; echo "Rust: $(/root/.cargo/bin/rustc -V)"; echo "JDK: $($(ls -d /root/.cache/cameleer-bench/jdk-*)/bin/java -version 2>&1 | head -1)"'
}

one_side() { # one_side <phase> <side> <node-pub> <node-priv> <node-name>
  local phase=$1 side=$2 pub=$3 priv=$4 node=$5 results="$OUT/results.txt"
  case " $SIDES " in *" $side "*) ;; *) return 0 ;; esac
  local env="BENCH_KAFKA=$L_PRIV:9092 BENCH_PG=$L_PRIV BENCH_RUN=$phase$side$(date +%s)"
  local tag="phase=$phase node=$node"
  # a failing step is recorded as a FAILED line and the run goes on with the next one
  log "phase $phase: $side on $node, in-process scenarios"
  { on "$pub" "$env /root/cameleer/scripts/cloud-bench/bench-node.sh inproc $side $WARMUP $ITER $ITERS" 2>>"$OUT/run.log" \
    || echo "FAILED side=$side scenario=inproc"; } | sed "s/^\([A-Z]*\) /\1 $tag /" | tee -a "$results"
  log "phase $phase: $side on $node, kafka-stream"
  { on "$pub" "$env /root/cameleer/scripts/cloud-bench/bench-node.sh stream $side $WARMUP $ITER $ITERS" 2>>"$OUT/run.log" \
    || echo "FAILED side=$side scenario=kafka-stream"; } | sed "s/^\([A-Z]*\) /\1 $tag /" | tee -a "$results"
  local secs=$((WARMUP + ITER * ITERS + 10))
  for sc in $SERVED; do
    log "phase $phase: $side on $node, $sc over the network ($CONNS connections)"
    [ "$sc" = http-sql ] && on "$L_PUB" "PGPASSWORD=camelpw psql -q -h 127.0.0.1 -U camel -d bench -c 'TRUNCATE orders'"
    { on "$pub" "$env /root/cameleer/scripts/cloud-bench/bench-node.sh serve $side $sc $PORT $secs" 2>>"$OUT/run.log" \
      || echo "FAILED side=$side"; } | sed "s/^\([A-Z]*\) /\1 $tag scenario=$sc /" | tee -a "$results"
    { on "$L_PUB" "/root/cameleer/target/release/cameleer-bench loadgen-remote $priv $PORT $sc $CONNS $WARMUP $ITER $ITERS" 2>>"$OUT/run.log" \
      || echo "FAILED scenario=$sc"; } | sed -e "s/^RESULT /RESULT $tag side=$side /" -e "s/^FAILED /FAILED $tag side=$side /" | tee -a "$results"
    { on "$pub" "/root/cameleer/scripts/cloud-bench/bench-node.sh served-wait" || true; } | sed "s/^PROC /PROC $tag /" | tee -a "$results"
    if [ "$sc" = http-sql ]; then
      on "$L_PUB" "PGPASSWORD=camelpw psql -At -h 127.0.0.1 -U camel -d bench -c \"SELECT 'ROWS $tag side=$side scenario=http-sql rows=' || count(*) FROM orders\"" | tee -a "$results"
    fi
  done
}

bench() {
  mkdir -p "$OUT"
  { echo "== server a"; machine "$A_PUB"; echo "== server b"; machine "$B_PUB"; echo "== load"; machine "$L_PUB"
    echo "Commit: $(git -C "$ROOT" rev-parse --short HEAD)"; echo "Date: $(date -u '+%Y-%m-%d %H:%M UTC')"
    echo "Plan: warmup $WARMUP s, $ITERS x $ITER s; $CONNS HTTP connections; $STREAM_RECORDS stream records"; } > "$OUT/machine.txt"
  : > "$OUT/results.txt"; : > "$OUT/run.log"
  one_side 1 java "$A_PUB" "$A_PRIV" a
  one_side 1 rust "$B_PUB" "$B_PRIV" b
  one_side 1 native "$A_PUB" "$A_PRIV" a
  one_side 2 rust "$A_PUB" "$A_PRIV" a
  one_side 2 java "$B_PUB" "$B_PRIV" b
  one_side 2 native "$B_PUB" "$B_PRIV" b
  log "results in $OUT/results.txt"
}

suite() {
  log "hermetic test suite on server a"
  on "$A_PUB" 'cd /root/cameleer && export PATH=/root/.cargo/bin:$PATH && cargo build --workspace 2>&1 | grep -cE "^warning" ; cargo test --workspace 2>&1 | tail -n +1' >"$OUT/test-linux.log" 2>&1 || true
  awk '/^test result/ {p+=$4; f+=$6} END {print "linux suite: passed", p, "failed", f}' "$OUT/test-linux.log"
  grep -E "^test .* FAILED|panicked" "$OUT/test-linux.log" | head -20 || true
}

case "${1:-all}" in
  setup) setup ;;
  prefill) prefill ;;
  bench) bench ;;
  test) suite ;;
  all) setup; prefill; bench ;;
  *) echo "usage: $0 setup|prefill|bench|test|all" >&2; exit 1 ;;
esac
