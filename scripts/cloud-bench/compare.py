#!/usr/bin/env python3
"""Compare two builds of Cameleer from cloud-bench result files (sides `rust` and `base`).

    python3 scripts/cloud-bench/compare.py "64 connections" results-A.txt results-A2.txt         "256 connections" results-B.txt results-B2.txt > tables.md

Arguments alternate between a label and the result files of that group. The two servers of the
benchmark differ in speed (one was 10 to 25% faster in these rounds) and the two phases differ in
time, so run.sh measures every build on both servers, once in each phase. The figure for a build is
therefore the mean over all of its records in the group, and the ratio is the ratio of those means.
The spread is the ratio computed inside each file that has both builds (a file is one full run). CPU
per request is (user + sys seconds of the server process) / (requests of the warm-up and the timed
iterations, taken at the timed rate): an estimate, the same for both builds. Stdlib only.
"""
import math
import re
import sys
from collections import defaultdict

WARMUP, ITER, ITERS = 15, 5, 5  # the defaults of run.sh (BENCH_WARMUP, BENCH_ITER, BENCH_ITERS)
ACTIVE = WARMUP + ITER * ITERS
NAMES = {
    "http": "HTTP, fixed reply",
    "rest-json": "REST API with JSON",
    "grpc": "gRPC unary call (protobuf)",
    "mcp": "MCP tool call, stateless",
    "mcp-session": "MCP tool call, session",
}
ORDER = ["http", "rest-json", "grpc", "mcp", "mcp-session"]


def fields(line):
    return dict(re.findall(r"(\w+)=(\S+)", line))


def load(path):
    out = defaultdict(dict)  # (scenario, side, node) -> record
    for line in open(path, encoding="utf-8"):
        if not (line.startswith("RESULT") or line.startswith("PROC")):
            continue
        f = fields(line)
        if "scenario" not in f or "side" not in f or "node" not in f:
            continue
        rec = out[(f["scenario"], f["side"], f["node"])]
        if line.startswith("RESULT"):
            rec["tput"] = float(f["tput"])
            rec["p99"] = float(f["p99_us"]) / 1000.0
        else:
            rec["rss"] = float(f["peak_rss_kb"]) / 1024.0
            rec["cpu"] = float(f["user_s"]) + float(f["sys_s"])
    return out


def mean(xs):
    return sum(xs) / len(xs)


def per_record(rec):
    """One measurement: throughput, p99 (ms), CPU per request (us), peak memory (MB)."""
    out = {"tput": rec["tput"], "p99": rec["p99"]}
    if "cpu" in rec:
        out["cpu"] = rec["cpu"] / (rec["tput"] * ACTIVE) * 1e6
        out["rss"] = rec["rss"]
    return out


def group(label, files):
    sides = defaultdict(lambda: defaultdict(list))  # scenario -> side -> [measurement]
    per_file = defaultdict(list)  # scenario -> [ratio inside one file]
    for path in files:
        recs = load(path)
        in_file = defaultdict(lambda: defaultdict(list))
        for (scenario, side, node), rec in recs.items():
            if "tput" not in rec or side not in ("rust", "base"):
                continue
            m = per_record(rec)
            sides[scenario][side].append(m)
            in_file[scenario][side].append(m["tput"])
        for scenario, d in in_file.items():
            if d["rust"] and d["base"] and len(d["rust"]) == len(d["base"]):
                per_file[scenario].append(mean(d["rust"]) / mean(d["base"]))
    print(f"### {label}\n")
    print("| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |")
    print("|---|---|---|---|---|---|---|---|---|")
    for sc in ORDER:
        if sc not in sides or not sides[sc]["rust"] or not sides[sc]["base"]:
            continue
        n, b = sides[sc]["rust"], sides[sc]["base"]
        col = lambda ms, k: mean([m[k] for m in ms if k in m])
        runs = ", ".join(f"{r:.2f}" for r in per_file[sc]) or "-"
        print(
            f"| {NAMES[sc]} | {col(n, 'tput'):,.0f} | {col(b, 'tput'):,.0f} | **{col(n, 'tput') / col(b, 'tput'):.2f}x** | {runs} "
            f"| {col(n, 'p99'):.2f} / {col(b, 'p99'):.2f} ms | {col(n, 'cpu'):.0f} / {col(b, 'cpu'):.0f} us | {col(n, 'rss'):.1f} / {col(b, 'rss'):.1f} MB | {len(n)} / {len(b)} |"
        )
    print()


def main(argv):
    groups, label = [], None
    for a in argv:
        if a.endswith(".txt"):
            if label is None:
                sys.exit("a label must come before the files")
            groups[-1][1].append(a)
        else:
            label = a
            groups.append((a, []))
    for lab, files in groups:
        group(lab, files)


if __name__ == "__main__":
    main(sys.argv[1:])
