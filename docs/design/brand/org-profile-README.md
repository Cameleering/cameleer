# Cameleering

**Carry more on less.** No claim without a number.

Cameleering is a way of engineering: make software do the same job on less time, memory, size and
dependencies, and prove every gain against the original, on the same hardware, with the method
published. Sometimes the answer is a setting or a smaller default. Sometimes it is a rewrite.

This organization holds the projects that put it into practice. Each one is measured before it
claims anything.

## Why cameleering, and why now

### Cheap hardware, and what we spent it on

Chips got faster every year, and memory and disks got cheaper. That was good news, and we spent it:
on features, frameworks and layers, often for good reasons. The cost did not go away. It moved into
bigger binaries, slower starts, and machines that sit half idle while they wait.

Along the way the unit of care changed. Engineers who once counted CPU cycles began counting
milliseconds, then seconds. Asynchronous calls and loading screens made waiting feel shorter. They
did not make it shorter: a spinner hides a bottleneck, it does not remove it.

### Coding agents, and more data

Code is now cheap to produce, and the speed of building software has become the benchmark instead
of the speed of the software. In one benchmark, code written by GPT-4 took on average 3.12 times as
long to run as human solutions to the same tasks
([EffiBench, NeurIPS 2024](https://arxiv.org/abs/2402.02037)). In a controlled study, experienced
developers using AI tools were 19% slower while believing they were faster
([METR, July 2025](https://metr.org/blog/2025-07-10-early-2025-ai-experienced-os-dev-study/)); a 2026
follow-up suggests that has since changed, but could not measure it cleanly
([METR, February 2026](https://metr.org/blog/2026-02-24-uplift-update/)). Either way, those studies
measure how fast people finish, not how fast the software runs.
Code duplication in large codebases rose sharply in 2024
([GitClear](https://www.gitclear.com/ai_assistant_code_quality_2025_research)), though that study
cannot say which lines AI wrote, and other research found small quality gains from AI adoption
([DORA 2024](https://dora.dev/research/2024/dora-report/)).

At the same time every product collects more data than the last, so the work of storing and
processing it keeps growing.

## The projects

| project | what it is | status |
|---|---|---|
| **[cameleer](https://github.com/cameleering/cameleer)** | Apache Camel routes in Rust, and the reliable action layer for AI agents: one std-only native binary, about 230 components | **measured** against Apache Camel 4.23.0 on Java 21, same machines: startup 3 ms vs 1.1 s, peak memory 4.8–54x less, throughput 1.7–8.7x, one p99 loss ([the ledger](https://cameleer.ing/cameleering/ledger/)) |
| **[simoom](https://github.com/cameleering/simoom)** | an embeddable HTTP/1.1, HTTP/2 and HTTP/3 server engine in std-only Rust, Cameleer's HTTP engine | **measured** against Cameleer's previous HTTP server: 1.7–1.8x throughput, 43% less CPU per request, 65% less memory with 1,000 idle connections; against hyper 1.12.0 and may-minihttp 0.1.11 on the same machines: +8% to +23% plaintext throughput; losses listed ([the ledger](https://cameleer.ing/cameleering/ledger/#simoom)); not on crates.io yet |
| **karez** | database connectivity for Rust shaped like JDBC, ODBC and ADBC, with Arrow results | design stage, not public yet, targets only |
| **ksar** | cryptographic primitives and PKI | planned, no repository yet |

A project's numbers belong to that project alone. A project with no measurements shows its status
and nothing else.

## The principles

1. Measure before you claim.
2. Baselines over boasts.
3. Same job, same contract.
4. Pack only what the journey needs.
5. Real waiting over felt waiting.
6. Budgets in the build.
7. Rewrite when the numbers say so.
8. Publish the losses.

[What is cameleering?](https://cameleer.ing/cameleering/) · [The ledger](https://cameleer.ing/cameleering/ledger/) ·
[The method](https://cameleer.ing/cameleering/method/)

## Where to find Cameleer

- **Site and docs:** [cameleer.ing](https://cameleer.ing/) · [cameleer.ing/docs](https://cameleer.ing/docs/)
- **Source:** [github.com/cameleering/cameleer](https://github.com/cameleering/cameleer)
- **Container image:** [`cameleering/cameleer`](https://hub.docker.com/r/cameleering/cameleer) on Docker Hub, and `ghcr.io/cameleering/cameleer`
- **Crates:** [`cameleer-main`](https://crates.io/crates/cameleer-main) and the other `cameleer-*` crates on crates.io
- **For AI assistants:** [cameleer.ing/llms.txt](https://cameleer.ing/llms.txt)

## Contributing

Issues and pull requests are welcome in each project's repository; each one has a CONTRIBUTING.md.
If you measured something differently and got a different number, open an issue with your method:
a corrected row is worth more than a flattering one.

Independent open source under the Apache License 2.0. Not affiliated with or endorsed by the Apache
Software Foundation; Apache, Apache Camel and Camel are trademarks of the ASF.
