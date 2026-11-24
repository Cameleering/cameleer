### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,337,237 msg/s | 710,684 msg/s | 282,090 msg/s | 3.2x | 8.2x |
| In-memory queue (seda), one-way | 1,604,092 msg/s | 587,542 msg/s | 343,238 msg/s | 2.7x | 4.6x |
| In-memory queue (seda), request-reply | 762,372 msg/s | 107,037 msg/s | 63,593 msg/s | 7.1x | 11.9x |
| Content-based router | 1,230,030 msg/s | 397,464 msg/s | 148,884 msg/s | 3.0x | 8.2x |
| Split and aggregate a JSON order | 76,179 orders/s | 29,260 orders/s | 10,154 orders/s | 2.6x | 7.5x |
| CSV batch (1,000-row files) | 1,152 files/s | 461 files/s | 201 files/s | 2.5x | 5.7x |
| Kafka stream processing | 9,269 records/s | 660 records/s | 654 records/s | 14.0x | 14.1x |
| HTTP, fixed reply | 89,306 req/s | 56,314 req/s | 28,352 req/s | 1.5x | 3.1x |
| REST API with JSON | 74,728 req/s | 39,632 req/s | 18,395 req/s | 1.8x | 4.0x |
| HTTP into Kafka | 38,984 req/s | 9,428 req/s | 8,789 req/s | 4.1x | 4.4x |
| HTTP into PostgreSQL | 14,673 req/s | 6,482 req/s | 4,350 req/s | 2.2x | 3.3x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 1 µs | 2 µs | 9 µs |
| In-memory queue (seda), request-reply | 30 µs | 84 µs | 142 µs |
| Content-based router | 1 µs | 5 µs | 17 µs |
| Split and aggregate a JSON order | 21 µs | 63 µs | 332 µs |
| CSV batch (1,000-row files) | 1.1 ms | 3.2 ms | 10.6 ms |
| HTTP, fixed reply | 2.6 ms | 3.7 ms | 7.0 ms |
| REST API with JSON | 2.5 ms | 4.8 ms | 11.3 ms |
| HTTP into Kafka | 2.8 ms | 8.2 ms | 12.2 ms |
| HTTP into PostgreSQL | 9.7 ms | 12.7 ms | 23.1 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 8 MB | 351 MB | 112 MB | - | - | - |
| In-memory queue (seda), one-way | 11 MB | 424 MB | 197 MB | - | - | - |
| In-memory queue (seda), request-reply | 10 MB | 222 MB | 113 MB | - | - | - |
| Content-based router | 8 MB | 325 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 9 MB | 337 MB | 113 MB | - | - | - |
| CSV batch (1,000-row files) | 9 MB | 351 MB | 131 MB | - | - | - |
| Kafka stream processing | 40 MB | 223 MB | 129 MB | - | - | - |
| HTTP, fixed reply | 18 MB | 284 MB | 120 MB | 318% | 315% | 306% |
| REST API with JSON | 20 MB | 411 MB | 125 MB | 318% | 312% | 303% |
| HTTP into Kafka | 77 MB | 352 MB | 128 MB | 278% | 144% | 160% |
| HTTP into PostgreSQL | 25 MB | 300 MB | 141 MB | 118% | 220% | 259% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 2.9 ms | 1092.1 ms | 27.4 ms |
| memory at startup | 9 MB | 115 MB | 68 MB |
