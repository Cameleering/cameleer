# Documentation and claims review, 2026-11-20 (before 0.8.0)

Read-only audit of what README, `docs/**`, `CHANGELOG.md`, `docs/STATUS.md`, `docs/BENCHMARKS.md`,
`docs/FEATURES.md`, `docs/guides/**`, `docs/components/**`, `site/**`, `llms.txt`, `scripts/llms/*`,
`site/cameleer/facts.json` and `docker/README.dockerhub.md` say, against `origin/main` at `ec33695`
(2026-11-19) and, where marked **[PR 67]** or **[PR 68]**, once those pull requests land (diffs read with
`gh pr diff`). Template: `docs-review-2026-11-09.md`. Internal: `docs/design/` is not published.

Nothing was fixed. The site, `llms.txt` and `scripts/llms/*` belong to the SEO session
(`local_b352cf6d-2830-4298-9464-d72fe0a8cbea`); section 5 is the exact list for it (a copy is in the shared
notes folder).

What was checked by running something (not just reading):

- `python scripts/build-docs.py` and `python scripts/check-site.py` on a clean `git archive` of `origin/main`:
  279 docs pages built, **384 HTML pages, 353 sitemap URLs, 0 warnings, no errors**. The committed `llms.txt`
  is byte-identical to the built one.
- `cargo test -p cameleer-main --test docs_reference --test features` on `main`: 14 passed and 6 passed.
- Not run: the full workspace suite, the live tiers, and any of this on the PR branches. No droplets were
  created (spending needs the owner's approval; the plan and cost are in 1.9).

## Summary

1. **The HTTP/2 "loss" is still published as an unexplained fact in 10 places** on `main` (README, BENCHMARKS,
   STATUS, three llms files, the site home and the ledger). PR 67 fixes README, BENCHMARKS, STATUS, grpc.md and
   CHANGELOG, and leaves the site and the llms files (by design), so after PR 67 merges the repository
   contradicts the site. PR 67's own wording is also too strong ("the load generator, not the server"): its
   design note says two causes (1.1).
2. **The gRPC figures that the site, README, grpc.md and llms.txt lead with (41,020 calls/s, "3.9x") were
   limited by the load generator's client.** The same server shape did 73,000 to 93,000 calls/s under `h2load`.
   The ratio is real within its harness; the absolute figure is not what the server can do (1.2).
3. **The headline comparison with Apache Camel is Cameleer 0.4.0 (2026-11-06)**: before the simoom server,
   before the Kafka producer fix and before the MCP work. The same BENCHMARKS page holds two figures for HTTP
   into Kafka (17,854 and 48,800 req/s) and the site still advertises, as "one loss", a p99 that the later run
   showed is no longer a loss. It needs one re-measurement on 0.8.0 (1.3, 1.9).
4. Several size and cold-start figures are older than the code: `core` is published as 5.8 to 5.9 MB, the
   0.7.0 round measured 7.05 MB (1.7).
5. Dependency and version statements: simoom 0.1.0 in two llms files; the "Rust crate" snippets say `0.6` (which
   cargo reads as `>=0.6, <0.7`, so a new user gets an old release) in README, two guides, `facts.json`; the
   site's own snippet says `0.5` (2).
6. Stale "next step" statements about the event-driven HTTP server core (ROADMAP, BENCHMARKS, STATUS): it
   shipped in 0.6.1 (3).
7. The 0.7.1 wrapper crate sits under "Unreleased" in the CHANGELOG, and 0.8.0 needs a different structure
   than the "Unreleased" section has today (3.4).

Ranking used below: **A** = a reader draws a wrong conclusion from it; **B** = stale or inconsistent but a
careful reader can tell; **C** = cosmetic.

## 1. Benchmark claims

### 1.1 [A] HTTP/2 "at half of HTTP/1.1, cause not known"

| where | says | true, and evidence | replacement |
|---|---|---|---|
| `README.md:105` (main) | "HTTP/2 at about half the request rate of HTTP/1.1 for small calls" | the 0.51x measured `cameleer-bench`'s HTTP/2 client; under nghttp2 `h2load` the server serves HTTP/2 at 1.6x to 1.8x its HTTP/1.1 rate (`benchmarks/results/2026-11-19-linux-http2-peer/`, PR 67 branch) | **[PR 67] fixes it** |
| `docs/BENCHMARKS.md:340-343` | "HTTP/2 serves about half ... Not explained. HTTP/1.1 remains the faster protocol" | same | **[PR 67] fixes it** |
| `docs/STATUS.md:544,547,557` | "(not explained)", "why HTTP/2 is slower than HTTP/1.1", "Open: why HTTP/2 serves about half" | same | **[PR 67] fixes it** |
| `llms.txt:19`, `site/cameleer/llms.txt:19`, `scripts/llms/cameleer.txt:19` | "loss: HTTP/2 serves about half the small requests per second of HTTP/1.1 on the same route, cause not known" | same | one clause, see section 5 |
| `site/cameleer/index.html:417` | "One loss: ... HTTP/2 serves about half ... (63,941 against 125,721); the cause is not known yet" | same | section 5 |
| `site/ledger/index.html:139` | table row "HTTP/2 against HTTP/1.1 on 0.7.0 ... 0.51x (loss)" (class `loss`) | not a server loss | section 5 |
| `site/ledger/index.html:145` | "It is not the number of connections and not the load generator; the cause is not known. HTTP/1.1 is the faster protocol for small calls on this server." | the opposite: it was mostly the client (about 53 vs 21 microseconds of load-node CPU per call) | section 5 |

Points where PR 67's own text over-claims (fix in the PR before it merges, or word the 0.8.0 CHANGELOG
carefully):

