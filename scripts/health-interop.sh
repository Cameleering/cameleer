#!/usr/bin/env bash
# The health pack's live tier: independent HL7, FHIR and DICOM peers for Cameleer.
#
#   scripts/health-interop.sh install   # DCMTK + Orthanc (dicom-interop.sh), python-hl7, the HAPI FHIR image
#   scripts/health-interop.sh start     # start every peer (running ones are left alone)
#   scripts/health-interop.sh stop      # stop what this script started
#   scripts/health-interop.sh status    # show which ports answer
#   scripts/health-interop.sh env       # print the variables the tests need (eval "$(... env)")
#   scripts/health-interop.sh all       # install + start
#
# Then: eval "$(scripts/health-interop.sh env)" && CAMEL_HEALTH_INTEROP=1 cargo test -p cameleer-health --test 'live_*'
# (CAMEL_DICOM_INTEROP=1 still runs only the DICOM part.)
#
# Peers, all on 127.0.0.1:
#   DICOM  everything of scripts/dicom-interop.sh (DCMTK storescp 11130/11131, Orthanc 14242/18042)
#   HL7    python-hl7 0.4.5 (PyPI wheel, SHA-256 pinned, imported from the wheel file: nothing is
#          installed). An MLLP receiver on 12575 acknowledges with AA and writes each message to
#          $DIR/hl7/; the tests also use its MLLP client against a Cameleer consumer on 12576.
#   FHIR   HAPI FHIR JPA server 8.12.0 (image hapiproject/hapi pinned by digest, in-memory H2
#          database) on 18085, base http://127.0.0.1:18085/fhir. Needs Docker.
#
# State lives under $CAMEL_HEALTH_INTEROP_DIR (default $HOME/camel-health-interop).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
DIR="${CAMEL_HEALTH_INTEROP_DIR:-$HOME/camel-health-interop}"
LOGS="$DIR/logs"
RUN="$DIR/run"

HL7_WHEEL=hl7-0.4.5-py2.py3-none-any.whl
HL7_URL=https://files.pythonhosted.org/packages/8d/2b/024977b044ca3aa79153e0a4ff13cc465064ccf9cf7b60df46e6eab8340e/$HL7_WHEEL
HL7_SHA256=f46bf7c165801fd40e8fcc9f2be41c5794415039adc97646aee7ee6b20ce8ccd
HAPI_IMAGE=hapiproject/hapi:v8.12.0-2@sha256:0ff86aac4c7bd13a1ee226444a70263bb46ed1c28ef543c30a44f865b80ca04e
HAPI_NAME=camel-health-hapi

log() { echo "[health-interop] $*"; }
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
wait_port() {
  local p=$1 n=0 max=${2:-100}
  while ! port_open "$p"; do
    n=$((n + 1))
    if [ $n -gt "$max" ]; then log "port $p did not open"; return 1; fi
    sleep 0.2
  done
}

cmd_install() {
  bash "$HERE/dicom-interop.sh" install
  bash "$HERE/dicom-interop.sh" setup
  mkdir -p "$DIR/wheels" "$LOGS" "$RUN" "$DIR/hl7"
  local w="$DIR/wheels/$HL7_WHEEL"
  if [ ! -f "$w" ] || [ "$(sha256sum "$w" | cut -c1-64)" != "$HL7_SHA256" ]; then
    curl -fsSL -o "$w.tmp" "$HL7_URL"
    local got; got="$(sha256sum "$w.tmp" | cut -c1-64)"
    if [ "$got" != "$HL7_SHA256" ]; then rm -f "$w.tmp"; log "checksum mismatch for $HL7_WHEEL: $got"; exit 1; fi
    mv "$w.tmp" "$w"
  fi
  if command -v docker >/dev/null 2>&1; then
    docker pull "$HAPI_IMAGE" >/dev/null
  else
    log "docker not found: the FHIR peer is not available"
  fi
  log "installed under $DIR"
}

start_hl7() {
  port_open 12575 && return 0
  mkdir -p "$DIR/hl7" "$LOGS" "$RUN"
  PYTHONPATH="$DIR/wheels/$HL7_WHEEL" nohup python3 "$HERE/health-interop/hl7_receiver.py" 127.0.0.1 12575 "$DIR/hl7" >"$LOGS/hl7.log" 2>&1 &
  echo $! > "$RUN/hl7.pid"
  wait_port 12575
}

start_fhir() {
  command -v docker >/dev/null 2>&1 || { log "docker not found: skipping FHIR"; return 0; }
  port_open 18085 && return 0
  docker rm -f "$HAPI_NAME" >/dev/null 2>&1 || true
  docker run -d --name "$HAPI_NAME" -p 127.0.0.1:18085:8080 "$HAPI_IMAGE" >/dev/null
  wait_port 18085 600
  # the port opens before the server answers
  for _ in $(seq 1 300); do
    if curl -fsS http://127.0.0.1:18085/fhir/metadata >/dev/null 2>&1; then break; fi
    sleep 1
  done
}

cmd_start() {
  bash "$HERE/dicom-interop.sh" start
  start_hl7
  start_fhir
  log "started"
}

cmd_stop() {
  bash "$HERE/dicom-interop.sh" stop
  if [ -f "$RUN/hl7.pid" ]; then kill "$(cat "$RUN/hl7.pid")" 2>/dev/null || true; rm -f "$RUN/hl7.pid"; fi
  command -v docker >/dev/null 2>&1 && docker rm -f "$HAPI_NAME" >/dev/null 2>&1 || true
  log "stopped"
}

cmd_status() {
  bash "$HERE/dicom-interop.sh" status
  for p in 12575 18085; do
    if port_open $p; then echo "$p open"; else echo "$p closed"; fi
  done
}

cmd_env() {
  bash "$HERE/dicom-interop.sh" env
  echo "export CAMEL_HEALTH_INTEROP_DIR=\"$DIR\""
  echo "export CAMEL_HEALTH_HL7_PYTHONPATH=\"$DIR/wheels/$HL7_WHEEL\""
  echo "export CAMEL_HEALTH_HL7_CLIENT=\"$HERE/health-interop/hl7_send.py\""
  echo "export CAMEL_HEALTH_FHIR_URL=\"http://127.0.0.1:18085/fhir\""
}

case "${1:-}" in
  install) cmd_install ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  env) cmd_env ;;
  all) cmd_install; cmd_start ;;
  *) sed -n '2,10p' "$0"; exit 2 ;;
esac
