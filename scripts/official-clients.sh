#!/usr/bin/env bash
# Official-client cross-verification for rust-camel.
#
# Drives rust-camel's embedded brokers, datastores, cloud emulators and protocol stacks with
# independently written, officially published clients (librdkafka/kcat, pymongo, the DataStax
# Cassandra driver, elasticsearch-py, redis-py, boto3, the Azure SDK for Python, gcloud /
# gsutil / bq, grpcio, Go's crypto/tls and net/http). The suites live in
# crates/<crate>/tests/official_*.rs, the client programs in scripts/official-clients/.
#
#   scripts/official-clients.sh install    # apt-get install the clients (root, idempotent)
#   scripts/official-clients.sh versions   # print client versions
#   scripts/official-clients.sh test       # CAMEL_OFFICIAL_CLIENTS=1 cargo test --workspace --test 'official_*'
#   scripts/official-clients.sh all        # install + versions + test
#
# Requirements outside apt: the Google Cloud SDK (gcloud, gsutil, bq; e.g. under
# /opt/google-cloud-sdk, on PATH) and a Go toolchain >= 1.22 (/usr/local/go/bin/go, or set
# CAMEL_OFFICIAL_GO; `golang-go` from apt also works). The Python clients are the Ubuntu 24.04
# python3-* packages, run with /usr/bin/python3.12 (override with CAMEL_OFFICIAL_PYTHON).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${CAMEL_OFFICIAL_PYTHON:-/usr/bin/python3.12}"
GO="${CAMEL_OFFICIAL_GO:-/usr/local/go/bin/go}"
[ -x "$GO" ] || GO=go

APT_PACKAGES=(
  kcat python3-confluent-kafka python3-lz4 python3-snappy
  python3-pymongo python3-cassandra python3-elasticsearch python3-redis
  python3-boto3
  python3-azure python3-azure-storage python3-azure-cosmos
  python3-grpcio python3-grpc-tools python3-protobuf
  python3-jwt python3-cryptography
)

install() {
  local sudo=""
  [ "$(id -u)" = 0 ] || sudo=sudo
  $sudo apt-get update -qq
  DEBIAN_FRONTEND=noninteractive $sudo apt-get install -y -qq "${APT_PACKAGES[@]}"
  command -v "$GO" >/dev/null || DEBIAN_FRONTEND=noninteractive $sudo apt-get install -y -qq golang-go
  command -v gcloud >/dev/null || echo "note: the Google Cloud SDK (gcloud/gsutil/bq) is not on PATH; the GCP CLI test needs it" >&2
}

versions() {
  echo "kcat:            $(kcat -V 2>/dev/null | grep -m1 -o 'Version [0-9.]*' || echo missing)"
  "$PY" -I - <<'EOF'
import importlib
mods = [("confluent_kafka", "confluent-kafka"), ("pymongo", "pymongo"), ("cassandra", "cassandra-driver"),
        ("elasticsearch", "elasticsearch-py"), ("redis", "redis-py"), ("boto3", "boto3"), ("botocore", "botocore"),
        ("azure.core", "azure-core"), ("azure.storage.blob", "azure-storage-blob"), ("azure.storage.queue", "azure-storage-queue"),
        ("azure.storage.filedatalake", "azure-storage-file-datalake"), ("azure.storage.fileshare", "azure-storage-file-share"),
        ("azure.identity", "azure-identity"), ("azure.keyvault.secrets", "azure-keyvault-secrets"), ("azure.keyvault.keys", "azure-keyvault-keys"),
        ("azure.cosmos", "azure-cosmos"), ("azure.servicebus", "azure-servicebus"), ("azure.eventhub", "azure-eventhub"),
        ("grpc", "grpcio"), ("google.protobuf", "protobuf")]
for mod, name in mods:
    try:
        m = importlib.import_module(mod)
        v = getattr(m, "__version__", None) or getattr(getattr(m, "_version", None), "VERSION", "?")
        if mod == "confluent_kafka":
            v = "%s (librdkafka %s)" % (m.version()[0], m.libversion()[0])
        print("%-16s %s" % (name + ":", v))
    except Exception as e:
        print("%-16s missing (%s)" % (name + ":", e))
EOF
  echo "gcloud:          $(gcloud version 2>/dev/null | head -1 || echo missing)"
  echo "bq:              $(bq version 2>/dev/null | tail -1 || echo missing)"
  echo "go:              $("$GO" version 2>/dev/null || echo missing)"
}

run_tests() {
  cd "$ROOT"
  CAMEL_OFFICIAL_CLIENTS=1 CAMEL_OFFICIAL_PYTHON="$PY" CAMEL_OFFICIAL_GO="$(command -v "$GO")" cargo test --workspace --test 'official_*' "$@"
}

case "${1:-}" in
  install) install ;;
  versions) versions ;;
  test) shift; run_tests "$@" ;;
  all) install; versions; run_tests ;;
  *) sed -n '2,20p' "$0"; exit 1 ;;
esac
