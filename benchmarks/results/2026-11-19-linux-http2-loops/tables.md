### 64 connections

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| REST API with JSON | 98,642 | 104,920 | **0.94x** | 0.94 | 2.23 / 2.34 ms | 29 / 29 us | 9.8 / 9.8 MB | 2 / 2 |
| gRPC unary call (protobuf) | 61,134 | 58,635 | **1.04x** | 1.04 | 2.31 / 2.31 ms | 35 / 28 us | 14.1 / 14.1 MB | 2 / 2 |
| MCP tool call, stateless | 53,094 | 53,578 | **0.99x** | 0.99 | 3.96 / 4.17 ms | 52 / 55 us | 21.9 / 22.3 MB | 2 / 2 |
| MCP tool call, session | 55,420 | 54,670 | **1.01x** | 1.01 | 3.92 / 4.15 ms | 50 / 51 us | 21.9 / 22.7 MB | 2 / 2 |

