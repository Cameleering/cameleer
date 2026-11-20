### 64 connections

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 95,705 | 107,037 | **0.89x** | 0.86, 0.92 | 2.07 / 1.92 ms | 30 / 27 us | 9.3 / 10.7 MB | 4 / 4 |
| REST API with JSON | 81,748 | 80,989 | **1.01x** | 1.03, 0.99 | 2.19 / 2.29 ms | 34 / 34 us | 9.8 / 11.0 MB | 4 / 4 |
| gRPC unary call (protobuf) | 94,420 | 8,314 | **11.36x** | 11.47, 11.24 | 1.18 / 14.79 ms | 24 / 270 us | 9.2 / 13.6 MB | 4 / 4 |
| MCP tool call, stateless | 41,944 | 43,540 | **0.96x** | 0.96, 0.96 | 4.49 / 4.31 ms | 66 / 67 us | 21.7 / 30.4 MB | 4 / 4 |
| MCP tool call, session | 43,265 | 44,610 | **0.97x** | 0.97, 0.97 | 4.31 / 4.21 ms | 64 / 63 us | 22.1 / 30.4 MB | 4 / 4 |


### 256 connections

| scenario | new req/s | old req/s | new / old | per-run ratios | p99 new / old | CPU per request new / old | peak memory new / old | records new / old |
|---|---|---|---|---|---|---|---|---|
| HTTP, fixed reply | 95,586 | 98,753 | **0.97x** | 0.97 | 8.27 / 8.21 ms | 29 / 28 us | 9.5 / 10.8 MB | 2 / 2 |
| REST API with JSON | 86,812 | 84,108 | **1.03x** | 1.03 | 10.09 / 9.44 ms | 34 / 34 us | 10.2 / 11.6 MB | 2 / 2 |
| gRPC unary call (protobuf) | 106,180 | 7,968 | **13.33x** | 13.33 | 4.72 / 54.26 ms | 24 / 263 us | 10.2 / 19.3 MB | 2 / 2 |
| MCP tool call, stateless | 48,580 | 49,581 | **0.98x** | 0.98 | 12.78 / 12.71 ms | 59 / 59 us | 81.0 / 111.1 MB | 2 / 2 |
| MCP tool call, session | 51,115 | 52,492 | **0.97x** | 0.97 | 12.39 / 12.19 ms | 56 / 55 us | 57.8 / 82.6 MB | 2 / 2 |


### Per-run and per-server ratios (new / 0.7.0), incl. rest-json-h2 which compare.py does not list
```
== target/A-64-run1.txt
http          new     85052 base     98978 ratio 0.86 p99 2.47/2.34  per-phase(node:new/base): b:0.89 a:0.84
rest-json     new     85384 base     82912 ratio 1.03 p99 2.07/2.23  per-phase(node:new/base): b:1.04 a:1.02
rest-json-h2  new     84880 base     85222 ratio 1.00 p99 1.52/1.34  per-phase(node:new/base): b:0.96 a:1.04
grpc          new     94496 base      8238 ratio 11.47 p99 1.19/13.70  per-phase(node:new/base): b:11.22 a:11.73
mcp           new     41987 base     43586 ratio 0.96 p99 4.52/4.23  per-phase(node:new/base): b:0.97 a:0.95
mcp-session   new     43424 base     44693 ratio 0.97 p99 4.27/4.20  per-phase(node:new/base): b:0.98 a:0.97
== target/A-64-run2-swapped.txt
http          new    106358 base    115095 ratio 0.92 p99 1.68/1.50  per-phase(node:new/base): b:0.86 a:0.99
rest-json     new     78113 base     79067 ratio 0.99 p99 2.31/2.34  per-phase(node:new/base): b:0.96 a:1.01
rest-json-h2  new     78682 base     77322 ratio 1.02 p99 1.43/1.64  per-phase(node:new/base): b:1.31 a:0.80
grpc          new     94345 base      8390 ratio 11.24 p99 1.17/15.89  per-phase(node:new/base): b:11.08 a:11.41
mcp           new     41902 base     43493 ratio 0.96 p99 4.46/4.39  per-phase(node:new/base): b:0.94 a:0.99
mcp-session   new     43106 base     44528 ratio 0.97 p99 4.36/4.22  per-phase(node:new/base): b:0.96 a:0.98
== target/A-256.txt
http          new     95586 base     98753 ratio 0.97 p99 8.27/8.21  per-phase(node:new/base): b:1.02 a:0.91
rest-json     new     86812 base     84108 ratio 1.03 p99 10.09/9.44  per-phase(node:new/base): b:1.05 a:1.01
rest-json-h2  new     93604 base     99156 ratio 0.94 p99 5.76/4.06  per-phase(node:new/base): b:1.01 a:0.86
grpc          new    106180 base      7968 ratio 13.33 p99 4.72/54.26  per-phase(node:new/base): b:13.07 a:13.59
mcp           new     48580 base     49581 ratio 0.98 p99 12.78/12.71  per-phase(node:new/base): b:0.98 a:0.98
mcp-session   new     51115 base     52492 ratio 0.97 p99 12.39/12.19  per-phase(node:new/base): b:0.98 a:0.97
```
