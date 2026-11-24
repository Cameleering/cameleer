### Throughput (higher is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) | Cameleer vs Camel JVM | Cameleer vs Camel native |
|---|---:|---:|---:|---:|---:|
| Direct call through a route | 2,433,219 msg/s | 783,372 msg/s | 296,942 msg/s | 3.1x | 8.1x |
| In-memory queue (seda), one-way | 1,812,116 msg/s | 689,940 msg/s | 386,504 msg/s | 2.6x | 4.6x |
| In-memory queue (seda), request-reply | 786,688 msg/s | 135,550 msg/s | 90,121 msg/s | 5.8x | 8.7x |
| Content-based router | 1,162,850 msg/s | 426,384 msg/s | 158,606 msg/s | 2.7x | 7.3x |
| Split and aggregate a JSON order | 85,016 orders/s | 29,844 orders/s | 11,985 orders/s | 2.8x | 7.0x |
| CSV batch (1,000-row files) | 1,242 files/s | 565 files/s | 240 files/s | 2.1x | 5.1x |
| Kafka stream processing | 14,161 records/s | 706 records/s | 697 records/s | 20.0x | 20.3x |
| HTTP, fixed reply | 120,542 req/s | 73,919 req/s | 36,254 req/s | 1.6x | 3.3x |
| REST API with JSON | 123,280 req/s | 45,136 req/s | 22,799 req/s | 2.7x | 5.4x |
| HTTP into Kafka | 44,043 req/s | 10,012 req/s | 9,492 req/s | 4.3x | 4.6x |
| HTTP into PostgreSQL | 17,238 req/s | 8,150 req/s | 5,241 req/s | 2.1x | 3.2x |

### Latency, p99 (lower is better)

| scenario | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| Direct call through a route | 0 µs | 2 µs | 7 µs |
| In-memory queue (seda), request-reply | 27 µs | 69 µs | 102 µs |
| Content-based router | 1 µs | 4 µs | 15 µs |
| Split and aggregate a JSON order | 22 µs | 60 µs | 168 µs |
| CSV batch (1,000-row files) | 889 µs | 2.8 ms | 9.4 ms |
| HTTP, fixed reply | 1.5 ms | 3.2 ms | 6.4 ms |
| REST API with JSON | 1.4 ms | 5.2 ms | 9.8 ms |
| HTTP into Kafka | 2.4 ms | 7.8 ms | 11.9 ms |
| HTTP into PostgreSQL | 8.7 ms | 11.1 ms | 20.1 ms |

### Memory and CPU

| scenario | Cameleer: peak memory | Apache Camel (JVM): peak memory | Apache Camel (Quarkus native): peak memory | Cameleer: CPU | Apache Camel (JVM): CPU | Apache Camel (Quarkus native): CPU |
|---|---:|---:|---:|---:|---:|---:|
| Direct call through a route | 8 MB | 323 MB | 112 MB | - | - | - |
| In-memory queue (seda), one-way | 10 MB | 428 MB | 220 MB | - | - | - |
| In-memory queue (seda), request-reply | 9 MB | 239 MB | 113 MB | - | - | - |
| Content-based router | 8 MB | 271 MB | 113 MB | - | - | - |
| Split and aggregate a JSON order | 8 MB | 337 MB | 113 MB | - | - | - |
| CSV batch (1,000-row files) | 9 MB | 324 MB | 131 MB | - | - | - |
| Kafka stream processing | 37 MB | 213 MB | 130 MB | - | - | - |
| HTTP, fixed reply | 9 MB | 286 MB | 121 MB | 218% | 315% | 305% |
| REST API with JSON | 10 MB | 398 MB | 132 MB | 222% | 312% | 303% |
| HTTP into Kafka | 31 MB | 313 MB | 129 MB | 202% | 130% | 141% |
| HTTP into PostgreSQL | 29 MB | 295 MB | 142 MB | 132% | 229% | 272% |

### Startup

| | Cameleer | Apache Camel (JVM) | Apache Camel (Quarkus native) |
|---|---:|---:|---:|
| time to a started context | 3.0 ms | 1013.6 ms | 23.6 ms |
| memory at startup | 9 MB | 116 MB | 69 MB |