- **"measured the load generator, not the server"** (PR 67 CHANGELOG, BENCHMARKS, README, STATUS). The PR's
  design note (`docs/design/http2-throughput.md`, "Result in one paragraph") says part of the gap was in the
  server: simoom's lazy event loops left 3 of 4 long-lived HTTP/2 connections on one loop (reproduced on WSL2,
  gRPC unary 27.6k to 57.2k calls/s), and the rest was the client. On the droplets the eager-loops round gave
  `rest-json-h2` 1.05x and gRPC 1.04x ("The droplets do not confirm the local gain"). Say "mostly the load
  generator; on a machine where connections arrive in a burst, also the server's lazy loops".
- **The CHANGELOG headline for eager loops is the WSL2 figure** (gRPC 2.07x, `rest-json-h2` +14%, "means of three
  noisy rounds" on a loaded host with generator and server on the same CPUs). The droplet figure on the same
  change is 1.04x and 1.05x. Quote both, or only the droplets.
- **The h2load table is one run per case, two servers, and not a like-for-like protocol test.** HTTP/1.1 is
  64 connections with one request in flight each; HTTP/2 is 4 connections with 16 streams each. And `h2load`'s
  HTTP/1.1 (86,429 / 70,696) is *below* `cameleer-bench`'s own HTTP/1.1 (103,390 / 75,271), so `h2load` did not
  reach an HTTP/1.1 ceiling either. "HTTP/2 is 1.6x to 1.8x HTTP/1.1 on this server" should read "with `h2load`,
  16 streams per connection, one run on two droplets".
- **Region and build differ between tables on the same page**: the h2load run is sfo3 on `916bbca`; the 0.7.0
  round is blr1. Fine, but the table in BENCHMARKS mixes both `cameleer-bench` rows (103,390; 79,182) with the
  0.7.0 round's (125,721; 63,941) without saying they come from different sessions.

### 1.2 [A] gRPC absolute figures are limited by the load generator

Where: `docs/components/grpc.md:157-164` ("about 41,000 calls per second"), `README.md:106-109` ("3.9x the
0.7.0 rate"), `CHANGELOG.md` Unreleased first bullet ("41,020 against 10,514 per second"),
`docs/BENCHMARKS.md:377-400` (41,020 / 10,514), `site/cameleer/index.html:418`, `site/ledger/index.html:151-165`,
`llms.txt:19`, `site/cameleer/llms.txt:19`, `scripts/llms/cameleer.txt:19`.

- Evidence (PR 67 branch, `docs/design/http2-throughput.md` and `2026-11-19-linux-http2-peer/`): the same
  server shape under `h2load` did **93,157 / 73,426** unary calls/s; `cameleer-bench` reached 58,046 / 49,911
  on the same machines. `cameleer-bench`'s gRPC path is the blocking `simoom-client` (about two thread
  hand-offs per response). So 41,020 is neither "what the server does" nor a stable number: the loops round
  measured the pre-pool-fix `28cea75` (which already includes the pool) at **58,635** gRPC calls/s on another
  session's droplets, against 41,020 for the same code in the memory round. Absolute figures differ by 40%
  between sessions; ratios inside a session are what the harness supports.
- The "3.9x" is also a ratio against a 0.7.0 server that was limited by thread creation (10,500 calls/s), so
  against a fast client it may be larger. It cannot be smaller than what is claimed for the server, but it is
  not a measurement of the server's ceiling.
- Replace "about 41,000 calls per second" with the ratio and the CPU per call (37 against 214 microseconds is a
  server-side figure and holds), and add one sentence: "cameleer-bench's client tops out near 41,000 to
  60,000 calls/s; with nghttp2's `h2load` the same server served 73,000 to 93,000."
- `docs/BENCHMARKS.md:344-346` and `site/ledger/index.html:146` say the ceiling appeared "while neither the
  server nor the load generator was out of CPU". The pool round proved the cause was thread creation; PR 67
  shows the client also has a ceiling. State both.

### 1.3 [A] The Camel comparison is Cameleer 0.4.0

- `README.md:84-104` ("The table above is Cameleer against Apache Camel (0.4.0)" is stated, once, below the
  table), `docs/BENCHMARKS.md:9,123` (Cameleer 0.4.0), `site/cameleer/facts.json:31-40`
  (`measured.version` 0.4.0) are honest. **`llms.txt:3` and `site/cameleer/llms.txt:3`, the site home, and
  `docker/README.dockerhub.md:9` say "starts in 3 ms ... 1.7-8.7x the throughput" with no version**, and the
  current version is 0.7.0.
- The HTTP rows are the ones that changed most: Cameleer's HTTP server was replaced by simoom in 0.6.1. In the
  0.7.0 round (same page, different session, blr1, Xeon 8280 vs the Camel round's 8168) the same code served
  `http` at 146,477 and `rest-json` at 123,793 req/s, against 99,012 and 80,367 in the table. Not comparable
  (different CPU, different session) but it says the table understates today's server by roughly half again,
  and the comparison with Camel should be rerun on the release (1.9).
- **Two figures for the same scenario on one page**: `docs/BENCHMARKS.md:36` HTTP into Kafka 17,854 req/s (1.7x
  Camel) against `:253-259` 48,800 req/s (5.0x), Kafka stream processing 6,081 against 17,881 records/s (8.7x
  against ~28x). README:90-93, the site home (`site/cameleer/index.html:411`), the compare page
  (`site/cameleer/compare/apache-camel-vs-cameleer/index.html:186,200`) and the llms files use the 0.4.0 values.
  The SEO session's 2026-11-07 note already said "do not mix runs"; the page now mixes them in one file.
- **The advertised loss no longer exists in the later run**: "one loss: p99 for HTTP into Kafka is 8.6 ms vs
  7.8 ms" is published in `llms.txt:3`, `site/cameleer/llms.txt:3`, `site/cameleer/facts.json:41-43`,
  `site/cameleer/index.html:416` and BENCHMARKS:84-88. The 2026-11-06 second run (`BENCHMARKS.md:253`) measured
  p99 2.4 ms against Camel's 7.9 ms on the same scenario. Either remove the "loss" after the rerun, or say
  "0.4.0".
- `README.md:26`, `docs/BENCHMARKS.md:1-4`, `docker/README.dockerhub.md:9`: "in every scenario" is true of
  throughput only; the BENCHMARKS page itself lists the p99 exception.
- `docs/BENCHMARKS.md:275-282` "The HTTP ceiling" (166,374 req/s for a thread-per-connection do-nothing server;
  Cameleer "112,910 (68%)") predates simoom. The 0.7.0 round's fixed reply is 146,477 req/s, 88% of that
  "ceiling", and "an event-driven server core is the next step" (line 281) is done. Also `docs/ROADMAP.md:168-169`
  ("68% of a do-nothing server", "Event-driven HTTP server core ... 🚧 next step"), `docs/STATUS.md:306`
  ("an event-driven server core is the next step") and `:663` ("An event-driven cameleer-http server is not
  built"; that section is "Pending / todo", so it reads as current).

### 1.4 [B] MCP figures are 0.5.1 numbers

`39,922 vs 22,604 calls/s, 56 vs 289 MB, 17.7x the TypeScript SDK` in `llms.txt:40`,
`site/cameleer/mcp/index.html:32,295,339`, `compare/camel-mcp-server-vs-cameleer/index.html:32,168,220`,
`compare/enterprise-mcp-platforms/index.html:32,118,157`, `use-cases/mcp-tool-server/index.html:298`,
`site/cameleer/index.html:350`, BENCHMARKS:191-208. Measured on Cameleer 0.5.1 (2026-11-12). The 0.7.0 round
measured the same scenario at 55,816 (session) and 54,179 (stateless) calls/s with p99 4.10 ms, but only for
Cameleer. The "Camel's p99 is lower (5.2 against 6.3 ms)" admission and "1.77x" are 0.5.1 facts. Label the
version in each place, and rerun with the Camel and TypeScript sides (1.9, scenario `mcp-session`).

### 1.5 [B] Two simoom comparisons on the ledger are for simoom 0.1.0

`site/ledger/index.html:177-246` ("simoom 0.1.0", campaign 4 at commit `a81c368`, "simoom is 2nd of 19, 6% and
16% behind may-minihttp"): simoom 0.2.0 changes the server's reply batching and the loops; the figures are
dated and labelled, so this is not wrong, but the claim "simoom ... 2nd of 19" is now about the previous
release. Rerun is simoom's own repository's call; if it is not rerun, keep the "0.1.0" label visible on the
simoom pages.

### 1.6 [B] Machines, regions and setup text

- `docs/BENCHMARKS.md:118` says the three droplets are in `blr1` with Xeon Platinum 8168; the 0.7.0 round
  (`site/ledger/index.html:123`) says blr1 with Xeon Platinum 8280; the pool and memory rounds are sfo3
  (BENCHMARKS:383 says blr1 "no longer offers c-4"). `scripts/cloud-bench/droplets.sh:17` and
  `scripts/cloud-bench/README.md:14` still default to blr1, where `c-4` is not offered: the documented
  `droplets.sh up` fails unless `DO_REGION` is set. Put `DO_REGION=sfo3` in the README (and the default) and
  list the CPU per region.
- `scripts/cloud-bench/README.md` does not mention **[PR 67]** `peer.sh` (h2load), `BENCH_H2_CLIENT_HTTP1`, or
  the h2load package; add a "Telling a server limit from a client limit" section.
- Absolute figures differ 10 to 25% between the two servers of one session and about 40% between sessions
  (1.2). BENCHMARKS only says it for the two servers. Add one sentence: compare ratios inside a session.

### 1.7 [B] Binary size, cold start and memory

| where | says | measured | evidence |
|---|---|---|---|
| `README.md:329` | "down to a 5.9 MB binary with `core`" | 7,047,640 bytes (7.05 MB) stripped, Linux, 0.7.0 | `BENCHMARKS.md:330`, `site/ledger/index.html:130` |
| `docs/DEPLOYMENT.md:252` | "`core` 5.8 MB (glibc) ... against 23.9 MB with every component" | `full` 26,523,736 bytes (26.5 MB) | same |
| `docs/guides/build-features.md:194-195` | table `core` 5.8/6.0 MB, `full` 23.9/24.1 MB | same | same |
| `docs/BENCHMARKS.md:283-299` | FaaS table: HTTP route ready 1.17 ms, 23.9 MB, 10.4 MB RSS | 0.7.0 on the droplet: `full` ready in 2.2 ms, `core` 1.6 ms (different CPU, so not directly comparable) | ledger:130-131 |
| `docs/DEPLOYMENT.md:158`, `llms.txt:31` | "about 10 MB per route" | 10.4 MB RSS in 2026-11-05; the 0.5.0 `full` build measured 11.0 MB; **[PR 67]** adds 0.5 MB when idle and 0.8 ms of start-up on 12 CPUs (except on Lambda) | PR 67 CHANGELOG |
| `docker/README.dockerhub.md:13`, `DEPLOYMENT.md:13` | image "about 20 MB" | not measured since the pack and MCP growth | `container.yml` |

The `core` figure grew with the simoom move (+7.4% at 0.6.1, per BENCHMARKS:329) and with the MCP client; the
docs kept the 2026-11-05 numbers. Re-measure with `scripts/cloud-bench/run.sh sizes` (1.9).

### 1.8 [C] Smaller benchmark inconsistencies

- `README.md:88` "Memory under load 8-68 MB / 213-440 / 112-198" against BENCHMARKS' generated table (7-67, 214-440,
  113-199); `facts.json` says 7-67 and 214-440. Pick the generated values.
- `docs/BENCHMARKS.md:349` ("At 256 connections ... 18.9 against 17.5 MB"): the memory *loss* is in the ledger
  (`:149`) but not the main 0.7.0 summary table; fine, but the page then says in the memory round that 256 calls
  use less. Add a pointer.
- `llms.txt:19` puts "both builds stop near 11,000 calls/s" and "3.9x" in one bullet; the first is 0.7.0 history, the
  second is `main`. Split them (section 5).
- Not measured, and every page says so except the site home: gRPC streaming, HTTP/2 over TLS, the gRPC client
  against the old client, Windows. Keep.

### 1.9 Re-measurement plan after simoom 0.2.0 (needs the owner's approval; nothing was created)

Cost: three `c-4` droplets (4 dedicated vCPU, 8 GB) are about 0.12 US dollars per hour each at DigitalOcean's list
price (check the current price page), so about 0.36 an hour for the trio. BENCHMARKS says a full run is "about one
US dollar". The plan below is two sessions, roughly **2 to 3 dollars in total** including setup time (Rust build
~15 min per droplet, Kafka prefill, the JVM and Quarkus builds). All in one region (`DO_REGION=sfo3`; check that
`c-4` is offered), servers swap roles, run each case twice, compare inside the session. Never touch the droplet
`ubuntu-s-4vcpu-8gb-blr1`.

Session A, Cameleer against itself and against an independent client (about 1.5 hours):

1. Build a baseline branch from `v0.7.0` with the benchmark scenarios cherry-picked (as `bench-base-0.6.1` did),
   then:
   ```sh
   export DO_TOKEN_FILE=~/do-token.txt DO_REGION=sfo3
   scripts/cloud-bench/droplets.sh up
   BENCH_BASE=bench-base-0.7.0 scripts/cloud-bench/run.sh setup
   BENCH_BASE=bench-base-0.7.0 BENCH_SIDES="rust base" BENCH_INPROC=0 \
     BENCH_SERVED="http rest-json rest-json-h2 grpc" BENCH_H2_CONNS=4 scripts/cloud-bench/run.sh bench   # twice
   BENCH_CONNS=256 ... same line                                                                       # once
   python3 scripts/cloud-bench/compare.py "64 connections" results-1.txt results-2.txt
   scripts/cloud-bench/peer.sh                      # h2load: HTTP/1.1 64, HTTP/2 4x16, 16x4, gRPC 4x16; twice
   BENCH_BASE=bench-base-0.7.0 scripts/cloud-bench/run.sh sizes
   ```
   Scenarios that answer open questions: `rest-json-h2` and `grpc` under both clients (does the simoom 0.2.0 client
   close the 0.62x and 0.51x gaps?); `grpc` at 64 and 256 against 0.7.0 (the "3.9x" becomes one figure on
   0.8.0); `http` and `rest-json` against 0.7.0 (does eager loops cost anything, is the 5% on `rest-json` real
   now, `CAMEL_SERVER_LAZY_LOOPS=true` as a third side if wanted); `sizes` (binary, cold start, RSS, with the
   eager-loops cost).
2. Add what is unmeasured and cheap: `h2load` over TLS (`https://`, ALPN `h2`), and gRPC streaming only if a
   scenario exists (it does not; say "not measured" again otherwise).

Session B, the Camel comparison on 0.8.0 (about 2 hours):

   ```sh
   scripts/cloud-bench/run.sh all                    # sides java, rust, native; in-process and served scenarios
   BENCH_SIDES="java rust node" BENCH_INPROC=0 BENCH_SERVED="" scripts/cloud-bench/run.sh bench   # mcp-session; node needs the side node
   python3 scripts/cloud-bench/summarize.py          # regenerates the tables
   scripts/cloud-bench/droplets.sh down
   ```
   Replaces the 0.4.0 table (BENCHMARKS:20-70, README:84-95, the compare page, `facts.json` `measured`, ledger
   "Cameleer, measured 2026-11-06") and the MCP figures (1.4) from one run, ends the "mixed runs" problem, and
   answers whether the Kafka p99 "loss" is still a loss. Camel and Quarkus versions stay pinned. **Decision for the
   owner:** whether to rerun the Camel sides at all (Camel did not change, the Camel numbers from 2026-11-06 stand,
   and only Cameleer's side needs rerunning; the figures then mix sessions, which the page already does).

## 2. Dependency and version statements

| # | rank | where | says | true | replacement |
|---|---|---|---|---|---|
| 2.1 | B | `site/llms.txt:10`, `scripts/llms/cameleering.txt:10` | "simoom 0.1.0" | simoom 0.2.0 released 2026-11-20; **[PR 67]** moves Cargo.toml and `Cargo.lock` (and adds the dev-dependency `simoom-h2` in `cameleer-grpc`, covered by `scripts/sbom.py`'s `simoom-` prefix) | "simoom 0.2.0" |
| 2.2 | B | `site/simoom/**` (`index.html:322`, `docs/**`, `llms.txt:20`, `llms-full.txt:49,56,94,...`, `compare/*`, `use-cases/*`) | `simoom = "0.1"`, "simoom 0.1.0, the first release" | the pages are a copy of simoom's own site; sync from simoom (already 0.2 there, per the simoom session) | site sync |
| 2.3 | B | `site/ledger/index.html:178` | "simoom 0.1.0, the HTTP engine..." | the measurements are 0.1.0; keep, label | see 1.5 |
| 2.4 | **A** | `README.md:425-426` (`cameleer-core = "0.6"`, `cameleer-http = "0.6"`), `docs/guides/getting-started.md:51-52`, `docs/guides/getting-started.md:211` and `docs/guides/cli.md:162` (`cameleer-main = "0.6"`), `site/cameleer/facts.json:21` (`cameleer = "0.6"`), `site/cameleer/index.html:664-665` (`"0.5"`) | old minor versions | cargo reads `"0.6"` as `^0.6` (below 0.7), so the snippet pulls the 0.6 line; `docs/guides/build-features.md:139-148` and `docs/guides/rust.md` use 0.7 | `"0.8"` at the release, `"0.7"` now; add a check to `docs_reference.rs` that every `cameleer-* = "0.x"` snippet matches the workspace version |
| 2.5 | B | `docs/STATUS.md:3-4` | "Last updated 2026-11-16 (release 0.7.0) ... verified head `7de6166`" | main is at `ec33695`; many sections after 0.7.0 | update at release |
| 2.6 | B | `docs/STATUS.md:17` | "19 `cameleer-*` crates" | 22 library crates in the workspace (`ls crates`: 23, minus `cameleer-bench`) plus the `cameleer` wrapper in `publish/` | verify against crates.io, then correct; `CHANGELOG.md:461` is history |
| 2.7 | B | `docs/STATUS.md:57,62,672` | 2249, 2697, "Expect 2844 passing" | latest runs 2961 (0.7.0), 2971 (PR 67), 2968 (PR 68) | "expect about 2,970 (Windows 11, 2026-11-20)" |
| 2.8 | B | `docs/STATUS.md:33,99`, `docs/ROADMAP.md:160` | `http2` listed among the cargo features | removed in 0.6.1 (`--features http2` is an error); `build-features.md` is correct | "plus `templates`, `jq`, `xpath` and `validate`" |
| 2.9 | B | `docs/FEATURES.md:240` | `http` client "`httpVersion=HTTP/2`: cameleer-grpc's HTTP/2 stack" | that stack was deleted in 0.7.0; it is `simoom-client` | "on simoom-client"; `:268` "over cameleer-grpc HTTP/2" likewise |
| 2.10 | B | `docs/INTEROP.md:86` | bug 19 names `H2Server`, `cameleer-grpc` `h2` | code deleted in 0.7.0; add a note "(replaced by simoom in 0.7.0)" | note |
| 2.11 | C | `llms.txt:20` | "(163 pages) ... languages (26 names, 22 pages) and data formats (29)" | the same file says 161 component pages at `:103`; `docs/components` has 161 `.md` and the built site has 163 HTML pages (2 generated extras) | use the `.md` counts (161/21/27) or explain, and let `build-docs.py` fill them in |
| 2.12 | B | `site/cameleer/facts.json`, `llms.txt:22` | version 0.7.0, no mention of the 0.7.1 wrapper | `cameleer` 0.7.1 exists on crates.io (registry name change), the tag and images are 0.7.0 | decide: at 0.8.0 everything is 0.8.0 |
| 2.13 | C | `docs/design/` and CLAUDE.md | rule 1 lists "simoom, simoom-tls and the crates under them" | `simoom-h2`, `simoom-hpack`, `simoom-rt`, `simoom-sys` ... are all in `Cargo.lock`; the rule and `sbom.py` cover them by prefix | none (consistent) |

## 3. How we handle things

### 3.1 [B] Cargo features

- `docs/guides/build-features.md` is right that HTTP/2 is in `core` since 0.7 (`:197,216`). Wrong in the three
  places in 2.8/2.9 and `README.md` ("gRPC and HTTP/2 on simoom" is right).
- **[PR 67]** adds no feature flag, but adds the environment variable `CAMEL_SERVER_LAZY_LOOPS=true|false` and
  keeps lazy start on AWS Lambda (`AWS_LAMBDA_RUNTIME_API`). It is documented in the CHANGELOG, STATUS and the
  design note only. **Add to `docs/guides/configuration.md` (the environment variable list, line 100-105) and
  `docs/DEPLOYMENT.md`** (Lambda and containers: eager is the default, 0.8 ms later on 12 CPUs, +0.5 MB; set
  `true` to restore the old behaviour). `docs_reference.rs` does not check the env list, so no test will
  catch the omission.
- `docs/components/platform-http.md:146` says "an event loop on a few threads": with eager start it is one loop
  per core (12 on a 12-CPU machine). Say "one per CPU".
- If the optional async-routing PR is merged: it adds `CAMEL_ASYNC_ROUTING`, the endpoint option
  `maxInflightRequests` (503 `Retry-After: 1` beyond it) and "on by default" for `platform-http` routes. That is a
  behaviour change (a route answers from a task) and belongs under **Changed**; it needs a page for each
  property, a mention in `observability.md` (the 503) and its own benchmark row. Decide before the release
  notes are written.

### 3.2 [A] HTTP/2 and gRPC statements after PR 67

- `docs/components/grpc.md:157-166` after PR 67 says "7.8 microseconds of server CPU per call, measured locally with
  nghttp2's h2load" next to droplet figures of 37 microseconds per call. The two are different machines, clients
  and builds (WSL2 + simoom 0.2.0 reply batching vs droplets without it). Label each, or keep only one.
- PR 67's grpc.md/CHANGELOG say gRPC unary "235k -> 335k calls/s" and "151k -> 235k" (WSL2, h2load, 12 CPUs). They
  are not comparable with the droplets' 73-93k (4 vCPU) and should not reach the site.
- No page documents the eager/lazy trade-off or when to set the variable (3.1).

### 3.3 [B] "Not released yet" statements that become wrong on release

All say "after release 0.7.0, not released yet" and must flip to "since 0.8.0" with the release:
`README.md:106`, `docs/components/grpc.md:160-161` (and PR 67's added sentence), `llms.txt:19`,
`site/cameleer/llms.txt:19`, `scripts/llms/cameleer.txt:19`, `site/cameleer/index.html:418`,
`site/ledger/index.html:151` (heading "After release 0.7.0, not released yet"), `:155`, and ledger bullet at `:146`
("next table, not released yet"). `CHANGELOG.md` moves from "Unreleased" to "0.8.0".

### 3.4 What the 0.8.0 CHANGELOG entry should say

Today's "Unreleased" has two bullets (the gRPC pool, the 0.7.1 note); PR 67 and PR 68 add seven. Proposed structure
(versions follow the repository rule that a minor before 1.0 lists behaviour changes under **Changed**):

```
## 0.8.0 (YYYY-MM-DD)
Headline: gRPC and HTTP/2 on simoom 0.2.0: the gRPC server runs calls on a pool, the event loops start at once,
unary calls are cheaper, and the 2026-11-18 "HTTP/2 at 0.51x of HTTP/1.1" turned out to be the benchmark's client.

- The gRPC server runs the threads of its calls on a pool (#62, #65).                  [figure: ratio + CPU, 1.2]
- The HTTP and gRPC servers start every event loop. **Changed:** an HTTP route is ready about 0.8 ms later on 12
  CPUs and an idle server uses 0.5 MB more; `CAMEL_SERVER_LAZY_LOOPS=true` restores the lazy start, which stays
  on AWS Lambda.
- gRPC unary calls are cheaper on the server (one response with its trailers; fast unary calls on the loop).
- simoom 0.2.0 (every simoom crate 0.1 -> 0.2): reply batching, head and body in one write, the blocking client's
  HTTP/2 on event loops. The gRPC client sends a call in one write; stream openings are no longer serialized.
- Headers are kept sorted by name (#68). Say it is internal (iteration order is Camel's and unchanged), and drop
  "helps every route" and the 1-5% figure unless it is measured on the synchronous path (it was measured on the
  asynchronous path of a PR that may not ship).
- Benchmarks and docs: HTTP/2 against HTTP/1.1 correction, `scripts/cloud-bench/peer.sh`, new results folders.
- Crate `cameleer` 0.7.1 note: move to its own "0.7.1 (2026-11-14?)" heading (it is published) and add a
  `docs/release-notes/0.7.1.md` or state why not.
Upgrading: the lazy-loops variable; nothing else breaks. State "behaviour: header iteration unchanged".
```
Also add `docs/release-notes/0.8.0.md` (0.6.0, 0.6.1 and 0.7.0 have one; they become GitHub Release bodies).

### 3.5 What was open in the 2026-11-09 review (status on `main`)

| 2026-11-09 finding | now |
|---|---|
| Kafka keystores "not supported" (`kafka.md`), module comment `kafka/mod.rs:39` | **done** (page and comment correct) |
| `camel.ssl.enabled=true` on every global-TLS mention | done (spot-checked ftps/kafka) |
| `cameleer export` standalone claim | **open as a code issue**: still a path dependency; documented (`getting-started.md:207`, `cli.md:158-162` with `cameleer-main = "0.6"`, see 2.4) |
| `SSL_CERT_FILE`, Windows trust store | documented; **code gap still open** (no Windows root store, `x509.rs`) |
| `concepts.md:241-242` | done |
| `INTEROP.md:30,32,253,345` | done: the 72/34/31 runs are dated, the 74/58/35 are the "latest" line |
| `STATUS.md` published | **done**: `INTERNAL` in `build-docs.py:42` has `STATUS.md`; `release-notes` and `design` are internal dirs |
| RTD canonical / toctree | `docs/conf.py:45` sets `html_baseurl` to cameleer.ing; not re-verified the toctree |
| `GUIDE_ORDER` | done (`build-docs.py:65`) |
| `llms.txt` sections, CLI list | done (one "Documentation" section; list includes `test`, `tool`, `cmd`, `log`); counts still off, see 2.11 |
| missing headers/options on five component pages | not re-checked |
| Missing cross-cutting guides (1-8) | **done** (2026-11-09 and 11-11) |
| Learning path: tutorial, recipes, per-transport "errors and retries", YAML reference gaps, deployment (systemd, Windows service, Helm) | **open** (not started; `docs/guides` has 30 pages + 10 MCP) |
| Answer-engine: FAQs on 22 MCP/agent pages; `llms-full.txt` size; orphan pages | `llms` is now split into bundles (done); orphans **still open**: `aws2-timestream`, `cql`, `google-firestore`, `google-functions`, `google-secret-manager`, `saga` have no inbound link from `docs/` |
| `cameleer catalog component <scheme>` for agents | **open** (`cli.md:186` lists `catalog [components\|languages\|kamelets]` only) |
| `cameleer export` from crates.io/image | open (code) |

## 4. Links, anchors and the docs harness

- Main is clean: `check-site.py` found 0 broken links, anchors, JSON-LD, redirects or sitemap problems; the anchors
  the site uses into BENCHMARKS (`#grpc-and-http2-on-simoom-070-2026-11-18`,
  `#grpc-on-a-pool-of-threads-without-the-memory-cost-2026-11-19`, `#the-http-ceiling`,
  `#faas-cold-start-and-size`) all exist.
- **[PR 67] / [PR 68]**: they do not rename a heading, so no anchor breaks. PR 67 changes a bullet's title in
  BENCHMARKS; nothing links to it. PR 67 adds `docs/design/http2-throughput.md` (internal, excluded from the site).
  The docs harness tests (`docs_reference`, `docs_*`, `features`) are unaffected by their doc edits; I did not
  run them on the branches.
- **If the 0.8.0 text changes headings**: BENCHMARKS' "gRPC and HTTP/2 on simoom (0.7.0, 2026-11-18)" is linked by
  README, grpc.md, `site/cameleer/index.html:417`, `site/ledger/index.html:139,157`, `llms.txt:19`; keep the
  heading or add a redirect, and rebuild with `build-docs.py` + `check-site.py`.
- `llms.txt` (root) and `site/cameleer/llms.txt` are **copies of `scripts/llms/cameleer.txt` plus the generated
  bundle list**; edit the source file under `scripts/llms/` and let `build-docs.py` write the others, or the next
  build overwrites the edit. Same for `scripts/llms/cameleering.txt` -> `site/llms.txt`.
- Suggested new guards (small, each catches a finding above): a test that every `cameleer-* = "0.x"` snippet
  in `README.md`, `docs/**` and `site/**/facts.json` equals the workspace minor; a test that `CAMEL_*`
  environment variables read in `crates/**/src` (grep `std::env::var("CAMEL_`) appear in `docs/guides/configuration.md`;
  a check that "not released yet" phrases are absent from a tagged commit (a CI step on `v*` tags).

## 5. Site pages and llms files for the SEO session (exact edits)

Everything here is `site/**`, `llms.txt`, `scripts/llms/*` or `facts.json`; do not edit from this branch.

### 5.1 HTTP/2 correction (from PR 67's note `site-llms-corrections.md`; the old diff conflicts with current text)

- `scripts/llms/cameleer.txt:19` (then `llms.txt:19` and `site/cameleer/llms.txt:19` by rebuild): replace
  "loss: HTTP/2 serves about half the small requests per second of HTTP/1.1 on the same route, cause not known;"
  with "an HTTP/2 result of about half the HTTP/1.1 rate (2026-11-18) measured the benchmark's own HTTP/2 client:
  driven by nghttp2's h2load (2026-11-19) the server serves HTTP/2 at 1.6x to 1.8x its HTTP/1.1 rate (one run,
  two servers);". In the same bullet split "(both builds stop near 11,000 calls/s)" (0.7.0 and 0.6.1) from the
  "3.9x" sentence, and replace "41,020 against 10,514" by the ratio and 37 against 214 microseconds, with "the
  load generator tops out below what h2load reached on the same server (73,000 to 93,000 calls/s)".
- `site/cameleer/index.html:417` (paragraph "0.7.0 against 0.6.1"): delete "**One loss:** on this server HTTP/2 serves
  about half ... the cause is not known yet." and put "HTTP/2 first measured at about half the HTTP/1.1 rate;
  that was the benchmark's own client (nghttp2's h2load: 1.6x to 1.8x HTTP/1.1)."
  `:418`: "After release 0.7.0, not released yet" -> "Since 0.8.0" at the release, and soften "41,020 against 10,514"
  as in 1.2.
- `site/ledger/index.html:139`: remove `class="loss"` from the HTTP/2 row, reword to "measured by the benchmark's
  own client"; add a row for `h2load` 2026-11-19 (HTTP/1.1 86,429 / 70,696; HTTP/2 153,010 / 115,820 req/s; gRPC
  93,157 / 73,426 calls/s; servers a / b, one run). `:145`: replace the "cause is not known" bullet with the
  explanation. `:146`: add "the load generator's client also has a ceiling" . `:151`, `:155`: "not released yet"
  wording as in 3.3; `:165` (note) keep.
- Do not publish PR 67's WSL2 figures (2.07x, 235k/335k calls/s) as results; the droplets gave 1.04x (1.1).

### 5.2 Headline claims with a version

- `llms.txt:3`, `site/cameleer/llms.txt:3` (and `scripts/llms/cameleer.txt:3`): after "1.7-8.7x the throughput"
  add "(Cameleer 0.4.0, 2026-11-06)"; and "one loss: p99 for HTTP into Kafka is 8.6 ms vs 7.8 ms" -> remove or
  version it (the later run measured 2.4 against 7.9 ms; 1.3). Same in `site/cameleer/index.html:416`,
  `site/cameleer/facts.json:41-43`, `docker/README.dockerhub.md:9` (not site, for the repo owner).
- After the rerun (1.9): replace the 0.4.0 table values on `site/cameleer/index.html:411`, the compare page
  (`compare/apache-camel-vs-cameleer/index.html:186,200`, the `bench:` markers show it is generated by
  `summarize.py`), `facts.json` `measured`, and the ledger "Cameleer, measured 2026-11-06" section.
- MCP pages (1.4): version-label `39,922` / `22,604` / `17.7x` on `mcp/index.html:32,295,339`,
  `compare/camel-mcp-server-vs-cameleer/index.html:32,168,220`, `compare/enterprise-mcp-platforms/index.html:32,118,157`,
  `use-cases/mcp-tool-server/index.html:298`, `site/cameleer/index.html:350`, `llms.txt:40`.

### 5.3 Versions

- `site/llms.txt:10`, `scripts/llms/cameleering.txt:10`: "simoom 0.1.0" -> "simoom 0.2.0".
- `site/simoom/**`: resync from simoom (2.2).
- `site/cameleer/facts.json:21`: `"cameleer = \"0.6\""` -> the current minor; `:6-7` version and date, `install.container`,
  `image_tags` at the release.
- `site/cameleer/index.html:664-665`: `"0.5"` -> current minor.
- `llms.txt:20`: counts (2.11).

### 5.4 At the 0.8.0 release (all places that carry the version)

`facts.json` (`version`, `released`, `install.container`, `image_tags`), `llms.txt:3,22` and the two copies,
`site/cameleer/index.html` hero and install lines, `docker/README.dockerhub.md:65-68`, `site/feed.xml`, `README`
install lines. The 0.7.0 release commit (`c6c13c7`) shows the set.

## 6. Proposed 0.8.0 release checklist

Code
- [ ] Merge PR 67 (after the wording fixes in 1.1) and PR 68 (drop or re-measure the "helps every route" figure);
      decide the optional async-routing PR (default on; needs docs and a benchmark row, 3.1) or leave it for 0.9.
- [ ] Version `0.8.0` in the workspace (`cameleer-health` keeps `0.6.0` unless it changed); `Cargo.lock` with
      published simoom 0.2.0 crates; no `[patch]`.
- [ ] `cargo build --workspace` with zero warnings; `cargo test --workspace` (expect about 2,970); `cargo test -p
      cameleer-main --no-default-features` and with `--features health`; the official tier in WSL (HTTP/2 3/3, TLS
      2/2, gRPC 2/2); live and simulator tiers (not run on the simoom client since 0.6.1; STATUS "Not verified").
Docs
- [ ] CHANGELOG "0.8.0" (3.4), `docs/release-notes/0.8.0.md`, a heading for 0.7.1.
- [ ] STATUS: header, test counts, crate count, remove the "event-driven next step", the `http2` feature mentions.
- [ ] README, grpc.md, BENCHMARKS: flip "not released yet" (3.3), fix 1.1 to 1.3, 1.7; add the env variable (3.1);
      fix `0.6` snippets (2.4); ROADMAP 2.3 table; FEATURES:240,268.
- [ ] Add the guards in section 4. Run `python scripts/build-docs.py && python scripts/check-site.py` (0 warnings).
Benchmarks
- [ ] The two droplet sessions in 1.9 (owner approval); results under `benchmarks/results/<date>-linux-0.8.0/`;
      `summarize.py` tables; the cloud-bench README (region, `peer.sh`).
Site and llms (SEO session)
- [ ] Section 5 edits; regenerate `llms.txt` and bundles; `check-site.py`; sitemap.
Containers
- [ ] `container.yml` on the `v0.8.0` tag: tags `0.8.0`, `0.8`, `latest`, `health`, `0.8.0-health`, `0.8-health`
      (docker.io and ghcr.io, amd64 and arm64); confirm the first real run of the health variant (still open since
      0.6.0); image sizes re-measured (1.7); DT_RELR for arm64 still open.
Crates
- [ ] `scripts/publish-crates.sh` (dependency order, waits out the new-crate rate limit; no new crate this time);
      the `cameleer` wrapper to 0.8.0; check `cargo install cameleer` and `cargo add cameleer`.
Archives and registry
- [ ] `archives.yml` on the tag: Linux x86-64, Windows x86-64, macOS arm64, SHA-256, SBOM, cosign bundle attached.
- [ ] MCP Registry: `server.json` version, README `mcp-name` line, the OIDC workflow; name
      `io.github.Cameleering/cameleer` (case-sensitive).
- [ ] The repository rule: main is protected (PR with three checks, immutable `v*` tags); no AI trailers or model
      names in commits or PRs; the release PR is the owner's call.
- [ ] Tell the simoom session and the SEO session when the tag exists.

Not covered by this review: `docs/components/**` page by page (spot-checked grpc, http, platform-http, kafka),
`docs/design/**` (internal), the 60 or so site pages other than home, ledger, compare, mcp and projects, and
anything reachable only on crates.io, Docker Hub or the MCP Registry.
