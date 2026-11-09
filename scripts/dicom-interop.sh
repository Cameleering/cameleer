#!/usr/bin/env bash
# Live DICOM interop peers for Cameleer: DCMTK and Orthanc (with its DICOMweb plugin).
#
#   scripts/dicom-interop.sh install   # DCMTK 3.6.7, Orthanc 1.12.2, orthanc-dicomweb (see below)
#   scripts/dicom-interop.sh setup     # TLS certificates and the Orthanc configuration (idempotent)
#   scripts/dicom-interop.sh start     # start the peers (already running ones are left alone)
#   scripts/dicom-interop.sh stop      # stop the peers started by this script
#   scripts/dicom-interop.sh status    # show which ports answer
#   scripts/dicom-interop.sh versions  # print the peer versions
#   scripts/dicom-interop.sh env       # print the variables the tests need (eval "$(... env)")
#   scripts/dicom-interop.sh all       # install + setup + start
#
# Then: eval "$(scripts/dicom-interop.sh env)" && CAMEL_DICOM_INTEROP=1 cargo test -p cameleer-dicom --test live_dicom
#
# Install: as root the packages come from apt (`apt-get install`). Without root they are
# downloaded with `apt-get download` (apt checks each file against the signed archive index),
# checked again against the SHA-256 values pinned below for Ubuntu 24.04 (noble), and unpacked
# with `dpkg -x` into $DIR/root; nothing is installed system-wide.
#
# Everything lives under $CAMEL_DICOM_INTEROP_DIR (default $HOME/camel-dicom-interop). Ports, all
# on 127.0.0.1:
#
#   DCMTK storescp   11130  AE STORESCP, writes to $DIR/storescp
#                    11131  the same over TLS (BCP 195 non-downgrading profile)
#   Orthanc          14242  DICOM, AE ORTHANC (C-ECHO, C-STORE, C-FIND, C-MOVE, C-GET SCP)
#                    18042  HTTP REST and DICOMweb (/dicom-web), no authentication
#   (tests)          11140  where the tests run a Cameleer SCP, AE CAMELEER: Orthanc's C-MOVE
#                           destination `CAMELEER`, and the target of the DCMTK SCU tools
#                    11141  the same over TLS
set -euo pipefail

DIR="${CAMEL_DICOM_INTEROP_DIR:-$HOME/camel-dicom-interop}"
ROOT="$DIR/root"
CERTS="$DIR/certs"
LOGS="$DIR/logs"
RUN="$DIR/run"

PACKAGES=(dcmtk orthanc orthanc-dicomweb)
# Ubuntu 24.04 (noble) packages for the non-root install: name=version sha256
PINNED="
dcmtk_3.6.7-9.1build4_amd64.deb 278fedbe3372c3252ece5361d980cfac34f48beeafac7f5e86643272c1ea9957
libdcmtk17t64_3.6.7-9.1build4_amd64.deb 4682d1d06d2887657c2ccb6a758fde7a9457fe4c189875230a80eecf6919e9b3
libwrap0_7.6.q-33_amd64.deb 36118b862539868408ee056411d6e470068d14c46e5296b29bfb744770f5d2ce
orthanc_1.12.2+dfsg-1build4_amd64.deb b69c511d02b014956ab3aeff7291aafef1c5d8b3d80e84560c8ae1b4d443485d
orthanc-dicomweb_1.16+dfsg-1build2_amd64.deb 7930923e04dca7febb1bb1f252fe3e93bfafa12612d111881fcc1d35dbecaa5f
libcivetweb1_1.16+dfsg-1ubuntu0.1_amd64.deb 5efd68a345099a05dad249dd91936fd08cda99f81b8c11b836383bd338ea6ddb
libboost-filesystem1.83.0_1.83.0-2.1ubuntu3.2_amd64.deb 154b74fe60f7fc6223b0ff86063b8eb9429bfd1f532085fce66b555ebf2bf96c
libboost-iostreams1.83.0_1.83.0-2.1ubuntu3.2_amd64.deb 614e516fc34d3a9f4b39149c79bde94160631b6486943d9782e264ab475fcdc3
libboost-locale1.83.0_1.83.0-2.1ubuntu3.2_amd64.deb 7af19becd12c27e23af2c0cfd05ed35dfb3c6758cd25bb2d8c1eeff4a5883e3b
libboost-thread1.83.0_1.83.0-2.1ubuntu3.2_amd64.deb d6b7411fd5368a49ffe88ad66d471bc57a43c2bb4eb105ee3f5fb921bfdf8924
liblua5.3-0_5.3.6-2build2_amd64.deb d0c29c8b7275fa9443f2b7274275a5459b4d950301145ee95da08ad8a3512c78
libprotobuf32t64_3.21.12-8.2ubuntu0.4_amd64.deb 765aec7fc6c7c14aea8d10677c17617d71acc5103ad580f0e97bff903acccdaf
libpugixml1v5_1.14-0.1build1_amd64.deb b80a2e82b4e4a2eab679e23fe329b5e08e6e0ef1478719bb59f9b9b3ce2c47ee
"

