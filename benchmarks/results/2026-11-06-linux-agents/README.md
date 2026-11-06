# Linux cloud benchmark, action layer for AI agents, 2026-11-06

Raw records behind the "Action layer and MCP" section of
[docs/BENCHMARKS.md](../../../docs/BENCHMARKS.md), made with
[scripts/cloud-bench](../../../scripts/cloud-bench/) on three DigitalOcean `c-4` droplets
(4 dedicated vCPUs of an Intel Xeon Platinum 8280, 8 GB, Ubuntu 24.04, `blr1`); the servers swap
roles halfway (phase 1 and 2).

| file | sides | commit |
|---|---|---|
| `results-all-sides.txt` | Cameleer (`rust`), Cameleer `main` at `5146f8d` (`base`), Apache Camel 4.23.0 on Java 21 (`java`, including `camel-mcp-server`), Camel Quarkus 3.40.0 native (`native`), a hand-written server on the official MCP TypeScript SDK 1.32.2 on Node.js 22.23.4 (`node`) | `3dd6501` |
| `results-cameleer-rerun.txt` | Cameleer only, after the envelope and MCP worker changes | `575d460` |
| `mcp-final.txt` | Cameleer MCP scenarios after the session-expiry change | `7ea0b91` |
| `ceiling.txt` | a do-nothing HTTP server without Camel (`cameleer-bench raw-http`), the ceiling of the load generator and socket model | `7ea0b91` |
| `sizes.txt` | `cameleer` CLI binary size and cold start, this branch vs `main` | `7ea0b91` |

The Cameleer rows of `results-all-sides.txt` for `ai-tool`, `mcp` and `mcp-session` are from the
build before the envelope fixes (a thread per tool call); the reruns supersede them.

`sizes.txt` is from `7ea0b91`. After `c4599aa` (the `agent` feature of cameleer-core) the
stripped `core` build is 5,931,840 bytes against 5,869,104 for `main`, with the same cold start
(1.33 ms median of 60 runs each, alternating) and peak RSS (6.1 MB).
