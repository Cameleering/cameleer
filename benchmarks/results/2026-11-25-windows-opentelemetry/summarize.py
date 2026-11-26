import re, sys, statistics as st
rows = {}
spans = {}
for line in open(sys.argv[1]):
    m = re.search(r"side=(\w+) RESULT scenario=(\S+) msgs=(\d+) .*?tput=(\d+).*?p99_us=(\S+)", line)
    if not m:
        continue
    side, sc, msgs, tput, p99 = m.group(1), m.group(2), int(m.group(3)), int(m.group(4)), m.group(5)
    rows.setdefault((sc, side), []).append((tput, p99))
    t = re.search(r"spans_started=(\d+).*?exported=(\d+) dropped=(\d+)", line)
    if t:
        spans.setdefault((sc, side), []).append((int(t.group(1)) / msgs, int(t.group(2)), int(t.group(3)), int(t.group(1))))
sides = ["main", "off", "spans", "otlp"]
print("| scenario | " + " | ".join(sides) + " | off/main | spans/off | otlp/off | spans per msg | ns per span (spans) | exported share (otlp) |")
print("|---|" + "---|" * (len(sides) + 6))
for sc in ["direct", "seda", "cbr", "splitagg"]:
    med = {s: st.median([t for t, _ in rows.get((sc, s), [(0, 0)])]) for s in sides}
    def r(a, b):
        return "%.2fx" % (med[a] / med[b]) if med[b] else "-"
    sp = spans.get((sc, "spans"), [])
    per = st.median([x[0] for x in sp]) if sp else 0
    # cost per span: (1/tput_spans - 1/tput_off) / spans per message
    ns = ((1e9 / med["spans"]) - (1e9 / med["off"])) / per if per and med["spans"] and med["off"] else 0
    op = spans.get((sc, "otlp"), [])
    share = st.median([x[1] / x[3] for x in op]) if op else 0
    print("| %s | %s | %s | %s | %s | %.1f | %.0f | %.0f%% |" % (sc, " | ".join("{:,}".format(int(med[s])) for s in sides), r("off", "main"), r("spans", "off"), r("otlp", "off"), per, ns, share * 100))
print()
for k, v in sorted(rows.items()):
    print(k, [t for t, _ in v])
