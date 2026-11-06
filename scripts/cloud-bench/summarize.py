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
import math
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
    """a / b, rounded down to one decimal (a published ratio never rounds up)."""
    return "-" if not a or not b else f"{math.floor(a / b * 10) / 10:.1f}x"


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


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def tput(data, side, sc):
    return data.get(side, {}).get("scenarios", {}).get(sc, {}).get("tput")


def html_fragments(data):
    """HTML pieces for the comparison page, keyed by marker name (<!-- bench:NAME -->)."""
    sides = [(k, n) for k, n in SIDES if k in data]
    frag = {}
    # bar charts: one group per scenario, bars scaled to the fastest side
    groups = {
        "bars-inprocess": ["direct", "seda", "seda-inout", "cbr", "splitagg", "csv"],
        "bars-network": ["http", "rest-json", "http-kafka", "http-sql", "kafka-stream"],
    }
    for name, scs in groups.items():
        h = ['<div class="bars">']
        for sc in scs:
            vals = [(k, n, tput(data, k, sc)) for k, n in sides]
            if all(v is None for _, _, v in vals):
                continue
            top = max(v for _, _, v in vals if v)
            unit = UNITS.get(sc, "msg/s")
            h.append(f'<div class="bar-group"><h4>{esc(NAMES[sc])}</h4>')
            for k, n, v in vals:
                if v is None:
                    continue
                cls = {"rust": "bar us", "java": "bar jvm", "native": "bar native"}[k]
                h.append(f'<div class="{cls}"><span>{esc(n)}</span><span class="track"><i class="fill" style="width:{100 * v / top:.1f}%"></i></span><span class="val">{fmt(v)} {unit}</span></div>')
            h.append("</div>")
        h.append("</div>")
        frag[name] = "\n".join(h)
    # tables
    num = ' class="num"'

    def table(head, rows):
        th = "".join("<th%s>%s</th>" % (num if i else "", esc(c)) for i, c in enumerate(head))
        body = "".join("<tr>" + "".join("<td%s>%s</td>" % (num if i else "", c) for i, c in enumerate(r)) + "</tr>" for r in rows)
        return '<div class="c-table-wrap"><table class="c-table"><thead><tr>%s</tr></thead><tbody>%s</tbody></table></div>' % (th, body)
    others = [(k, n) for k, n in sides if k != "rust"]
    rows = []
    for sc in ORDER:
        vals = [tput(data, k, sc) for k, _ in sides]
        if all(v is None for v in vals):
            continue
        unit = UNITS.get(sc, "msg/s")
        rust = tput(data, "rust", sc)
        rows.append([esc(NAMES[sc])] + [f"{fmt(v)} {unit}" if v else "-" for v in vals]
                    + [f'<span class="win">{ratio(rust, tput(data, k, sc))}</span>' if tput(data, k, sc) else "-" for k, _ in others])
    frag["table-throughput"] = table(["scenario"] + [n for _, n in sides] + [SHORT[k] for k, _ in others], rows)
    rows = []
    for sc in ORDER:
        vals = [data[k]["scenarios"].get(sc, {}).get("p99_us") for k, _ in sides]
        if all(v is None for v in vals):
            continue
        rows.append([esc(NAMES[sc])] + ["-" if v is None else (f"{v / 1000:.1f} ms" if v >= 1000 else f"{v:.0f} µs") for v in vals])
    frag["table-latency"] = table(["scenario, p99 latency"] + [n for _, n in sides], rows)
    rows = []
    for sc in ORDER:
        vals = [data[k]["scenarios"].get(sc, {}).get("peak_rss_mb") for k, _ in sides]
        if all(v is None for v in vals):
            continue
        rows.append([esc(NAMES[sc])] + ["-" if v is None else f"{v:.0f} MB" for v in vals])
    frag["table-memory"] = table(["scenario, peak memory"] + [n for _, n in sides], rows)
    # use-case cards: Cameleer's throughput, and how it compares with Camel on the JVM
    for sc in ORDER:
        rust, java = tput(data, "rust", sc), tput(data, "java", sc)
        if rust and java:
            unit = UNITS.get(sc, "msg/s")
            frag[f"card-{sc}"] = f'{ratio(rust, java)} <small>{fmt(rust)} vs {fmt(java)} {unit}</small>'
    frag["table-startup"] = table(["", *[n for _, n in sides]], [
        ["Time to a started context"] + ["-" if data[k]["startup_ms"] is None else (f"{data[k]['startup_ms']:.0f} ms" if data[k]["startup_ms"] >= 10 else f"{data[k]['startup_ms']:.1f} ms") for k, _ in sides],
        ["Memory at startup"] + ["-" if data[k]["startup_rss_mb"] is None else f"{data[k]['startup_rss_mb']:.0f} MB" for k, _ in sides],
    ])
    return frag


