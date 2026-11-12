#!/usr/bin/env python3
"""Write a CycloneDX 1.5 SBOM (JSON) for a Cameleer crate or distribution.

The crates and their edges come from `cargo tree -p <package> -e normal` with the same feature
flags as the build (cargo tree resolves features like `cargo build -p`, unlike `cargo metadata`,
which unifies the whole workspace), so the SBOM lists exactly the crates compiled in: Cameleer is
std-only, so that is `cameleer-*` crates of this workspace, plus the Rust toolchain and its
standard library (from `rustc -vV`). Descriptions and licences come from `cargo metadata`.
Standard library only; no third-party tools.

  scripts/sbom.py --package cameleer-main --out dist/cameleer.cdx.json                     # general distribution
  scripts/sbom.py --package cameleer-main --features full,health --out dist/cameleer-health.cdx.json
  scripts/sbom.py --package cameleer-health --out dist/cameleer-health-crate.cdx.json      # the pack crate
  scripts/sbom.py --package cameleer-main --no-default-features --features health --target x86_64-unknown-linux-gnu

Timestamps come from SOURCE_DATE_EPOCH when set (reproducible builds), and the serial number is
derived from the package, version and feature set, so the same inputs give the same file.
`--check FILE` re-reads an SBOM and checks its structure (used by the tests and CI).
"""

import argparse
import datetime
import hashlib
import json
import os
import subprocess
import sys
import uuid

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SPEC = "1.5"


def run(cmd):
    return subprocess.run(cmd, cwd=ROOT, check=True, capture_output=True, text=True).stdout


def metadata():
    return json.loads(run(["cargo", "metadata", "--format-version", "1", "--locked", "--no-deps"]))


def tree(package, features, no_default, target):
    """(name, version) of the root, the set of (name, version) compiled in, and the edges."""
    cmd = ["cargo", "tree", "--locked", "-p", package, "-e", "normal", "--prefix", "depth", "--format", "{p}"]
    if no_default:
        cmd.append("--no-default-features")
    if features:
        cmd += ["--features", features]
    if target:
        cmd += ["--target", target]
    stack, crates, edges, root = [], set(), {}, None
    for line in run(cmd).splitlines():
        i = 0
        while i < len(line) and line[i].isdigit():
            i += 1
        if i == 0:
            continue
        depth = int(line[:i])
        parts = line[i:].split()
        key = (parts[0], parts[1].lstrip("v"))
        crates.add(key)
        edges.setdefault(key, set())
        del stack[depth:]
        if stack:
            edges[stack[-1]].add(key)
        else:
            root = key
        stack.append(key)
    return root, crates, edges


def toolchain():
    info = {}
    for line in run(["rustc", "-vV"]).splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            info[k.strip()] = v.strip()
    return info


def file_sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        h.update(f.read())
    return h.hexdigest()


