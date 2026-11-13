# Home page positioning and message architecture (cameleer.ing)

_Strategy note, 2026-11-13. Input: `site/index.html` (f448c2e), the brand guide, the ledger,
FinOps, method and when-to-rewrite pages, `personas-ux.md`, `review-ux-content.md`. No site files
changed._

**The diagnosis in one line:** the page explains a *practice* to people who arrived wanting a
*result*. The visitor's question is "what do I get, and can I get it too?". The page answers "here
is what cameleering is". Fix the order: outcome, then the money, then the playbook. Only after
that comes the name.

---

## 1. Positioning canvas

### Competitive alternatives (what the visitor does if we don't exist)

| alternative | what it leaves on the table | our line against it |
|---|---|---|
| Buy more cloud | the waste, billed every month | "Every idle gigabyte is billed monthly." |
| FinOps rightsizing, commitments | how much the software needs | "Rightsizing fits the box to the software. This shrinks the software." |
| JVM tuning, Quarkus native, GraalVM, SnapStart | the same layers and threads; native still takes 30 ms and 69 MB | "We compare against Quarkus native too: 10x faster start." |
| Kubernetes autoscaling, scale to zero | billed cold starts; density unchanged | "Scale to zero only pays if the start is cheap." |
| "Just rewrite it" | the risk, the contract, the proof | "A rewrite is the last rung, used when the numbers say so." |
| Green-software reporting (SCI) | the code itself is unchanged | "SCI reports it. This removes it. They fit together." |

### Unique attributes (no rival has them together)

1. Before and after against the original, on the same machines, including the strongest
   baseline (Quarkus native; 20 HTTP servers for simoom).
2. Losses in the same table as the wins.
3. The same contract kept: Camel's YAML routes run unchanged.
4. A method anyone can run: eight metrics, fair-baseline rules, a ten-question check, a report
   format.
5. Zero third-party dependencies in both shipped projects.
6. Nothing to buy: Apache 2.0, with the harness and the raw data open.

### Value (in the visitor's own terms)

- **Fewer, smaller machines** (density, vCPUs), from less memory and more throughput per CPU.
- **Cold starts that cost cents**, so scale to zero works, from millisecond start-up.
- **Users wait less**, from lower p99 where it is lower.
- **Less to ship, patch and audit**, from one std-only binary.
- **The same result on your own code, with proof finance accepts**, from the method. This is the
  differentiator.

### Target segments, ranked for the home page

1. **CTO or VP Engineering under cloud-cost pressure** (JVM-heavy, Kubernetes or Lambda).
   Question: "Is this real money, and is it a pitch?"
2. **FinOps practitioner.** Question: "Is there a method I can hand to engineering, with sources?"
3. **Platform or SRE engineer** running JVM integration on Kubernetes. Question: "How much
   better is it, really, and does it run my routes?" Route them to Cameleer in one click.
4. **Efficiency-minded engineer.** Question: "Is the method rigorous, and can I copy it?"
5. *Product seekers* (Rust HTTP, MCP, healthcare). These are not a home-page audience: give them
   a one-click exit above the fold and nothing else.

### Market category

- **Category:** *efficiency engineering* (as in the brand guide), with Cameleering as the named
  method inside it. Don't create a new category on the home page. It costs the paragraph of
  explanation that is losing visitors now.
- **Bridging frame:** "the code side of FinOps". FinOps already has a budget and an audience, and
  its Usage Optimization capability says engineering does the work. Use the phrase as the
  sub-headline, not as a brand.
- **The word "cameleering"** comes only after the outcome and the playbook, as a label: "We call
  this cameleering."

---

## 2. Message hierarchy

### One-line promise (H1 candidates, 10 words or fewer)

| # | H1 | why |
|---|---|---|
| **A (recommended)** | **Shrink what your software needs, not just the instances.** | Names the alternative (rightsizing) and the difference; "your" puts the reader's estate in it. |
| B | The same work, on a fraction of the machine. | Today's H1. Calm but abstract: no actor, no money. Keep it for the OG text. |
| C | Same software, same job, far fewer machines. | Concrete and CFO-friendly; needs the numbers right beside it. |
| D | Cut what the software costs to run. Then prove it. | The promise plus the proof habit; suits FinOps traffic. |
| E | 3 ms instead of 1.1 s. Here is how. | The strongest number, but it needs its project and baseline beside it. Use it as a social card. |

