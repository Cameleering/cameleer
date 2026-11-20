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

## Droplet round (2026-11-19, run with the owner's approval; droplets deleted afterwards)

Three `c-4` droplets in blr1, `scripts/cloud-bench`, `BENCH_BASE=28cea75` (before this change) against `6252f0a`
(eager loops), 64 connections, 4 HTTP/2 connections, both servers and both phases, one run
(`benchmarks/results/2026-11-19-linux-http2-loops/`). Not the placement change of simoom-rt, which is not
released.

| scenario | new req/s | old req/s | new / old |
|---|---|---|---|
| `rest-json` (HTTP/1.1) | 98,642 | 104,920 | 0.94x (noise: the two servers differ by 10 to 25%) |
| `rest-json-h2` | 79,419 | 75,532 | 1.05x |
| gRPC unary | 61,134 | 58,635 | 1.04x |

**The droplets do not confirm the local gain.** Here the old build already served `rest-json-h2` at 75k (the
0.7.0 round measured 64k) and gRPC at 58.6k (that round: 41k after the pool fix), so HTTP/2 is at 0.75x of
HTTP/1.1, not 0.51x, and the server process used only 125 to 170% of 400% CPU on both builds. Lazy start
pins connections on the WSL2 test box because the client and server share 12 CPUs and the connections
arrive in one burst; on the droplets the connections arrive a few ms apart from another machine, and the
placement is luckier or the loops grew sooner. What the droplets show is that the server is not the limit
at 4 connections there: the load node (4 vCPUs running 64 request threads and 4 pump threads) is, which is
the client cost described above. So the root cause of "HTTP/2 at half of HTTP/1.1" is two things, in this
order on the droplets: the client's two hand-offs per response (about 53 against 21 us of load-node CPU per
request), and, where loops start lazily, the stranded connections (confirmed on WSL2, worth 2x for gRPC
there, +4 to +5% on the droplets). The eager start is kept because it removes a failure mode that depends on
timing, costs 0.8 ms of start-up on 12 CPUs, and did not slow anything; whether that is worth it without a
measured gain on the target hardware is the owner's call (`CAMEL_SERVER_LAZY_LOOPS=true` restores the old
behaviour).

## Independent client (2026-11-19, droplets in sfo3, deleted afterwards)

`scripts/cloud-bench/peer.sh` drove the same server build (`916bbca`) with nghttp2's `h2load` and with
cameleer-bench's own generator, on two servers (records in `benchmarks/results/2026-11-19-linux-http2-peer/`).
Requests per second, server `a` / server `b`:

| client | `rest-json` HTTP/1.1, 64 conns | `rest-json` HTTP/2, 4 conns x 16 streams | HTTP/2, 16 x 4 | gRPC unary, 4 x 16 |
|---|---|---|---|---|
| h2load | 86,429 / 70,696 | **153,010 / 115,820** | 139,469 / 125,521 | **93,157 / 73,426** |
| cameleer-bench | 103,390 / 75,271 | 79,182 / 75,013 | - | 58,046 / 49,911 |

**The server has no HTTP/2 gap.** Under h2load it serves HTTP/2 at 1.6 to 1.8x its HTTP/1.1 rate (one
request per connection turn costs a read and a write system call per request; 16 streams per connection
batch them), with 135 to 161% of 400% CPU, and gRPC unary at 93k/73k calls/s. h2load reports a mean of 398 us (sd 50 us) per HTTP/2 request on `a`, against 738 us for HTTP/1.1.

**The 0.51x of the 0.7.0 round was the load generator.** cameleer-bench's HTTP/2 path (`simoom-client`,
two thread hand-offs per response) reaches about half of what the server serves (79k against 153k on
`a`), and its gRPC path about 0.62x (58k against 93k). Its HTTP/1.1 path is faster than h2load's (103k
against 86k on `a`), which is why its HTTP/1.1 figure looked high next to its HTTP/2 figure.

Consequences:

1. Published HTTP/2 and gRPC throughput of the server should be measured with h2load (or another
   event-driven client), not with cameleer-bench's generator. `docs/BENCHMARKS.md` and the site list
   "HTTP/2 at 0.51x of HTTP/1.1" as a loss: that row measured the client, not the server.
2. The client cost is real for Cameleer as an HTTP/2 and gRPC client (`http` producer with HTTP/2, `grpc`
   producer, Kinesis `SubscribeToShard`): about 53 us of CPU per call against about 21 us for HTTP/1.1. That
   is what the event-driven HTTP/2 client in simoom-client (above) would fix.

