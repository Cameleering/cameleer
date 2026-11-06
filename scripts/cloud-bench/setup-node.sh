#!/usr/bin/env bash
# Prepare one Ubuntu 24.04 droplet of the cloud benchmark. Runs as root on the droplet, from the
# unpacked source tree (/root/cameleer).
#
#   setup-node.sh server              build the benchmark sides (Cameleer, Apache Camel, Camel Quarkus native)
#   setup-node.sh load <private-ip>   build the load generator, start Kafka and PostgreSQL
#
# Every download is pinned and checksum-verified. Both servers get the same OS settings.
set -euo pipefail

ROLE="${1:?server|load}"
SRC=/root/cameleer
RUST_VERSION=1.99.1
RUSTUP_SHA256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
KAFKA_VERSION=4.3.2
KAFKA_FILE=kafka_2.13-$KAFKA_VERSION.tgz
KAFKA_SHA512=c2063485f90312bba795052df3e49b8eea337adf97f1111b946c60771c7a2f088055d2ccc7221d72fe30bd6a3f7a58348f14e14240eaddfe60446ea1c033618b
# the GraalVM release Quarkus 3.40 is built against (benchmarks/camel-quarkus)
MANDREL_VERSION=25.0.4.1-Final
MANDREL_FILE=mandrel-java25-linux-amd64-$MANDREL_VERSION.tar.gz
MANDREL_SHA256=e47ff6bfe6a8dbb482fdc65c9a49fc6f3ba2fa61ef3cc8fb6229a88989054c31

log() { printf '==> [%s] %s\n' "$(hostname)" "$*" >&2; }

fetch() { # fetch URL FILE ALGO SUM
  [ -f "$2" ] || curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"
  echo "$4  $2" | "${3}sum" -c - >/dev/null || { rm -f "$2"; echo "checksum mismatch: $2" >&2; exit 1; }
}

base() {
  log "waiting for cloud-init and apt"
  cloud-init status --wait >/dev/null 2>&1 || true
  systemctl stop unattended-upgrades apt-daily.timer apt-daily-upgrade.timer 2>/dev/null || true
  export DEBIAN_FRONTEND=noninteractive
  for _ in $(seq 30); do apt-get update -qq && break; sleep 5; done
  apt-get install -y -qq build-essential pkg-config curl ca-certificates time sysstat unzip >/dev/null
  log "kernel settings (same on every node)"
  cat > /etc/sysctl.d/90-bench.conf <<'EOF'
net.core.somaxconn = 8192
net.ipv4.tcp_max_syn_backlog = 8192
net.ipv4.ip_local_port_range = 10240 65535
net.ipv4.tcp_tw_reuse = 1
net.core.netdev_max_backlog = 16384
fs.file-max = 2097152
EOF
  sysctl -q --system
  log "Rust $RUST_VERSION"
  if [ ! -x /root/.cargo/bin/cargo ]; then
    fetch https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init /tmp/rustup-init sha256 "$RUSTUP_SHA256"
    chmod +x /tmp/rustup-init
    /tmp/rustup-init -y -q --profile minimal --default-toolchain "$RUST_VERSION" >/dev/null
  fi
  export PATH=/root/.cargo/bin:$PATH
  rustc -V >&2
}