def headline(data):
    """The figures quoted in headlines, all against Apache Camel on the JVM."""
    r, j = data["rust"], data["java"]
    tp = [tput(data, "rust", sc) / tput(data, "java", sc) for sc in ORDER if tput(data, "rust", sc) and tput(data, "java", sc)]
    mem = [r["scenarios"][sc]["peak_rss_mb"] and j["scenarios"][sc]["peak_rss_mb"] / r["scenarios"][sc]["peak_rss_mb"]
           for sc in ORDER if sc in r["scenarios"] and sc in j["scenarios"] and r["scenarios"][sc].get("peak_rss_mb") and j["scenarios"][sc].get("peak_rss_mb")]
    return {
        "startup_ms": r["startup_ms"], "java_startup_ms": j["startup_ms"], "startup_x": j["startup_ms"] / r["startup_ms"],
        "tput_min_x": min(tp), "tput_max_x": max(tp), "mem_min_x": min(mem), "mem_max_x": max(mem),
        "max_rss_mb": max(s["peak_rss_mb"] for s in r["scenarios"].values() if s.get("peak_rss_mb")),
    }


def fill(path, frag):
    text = open(path, encoding="utf-8").read()
    for name, body in frag.items():
        pat = re.compile(r"(<!-- bench:%s -->).*?(<!-- /bench:%s -->)" % (re.escape(name), re.escape(name)), re.S)
        text, n = pat.subn(lambda m: m.group(1) + body + m.group(2), text)
        if n == 0:
            continue
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write(text)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--side", action="append", required=True, help="side=results-file")
    ap.add_argument("--json", help="also write the summary as JSON")
    ap.add_argument("--fill", action="append", default=[], help="replace <!-- bench:NAME --> regions in this HTML file")
    a = ap.parse_args()
    sys.stdout.reconfigure(encoding="utf-8")
    files = dict(s.split("=", 1) for s in a.side)
    data = summarize(files)
    if a.json:
        with open(a.json, "w", encoding="utf-8", newline="\n") as f:
            json.dump({"sides": data, "headline": headline(data) if "rust" in data and "java" in data else None}, f, indent=1, sort_keys=True)
    for path in a.fill:
        frag = html_fragments(data)
        h = headline(data)
        # headline numbers always round against Cameleer: its times and memory up, ratios down
        r, j = data["rust"]["scenarios"], data["java"]["scenarios"]
        frag.update({
            "stat-startup-ms": f"{math.ceil(h['startup_ms'])} ms",
            "stat-java-startup": f"{math.floor(h['java_startup_ms'] / 100) / 10:.1f} s",
            "stat-startup-x": f"{int(h['startup_x'] // 50 * 50)}x",
            "stat-tput": f"{math.floor(h['tput_min_x'] * 10) / 10:.1f}–{math.floor(h['tput_max_x'] * 10) / 10:.1f}x",
            "stat-mem": f"{math.floor(h['mem_min_x'] * 10) / 10:.1f}–{math.floor(h['mem_max_x'])}x",
            "stat-mem-max": f"{math.floor(h['mem_max_x'])}x",
            "stat-mem-direct": f"{math.ceil(r['direct']['peak_rss_mb'])} MB vs {math.floor(j['direct']['peak_rss_mb'])} MB",
            "stat-max-rss": f"{math.ceil(h['max_rss_mb'])} MB",
        })
        frag["md-tables"] = "\n" + markdown(data) + "\n"
        fill(path, frag)
    sys.stdout.write(markdown(data))


if __name__ == "__main__":
    main()
