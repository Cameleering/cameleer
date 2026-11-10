#!/usr/bin/env bash
# Build a release archive of the `cameleer` binary for one distribution and target, with a
# SHA-256 sum next to it:
#
#   scripts/package-archive.sh general x86_64-unknown-linux-gnu   -> dist/cameleer-<version>-<target>.tar.gz
#   scripts/package-archive.sh health  x86_64-pc-windows-msvc     -> dist/cameleer-health-<version>-<target>.zip
#
# general = the default build (`full`); health = `full` plus the health pack
# (docs/packs/index.md). Windows targets get a .zip, the others a .tar.gz. The archive holds
# the binary (always named `cameleer`), LICENSE and README.md. Next to it: a SHA-256 sum and a
# CycloneDX SBOM of exactly the crates compiled in (scripts/sbom.py). Nothing is uploaded.
set -euo pipefail

dist="${1:?distribution: general or health}"
target="${2:?target triple, for example x86_64-unknown-linux-gnu}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

version="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
case "$dist" in
  general) features=(); sbom=(); name="cameleer-$version-$target" ;;
  health)  features=(--features "full,health"); sbom=(--features "full,health"); name="cameleer-health-$version-$target" ;;
  *) echo "unknown distribution: $dist (general or health)" >&2; exit 2 ;;
esac

cargo build --profile dist --locked -p cameleer-main --target "$target" ${features[@]+"${features[@]}"}

exe=cameleer
case "$target" in *windows*) exe=cameleer.exe ;; esac
stage="$root/dist/$name"
rm -rf "$stage"
mkdir -p "$stage"
cp "${CARGO_TARGET_DIR:-target}/$target/dist/$exe" "$stage/"
cp LICENSE README.md "$stage/" 2>/dev/null || true

cd "$root/dist"
case "$target" in
  *windows*) archive="$name.zip"; (cd "$name" && zip -qr "../$archive" .) ;;
  *)         archive="$name.tar.gz"; tar -czf "$archive" "$name" ;;
esac
if command -v sha256sum >/dev/null 2>&1; then sha256sum "$archive" > "$archive.sha256"; else shasum -a 256 "$archive" > "$archive.sha256"; fi
rm -rf "$name"
cd "$root"
python3 scripts/sbom.py --package cameleer-main ${sbom[@]+"${sbom[@]}"} --target "$target" --name "${name%-$version-$target}" --out "dist/$name.cdx.json" >/dev/null
python3 scripts/sbom.py --check "dist/$name.cdx.json"
echo "dist/$archive"
