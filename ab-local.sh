#!/usr/bin/env bash
B=$HOME/simoom-spike-target/release/cameleer-bench
run() { # engine scenario
  local eng=$1 sc=$2 port=$((20000 + RANDOM % 5000))
  local inl=; [ "${eng#*+}" = inline ] && inl=1
  env CAMEL_HTTP_ENGINE=${eng%%+*} ${inl:+CAMEL_SIMOOM_INLINE=1} taskset -c 0-3 $B serve $sc $port 40 >/tmp/srv.$$ 2>&1 &
  local pid=$!
  for _ in $(seq 50); do grep -q READY /tmp/srv.$$ && break; sleep 0.2; done
  taskset -c 4-11 $B loadgen-remote 127.0.0.1 $port $sc 64 5 5 3 2>&1 | sed "s/^/$eng /"
  kill $pid 2>/dev/null; wait $pid 2>/dev/null
}
for round in 1 2 3; do
  for sc in mcp-session http; do
    for eng in classic simoom; do run $eng $sc; done
  done
done
