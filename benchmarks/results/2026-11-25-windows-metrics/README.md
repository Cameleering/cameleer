# Metrics overhead (2026-11-25, Windows, one machine)

What Camel's built-in meters (`camel.metrics.enabled`) cost the in-process scenarios of `cameleer-bench`.

- **Machine:** AMD Ryzen 5 5600X (6 cores, 12 threads), 16 GB, Windows 11; a desktop, so noisy. No droplets.
- **Builds:** `cargo build --release -p cameleer-bench`; `main` is `b7c0e73`, built in its own worktree; the branch is
  `metrics` at the commit of this directory.
- **Sides**, set by `CAMEL_BENCH_METRICS`:
  - `main`: the `main` build;
  - `off`: this branch, no meters installed (the default);
  - `micrometer`: `camel.metrics.*` defaults: the route policy at route and context level, the exchange event notifier
    (completed, failed and sent timers, inflight and last-exchange gauges) and the route gauges;
  - `opentelemetry`: the same with `camel-opentelemetry-metrics`' names and histograms in milliseconds.
- **Method:** `run.sh`: 3 rounds, each scenario, each side in turn, `cameleer-bench run <scenario> 2 3 3`; medians.

| scenario | main | off | micrometer | opentelemetry | off/main | micrometer/off | opentelemetry/off | ns per message (micrometer) |
|---|---|---|---|---|---|---|---|---|
| direct | 3,215,692 | 3,080,600 | 1,839,761 | 1,711,024 | 0.96x | 0.60x | 0.56x | 219 |
| seda | 4,708,740 | 4,798,643 | 3,204,494 | 3,379,313 | 1.02x | 0.67x | 0.70x | 104 |
| cbr | 1,353,177 | 1,410,056 | 1,035,216 | 988,435 | 1.04x | 0.73x | 0.70x | 257 |
| splitagg | 85,784 | 88,240 | 84,113 | 83,026 | 1.03x | 0.95x | 0.94x | 556 |

Messages per second; "ns per message" is `1/micrometer - 1/off` from throughput (wall time across threads for `seda`;
splitagg's figure is within its noise).

- **Off:** 0.96x to 1.04x of `main`: noise. Nothing is installed; the registry does not exist until a component or an
  option creates it.
- **On:** about 0.1 to 0.26 µs per message for the default meters in the scenarios with one route and one or two sends
  per message: per exchange one route timer and three counters at route level, the same at context level, a
  completed/failed timer, a sent timer per send and the last-exchange gauge. A route that does any I/O will not notice.

Not measured: the components alone (`micrometer:counter:...` per message), message history, OTLP export, Linux.
