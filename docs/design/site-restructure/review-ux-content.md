# Review of the site restructure plan: positioning, content and UX

_2026-11-13. Adversarial review of `plan.md` (draft), checked against the brand guide, the live
ledger and FinOps pages, and the research reports. Internal._

**Verdict.** The migration side (URLs, stubs, build, llms files) is solid. The root page is not yet
the practice-first page the owner asked for. It is a well-behaved portfolio page that protects
product traffic three times over and leaves FinOps fourth. Two of the hero numbers break the house
rules that the page exists to show off. The fixes are small and mostly about order and choice of
numbers.

Every number in the plan was checked against `site/cameleering/ledger/index.html` and
`site/cameleering/finops/index.html`. All of them appear in those pages. The problems are which
numbers were chosen and what baseline goes with them.

---

## Major findings

### M1. FinOps comes fourth, after three routes away from the practice

**Evidence.** `plan.md:154-157`: hero (with "Looking for Cameleer or simoom?"), then 2 Projects
("send product seekers on in one click"), then 3 Who it is for (two of three doors lead to
products), then 4 FinOps. Rule `plan.md:137` puts a product link above the fold as well.

**Why it matters.** The brief (`plan.md:5-7`) says "FinOps content on the main page" and "first
understands cameleering, then goes to Cameleer and simoom". As outlined, a visitor sees two product
exits before seeing what the practice saves. The visitors who need the fast exit are already
covered by deep links (`plan.md:32-36`) and by the hero line. Sections 2 and 3 repeat that exit.

**Change.** Use one routing device, a door strip in the hero (see the draft below). Then reorder:
1 Hero, 2 What it saves (FinOps), 3 One crossing, step by step (M4), 4 Projects as proof,
5 Why now, 6 Principles, 7 Further reading, 8 Get involved. Drop section 3 as a separate band.

### M2. The simoom hero row is not measured "against the original" and has no baseline number

**Evidence.** `plan.md:154`: "simoom: 1st of 21 HTTP servers on start-up". The ledger figure is
0.94 ms against hyper's 1.48 ms (`ledger/index.html:160`), a 0.54 ms gap shown as a rank. The plan's
own rule says "The root never shows a bare number" (`plan.md:139`), and so does "every number with its
baseline" (`plan.md:272`). The definition says "proving every gain against the original"
(`cameleering-brand.md:21-23`). simoom's original is cameleer-http, the server it replaces
(`ledger/index.html:127-138`). The research row had the right figure, "43% less CPU per request
than the server it replaced" (`research/personas-ux.md:288`), and the plan dropped it. The paired
loss, HTTP/3 against H2O, is a 4-server lane (`ledger:153`). The headline wins come from 21-server
lanes. So the page shows a win from a wide comparison and a loss from a narrow one.

**Change.** Lead with the comparison against the original: CPU per request, about 13 µs against
about 23 µs, 43% less (`ledger:134`). Give the field result in the same row, plaintext GET 2nd of
19, 6% behind may-minihttp (`ledger:147`), as the loss. The win and the loss then cover the same
kind of work, and the reader sees where simoom stands among the 20 other servers.

### M3. Cameleer's hero and FinOps rows pick the most flattering baseline and only one end of the range

**Evidence.**
- `plan.md:154` gives start-up as 3 ms against 1.1 s on the JVM only. The ledger also has Apache
  Camel on Quarkus native, 30 ms, 10x (`ledger:110`), which is the honest answer to "but native".
- `plan.md:157` and research row 4 (`personas-ux.md:290`) lead with "20 vs 1,024 integrations per
  8 GiB". That is the direct route. HTTP into Kafka gives 24 against 122 (`finops/index.html:102-103`).
  The brand guide asks for "Both ends of every range" (`cameleering-brand.md:79`).
- The FinOps page says "5 to 54 times as many integrations per node" (`finops:106`). Its own table
  gives 5.1x to 51x, and the ledger's peak-memory range is 4.8-54x (`ledger:100`). Reusing that
  sentence on the root would put three different ranges on the site.

**Change.** In the hero, show peak memory "7-67 MB vs 214-440 MB, 4.8-54x less" and start-up with
both baselines. On the root FinOps card, show both routes (20→1,024 and 24→122). Fix `finops:106`
to "5 to 51 times" before anything is copied from it.

### M4. The practice is described but never shown as something the reader can do

**Evidence.** Section 5 lists eight principles and the seven-step process as a strip
(`plan.md:158`). No section walks through a crossing. "Who it is for" (`plan.md:156`) has no door for
"I want to measure my own software". The CTA for contributors is GitHub. Also, "practice" can mean a
consultancy ("a law practice"). The plan never says whether Cameleering offers services, so a CTO
who answers "Get involved" (`plan.md:161`) won't know what they are contacting.

**Why it matters.** "First understand cameleering, then see how it is done" calls for a worked
example that connects the method to the proof. Without one, the root reads as two products with an
umbrella name over them.

