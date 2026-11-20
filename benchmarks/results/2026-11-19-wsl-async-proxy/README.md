# Asynchronous routing, phase 0 (2026-11-19, WSL2)

These are the raw results for `docs/design/async-routing.md`, section 12.

**Setup.**
- Machine: WSL2 Ubuntu 24.04 on Windows 11, 12 logical CPUs.
- Each run started only when Windows was below 15% CPU (the `idle=` column).
- Three rounds, with the variants interleaved.

**`proxy.sh <async|sync> <fast|sleepN> <conns> <secs>`** chains three processes:
1. the load generator, `cameleer-bench loadgen-remote`, on CPUs 6-11;
2. Cameleer, `cameleer-bench serve http-proxy`, on CPUs 0-3: a `jetty:` consumer → `http` producer
   route with `bridgeEndpoint=true`;
3. the backend, `cameleer-bench raw-http`, on CPUs 4-5: `poll` for `fast`, or one thread per
   connection with a 20 ms delay for `sleep20`.

`sync` sets `CAMEL_ASYNC_ROUTING=false`.

**What is counted.** CPU and context switches are the Cameleer process's, read from `/proc` over the
middle of the measured window and divided by requests. The backend and the load generator are not
counted.

**Files.** `rounds.out` holds the raw lines.
