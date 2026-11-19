#!/usr/bin/env bash
# Independent load generator for the HTTP/2 question (docs/design/http2-throughput.md): the same
# Cameleer server driven by nghttp2's h2load (HTTP/1.1 and HTTP/2) and by cameleer-bench's own load
# generator, so a limit of the server can be told apart from a limit of cameleer-bench's client.
# Runs on your machine after `droplets.sh up` and `run.sh setup`; results in target/cloud-bench/peer.txt.
#
#   scripts/cloud-bench/peer.sh
#
# PEER_SECS (default 25) timed seconds per run after PEER_WARMUP (default 10) seconds of warm-up.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$ROOT/target/cloud-bench"
STATE="$OUT/droplets.txt"
KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
SECS="${PEER_SECS:-25}"; WARMUP="${PEER_WARMUP:-10}"
PORT=8080
[ -f "$STATE" ] || { echo "no $STATE: run droplets.sh up first" >&2; exit 1; }
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
L_PUB=$(field cameleer-bench-load 2)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
RES="$OUT/peer.txt"

log "load node: h2load (Ubuntu's nghttp2-client) and the request bodies"
on "$L_PUB" 'command -v h2load >/dev/null || (apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq nghttp2-client >/dev/null); h2load --version | head -1
python3 - <<"PY"
items = ",".join("{\"sku\":\"SKU-%d\",\"qty\":%d,\"price\":%d.99}" % (i, i + 1, i + 3) for i in range(10))
open("/root/order.json", "w").write("{\"orderId\":1000000001,\"customer\":\"ACME\",\"items\":[%s]}" % items)
# a gRPC message: no compression, length, then bench.Req {name: "bench", id: 1}
msg = bytes([0x0a, 5]) + b"bench" + bytes([0x10, 1])
open("/root/grpc.bin", "wb").write(bytes([0]) + len(msg).to_bytes(4, "big") + msg)
PY' | tee -a "$RES"

# h2load <label> <args...>: one timed run; prints the throughput, failures and latency lines
h2run() {
  local label=$1 priv=$2; shift 2
  on "$L_PUB" "h2load -t 4 -D $SECS --warm-up-time $WARMUP $* 2>&1" \
    | awk -v l="$label" '/^finished in/ {t=$0} /^requests:/ {r=$0} /^status codes:/ {s=$0} /^time for request:/ {q=$0}
        END {print "H2LOAD " l " | " t " | " r " | " s " | " q}' | tee -a "$RES"
}

one_server() { # one_server <name> <pub> <priv>
  local node=$1 pub=$2 priv=$3 secs=$((WARMUP + SECS + 15))
  local url="http://$priv:$PORT"
  run_case() { # run_case <scenario> <label> <command on the load node>
    local sc=$1 label=$2; shift 2
    on "$pub" "/root/cameleer/scripts/cloud-bench/bench-node.sh serve rust $sc $PORT $secs" >/dev/null
    "$@"
    on "$pub" "/root/cameleer/scripts/cloud-bench/bench-node.sh served-wait" | sed "s/^PROC /PROC node=$node case=$label /" | tee -a "$RES"
  }
  own() { on "$L_PUB" "/root/cameleer/target/release/cameleer-bench loadgen-remote $priv $PORT $1 64 $WARMUP 5 $((SECS / 5))" | sed "s/^RESULT /OWN node=$node /" | tee -a "$RES"; }
  local post="-d /root/order.json -H 'content-type: application/json'"
  local grpc="-d /root/grpc.bin -H 'content-type: application/grpc' -H 'te: trailers'"
  log "$node: h2load HTTP/1.1, 64 connections"
  run_case rest-json h1-c64 h2run "node=$node case=h1-c64" "$priv" --h1 -c 64 -m 1 $post "$url/orders"
  log "$node: h2load HTTP/2, 4 connections x 16 streams"
  run_case rest-json h2-c4m16 h2run "node=$node case=h2-c4m16" "$priv" -c 4 -m 16 $post "$url/orders"
  log "$node: h2load HTTP/2, 16 connections x 4 streams"
  run_case rest-json h2-c16m4 h2run "node=$node case=h2-c16m4" "$priv" -c 16 -m 4 $post "$url/orders"
  log "$node: h2load gRPC unary, 4 connections x 16 streams"
  run_case grpc grpc-c4m16 h2run "node=$node case=grpc-c4m16" "$priv" -c 4 -m 16 $grpc "$url/bench.Echo/Say"
  log "$node: cameleer-bench's generator: rest-json, rest-json-h2, grpc"
  run_case rest-json own-h1 own rest-json
  run_case rest-json-h2 own-h2 own rest-json-h2
  run_case grpc own-grpc own grpc
}

: > "$RES"
one_server a "$(field cameleer-bench-a 2)" "$(field cameleer-bench-a 3)"
one_server b "$(field cameleer-bench-b 2)" "$(field cameleer-bench-b 3)"
log "results in $RES"
