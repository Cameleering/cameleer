#!/usr/bin/env bash
# Runs on a server droplet (called by `run.sh sizes`): the `cameleer` CLI of this commit
# (/root/cameleer) against the baseline (/root/cameleer-base), both `dist` builds, with every
# component (`full`) and with none (`core`). Prints one record per build:
#
#   SIZE build=new-full bytes=... stripped=...
#   COLDSTART build=new-full route=http ready_ms=... exit_ms=... peak_kb=...
#
# The cold starts alternate between the builds (ROUNDS rounds of REPEAT runs each), so a slow
# moment of the machine hits both. Routes: one platform-http route (killed when ready) and a
# timer that fires once (`--max-messages 1`, a run-once job), as in benchmarks/analysis.md.
set -euo pipefail
export PATH=/root/.cargo/bin:$PATH
ROUNDS="${SIZES_ROUNDS:-4}"; REPEAT="${SIZES_REPEAT:-20}"
BENCH=/root/cameleer/target/release/cameleer-bench
OUT=/root/sizes; mkdir -p "$OUT"

build() { # build <src> <name> <feature args...>
  local src=$1 name=$2; shift 2
  echo "building $name" >&2
  (cd "$src" && CARGO_TARGET_DIR="$OUT/target-$name" cargo build -q --profile dist -p cameleer-main --bin cameleer "$@") >&2
  cp "$OUT/target-$name/dist/cameleer" "$OUT/cameleer-$name"
  cp "$OUT/cameleer-$name" "$OUT/cameleer-$name.stripped" && strip "$OUT/cameleer-$name.stripped"
  echo "SIZE build=$name bytes=$(stat -c %s "$OUT/cameleer-$name") stripped=$(stat -c %s "$OUT/cameleer-$name.stripped")"
}

build /root/cameleer new-full
build /root/cameleer-base base-full
build /root/cameleer new-core --no-default-features --features core
build /root/cameleer-base base-core --no-default-features --features core

cat > "$OUT/http.camel.yaml" <<'EOF'
- route:
    from:
      uri: platform-http:/hello
      steps:
        - setBody:
            constant: hello
EOF
cat > "$OUT/timer.camel.yaml" <<'EOF'
- route:
    from:
      uri: timer:once?repeatCount=1
      steps:
        - setBody:
            constant: done
        - log: "${body}"
EOF

cd "$OUT"
for r in $(seq "$ROUNDS"); do
  for b in base-full new-full base-core new-core; do
    bin="$OUT/cameleer-$b.stripped"
    echo "round $r: $b" >&2
    l=$("$BENCH" coldstart --kill --repeat "$REPEAT" --ready "Routes startup" -- "$bin" run "$OUT/http.camel.yaml")
    echo "${l/COLDSTART/COLDSTART build=$b route=http round=$r}"
    l=$("$BENCH" coldstart --repeat "$REPEAT" --ready "Routes startup" -- "$bin" run "$OUT/timer.camel.yaml" --max-messages 1)
    echo "${l/COLDSTART/COLDSTART build=$b route=timer round=$r}"
  done
done
