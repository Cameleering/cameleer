#!/usr/bin/env bash
# The official OpenTelemetry Collector against Cameleer's OTLP/HTTP exporter.
#
# Installs the core distribution of the Collector (`otelcol`) at a pinned version, verified by
# SHA-256, and runs crates/cameleer-opentelemetry/tests/official_otel_collector.rs: Cameleer
# exports to the Collector's OTLP/HTTP receiver, the Collector forwards with its own otlphttp
# exporter to Cameleer's embedded receiver, and the test compares what arrives with what was
# sent. No root needed; nothing leaves the machine.
#
#   scripts/otel-collector.sh install   # download, verify, unpack into $CAMEL_OTELCOL_DIR
#   scripts/otel-collector.sh version   # print the Collector version
#   scripts/otel-collector.sh test      # CAMEL_OFFICIAL_CLIENTS=1 cargo test ... official_otel_collector
#   scripts/otel-collector.sh all       # install + version + test
#
# Linux x86_64 (Ubuntu 24.04, WSL2 works). The version is pinned: to move it, change VERSION and
# SHA256 together (the digest GitHub lists for the release asset
# otelcol_<version>_linux_amd64.tar.gz of open-telemetry/opentelemetry-collector-releases).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="0.165.0"
SHA256="78570dd4bd6570bc41eb327869ef913bbf91b87dd8170ebebbf43bec71f6370d"
ASSET="otelcol_${VERSION}_linux_amd64.tar.gz"
URL="https://github.com/open-telemetry/opentelemetry-collector-releases/releases/download/v${VERSION}/${ASSET}"
DIR="${CAMEL_OTELCOL_DIR:-$HOME/.cache/cameleer/otelcol-${VERSION}}"
BIN="$DIR/otelcol"

install() {
  if [ -x "$BIN" ]; then
    echo "otelcol ${VERSION} already installed in $DIR"
    return
  fi
  mkdir -p "$DIR"
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN
  curl -fsSL --retry 3 -o "$tmp/$ASSET" "$URL"
  echo "${SHA256}  $tmp/$ASSET" | sha256sum -c --quiet - || { echo "checksum mismatch for $ASSET" >&2; exit 1; }
  tar -xzf "$tmp/$ASSET" -C "$tmp" otelcol
  mv "$tmp/otelcol" "$BIN"
  chmod +x "$BIN"
  echo "installed otelcol ${VERSION} in $DIR (sha256 verified)"
}

version() {
  "$BIN" --version
}

test_() {
  cd "$ROOT"
  CAMEL_OFFICIAL_CLIENTS=1 CAMEL_OTELCOL="$BIN" cargo test -p cameleer-opentelemetry --test official_otel_collector -- --nocapture
}

case "${1:-all}" in
  install) install ;;
  version) version ;;
  test) test_ ;;
  all) install; version; test_ ;;
  *) echo "usage: $0 install|version|test|all" >&2; exit 2 ;;
esac
