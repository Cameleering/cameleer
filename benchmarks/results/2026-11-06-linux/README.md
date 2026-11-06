# Linux cloud benchmark, 2026-11-06

Raw records of the run behind [docs/BENCHMARKS.md](../../../docs/BENCHMARKS.md), made with
[scripts/cloud-bench](../../../scripts/cloud-bench/) on three DigitalOcean `c-4` droplets
(4 dedicated vCPUs, 8 GB, Ubuntu 24.04, `blr1`).

| file | sides | commit |
|---|---|---|
| `results-camel-jvm.txt` | Apache Camel 4.23.0 on Java 21 (also an earlier Cameleer build, not used) | `0536d78` |
| `results-cameleer-and-quarkus-native.txt` | Cameleer 0.4.0 and Apache Camel 4.22.1 on Quarkus 3.40.0, native | `8d0d902` |

Both runs used the same three droplets, an hour apart. Between the two commits the Apache Camel
harness only moved its HikariCP setup out of the route code and reads its settings when used
instead of in a static initializer (for the native build); the routes and the JVM side's
behaviour are the same. `machine-*.txt` have the hardware, versions and plan of each run.
Regenerate the tables with:

```sh
python3 scripts/cloud-bench/summarize.py \
  --side rust=benchmarks/results/2026-11-06-linux/results-cameleer-and-quarkus-native.txt \
  --side java=benchmarks/results/2026-11-06-linux/results-camel-jvm.txt \
  --side native=benchmarks/results/2026-11-06-linux/results-cameleer-and-quarkus-native.txt
```
