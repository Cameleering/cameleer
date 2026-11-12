# Cameleering

**Carry more on less.** No claim without a number.

Cameleering is a way of engineering: make software do the same job on less time, memory, size and
dependencies, and prove every gain against the original, on the same hardware, with the method
published. Sometimes the answer is a setting or a smaller default. Sometimes it is a rewrite.

This organization holds the projects that put it into practice. Each one is measured before it
claims anything.

## The projects

| project | what it is | status |
|---|---|---|
| **[cameleer](https://github.com/cameleering/cameleer)** | Apache Camel routes in Rust, and the reliable action layer for AI agents: one std-only native binary, about 230 components | **measured** against Apache Camel 4.23.0 on Java 21, same machines: startup 3 ms vs 1.1 s, peak memory 4.8–54x less, throughput 1.7–8.7x, one p99 loss ([the ledger](https://cameleer.ing/cameleering/ledger/)) |
| **simoom** | an embeddable HTTP/1.1, HTTP/2 and HTTP/3 server engine in std-only Rust | in progress, not public yet, not measured |
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