**Change.**
- Add a section "One crossing, step by step": the seven steps, each with Cameleer's real instance.
  - Pick: Apache Camel.
  - Baseline: 4.23.0 on Java 21 and Quarkus native, on c-4 machines.
  - Budget: the CI gates.
  - Cut: std-only, one binary.
  - Prove: `scripts/cloud-bench`.
  - Publish: the ledger row and the Kafka p99 loss.
  - Guard: the budgets in CI.

  Link each step to its evidence. This replaces the bare principles strip.
- Add a fourth door: "You want to measure your own software → the method and the report format".
- Add owner decision D5: does Cameleering offer anything (services, reviews, nothing)? Say it in
  one line on the root and in llms.txt.

### M5. The two-level header stacks two near-identical names and takes vertical space on 279 docs pages

**Evidence.** `plan.md:107-109` stacks "[tile] Cameleering" above "[C] Cameleer". The family bar's
"Practice" link points to the root, which the tile already links to. "Method" ("How we measure"),
the page that backs the house rule, is missing from the bar. The docs keep their own sidebar under
both bars.

**Why it matters.** A first-time visitor sees two wordmarks one letter-group apart and reads it as
a duplicate or a typo. This is exactly the Cameleer/Cameleering confusion the plan lists as a risk
(`plan.md:414`). On docs pages, two bars push the content down on every page view.

**Change.** Test a single bar with a breadcrumb lockup before committing to two bars:
`[tile] Cameleering / [C] Cameleer   Docs  Use cases  MCP …   Cameleering ▾`. The "/" makes the
hierarchy explicit. The family menu holds Ledger, Method, FinOps and Projects. If two bars stay,
cap the family bar at 32 px (`personas-ux.md:204`), replace "Practice" with "Method", and set the
tile wordmark smaller than the project mark on project pages.

### M6. "Apache Camel in Rust" is a trademark risk the current site partly avoids

**Evidence.** The plan uses it in the `/cameleer/` title (`plan.md:178`), in the H1 "Apache Camel
routes, in Rust." (`plan.md:176`) and in the root anchor (`plan.md:199`). Today's H1 is deliberately
"Integration routes, in Rust instead of on the JVM" (`site/index.html:276`). The brand voice says
"an independent Rust implementation in the style of Apache Camel" and forbids replacement wording
(`cameleering-brand.md:116-118`). The research also proposes `isBasedOn` Camel in JSON-LD and
"(Apache Camel in Rust)" in 279 docs titles (`research/search-discovery.md:230, 237`).

**Why it matters.** "Apache Camel in Rust" reads as "a version of Apache Camel". ASF policy
objects to exactly that, and the plan moves the root and anchors towards more of it.

**Change.** Keep the query words but make them descriptive. Title: `Cameleer: Run Apache Camel
Routes in Rust, MCP for Agents` (57 characters). Keep today's H1 wording. Anchor: "Cameleer, Apache
Camel-compatible routes in Rust". State in the plan that `isBasedOn` is not used and that docs titles
stay `<topic> | Cameleer docs`.

### M7. The content programme promotes claims that have no ledger row, and skips the remeasure

**Evidence.**
- Item 2, the MCP benchmark (`plan.md:331`), uses figures from STATUS. The ledger has no MCP row
  (`ledger:80-85`).
- Item 7 targets "camel k alternative" (`plan.md:337`) with no Camel K baseline measured.
- The Cameleer row is 0.4.0 (`ledger:94`; `plan.md:186`), while the product is at 0.5.x and 0.6.0
  is next.
- Practice content sits at 6, 8 and 9.

**Why it matters.** "No claim without a number" really means no claim without a ledger row and a
crossing report (`cameleering-brand.md:83-84`). Pages that skip the ledger undercut the root's
promise.

**Change.**
- Add item 0: remeasure Cameleer on the current release and supersede the 0.4.0 row.
- Before item 2, add an MCP ledger row, with Camel's lower p99 as the loss.
- Drop the Camel K target until there is a Camel K baseline, or retitle the page "footprint" with no
  "alternative" query.
- Promote the FinOps calculator (6) and "a crossing, start to finish" (M4) to the top three.
- Defer item 10 (the DICOM HTML statement): it serves a few procurement visitors.

### M8. Missing personas: permanent product traffic at the root, the Camel community, people evaluating the organization

**Evidence.**
- Every published crate and image points `homepage` at the root forever (`plan.md:46-48`). Phase 5
  only changes new releases (`plan.md:387`). So crates.io and Docker Hub users land on the practice
  page for years. The plan treats them only as a ranking risk (`plan.md:414`).
- No page addresses the Apache Camel community, although A4 plans a post to it (`plan.md:407`).
- No About page exists. Journalists, analysts, partners and investors all ask the same questions:
  who maintains this, is it independent, how is it sustained, how do I contact you. The plan only
  notes "one maintainer" as a risk (`plan.md:417`). The research flags "single maintainer" as the
  security reviewer's objection (`personas-ux.md:148`).

