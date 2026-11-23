#!/usr/bin/env bash
# Soak test: one served scenario under constant load for a long time, to see what the short runs of run.sh cannot:
# memory growth, thread and descriptor leaks, and throughput and p99 drifting over time. Runs from your machine against the
# droplets of droplets.sh up / run.sh setup, like run.sh.
#
#   scripts/cloud-bench/soak.sh <scenario> <side> [server a|b]
#
# scenario: http-kafka or mcp-session (any served scenario works); side: rust, java, native or node (mcp-session only).
# Environment: SOAK_MINUTES (default 60), SOAK_EPOCH seconds per load epoch (default 300), SOAK_SAMPLE seconds between
# samples of the server process (default 30), BENCH_CONNS (default 64), BENCH_WARMUP (default 30).
#
# Output in target/cloud-bench/soak-<scenario>-<side>.txt: one RESULT line per epoch (throughput and latency of that
# epoch), SAMPLE lines (RSS, threads, descriptors, CPU ticks of the server process over time), the PROC record, and a
# SUMMARY line comparing the first and the last epoch. A server that dies, or an epoch that fails, is recorded as FAILED.
# http-kafka writes to the topic orders-<side>: its retention is capped at 2 GB first so the load node's disk does not fill.
set -euo pipefail

SC=${1:?scenario}; SIDE=${2:?side}; SRV=${3:-a}
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$ROOT/target/cloud-bench"; STATE="$OUT/droplets.txt"
KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
MIN="${SOAK_MINUTES:-60}"; EPOCH="${SOAK_EPOCH:-300}"; EVERY="${SOAK_SAMPLE:-30}"
CONNS="${BENCH_CONNS:-64}"; WARM="${BENCH_WARMUP:-30}"; PORT=8080
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
L_PUB=$(field cameleer-bench-load 2); L_PRIV=$(field cameleer-bench-load 3)
S_PUB=$(field cameleer-bench-$SRV 2); S_PRIV=$(field cameleer-bench-$SRV 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
RES="$OUT/soak-$SC-$SIDE.txt"; : > "$RES"
EPOCHS=$(( (MIN * 60 + EPOCH - 1) / EPOCH ))
TOTAL=$(( WARM + EPOCHS * EPOCH + 60 ))

if [ "$SC" = http-kafka ]; then
  log "capping the retention of orders-$SIDE at 2 GB"
  on "$L_PUB" "J=\$(ls -d /root/.cache/cameleer-bench/jdk-* | head -1); JAVA_HOME=\$J /opt/kafka/bin/kafka-configs.sh --bootstrap-server 127.0.0.1:9092 --alter --entity-type topics --entity-name orders-$SIDE --add-config retention.bytes=500000000,segment.bytes=134217728 >/dev/null" || echo "WARNING: retention not set" >&2
fi
log "soak $SC on $SIDE (server $SRV): $EPOCHS epochs of ${EPOCH}s, about $((TOTAL / 60)) minutes"
env="BENCH_KAFKA=$L_PRIV:9092 BENCH_PG=$L_PRIV BENCH_RUN=soak$(date +%s)"
on "$S_PUB" "$env /root/cameleer/scripts/cloud-bench/bench-node.sh serve $SIDE $SC $PORT $TOTAL" 2>>"$OUT/run.log" | tee -a "$RES"
on "$S_PUB" "/root/cameleer/scripts/cloud-bench/bench-node.sh sample $EVERY $TOTAL" 2>>"$OUT/run.log" >> "$RES" &
SAMPLER=$!
for e in $(seq 1 "$EPOCHS"); do
  warm=0; [ "$e" = 1 ] && warm=$WARM
  log "epoch $e/$EPOCHS"
  { on "$L_PUB" "/root/cameleer/target/release/cameleer-bench loadgen-remote $S_PRIV $PORT $SC $CONNS $warm $EPOCH 1" 2>>"$OUT/run.log" \
    || echo "FAILED scenario=$SC"; } | sed -e "s/^RESULT /RESULT epoch=$e side=$SIDE /" -e "s/^FAILED /FAILED epoch=$e side=$SIDE /" | tee -a "$RES"
done
kill $SAMPLER 2>/dev/null || true
on "$S_PUB" "pkill -f 'cameleer-bench serve' || true; /root/cameleer/scripts/cloud-bench/bench-node.sh served-wait || true" 2>>"$OUT/run.log" | tee -a "$RES"
python3 - "$RES" <<'PY' | tee -a "$RES"
import re,sys
rs=[l for l in open(sys.argv[1]) if l.startswith('RESULT epoch=')]
sm=[l for l in open(sys.argv[1]) if l.startswith('SAMPLE')]
g=lambda l,k: float(re.search(k+r'=([\d.]+)',l).group(1))
if rs:
    a,b=rs[0],rs[-1]
    print(f"SUMMARY epochs={len(rs)} tput_first={g(a,'tput'):.0f} tput_last={g(b,'tput'):.0f} p99_first_us={g(a,'p99_us'):.0f} p99_last_us={g(b,'p99_us'):.0f}",end='')
    if sm:
        # growth after the first 10 minutes of samples, so the warm-up is not counted
        s=[l for l in sm if g(l,'t')>=600] or sm
        print(f" rss_kb_start={g(s[0],'rss_kb'):.0f} rss_kb_end={g(s[-1],'rss_kb'):.0f} rss_kb_max={max(g(l,'rss_kb') for l in sm):.0f} threads_end={g(sm[-1],'threads'):.0f} fds_end={g(sm[-1],'fds'):.0f}")
    else: print()
PY
