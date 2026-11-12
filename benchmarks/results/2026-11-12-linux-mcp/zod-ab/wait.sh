#!/usr/bin/env bash
while kill -0 "$(cat /root/served.pid)" 2>/dev/null; do sleep 0.5; done
cat /root/served.time
