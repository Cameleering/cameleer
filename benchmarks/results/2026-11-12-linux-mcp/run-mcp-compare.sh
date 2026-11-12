#!/usr/bin/env bash
# The MCP session comparison in one session, the layout of scripts/cloud-bench: two identical
# servers (a, b) and a load node. Phase 1: Camel on a, Cameleer on b, TypeScript SDK on a;
# phase 2: swapped. Only the MCP scenarios; no Kafka, PostgreSQL or in-process scenarios.
# Always deletes the droplets it made (trap).
set -uo pipefail
W="$(cd "$(dirname "$0")" && pwd)"
REPO="$(git rev-parse --show-toplevel)"
: "${DO_TOKEN_FILE:?set DO_TOKEN_FILE to the file holding a DigitalOcean API token}"; export DO_TOKEN_FILE
export BENCH_STATE="$W/droplets.txt"
KEY="$HOME/.ssh/id_ed25519"
SSH=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$W/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*"; }
down() { log "deleting the droplets"; bash "$REPO/scripts/cloud-bench/droplets.sh" down; }
trap down EXIT

cd "$REPO"
git fetch -q origin
SRC_REF="${SRC_REF:-origin/main}"
log "source: $(git rev-parse --short $SRC_REF)"
git show $SRC_REF:scripts/cloud-bench/mcp-ts/package.json | grep '"zod"'
git archive --format=tar.gz -o "$W/src.tar.gz" $SRC_REF
# the server setup without the Quarkus native build (not part of this comparison)
python "$W/trim.py" "$SRC_REF" "$W/setup-trim.sh"
grep -c Mandrel "$W/setup-trim.sh" | sed 's/^/mandrel lines left (consts only expected): /'

