#!/usr/bin/env bash
# On a server droplet: RSS (kB) of an idle server of /root/v/bin/<name>, after READY and 8 s. usage: vidle.sh <name> <scenario>
set -u
name=$1; sc=$2
rm -f /root/idle.out
nohup "/root/v/bin/$name" serve "$sc" 8082 40 >/root/idle.out 2>/dev/null &
pid=$!
for _ in $(seq 60); do grep -q '^READY' /root/idle.out && break; sleep 0.5; done
sleep 8
echo "IDLE label=$name scenario=$sc rss_kb=$(awk '/VmRSS/ {print $2}' /proc/$pid/status) threads=$(awk '/Threads/ {print $2}' /proc/$pid/status)"
kill $pid; wait $pid 2>/dev/null