**Sub-headline (recommended):** "The code side of FinOps. We rebuild widely used software to
need less memory, CPU and start-up time, prove it against the original on the same machines,
losses included, and publish the playbook so you can do the same to yours."

The house rule ("No claim without a number") goes in small type under the CTAs, not in the
headline. The owner is right that "measured" is not the benefit.

### Three benefit pillars and their proof

Only ledger and FinOps numbers appear here. Each one names its project. Ratios are rounded down,
and both ends of a range are shown.

**Pillar 1. Fewer machines for the same work.**
- 4.8-54x less peak memory: 7-67 MB against 214-440 MB, 11 scenarios (Cameleer vs Apache Camel 4.23, JVM).
- 20 → 1,024 integrations per 8 GiB node on a direct route, 24 → 122 for HTTP into Kafka
  (Cameleer; arithmetic from measured peak memory, ignores CPU and the orchestrator).
- 1.7-8.7x the throughput on the same CPUs (Cameleer vs Camel JVM); 1.9-9.9x vs Quarkus native.
- 43% less CPU per request than the server it replaced (simoom vs cameleer-http, workstation).

**Pillar 2. Starts in milliseconds, so scale to zero pays.**
- 3 ms against 1.1 s to a started context, 450x (Cameleer vs Camel JVM); 10x against Quarkus
  native's 30 ms. Always show both baselines.
- $9.17 → $0.03 per million Lambda cold starts at 512 MB (Cameleer; arithmetic, start-up measured on
  VMs, not inside Lambda).
- 128 MB instead of 512 MB Lambda setting: $0.83 → $0.21 per million 100 ms invocations
  (FinOps arithmetic; the CPU caveat must be stated).
- First of 21 HTTP servers on start-up, 0.94 ms (simoom).

