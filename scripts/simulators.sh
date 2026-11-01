#!/usr/bin/env bash
# Third-party simulators for rust-camel.
#
# Installs, starts and stops independent, widely used simulators of cloud services and data
# stores on localhost, so the `sim_*` suites (run with CAMEL_SIMULATORS=1) can exercise
# rust-camel's components against implementations that were not written in this workspace.
#
#   scripts/simulators.sh install    # pip / npm / Go installs (idempotent, needs network)
#   scripts/simulators.sh start      # start every simulator (running ones are left alone)
#   scripts/simulators.sh stop       # stop every simulator started by this script
#   scripts/simulators.sh status     # which ports answer
#   scripts/simulators.sh versions   # simulator versions
#   scripts/simulators.sh test       # CAMEL_SIMULATORS=1 cargo test --workspace --test 'sim_*'
#   scripts/simulators.sh all        # install + start
#
# Tools are installed under $CAMEL_SIM_HOME (default /opt/camel-sim), state and logs live in
# $CAMEL_SIM_HOME/state. TLS listeners use the interop PKI of scripts/live-interop.sh
# ($CAMEL_INTEROP_DIR/certs, generated here if missing). Ports (all on 127.0.0.1):
#
#   moto (AWS)            15000  HTTP, all services          15001 HTTPS (interop certificate)
#                         15002  HTTP with IAM authentication + SigV4 verification enforced
#                                (credentials written to state/moto-iam.env by `start`)
#   Azurite (Azure)       20000 / 20001 / 20002  Blob / Queue / Table (HTTP, Shared Key + SAS)
#                         20010 / 20011 / 20012  Blob / Queue / Table (HTTPS, + OAuth "basic")
#   fake-gcs-server (GCS) 14443  HTTP JSON + XML API         14444 HTTPS
#   pstest (Pub/Sub)      18085  gRPC (plaintext)
#   FerretDB (MongoDB)    27117  wire protocol               27118 TLS
#                                backed by the live-interop PostgreSQL 16 (127.0.0.1:15432,
#                                database `ferretdb`), SCRAM-SHA-256/SHA-1 user camel/camelpw
#   kfake (Kafka)         19092,19093,19094  three brokers, plaintext
#                         19095  SASL PLAIN / SCRAM-SHA-256 / SCRAM-SHA-512 (camel/camelpw)
#                         19096  TLS                          19097 SASL_SSL
#
# Not available offline (see docs/INTEROP.md, "Third-party simulators"): Apache Kafka, Redpanda,
# Elasticsearch / OpenSearch, Cassandra (tarballs > the 10 MB download limit, Apache / vendor
# download hosts blocked), LocalStack (needs Docker), the Cosmos DB / Service Bus emulators
# (Windows / Docker only), the gcloud Pub/Sub / Firestore emulators (dl.google.com blocked).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
HOME_DIR="${CAMEL_SIM_HOME:-/opt/camel-sim}"
STATE="$HOME_DIR/state"
LOGS="$STATE/logs"
RUN="$STATE/run"
BIN="$HOME_DIR/bin"
VENV="$HOME_DIR/venv"
INTEROP="${CAMEL_INTEROP_DIR:-/var/lib/camel-interop}"
CERTS="$INTEROP/certs"
PW=camelpw
GO="${CAMEL_SIM_GO:-/usr/local/go/bin/go}"
[ -x "$GO" ] || GO=go

MOTO_VERSION=5.2.5
AZURITE_VERSION=3.37.0
FAKE_GCS_VERSION=v1.53.1      # the last release that builds with Go 1.24
PUBSUB_VERSION=v1.50.1        # cloud.google.com/go/pubsub (pstest)
FERRETDB_VERSION=v1.24.2      # FerretDB 1.x runs on plain PostgreSQL (2.x needs DocumentDB)
KFAKE_VERSION=v0.0.0-20251220215110-24b7a27738c1   # franz-go v1.20.6
GH=github.com