server() {
  base
  cd "$SRC"
  log "building Cameleer and Apache Camel on the JVM (scripts/bench.sh build)"
  BENCH_TOOLS=/root/.cache/cameleer-bench scripts/bench.sh build >/root/build.log 2>&1 || { tail -40 /root/build.log; exit 1; }
  log "Mandrel $MANDREL_VERSION and the Camel Quarkus native executable"
  apt-get install -y -qq zlib1g-dev >/dev/null
  fetch "https://github.com/graalvm/mandrel/releases/download/mandrel-$MANDREL_VERSION/$MANDREL_FILE" "/root/$MANDREL_FILE" sha256 "$MANDREL_SHA256"
  [ -x /opt/mandrel/bin/native-image ] || { mkdir -p /opt/mandrel && tar -xzf "/root/$MANDREL_FILE" -C /opt/mandrel --strip-components=1; }
  local tools=/root/.cache/cameleer-bench
  JAVA_HOME="$(ls -d $tools/jdk-*)" "$(ls -d $tools/apache-maven-*)/bin/mvn" -C -B -q -f benchmarks/camel-quarkus/pom.xml package -Dnative \
    -Dquarkus.native.graalvm-home=/opt/mandrel -Dquarkus.native.native-image-xmx=6g >/root/native.log 2>&1 || { tail -40 /root/native.log; exit 1; }
  ls -la target/release/cameleer-bench benchmarks/java-camel/target/classes/bench/Bench.class benchmarks/camel-quarkus/target/*-runner >&2
}

load() {
  local ip="${2:?private ip}"
  base
  cd "$SRC"
  log "building the load generator"
  cargo build -q --release -p cameleer-bench
  BENCH_TOOLS=/root/.cache/cameleer-bench scripts/bench.sh tools >/dev/null
  local java_home; java_home="$(ls -d /root/.cache/cameleer-bench/jdk-*)"

  log "PostgreSQL"
  apt-get install -y -qq postgresql >/dev/null
  local conf; conf="$(ls -d /etc/postgresql/*/main)"
  sed -i "s/^#\?listen_addresses.*/listen_addresses = '*'/" "$conf/postgresql.conf"
  grep -q 'bench-net' "$conf/pg_hba.conf" || echo "host bench camel 10.0.0.0/8 scram-sha-256 # bench-net" >> "$conf/pg_hba.conf"
  systemctl restart postgresql
  sudo -u postgres psql -q -v ON_ERROR_STOP=1 <<'EOF'
DO $$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'camel') THEN CREATE ROLE camel LOGIN PASSWORD 'camelpw'; END IF;
END $$;
SELECT 'CREATE DATABASE bench OWNER camel' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'bench')\gexec
EOF
  PGPASSWORD=camelpw psql -q -h 127.0.0.1 -U camel -d bench -c \
    "CREATE TABLE IF NOT EXISTS orders (id bigserial PRIMARY KEY, order_id bigint NOT NULL, customer text NOT NULL, side text NOT NULL, created timestamptz NOT NULL DEFAULT now())"

  log "Kafka $KAFKA_VERSION (KRaft, one broker)"
  fetch "https://archive.apache.org/dist/kafka/$KAFKA_VERSION/$KAFKA_FILE" "/root/$KAFKA_FILE" sha512 "$KAFKA_SHA512"
  [ -d /opt/kafka ] || { tar -xzf "/root/$KAFKA_FILE" -C /opt && mv "/opt/kafka_2.13-$KAFKA_VERSION" /opt/kafka; }
  cat > /opt/kafka/bench.properties <<EOF
process.roles=broker,controller
node.id=1
controller.quorum.voters=1@127.0.0.1:9093
listeners=PLAINTEXT://0.0.0.0:9092,CONTROLLER://127.0.0.1:9093
advertised.listeners=PLAINTEXT://$ip:9092
controller.listener.names=CONTROLLER
listener.security.protocol.map=PLAINTEXT:PLAINTEXT,CONTROLLER:PLAINTEXT
log.dirs=/var/lib/kafka
num.partitions=4
offsets.topic.replication.factor=1
transaction.state.log.replication.factor=1
transaction.state.log.min.isr=1
group.initial.rebalance.delay.ms=0
EOF
  if [ ! -f /var/lib/kafka/meta.properties ]; then
    JAVA_HOME="$java_home" /opt/kafka/bin/kafka-storage.sh format -t "$(JAVA_HOME="$java_home" /opt/kafka/bin/kafka-storage.sh random-uuid)" \
      -c /opt/kafka/bench.properties >/dev/null
  fi
  if ! (exec 3<>/dev/tcp/127.0.0.1/9092) 2>/dev/null; then
    JAVA_HOME="$java_home" KAFKA_HEAP_OPTS="-Xms2g -Xmx2g" nohup /opt/kafka/bin/kafka-server-start.sh /opt/kafka/bench.properties >/root/kafka.log 2>&1 &
    for _ in $(seq 60); do (exec 3<>/dev/tcp/127.0.0.1/9092) 2>/dev/null && break; sleep 1; done
  fi
  local kt="/opt/kafka/bin/kafka-topics.sh --bootstrap-server 127.0.0.1:9092"
  for t in orders-rust orders-java orders-native stream-in stream-out-rust stream-out-java stream-out-native; do
    JAVA_HOME="$java_home" $kt --create --if-not-exists --topic "$t" --partitions 4 --replication-factor 1 >/dev/null
  done
  JAVA_HOME="$java_home" $kt --list >&2
}

case "$ROLE" in
  server) server ;;
  load) load "$@" ;;
  *) echo "usage: $0 server|load <ip>" >&2; exit 1 ;;
esac
log "done"
