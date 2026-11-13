# Home page copy: cameleer.ing (Cameleering)

_Draft, 2026-11-13. Copy only; no site files changed. Lines starting with **Note** are editorial notes
for whoever builds the page, not page copy. Every figure comes from `site/ledger/`, `site/finops/`,
`site/cameleer/`, `site/simoom/` or `site/simoom/benchmarks/`, with ratios rounded down. FinOps
figures are arithmetic and say so where they appear._

**`<title>`** (58 chars): `Software Efficiency: Same Work, Less Machine | Cameleering`

**Meta description** (155 chars): `Make software do the same work on less machine, and prove it. Cameleer starts in 3 ms where Apache Camel takes 1.1 s. The method and every result are open.`

---

## 1. Hero

**Note:** The purpose is to show the results first and the way to get them second. The eyebrow
stays small.

**Eyebrow:** Cameleering · software efficiency, measured against the original

**H1:** The same work, on a fraction of the machine.

**Sub:** Cameleering makes software carry the same load on less memory, CPU and start-up time, then
proves each gain against the original on the same machines. Below are the results so far, and the
steps to get your own.

**Outcome cards** (each has a headline, the before and after, and a project tag):

| headline | before → after | tag |
|---|---|---|
| **450x faster start-up** | 1.1 s → 3 ms to a started context (Apache Camel 4.23 on Java 21; on Quarkus native, 30 ms) | Cameleer vs Apache Camel |
| **300x cheaper cold starts** | $9.17 → $0.03 per million AWS Lambda cold starts at 512 MB | Cameleer · arithmetic on measured start-up |
| **51x more integrations per node** | 20 → 1,024 per 8 GiB on a direct route; 24 → 122 for HTTP into Kafka | Cameleer · arithmetic on measured peak memory |
| **Up to 54x less memory** | Peak 214–440 MB → 7–67 MB: 4.8x to 54x less across 11 scenarios | Cameleer vs Apache Camel |
| **1.7–8.7x the throughput** | More messages per second than Apache Camel in all 11 scenarios, on the same machines | Cameleer vs Apache Camel |
| **43% less CPU per request** | About 23 µs → 13 µs, with 1.7–1.8x the requests per second | simoom vs cameleer-http |

**Losses line:** Where we lose, shown here with the wins: Cameleer's p99 for HTTP into Kafka is
8.6 ms against Apache Camel's 7.8 ms. simoom is 6% behind may-minihttp on plain HTTP and 43% behind
H2O on HTTP/3. Every row is in [the ledger](/ledger/).

**CTAs:** primary **Run it on your software** → `#playbook`; secondary **See every result and
loss** → `/ledger/`

**Product exits** (a one-line row; on phones it sits inside the first screen):
- I pay for the machines → [What code efficiency saves](/finops/)
- I run Apache Camel → [Cameleer runs its routes in one binary](/cameleer/)
- I need a Rust HTTP server → [simoom: no tokio, no dependencies](/simoom/)
- I build AI agents → [Cameleer as an MCP tool server](/cameleer/mcp/)

---

## 2. Definition strip

**Note:** This strip is where visitors get the concept. Keep it to one sentence and the house rule.

> Cameleering is the practice of making software carry the same load on less (less time, memory,
> size and dependencies) and proving every gain against the original, on the same hardware, with the
> method published.

**Small line:** Carry more on less. No claim without a number.

---

## 3. Benefits

**Note:** The benefits are told in the reader's terms. Each card has one proof and one link.

**H2:** What less machine buys you

**Sub:** Software decides how much memory and CPU every instance needs. Flexera's 2026 survey puts
cloud waste at 29% of infrastructure spend.

**Card: A smaller bill.** Serverless now bills the cold start. At 512 MB, a million cold starts cost
$9.17 for Apache Camel on Java 21 and $0.03 for Cameleer. A function that fits in 128 MB instead of
512 MB costs $0.21 instead of $0.83 per million 100 ms calls. _Arithmetic on measured numbers and
AWS list prices._ → [The arithmetic, with its limits](/finops/)

