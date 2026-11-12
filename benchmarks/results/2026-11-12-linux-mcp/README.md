# MCP comparison rerun and the TypeScript baseline, 2026-11-12

Raw records behind the "Action layer and MCP" section of
[docs/BENCHMARKS.md](../../../docs/BENCHMARKS.md). Three DigitalOcean `c-4` droplets (4 dedicated
vCPUs of an Intel Xeon Platinum 8280, 8 GB, Ubuntu 24.04, `blr1`), two servers that swap roles
(phase 1 and 2) and a load node, as in `scripts/cloud-bench`; 64 connections, 15 s warmup,
5 x 5 s. Cameleer 0.5.1 at `306a5df`, Apache Camel 4.23.0 on Java 21, the TypeScript SDK 1.32.2 on
Node.js 22.23.4 with zod 4.6.7.

| file | what |
|---|---|
| `results.txt`, `machine.txt` | Camel, Cameleer and the TypeScript SDK in one session: `mcp-session` on all three, the stateless `mcp` scenario on Cameleer only |
| `run-mcp-compare.sh`, `trim.py` | the script that made them: creates the droplets, builds, measures, deletes the droplets (`DO_TOKEN_FILE` names a file with a DigitalOcean API token) |
| `zod-ab/` | the TypeScript baseline with zod 3.25.76 and zod 4.6.7 on one droplet, alternating, three rounds each; `run-zod-ab.sh` and the small scripts it ships to the droplet |

Means of the two phases (peak memory in MiB, as `peak_rss_kb / 1024`):

| scenario | Cameleer | Camel `camel-mcp-server` | TypeScript SDK |
|---|---:|---:|---:|
| `mcp-session`, calls/s | 39,922 (41,210 and 38,634) | 22,604 (20,365 and 24,842) | 2,257 (2,150 and 2,363) |
| p99, ms | 6.3 | 5.2 | 54.8 |
| peak memory, MiB | 55.8 | 288.8 | 197.4 |
| stateless `mcp`, calls/s | 50,514 (52,501 and 48,526) | not offered | not offered |

`bench-node.sh` stops without a message when `benchmarks/camel-quarkus/target/*-runner` is missing
(`ls | head -1` under `pipefail`). This run skipped the Quarkus native build, so the script
creates an empty placeholder runner; a first attempt without it measured nothing.
