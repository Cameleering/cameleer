#!/usr/bin/env bash
# Re-measure the mcp-session TypeScript baseline with zod 3.25.76 (main) and zod 4.6.7 (PR #17)
# on the same server droplet, alternating, so drift and machine differences cancel out.
# Always deletes the droplets it made (trap), even on failure.
set -uo pipefail
W="$(cd "$(dirname "$0")" && pwd)"
REPO="$(git rev-parse --show-toplevel)"
: "${DO_TOKEN_FILE:?set DO_TOKEN_FILE to the file holding a DigitalOcean API token}"; export DO_TOKEN_FILE
export DO_NAMES="cameleer-bench-a cameleer-bench-load"
export BENCH_STATE="$W/droplets.txt"
KEY="$HOME/.ssh/id_ed25519"
SSH=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$W/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*"; }
down() { log "deleting the droplets"; bash "$REPO/scripts/cloud-bench/droplets.sh" down; }
trap down EXIT

cd "$REPO"
git fetch -q origin
SRC_REF=origin/main
log "source: $(git rev-parse --short $SRC_REF)"
git archive --format=tar.gz -o "$W/src.tar.gz" $SRC_REF
for v in z3 z4; do rm -rf "$W/$v"; mkdir -p "$W/$v"; done
for f in package.json package-lock.json server.mjs; do
  git show $SRC_REF:scripts/cloud-bench/mcp-ts/$f > "$W/z3/$f"
  git show origin/dependabot/npm_and_yarn/scripts/cloud-bench/mcp-ts/zod-4.6.7:scripts/cloud-bench/mcp-ts/$f > "$W/z4/$f"
done
diff -q "$W/z3/server.mjs" "$W/z4/server.mjs" && log "server.mjs identical in both"
grep '"zod"' "$W/z3/package.json" "$W/z4/package.json"

# The account already holds this machine's public key (as another session's key), and DigitalOcean
# refuses a duplicate upload, so create the droplets with that key's fingerprint instead and
# leave the key alone. droplets.sh down deletes only the droplets tagged cameleer-bench.
TOKEN="$(grep -o 'dop_v1_[0-9a-f]\{64\}' "$DO_TOKEN_FILE" | head -1)"
FP="$(ssh-keygen -lE md5 -f "$KEY.pub" | awk '{print $2}' | sed 's/^MD5://')"
curl -sS -X POST -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" https://api.digitalocean.com/v2/droplets   -d "{\"names\":[\"cameleer-bench-a\",\"cameleer-bench-load\"],\"region\":\"blr1\",\"size\":\"c-4\",\"image\":\"ubuntu-24-04-x64\",\"ssh_keys\":[\"$FP\"],\"tags\":[\"cameleer-bench\"],\"monitoring\":false,\"ipv6\":false}"   | python -c "import json,sys; r=json.load(sys.stdin); print('created:', [d['name'] for d in r['droplets']]) if 'droplets' in r else sys.exit('create failed: '+r.get('message',''))" || exit 1
for _ in $(seq 60); do
  n=$(curl -sS -H "Authorization: Bearer $TOKEN" "https://api.digitalocean.com/v2/droplets?tag_name=cameleer-bench" | python -c "
import json,sys
d=json.load(sys.stdin)['droplets']
ok=[x for x in d if x['status']=='active' and any(n['type']=='private' for n in x['networks']['v4'])]
print(len(ok))")
  [ "$n" = 2 ] && break; sleep 5
done
curl -sS -H "Authorization: Bearer $TOKEN" "https://api.digitalocean.com/v2/droplets?tag_name=cameleer-bench" | python -c "
import json,sys
for d in json.load(sys.stdin)['droplets']:
    ip=lambda t: next((n['ip_address'] for n in d['networks']['v4'] if n['type']==t), '-')
    print(d['name'], ip('public'), ip('private'), d['status'])" | tee "$BENCH_STATE"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$BENCH_STATE"; }
S_PUB=$(field cameleer-bench-a 2); S_PRIV=$(field cameleer-bench-a 3)
L_PUB=$(field cameleer-bench-load 2)
on() { local h=$1; shift; ssh "${SSH[@]}" "root@$h" "$@"; }
for i in 1 2 3 4 5 6 7 8 9 10 11 12; do on "$S_PUB" true 2>/dev/null && on "$L_PUB" true 2>/dev/null && break; sleep 10; done

log "shipping files"
scp -q "${SSH[@]}" "$W/remote-setup.sh" "root@$S_PUB:/root/remote-setup.sh"
scp -q "${SSH[@]}" "$W/serve.sh" "$W/wait.sh" "root@$S_PUB:/root/"
scp -q "${SSH[@]}" "$W/remote-setup.sh" "root@$L_PUB:/root/remote-setup.sh"
scp -q "${SSH[@]}" "$W/src.tar.gz" "root@$L_PUB:/root/src.tar.gz"
on "$L_PUB" 'mkdir -p /root/cameleer && tar -xzf /root/src.tar.gz -C /root/cameleer'
for v in z3 z4; do
  on "$S_PUB" "mkdir -p /root/$v"
  scp -q "${SSH[@]}" "$W/$v/package.json" "$W/$v/package-lock.json" "$W/$v/server.mjs" "root@$S_PUB:/root/$v/"
done
log "setting up both nodes in parallel"
on "$S_PUB" 'bash /root/remote-setup.sh server' > "$W/setup-server.log" 2>&1 &
p1=$!
on "$L_PUB" 'bash /root/remote-setup.sh load' > "$W/setup-load.log" 2>&1 &
p2=$!
wait $p1 || { tail -20 "$W/setup-server.log"; exit 1; }
wait $p2 || { tail -20 "$W/setup-load.log"; exit 1; }
tail -n 3 "$W/setup-server.log" "$W/setup-load.log"

{ echo "== server"; on "$S_PUB" 'echo "CPU: $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2 | sed "s/^ *//")"; echo "vCPUs: $(nproc)"; echo "RAM: $(awk "/MemTotal/ {printf \"%.1f GiB\", \$2/1048576}" /proc/meminfo)"; echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME") $(uname -r)"; /opt/node/bin/node -v'
  echo "Commit: $(git rev-parse --short $SRC_REF)"; echo "Date: $(date -u '+%Y-%m-%d %H:%M UTC')"; } > "$W/machine.txt"

WARM=15; ITER=5; ITERS=5; CONNS=64; PORT=8080
SECS=$((WARM + ITER * ITERS + 10))
: > "$W/results.txt"
for round in ${ROUNDS:-1 2 3}; do
  for v in z3 z4; do
    log "round $round: $v"
    on "$S_PUB" "bash /root/serve.sh /root/$v $PORT $SECS" || { echo "FAILED variant=$v round=$round" | tee -a "$W/results.txt"; continue; }
    on "$L_PUB" "/root/cameleer/target/release/cameleer-bench loadgen-remote $S_PRIV $PORT mcp-session $CONNS $WARM $ITER $ITERS" 2>>"$W/run.log" | sed "s/^RESULT /RESULT variant=$v round=$round /" | tee -a "$W/results.txt"
    on "$S_PUB" 'bash /root/wait.sh' | sed "s/^PROC /PROC variant=$v round=$round /" | tee -a "$W/results.txt"
  done
done
log "done"