**Pillar 3. Less to ship and less to trust.**
- Zero third-party dependencies in both projects (a fact, not a ratio); about 6 MB with Cameleer's
  core alone (stated on the site's step table).
- **Gap:** the ledger has no size, dependency or idle-thread row. Image size (12.5 MB core), idle
  threads (34 → 2) and binary size live in STATUS and BENCHMARKS only. Add a ledger row with a
  baseline (the Camel image, its jar count) before this pillar carries a headline ratio. Until
  then, keep the pillar factual and without ratios.

**The loss line stays in the hero.** "Where we lose: Cameleer's p99 for HTTP into Kafka is 8.6 ms
against Camel's 7.8 ms; simoom is 6% behind may-minihttp on plain HTTP." It is the strongest trust
signal on the page and costs one line.

### "You can do this too": make the method a playbook, not a philosophy

Today the page describes one idea three times (the principles, the three steps, the crossing
table), and none of them reads as a task. Merge them into **"Do the same to your most expensive
service"**: what you do, what you end with, and a link to how Cameleer did it.

| step | you do | you end with |
|---|---|---|
| 1. Pick | take the top services from the cost report | one service, ranked by spend |
| 2. Baseline | measure the eight numbers on fixed hardware, with versions written down | a baseline sheet |
| 3. Translate | turn each number into billed units (GB-s, vCPU-h, instances per node) | a dollar figure finance accepts |
| 4. Change | climb the ladder: setting → smaller default → removed layer → different runtime → rewrite | the cheapest change that meets the budget |
| 5. Prove | the same harness, same machines, both ends of every range | before/after, losses included |
| 6. Rightsize | lower requests, limits and memory settings to match | the saving on the bill (without this step the software shrinks and the bill does not) |
| 7. Guard | put the budgets in CI | regressions fail the build, not the invoice |

This merges the method's seven steps with the FinOps page's six. Steps 3 and 6 matter most to
CTOs and FinOps readers, and today's crossing table leaves out both.

Making it concrete:
- **Name the deliverable:** "a one-page crossing report". Show its eight-heading skeleton inline.
- **Name the checklist:** the water check, shown as a list.
- **Build next (gap):** a copyable report template and a generic harness. `scripts/cloud-bench`
  is Cameleer-specific. A download turns "read the method" into "start the method".
- **Use the ladder to answer the rewrite question:** a rewrite is the last rung. Say honestly
  that both projects are rewrites, because the numbers said the design was the limit.

### Objections and the line that answers each

| objection | answer line (on the page, near where it arises) |
|---|---|
| RIIR fatigue | "Not a language switch. The gain is in how software uses its language: what ships, the defaults, threads, layers and waiting. A rewrite is the last rung." |
| "Benchmarks are cherry-picked" | "The strongest baseline too (Quarkus native), both ends of every range, the losses in the same table, and the scripts and raw data to rerun it." |
| "We can't rewrite" | "You don't start there. Steps 1 to 3 change nothing; step 4 starts with a setting. If you run Camel routes, Cameleer runs the same YAML." |
| "Small project, one maintainer" | "Yes. That is why nothing depends on trusting us: Apache 2.0, rerunnable scripts, raw results. Judge the numbers, not the headcount." Name the maintainer on About. |
| "Is this a vendor pitch?" | "There is nothing to buy and no paid edition. The maintainer takes consulting work on measuring and slimming software; that is the only commercial sentence on the site." |

---

## 3. Page narrative, in order

| # | section | job | visitor question | CTA |
|---|---|---|---|---|
| 0 | Header, project shortcuts | one-click exit for product seekers | "Is the thing I searched for here?" | Cameleer · simoom (kept, also on phones) |
| 1 | **Hero**: H1, sub-headline, three before → after tiles (450x start-up, 20 → 1,024 per node, $9.17 → $0.03), the loss line | outcome in 5 seconds | "What do I get?" | **See what it saves you** (§2) · **Do it on your software** (§4) |
| 2 | **What it saves**: the three pillars, two proof lines each, plus the density and Lambda tables | turn ratios into money | CTO/FinOps: "Is this real money?" | Code efficiency as a FinOps lever |
| 3 | **Who it's for**: five doors (I pay for the machines / I run FinOps / I run Apache Camel / I want to measure my own software / I need a Rust HTTP server or MCP tools) | segment, then route | "Which part is mine?" | one link per door |
| 4 | **Do the same to your most expensive service**: the seven-step playbook, deliverable, water check | adoptability | engineer/FinOps: "Can I copy this?" | The method · get the report template |
| 5 | **Why these numbers hold up**: the fair-baseline rules (do / don't), labels, "rows are never deleted" | defuse "cherry-picked" | "Should I believe it?" | The ledger |
| 6 | **The projects, as proof**: Cameleer, simoom, karez (Cameleering), ksar (planned) | evidence that the method ships | "Has anyone done it?" | each microsite |
| 7 | **This is cameleering**: the definition (verbatim) and the eight principles as one line each | name the thing, now that it is earned | "What do I call this?" | About |
| 8 | **How it compares**: the alternatives table from §1 | position against what they would do otherwise | "Why not just tune or rightsize?" | When a rewrite pays off |
| 9 | **Why now** (shortened to three sentences): idle spend, billed cold starts, generated code that runs slower | urgency without guilt | "Why bother this year?" | the evidence pages |
| 10 | **Talk to us**: nothing for sale; consulting; Discussions | a contact path for the CTO | "Who do I call?" | GitHub Discussions |
| 11 | FAQ | search and LLM answers | leftovers | — |

Changes from today:
- Delete the separate three-step "How cameleering works". It is absorbed into §4 and §7.
- Move the definition down from the second screen.
- Drop the role doors from the hero into §3, keeping only the project shortcuts up top.
- Shrink "Why now" and the lineage table.
- Change the `<title>` from "Software Efficiency, Measured" to the outcome, for example
  "Shrink What Your Software Needs | Cameleering".

---

## 4. Words and tone

**Use:**
- fewer machines, per node, cold start, scale to zero, p99, billed
- before → after, baseline, against the original, same machines
- both ends, losses, rerun, playbook, crossing report, budget, guard, ladder
- "the code side of FinOps"
- "carry more on less" (tagline, footer and OG only)

**Avoid:**
- "measured" as the headline value
- "Rust" in any headline
- rewrite-first framing ("rewrite it in", "replace your stack")
- blazing, revolutionary, game-changing, "Camel killer", "replacement for Apache Camel"
- bloat as an insult
- "Cameleering's 54x" (numbers belong to the project)
- carbon or CO2 headline claims
- "save X%" without arithmetic
- "up to" without the lower bound
- "guaranteed"
- practice-speak above the fold (methodology, philosophy, principles)

**Guardrails:**
1. Every number carries its project, baseline and lower bound within one line.
2. Label arithmetic as arithmetic, and say what it ignores.
3. Never blame developers. The waste is in defaults and layers, not in people.
4. Every screen that shows wins also shows a loss.
5. Voice: a field engineer's logbook, written in the second person, with no exclamation marks.
6. "An independent implementation in the style of Apache Camel"; the trademark notice goes in
   the footer.
