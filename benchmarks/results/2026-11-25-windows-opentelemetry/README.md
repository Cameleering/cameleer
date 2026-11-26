# OpenTelemetry tracing overhead (2026-11-25, Windows, one machine)

What tracing costs the in-process scenarios of `cameleer-bench`, with tracing off and on.

- **Machine:** AMD Ryzen 5 5600X (6 cores, 12 threads), 16 GB, Windows 11; an ordinary desktop with other
  programs open, so the figures are noisy (round 3 ran at about half speed for every side). No droplets.
- **Builds:** `cargo build --release -p cameleer-bench`. `main` is `b7c0e73` (0.8.0 plus site fixes), built in its own
  worktree; the branch is `opentelemetry` at the commit of this directory.
- **Sides**, set by `CAMEL_BENCH_OTEL` (read by `cameleer-bench run`):
  - `main`: the `main` build, no tracing code;
  - `off`: this branch, tracing not installed (the default);
  - `spans`: this branch, tracer installed with every span sampled and a span processor that drops it
    (`OTEL_TRACES_EXPORTER=none`): the cost of making the spans and propagating the context;
  - `otlp`: this branch, the whole pipeline: the batch span processor with the default settings (queue 2048, batch
    512, 5 s delay) exporting over OTLP/HTTP protobuf to an in-process receiver that answers 200 and discards.
- **Method:** `run.sh`: for each of 3 rounds, each scenario, each side in turn: `cameleer-bench run <scenario> 2 3 3`
  (2 s warm-up, three 3 s iterations); the table is the median of the 3 rounds (`summarize.py results.txt`).
  `results-first-build.txt` is the same run on the first build, before the per-span cost was cut (see below).

| scenario | main | off | spans | otlp | off/main | spans/off | otlp/off | spans per msg | ns per span (spans) | spans exported (otlp) |
|---|---|---|---|---|---|---|---|---|---|---|
| direct | 4,011,814 | 3,707,191 | 923,424 | 622,524 | 0.92x | 0.25x | 0.17x | 2.5 | 330 | 49% |
| seda | 5,095,791 | 5,472,525 | 2,324,393 | 2,161,027 | 1.07x | 0.42x | 0.39x | 2.5 | 100 | 11% |
| cbr | 1,548,251 | 1,665,587 | 719,423 | 617,865 | 1.08x | 0.43x | 0.37x | 2.4 | 326 | 67% |
| splitagg | 100,357 | 93,828 | 72,106 | 57,431 | 0.93x | 0.77x | 0.61x | 2.5 | 1290 | 99% |

Messages per second. "ns per span" is `(1/spans - 1/off) / spans per message`, from throughput, so for `seda` (4
producer threads, 4 consumers) it is wall time across threads, not CPU per span; splitagg's figure is within its noise
(its `off` side ranged 57k to 100k).

**What it says**

- **Off:** 0.92x to 1.08x of `main`, in both directions: noise. With tracing off no tracing code runs (no route hook,
  no producer wrapper, no per-message check); the only changes on that path are a read of the producer-wrapper list
  when a producer is created and one relaxed atomic load in `log::emit`.
- **On:** a span costs about 0.1 to 0.35 µs on this machine (2.5 spans per message here: the template's send, the
  route, the `to`). In absolute terms: a `direct` route goes from 0.27 µs to 1.1 µs per message; a route that does any
  I/O will not notice.
- **Export:** the batch exporter sent about 0.8 million spans per second over one keep-alive connection
  (7.5 million in a 9 s `direct` run); above that the bounded queue drops spans and counts them, by design, and the
  routes keep their speed (`otlp` is 0.6x to 0.9x of `spans`). Real deployments sample (`OTEL_TRACES_SAMPLER`) or have
  far fewer spans per second.

**The first build** (`results-first-build.txt`) cost 1.1 to 2.2 µs per span: every span cloned and masked the endpoint
URI, formatted its attributes, wrote and parsed the active span as a `traceparent` string, cloned the previous property
values to restore them, and read `Instant::now` (a QueryPerformanceCounter call on Windows) twice. The tracer now
computes an endpoint's fixed attributes once and shares them (`SpanData::shared_attributes`), computes names and the
per-exchange attributes only for sampled spans, keeps the active span as 25 bytes, restores the parent from the span
itself, times spans with the core's tick clock and counts with striped counters. A profile
(`cameleer-bench profile direct 5`, `CAMEL_BENCH_OTEL=spans`) of the final build has the exchange-id generation for
the `exchangeId` attribute (about 9%), the wall-clock start time and the `traceparent` header formatting as the
largest remaining parts.

Not measured: Linux, HTTP or Kafka scenarios with tracing, memory, a real Collector over the network.
