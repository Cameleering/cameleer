#!/usr/bin/env bash
# Official-client cross-verification for Cameleer.
#
# Drives Cameleer's embedded brokers, datastores, cloud emulators and protocol stacks with
# independently written, officially published clients (librdkafka/kcat, pymongo, the DataStax
# Cassandra driver, elasticsearch-py, redis-py, boto3, the Azure SDK for Python, Alibaba Cloud's
# SLS Python SDK (aliyun-log-python-sdk), gcloud /
# gsutil / bq, grpcio, Go's crypto/tls and net/http, Elastic's Filebeat). The suites live in
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
# Filebeat (the Apache-2.0 OSS build, for the lumberjack consumer) is downloaded from
# artifacts.elastic.co at a pinned version and checked against a pinned SHA-512 before it is
# unpacked into /opt/camel-official/filebeat (override with CAMEL_OFFICIAL_FILEBEAT).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PY="${CAMEL_OFFICIAL_PYTHON:-/usr/bin/python3.12}"
GO="${CAMEL_OFFICIAL_GO:-/usr/local/go/bin/go}"
[ -x "$GO" ] || GO=go

FILEBEAT_VERSION=9.2.0
FILEBEAT_SHA512_X86_64=86bf06f9f0cb71c7973a341ca1d6785c8e678a113e104febf4b594b1d6fbbb0eb194f49adb2c32f44b143c890336e35a3fcff019697e48d68529ac62dfee1f4c
FILEBEAT_DIR=/opt/camel-official/filebeat
FILEBEAT="${CAMEL_OFFICIAL_FILEBEAT:-$FILEBEAT_DIR/filebeat}"

APT_PACKAGES=(
  kcat python3-confluent-kafka python3-lz4 python3-snappy
  python3-pymongo python3-cassandra python3-elasticsearch python3-redis
  python3-boto3
  python3-azure python3-azure-storage python3-azure-cosmos
  python3-grpcio python3-grpc-tools python3-protobuf
  python3-jwt python3-cryptography
  python3-six python3-requests python3-dateutil python3-jmespath python3-dateparser
)

# Alibaba Cloud's SLS Python SDK is not packaged by Ubuntu: its source release from PyPI, pinned
# and checked against this SHA-256 (the digest PyPI publishes for the file), unpacked under
# /opt/camel-official; its dependencies are the python3-* packages above.
SLS_SDK_VERSION=0.9.53
SLS_SDK_SHA256=271dc74d366d3cf97f2c42e887f5f4200ef84b04c459b35f9a888a1916acf207
SLS_SDK_URL="https://files.pythonhosted.org/packages/source/a/aliyun-log-python-sdk/aliyun_log_python_sdk-${SLS_SDK_VERSION}.tar.gz"
SLS_SDK_DIR="${CAMEL_OFFICIAL_SLS_SDK:-/opt/camel-official/aliyun-log-python-sdk-${SLS_SDK_VERSION}}"

install_sls_sdk() {
  local sudo="$1" tmp
  [ -d "$SLS_SDK_DIR/aliyun/log" ] && return 0
  tmp=$(mktemp -d)
  curl -fsSL -o "$tmp/sdk.tar.gz" "$SLS_SDK_URL"
  echo "$SLS_SDK_SHA256  $tmp/sdk.tar.gz" | sha256sum -c - >/dev/null || { echo "aliyun-log-python-sdk: checksum mismatch" >&2; rm -rf "$tmp"; return 1; }
  tar -xzf "$tmp/sdk.tar.gz" -C "$tmp"
  $sudo mkdir -p "$(dirname "$SLS_SDK_DIR")"
  $sudo rm -rf "$SLS_SDK_DIR"
  $sudo mv "$tmp/aliyun_log_python_sdk-${SLS_SDK_VERSION}" "$SLS_SDK_DIR"
  rm -rf "$tmp"
}

install() {
  local sudo=""
  [ "$(id -u)" = 0 ] || sudo=sudo
  $sudo apt-get update -qq
  DEBIAN_FRONTEND=noninteractive $sudo apt-get install -y -qq "${APT_PACKAGES[@]}"
  command -v "$GO" >/dev/null || DEBIAN_FRONTEND=noninteractive $sudo apt-get install -y -qq golang-go
  install_filebeat "$sudo"
  install_sls_sdk "$sudo"
  command -v gcloud >/dev/null || echo "note: the Google Cloud SDK (gcloud/gsutil/bq) is not on PATH; the GCP CLI test needs it" >&2
}

# Filebeat OSS, pinned and checksum-verified (x86_64 only; other machines set CAMEL_OFFICIAL_FILEBEAT).
install_filebeat() {
  local sudo="$1"
  if [ -x "$FILEBEAT_DIR/filebeat" ] && "$FILEBEAT_DIR/filebeat" version 2>/dev/null | grep -q "version $FILEBEAT_VERSION "; then
    return
  fi
  if [ "$(uname -m)" != x86_64 ]; then
    echo "note: Filebeat is pinned for x86_64 only; set CAMEL_OFFICIAL_FILEBEAT for the lumberjack test" >&2
    return
  fi
  local name="filebeat-oss-$FILEBEAT_VERSION-linux-x86_64.tar.gz" tmp
  tmp="$(mktemp -d)"
  curl -fsSL -o "$tmp/$name" "https://artifacts.elastic.co/downloads/beats/filebeat/$name"
  echo "$FILEBEAT_SHA512_X86_64  $tmp/$name" | sha512sum -c -
  $sudo mkdir -p "$FILEBEAT_DIR"
  $sudo tar -xzf "$tmp/$name" -C "$FILEBEAT_DIR" --strip-components=1
  rm -rf "$tmp"
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
  echo "filebeat:        $("$FILEBEAT" version 2>/dev/null || echo missing)"
  echo "aliyun-log-sdk:  $(sed -n "s/^__version__ = '\(.*\)'/\1/p" "$SLS_SDK_DIR/aliyun/log/version.py" 2>/dev/null || echo missing)"
}

run_tests() {
  cd "$ROOT"
  CAMEL_OFFICIAL_CLIENTS=1 CAMEL_OFFICIAL_PYTHON="$PY" CAMEL_OFFICIAL_GO="$(command -v "$GO")" CAMEL_OFFICIAL_FILEBEAT="$FILEBEAT" CAMEL_OFFICIAL_SLS_SDK="$SLS_SDK_DIR" cargo test --workspace --test 'official_*' "$@"
}

case "${1:-}" in
  install) install ;;
  versions) versions ;;
  test) shift; run_tests "$@" ;;
  all) install; versions; run_tests ;;
  *) sed -n '2,20p' "$0"; exit 1 ;;
esac
