#!/usr/bin/env bash
# MCP conformance tier: runs the official conformance suite (@modelcontextprotocol/conformance)
# against `cameleer run --mcp` serving scripts/mcp-conformance/fixtures.camel.yaml (the tools,
# resources and prompts the suite calls).
#
#   scripts/mcp-conformance.sh                 # 2025-11-25 and 2025-06-18
#   scripts/mcp-conformance.sh 2025-06-18      # one spec version
#
# Known gaps are listed in scripts/mcp-conformance/expected-failures.yaml; any other failure, or
# a listed scenario that passes, fails the run. Suite 0.1.16 predates the 2026-07-28 revision,
# which the hermetic tests (crates/cameleer-ai/tests/mcp.rs) cover.
#
# Needs Node.js 20+ and npm. The suite version and its npm integrity hash are pinned below; the
# install stops if the downloaded package does not match. Results go to
# target/mcp-conformance/<version>/.
set -euo pipefail

VERSION="0.1.16"
INTEGRITY="sha512-GI7qiN0r39/MH2srVUR3AXaEN0YLCro20lIBbnvc1frBhszenxvUifBuTzxeVQVagILfBzCIcnungUOma8OrgA=="
PORT="${MCP_CONFORMANCE_PORT:-3901}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="${MCP_CONFORMANCE_DIR:-$ROOT/target/mcp-conformance}"
if [ $# -gt 0 ]; then SPECS=("$@"); else SPECS=(2025-11-25 2025-06-18); fi

command -v node >/dev/null || { echo "node is required" >&2; exit 2; }
command -v npm >/dev/null || { echo "npm is required" >&2; exit 2; }

mkdir -p "$WORK/suite"
cd "$WORK/suite"
[ -f package.json ] || echo '{"name":"cameleer-mcp-conformance","private":true}' > package.json
npm install --ignore-scripts --no-audit --no-fund --save-exact "@modelcontextprotocol/conformance@$VERSION" >/dev/null
got=$(node -e "console.log(require('./package-lock.json').packages['node_modules/@modelcontextprotocol/conformance'].integrity)")
if [ "$got" != "$INTEGRITY" ]; then
  echo "integrity mismatch for @modelcontextprotocol/conformance@$VERSION: $got" >&2
  exit 3
fi
SUITE="$WORK/suite/node_modules/@modelcontextprotocol/conformance/dist/index.js"

cd "$ROOT"
cargo build -q -p cameleer-main --bin cameleer
rm -f "$WORK/cameleer.log"
CAMEL_LOCAL_STATUS=false target/debug/cameleer run scripts/mcp-conformance/fixtures.camel.yaml \
  --mcp --port "$PORT" --prop camel.server.host=127.0.0.1 > "$WORK/cameleer.log" 2>&1 &
PID=$!
trap 'kill $PID 2>/dev/null || true' EXIT
for _ in $(seq 1 100); do
  grep -q "MCP server listening" "$WORK/cameleer.log" 2>/dev/null && break
  sleep 0.1
done

status=0
for spec in "${SPECS[@]}"; do
  rm -rf "${WORK:?}/$spec"
  mkdir -p "$WORK/$spec"
  echo "== MCP conformance, spec version $spec"
  if node "$SUITE" server --url "http://127.0.0.1:$PORT/mcp" --spec-version "$spec" -o "$WORK/$spec" \
      --expected-failures "$ROOT/scripts/mcp-conformance/expected-failures.yaml" > "$WORK/$spec/output.txt" 2>&1; then
    echo "== $spec: passed (known gaps in expected-failures.yaml)"
  else
    echo "== $spec: FAILED"
    status=1
  fi
  sed -n '/SUMMARY/,$p' "$WORK/$spec/output.txt"
done
exit $status