TOKEN="$(grep -o 'dop_v1_[0-9a-f]\{64\}' "$DO_TOKEN_FILE" | head -1)"
FP="$(ssh-keygen -lE md5 -f "$KEY.pub" | awk '{print $2}' | sed 's/^MD5://')"
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" https://api.digitalocean.com/v2/droplets \
  -d "{\"names\":[\"cameleer-bench-a\",\"cameleer-bench-b\",\"cameleer-bench-load\"],\"region\":\"blr1\",\"size\":\"c-4\",\"image\":\"ubuntu-24-04-x64\",\"ssh_keys\":[\"$FP\"],\"tags\":[\"cameleer-bench\"],\"monitoring\":false,\"ipv6\":false}" \
  | python -c "import json,sys; r=json.load(sys.stdin); print('created:', [d['name'] for d in r['droplets']]) if 'droplets' in r else sys.exit('create failed: '+r.get('message',''))" || exit 1
for _ in $(seq 80); do
  n=$(curl -sS -H "Authorization: Bearer $TOKEN" "https://api.digitalocean.com/v2/droplets?tag_name=cameleer-bench" | python -c "
import json,sys
d=json.load(sys.stdin)['droplets']
print(len([x for x in d if x['status']=='active' and any(n['type']=='private' for n in x['networks']['v4'])]))")
  [ "$n" = 3 ] && break; sleep 5
done
curl -sS -H "Authorization: Bearer $TOKEN" "https://api.digitalocean.com/v2/droplets?tag_name=cameleer-bench" | python -c "
import json,sys
for d in json.load(sys.stdin)['droplets']:
    ip=lambda t: next((n['ip_address'] for n in d['networks']['v4'] if n['type']==t), '-')
    print(d['name'], ip('public'), ip('private'), d['status'])" | tee "$BENCH_STATE"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$BENCH_STATE"; }
A_PUB=$(field cameleer-bench-a 2); A_PRIV=$(field cameleer-bench-a 3)
B_PUB=$(field cameleer-bench-b 2); B_PRIV=$(field cameleer-bench-b 3)
L_PUB=$(field cameleer-bench-load 2)
on() { local h=$1; shift; ssh "${SSH[@]}" "root@$h" "$@"; }
for h in "$A_PUB" "$B_PUB" "$L_PUB"; do for i in $(seq 24); do on "$h" true 2>/dev/null && break; sleep 10; done; done

log "shipping the source"
for h in "$A_PUB" "$B_PUB" "$L_PUB"; do
  scp -q "${SSH[@]}" "$W/src.tar.gz" "root@$h:/root/src.tar.gz"
  on "$h" 'rm -rf /root/cameleer && mkdir -p /root/cameleer && tar -xzf /root/src.tar.gz -C /root/cameleer && chmod +x /root/cameleer/scripts/*.sh /root/cameleer/scripts/cloud-bench/*.sh'
done
scp -q "${SSH[@]}" "$W/setup-trim.sh" "root@$A_PUB:/root/cameleer/scripts/cloud-bench/setup-node.sh"
scp -q "${SSH[@]}" "$W/setup-trim.sh" "root@$B_PUB:/root/cameleer/scripts/cloud-bench/setup-node.sh"
scp -q "${SSH[@]}" "$W/remote-setup.sh" "root@$L_PUB:/root/remote-setup.sh"
log "setting up the nodes in parallel (logs in $W/setup-*.log)"
on "$A_PUB" /root/cameleer/scripts/cloud-bench/setup-node.sh server > "$W/setup-a.log" 2>&1 & p1=$!
on "$B_PUB" /root/cameleer/scripts/cloud-bench/setup-node.sh server > "$W/setup-b.log" 2>&1 & p2=$!
on "$L_PUB" 'bash /root/remote-setup.sh load' > "$W/setup-load.log" 2>&1 & p3=$!
wait $p1 || { tail -30 "$W/setup-a.log"; exit 1; }
wait $p2 || { tail -30 "$W/setup-b.log"; exit 1; }
wait $p3 || { tail -30 "$W/setup-load.log"; exit 1; }
tail -n 2 "$W/setup-a.log" "$W/setup-b.log" "$W/setup-load.log"
# bench-node.sh looks for the Quarkus runner with `ls ... | head -1` under pipefail, which stops the
# script when there is none; this comparison skips the native build, so give it a placeholder.
for h in "$A_PUB" "$B_PUB"; do on "$h" 'mkdir -p /root/cameleer/benchmarks/camel-quarkus/target && touch /root/cameleer/benchmarks/camel-quarkus/target/placeholder-runner'; done

machine() { on "$1" 'echo "CPU: $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2 | sed "s/^ *//")"; echo "vCPUs: $(nproc)"; echo "RAM: $(awk "/MemTotal/ {printf \"%.1f GiB\", \$2/1048576}" /proc/meminfo)"; echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME") $(uname -r)"; echo "Rust: $(/root/.cargo/bin/rustc -V)"; echo "JDK: $($(ls -d /root/.cache/cameleer-bench/jdk-*)/bin/java -version 2>&1 | head -1)"; echo "Node: $(/opt/node/bin/node -v)"; echo "zod: $(/opt/node/bin/node -p "require(\"/root/cameleer/scripts/cloud-bench/mcp-ts/node_modules/zod/package.json\").version")"'; }
{ echo "== server a"; machine "$A_PUB"; echo "== server b"; machine "$B_PUB"; echo "== load"; on "$L_PUB" 'echo "vCPUs: $(nproc)"'
  echo "Commit: $(git rev-parse --short $SRC_REF)"; echo "Date: $(date -u '+%Y-%m-%d %H:%M UTC')"
  echo "Plan: warmup 15 s, 5 x 5 s; 64 HTTP connections; MCP scenarios only"; } > "$W/machine.txt"

WARM=15; ITER=5; ITERS=5; CONNS=64; PORT=8080
SECS=$((WARM + ITER * ITERS + 10))
: > "$W/results.txt"
one() { # one <phase> <side> <scenario> <server-pub> <server-priv> <node-name>
  local phase=$1 side=$2 sc=$3 pub=$4 priv=$5 node=$6
  local tag="phase=$phase node=$node"
  log "phase $phase: $side on $node, $sc ($CONNS connections)"
  { on "$pub" "BENCH_KAFKA=- BENCH_PG=- BENCH_RUN=$phase$side$(date +%s) /root/cameleer/scripts/cloud-bench/bench-node.sh serve $side $sc $PORT $SECS" 2>>"$W/run.log" \
    || echo "FAILED side=$side"; } | sed "s/^\([A-Z]*\) /\1 $tag scenario=$sc /" | tee -a "$W/results.txt"
  { on "$L_PUB" "/root/cameleer/target/release/cameleer-bench loadgen-remote $priv $PORT $sc $CONNS $WARM $ITER $ITERS" 2>>"$W/run.log" \
    || echo "FAILED scenario=$sc"; } | sed -e "s/^RESULT /RESULT $tag side=$side /" -e "s/^FAILED /FAILED $tag side=$side /" | tee -a "$W/results.txt"
  { on "$pub" "/root/cameleer/scripts/cloud-bench/bench-node.sh served-wait" || true; } | sed "s/^PROC /PROC $tag /" | tee -a "$W/results.txt"
}
one 1 java mcp-session "$A_PUB" "$A_PRIV" a
one 1 rust mcp "$B_PUB" "$B_PRIV" b
one 1 rust mcp-session "$B_PUB" "$B_PRIV" b
one 1 node mcp-session "$A_PUB" "$A_PRIV" a
one 2 rust mcp "$A_PUB" "$A_PRIV" a
one 2 rust mcp-session "$A_PUB" "$A_PRIV" a
one 2 java mcp-session "$B_PUB" "$B_PRIV" b
one 2 node mcp-session "$B_PUB" "$B_PRIV" b
log "done"