## Profiling round (2026-11-19, WSL2, perf 6.8 with frame pointers)

`perf record -g` and `perf stat` on each side, the server driven by nghttp2's `h2load` (Ubuntu's
`nghttp2-client` 1.59.0) so the client cannot limit it, the client against the same server. Scripts in the
session notes: `prof.sh` (client), `sprof.sh` (server), `futex.py` (futex time by the client function that
caused it).

### Server, HTTP/2 REST: no waste found

`h2load -c 4 -m 16`: 300k req/s on 2.78 CPUs, **9.3 us per request**, IPC 1.68, 73% of the time in user
space and most of it the route (Camel's router, JSONPath, Simple). HTTP/1.1 with 64 connections costs about
36 us per request on the same server, of which the `send` of every response is 37% (on loopback the send also
runs the receiver's TCP input): one write per response is what HTTP/1.1 without pipelining costs; HTTP/2
spreads it over 16 responses per write.

### Server, gRPC unary: 34.5 -> 7.8 us per call (4.4x)

Before: 151k calls/s on 5.2 CPUs, 34.5 us per call, IPC 0.64. Two causes:

1. **A flush per reply** (simoom-rt, 36.5% of the time). Each call ran on a pool thread and its reply reached
   the loop as a message; the loop's early flush (K4: when 16 or more events of a turn are left, a
   connection's output is written as soon as its callback returns) assumed one event per connection. A
   multiplexed connection gets many messages per batch, so N replies became N `send` calls. Fix (simoom
   `153f5da`): the early flush waits for a connection's last message of the batch. 151k -> 235k calls/s.
   Test `a_batch_of_messages_for_one_connection_is_written_once` (32 replies: 18 sends before, 1 after).
2. **A thread hand-off and back per call**, and a reply sent as a stream (head, data, trailers: three
   messages and a body sink). Fix (Cameleer): `GrpcService::unary` says which methods are unary (the consumer
   reads it from the proto descriptor); a unary reply is held and sent as one response with its trailers;
   a unary call whose request arrived whole runs on the loop thread while its route is fast, under the same
   governor as REST routes (a route that stalls the loop goes back to the pool). 235k -> 335k calls/s on
   2.6 CPUs, 7.8 us per call, IPC 1.51: cheaper than a REST call. Tests
   `a_unary_reply_carries_its_message_metadata_and_status`,
   `fast_unary_calls_run_on_the_loop_and_blocking_ones_on_the_pool`.

### Client (simoom-client and cameleer-grpc's client)

Before: HTTP/2 with 4 connections, 85k req/s locally under perf, 6.25 CPUs, 1.8 context switches and 0.64
CPU migrations per request, IPC 0.40; **futex system calls 37% of the client's time**, 63% in the kernel.
By cause: waiting for the response 15.8%, the connection lock contended when opening a stream 10.0%, the
pump's wake-ups 8.5%.

1. **The head and the body in two writes** (simoom `1bfc257`): `open` wrote the HEADERS, `write` the DATA.
   A request body in memory now leaves with the head in one write, and `Client::call_with_body` does the
   same for calls (a gRPC unary call's head, message and end of stream). Kernel time per request with one
   stream per connection 22.6 -> 15.4 us (the HTTP/1.1 level).
2. **Wake-ups under the lock, one per event** (simoom `f8a1733`): the pump woke a waiting thread for every
   event of its stream (head, data, end: up to three futex wakes per response) while it held the connection
   lock, so the woken thread blocked on that lock at once. Now each waiting thread is woken once per batch,
   after the lock is released. HTTP/2 with 4 connections 103k -> 152k req/s (+48%), kernel time per request
   35 -> 24 us; the contention share fell from 10% to 2.5%. Test `a_waiting_thread_is_woken_once_per_batch`.
3. **cameleer-grpc serialized every stream opening** behind a mutex (meant for a cold channel): it is now
   taken only while the channel has no connection. With 1 and 2: gRPC unary through the crate's client 95k
   -> 121k calls/s.

What is left in the client is the hand-off a blocking API needs: the requesting thread sleeps until the
pump wakes it (about 1.2 context switches per request, against 1.1 for HTTP/1.1).

### End to end, cameleer-bench's own generator (WSL2, 3 interleaved rounds, 64 calls)

Before: Cameleer `6252f0a` on simoom 0.1.0 from crates.io. After: Cameleer `2ca6b30` plus the
`call_with_body` client path, on the local simoom checkout (`7894b4c`, `1bfc257`, `f8a1733`, `153f5da`).

| scenario | before req/s | after req/s | client CPU per request before / after |
|---|---:|---:|---|
| `rest-json-h2`, 4 connections | 95,691 / 97,936 / 99,392 | 158,994 / 157,436 / 151,781 | 53.5 / 36.1 us |
| `grpc` unary, 4 channels | 76,102 / 76,202 / 51,511 | 162,222 / 158,585 / 159,124 | 74 / 36 us |
| `rest-json` (HTTP/1.1, hand-written client) | 279,965 / 215,470 / 220,156 | 231,351 / 269,453 / 270,729 | 16 / 16 us (unchanged code) |

What still separates HTTP/2 from HTTP/1.1 in this harness is the client: 36 us against 16 us per request.
The HTTP/1.1 generator is a hand-written raw client in which each thread blocks in `read` on its own socket.
The HTTP/2 client is a general library: a pump thread per connection wakes the requesting thread, about 1.3
context switches per request. 62% of its time is in the kernel; the largest kernel item, 9.4%, is the
scheduler's run-queue spinlock in cross-CPU wake-ups. Its user time is spread out: allocation about 5%,
formatting, HPACK, URL parsing, none above 3.5%. Closing that last gap needs callers that do not block per
request (an asynchronous producer API in Cameleer on `simoom_client::rt`), not a faster blocking client.

### What is not done

- simoom is not released: the four simoom commits above reach Cameleer with simoom 0.2.0. Cameleer's
  `cameleer-grpc` client then switches to `Client::call_with_body` (a one-call change, kept as a diff in the
  session notes); until then it sends the head, message and end of stream in three writes.
- No droplet round with these changes: the droplets build Cameleer from its tarball against crates.io,
  so a round needs the simoom release or a `[patch]` with the simoom sources shipped along.

## Root cause of the client gap: a cost model (2026-11-19, WSL2)

The question: why does the blocking HTTP/2 client cost 36 us per request while the hand-written
HTTP/1.1 client costs 16? The answer: most of the gap is in *who is billed* for one unavoidable
operation, not in extra work.

### The model

For a closed-loop client with N calls in flight, the client CPU per request is C = U + K, where
K = sum_s n_s c_s + n_sleep c_switch + n_wake c_wake. The capacity on P CPUs is X <= P / C, and
Little's law (Little, 1961) gives the response time R = N / X.

A synchronous caller sleeps until its response arrives, so every request costs exactly one wake
of a sleeping thread somewhere. The protocol decides who performs that wake:

- **HTTP/1.1, a thread blocking in `read` on its own socket.** The wake runs in the TCP receive
  path of the sender. On loopback that is inside the server thread's `sendto`; with a NIC it is
  the receiving host's softirq. Either way it is not billed to the client process.
- **HTTP/2 over a shared connection.** The response arrives on a socket that many callers share.
  Whoever reads it must wake the caller it belongs to: a thread of the client, billed to the
  client.

### Unit costs (`ubench`, std-only, in the session notes; ns per round trip, two one-way hand-offs)

| operation | any CPU | one CPU | two CPUs |
|---|---:|---:|---:|
| system call (0-byte read) | 201 | | |
| cache-line ping-pong (spinning) | | | 55 |
| Mutex + Condvar ping-pong | 24,116 | 10,728 | 24,857 |
| loopback TCP request/response (each side blocking in `read`) | 52,726 | 16,318 | 49,432 |
| TCP + a reader thread waking the requester (the blocking HTTP/2 shape) | 96,257 | 24,691 | 66,918 |

A cross-CPU wake in this VM takes about 12 us of latency, against about 5 us on one CPU. The
difference is the inter-processor interrupt to a halted virtual CPU, which in a VM costs a VM exit
(an order of magnitude more than on bare metal; QNX hypervisor documentation, ELVIS, ATC 2013).
The extra reader thread adds 17 to 44 us to a round trip.

### Per-request counts of the client process (perf; 64 calls)

| | raw HTTP/1.1 | simoom-client over HTTP/1.1 | simoom-client over HTTP/2, 4 connections |
|---|---:|---:|---:|
| system calls | 2.04 | 2.05 | 3.19 |
| futex | 0 | 0 | 2.37 |
| sendto / recvfrom | 1.02 / 1.02 | 1.02 / 1.02 | 0.67 / 0.14 (about 7 responses per read) |
| wakes performed by the client (`sched_waking`) | 0.11 | 0.13 | 1.28 |
| context switches | 1.02 | 1.07 | 1.33-1.38 |
| CPU migrations | 0.01 | 0.01 | 0.21-0.29 |
| user instructions / cycles | 6.7k / 10.8k | 31.5k / 37.6k | 40.4k / 57.4k |
| kernel instructions / cycles | 25.4k / 78.1k | 26.0k / 87.1k | 31.1k / 136.2k |

The HTTP/1.1 clients perform 0.1 wakes per request, the HTTP/2 client 1.28. The HTTP/2 client
executes only 5k more kernel instructions per request but spends 49k more kernel cycles. Its kernel
time goes to cross-CPU wakes, not to work. The library control (`BENCH_H2_CLIENT_HTTP1=1` in
cameleer-bench, the same simoom-client over HTTP/1.1) separates the library's user-space work
(+27k cycles over the raw client) from what HTTP/2 adds (+20k user cycles: framing, HPACK, the
multiplexer).

### The whole machine: client plus server CPU per request

Three rounds, interleaved, each started only after Windows had stayed below 15% CPU for 3 s
(`quiet.sh` + `tot.sh`):

| | client us | server us | total us |
|---|---:|---:|---:|
| raw HTTP/1.1 | 18.1-31.6 | 26.4-84.6 | 44.5-116 (bimodal, as in every earlier run) |
| simoom-client over HTTP/1.1 | 24.2-25.8 | 41.1-44.3 | **65.4-70.1** |
| simoom-client over HTTP/2 | 39.0-41.6 | 22.0-22.6 | **61.0-64.2** |

With the same library, HTTP/2 costs the machine *less* than HTTP/1.1 per request (about 62 against
68 us). The client spends 15 us more and the server about 21 us less: the per-response wake moved
from the server's `sendto` to the client's reader thread. "The client is twice as slow as HTTP/1.1"
compared a client that is billed for the wake with one that is not.

### What can remove the wake

Shared connections with blocking callers have a floor. Let k be the number of responses that one
read of the socket delivers (about 7 here). Some thread must read the socket, and every response it
reads for another caller costs that caller a wake. Wakes billed to the client per request:

| design | wakes per request | note |
|---|---:|---|
| pump thread per connection (the blocking client today) | 1 + 1/k | the pump's own sleep and wake, plus one wake per response |
| event-driven connection + blocking facade | 1 | the loop wakes the caller; removes the pump thread and the connection mutex |
| leader/followers (Schmidt, O'Ryan, Pyarali, Kircher, Buschmann, PLoP 2000 / POSA2) | (k-1)/k | the reading thread keeps its own response; gains about 1/k, little at k = 7 |
| spin then park | 1 | spinning only pays when R is below the spin budget; here R = N/X is about 400 us |
| one connection per caller | 0 | the HTTP/1.1 shape: gives up multiplexing |
| event-driven connection + asynchronous caller | **0** | the response completes on the loop that read it (FlexSC, OSDI 2010: decouple invocation from completion) |

Context-switch costs: Li, Ding and Shen measured a direct cost of about 3.4 us and indirect cache
costs of tens of us (ExpCS 2007).

### Measured (simoom branch `h2-event-client`, commit `0199bd9`, separate session; WSL2, 64 calls, 4 connections)

| client | client CPU per request | req/s |
|---|---:|---:|
| blocking `Client` (today) | 36 us | 88k |
| event-driven HTTP/2 + blocking facade | 24 us | 212k-239k |
| event-driven HTTP/2, asynchronous callers | **3.2-4.8 us** | **513k-606k** |

This matches the model. The facade removes the pump and the mutex but keeps one wake per request;
only asynchronous callers remove it.

### What this means for Cameleer

Cameleer's routing engine is synchronous: `cameleer_core::spi::Processor::process(&self, &mut
Exchange)` has no asynchronous path. A route thread therefore waits for each reply, and an
http, gRPC or HTTP/2 producer can at best use the blocking facade (24 us). Getting to the
asynchronous number needs Camel's own answer: the `AsyncProcessor` contract, where `process(exchange,
callback)` returns at once and the routing engine continues the exchange from the callback on the
I/O loop. That is a change to the core routing engine (pipelines, error handlers, EIPs that wait),
not to the client.
