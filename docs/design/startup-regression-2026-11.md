# Start-up regression, 0.6.1 to 0.8.0 candidate (2026-11-22)

Internal note. The in-process `startup` scenario of the 0.4.0 round (2026-11-06) read 2.1 to 2.8 ms; the
0.8.0 candidate round (2026-11-20) read 12.1 to 12.5 ms. This note says why and what was done.

## Result

- **It is real, and it is one kernel stall, not a slow component.** From 0.6.1 on, starting the HTTP
  server (simoom) while the process already has a second thread costs one RCU grace period in the
  kernel: 22 to 40 ms on WSL2 (12 CPUs), about 10 ms on the 4-CPU droplets (the 12 ms seen there).
- **Cause:** simoom grows the descriptor table to 1024 slots at server start
  (`simoom_sys::net::reserve_descriptors`, `Config::reserve_fds`, present in simoom 0.1.0 and 0.2.0).
  Linux (`expand_fdtable`) reallocates the table directly while the process has one thread, and calls
  `synchronize_rcu()` when the table is shared by more than one thread. The `startup` scenario adds the
  seda and timer routes (which spawn consumer threads) before the HTTP route, so the HTTP server started
  in a multi-threaded process. The 0.4.0 server did not reserve descriptors, so it had no such cost.
- **Fix:** `cameleer_http::register` (and the gRPC `register`) grow the table once, while the process is
  still single-threaded (`server_simoom::reserve_descriptors_early`, a `Once`). The later call inside simoom
  finds the table big enough and does nothing. Start-up in the scenario: 27 to 31 ms back to 2.6 to 2.8 ms.
- **Not the cause:** the machine or region (0.4.0 builds measure 1.6 to 1.8 ms on the same WSL2 machine as
  the 31 ms of 0.7.0), eager event loops, asynchronous routing, the AI registry, the number of CPUs.

## What the two numbers measured

The `measure` harness is the same in both rounds: spawn the process, time to the `READY` line (so it
includes process creation), median of 5 runs, `cameleer-bench startup`. The scenario is **not** the same:
since 0.5.0 `context()` also registers `cameleer-ai` and adds the `ai-tool` routes (10 routes, not 9), and
the HTTP route was always added last. Both stay in the figures below; the AI additions cost about 0.1 ms
(`cameleer_ai::register` 0.03 ms, its routes 0.03 ms of route building).

## Per release (WSL2 Ubuntu 24.04, 12 CPUs, `--release`, median of 40 runs, two rounds, same scenario)

| build | startup |
|---|---|
| 8d0d902 (the 0.4.0 round) | 1.6, 1.8 ms |
| v0.5.0 | 1.8, 1.8 ms |
| v0.6.0 | 1.8, 1.8 ms |
| v0.6.1 (HTTP server on simoom) | 23.3, 30.9 ms |
| v0.7.0 | 31.1, 27.3 ms |
| `main` before the fix | 27.5, 31.3 ms |
| `main` with the fix | 2.8, 2.8 ms |

The jump is between v0.6.0 and v0.6.1, the release that moved the HTTP server to simoom. The mechanism was
found by timestamps (the 25 to 40 ms were all inside `simoom::Builder::start`, and only when a seda or timer
route had started first: seda alone 0.5 ms, timer alone 0.3 ms, HTTP alone 1.2 ms, seda + HTTP 14 to 30 ms,
timer + HTTP 23 to 40 ms), not by a commit-level bisect.
`CAMEL_SERVER_LAZY_LOOPS`, `CAMEL_ASYNC_ROUTING=false` and `taskset` to 1, 4 or 12 CPUs did not change the
31 ms. A real `cameleer run` of only an HTTP route is single-threaded at that point, which is why the `sizes`
check (3.0 ms) never showed it; an application with a timer or seda route plus an HTTP route did pay it.

## The eager event loops

With the fix, the HTTP route alone starts in 1.1 to 1.7 ms against 0.2 ms for a context with no server, so
the server, its 12 loops and the listener cost about 1 ms on 12 CPUs. `CAMEL_SERVER_LAZY_LOOPS=true` reads
about 1 ms less in one noisy run; that was not measured carefully (the Windows test suite ran at the same
time) and the trade is unchanged: see `docs/design/http2-throughput.md`.

## Test

`crates/cameleer-http/tests/early_descriptors.rs` (no libtest harness, so it runs on the only thread):
after `register`, `/proc/self/status` `FDSize` must be at least 1024. It fails without the fix
("register did not grow the descriptor table: 64 -> 64") and does not depend on timing.

## What a droplet re-measurement needs (not run)

One `c-4` droplet for about 15 minutes: a release build of `cameleer-bench` (about 3 to 6 minutes of
compiling on 4 CPUs) and `cameleer-bench measure --side rust --scenario startup --repeat 5 -- <bench> startup`
for `main`, a few rounds. Under $0.10. A full rerun of the three-droplet session for Camel and Quarkus
sides is not needed for this figure (the Java 1,124 ms and native 27 ms were measured in that session).

## What the site may claim

On 0.8.0 as released **without** this fix, the in-process figure is 12 ms (about 90x Camel on the JVM), and
about 25 to 30 ms on a 12-CPU machine. With this fix, expect the 2 to 3 ms class again (about 400x on the
droplet's 1,124 ms), but it has been measured only on WSL2 here: say "about 3 ms in our benchmark scenario"
only after one droplet run confirms it. The `sizes` check (`cameleer run` of a one-route file, 2.5 to 3.0 ms
to `Routes startup`) was not affected.

Caveats: the figure includes process creation and the AI routes; it is the median of runs, not a tail;
Windows and macOS have no descriptor-table stall (the reservation is a no-op there).
