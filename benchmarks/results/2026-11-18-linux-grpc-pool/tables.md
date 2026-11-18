### 64 connections (two runs)

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 146,706 | 142,250 | **1.03x** | 1.00, 1.06 | 1.28 / 1.44 ms | 20 / 21 us | 10.6 / 10.8 MB | 4 / 4 |
| REST API with JSON | 119,660 | 126,538 | **0.95x** | 0.98, 0.91 | 1.56 / 1.44 ms | 24 / 22 us | 11.1 / 11.1 MB | 4 / 4 |
| gRPC unary call (protobuf) | 47,282 | 10,174 | **4.65x** | 4.82, 4.48 | 2.52 / 13.96 ms | 57 / 221 us | 49.0 / 14.1 MB | 4 / 4 |
| MCP tool call, stateless | 52,559 | 53,143 | **0.99x** | 1.05, 0.93 | 4.09 / 4.00 ms | 56 / 55 us | 30.7 / 30.7 MB | 4 / 4 |
| MCP tool call, session | 53,887 | 54,468 | **0.99x** | 1.05, 0.93 | 4.07 / 3.96 ms | 52 / 52 us | 30.5 / 30.8 MB | 4 / 4 |

### 256 connections

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 157,310 | 152,183 | **1.03x** | 1.03 | 3.81 / 4.27 ms | 17 / 18 us | 11.2 / 10.8 MB | 2 / 2 |
| gRPC unary call (protobuf) | 45,108 | 10,400 | **4.34x** | 4.34 | 10.22 / 39.06 ms | 56 / 200 us | 168.5 / 20.5 MB | 2 / 2 |
| MCP tool call, stateless | 60,206 | 60,394 | **1.00x** | 1.00 | 11.40 / 11.40 ms | 49 / 49 us | 112.9 / 112.5 MB | 2 / 2 |
| MCP tool call, session | 63,415 | 64,912 | **0.98x** | 0.98 | 11.01 / 10.49 ms | 46 / 45 us | 83.0 / 82.9 MB | 2 / 2 |

