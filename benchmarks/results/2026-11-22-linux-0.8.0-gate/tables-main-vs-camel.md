### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,288,806 msg/s | 710,684 msg/s | 282,090 msg/s | 3.2x | 8.1x |
| In-memory queue (seda), one-way | 1,603,158 msg/s | 587,542 msg/s | 343,238 msg/s | 2.7x | 4.6x |
| In-memory queue (seda), request-reply | 757,173 msg/s | 107,037 msg/s | 63,593 msg/s | 7.0x | 11.9x |
| Content-based router | 1,156,376 msg/s | 397,464 msg/s | 148,884 msg/s | 2.9x | 7.7x |
| Split and aggregate a JSON order | 78,624 orders/s | 29,260 orders/s | 10,154 orders/s | 2.6x | 7.7x |
| CSV batch (1,000-row files) | 1,161 files/s | 461 files/s | 201 files/s | 2.5x | 5.7x |
| Kafka stream processing | 9,230 records/s | 660 records/s | 654 records/s | 13.9x | 14.1x |
| HTTP, fixed reply | 111,919 req/s | 56,314 req/s | 28,352 req/s | 1.9x | 3.9x |
| REST API with JSON | 83,332 req/s | 39,632 req/s | 18,395 req/s | 2.1x | 4.5x |
| HTTP into Kafka | 31,232 req/s | 9,428 req/s | 8,789 req/s | 3.3x | 3.5x |
| HTTP into PostgreSQL | 9,768 req/s | 6,482 req/s | 4,350 req/s | 1.5x | 2.2x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 0 µs | 2 µs | 9 µs |
| In-memory queue (seda), request-reply | 31 µs | 84 µs | 142 µs |
| Content-based router | 1 µs | 5 µs | 17 µs |
| Split and aggregate a JSON order | 24 µs | 63 µs | 332 µs |
| CSV batch (1,000-row files) | 1.1 ms | 3.2 ms | 10.6 ms |
| HTTP, fixed reply | 1.5 ms | 3.7 ms | 7.0 ms |
| REST API with JSON | 2.2 ms | 4.8 ms | 11.3 ms |
| HTTP into Kafka | 3.4 ms | 8.2 ms | 12.2 ms |
| HTTP into PostgreSQL | 17.8 ms | 12.7 ms | 23.1 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 8 MB | 351 MB | 112 MB | - | - | - |
| In-memory queue (seda), one-way | 10 MB | 424 MB | 197 MB | - | - | - |
| In-memory queue (seda), request-reply | 9 MB | 222 MB | 113 MB | - | - | - |
| Content-based router | 8 MB | 325 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 8 MB | 337 MB | 113 MB | - | - | - |
| CSV batch (1,000-row files) | 9 MB | 351 MB | 131 MB | - | - | - |
| Kafka stream processing | 39 MB | 223 MB | 129 MB | - | - | - |
| HTTP, fixed reply | 9 MB | 284 MB | 120 MB | 222% | 315% | 306% |
| REST API with JSON | 10 MB | 411 MB | 125 MB | 219% | 312% | 303% |
| HTTP into Kafka | 58 MB | 352 MB | 128 MB | 206% | 144% | 160% |
| HTTP into PostgreSQL | 29 MB | 300 MB | 141 MB | 88% | 220% | 259% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 3.4 ms | 1092.1 ms | 27.4 ms |
| memory at startup | 9 MB | 115 MB | 68 MB |
