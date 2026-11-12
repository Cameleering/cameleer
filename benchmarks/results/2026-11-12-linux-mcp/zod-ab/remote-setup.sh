#!/usr/bin/env bash
# Runs as root on a fresh Ubuntu 24.04 droplet. Usage: remote-setup.sh server|load
# Same pinned, checksum-verified downloads as scripts/cloud-bench/setup-node.sh.
set -euo pipefail
ROLE="${1:?server|load}"
RUST_VERSION=1.99.1
RUSTUP_SHA256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
NODE_VERSION=22.23.4
NODE_FILE=node-v$NODE_VERSION-linux-x64.tar.xz
NODE_SHA256=1805bcd35e6815559bd2eb97db438d3fdb0eb3a5e6a977b7c1badca76337e9ce
log() { printf '==> [%s] %s\n' "$(hostname)" "$*" >&2; }
fetch() { [ -f "$2" ] || curl -fsSL --proto '=https' --tlsv1.2 -o "$2" "$1"; echo "$4  $2" | "${3}sum" -c - >/dev/null || { rm -f "$2"; echo "checksum mismatch: $2" >&2; exit 1; }; }

cloud-init status --wait >/dev/null 2>&1 || true
systemctl stop unattended-upgrades apt-daily.timer apt-daily-upgrade.timer 2>/dev/null || true
export DEBIAN_FRONTEND=noninteractive
for _ in $(seq 30); do apt-get update -qq && break; sleep 5; done
apt-get install -y -qq build-essential pkg-config curl ca-certificates time sysstat xz-utils >/dev/null
cat > /etc/sysctl.d/90-bench.conf <<'EOF'
net.core.somaxconn = 8192
net.ipv4.tcp_max_syn_backlog = 8192
net.ipv4.ip_local_port_range = 10240 65535
net.ipv4.tcp_tw_reuse = 1
net.core.netdev_max_backlog = 16384
fs.file-max = 2097152
EOF
sysctl -q --system

if [ "$ROLE" = server ]; then
  log "Node.js $NODE_VERSION"
  fetch "https://nodejs.org/dist/v$NODE_VERSION/$NODE_FILE" "/root/$NODE_FILE" sha256 "$NODE_SHA256"
  mkdir -p /opt/node && tar -xJf "/root/$NODE_FILE" -C /opt/node --strip-components=1
  for v in z3 z4; do
    log "npm ci in $v"
    (cd /root/$v && PATH=/opt/node/bin:$PATH npm ci --ignore-scripts --no-audit --no-fund >/root/npm-$v.log 2>&1) || { tail -20 /root/npm-$v.log; exit 1; }
    echo "$v zod=$(PATH=/opt/node/bin:$PATH node -p "require('/root/$v/node_modules/zod/package.json').version")" >&2
  done
else
  log "Rust $RUST_VERSION"
  fetch https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init /tmp/rustup-init sha256 "$RUSTUP_SHA256"
  chmod +x /tmp/rustup-init
  /tmp/rustup-init -y -q --profile minimal --default-toolchain "$RUST_VERSION" >/dev/null
  export PATH=/root/.cargo/bin:$PATH
  log "building the load generator"
  cd /root/cameleer && cargo build -q --release -p cameleer-bench
  ls -la /root/cameleer/target/release/cameleer-bench >&2
fi
log "ready"
