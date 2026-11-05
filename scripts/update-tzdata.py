#!/usr/bin/env python3
"""Refresh the bundled IANA time-zone database in crates/cameleer-core/tzdata/.

The zones come from the `tzdata` package on PyPI, maintained by the Python core
team: the IANA tz database compiled to TZif files with the reference `zic`. That
avoids needing `zic` on Windows. The wheel's SHA-256 is pinned below and also
checked against the digest PyPI publishes. A mismatch aborts without writing.

To update, set TZDATA_VERSION to the new release from https://pypi.org/project/tzdata/,
run `python3 scripts/update-tzdata.py --print-digest`, check the digest against the PyPI
page, put it in TZDATA_SHA256, run the script again, then run `cargo test -p cameleer-core tz`.

Output (both committed):
  crates/cameleer-core/tzdata/zones.bin  - all zones packed (format: see cameleer-core/src/tz.rs)
  crates/cameleer-core/tzdata/VERSION    - IANA release (e.g. 2026f) and the source wheel
Standard library only.
"""

import hashlib
import io
import json
import os
import struct
import sys
import urllib.request
import zipfile

TZDATA_VERSION = "2026.6"
TZDATA_SHA256 = "0a10eec6e13ef16b627fabed9a5f2ba61a62b1c67f327305f4e7e412a70b4aec"

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "crates", "cameleer-core", "tzdata")
MAGIC = b"CTZ1"


def fetch(url):
    with urllib.request.urlopen(url, timeout=60) as r:
        return r.read()


def wheel_info():
    meta = json.loads(fetch(f"https://pypi.org/pypi/tzdata/{TZDATA_VERSION}/json"))
    for u in meta["urls"]:
        if u["packagetype"] == "bdist_wheel":
            return u["url"], u["filename"], u["digests"]["sha256"]
    sys.exit(f"no wheel published for tzdata {TZDATA_VERSION}")


def main():
    url, filename, pypi_digest = wheel_info()
    if "--print-digest" in sys.argv:
        print(f"{filename} {pypi_digest}")
        return
    if pypi_digest != TZDATA_SHA256:
        sys.exit(f"PyPI digest {pypi_digest} differs from the pinned {TZDATA_SHA256}")
    data = fetch(url)
    digest = hashlib.sha256(data).hexdigest()
    if digest != TZDATA_SHA256:
        sys.exit(f"downloaded {filename} has SHA-256 {digest}, expected {TZDATA_SHA256}")

    zones = {}
    iana = None
    prefix = "tzdata/zoneinfo/"
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        for name in z.namelist():
            if name == "tzdata/__init__.py":
                for line in z.read(name).decode().splitlines():
                    if line.startswith("IANA_VERSION"):
                        iana = line.split("=", 1)[1].strip().strip("\"'")
            if not name.startswith(prefix) or name.endswith("/"):
                continue
            body = z.read(name)
            if body[:4] == b"TZif":
                zones[name[len(prefix):]] = body
    if not iana or len(zones) < 300:
        sys.exit(f"unexpected wheel contents: IANA version {iana!r}, {len(zones)} zones")

    out = io.BytesIO()
    out.write(MAGIC)
    out.write(struct.pack(">I", len(zones)))
    for zone in sorted(zones):
        n = zone.encode("ascii")
        out.write(struct.pack(">H", len(n)))
        out.write(n)
        out.write(struct.pack(">I", len(zones[zone])))
        out.write(zones[zone])
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "zones.bin"), "wb") as f:
        f.write(out.getvalue())
    with open(os.path.join(OUT, "VERSION"), "w", newline="\n") as f:
        f.write(f"{iana}\n{filename} sha256:{digest}\n")
    print(f"IANA {iana}: {len(zones)} zones, {out.tell()} bytes -> {os.path.relpath(OUT, ROOT)}")


if __name__ == "__main__":
    main()
