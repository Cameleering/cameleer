#!/usr/bin/env bash
# Cameleer vs Apache Camel (Java) benchmark suite.
#
#   scripts/bench.sh            # everything: tools, build, run both sides, write docs/BENCHMARKS.md
#   scripts/bench.sh tools      # download + checksum-verify the pinned JDK and Maven only
#   scripts/bench.sh build      # build both sides
#   scripts/bench.sh run        # run both sides (results in target/bench/)
#   scripts/bench.sh report     # regenerate docs/BENCHMARKS.md from target/bench/
#
# Runs on Linux and on Windows under Git Bash (scripts/bench.ps1 wraps it). Environment:
#   BENCH_QUICK=1          short warmup/measurement (smoke test, numbers not meaningful)
#   BENCH_SCENARIOS="..."  subset of: direct seda cbr splitagg http startup
#   BENCH_TOOLS=dir        where the JDK and Maven are cached (default ~/.cache/cameleer-bench)
#   BENCH_JAVA_HOME=dir    use this JDK instead of the pinned Temurin download
#   BENCH_OUT=file         report path (default docs/BENCHMARKS.md)
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

# ---- pinned versions and checksums -------------------------------------------------------
JDK_VERSION="21.0.13+9"
JDK_TAG="jdk-21.0.13%2B9"
JDK_WIN_FILE="OpenJDK21U-jdk_x64_windows_hotspot_21.0.13_9.zip"
JDK_WIN_SHA256="a4dbaa05fca4bd89d84584e58c459bd732efebb1722af80ab13c6516832004f9"
JDK_LINUX_FILE="OpenJDK21U-jdk_x64_linux_hotspot_21.0.13_9.tar.gz"
JDK_LINUX_SHA256="d7f5d5cf865a2902b70ff549b97586a7d3f234e016f041ab447ea740c7d66d1c"
MAVEN_VERSION="3.9.16"
MAVEN_ZIP_SHA512="ed41650d42485cfc243fad22158caf9cbb5dc408ce7a09ddb94dd42a019de929ca43065bfa450612cf12bf78b5cafa3884b96c090de326ff590448c933454af3"
MAVEN_TGZ_SHA512="831a8591fe20c8243b1dbe7d71e3244f31d1665b0804b2e825e38cbbe5ce0cafb8338851f90780735568773e0a6cd07bbec107cda0b896b008b861075358b6f6"
# The Camel version itself is pinned in benchmarks/java-camel/pom.xml.

TOOLS="${BENCH_TOOLS:-$HOME/.cache/cameleer-bench}"
OUTDIR="$ROOT/target/bench"
REPORT="${BENCH_OUT:-$ROOT/docs/BENCHMARKS.md}"
SCENARIOS="${BENCH_SCENARIOS:-direct seda cbr splitagg http startup}"

case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) WINDOWS=1; EXE=".exe"; CPSEP=";" ;;
  *) WINDOWS=0; EXE=""; CPSEP=":" ;;
esac