**Change.**
- The hero door strip must sit in the first 375×667 viewport. Measure that in the phase 3 browser
  pass.
- Add `/about/`: maintainer, independence from the ASF, licence, how decisions are made, contact,
  logo files and the one-line definitions as a press kit.
- Add a short "Apache Camel and us" note on `/cameleer/`: what Cameleer owes Camel, factual
  comparisons only, how to report a semantic mismatch.

---

## Minor findings

1. **The root title breaks the plan's own rules.** `Cameleering: Software Efficiency, Measured
   Against the Original` is 63 characters and puts the brand first (`plan.md:145` against `plan.md:223`,
   "at most 60, query first, brand last"). Suggested: `Software Efficiency, Measured | Cameleering`
   (43 characters).
2. **The plan doesn't update the brand guide.**
   - The Domains section still says the root stays Cameleer's home. Its phase 2 trigger, "a second
     project has measured results", is now met (`cameleering-brand.md:67-71`).
   - The positioning puts JVM platform teams first and CTOs third (`cameleering-brand.md:44-46`).
     The root puts the CTO first.
   - The canonical footer line (`cameleering-brand.md:60`) isn't mentioned.

   Make updating the guide part of phase 3.
3. **The `/cameleering/` index is never mapped.** Section 2.1 maps `/cameleering/<x>/` only
   (`plan.md:61`). The 404 prefix map omits `cameleering` (`plan.md:321`). That page holds the
   source list, including the IEA citation (`site/cameleering/index.html:255`), which must move with
   the "why now" cards.
4. **The "why now" section.**
   - Sixth of eight is not the "prominent" placement the owner chose
     (`cameleering-brand.md:49`). Move it to 5, directly after the proof.
   - "shared power" (`plan.md:159`) reads as politics. Use the live heading "To the power we all
     share" and keep its humility line, "One project cannot change that"
     (`site/cameleering/index.html:120`).
5. **Mobile rules from the research are missing.** Carry two of them into the plan
   (`personas-ux.md:276-279`):
   - the ledger card layout under 600 px;
   - "baseline in the same card, never in a tooltip".

   The mini-ledger is a table, so it needs both.
6. **The figure 43% appears three times in simoom's row of the ledger**: 43% less CPU, H2O 43%
   ahead, a 43% larger executable. Never put two of them side by side.
7. **The calculator (item 6) can read like a quote.** Restrict its inputs to ledger rows, keep the
   "arithmetic, not a measurement" label on the output, and leave out carbon output
   (`cameleering-brand.md:52`).

---

## What the plan gets right

- No interstitials, ever, and branded queries reach the product in one click (`plan.md:131-138`).
- Numbers stay with the project that measured them, which follows the brand's grammar rule.
- The dated endorsement chip; one published loss next to each win in the hero.
- karez and ksar shown unmeasured, as the ledger rules require.
- Docs stay at `/docs/`, stubs forever, phases 3 and 4 ship together.
- The llms.txt disambiguation line and `facts.json` as the single source.
- "Arithmetic, not a measurement" labels and the Lambda caveat kept; the contact gap raised (D4).

---

## Proposed root hero (draft)

**Eyebrow:** Cameleering

**H1 (text):** Software efficiency, measured against the original.

**Visual line** (`aria-hidden`; the accessible name comes from the H1): Carry more on less.

**Definition (verbatim):** Cameleering is the practice of making software carry the same load on
less (less time, memory, size and dependencies) and proving every gain against the original, on the
same hardware, with the method published.

**House rule:** No claim without a number.

**Mini-ledger:**

| project | measured against | gain | loss we publish |
|---|---|---|---|
| **Cameleer** 0.4.0, integration engine (2026-11-06) | Apache Camel 4.23.0 on Java 21 and on Quarkus native, same DigitalOcean c-4 machines | peak memory 7-67 MB vs 214-440 MB (4.8-54x less); start-up 3 ms vs 1.1 s on the JVM, 30 ms native | p99 HTTP into Kafka 8.6 ms vs 7.8 ms |
| **simoom** 0.1.0, HTTP engine (2026-11-09 to 12) | cameleer-http, the server it replaces, same workstation; then 20 other servers on c-4 droplets | CPU per request about 13 µs vs about 23 µs (43% less) | plaintext GET 2nd of 19, 6% behind may-minihttp |

Under the table: "Two projects so far. karez and ksar are not measured yet." and the link
"Every row, its method and its raw data → the ledger".

**CTAs:** primary "Read the ledger"; secondary "How we measure".

**Door strip** (immediately below; also the product exit for crates.io and Docker Hub visitors):

- "I pay for the machines" → What code efficiency saves
- "I run Apache Camel" → Cameleer
- "I need a Rust HTTP server" → simoom
- "I build AI agents" → Cameleer as an MCP tool server
- "I want to measure my own software" → the method
