#!/usr/bin/env bash
# Create and delete the DigitalOcean droplets of the cloud benchmark (see README.md here).
#
#   scripts/cloud-bench/droplets.sh up       create the droplets (prints name and IPs)
#   scripts/cloud-bench/droplets.sh status   list droplets and SSH keys tagged for the benchmark
#   scripts/cloud-bench/droplets.sh down     delete every droplet tagged cameleer-bench and the SSH key
#
# Environment:
#   DO_TOKEN_FILE   file holding a DigitalOcean API token (read here only, never printed)
#   DO_REGION       region (default blr1); DO_SIZE size (default c-4, 4 dedicated vCPU, 8 GB)
#   DO_SSH_PUB      public key to log in with (default ~/.ssh/id_ed25519.pub)
#   BENCH_STATE     where the droplet list is written (default target/cloud-bench/droplets.txt)
set -euo pipefail

TAG=cameleer-bench
KEY_NAME=cameleer-bench-key
REGION="${DO_REGION:-blr1}"
SIZE="${DO_SIZE:-c-4}"
IMAGE=ubuntu-24-04-x64
NAMES=(cameleer-bench-a cameleer-bench-b cameleer-bench-load)
SSH_PUB="${DO_SSH_PUB:-$HOME/.ssh/id_ed25519.pub}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
STATE="${BENCH_STATE:-$ROOT/target/cloud-bench/droplets.txt}"
API=https://api.digitalocean.com/v2

[ -n "${DO_TOKEN_FILE:-}" ] && [ -f "$DO_TOKEN_FILE" ] || { echo "set DO_TOKEN_FILE to the token file" >&2; exit 1; }
TOKEN="$(grep -o 'dop_v1_[0-9a-f]\{64\}' "$DO_TOKEN_FILE" | head -1)"
[ -n "$TOKEN" ] || { echo "no DigitalOcean token in $DO_TOKEN_FILE" >&2; exit 1; }

api() { # api METHOD PATH [JSON]
  local args=(-sS -X "$1" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json")
  [ $# -ge 3 ] && args+=(-d "$3")
  curl "${args[@]}" "$API$2"
}

PY=python3
"$PY" -c 'import sys' 2>/dev/null || PY=python
py() { "$PY" -c "$@"; }

key_id() {
  api GET "/account/keys?per_page=200" | py "
import json,sys
for k in json.load(sys.stdin)['ssh_keys']:
    if k['name']=='$KEY_NAME': print(k['id'])"
}

list() { # name public_ip private_ip status, one droplet per line
  api GET "/droplets?tag_name=$TAG&per_page=200" | py "
import json,sys
for d in json.load(sys.stdin)['droplets']:
    ip=lambda t: next((n['ip_address'] for n in d['networks']['v4'] if n['type']==t), '-')
    print(d['name'], ip('public'), ip('private'), d['status'], d['size_slug'], d['region']['slug'], d['id'])"
}

up() {
  local kid; kid="$(key_id)"
  if [ -z "$kid" ]; then
    local pub; pub="$(cat "$SSH_PUB")"
    kid="$(api POST /account/keys "{\"name\":\"$KEY_NAME\",\"public_key\":\"$pub\"}" | py "import json,sys; print(json.load(sys.stdin)['ssh_key']['id'])")"
  fi
  local names; names="$(printf '"%s",' "${NAMES[@]}")"
  api POST /droplets "{\"names\":[${names%,}],\"region\":\"$REGION\",\"size\":\"$SIZE\",\"image\":\"$IMAGE\",\"ssh_keys\":[$kid],\"tags\":[\"$TAG\"],\"monitoring\":false,\"ipv6\":false}" \
    | py "import json,sys; r=json.load(sys.stdin); print('created:', [d['name'] for d in r['droplets']]) if 'droplets' in r else sys.exit('create failed: '+r.get('message',''))"
  echo "waiting for the droplets to become active" >&2
  for _ in $(seq 60); do
    if [ "$(list | awk '$4=="active" && $3!="-"' | wc -l)" -eq "${#NAMES[@]}" ]; then break; fi
    sleep 5
  done
  mkdir -p "$(dirname "$STATE")"
  list | tee "$STATE"
}

down() {
  local ids; ids="$(list | awk '{print $7}')"
  if [ -n "$ids" ]; then
    api DELETE "/droplets?tag_name=$TAG" >/dev/null
    echo "deleting droplets: $(list | awk '{print $1}' | tr '\n' ' ')" >&2
    for _ in $(seq 60); do [ -z "$(list)" ] && break; sleep 5; done
  fi
  local kid; kid="$(key_id)"
  [ -n "$kid" ] && api DELETE "/account/keys/$kid" >/dev/null && echo "deleted SSH key $KEY_NAME" >&2
  status
}

status() {
  echo "droplets tagged $TAG:"; list | sed 's/^/  /'
  echo "SSH key $KEY_NAME: $(key_id || true)"
}

case "${1:-status}" in
  up) up ;;
  down) down ;;
  status) status ;;
  *) echo "usage: $0 up|status|down" >&2; exit 1 ;;
esac
