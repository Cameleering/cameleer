#!/usr/bin/env bash
# Driver (runs on your machine, needs droplets.sh up and the setup of vsetup.sh): runs plan.txt twice, pass 1 on
# server b (phase 1) in plan order, pass 2 on server a (phase 2) in reverse order, so each case ran on both machines
# and in both orders. A failing case is retried once; a second failure stops the run.
#   vrun.sh <plan> <results-file>
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/../../../.." && pwd)"
OUT="$ROOT/target/cloud-bench"; STATE="$OUT/droplets.txt"
KEY="${DO_SSH_KEY:-$HOME/.ssh/id_ed25519}"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$STATE"; }
A_PUB=$(field cameleer-bench-a 2); A_PRIV=$(field cameleer-bench-a 3)
B_PUB=$(field cameleer-bench-b 2); B_PRIV=$(field cameleer-bench-b 3)
L_PUB=$(field cameleer-bench-load 2); L_PRIV=$(field cameleer-bench-load 3)
SSH_OPTS=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$OUT/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
on() { local h=$1; shift; ssh "${SSH_OPTS[@]}" "root@$h" "$@"; }
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*" >&2; }
PLAN=$1; RES=$2; : > "$RES"
WARM=15; ITER=5; ITERS=5; CONNS=64; PORT=8080; secs=$((WARM + ITER * ITERS + 10))
on "$L_PUB" "pkill -f 'raw-http 808[1]' || true"
on "$L_PUB" "nohup /root/cameleer/target/release/cameleer-bench raw-http 8081 poll >/root/raw.out 2>&1 &" >/dev/null 2>&1 || true
sleep 2; on "$L_PUB" "ss -ltn | grep -c ':8081 '" >&2
case1() { # phase node pub priv label venv scenario bin
  local phase=$1 node=$2 pub=$3 priv=$4 label=$5 venv=$6 sc=$7 bin=$8 tmp; tmp=$(mktemp)
  [ "$venv" = - ] && venv=""
  log "phase $phase node $node: $label $sc"
  [ "$sc" = http-sql ] && on "$L_PUB" "PGPASSWORD=camelpw psql -q -h 127.0.0.1 -U camel -d bench -c 'TRUNCATE orders'" >/dev/null 2>&1
  on "$pub" "BENCH_KAFKA=$L_PRIV:9092 BENCH_PG=$L_PRIV BENCH_PROXY_TARGET=$L_PRIV:8081 VENV='$venv' /root/v/vserve.sh $bin $sc $PORT $secs" >/dev/null 2>>"$OUT/vrun.err" || { rm -f "$tmp"; return 1; }
  on "$L_PUB" "/root/cameleer/target/release/cameleer-bench loadgen-remote $priv $PORT $sc $CONNS $WARM $ITER $ITERS" 2>>"$OUT/vrun.err" \
    | sed -e "s/^RESULT /RESULT phase=$phase node=$node label=$label scenario=$sc /" > "$tmp"
  local p; p=$(on "$pub" "/root/v/vserve.sh wait" 2>>"$OUT/vrun.err" | sed "s/^PROC side=[^ ]* scenario=[^ ]* /PROC phase=$phase node=$node label=$label scenario=$sc /")
  if grep -q '^RESULT' "$tmp"; then cat "$tmp" >> "$RES"; echo "$p" >> "$RES"; tail -n2 "$tmp" | head -1 | cut -c1-200 >&2; rm -f "$tmp"; return 0; fi
  rm -f "$tmp"; return 1
}
run() { # phase node pub priv line
  local line=$5; IFS='|' read -r label venv sc bin <<< "$line"
  case1 "$1" "$2" "$3" "$4" "$label" "$venv" "$sc" "$bin" || case1 "$1" "$2" "$3" "$4" "$label" "$venv" "$sc" "$bin" || { echo "FAILED twice: $line" | tee -a "$RES"; exit 2; }
}
mapfile -t LINES < <(grep -v '^#' "$PLAN" | grep -v '^$')
[ "${PASS1:-1}" = 1 ] && for l in "${LINES[@]}"; do run 1 b "$B_PUB" "$B_PRIV" "$l"; done
for ((i=${#LINES[@]}-1; i>=0; i--)); do run 2 a "$A_PUB" "$A_PRIV" "${LINES[$i]}"; done
log "results in $RES"
