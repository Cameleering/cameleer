# HTTP/2 with an independent client, 2026-11-19

Records behind the section "Independent client" of
[docs/design/http2-throughput.md](../../../docs/design/http2-throughput.md).

Three DigitalOcean `c-4` droplets in `sfo3` (c-4 was not available in blr1 that day): servers `a` and `b`,
one load node; Ubuntu 24.04. Cameleer at `916bbca` (eager event loops, simoom 0.1.0 from crates.io), the
`rest-json` and `grpc` scenarios of `cameleer-bench serve`. `scripts/cloud-bench/peer.sh` drives each server
with nghttp2's `h2load` (Ubuntu's `nghttp2-client` package, 4 threads, 10 s warm-up, 25 s timed) and then with
`cameleer-bench loadgen-remote` (64 calls, 10 s warm-up, 5 x 5 s). One run per case; the droplets were deleted
afterwards. `PROC` lines: the server process over the whole run, including warm-up and idle seconds.

`peer.txt`: `H2LOAD` lines (h2load's summary), `OWN` lines (cameleer-bench), `PROC` lines.
