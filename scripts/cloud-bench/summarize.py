#!/usr/bin/env python3
"""Summarize cloud-bench results (scripts/cloud-bench/run.sh) into markdown tables and JSON.

    python3 scripts/cloud-bench/summarize.py --side rust=results-3.txt --side java=results-2.txt \
        --side native=results-3.txt --json summary.json > tables.md

Each --side takes that side's records from one results file, so sides measured in different runs
on the same droplets can be combined. Every figure is the mean of the phases (the two servers).
Stdlib only.
"""
import argparse
import json
import re
import sys
from collections import defaultdict

ORDER = ["direct", "seda", "seda-inout", "cbr", "splitagg", "csv", "kafka-stream", "http", "rest-json", "http-kafka", "http-sql"]
NAMES = {
    "direct": "Direct call through a route",
    "seda": "In-memory queue (seda), one-way",
    "seda-inout": "In-memory queue (seda), request-reply",
    "cbr": "Content-based router",
    "splitagg": "Split and aggregate a JSON order",
    "csv": "CSV batch (1,000-row files)",
    "kafka-stream": "Kafka stream processing",
    "http": "HTTP, fixed reply",
    "rest-json": "REST API with JSON",
    "http-kafka": "HTTP into Kafka",
    "http-sql": "HTTP into PostgreSQL",
}
UNITS = {"splitagg": "orders/s", "csv": "files/s", "kafka-stream": "records/s", "http": "req/s", "rest-json": "req/s", "http-kafka": "req/s", "http-sql": "req/s"}
SIDES = [("rust", "Cameleer"), ("java", "Apache Camel (JVM)"), ("native", "Apache Camel (Quarkus native)")]
SHORT = {"java": "Cameleer vs Camel JVM", "native": "Cameleer vs Camel native"}


def parse(path, side):
    """Records of one side: {kind: [dict]}."""
    out = defaultdict(list)
    for line in open(path, encoding="utf-8"):
        kind = line.split(" ", 1)[0]
        kv = dict(re.findall(r"(\w+)=(\S+)", line))
        if kv.get("side") == side:
            out[kind].append(kv)
    return out


def mean(v):
    return sum(v) / len(v) if v else None


def summarize(files):
    data = {}
    for side, path in files.items():
        recs = parse(path, side)
        s = {"scenarios": {}, "startup_ms": None, "startup_rss_mb": None}
        for sc in ORDER:
            res = [r for r in recs["RESULT"] if r.get("scenario") == sc and "tput" in r]
            if not res:
                continue
            procs = [r for r in recs["PROC"] if r.get("scenario") == sc]
            rss = [r for r in recs["RSS"] if r.get("scenario") == sc]
            p99 = [float(r["p99_us"]) for r in res if r.get("p99_us", "-") != "-"]
            entry = {
                "tput": mean([float(r["tput"]) for r in res]),
                "tput_phases": [float(r["tput"]) for r in res],
                "p99_us": mean(p99),
                "peak_rss_mb": mean([int(r["peak_rss_kb"]) / 1024 for r in procs]) or mean([int(r["peak_kb"]) / 1024 for r in rss]),
                "cpu_pct": mean([float(r["cpu_pct"].rstrip("%")) for r in procs]),
            }
            if entry["cpu_pct"]:
                entry["per_core"] = entry["tput"] / (entry["cpu_pct"] / 100)
            s["scenarios"][sc] = entry
        st = recs["STARTUP"]
        if st:
            s["startup_ms"] = mean([float(r["ms"]) for r in st])
        rs = [r for r in recs["RSS"] if r.get("scenario") == "startup"]
        if rs:
            s["startup_rss_mb"] = mean([int(r["peak_kb"]) / 1024 for r in rs])
        data[side] = s
    return data


def fmt(v, unit=""):
    if v is None:
        return "-"
    if v >= 100_000:
        return "%s" % f"{v:,.0f}"
    if v >= 100:
        return f"{v:,.0f}"
    return f"{v:,.1f}"


def ratio(a, b):
    return "-" if not a or not b else f"{a / b:.1f}x"


def markdown(data):
    sides = [(k, n) for k, n in SIDES if k in data]
    out = []
    head = "| scenario | " + " | ".join(n for _, n in sides) + " | " + " | ".join(SHORT[k] for k, n in sides if k != "rust") + " |"
    out.append("### Throughput (higher is better)\n")
    out.append(head)
    out.append("|---|" + "---:|" * (len(sides) + len(sides) - 1))
    for sc in ORDER:
        row = [data[k]["scenarios"].get(sc, {}).get("tput") for k, _ in sides]
        if all(v is None for v in row):
            continue
        unit = UNITS.get(sc, "msg/s")
        rust = data.get("rust", {}).get("scenarios", {}).get(sc, {}).get("tput")
        cells = [f"{fmt(v)} {unit}" if v else "-" for v in row]
        ratios = [ratio(rust, data[k]["scenarios"].get(sc, {}).get("tput")) for k, _ in sides if k != "rust"]
        out.append(f"| {NAMES[sc]} | " + " | ".join(cells) + " | " + " | ".join(ratios) + " |")
    out.append("\n### Latency, p99 (lower is better)\n")
    out.append("| scenario | " + " | ".join(n for _, n in sides) + " |")
    out.append("|---|" + "---:|" * len(sides))
    for sc in ORDER:
        row = [data[k]["scenarios"].get(sc, {}).get("p99_us") for k, _ in sides]
        if all(v is None for v in row):
            continue
        cells = ["-" if v is None else (f"{v / 1000:.1f} ms" if v >= 1000 else f"{v:.0f} µs") for v in row]
        out.append(f"| {NAMES[sc]} | " + " | ".join(cells) + " |")
    out.append("\n### Memory and CPU\n")
    out.append("| scenario | " + " | ".join(f"{n}: peak memory" for _, n in sides) + " | " + " | ".join(f"{n}: CPU" for _, n in sides) + " |")
    out.append("|---|" + "---:|" * (2 * len(sides)))
    for sc in ORDER:
        mem = [data[k]["scenarios"].get(sc, {}).get("peak_rss_mb") for k, _ in sides]
        cpu = [data[k]["scenarios"].get(sc, {}).get("cpu_pct") for k, _ in sides]
        if all(v is None for v in mem):
            continue
        out.append(f"| {NAMES[sc]} | " + " | ".join("-" if v is None else f"{v:.0f} MB" for v in mem) + " | "
                   + " | ".join("-" if v is None else f"{v:.0f}%" for v in cpu) + " |")
    out.append("\n### Startup\n")
    out.append("| | " + " | ".join(n for _, n in sides) + " |")
    out.append("|---|" + "---:|" * len(sides))
    out.append("| time to a started context | " + " | ".join("-" if data[k]["startup_ms"] is None else f"{data[k]['startup_ms']:.1f} ms" for k, _ in sides) + " |")
    out.append("| memory at startup | " + " | ".join("-" if data[k]["startup_rss_mb"] is None else f"{data[k]['startup_rss_mb']:.0f} MB" for k, _ in sides) + " |")
    return "\n".join(out) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--side", action="append", required=True, help="side=results-file")
    ap.add_argument("--json", help="also write the summary as JSON")
    a = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    files = dict(s.split("=", 1) for s in a.side)
    data = summarize(files)
    if a.json:
        with open(a.json, "w", encoding="utf-8", newline="\n") as f:
            json.dump(data, f, indent=1, sort_keys=True)
    sys.stdout.write(markdown(data))


if __name__ == "__main__":
    main()
