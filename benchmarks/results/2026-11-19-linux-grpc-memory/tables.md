### 64 connections, new against 0.7.0 (run 1)

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 116,325 | 123,545 | **0.94x** | 0.94 | 1.96 / 1.59 ms | 24 / 23 us | 9.2 / 10.6 MB | 2 / 2 |
| REST API with JSON | 109,794 | 122,312 | **0.90x** | 0.90 | 1.56 / 1.42 ms | 27 / 23 us | 13.6 / 11.0 MB | 2 / 2 |
| gRPC unary call (protobuf) | 41,020 | 10,514 | **3.90x** | 3.90 | 3.96 / 12.71 ms | 37 / 214 us | 13.5 / 13.8 MB | 2 / 2 |
| MCP tool call, stateless | 53,277 | 53,939 | **0.99x** | 0.99 | 4.14 / 3.87 ms | 55 / 55 us | 21.8 / 30.4 MB | 2 / 2 |
| MCP tool call, session | 54,966 | 55,696 | **0.99x** | 0.99 | 4.07 / 4.01 ms | 52 / 51 us | 22.1 / 30.4 MB | 2 / 2 |

### 64 connections, new against 0.7.0 (run 2)

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 138,231 | 132,289 | **1.04x** | 1.04 | 0.86 / 0.82 ms | 19 / 19 us | 8.9 / 9.9 MB | 2 / 2 |
| REST API with JSON | 119,501 | 125,496 | **0.95x** | 0.95 | 1.42 / 1.31 ms | 23 / 23 us | 9.4 / 11.0 MB | 2 / 2 |
| gRPC unary call (protobuf) | 41,712 | 10,540 | **3.96x** | 3.96 | 3.98 / 10.75 ms | 35 / 205 us | 12.9 / 13.5 MB | 2 / 2 |
| MCP tool call, stateless | 53,634 | 53,631 | **1.00x** | 1.00 | 4.22 / 3.97 ms | 55 / 55 us | 22.3 / 30.6 MB | 2 / 2 |
| MCP tool call, session | 54,772 | 55,528 | **0.99x** | 0.99 | 4.38 / 3.91 ms | 51 / 50 us | 22.4 / 30.8 MB | 2 / 2 |

### 256 connections, new against 0.7.0

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 122,929 | 128,132 | **0.96x** | 0.96 | 5.77 / 5.47 ms | 21 / 20 us | 10.8 / 15.2 MB | 2 / 2 |
| gRPC unary call (protobuf) | 36,266 | 10,194 | **3.56x** | 3.56 | 23.07 / 41.16 ms | 38 / 204 us | 18.6 / 20.1 MB | 2 / 2 |
| MCP tool call, stateless | 58,892 | 59,388 | **0.99x** | 0.99 | 11.93 / 11.47 ms | 50 / 50 us | 81.2 / 112.3 MB | 2 / 2 |
| MCP tool call, session | 62,494 | 62,207 | **1.00x** | 1.00 | 11.40 / 11.34 ms | 46 / 47 us | 55.8 / 83.2 MB | 2 / 2 |

### 64 connections, new against the pool build (order swapped)

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 139,434 | 139,914 | **1.00x** | 1.00 | 0.84 / 0.82 ms | 19 / 19 us | 9.1 / 10.1 MB | 2 / 2 |
| REST API with JSON | 121,576 | 123,617 | **0.98x** | 0.98 | 1.36 / 1.32 ms | 23 / 23 us | 9.8 / 10.8 MB | 2 / 2 |
| gRPC unary call (protobuf) | 40,788 | 39,020 | **1.05x** | 1.05 | 4.01 / 4.12 ms | 35 / 60 us | 14.3 / 49.7 MB | 2 / 2 |
| MCP tool call, stateless | 53,562 | 53,648 | **1.00x** | 1.00 | 4.07 / 3.92 ms | 55 / 55 us | 22.2 / 30.5 MB | 2 / 2 |
| MCP tool call, session | 55,169 | 55,090 | **1.00x** | 1.00 | 4.01 / 3.87 ms | 52 / 51 us | 22.3 / 30.7 MB | 2 / 2 |

