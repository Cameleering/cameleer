# 0.8.0 candidate against 0.7.0, and against an independent client, 2026-11-20

Session A of section 1.9 of `docs/design/docs-review-2026-11-20.md`. Raw records, `machine.txt` and `tables.md`
(made by `scripts/cloud-bench/compare.py`, plus the per-server table at the end of `tables.md`).

**Setup.** Three DigitalOcean `c-4` droplets in **`sfo2`** (4 dedicated vCPUs, Xeon Platinum 8168 on the servers and
8280 on the load node, 8 GB, Ubuntu 24.04.5, Rust 1.99.1). `sfo3`, used on 2026-11-19, no longer offers `c-4`; only
`ams3`, `lon1` and `sfo2` do, so absolute figures differ from earlier rounds and only in-session comparisons count.
Two servers that swap roles, a load node, 15 s warm-up, 5 x 5 s, 64 connections (256 once), `BENCH_H2_CONNS=4`.

**Builds.**
- *new:* local branch `bench-0.8.0-candidate` at `7ee75e7` = `origin/main` (`ec33695`) + PR 67 (`692eda8`, simoom 0.2.0,
  eager loops, cheaper gRPC unary) + PR 68 (`f212a04`, header copies). Real `Cargo.lock` (simoom 0.2.0), no `[patch]`.
  Not pushed.
- *old:* local branch `bench-base-0.7.0` at `0b2b68c` = tag `v0.7.0` + the benchmark scenarios of `main` (`crates/cameleer-bench`,
  `scripts/cloud-bench`). simoom 0.1.0. **0.7.0 does not contain the gRPC pool fix (PR 65)**, so the gRPC ratio
  below is "0.8.0 candidate against 0.7.0", not against `main`.
- The load generator is the candidate's `cameleer-bench` for both sides (simoom-client 0.2.0 as the HTTP/2 client).

**Passes.** `64-run1` (rust then base), `64-run2-swapped` (base then rust, the order-effect check), `256`. Then `sizes`
(`sizes.txt`: 4 rounds x 20 cold starts per build) and `peer.sh` twice (`peer-1.txt`, `peer-2.txt`: h2load,
nghttp2 1.59.0, against the candidate). No case failed. h2load over TLS (ALPN `h2`) was **not** run (there is no TLS
route in `peer.sh`); gRPC streaming not measured (no scenario).

## Results (new / 0.7.0)

| | 64 conn run 1 | 64 conn swapped | 256 conn |
|---|---|---|---|
| `http` | 0.86x | 0.92x | 0.97x |
| `rest-json` | 1.03x | 0.99x | 1.03x |
| `rest-json-h2` | 1.00x | 1.02x | 0.94x |
| `grpc` | 11.5x | 11.2x | 13.3x |
| MCP stateless | 0.96x | 0.96x | 0.98x |
| MCP session | 0.97x | 0.97x | 0.97x |

Absolute, new build: `http` 85k to 106k req/s, `rest-json` 78k to 87k, `rest-json-h2` 79k to 94k (the same as
HTTP/1.1), `grpc` 94k (64) and 106k (256) calls/s at 1.2 ms and 4.7 ms p99 (0.7.0: 8.3k and 8.0k calls/s, 14.8 and
54 ms), 24 us of server CPU per gRPC call (0.7.0: 270 us), peak memory 9.2 MB (0.7.0: 13.6), 10.2 against 19.3 MB at 256.

Independent client (`peer-1/2.txt`, h2load, same server, requests/s over the two servers):
HTTP/1.1 64 connections 67k to 92k (mean about 80k); HTTP/2 4x16 116k to 126k; HTTP/2 16x4 121k to 127k; **gRPC 4x16
129k to 135k**. cameleer-bench's own generator on the same build: `rest-json` 67k to 83k, `rest-json-h2` 71k to 86k,
`grpc` 94k. So the server serves HTTP/2 at 1.5x to 1.6x of its HTTP/1.1 rate under h2load and gRPC at 1.4x the rate
cameleer-bench's generator reaches. One h2load run had 8 failed requests out of 3.07 million (`peer-1.txt`, node b,
h2-c16m4).

Sizes (`sizes.txt`, mean of 4 x 20 starts; new / 0.7.0): `full` binary 30,219,544 / 29,979,312 bytes (+0.8%),
stripped 26.72 / 26.52 MB; `core` 8,416,632 / 8,210,640 (+2.5%). Cold start to ready: `full` http route 3.02 / 2.72 ms,
timer 3.38 / 3.31 ms; `core` http 2.45 / 1.99 ms, timer 2.60 / 2.51 ms. Peak RSS: `full` http 11.3 / 10.9 MB, `core`
6.8 / 6.5 MB. This is the cost of the eager event loops (one per core) that PR 67 adds, about 0.3 to 0.5 ms and 0.3 MB.

## Noise and order effects

There is no identical-code control in this round (the two builds differ in the server). Noise is estimated from the
swapped pass and the per-server ratios in `tables.md`: `http` moves 0.84 to 1.02 per server and between passes, so
its 0.89x mean over both 64 connection passes (0.97x at 256) is not distinguishable from noise; `rest-json` 0.96 to
1.05 (mean 1.01x, no loss, the 5% of earlier rounds is not there); `rest-json-h2` 0.80 to 1.31 per server, mean 1.00x.
MCP is tight (0.94 to 0.99 per server, 0.96x to 0.98x every pass, sides in either order), so a 2% to 4% MCP
slowdown is probably real; this round cannot say whether eager loops or the header change causes it.
Side order did not change a ratio beyond that noise. `connects` in the generator's records is 2,400 to 4,500 for `http`/`rest-json`
over 64 connections for both builds (connection churn in the generator, not new in the candidate).