log() { echo "[simulators] $*"; }
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
wait_port() {
  local port=$1 name=$2 i
  for i in $(seq 1 120); do
    if port_open "$port"; then return 0; fi
    sleep 0.5
  done
  echo "$name did not open port $port (see $LOGS)" >&2
  return 1
}

# ---------------------------------------------------------------------------------------------
# install

# Go builds. sum.golang.org is not reachable, and proxy.golang.org redirects large module zips
# to storage.googleapis.com, which is blocked. So programs are built inside their own module
# (every dependency checked against the upstream go.sum), GitHub-hosted modules come straight
# from git, and google.golang.org/api is packed from its git tag by scripts/simulators/modzip
# into a file GOPROXY - the go command accepts that zip only if its hash matches go.sum.
go_env() {
  export GOTOOLCHAIN=local GONOPROXY=$GH GOPROXY="file://$HOME_DIR/goproxy,https://proxy.golang.org"
}

build_modzip() {
  [ -x "$BIN/modzip" ] && return 0
  mkdir -p "$HOME_DIR/modzip"
  cp "$ROOT/scripts/simulators/modzip/main.go" "$HOME_DIR/modzip/"
  printf 'module camel-sim/modzip\n\ngo 1.24.0\n' > "$HOME_DIR/modzip/go.mod"
  (cd "$HOME_DIR/modzip" && go_env && GOFLAGS=-mod=mod GONOSUMDB=golang.org/x/mod "$GO" get golang.org/x/mod@v0.24.0 \
    && "$GO" build -o "$BIN/modzip" .)
}

