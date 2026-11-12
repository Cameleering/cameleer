# Cameleering

**Carry more on less.** No claim without a number.

Cameleering is the practice of making software carry the same load on less (less time, memory,
size and dependencies) and proving every gain against the original, on the same hardware, with the
method published. Sometimes the answer is a setting or a smaller default. Sometimes it is a rewrite.

This organization holds the projects that put it into practice, and the ledger that keeps them
honest.

| project | what it is | status | result |
|---|---|---|---|
| [cameleer](https://github.com/cameleering/cameleer) | integration engine in the style of Apache Camel, one std-only native binary | measured | vs Apache Camel 4.23.0 on Java 21: startup 3 ms vs 1.1 s; peak memory 4.8–54x less; throughput 1.7–8.7x; one p99 loss ([ledger](https://cameleer.ing/cameleering/ledger/)) |
| simoom | embeddable HTTP/1.1, HTTP/2 and HTTP/3 server engine | in progress | not measured yet |
| karez | database connectivity shaped like JDBC, ODBC and ADBC, with Arrow results | design | targets only |
| (crypto family) | cryptographic primitives and PKI | planned | none |

The principles are on the site:
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

Independent open source under the Apache License 2.0. Not affiliated with or endorsed by the Apache
Software Foundation; Apache, Apache Camel and Camel are trademarks of the ASF.

<!-- Draft for cameleering/.github profile/README.md. Publish on transfer day; link simoom and karez
     only once their repos are public. -->
