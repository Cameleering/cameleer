# Why HTTP/2 served half the requests of HTTP/1.1 (and gRPC a third of what it can)

Internal design note, 2026-11-19. Question from the 0.7.0 round
(`benchmarks/results/2026-11-18-linux-grpc-http2/`): `rest-json-h2` served 63,900 req/s against 125,700
for `rest-json` at 64 concurrent calls, with the same server CPU per request and the server far from
saturated.

## Result in one paragraph

Part of the gap was in the **server**: simoom starts its event loops lazily (`lazy_loops`, set by the
`simoom` server for FaaS start-up) and grows them when a third connection is open at once, but a
connection never moves between loops. HTTP/1.1 load opens many connections, so the later ones spread. An
HTTP/2 or gRPC client keeps **four** connections with many streams each; three of them stay on loop 0, so
one thread serves 75% of the traffic and saturates at about 60,000 small requests per second while the
other loops idle. Starting every loop with the server (`cameleer_http::server_simoom::event_loops`) fixes
that: gRPC unary 2.1x, `rest-json-h2` +14% (noisy, see below). The rest of the gap is in the
**client** (`simoom-client`'s one pump thread per connection); that part lives in the simoom repository
and is not fixed here.

## Model

Closed loop with `N` calls in flight (Little's law): `X = N / R`, `R` the mean latency. For the droplet run
`N = 64`, `R = 0.86 ms` gives 74k/s (measured 63.9k with `R` of 0.86 to 1.20 ms), so the request rate fell
because latency doubled, not because requests got more expensive: they queue somewhere.

Where they queue: if the event loop that serves a share `f` of the traffic needs `s` of CPU per request,
that loop caps the total at `X <= 1 / (f * s)`. The server cost per request in the h2 runs is `s` of about
16 to 20 us (21 and 19 us on the droplets). With 4 connections and loop 0 holding 3 of them, `f = 3/4`:
`X <= 1 / (0.75 * 18 us)` = 74k/s, and 4 of 4 (`f = 1`) gives 56k/s. The measured 62k to 69k (WSL2) and
63.9k (droplets) sit in that range. The other loops idle, which is why the total CPU was low (126% and 79%
of 400%) and the CPU per request unchanged. HTTP/1.1 with 64 connections (most opened after the loops grew,
and re-opened by the load generator) has `f` of about 1/12 per loop and never meets the bound.

## Measurements (WSL2 Ubuntu 24.04, 12 logical CPUs, server and load generator on one machine)

Build: `cargo build --release -p cameleer-bench`; run: `cameleer-bench serve <scenario> <port> 30`, then
`cameleer-bench loadgen-remote 127.0.0.1 <port> <scenario> 64 2 3 3`, with `BENCH_H2_CONNS` for the number
of HTTP/2 connections (default 4). Per-thread CPU from `/proc/<pid>/task/*/stat` over 2 s.

1. **Reproduced on Linux, not on Windows.** `rest-json` 170,857 req/s against `rest-json-h2` 67,641 on
   WSL2; on Windows 11 `rest-json-h2` is 162,637 against 114,417 for HTTP/1.1. The effect is the Linux
   `SO_REUSEPORT`/lazy-loop placement, so the loop placement, not the protocol, differs.
2. **The throughput depends on the number of HTTP/2 connections, not on the load.** `BENCH_H2_CONNS`
   1, 4, 16, 64: 33.5k, 76k, 133k, 112k req/s; the server used 3.4 s of CPU in 13 s with one connection.
3. **Server threads, 4 connections:** `simoom-0` at 103 to 104% of a core, every other loop thread and every
   offload worker at 0 (thread list in the run). With 16 connections: 20 to 40% on every loop.
4. **Eager loops** (the experiment: `lazy_loops = false`): four loops at 51 to 54% each, 81k to 90k req/s
   instead of 63k to 69k.
5. **The client is the next limit.** With the loops spread, the load generator used 4.5 cores
   (58 s of CPU in 13 s, 2/3 in the kernel) for 85k req/s, about 53 us per request against about 21 us for
   the HTTP/1.1 generator. With one connection the server used 26% of a core for 33.5k req/s and the load
   generator 16 s of CPU in 13 s, so the per-connection limit sits in `simoom-client`: one pump thread per
   connection reads, takes the connection lock, wakes the waiting request threads one condition variable at
   a time (`crates/simoom-client/src/mux.rs`, `read_once` and `pump`), and request threads take the same lock to
   send. That is a serial section of about 30 us per response per connection.

Not the cause (tested or excluded by the numbers): connection count of the load generator (see 2), Nagle
(the client sets `TCP_NODELAY`, latency is 1 ms not 40 ms), HPACK or frame cost (server CPU per request the
same as HTTP/1.1), WINDOW_UPDATE round trips (16 streams of a small body per connection, far below the
window).

## The fix

`cameleer_http::server_simoom::event_loops` applies `lazy_loops = false` to the HTTP server and the gRPC
server. Lazy start stays for FaaS: `AWS_LAMBDA_RUNTIME_API` set, or `CAMEL_SERVER_LAZY_LOOPS=true`;
`CAMEL_SERVER_LAZY_LOOPS=false` forces eager. Cost (same machine, 30 starts each, two rounds):
HTTP route ready 1.13 to 1.22 ms lazy against 1.92 to 1.96 ms eager, peak RSS 8.1 MB against 8.6 to 8.7 MB
(12 loops); gRPC server peak RSS at 64 calls 16.3 MB against 16.4 to 16.5 MB. The published FaaS cold start of
the container image stays with the lazy start only where Lambda is detected; a container or a server
without Lambda now starts about 0.8 ms later on 12 CPUs (less on fewer).

Tests: `lazy_loops_only_for_faas_unless_set`, and `long_lived_connections_spread_over_the_loops` (three
connections on a 4-loop server reach at least two loops in six attempts; it fails 40 of 40 times with
`CAMEL_SERVER_LAZY_LOOPS=true` and passes 40 of 40 without).

## Before and after (WSL2, 3 rounds alternating, 64 calls, 4 HTTP/2 connections; req/s)

| scenario | before (lazy) | after (eager) |
|---|---|---|
| `rest-json` (HTTP/1.1) | 132k, 144k, 61k | 154k, 175k, 128k |
| `rest-json-h2` | 65k, 80k, 65k | 85k, 81k, 74k |
| `grpc` unary | 21.6k, 33.1k, 28.1k | 62.3k, 50.8k, 58.4k |

Noise is large (a loaded WSL2 host, load generator on the same CPUs; HTTP/1.1 round 3 shows a disturbed
pair), so read the means: `rest-json-h2` 70k to 80k (+14%), `grpc` 27.6k to 57.2k (2.07x). A second gRPC
pair (`mem.sh`): 30.1k, 31.8k lazy against 58.0k, 35.4k eager. Memory is within 0.2 MB at 64 calls.
Windows was not remeasured; the hermetic tests of `cameleer-http` and `cameleer-grpc` pass there.

## Placement fix in simoom-rt (simoom repository, commit 7894b4c, not released)

With every loop started, `SO_REUSEPORT` still picks the loop by a hash of the four-tuple, so 4 long-lived
connections landed 3 on one loop about 18% of the time. `simoom-rt` now hands a freshly accepted connection
to the least loaded loop when this loop holds more than the least loaded one and the server has at most
4 connections per loop (`balance_target`, `loop_pending` counts hand-offs not yet adopted, so a burst does not
all pick the same loop). With many connections the hash evens out and a hand-off per accept would only cost a
wake-up, so the rule stays off. Test `few_connections_spread_one_per_loop` (4 loops, 4 connections, every loop
gets exactly one, 10 rounds): fails without the change (`[0, 2, 1, 1]` in round 0), passes with it; the
`simoom-rt` and `simoom` suites pass on WSL2. Cameleer picks this up with the next simoom release; a local
before/after throughput measurement was not conclusive because the machine was loaded by the droplet setup
(HTTP/2 between 25k and 83k req/s in both builds), so the evidence is the placement test.

## The client, measured but not fixed

Context switches of the load generator over 2 s (`/proc/<pid>/task/*/status`, `ctx.sh`): HTTP/1.1 417k for
341k requests, 1.2 per request; HTTP/2 381k for 170k requests, 2.2 per request. One thread per request over a
shared connection needs two hand-offs per response: the pump thread wakes from `read`, then wakes the request
thread through its condition variable. In a VM a cross-core wake costs far more than the 5 us of bare metal,
which is consistent with 53 us against 21 us per request. Spinning before parking does not help (responses
arrive a few hundred us later under load), and a leader/follower read only moves the wake. Removing the hop
needs an event-driven HTTP/2 client: `simoom-client`'s `rt::RtClient` already runs HTTP/1.1 on simoom-rt loops
with completion callbacks, and the sans-io `simoom-h2` engine is what the blocking client drives, so the work is
an HTTP/2 connection type for `RtClient` plus a blocking facade over it for `Client`. It is a feature of its
own (stream limits, flow control, GOAWAY and retries on the loop), not a patch, and is not started.

## What remains, ranked

1. **An event-driven HTTP/2 client** in simoom-client, as above. This is what keeps HTTP/2 below HTTP/1.1 in
   this harness once the server is fixed (85k against 150k to 175k). The droplets have a separate load node, so
   the cost there is on its 4 CPUs.
2. **Release simoom with the placement change** and bump Cameleer's dependency.
3. **The load generator** can use `BENCH_H2_CONNS=16` as an additional record to separate client from server.

## Droplet round to confirm (needs the owner's approval, not run)

Same three `c-4` droplets and `scripts/cloud-bench`; new build against the 0.7.0 tag, scenarios `rest-json`,
`rest-json-h2` (4 and 16 connections), `grpc`, 64 and 256 calls, two phases, with
`CAMEL_SERVER_LAZY_LOOPS=true` on the old side to separate this change from anything else. Expect gRPC and
`rest-json-h2` up as above and `rest-json-h2` still below `rest-json` because of the client.
