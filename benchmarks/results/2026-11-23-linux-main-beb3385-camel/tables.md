### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,419,660 msg/s | 761,660 msg/s | 297,438 msg/s | 3.1x | 8.1x |
| In-memory queue (seda), one-way | 1,786,960 msg/s | 662,535 msg/s | 386,344 msg/s | 2.6x | 4.6x |
| In-memory queue (seda), request-reply | 775,338 msg/s | 137,808 msg/s | 91,450 msg/s | 5.6x | 8.4x |
| Content-based router | 1,135,904 msg/s | 433,457 msg/s | 158,872 msg/s | 2.6x | 7.1x |
| Split and aggregate a JSON order | 84,354 orders/s | 30,885 orders/s | 11,684 orders/s | 2.7x | 7.2x |
| CSV batch (1,000-row files) | 1,264 files/s | 560 files/s | 237 files/s | 2.2x | 5.3x |
| Kafka stream processing | 14,036 records/s | 706 records/s | 698 records/s | 19.8x | 20.1x |
| HTTP, fixed reply | 146,114 req/s | 70,737 req/s | 35,360 req/s | 2.0x | 4.1x |
| REST API with JSON | 141,276 req/s | 45,700 req/s | 22,604 req/s | 3.0x | 6.2x |
| HTTP into Kafka | 44,402 req/s | 10,182 req/s | 9,550 req/s | 4.3x | 4.6x |
| HTTP into PostgreSQL | 17,386 req/s | 8,088 req/s | 5,276 req/s | 2.1x | 3.2x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 0 µs | 2 µs | 7 µs |
| In-memory queue (seda), request-reply | 28 µs | 68 µs | 101 µs |
| Content-based router | 1 µs | 4 µs | 15 µs |
| Split and aggregate a JSON order | 21 µs | 55 µs | 180 µs |
| CSV batch (1,000-row files) | 868 µs | 2.9 ms | 9.5 ms |
| HTTP, fixed reply | 606 µs | 3.6 ms | 6.6 ms |
| REST API with JSON | 836 µs | 5.0 ms | 10.2 ms |
| HTTP into Kafka | 2.5 ms | 7.6 ms | 11.8 ms |
| HTTP into PostgreSQL | 8.7 ms | 11.0 ms | 20.1 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 8 MB | 334 MB | 112 MB | - | - | - |
| In-memory queue (seda), one-way | 10 MB | 419 MB | 205 MB | - | - | - |
| In-memory queue (seda), request-reply | 9 MB | 240 MB | 113 MB | - | - | - |
| Content-based router | 8 MB | 302 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 8 MB | 326 MB | 113 MB | - | - | - |
| CSV batch (1,000-row files) | 8 MB | 329 MB | 133 MB | - | - | - |
| Kafka stream processing | 39 MB | 211 MB | 129 MB | - | - | - |
| HTTP, fixed reply | 9 MB | 267 MB | 120 MB | 216% | 315% | 304% |
| REST API with JSON | 9 MB | 421 MB | 133 MB | 236% | 310% | 303% |
| HTTP into Kafka | 29 MB | 333 MB | 129 MB | 202% | 134% | 142% |
| HTTP into PostgreSQL | 29 MB | 289 MB | 142 MB | 132% | 232% | 271% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 3.0 ms | 1034.2 ms | 23.8 ms |
| memory at startup | 9 MB | 116 MB | 69 MB |