log() { echo "[dicom-interop] $*"; }
port_open() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }
wait_port() {
  local p=$1 n=0
  while ! port_open "$p"; do
    n=$((n + 1))
    if [ $n -gt 100 ]; then log "port $p did not open"; return 1; fi
    sleep 0.1
  done
}

# Binaries: the system ones after a root install, the unpacked ones otherwise.
bin() {
  if [ -x "$ROOT/usr/bin/$1" ]; then echo "$ROOT/usr/bin/$1"; elif [ -x "$ROOT/usr/sbin/$1" ]; then echo "$ROOT/usr/sbin/$1"; else command -v "$1"; fi
}
lib_env() {
  if [ -d "$ROOT/usr/lib/x86_64-linux-gnu" ]; then
    export LD_LIBRARY_PATH="$ROOT/usr/lib/x86_64-linux-gnu:$ROOT/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    export DCMDICTPATH="$ROOT/usr/share/libdcmtk17/dicom.dic"
  fi
}
plugins_dir() {
  if [ -d "$ROOT/usr/share/orthanc/plugins" ]; then echo "$ROOT/usr/share/orthanc/plugins"; else echo /usr/share/orthanc/plugins; fi
}

cmd_install() {
  mkdir -p "$DIR"
  if [ "$(id -u)" = 0 ]; then
    DEBIAN_FRONTEND=noninteractive apt-get install -y "${PACKAGES[@]}"
    return
  fi
  mkdir -p "$DIR/debs" "$ROOT"
  cd "$DIR/debs"
  while read -r file sum; do
    [ -z "$file" ] && continue
    if [ ! -f "$file" ]; then
      pkg="${file%%_*}"
      ver="${file#*_}"; ver="${ver%_amd64.deb}"
      apt-get download "$pkg=$ver" >/dev/null
    fi
    got="$(sha256sum "$file" | cut -c1-64)"
    if [ "$got" != "$sum" ]; then
      log "checksum mismatch for $file: expected $sum, got $got"
      rm -f "$file"
      exit 1
    fi
    dpkg -x "$file" "$ROOT"
  done <<< "$PINNED"
  log "unpacked into $ROOT"
}

