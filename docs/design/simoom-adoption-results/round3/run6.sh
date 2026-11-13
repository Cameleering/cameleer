#!/usr/bin/env bash
# Same-droplet A/B of the HTTP engines on DigitalOcean (the method of simoom's bench/cloud/ab.sh:
# one server droplet at a time, a separate load node, variants interleaved, the server droplet
# rotated between rounds). Variants: classic, simoom with the completion-based path and the per-route governor.
# Scenarios: http, rest-json, mcp-session at 64 connections, http at 256. Always deletes its droplets.
set -uo pipefail
W="$(cd "$(dirname "$0")" && pwd)"
REPO=/e/Work/DataHQ/Code/Rust-camel/.claude/worktrees/simoom-spike
export DO_TOKEN_FILE="$DO_TOKEN_FILE"
export BENCH_STATE="$W/droplets.txt"
KEY="$HOME/.ssh/id_ed25519"
SSH=(-i "$KEY" -o StrictHostKeyChecking=accept-new -o UserKnownHostsFile="$W/known_hosts" -o ServerAliveInterval=30 -o BatchMode=yes)
log() { printf '==> %s %s\n' "$(date -u +%H:%M:%S)" "$*"; }
down() { log "deleting the droplets"; bash "$REPO/scripts/cloud-bench/droplets.sh" down; }
trap down EXIT
on() { local h=$1; shift; ssh "${SSH[@]}" "root@$h" "$@"; }

git -C "$REPO" archive --format=tar.gz -o "$W/src.tar.gz" HEAD
log "source $(git -C "$REPO" rev-parse --short HEAD)"
bash "$REPO/scripts/cloud-bench/droplets.sh" up | tee "$W/up.log"
field() { awk -v n="$1" -v c="$2" '$1==n {print $c}' "$BENCH_STATE"; }
A_PUB=$(field cameleer-bench-a 2); A_PRIV=$(field cameleer-bench-a 3)
B_PUB=$(field cameleer-bench-b 2); B_PRIV=$(field cameleer-bench-b 3)
L_PUB=$(field cameleer-bench-load 2)
for h in "$A_PUB" "$B_PUB" "$L_PUB"; do for i in $(seq 24); do on "$h" true 2>/dev/null && break; sleep 10; done; done

log "shipping the source and building on all three"
for h in "$A_PUB" "$B_PUB" "$L_PUB"; do
  scp -q "${SSH[@]}" "$W/src.tar.gz" "root@$h:/root/src.tar.gz"
  scp -q "${SSH[@]}" "$W/remote-setup.sh" "root@$h:/root/remote-setup.sh"
  on "$h" 'rm -rf /root/cameleer && mkdir -p /root/cameleer && tar -xzf /root/src.tar.gz -C /root/cameleer'
done
i=0; for h in "$A_PUB" "$B_PUB" "$L_PUB"; do on "$h" 'bash /root/remote-setup.sh load' > "$W/setup-$i.log" 2>&1 & eval "p$i=\$!"; i=$((i+1)); done
for k in 0 1 2; do eval "wait \$p$k" || { tail -20 "$W/setup-$k.log"; exit 1; }; done
tail -n 2 "$W"/setup-*.log

machine() { on "$1" 'echo "CPU: $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2 | sed "s/^ *//")"; echo "vCPUs: $(nproc)"; echo "RAM: $(awk "/MemTotal/ {printf \"%.1f GiB\", \$2/1048576}" /proc/meminfo)"; echo "OS: $(. /etc/os-release && echo "$PRETTY_NAME") $(uname -r)"; echo "Rust: $(/root/.cargo/bin/rustc -V)"'; }
{ echo "== server a"; machine "$A_PUB"; echo "== server b"; machine "$B_PUB"; echo "== load"; machine "$L_PUB"
  echo "Commit: $(git -C "$REPO" rev-parse --short HEAD)"; echo "Date: $(date -u '+%Y-%m-%d %H:%M UTC')"
  echo "Plan: warmup 5 s, 3 x 5 s per run; 64 connections (http also 256); variants interleaved, 2 rounds, server droplet rotated"; } > "$W/machine.txt"

: > "$W/results.txt"
BIN=/root/cameleer/target/release/cameleer-bench
one() { # one <round> <variant> <scenario> <conns> <server-pub> <server-priv> <node>
  local round=$1 var=$2 sc=$3 conns=$4 pub=$5 priv=$6 node=$7
  local eng=${var%%+*} inl=""; [ "${var#*+}" = inline ] && inl="CAMEL_SIMOOM_INLINE=1"
  local tag="round=$round node=$node variant=$var scenario=$sc conns=$conns"
  log "$tag"
  on "$pub" "[ -f /root/srv.pid ] && kill \$(cat /root/srv.pid) 2>/dev/null; rm -f /root/srv.log /root/srv.pid; env CAMEL_HTTP_ENGINE=$eng $inl setsid nohup $BIN serve $sc 8080 60 < /dev/null > /root/srv.log 2>&1 & echo \$! > /root/srv.pid; echo started"
  for _ in $(seq 50); do on "$pub" 'grep -q READY /root/srv.log' && break; sleep 0.5; done
  on "$L_PUB" "$BIN loadgen-remote $priv 8080 $sc $conns 5 5 3" > "$W/lg.out" 2>>"$W/run.log" &
  local lg=$!
  sleep 6
  local a; a=$(on "$pub" 'p=$(cat /root/srv.pid); awk "{print \$14+\$15}" /proc/$p/stat')
  sleep 14
  local b thr rss; read -r b thr rss < <(on "$pub" 'p=$(cat /root/srv.pid); echo "$(awk "{print \$14+\$15}" /proc/$p/stat) $(ls /proc/$p/task | wc -l) $(awk "/VmHWM/ {print \$2}" /proc/$p/status)"')
  wait $lg
  on "$pub" 'kill $(cat /root/srv.pid); sleep 0.5; true'
  { sed "s/^RESULT /RESULT $tag /" "$W/lg.out" || echo "FAILED $tag"; echo "PROC $tag cpu_ticks_14s=$((b-a)) threads=$thr peak_rss_kb=$rss"; } | tee -a "$W/results.txt"
}
for round in 1 2; do
  if [ $round = 1 ]; then P=$A_PUB; Q=$A_PRIV; N=a; else P=$B_PUB; Q=$B_PRIV; N=b; fi
  for spec in "http 64" "rest-json 64" "mcp-session 64" "http 256"; do
    set -- $spec
    for v in classic simoom; do one $round $v $1 $2 "$P" "$Q" $N; done
  done
done
log "done"
