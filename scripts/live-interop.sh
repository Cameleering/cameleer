#!/usr/bin/env bash
# Live interop servers for rust-camel.
#
# Installs, configures, starts and stops real, independent server software on localhost so the
# `live_*` test suites (run with CAMEL_LIVE_INTEROP=1) can exercise rust-camel against them.
#
#   scripts/live-interop.sh install   # apt-get install the packages (root, idempotent)
#   scripts/live-interop.sh setup     # certificates, users, configuration files (idempotent)
#   scripts/live-interop.sh start     # start every server (already running ones are left alone)
#   scripts/live-interop.sh stop      # stop every server started by this script
#   scripts/live-interop.sh status    # show which ports answer
#   scripts/live-interop.sh versions  # print server versions
#   scripts/live-interop.sh all       # install + setup + start
#
# Then: CAMEL_LIVE_INTEROP=1 cargo test --workspace live_
#
# Everything lives under $CAMEL_INTEROP_DIR (default /var/lib/camel-interop); the tests read the
# same variable (and the same defaults) to find the CA certificate. Ports (all on 127.0.0.1):
#
#   PostgreSQL 16   15432  (SCRAM-SHA-256, TLS available: sslmode=require/verify-full)
#   Redis           16379  (requirepass)          16380 TLS
#   Mosquitto       11883  anonymous   11884 username/password   18883 TLS + username/password
#   RabbitMQ        15673  AMQP 0-9-1 + AMQP 1.0  15674 AMQPS (0-9-1 + 1.0)
#                   16613  STOMP       16614 STOMP over TLS     11885 MQTT
#   vsftpd          2121   FTP + explicit FTPS (AUTH TLS), passive ports 30000-30049
#                   2990   implicit FTPS
#                   2991   explicit FTPS with require_ssl_reuse=YES
#   Postfix         2525   SMTP (+STARTTLS, AUTH via Dovecot SASL)   4465 SMTPS (implicit TLS)
#   Dovecot         11143  IMAP (+STARTTLS)  11993 IMAPS  11110 POP3 (+STLS)  11995 POP3S
#   nginx           18080  HTTP/1.1 (gzip)   18443 HTTPS with HTTP/2 (ALPN h2, http/1.1)
#
# Credentials: user `camel` / password `camelpw` everywhere (system users `camelftp` and
# `camelmail` for FTP and mail, same password; mail address camelmail@localhost).
set -euo pipefail

DIR="${CAMEL_INTEROP_DIR:-/var/lib/camel-interop}"
CERTS="$DIR/certs"
LOGS="$DIR/logs"
RUN="$DIR/run"
PW=camelpw
PGBIN=/usr/lib/postgresql/16/bin

PACKAGES=(postgresql redis-server mosquitto mosquitto-clients rabbitmq-server vsftpd
  dovecot-imapd dovecot-pop3d postfix nginx libnginx-mod-http-echo openssl)

log() { echo "[live-interop] $*"; }

need_root() {
  if [ "$(id -u)" != 0 ]; then echo "must run as root" >&2; exit 1; fi
}

port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

wait_port() {
  local port=$1 name=$2 i
  for i in $(seq 1 120); do
    if port_open "$port"; then return 0; fi
    sleep 0.5
  done
  echo "$name did not open port $port" >&2
  return 1
}

