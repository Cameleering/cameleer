#!/usr/bin/env bash
# Start the node baseline the way scripts/cloud-bench/bench-node.sh starts a served scenario.
# usage: serve.sh <dir> <port> <secs>     returns at READY; wait.sh waits for the exit
set -euo pipefail
dir=$1; port=$2; secs=$3
cd "$dir"
ulimit -n 65536
rm -f /root/served.out /root/served.time
nohup /usr/bin/time -o /root/served.time -f "PROC peak_rss_kb=%M cpu_pct=%P user_s=%U sys_s=%S" \
  /opt/node/bin/node server.mjs serve mcp-session "$port" "$secs" >/root/served.out 2>/root/served.err &
echo $! > /root/served.pid
for _ in $(seq 120); do
  grep -q '^READY' /root/served.out 2>/dev/null && { grep '^VERSION' /root/served.out; exit 0; }
  kill -0 "$(cat /root/served.pid)" 2>/dev/null || { cat /root/served.err >&2; exit 1; }
  sleep 0.5
done
echo "server did not get ready" >&2; exit 1