log() { printf '==> %s\n' "$*" >&2; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

verify() { # verify <algo> <expected> <file>
  local got
  got="$("${1}sum" "$3" | cut -d' ' -f1)"
  [ "$got" = "$2" ] || { rm -f "$3"; die "checksum mismatch for $3: expected $2, got $got"; }
}

fetch() { # fetch <url> <file> <algo> <expected>
  if [ -f "$2" ]; then verify "$3" "$4" "$2"; return; fi
  log "downloading $1"
  curl -fsSL --proto '=https' --tlsv1.2 -o "$2.part" "$1"
  mv "$2.part" "$2"
  verify "$3" "$4" "$2"
  log "checksum ok ($3) $2"
}

tools() {
  mkdir -p "$TOOLS"
  if [ -n "${BENCH_JAVA_HOME:-}" ]; then
    JAVA_HOME="$BENCH_JAVA_HOME"
  else
    local jdk_dir="$TOOLS/jdk-$JDK_VERSION"
    if [ ! -x "$jdk_dir/bin/java$EXE" ]; then
      local base="https://github.com/adoptium/temurin21-binaries/releases/download/$JDK_TAG"
      rm -rf "$TOOLS/jdk-extract" && mkdir -p "$TOOLS/jdk-extract"
      if [ "$WINDOWS" = 1 ]; then
        fetch "$base/$JDK_WIN_FILE" "$TOOLS/$JDK_WIN_FILE" sha256 "$JDK_WIN_SHA256"
        unzip -q "$TOOLS/$JDK_WIN_FILE" -d "$TOOLS/jdk-extract"
      else
        fetch "$base/$JDK_LINUX_FILE" "$TOOLS/$JDK_LINUX_FILE" sha256 "$JDK_LINUX_SHA256"
        tar -xzf "$TOOLS/$JDK_LINUX_FILE" -C "$TOOLS/jdk-extract"
      fi
      rm -rf "$jdk_dir" && mv "$TOOLS/jdk-extract"/jdk-* "$jdk_dir" && rm -rf "$TOOLS/jdk-extract"
    fi
    JAVA_HOME="$jdk_dir"
  fi
  local mvn_dir="$TOOLS/apache-maven-$MAVEN_VERSION"
  if [ ! -f "$mvn_dir/bin/mvn" ]; then
    local base="https://archive.apache.org/dist/maven/maven-3/$MAVEN_VERSION/binaries"
    if [ "$WINDOWS" = 1 ]; then
      fetch "$base/apache-maven-$MAVEN_VERSION-bin.zip" "$TOOLS/apache-maven-$MAVEN_VERSION-bin.zip" sha512 "$MAVEN_ZIP_SHA512"
      unzip -q -o "$TOOLS/apache-maven-$MAVEN_VERSION-bin.zip" -d "$TOOLS"
    else
      fetch "$base/apache-maven-$MAVEN_VERSION-bin.tar.gz" "$TOOLS/apache-maven-$MAVEN_VERSION-bin.tar.gz" sha512 "$MAVEN_TGZ_SHA512"
      tar -xzf "$TOOLS/apache-maven-$MAVEN_VERSION-bin.tar.gz" -C "$TOOLS"
    fi
  fi
  export JAVA_HOME
  MVN="$mvn_dir/bin/mvn"
  [ "$WINDOWS" = 1 ] && MVN="$mvn_dir/bin/mvn.cmd"
  JAVA="$JAVA_HOME/bin/java$EXE"
  "$JAVA" -version 2>&1 | head -1 >&2
}

if [ -n "${BENCH_QUICK:-}" ]; then
  WARMUP=2; ITER=1; ITERS=2; JAVA_WARMUP=3
else
  WARMUP=10; ITER=5; ITERS=5; JAVA_WARMUP=10
fi
HTTP_CONNS=16
BENCH_BIN="$ROOT/target/release/cameleer-bench$EXE"
JAVA_DIR="$ROOT/benchmarks/java-camel"

build() {
  tools
  log "building cameleer-bench (release)"
  cargo build --release -p cameleer-bench
  log "building the Java side (Maven, strict checksums)"
  "$MVN" -C -B -q -f "$JAVA_DIR/pom.xml" package
}

machine() {
  echo "Date: $(date -u '+%Y-%m-%d %H:%M UTC')"
  if [ "$WINDOWS" = 1 ]; then
    powershell.exe -NoProfile -Command '
      $c = Get-CimInstance Win32_Processor | Select-Object -First 1
      $o = Get-CimInstance Win32_OperatingSystem
      "CPU: $($c.Name.Trim())"
      "Cores / threads: $($c.NumberOfCores) / $($c.NumberOfLogicalProcessors)"
      "RAM: {0:N1} GiB" -f ((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB)
      "OS: $($o.Caption) $($o.Version)"
      "Power plan: $((powercfg /getactivescheme) -replace ".*\((.*)\).*", "`$1")"
      "CPU load before the run: $((Get-CimInstance Win32_Processor | Measure-Object LoadPercentage -Average).Average)%"' | tr -d '\r'
  else
    echo "CPU: $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2 | sed 's/^ *//')"
    echo "Cores / threads: $(lscpu -p=CORE 2>/dev/null | grep -v '^#' | sort -u | wc -l) / $(nproc)"
    echo "RAM: $(awk '/MemTotal/ {printf "%.1f GiB", $2/1048576}' /proc/meminfo)"
    echo "OS: $(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME") $(uname -r)"
    echo "Load average before the run: $(cut -d' ' -f1-3 /proc/loadavg)"
  fi
  echo "Rust: $(rustc -V)"
  echo "Cameleer commit: $(git rev-parse --short HEAD)$(git diff --quiet HEAD -- crates || echo ' (+ local changes)')"
  echo "JDK: $("$JAVA" -version 2>&1 | head -1)"
  echo "Maven: $MAVEN_VERSION"
  echo "Apache Camel: $(sed -n 's:.*<camel.version>\(.*\)</camel.version>.*:\1:p' "$JAVA_DIR/pom.xml")"
}

run_all() {
  tools
  mkdir -p "$OUTDIR" "$OUTDIR/tmp"
  local results="$OUTDIR/results.txt" logf="$OUTDIR/run.log"
  : > "$results"; : > "$logf"
  machine > "$OUTDIR/machine.txt"
  # java.exe on Windows needs native paths (E:/...), not Git Bash's /e/...
  local jdir="$JAVA_DIR" jtmp="$OUTDIR/tmp"
  if [ "$WINDOWS" = 1 ]; then jdir="$(cygpath -m "$JAVA_DIR")"; jtmp="$(cygpath -m "$OUTDIR/tmp")"; fi
  local java_cp="$jdir/target/classes${CPSEP}$jdir/target/lib/*"
  # Netty's selector wake-up uses a Unix-domain socket in the temp dir on Windows; give it a
  # short, writable directory (no effect on performance).
  local java_cmd=("$JAVA" "-Djdk.net.unixdomain.tmpdir=$jtmp" -cp "$java_cp" bench.Bench)
  local rust_cmd=("$BENCH_BIN")
  local side cmd warm
  for s in $SCENARIOS; do
    for side in rust java; do
      if [ "$side" = rust ]; then cmd=("${rust_cmd[@]}"); warm=$WARMUP; else cmd=("${java_cmd[@]}"); warm=$JAVA_WARMUP; fi
      log "$s ($side)"
      case "$s" in
        http)
          "$BENCH_BIN" measure --side "$side" --scenario http --http "$HTTP_CONNS" \
            --warmup "$warm" --iter "$ITER" --iters "$ITERS" -- "${cmd[@]}" http-server '{port}' ;;
        startup)
          "$BENCH_BIN" measure --side "$side" --scenario startup --repeat 5 -- "${cmd[@]}" startup ;;
        seda)
          # InOnly and InOut, one process each
          for sc in seda seda-inout; do
            "$BENCH_BIN" measure --side "$side" --scenario "$sc" -- "${cmd[@]}" run "$sc" "$warm" "$ITER" "$ITERS"
          done ;;
        *)
          "$BENCH_BIN" measure --side "$side" --scenario "$s" -- "${cmd[@]}" run "$s" "$warm" "$ITER" "$ITERS" ;;
      esac 2>>"$logf" | tee -a "$results"
    done
  done
}

report() {
  [ -f "$OUTDIR/results.txt" ] || die "no results: run scripts/bench.sh run first"
  local plan="warmup ${WARMUP} s, then ${ITERS} timed iterations of ${ITER} s"
  [ -n "${BENCH_QUICK:-}" ] && plan="$plan (BENCH_QUICK smoke run: numbers are not meaningful)"
  "$BENCH_BIN" report --results "$OUTDIR/results.txt" --machine "$OUTDIR/machine.txt" \
    --analysis "$ROOT/benchmarks/analysis.md" --plan "$plan" --out "$REPORT"
  log "wrote $REPORT"
}

case "${1:-all}" in
  tools) tools ;;
  build) build ;;
  run) run_all ;;
  report) report ;;
  all) build; run_all; report ;;
  *) die "unknown command: $1" ;;
esac