cmd_install() {
  need_root
  local missing=()
  for p in "${PACKAGES[@]}"; do
    dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p")
  done
  if [ ${#missing[@]} -eq 0 ]; then log "all packages installed"; return; fi
  export DEBIAN_FRONTEND=noninteractive
  printf '%s\n' "postfix postfix/main_mailer_type select Local only" "postfix postfix/mailname string localhost" \
    | debconf-set-selections
  apt-get update -q
  # Packages that cannot be installed are reported and skipped.
  for p in "${missing[@]}"; do
    apt-get install -y -q "$p" || log "WARNING: could not install $p"
  done
}

ensure_user() {
  local user=$1
  if ! id "$user" >/dev/null 2>&1; then
    useradd -m -s /bin/bash "$user"
  fi
  echo "$user:$PW" | chpasswd
}

gen_certs() {
  mkdir -p "$CERTS"
  if [ -f "$CERTS/server.pem" ] && [ -f "$CERTS/client.pem" ]; then return; fi
  log "generating test PKI in $CERTS"
  cd "$CERTS"
  openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=rust-camel interop CA" \
    -keyout ca.key -out ca.pem -addext "basicConstraints=critical,CA:TRUE" \
    -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout server.key -out server.csr 2>/dev/null
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nbasicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\n' > server.ext
  openssl x509 -req -in server.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 3650 \
    -extfile server.ext -out server.pem 2>/dev/null
  openssl req -newkey rsa:2048 -nodes -subj "/CN=camel" -keyout client.key -out client.csr 2>/dev/null
  printf 'basicConstraints=CA:FALSE\nkeyUsage=digitalSignature\nextendedKeyUsage=clientAuth\n' > client.ext
  openssl x509 -req -in client.csr -CA ca.pem -CAkey ca.key -CAcreateserial -days 3650 \
    -extfile client.ext -out client.pem 2>/dev/null
  cat server.pem server.key > server-combined.pem
  rm -f ./*.csr ./*.ext
  chmod 644 ./*.pem ./*.key
}

# A copy of the server key owned by a service account (some servers insist on strict modes).
key_copy() {
  local owner=$1 dest="$CERTS/server-$1.key"
  cp "$CERTS/server.key" "$dest"
  chown "$owner" "$dest"
  chmod 600 "$dest"
  echo "$dest"
}

setup_postgres() {
  [ -x "$PGBIN/initdb" ] || { log "postgresql 16 not installed, skipping"; return; }
  local data="$DIR/pgdata"
  if [ ! -f "$data/PG_VERSION" ]; then
    log "initdb $data"
    mkdir -p "$data"; chown postgres:postgres "$data"; chmod 700 "$data"
    echo "$PW" > "$DIR/pgpass.tmp"; chown postgres "$DIR/pgpass.tmp"
    su postgres -c "$PGBIN/initdb -D $data -U postgres --auth=scram-sha-256 --pwfile=$DIR/pgpass.tmp -E UTF8 --locale=C.UTF-8" >/dev/null
    rm -f "$DIR/pgpass.tmp"
  fi
  local key; key=$(key_copy postgres)
  cat > "$data/postgresql.auto.conf" <<EOF
listen_addresses = '127.0.0.1'
port = 15432
unix_socket_directories = '$RUN'
password_encryption = 'scram-sha-256'
ssl = on
ssl_cert_file = '$CERTS/server.pem'
ssl_key_file = '$key'
ssl_ca_file = '$CERTS/ca.pem'
max_connections = 200
max_prepared_transactions = 10
EOF
  cat > "$data/pg_hba.conf" <<EOF
local all all scram-sha-256
host  all all 127.0.0.1/32 scram-sha-256
host  all all ::1/128 scram-sha-256
EOF
  chown postgres:postgres "$data/postgresql.auto.conf" "$data/pg_hba.conf"
}

setup_redis() {
  cat > "$DIR/redis.conf" <<EOF
bind 127.0.0.1
port 16379
tls-port 16380
tls-cert-file $CERTS/server.pem
tls-key-file $CERTS/server.key
tls-ca-cert-file $CERTS/ca.pem
tls-auth-clients no
requirepass $PW
daemonize yes
pidfile $RUN/redis.pid
logfile $LOGS/redis.log
dir $DIR
save ""
appendonly no
EOF
}

setup_mosquitto() {
  command -v mosquitto >/dev/null || { log "mosquitto not installed, skipping"; return; }
  rm -f "$DIR/mosquitto.passwd"
  touch "$DIR/mosquitto.passwd"; chmod 600 "$DIR/mosquitto.passwd"
  mosquitto_passwd -b "$DIR/mosquitto.passwd" camel "$PW"
  chown mosquitto "$DIR/mosquitto.passwd"
  local key; key=$(key_copy mosquitto)
  # Two instances: `per_listener_settings true` (needed to mix anonymous and authenticated
  # listeners in one broker) makes mosquitto 2.0 drop messages queued for offline persistent
  # sessions, so the anonymous broker and the authenticated (plain + TLS) broker are separate.
  cat > "$DIR/mosquitto.conf" <<EOF
pid_file $RUN/mosquitto.pid
persistence false
log_dest file $LOGS/mosquitto.log
user mosquitto
listener 11883 127.0.0.1
allow_anonymous true
EOF
  cat > "$DIR/mosquitto-auth.conf" <<EOF
pid_file $RUN/mosquitto-auth.pid
persistence false
log_dest file $LOGS/mosquitto-auth.log
user mosquitto
allow_anonymous false
password_file $DIR/mosquitto.passwd

listener 11884 127.0.0.1

listener 18883 127.0.0.1
certfile $CERTS/server.pem
keyfile $key
cafile $CERTS/ca.pem
EOF
}

setup_rabbitmq() {
  command -v rabbitmq-server >/dev/null || { log "rabbitmq not installed, skipping"; return; }
  mkdir -p "$DIR/rabbitmq"; chown rabbitmq:rabbitmq "$DIR/rabbitmq"
  local key; key=$(key_copy rabbitmq)
  cat > "$DIR/rabbitmq/rabbitmq.conf" <<EOF
listeners.tcp.default = 127.0.0.1:15673
listeners.ssl.default = 127.0.0.1:15674
ssl_options.cacertfile = $CERTS/ca.pem
ssl_options.certfile = $CERTS/server.pem
ssl_options.keyfile = $key
ssl_options.verify = verify_none
ssl_options.fail_if_no_peer_cert = false
stomp.listeners.tcp.1 = 127.0.0.1:16613
stomp.listeners.ssl.1 = 127.0.0.1:16614
mqtt.listeners.tcp.default = 127.0.0.1:11885
loopback_users = none
default_user = camel
default_pass = $PW
default_vhost = /
default_permissions.configure = .*
default_permissions.read = .*
default_permissions.write = .*
default_user_tags.administrator = true
log.file = $LOGS/rabbitmq.log
log.console = false
EOF
  echo "[rabbitmq_stomp,rabbitmq_amqp1_0,rabbitmq_mqtt]." > "$DIR/rabbitmq/enabled_plugins"
  cat > "$DIR/rabbitmq/env" <<EOF
export RABBITMQ_CONFIG_FILE=$DIR/rabbitmq/rabbitmq.conf
export RABBITMQ_ENABLED_PLUGINS_FILE=$DIR/rabbitmq/enabled_plugins
export RABBITMQ_MNESIA_BASE=$DIR/rabbitmq/mnesia
export RABBITMQ_LOG_BASE=$LOGS
export RABBITMQ_NODENAME=camel-interop@localhost
export RABBITMQ_NODE_PORT=15673
export RABBITMQ_DIST_PORT=25673
export RABBITMQ_PID_FILE=$RUN/rabbitmq.pid
export ERL_EPMD_ADDRESS=127.0.0.1
export HOME=$DIR/rabbitmq
EOF
  chown -R rabbitmq:rabbitmq "$DIR/rabbitmq"
  if [ -f /var/lib/rabbitmq/.erlang.cookie ] && [ ! -f "$DIR/rabbitmq/.erlang.cookie" ]; then
    cp /var/lib/rabbitmq/.erlang.cookie "$DIR/rabbitmq/.erlang.cookie"
    chown rabbitmq:rabbitmq "$DIR/rabbitmq/.erlang.cookie"; chmod 400 "$DIR/rabbitmq/.erlang.cookie"
  fi
}

setup_vsftpd() {
  command -v vsftpd >/dev/null || { log "vsftpd not installed, skipping"; return; }
  ensure_user camelftp
  mkdir -p /home/camelftp/ftp; chown camelftp:camelftp /home/camelftp/ftp
  mkdir -p /var/run/vsftpd/empty
  local common
  common="listen=YES
listen_address=127.0.0.1
anonymous_enable=NO
local_enable=YES
write_enable=YES
local_umask=022
chroot_local_user=YES
allow_writeable_chroot=YES
local_root=/home/camelftp/ftp
pasv_enable=YES
pasv_address=127.0.0.1
pasv_min_port=30000
pasv_max_port=30049
port_enable=YES
seccomp_sandbox=NO
secure_chroot_dir=/var/run/vsftpd/empty
pam_service_name=vsftpd
background=YES
userlist_enable=NO
ssl_enable=YES
rsa_cert_file=$CERTS/server.pem
rsa_private_key_file=$CERTS/server.key
ssl_tlsv1=NO
ssl_sslv2=NO
ssl_sslv3=NO
require_ssl_reuse=NO
ssl_ciphers=HIGH
allow_anon_ssl=NO
force_local_data_ssl=NO
force_local_logins_ssl=NO"
  printf '%s\nlisten_port=2121\nvsftpd_log_file=%s/vsftpd.log\nxferlog_enable=YES\n' "$common" "$LOGS" > "$DIR/vsftpd.conf"
  printf '%s\nlisten_port=2990\nimplicit_ssl=YES\nvsftpd_log_file=%s/vsftpd-implicit.log\nxferlog_enable=YES\n' "$common" "$LOGS" > "$DIR/vsftpd-implicit.conf"
  # explicit FTPS whose data connections must resume the control connection's TLS session
  printf '%s\nlisten_port=2991\nvsftpd_log_file=%s/vsftpd-reuse.log\nxferlog_enable=YES\n' "${common/require_ssl_reuse=NO/require_ssl_reuse=YES}" "$LOGS" > "$DIR/vsftpd-reuse.conf"
}

setup_mail() {
  command -v dovecot >/dev/null || { log "dovecot not installed, skipping"; return; }
  ensure_user camelmail
  local uid gid; uid=$(id -u camelmail); gid=$(id -g camelmail)
  mkdir -p /home/camelmail/Maildir/{cur,new,tmp}; chown -R camelmail:camelmail /home/camelmail/Maildir
  echo "camelmail:{PLAIN}$PW:$uid:$gid::/home/camelmail" > "$DIR/dovecot.passwd"
  echo "camel:{PLAIN}$PW:$uid:$gid::/home/camelmail" >> "$DIR/dovecot.passwd"
  chmod 644 "$DIR/dovecot.passwd"
  local key; key=$(key_copy dovecot)
  cat > "$DIR/dovecot.conf" <<EOF
base_dir = $RUN/dovecot
state_dir = $DIR/dovecot-state
protocols = imap pop3
listen = 127.0.0.1
log_path = $LOGS/dovecot.log
mail_location = maildir:~/Maildir
first_valid_uid = 100
disable_plaintext_auth = no
auth_mechanisms = plain login
ssl = yes
ssl_cert = <$CERTS/server.pem
ssl_key = <$key
ssl_min_protocol = TLSv1.2
passdb {
  driver = passwd-file
  args = $DIR/dovecot.passwd
}
userdb {
  driver = passwd-file
  args = $DIR/dovecot.passwd
}
namespace inbox {
  inbox = yes
  mailbox Archive {
    auto = create
  }
  mailbox Trash {
    auto = create
    special_use = \\Trash
  }
}
service imap-login {
  inet_listener imap {
    address = 127.0.0.1
    port = 11143
  }
  inet_listener imaps {
    address = 127.0.0.1
    port = 11993
    ssl = yes
  }
}
service pop3-login {
  inet_listener pop3 {
    address = 127.0.0.1
    port = 11110
  }
  inet_listener pop3s {
    address = 127.0.0.1
    port = 11995
    ssl = yes
  }
}
service auth {
  unix_listener /var/spool/postfix/private/auth {
    mode = 0660
    user = postfix
    group = postfix
  }
}
EOF
  mkdir -p "$DIR/dovecot-state"

  command -v postconf >/dev/null || { log "postfix not installed, skipping"; return; }
  postconf -e "myhostname = localhost" "mydomain = localhost" "myorigin = localhost" \
    "mydestination = localhost, localhost.localdomain" "inet_interfaces = 127.0.0.1" \
    "inet_protocols = ipv4" "mynetworks = 127.0.0.0/8" "home_mailbox = Maildir/" \
    "mailbox_command =" "relayhost =" "smtpd_tls_cert_file = $CERTS/server.pem" \
    "smtpd_tls_key_file = $CERTS/server.key" "smtpd_tls_security_level = may" \
    "smtpd_tls_auth_only = no" "smtpd_sasl_type = dovecot" "smtpd_sasl_path = private/auth" \
    "smtpd_sasl_auth_enable = yes" "maillog_file = $LOGS/postfix.log" \
    "smtpd_relay_restrictions = permit_mynetworks permit_sasl_authenticated defer_unauth_destination" \
    "message_size_limit = 20480000" "compatibility_level = 3.6"
  # master.cf: SMTP on 2525 and SMTPS (wrapper mode) on 4465 instead of 25.
  postconf -M# smtp/inet 2>/dev/null || true
  postconf -M "127.0.0.1:2525/inet=127.0.0.1:2525 inet n - y - - smtpd"
  postconf -M "127.0.0.1:4465/inet=127.0.0.1:4465 inet n - y - - smtpd -o smtpd_tls_wrappermode=yes"
  postconf -M "postlog/unix-dgram=postlog unix-dgram n - n - 1 postlogd"
}

setup_nginx() {
  command -v nginx >/dev/null || { log "nginx not installed, skipping"; return; }
  mkdir -p "$DIR/www" "$DIR/nginx-tmp"
  echo "hello from nginx" > "$DIR/www/hello.txt"
  # a large, compressible document for gzip
  : > "$DIR/www/big.txt"
  for i in $(seq 1 2000); do echo "line $i: the quick brown fox jumps over the lazy dog" >> "$DIR/www/big.txt"; done
  printf '{"server":"nginx","ok":true}' > "$DIR/www/data.json"
  cat > "$DIR/nginx.conf" <<EOF
load_module /usr/lib/nginx/modules/ngx_http_echo_module.so;
worker_processes 1;
pid $RUN/nginx.pid;
error_log $LOGS/nginx-error.log;
events { worker_connections 256; }
http {
  include /etc/nginx/mime.types;
  access_log $LOGS/nginx-access.log;
  client_body_temp_path $DIR/nginx-tmp/body;
  proxy_temp_path $DIR/nginx-tmp/proxy;
  fastcgi_temp_path $DIR/nginx-tmp/fastcgi;
  uwsgi_temp_path $DIR/nginx-tmp/uwsgi;
  scgi_temp_path $DIR/nginx-tmp/scgi;
  keepalive_timeout 30s;
  keepalive_requests 1000;
  gzip on;
  gzip_types text/plain application/json;
  gzip_min_length 100;
  client_max_body_size 10m;
  server {
    listen 127.0.0.1:18080;
    listen 127.0.0.1:18443 ssl http2;
    server_name localhost;
    ssl_certificate $CERTS/server.pem;
    ssl_certificate_key $CERTS/server.key;
    ssl_protocols TLSv1.2 TLSv1.3;
    root $DIR/www;
    location /echo {
      echo_read_request_body;
      default_type text/plain;
      add_header X-Request-Method \$request_method always;
      add_header X-Request-Query "\$query_string" always;
      add_header X-Request-Ctype "\$content_type" always;
      add_header X-Request-Custom "\$http_x_custom" always;
      add_header X-Protocol \$server_protocol always;
      add_header X-Connection \$connection always;
      echo_request_body;
    }
    location /status/ {
      return 404 "not here\n";
    }
    location /redirect {
      return 302 /hello.txt;
    }
    location /proto {
      default_type text/plain;
      return 200 "\$server_protocol \$https \$connection\n";
    }
  }
}
EOF
}

cmd_setup() {
  need_root
  mkdir -p "$DIR" "$LOGS" "$RUN"
  chmod 755 "$DIR" "$LOGS" "$RUN"
  chmod 777 "$LOGS"
  gen_certs
  setup_postgres
  setup_redis
  setup_mosquitto
  setup_rabbitmq
  setup_vsftpd
  setup_mail
  setup_nginx
  log "setup done in $DIR"
}

start_postgres() {
  [ -f "$DIR/pgdata/PG_VERSION" ] || return 0
  port_open 15432 && return 0
  log "starting postgresql"
  chown postgres "$RUN" "$LOGS"
  su postgres -c "$PGBIN/pg_ctl -D $DIR/pgdata -l $LOGS/postgresql.log -w start" >/dev/null
  wait_port 15432 postgresql
  # the `camel` role and database (idempotent)
  PGPASSWORD=$PW psql -h 127.0.0.1 -p 15432 -U postgres -d postgres -qtAc \
    "DO \$\$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'camel') THEN CREATE ROLE camel LOGIN CREATEDB PASSWORD '$PW'; END IF; END \$\$" >/dev/null
  PGPASSWORD=$PW psql -h 127.0.0.1 -p 15432 -U postgres -d postgres -qtAc "SELECT 1 FROM pg_database WHERE datname = 'camel'" | grep -q 1 \
    || PGPASSWORD=$PW psql -h 127.0.0.1 -p 15432 -U postgres -d postgres -qc "CREATE DATABASE camel OWNER camel" >/dev/null
}

start_redis() {
  [ -f "$DIR/redis.conf" ] || return 0
  port_open 16379 && return 0
  log "starting redis"
  redis-server "$DIR/redis.conf"
  wait_port 16379 redis
}

start_mosquitto() {
  [ -f "$DIR/mosquitto.conf" ] || return 0
  chown mosquitto "$RUN"
  if ! port_open 11883; then
    log "starting mosquitto"
    mosquitto -c "$DIR/mosquitto.conf" -d
    wait_port 11883 mosquitto
  fi
  if ! port_open 18883; then
    mosquitto -c "$DIR/mosquitto-auth.conf" -d
    wait_port 18883 mosquitto-auth
  fi
}

start_rabbitmq() {
  [ -f "$DIR/rabbitmq/env" ] || return 0
  port_open 15673 && return 0
  log "starting rabbitmq (takes a few seconds)"
  chown rabbitmq "$RUN"
  su rabbitmq -s /bin/bash -c ". $DIR/rabbitmq/env; rabbitmq-server -detached" >/dev/null 2>&1
  wait_port 15673 rabbitmq
  wait_port 16613 rabbitmq-stomp
}

start_vsftpd() {
  [ -f "$DIR/vsftpd.conf" ] || return 0
  if ! port_open 2121; then log "starting vsftpd"; vsftpd "$DIR/vsftpd.conf"; wait_port 2121 vsftpd; fi
  if ! port_open 2990; then vsftpd "$DIR/vsftpd-implicit.conf"; wait_port 2990 vsftpd-implicit; fi
  if ! port_open 2991; then vsftpd "$DIR/vsftpd-reuse.conf"; wait_port 2991 vsftpd-reuse; fi
}

start_mail() {
  if [ -f "$DIR/dovecot.conf" ] && ! port_open 11143; then
    log "starting dovecot"
    mkdir -p /var/spool/postfix/private
    dovecot -c "$DIR/dovecot.conf"
    wait_port 11143 dovecot
  fi
  if command -v postfix >/dev/null && ! port_open 2525; then
    log "starting postfix"
    newaliases 2>/dev/null || true
    postfix start >/dev/null 2>&1 || postfix reload >/dev/null 2>&1
    wait_port 2525 postfix
  fi
}

start_nginx() {
  [ -f "$DIR/nginx.conf" ] || return 0
  port_open 18080 && return 0
  log "starting nginx"
  nginx -c "$DIR/nginx.conf"
  wait_port 18443 nginx
}

cmd_start() {
  need_root
  [ -d "$CERTS" ] || cmd_setup
  start_postgres
  start_redis
  start_mosquitto
  start_rabbitmq
  start_vsftpd
  start_mail
  start_nginx
  cmd_status
}

kill_pidfile() {
  local f=$1
  if [ -f "$f" ]; then kill "$(head -1 "$f")" 2>/dev/null || true; rm -f "$f"; fi
}

cmd_stop() {
  need_root
  if [ -f "$DIR/pgdata/postmaster.pid" ]; then
    su postgres -c "$PGBIN/pg_ctl -D $DIR/pgdata -m fast stop" >/dev/null 2>&1 || true
  fi
  if port_open 16379; then redis-cli -p 16379 -a "$PW" --no-auth-warning shutdown nosave >/dev/null 2>&1 || true; fi
  kill_pidfile "$RUN/mosquitto.pid"
  kill_pidfile "$RUN/mosquitto-auth.pid"
  if [ -f "$DIR/rabbitmq/env" ] && port_open 15673; then
    su rabbitmq -s /bin/bash -c ". $DIR/rabbitmq/env; rabbitmqctl -n camel-interop@localhost stop" >/dev/null 2>&1 || true
  fi
  pkill -f "vsftpd $DIR/vsftpd" 2>/dev/null || true
  if [ -f "$DIR/dovecot.conf" ]; then doveadm -c "$DIR/dovecot.conf" stop 2>/dev/null || true; fi
  postfix stop >/dev/null 2>&1 || true
  if [ -f "$RUN/nginx.pid" ]; then nginx -c "$DIR/nginx.conf" -s quit 2>/dev/null || true; fi
  log "stopped"
}

cmd_status() {
  local name port
  for entry in postgresql:15432 redis:16379 redis-tls:16380 mosquitto:11883 mosquitto-auth:11884 \
    mosquitto-tls:18883 rabbitmq-amqp:15673 rabbitmq-amqps:15674 rabbitmq-stomp:16613 \
    rabbitmq-stomp-tls:16614 rabbitmq-mqtt:11885 vsftpd:2121 vsftpd-implicit:2990 vsftpd-reuse:2991 \
    postfix-smtp:2525 postfix-smtps:4465 dovecot-imap:11143 dovecot-imaps:11993 \
    dovecot-pop3:11110 dovecot-pop3s:11995 nginx-http:18080 nginx-https:18443; do
    name=${entry%%:*}; port=${entry##*:}
    if port_open "$port"; then echo "  up    $name ($port)"; else echo "  DOWN  $name ($port)"; fi
  done
}

cmd_versions() {
  "$PGBIN/postgres" --version 2>/dev/null || true
  redis-server --version 2>/dev/null | cut -d' ' -f1-3 || true
  mosquitto -h 2>/dev/null | head -1 || true
  dpkg -s rabbitmq-server 2>/dev/null | grep '^Version' | sed 's/^/rabbitmq-server /' || true
  dpkg -s erlang-base 2>/dev/null | grep '^Version' | sed 's/^/erlang /' || true
  vsftpd -v 0>&- 2>&1 || true
  echo "postfix $(postconf -h mail_version 2>/dev/null)"
  echo "dovecot $(dovecot --version 2>/dev/null)"
  nginx -v 2>&1 || true
  openssl version
}

case "${1:-}" in
  install) cmd_install ;;
  setup) cmd_setup ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  versions) cmd_versions ;;
  all) cmd_install; cmd_setup; cmd_start ;;
  *) sed -n '2,40p' "$0"; exit 1 ;;
esac
