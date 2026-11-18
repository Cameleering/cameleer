### 64 connections (two runs)

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 146,477 | 146,770 | **1.00x** | 1.02, 0.98 | 1.42 / 1.52 ms | 20 / 20 us | 10.4 / 10.6 MB | 4 / 4 |
| REST API with JSON | 123,793 | 130,956 | **0.95x** | 0.94, 0.95 | 1.56 / 1.45 ms | 23 / 22 us | 10.9 / 11.1 MB | 4 / 4 |
| gRPC unary call (protobuf) | 10,472 | 9,926 | **1.06x** | 1.05, 1.06 | 10.85 / 16.66 ms | 204 / 287 us | 13.1 / 16.3 MB | 4 / 4 |
| MCP tool call, stateless | 54,179 | 54,187 | **1.00x** | 0.99, 1.01 | 4.16 / 4.08 ms | 54 / 54 us | 30.6 / 30.5 MB | 4 / 4 |
| MCP tool call, session | 55,816 | 56,082 | **1.00x** | 0.99, 1.00 | 4.10 / 3.96 ms | 51 / 50 us | 30.8 / 30.6 MB | 4 / 4 |

### 256 connections

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 145,191 | 148,790 | **0.98x** | 0.98 | 5.90 / 4.24 ms | 19 / 19 us | 10.9 / 10.9 MB | 2 / 2 |
| gRPC unary call (protobuf) | 10,328 | 9,882 | **1.05x** | 1.05 | 39.06 / 44.83 ms | 199 / 284 us | 18.9 / 17.5 MB | 2 / 2 |
| MCP tool call, stateless | 58,032 | 58,405 | **0.99x** | 0.99 | 12.19 / 11.67 ms | 51 / 50 us | 112.9 / 112.6 MB | 2 / 2 |
| MCP tool call, session | 63,274 | 62,846 | **1.01x** | 1.01 | 11.08 / 11.34 ms | 46 / 46 us | 82.9 / 82.9 MB | 2 / 2 |