def build(args):
    meta = metadata()
    by_name = {p["name"]: p for p in meta["packages"]}
    if args.package not in by_name:
        sys.exit("no workspace package named %s" % args.package)
    root_key, crates, edges = tree(args.package, args.features, args.no_default_features, args.target)
    root = by_name[root_key[0]]
    tc = toolchain()

    def ref(p):
        return "pkg:cargo/%s@%s" % (p["name"], p["version"])

    def component(p, kind="library"):
        c = {
            "type": kind,
            "bom-ref": ref(p),
            "name": p["name"],
            "version": p["version"],
            "purl": ref(p),
            "description": (p.get("description") or "").strip(),
        }
        if p.get("license"):
            c["licenses"] = [{"expression": p["license"]}]
        manifest = p.get("manifest_path")
        if manifest and os.path.exists(manifest):
            c["hashes"] = [{"alg": "SHA-256", "content": file_sha256(manifest)}]
            c["properties"] = [{"name": "cameleer:manifest-sha256", "value": "Cargo.toml"}]
        if p.get("repository"):
            c["externalReferences"] = [{"type": "vcs", "url": p["repository"]}]
        return c

    def pkg(key):
        # a workspace crate has full metadata; anything else (none today) is described by name
        return by_name.get(key[0]) or {"name": key[0], "version": key[1], "description": "", "license": None}

    deps = sorted((pkg(k) for k in crates if k != root_key), key=lambda p: p["name"])
    third_party = [p for p in deps if not p["name"].startswith("cameleer-")]
    rust = {
        "type": "framework",
        "bom-ref": "pkg:generic/rust-std@%s" % tc.get("release", "unknown"),
        "name": "rust-std",
        "version": tc.get("release", "unknown"),
        "description": "The Rust standard library, from the toolchain that built the binary",
        "licenses": [{"expression": "MIT OR Apache-2.0"}],
        "properties": [{"name": "rustc:commit-hash", "value": tc.get("commit-hash", "")}, {"name": "rustc:host", "value": tc.get("host", "")}],
    }
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    when = datetime.datetime.fromtimestamp(int(epoch), datetime.timezone.utc) if epoch else datetime.datetime.now(datetime.timezone.utc)
    key = "%s@%s;%s;%s;%s" % (root["name"], root["version"], args.features or "", args.no_default_features, args.target or "")
    name = args.name or root["name"]
    bom = {
        "bomFormat": "CycloneDX",
        "specVersion": SPEC,
        "serialNumber": "urn:uuid:%s" % uuid.uuid5(uuid.NAMESPACE_URL, "https://cameleer.ing/sbom/" + key),
        "version": 1,
        "metadata": {
            "timestamp": when.replace(microsecond=0).isoformat().replace("+00:00", "Z"),
            "tools": {"components": [{"type": "application", "name": "scripts/sbom.py", "version": root["version"]}]},
            "component": dict(component(root, "application" if args.package == "cameleer-main" else "library"), name=name),
            "properties": [
                {"name": "cargo:features", "value": args.features or ("" if args.no_default_features else "default")},
                {"name": "cargo:no-default-features", "value": str(args.no_default_features).lower()},
                {"name": "cargo:target", "value": args.target or tc.get("host", "")},
                {"name": "cameleer:third-party-crates", "value": str(len(third_party))},
            ],
        },
        "components": [component(p) for p in deps] + [rust],
        "dependencies": [
            {"ref": "pkg:cargo/%s@%s" % k, "dependsOn": sorted("pkg:cargo/%s@%s" % d for d in edges.get(k, ()))}
            for k in sorted(crates)
        ],
    }
    return bom


def check(path):
    with open(path, encoding="utf-8") as f:
        bom = json.load(f)
    errors = []
    if bom.get("bomFormat") != "CycloneDX" or bom.get("specVersion") != SPEC:
        errors.append("not a CycloneDX %s document" % SPEC)
    refs = {c["bom-ref"] for c in bom.get("components", [])}
    refs.add(bom["metadata"]["component"]["bom-ref"])
    for c in bom.get("components", []):
        for k in ("type", "name", "version", "bom-ref"):
            if not c.get(k):
                errors.append("component without %s: %s" % (k, c))
    for d in bom.get("dependencies", []):
        for r in [d["ref"]] + d.get("dependsOn", []):
            if r not in refs:
                errors.append("dependency on an unknown component: %s" % r)
    if errors:
        sys.exit("\n".join(errors))
    print("%s: %d components, %d dependency entries" % (path, len(bom["components"]), len(bom["dependencies"])))


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--package", default="cameleer-main")
    ap.add_argument("--features", default="")
    ap.add_argument("--no-default-features", action="store_true")
    ap.add_argument("--target", default="")
    ap.add_argument("--name", default="", help="name of the top component (default: the package)")
    ap.add_argument("--out", default="-")
    ap.add_argument("--check", metavar="FILE", help="check an SBOM instead of writing one")
    args = ap.parse_args()
    if args.check:
        check(args.check)
        return
    text = json.dumps(build(args), indent=2, sort_keys=False) + "\n"
    if args.out == "-":
        sys.stdout.write(text)
    else:
        os.makedirs(os.path.dirname(os.path.abspath(args.out)), exist_ok=True)
        with open(args.out, "w", encoding="utf-8", newline="\n") as f:
            f.write(text)
        print(args.out)


if __name__ == "__main__":
    main()
