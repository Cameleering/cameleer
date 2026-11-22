### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,183,948 msg/s | 702,037 msg/s | 265,536 msg/s | 3.1x | 8.2x |
| In-memory queue (seda), one-way | 1,530,109 msg/s | 577,238 msg/s | 332,326 msg/s | 2.6x | 4.6x |
| In-memory queue (seda), request-reply | 722,258 msg/s | 105,184 msg/s | 63,132 msg/s | 6.8x | 11.4x |
| Content-based router | 1,154,104 msg/s | 384,354 msg/s | 146,327 msg/s | 3.0x | 7.8x |
| Split and aggregate a JSON order | 74,755 orders/s | 29,349 orders/s | 10,350 orders/s | 2.5x | 7.2x |
| CSV batch (1,000-row files) | 1,102 files/s | 466 files/s | 188 files/s | 2.3x | 5.8x |
| Kafka stream processing | 10,104 records/s | 662 records/s | 654 records/s | 15.2x | 15.4x |
| HTTP, fixed reply | 102,131 req/s | 55,735 req/s | 27,828 req/s | 1.8x | 3.6x |
| REST API with JSON | 88,440 req/s | 35,986 req/s | 17,938 req/s | 2.4x | 4.9x |
| HTTP into Kafka | 31,351 req/s | 9,641 req/s | 9,006 req/s | 3.2x | 3.4x |
| HTTP into PostgreSQL | 17,279 req/s | 7,394 req/s | 4,584 req/s | 2.3x | 3.7x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 1 µs | 3 µs | 10 µs |
| In-memory queue (seda), request-reply | 33 µs | 88 µs | 148 µs |
| Content-based router | 2 µs | 5 µs | 18 µs |
| Split and aggregate a JSON order | 30 µs | 62 µs | 397 µs |
| CSV batch (1,000-row files) | 1.9 ms | 3.4 ms | 11.7 ms |
| HTTP, fixed reply | 1.7 ms | 3.7 ms | 7.2 ms |
| REST API with JSON | 2.1 ms | 5.4 ms | 11.9 ms |
| HTTP into Kafka | 3.5 ms | 8.1 ms | 13.0 ms |
| HTTP into PostgreSQL | 9.4 ms | 11.3 ms | 22.8 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 8 MB | 331 MB | 113 MB | - | - | - |
| In-memory queue (seda), one-way | 10 MB | 435 MB | 198 MB | - | - | - |
| In-memory queue (seda), request-reply | 9 MB | 195 MB | 113 MB | - | - | - |
| Content-based router | 8 MB | 328 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 8 MB | 323 MB | 113 MB | - | - | - |
| CSV batch (1,000-row files) | 8 MB | 318 MB | 131 MB | - | - | - |
| Kafka stream processing | 39 MB | 210 MB | 130 MB | - | - | - |
| HTTP, fixed reply | 9 MB | 262 MB | 120 MB | 220% | 315% | 306% |
| REST API with JSON | 10 MB | 411 MB | 124 MB | 224% | 312% | 303% |
| HTTP into Kafka | 57 MB | 317 MB | 128 MB | 202% | 144% | 166% |
| HTTP into PostgreSQL | 29 MB | 313 MB | 146 MB | 158% | 245% | 280% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 12.3 ms | 1126.9 ms | 27.4 ms |
| memory at startup | 9 MB | 116 MB | 69 MB |