# google.golang.org/api <version>: fetch the git tag (through the go command) and pack it.
pack_google_api() {
  local ver=$1 out="$HOME_DIR/goproxy/google.golang.org/api/@v"
  [ -f "$out/$ver.zip" ] && return 0
  build_modzip
  local info hash repo
  info=$(cd /tmp && go_env && GOFLAGS=-mod=mod GONOSUMDB=$GH "$GO" mod download -json "$GH/googleapis/google-api-go-client@$ver" 2>/dev/null || true)
  hash=$(echo "$info" | sed -n 's/.*"Hash": "\([0-9a-f]*\)".*/\1/p' | head -1)
  repo=$(grep -l "google-api-go-client" "$("$GO" env GOMODCACHE)"/cache/vcs/*.info 2>/dev/null | head -1)
  repo=${repo%.info}
  [ -n "$hash" ] && [ -d "$repo" ] || { log "could not fetch google-api-go-client $ver"; return 1; }
  "$BIN/modzip" "$repo" "$hash" google.golang.org/api "$ver" "$HOME_DIR/goproxy" >/dev/null
  curl -sSf -o "$out/$ver.mod" "https://proxy.golang.org/google.golang.org/api/@v/$ver.mod"
}

# go_module_src <module> <version> <dest>: the module's source tree, writable.
go_module_src() {
  local dir
  dir=$(cd /tmp && go_env && GOFLAGS=-mod=mod GONOSUMDB="$1" "$GO" mod download -json "$1@$2" | sed -n 's/.*"Dir": "\(.*\)",/\1/p')
  rm -rf "$3"; cp -r "$dir" "$3"; chmod -R u+w "$3"
}

install_moto() {
  if [ -x "$VENV/bin/moto_server" ]; then return 0; fi
  log "pip install moto[server]==$MOTO_VERSION"
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install -q "moto[server]==$MOTO_VERSION"
}

install_azurite() {
  command -v azurite >/dev/null && return 0
  log "npm install -g azurite@$AZURITE_VERSION"
  npm install -g "azurite@$AZURITE_VERSION"
}

install_fake_gcs() {
  [ -x "$BIN/fake-gcs-server" ] && return 0
  log "building fake-gcs-server $FAKE_GCS_VERSION"
  pack_google_api v0.264.0
  local src="$HOME_DIR/src/fake-gcs-server"
  go_module_src "$GH/fsouza/fake-gcs-server" "$FAKE_GCS_VERSION" "$src"
  (cd "$src" && go_env && GOFLAGS=-mod=readonly "$GO" build -trimpath -o "$BIN/fake-gcs-server" .)
}

install_pstest() {
  [ -x "$BIN/pstest-server" ] && return 0
  log "building pstest (cloud.google.com/go/pubsub $PUBSUB_VERSION)"
  pack_google_api v0.247.0
  local src="$HOME_DIR/pstest"
  mkdir -p "$src"
  cp "$ROOT/scripts/simulators/pstest/main.go" "$src/"
  printf 'module camel-sim/pstest\n\ngo 1.24.0\n' > "$src/go.mod"
  local pubsub
  pubsub=$(cd /tmp && go_env && GOFLAGS=-mod=mod GONOSUMDB=cloud.google.com/go/pubsub "$GO" mod download -json "cloud.google.com/go/pubsub@$PUBSUB_VERSION" | sed -n 's/.*"Dir": "\(.*\)",/\1/p')
  cp "$pubsub/go.sum" "$src/go.sum"   # dependency hashes as pinned by the pubsub module
  (cd "$src" && go_env && GOFLAGS=-mod=mod GONOSUMDB=cloud.google.com/go/pubsub "$GO" get "cloud.google.com/go/pubsub/pstest@$PUBSUB_VERSION" \
    && GOFLAGS=-mod=readonly "$GO" build -trimpath -o "$BIN/pstest-server" .)
}

install_ferretdb() {
  [ -x "$BIN/ferretdb" ] && return 0
  log "building FerretDB $FERRETDB_VERSION (PostgreSQL backend only)"
  local src="$HOME_DIR/src/ferretdb"
  go_module_src "$GH/FerretDB/FerretDB" "$FERRETDB_VERSION" "$src"
  echo "$FERRETDB_VERSION" > "$src/build/version/version.txt"
  # the SQLite backend needs modernc.org/sqlite (GitLab, blocked); FerretDB's own build tags drop it
  (cd "$src" && go_env && GOFLAGS=-mod=readonly "$GO" build -trimpath \
    -tags ferretdb_no_sqlite,ferretdb_no_hana,ferretdb_no_mysql -o "$BIN/ferretdb" ./cmd/ferretdb)
}

install_kfake() {
  [ -x "$BIN/kfake-server" ] && return 0
  log "building kfake (franz-go) $KFAKE_VERSION"
  local src="$HOME_DIR/kfake" mod="$GH/twmb/franz-go/pkg/kfake"
  mkdir -p "$src"
  cp "$ROOT/scripts/simulators/kfake/main.go" "$ROOT/scripts/simulators/kfake/go.mod" "$src/"
  local kdir
  kdir=$(cd /tmp && go_env && GOFLAGS=-mod=mod GONOSUMDB=$mod "$GO" mod download -json "$mod@$KFAKE_VERSION" | sed -n 's/.*"Dir": "\(.*\)",/\1/p')
  cp "$kdir/go.sum" "$src/go.sum"     # dependency hashes as pinned by kfake
  (cd "$src" && go_env && GOFLAGS=-mod=mod GONOSUMDB=$mod "$GO" mod tidy \
    && GOFLAGS=-mod=readonly "$GO" build -trimpath -o "$BIN/kfake-server" .)
}

cmd_install() {
  mkdir -p "$BIN" "$STATE"
  local f
  for f in install_moto install_azurite install_fake_gcs install_pstest install_ferretdb install_kfake; do
    $f || log "WARNING: $f failed"
  done
}

# ---------------------------------------------------------------------------------------------
# start / stop

gen_certs() {
  [ -f "$CERTS/server.pem" ] && [ -f "$CERTS/client.pem" ] && return 0
  if [ -x "$ROOT/scripts/live-interop.sh" ] && [ "$(id -u)" = 0 ]; then
    log "no interop PKI in $CERTS: running scripts/live-interop.sh setup"
    "$ROOT/scripts/live-interop.sh" setup >/dev/null
  else
    echo "no interop PKI in $CERTS: run scripts/live-interop.sh setup first" >&2
    exit 1
  fi
}

# spawn <name> <port> <command...>: start in the background unless the port answers.
spawn() {
  local name=$1 port=$2; shift 2
  port_open "$port" && return 0
  log "starting $name"
  nohup "$@" > "$LOGS/$name.log" 2>&1 &
  echo $! > "$RUN/$name.pid"
  wait_port "$port" "$name"
}

start_moto() {
  [ -x "$VENV/bin/moto_server" ] || return 0
  spawn moto 15000 "$VENV/bin/moto_server" -H 127.0.0.1 -p 15000
  spawn moto-tls 15001 "$VENV/bin/moto_server" -H 127.0.0.1 -p 15001 -c "$CERTS/server.pem" -k "$CERTS/server.key"
  if ! port_open 15002; then
    # IAM authentication: the first 4 actions are free (create user, key, policy, attach);
    # every later request needs a key that exists in moto's IAM and a valid SigV4 signature.
    rm -f "$STATE/moto-iam.env"
    INITIAL_NO_AUTH_ACTION_COUNT=4 spawn moto-iam 15002 "$VENV/bin/moto_server" -H 127.0.0.1 -p 15002
    "$VENV/bin/python" -I - "$STATE/moto-iam.env" <<'EOF'
import json, sys, boto3
iam = boto3.client("iam", endpoint_url="http://127.0.0.1:15002", region_name="us-east-1",
                   aws_access_key_id="bootstrap", aws_secret_access_key="bootstrap")
iam.create_user(UserName="camel")
key = iam.create_access_key(UserName="camel")["AccessKey"]
doc = {"Version": "2012-10-17", "Statement": [{"Effect": "Allow", "Action": "*", "Resource": "*"}]}
arn = iam.create_policy(PolicyName="camel-all", PolicyDocument=json.dumps(doc))["Policy"]["Arn"]
iam.attach_user_policy(UserName="camel", PolicyArn=arn)
with open(sys.argv[1], "w") as f:
    f.write("access_key=%s\nsecret_key=%s\n" % (key["AccessKeyId"], key["SecretAccessKey"]))
EOF
  fi
}

start_azurite() {
  command -v azurite >/dev/null || return 0
  mkdir -p "$STATE/azurite" "$STATE/azurite-tls"
  spawn azurite 20000 azurite --silent --skipApiVersionCheck --disableTelemetry --location "$STATE/azurite" \
    --blobPort 20000 --queuePort 20001 --tablePort 20002 --debug "$LOGS/azurite-debug.log"
  spawn azurite-tls 20010 azurite --silent --skipApiVersionCheck --disableTelemetry --location "$STATE/azurite-tls" \
    --blobPort 20010 --queuePort 20011 --tablePort 20012 --cert "$CERTS/server.pem" --key "$CERTS/server.key" --oauth basic
}

start_gcs() {
  [ -x "$BIN/fake-gcs-server" ] || return 0
  spawn fake-gcs 14443 "$BIN/fake-gcs-server" -host 127.0.0.1 -port 14443 -scheme http -backend memory \
    -external-url http://127.0.0.1:14443 -public-host 127.0.0.1:14443
  spawn fake-gcs-tls 14444 "$BIN/fake-gcs-server" -host 127.0.0.1 -port 14444 -scheme https -backend memory \
    -cert-location "$CERTS/server.pem" -private-key-location "$CERTS/server.key" \
    -external-url https://localhost:14444 -public-host localhost:14444
}

start_pstest() {
  [ -x "$BIN/pstest-server" ] || return 0
  spawn pstest 18085 "$BIN/pstest-server" -port 18085
}

start_ferretdb() {
  [ -x "$BIN/ferretdb" ] || return 0
  if ! port_open 15432; then
    log "PostgreSQL (scripts/live-interop.sh, port 15432) is not running: FerretDB skipped"
    return 0
  fi
  PGPASSWORD=$PW psql -h 127.0.0.1 -p 15432 -U camel -d camel -qtAc "SELECT 1 FROM pg_database WHERE datname = 'ferretdb'" | grep -q 1 \
    || PGPASSWORD=$PW psql -h 127.0.0.1 -p 15432 -U camel -d camel -qc "CREATE DATABASE ferretdb" >/dev/null
  mkdir -p "$STATE/ferretdb"
  spawn ferretdb 27117 "$BIN/ferretdb" --state-dir="$STATE/ferretdb" --telemetry=disable \
    --listen-addr=127.0.0.1:27117 --listen-tls=127.0.0.1:27118 \
    --listen-tls-cert-file="$CERTS/server.pem" --listen-tls-key-file="$CERTS/server.key" \
    --postgresql-url="postgres://camel:$PW@127.0.0.1:15432/ferretdb" --debug-addr=127.0.0.1:27119 \
    --test-enable-new-auth --setup-database=admin --setup-username=camel --setup-password="$PW"
}

start_kfake() {
  [ -x "$BIN/kfake-server" ] || return 0
  spawn kfake 19092 "$BIN/kfake-server" -ports 19092,19093,19094
  spawn kfake-sasl 19095 "$BIN/kfake-server" -ports 19095 -sasl "camel:$PW"
  spawn kfake-tls 19096 "$BIN/kfake-server" -ports 19096 -tls-cert "$CERTS/server.pem" -tls-key "$CERTS/server.key"
  spawn kfake-sasl-ssl 19097 "$BIN/kfake-server" -ports 19097 -sasl "camel:$PW" -tls-cert "$CERTS/server.pem" -tls-key "$CERTS/server.key"
}

cmd_start() {
  mkdir -p "$LOGS" "$RUN"
  gen_certs
  start_moto
  start_azurite
  start_gcs
  start_pstest
  start_ferretdb
  start_kfake
  cmd_status
}

cmd_stop() {
  local f
  for f in "$RUN"/*.pid; do
    [ -f "$f" ] || continue
    kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
  sleep 1
  log "stopped"
}

cmd_status() {
  local name port
  for entry in moto:15000 moto-tls:15001 moto-iam:15002 azurite-blob:20000 azurite-queue:20001 \
    azurite-table:20002 azurite-blob-tls:20010 azurite-queue-tls:20011 azurite-table-tls:20012 \
    fake-gcs:14443 fake-gcs-tls:14444 pstest:18085 ferretdb:27117 ferretdb-tls:27118 \
    kfake:19092 kfake-sasl:19095 kfake-tls:19096 kfake-sasl-ssl:19097; do
    name=${entry%%:*}; port=${entry##*:}
    if port_open "$port"; then echo "  up    $name ($port)"; else echo "  DOWN  $name ($port)"; fi
  done
}

cmd_versions() {
  echo "moto:            $("$VENV/bin/pip" show moto 2>/dev/null | sed -n 's/^Version: //p' || echo missing)"
  echo "azurite:         $(azurite --version 2>/dev/null || echo missing)"
  echo "fake-gcs-server: $FAKE_GCS_VERSION $( [ -x "$BIN/fake-gcs-server" ] || echo '(missing)')"
  echo "pstest:          cloud.google.com/go/pubsub $PUBSUB_VERSION $( [ -x "$BIN/pstest-server" ] || echo '(missing)')"
  echo "ferretdb:        $("$BIN/ferretdb" --version 2>/dev/null | sed -n 's/^version: //p' || echo missing)"
  echo "kfake:           franz-go/pkg/kfake $KFAKE_VERSION $( [ -x "$BIN/kfake-server" ] || echo '(missing)')"
  echo "go:              $("$GO" version 2>/dev/null || echo missing)"
}

run_tests() {
  cd "$ROOT"
  CAMEL_SIMULATORS=1 cargo test --workspace --test 'sim_*' "$@"
}

case "${1:-}" in
  install) cmd_install ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  versions) cmd_versions ;;
  test) shift; run_tests "$@" ;;
  all) cmd_install; cmd_start ;;
  *) sed -n '2,40p' "$0"; exit 1 ;;
esac