cmd_setup() {
  mkdir -p "$CERTS" "$LOGS" "$RUN" "$DIR/storescp" "$DIR/orthanc-db"
  if [ ! -f "$CERTS/ca.pem" ]; then
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 -subj "/CN=Cameleer DICOM interop CA" \
      -keyout "$CERTS/ca.key" -out "$CERTS/ca.pem" 2>/dev/null
    for who in dcmtk cameleer; do
      openssl req -newkey rsa:2048 -nodes -subj "/CN=localhost" -keyout "$CERTS/$who.key" -out "$CERTS/$who.csr" 2>/dev/null
      printf "subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth,clientAuth\n" > "$CERTS/$who.ext"
      openssl x509 -req -in "$CERTS/$who.csr" -CA "$CERTS/ca.pem" -CAkey "$CERTS/ca.key" -CAcreateserial \
        -days 3650 -extfile "$CERTS/$who.ext" -out "$CERTS/$who.pem" 2>/dev/null
    done
    log "certificates in $CERTS"
  fi
  cat > "$DIR/orthanc.json" <<EOF
{
  "Name": "cameleer-interop",
  "StorageDirectory": "$DIR/orthanc-db",
  "IndexDirectory": "$DIR/orthanc-db",
  "Plugins": ["$(plugins_dir)/libOrthancDicomWeb.so"],
  "HttpPort": 18042,
  "RemoteAccessAllowed": false,
  "AuthenticationEnabled": false,
  "DicomAet": "ORTHANC",
  "DicomPort": 14242,
  "DicomCheckCalledAet": false,
  "DicomModalities": {
    "CAMELEER": ["CAMELEER", "127.0.0.1", 11140],
    "STORESCP": ["STORESCP", "127.0.0.1", 11130]
  },
  "DicomAlwaysAllowStore": true,
  "DicomAlwaysAllowFind": true,
  "DicomAlwaysAllowMove": true,
  "DicomAlwaysAllowGet": true,
  "DicomAlwaysAllowEcho": true,
  "UnknownSopClassAccepted": true,
  "DicomWeb": { "Enable": true, "Root": "/dicom-web/", "EnableWado": true, "Ssl": false, "StudiesMetadata": "Full", "SeriesMetadata": "Full" }
}
EOF
  log "configuration in $DIR/orthanc.json"
}

start_storescp() {
  lib_env
  if ! port_open 11130; then
    nohup "$(bin storescp)" -aet STORESCP -od "$DIR/storescp" +xa 11130 >"$LOGS/storescp.log" 2>&1 &
    echo $! > "$RUN/storescp.pid"
    wait_port 11130
  fi
  if ! port_open 11131; then
    nohup "$(bin storescp)" -aet STORESCP -od "$DIR/storescp" +xa \
      +tls "$CERTS/dcmtk.key" "$CERTS/dcmtk.pem" +cf "$CERTS/ca.pem" -ic +py 11131 >"$LOGS/storescp-tls.log" 2>&1 &
    echo $! > "$RUN/storescp-tls.pid"
    wait_port 11131
  fi
}

start_orthanc() {
  lib_env
  if ! port_open 14242; then
    nohup "$(bin Orthanc)" "$DIR/orthanc.json" >"$LOGS/orthanc.log" 2>&1 &
    echo $! > "$RUN/orthanc.pid"
    wait_port 18042
    wait_port 14242
  fi
}

cmd_start() {
  [ -f "$DIR/orthanc.json" ] || cmd_setup
  start_storescp
  start_orthanc
  log "started"
}

cmd_stop() {
  for f in "$RUN"/*.pid; do
    [ -f "$f" ] || continue
    kill "$(cat "$f")" 2>/dev/null || true
    rm -f "$f"
  done
  log "stopped"
}

cmd_status() {
  for p in 11130 11131 14242 18042; do
    if port_open $p; then echo "$p open"; else echo "$p closed"; fi
  done
}

cmd_versions() {
  lib_env
  "$(bin storescu)" --version | head -1
  "$(bin Orthanc)" --version 2>&1 | head -1
  curl -s http://127.0.0.1:18042/plugins/dicom-web 2>/dev/null | grep -o '"Version" *: *"[^"]*"' || true
}

cmd_env() {
  echo "export CAMEL_DICOM_INTEROP_DIR=\"$DIR\""
  echo "export CAMEL_DICOM_DCMTK_BIN=\"$(dirname "$(bin storescu)")\""
  if [ -d "$ROOT/usr/lib/x86_64-linux-gnu" ]; then
    echo "export LD_LIBRARY_PATH=\"$ROOT/usr/lib/x86_64-linux-gnu:$ROOT/lib/x86_64-linux-gnu\""
    echo "export DCMDICTPATH=\"$ROOT/usr/share/libdcmtk17/dicom.dic\""
  fi
}

case "${1:-}" in
  install) cmd_install ;;
  setup) cmd_setup ;;
  start) cmd_start ;;
  stop) cmd_stop ;;
  restart) cmd_stop; cmd_start ;;
  status) cmd_status ;;
  versions) cmd_versions ;;
  env) cmd_env ;;
  all) cmd_install; cmd_setup; cmd_start ;;
  *) sed -n '2,12p' "$0"; exit 2 ;;
esac
