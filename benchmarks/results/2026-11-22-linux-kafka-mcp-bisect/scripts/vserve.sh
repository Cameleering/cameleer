#!/usr/bin/env bash
# On a server droplet: start one served scenario of /root/v/bin/<name> like bench-node.sh serve does,
# with extra environment (VENV, e.g. "CAMEL_ASYNC_ROUTING=false"), return at READY.
#   vserve.sh <name> <scenario> <port> <secs>      then vserve.sh wait
set -euo pipefail
ulimit -n 65536
if [ "$1" = wait ]; then
  while kill -0 "$(cat /root/served.pid)" 2>/dev/null; do sleep 0.5; done
  cat /root/served.time; exit 0
fi
name=$1; scenario=$2; port=$3; secs=$4
export BENCH_SIDE=rust
rm -f /root/served.out /root/served.time
# shellcheck disable=SC2086
nohup env ${VENV:-} /usr/bin/time -o /root/served.time -f "PROC side=$name scenario=$scenario peak_rss_kb=%M cpu_pct=%P user_s=%U sys_s=%S" \
  "/root/v/bin/$name" serve "$scenario" "$port" "$secs" >/root/served.out 2>/root/served.err &
echo $! > /root/served.pid
for _ in $(seq 120); do
  grep -q '^READY' /root/served.out 2>/dev/null && { grep '^VERSION' /root/served.out || true; exit 0; }
  kill -0 "$(cat /root/served.pid)" 2>/dev/null || { cat /root/served.err >&2; exit 1; }
  sleep 0.5
done
echo "server did not get ready" >&2; exit 1