**Card: Less waiting.** A started context in 3 ms instead of 1.1 s. A REST API with JSON has a p99
of 2.6 ms instead of 6.3 ms. Behind simoom, p99 fell from 0.87–0.91 ms to 0.32–0.44 ms while serving
1.7–1.8x the requests. → [Cameleer's numbers](/cameleer/)

**Card: Fewer machines.** 1,024 integrations fit in 8 GiB by peak memory instead of 20. At 1.7–8.7x
the throughput on the same CPUs, the same load needs fewer vCPUs, and each vCPU costs about
$29.55 a month on Fargate at list price. _Arithmetic; real density is lower on both sides._ →
[Density and vCPU worked examples](/finops/)

**Card: Simpler operations.** Cameleer is one binary with no JVM to install: 8 MB at start-up
instead of 114 MB, and down to 5.9 MB on disk with only the parts you build in. simoom has no
crates.io dependencies at all and holds 1,000 idle connections in 2.7 MB instead of 7.7 MB. →
[simoom](/simoom/)

---

## 4. Playbook: do it on your own software

**Note:** Give this section the anchor `id="playbook"`. It is the method, made concrete: seven steps
a team can start next week. Show each step as a row or card with four labels: Do, Record, Use,
Done when.

**H2:** Do it on your own software

**Sub:** Seven steps, in any language, on the service you already run. You need one service, two
identical machines and a load generator on a third machine.

**1. Pick the service that costs the most**
- **Do:** rank your services by spend in your cost reports and take the top one.
- **Record:** its monthly cost, instance count and the unit you are billed in.
- **Use:** step 1 of [the FinOps practice](/finops/).
- **Done when:** one service is named, with its cost on one line.

**2. Baseline it on fixed hardware**
- **Do:** measure start-up, idle and peak memory, throughput, p99, CPU per request, size and
  dependencies. Report the median of many runs.
- **Record:** the exact version, runtime, flags and machine type.
- **Use:** "What to measure" and the fair-baseline rules on [the method](/method/).
- **Done when:** a teammate can rerun the baseline with one script and get the same figures.

**3. Translate and budget**
- **Do:** turn each number into what you pay for: GB-seconds, vCPU-hours, instances per node. Set a
  target for each, labelled as a target.
- **Record:** the arithmetic, and what it leaves out.
- **Use:** the worked examples on [the FinOps page](/finops/).
- **Done when:** every target has a number, a unit and a price.

**4. Cut, starting with the smallest step**
- **Do:** climb the ladder one rung at a time: tune, remove, replace one component (strangler
  fig), rewrite the hot path; rewrite it all only when the design is the limit. Keep the behaviour
  users rely on.
- **Record:** one change per measurement, and why you made it.
- **Use:** [the ladder and the rewrite checklist](/when-to-rewrite/).
- **Done when:** the behaviour tests still pass and the change moved a number.

**5. Prove it on the same machines**
- **Do:** rerun the same harness on the same hardware, swapping the roles of the two machines.
  Round against yourself: your times and memory up, your ratios down.
- **Record:** both ends of every range, and every scenario you lose.
- **Use:** the "Don't" list on [the method](/method/).
- **Done when:** you have a before-and-after table with the losses in it.

**6. Rightsize, then guard**
- **Do:** lower requests, limits and memory settings to match, or the software shrinks and the bill
  does not. Then make CI fail on size, memory, start-up and p99 regressions.
- **Record:** the answers to the ten-question water check before each release.
- **Use:** the water check on [the method](/method/).
- **Done when:** a regression fails the build before it reaches the bill.

**7. Write the crossing report**
- **Do:** write one page per result, with these sections: **Load** (the workload), **Baseline**
  (exact versions), **Terrain** (hardware and OS), **Water** (start-up, memory, size, CPU),
  **Distance** (throughput and latency), **Losses**, **Repeat it** (scripts and raw results) and
  **Date**.
- **Done when:** someone outside the team could check your numbers.

**Box: When a result can be called Cameleered**
All five must hold:
- a named, current baseline version;
- both sides on the same hardware;
- a script anyone can rerun, and the raw results;
- the losses disclosed;
- measured within the last 12 months.

Work in progress is **Cameleering** and shows targets only. Work that has not started is
**planned**.

**CTAs:** **Open the method** → `/method/` · **Is a rewrite worth it?** → `/when-to-rewrite/`

**Help line:** Stuck on a baseline, or want a second pair of eyes? Ask in [GitHub
Discussions](https://github.com/cameleering/cameleer/discussions). The maintainer is open to
consulting.

---

## 5. Proof: two crossings, published

**Note:** This section shows the method working on widely used software, kept short with links out.

**H2:** It works on software people run every day

**Sub:** Two projects have run every step in public.

**Card: Cameleer** · Cameleered · 2026-11-06
An integration engine in the style of Apache Camel, and the first Cameleering project.
- **Baseline:** Apache Camel 4.23.0 on Java 21 and 4.22.1 on Quarkus native, on identical
  DigitalOcean c-4 machines.
- **Cut:** one std-only binary that keeps Camel's routes, YAML and endpoint URIs, with component
  families as build options.
- **Proved:** start-up 450x faster (10x against Quarkus native), 4.8–54x less memory, 1.7–8.7x the
  throughput.
- **Lost:** p99 for HTTP into Kafka, 8.6 ms against 7.8 ms.

→ [Full comparison](/cameleer/compare/apache-camel-vs-cameleer/) · [Every scenario and the raw
data](/docs/BENCHMARKS.html)

**Card: simoom** · Cameleered · 2026-11-12
An HTTP engine in std-only Rust, built to replace Cameleer's original server.
- **Against the server it replaced:** 1.7–1.8x the throughput, 43% less CPU per request, 65% less
  memory with 1,000 idle connections.
- **Against 20 other servers on the same droplets:** first of 21 on start-up (0.94 ms) and first of
  19 on CPU per request.
- **Lost:** 6% behind may-minihttp on plain HTTP, 43% behind H2O on HTTP/3, and 10th of 19 on
  answers within 1 ms under overload.

→ [simoom benchmarks](/simoom/benchmarks/) · [All projects, including the ones not yet
measured](/projects/)

---

## 6. Why now

**H2:** Why now: using less is a responsibility

**Body:** We spent faster chips and cheaper memory on features and layers, often for good reasons.
Now code is cheap to produce: in one benchmark, GPT-4's code took 3.12 times as long to run as human
solutions. The thing to fight is waste, not the people who build features.

- **The people who pay for the machines:** every idle gigabyte is paid for, every month, by someone.
- **The people who wait:** a slow start or a slow p99 is a person waiting, or a function billed
  while it warms up.
- **The power we share:** the IEA projects data-centre electricity use to reach about 945 TWh by
  2030. Cameleering reports milliseconds and megabytes and makes no carbon claims of its own.

---

## 7. FAQ

**H2:** Common questions

**Is this "rewrite it in Rust"?**
No. The gains come from how software uses its language: what goes into the build, which defaults
ship, and how many threads and allocations a request costs. A rewrite is the top step of the
ladder, used only when the design is the limit. Each claim is a measured result, never the language.

**Will I get the same savings?**
Not necessarily. The FinOps figures are arithmetic on measured numbers and list prices, and each
says what it leaves out. Yours depend on your workload, your discounts and whether you rightsize
afterwards. That is why step 2 is your own baseline.

**Why should I trust these numbers?**
Check them. Every result names its baseline version and hardware, and links to the scripts and raw
results. Ratios are rounded down, and the losses are in the same table as the wins.

**Can we do this on closed-source software?**
Yes. The steps work the same in private, and the crossing report goes to your team instead of the
web. The ledger here only lists results whose scripts and raw data are public.

**How is this different from FinOps?**
FinOps rightsizes the infrastructure to fit the software. Cameleering changes how much the software
needs, so the next rightsizing goes further. The two fit together.

**What does Cameleering sell?**
Nothing. The projects are open source under the Apache License 2.0, with no paid edition. The
maintainer is open to consulting; ask in GitHub Discussions.

**Is Cameleering affiliated with the Apache Software Foundation?**
No. Cameleer follows Apache Camel's names and semantics but contains no Camel source code and is not
affiliated with or endorsed by the ASF.

---

## 8. Final CTA

**H2:** Pick one service. Measure it this week.

**Sub:** Start with the baseline. Everything after it is a number you can show.

**CTAs:** primary **Open the method** → `/method/`; secondary **See the ledger** → `/ledger/`;
text link **Ask in GitHub Discussions** → https://github.com/cameleering/cameleer/discussions

**Note:** The footer stays as it is today, including the ASF trademark line.
