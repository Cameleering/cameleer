#!/usr/bin/env bash
# Package Camel routes as an AWS Lambda function for the `provided.al2023` runtime.
#
#   scripts/package-lambda.sh [--arch x86_64|arm64] [--features LIST] [--out FILE] routes...
#
# Builds `cameleer` as a static musl binary (Lambda's AL2023 has glibc 2.34, older than the
# glibc a current build machine links against), names it `bootstrap`, and zips it with the
# route and application.properties/yaml files given. `--features` picks the component
# families (cameleer-main cargo features, comma separated; default: all). Upload with
#
#   aws lambda create-function --function-name orders --runtime provided.al2023 \
#     --architectures x86_64 --handler direct:lambda --zip-file fileb://function.zip \
#     --role arn:aws:iam::<account>:role/<role>
#
# The handler setting names the endpoint that non-HTTP events go to (default direct:lambda);
# API Gateway and function URL events go to the platform-http routes. See docs/DEPLOYMENT.md.
# Needs: rustup (the musl target is added if missing), a C compiler for the target (for arm64
# on x86_64: aarch64-linux-gnu-gcc), and zip or python3.
set -euo pipefail

arch=x86_64
features=
out=function.zip
files=()
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) arch=$2; shift 2 ;;
    --features) features=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    -h|--help) sed -n '2,19p' "$0"; exit 0 ;;
    *) files+=("$1"); shift ;;
  esac
done
[ ${#files[@]} -gt 0 ] || { echo "no route files given" >&2; exit 2; }
case "$arch" in
  x86_64) target=x86_64-unknown-linux-musl ;;
  arm64|aarch64) target=aarch64-unknown-linux-musl
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=${CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER:-aarch64-linux-gnu-gcc} ;;
  *) echo "unknown --arch $arch (x86_64 or arm64)" >&2; exit 2 ;;
esac
for f in "${files[@]}"; do [ -f "$f" ] || { echo "no such file: $f" >&2; exit 2; }; done

root=$(cd "$(dirname "$0")/.." && pwd)
rustup target list --installed | grep -qx "$target" || rustup target add "$target"
args=(--profile dist --locked -p cameleer-main --bin cameleer --target "$target")
[ -n "$features" ] && args+=(--no-default-features --features "$features")
(cd "$root" && RUSTFLAGS="${RUSTFLAGS:-} -C strip=symbols" cargo build "${args[@]}")

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
install -m 755 "$root/target/$target/dist/cameleer" "$stage/bootstrap"
cp "${files[@]}" "$stage/"
out_abs=$(cd "$(dirname "$out")" && pwd)/$(basename "$out")
rm -f "$out_abs"
if command -v zip >/dev/null; then
  (cd "$stage" && zip -q -X "$out_abs" ./*)
else
  python3 - "$stage" "$out_abs" <<'PY'
import os, sys, zipfile
stage, out = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for name in sorted(os.listdir(stage)):
        info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
        info.external_attr = (0o755 if name == "bootstrap" else 0o644) << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        with open(os.path.join(stage, name), "rb") as f:
            z.writestr(info, f.read())
PY
fi
echo "$out_abs: $(stat -c %s "$out_abs" 2>/dev/null || wc -c < "$out_abs") bytes (bootstrap $(stat -c %s "$stage/bootstrap") bytes, $target)"
