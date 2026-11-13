# Site restructure: personas, journeys and information architecture

_Research note, 2026-11-13. Input to the plan that moves Cameleering to the root of cameleer.ing
and Cameleer into a microsite at `/cameleer/`, next to `/simoom/`. Based on the pages in `site/`
as of `bde29d8`, `docs/website-plan.md` and `docs/design/brand/cameleering-brand.md`. No traffic
data was available (no Search Console export in the repository), so the personas are built from the
pages' target keywords, the comparison and alternatives pages that exist, and the brand guide's
audience order. Check them against Search Console queries once the move is planned._

## 0. What the site is today, in one paragraph

The root (`site/index.html`) is Cameleer's product home. Its title is "Cameleer: Apache Camel in
Rust, MCP tools for AI agents". Cameleering lives at `/cameleering/` and its six sub-pages. The
practice reaches visitors through a "Cameleering" dropdown in Cameleer's header and through a band
right after the hero ("The first Cameleering project: carry more on less."). simoom already has its own
microsite at `/simoom/`, with its own header (Docs, Use cases, Benchmarks, Compare, Cameleering,
GitHub). Search traffic comes almost entirely to Cameleer pages: 345 URLs in `site/sitemap.xml`,
nearly all of them Cameleer docs, compare, alternatives and use-case pages. That is the asset the
move must not break.

### What still holds in `docs/website-plan.md`, and what is stale

| item in the plan | status |
|---|---|
| Two Cameleer audiences: Camel users first, agent builders second | holds; add healthcare (Mirth) as a third |
| Keyword map per Cameleer page (section 4) | holds, with every path gaining `/cameleer/` |
| "Cameleering is running Apache Camel routes on Cameleer" (3.4) | **stale.** The brand guide redefines it as the practice ("carry the same load on less") |
| "Never cameleering in titles or H1s" (3.4, 7) | **stale for the root.** Cameleering is now a brand with its own pages; still right for Cameleer reference docs |
| MCP "planned" slots, FAQ "not yet" (3.1, 5) | **stale.** MCP shipped in 0.5.0; `/mcp/` and `/use-cases/mcp-tool-server/` exist |
| Version 0.4.0, nav "Docs · Use cases · Agents · Compare · GitHub" | **stale.** 0.6.0; the nav now has MCP, Packs, Trust, Compare and a Cameleering menu |
| GitHub Pages URL stays canonical | **stale.** cameleer.ing is canonical since 2026-11-07 |
| Brand guide "Domains": root moves when a second project is measured | trigger met (simoom measured 2026-11-12) |
| Brand guide: "Do not link github.com/cameleering until transferred" | **stale.** Transferred 2026-11-12 |

## 1. Personas

Ten personas, in order of expected traffic value. "Lands on" names today's page; the new path
adds `/cameleer/` in front of every Cameleer page.

### P1. Platform or integration engineer running Apache Camel on Kubernetes
- **Searched:** "apache camel alternative rust", "camel quarkus native memory", "camel without jvm",
  "apache camel vs …".
- **Lands on:** `/compare/apache-camel-vs-cameleer/`, `/alternatives/apache-camel-in-rust/`,
  `/alternatives/apache-camel-alternatives/`, or a component page under `/docs/components/`.
- **First 10 seconds:** "Do my YAML routes run unchanged, and how much better is it, really?"
- **Proof needed:** same-route benchmarks with versions named (Camel 4.23 on Java 21 and 4.22 on
  Quarkus native), component parity, the migration checklist, the losses.
- **Next click:** `docs/guides/migrating-from-apache-camel.html`, then the quick start.
- **Conversion:** `docker run` of a route; star; later `cameleer validate` on their own routes.
- **Objections:** maturity and bus factor; no commercial support; "is this an Apache project?";
  which Camel features differ; unaudited from-scratch TLS.
- **Practice-first effect:** neutral to negative. They must reach Cameleer content in zero clicks.

### P2. CTO or VP Engineering under cloud-cost pressure
- **Searched or arrived from:** a LinkedIn or newsletter post, "reduce kubernetes memory cost jvm",
  "lambda init phase billing", or a link forwarded by their team.
- **Lands on:** `/cameleering/finops/` or the root.
- **First 10 seconds:** "Is this real money, and is it a vendor pitch?"
- **Proof needed:** arithmetic with public list prices (the FinOps page has it), the ledger with
  baselines and losses, a named, accountable owner.
- **Next click:** the ledger, then the project that matches their stack.
- **Conversion:** forwards the page to an engineer; reads the ledger; wants to **contact** someone.
  Today there is no contact path on any page (no `mailto:`, no contact page).
- **Objections:** rewrite risk (the `when-to-rewrite` page answers it well); sustainability of a
  small open-source effort; support.
- **Practice-first effect:** strongly positive. This is the reader the new root is for.

### P3. FinOps practitioner
- **Searched:** "code efficiency finops", "workload optimization engineering", "lambda cold start
  cost", "kubernetes idle resources".
- **Lands on:** `/cameleering/finops/`.
- **First 10 seconds:** "Is there a method I can bring to engineering, with sources?"
- **Proof needed:** cited sources (State of FinOps 2026, Flexera, Datadog, CAST AI), shown
  arithmetic, stated limits (the page says "arithmetic, not a measurement" where it is).
- **Next click:** `/cameleering/method/` (what to ask engineering to measure).
- **Conversion:** cites or shares the page; uses the six-step practice table.
- **Objections:** "not my remit, I don't change code"; carbon greenwashing (the practice correctly
  makes no carbon claims).
- **Practice-first effect:** positive.

### P4. Rust developer looking for an HTTP server or crate
- **Searched:** "rust http server without tokio", "hyper alternative", "zero dependency http server
  rust", "http3 server rust".
- **Lands on:** `/simoom/`, `/simoom/compare/…`, `/simoom/use-cases/…`.
- **First 10 seconds:** "What is the API, and is it faster than hyper or axum?"
- **Proof needed:** the campaign 4 table with places and losses (already on `/simoom/`), the
  `cargo add` line, minimum Rust version, platforms, `unsafe` policy.
- **Next click:** the simoom docs or benchmarks.
- **Conversion:** `cargo add simoom`; star.
- **Objections:** young crate; no async ecosystem interop; from-scratch TLS.
- **Practice-first effect:** none needed. `/simoom/` already works as the model.

### P5. AI or agent engineer who needs MCP tools
- **Searched:** "mcp server from existing api", "camel mcp server", "expose rest api as mcp tools",
  "idempotent agent tool calls", "mcp gateway".
- **Lands on:** `/mcp/`, `/use-cases/mcp-tool-server/`, `/compare/camel-mcp-server-vs-cameleer/`,
  `/compare/enterprise-mcp-platforms/`.
- **First 10 seconds:** "Can I serve my tools over MCP in minutes, and what does it add over the
  official SDK?"
- **Proof needed:** the one-command run (`--mcp`, `--mcp-expose`), conformance 39/39 and 26/26,
  the field test with Claude (157/160), throughput vs `camel-mcp-server` and the TypeScript SDK.
- **Next click:** `docs/guides/mcp.html` and the `mcp-shop` example.
- **Conversion:** `docker run … --mcp`, wired into Claude Desktop or Cursor.
- **Objections:** "I write Python or TypeScript"; hosted connector catalogs; YAML.
- **Practice-first effect:** negative if forced. They do not care about Wirth's law.

### P6. Healthcare integration engineer (HL7, FHIR, DICOM; Mirth refugees)
- **Searched:** "mirth connect alternatives", "mirth connect license 4.6", "hl7 mllp open source",
  "dicom router open source".
- **Lands on:** `/alternatives/mirth-connect-alternatives/`, `/healthcare/`,
  `/use-cases/hl7-mllp-healthcare-integration/`, `/use-cases/dicom-imaging-gateway/`.
- **First 10 seconds:** "Licence, ACK semantics, and has anyone tested it against real systems?"
- **Proof needed:** Apache-2.0; live tests against DCMTK, Orthanc, python-hl7 and HAPI FHIR; the
  DICOM conformance statement; "not a PACS, no pixel de-identification" stated plainly.
- **Next click:** the health pack guide; the `health` image tag.
- **Conversion:** `docker pull cameleering/cameleer:health`; reads the conformance statement.
- **Objections:** no GUI like Mirth's channel editor; no support contract; no hospital references.
- **Practice-first effect:** negative if forced. The pack belongs inside `/cameleer/`.

### P7. Efficiency-minded engineer, blogger or Hacker News reader
- **Arrived from:** HN, Reddit, Lobsters, a newsletter; searches like "wirth's law", "is ai code
  slower", "when to rewrite software".
- **Lands on:** `/cameleering/wirths-law/`, `/cameleering/ai-code-performance/`,
  `/cameleering/when-to-rewrite/`, or the root.
- **First 10 seconds:** "Is this rigorous, or a 'rewrite it in Rust' pitch?"
- **Proof needed:** the method, raw data links, losses in the same table, sources with dates.
- **Next click:** the ledger, then the method.
- **Conversion:** shares, stars, cites, possibly contributes.
- **Objections:** RIIR fatigue; cherry-picked benchmarks; sermon tone.
- **Practice-first effect:** positive, if the tone stays a logbook and not a manifesto.

### P8. Coding agent acting for a developer
- **Reads:** `llms.txt`, `llms-full.txt`, the `.md` twins of docs pages, `AGENTS.md`, the README.
- **First request:** "What is the exact command, option name or YAML key?"
- **Proof needed:** stable URLs, one consistent identity sentence, `cameleer validate`.
- **Conversion:** writes a route that validates; recommends the project to the developer.
- **Risk in the move:** today `/llms.txt` starts "# Cameleer". If the root `llms.txt` becomes
  Cameleering's, it must point to `/cameleer/llms.txt` and `/simoom/llms.txt` in its first lines,
  and old docs `.md` URLs must keep resolving.

### P9. Procurement or security reviewer
- **Arrives from:** an engineer's request to approve a dependency or image.
- **Lands on:** GitHub, `/docs/SECURITY.html` (it exists under `site/docs/`), `/trust/`.
- **First 10 seconds:** "Licence, who maintains it, how are releases signed, what is in the build?"
- **Proof needed:** Apache-2.0; zero third-party crates; CycloneDX SBOMs; cosign signatures;
  security policy; and an honest line that the from-scratch crypto has **not** had an independent
  audit (`docs/STATUS.md`, todo 5).
- **Conversion:** approval. **Objection:** unaudited TLS, single maintainer.
- **Gap:** `/trust/` today means confidential computing (attestation), not supply-chain trust.
  The word is taken by a feature page, so a family-level "Security and supply chain" page is
  missing.

### P10. Contributor
- **Arrives from:** GitHub, "Found a problem?" boxes, This Week in Rust.
- **First 10 seconds:** "Where do I start, and what are the rules (std-only, tests)?"
- **Conversion:** an issue or PR. Needs CONTRIBUTING, good-first issues, and the org page
  listing all four projects with their status.

## 2. Entry points and journeys under the new structure

New top level: `/` (Cameleering), `/ledger/`, `/method/`, `/finops/` and the essays (paths
decided by the SEO plan; this note assumes they move up one level or stay under
`/cameleering/`), `/cameleer/` (microsite), `/simoom/` (microsite).

| persona | most likely landing page (new path) | next pages | conversion | practice exposure |
|---|---|---|---|---|
| P1 Camel engineer | `/cameleer/compare/apache-camel-vs-cameleer/` | migration guide → quick start → ledger row | docker run, star | one endorsement line + ledger link |
| P2 CTO | `/` or `/finops/` | ledger → `/cameleer/` or `/simoom/` → contact | forward, contact | full |
| P3 FinOps | `/finops/` | method → ledger → when-to-rewrite | share, cite | full |
| P4 Rust dev | `/simoom/…` | simoom docs → benchmarks | cargo add | footer and family bar only |
| P5 agent engineer | `/cameleer/mcp/` | MCP guide → mcp-shop → camel-mcp compare | docker run --mcp | one line |
| P6 healthcare | `/cameleer/alternatives/mirth-connect-alternatives/` | `/cameleer/healthcare/` → guide → conformance | docker pull :health | one line |
| P7 blogger | `/wirths-law/` (or old path) | ledger → method → a project | share, star | full |
| P8 agent | `/llms.txt` | `/cameleer/llms.txt` → docs `.md` | valid route | a pointer only |
| P9 reviewer | GitHub → `/cameleer/docs/SECURITY.html` | SBOM, licence, ledger | approval | ledger as evidence |
| P10 contributor | GitHub org | `/` projects list → CONTRIBUTING | PR | full, briefly |

**Where practice-first helps:** P2, P3, P7 and, indirectly, P9 (a public ledger with losses is
evidence of seriousness). The root becomes a hub that explains why both projects exist and points
to them with numbers.

**Where it gets in the way:** P1, P5 and P6 bring most of today's search traffic, and they arrive
by deep links. Rules:

1. **No interstitials.** Deep links land on the product page. Never redirect a product query to a
   manifesto.
2. **Branded queries go to Cameleer.** A search for "cameleer" or "cameleer rust" must find
   `/cameleer/` first. The root's `<title>` and first paragraph must name Cameleer and simoom with
   a link above the fold, so a visitor who typed `cameleer.ing` looking for the engine is one click
   away (a "Looking for Cameleer, the integration engine? →" link in the hero).
3. **Practice content on product pages is one line**, never a band: "Cameleer is a Cameleering
   project · measured against Apache Camel 4.23 → ledger row".
4. **The root's sitelinks** will show `/cameleer/` and `/simoom/` if the root links them
   prominently and consistently; that keeps the brand query useful.

## 3. Navigation and IA

### 3.1 Two-level header: a family bar plus a project header

Today each site embeds the family inside its own nav: Cameleer has a "Cameleering" dropdown with
The practice, The ledger and simoom; simoom has a "Cameleering" link. That works for two projects,
but it puts simoom under Cameleer's menu and does not scale to karez and ksar.

Recommendation: a **slim family bar** (about 32 px, same on every page, including docs) above each
site's own header.

```
[tile] Cameleering   Practice  Ledger  FinOps        Projects ▾   GitHub
---------------------------------------------------------------------------
[C] Cameleer   Docs  Use cases  MCP  Healthcare  Compare  Benchmarks   [Run a route]
```

- **Projects ▾** lists every project with its ledger status, in the ledger's words: "Cameleer —
  integration engine · measured", "simoom — HTTP engine · measured", "karez — database
  connectivity · design", "ksar — crypto and PKI · planned". Only measured or public projects link
  to a site; the others link to their ledger row. This doubles as the project switcher.
- The **project header** keeps each product's own mark, colour and navigation. Cameleer's
  "Cameleering" dropdown is removed; the family bar replaces it.
- On the root, there is only the family bar, slightly taller, with a primary button "Read the
  ledger".
- The family bar is plain links, no JavaScript, so it is cheap to put in the docs template
  (`scripts/build-docs.py`) and in simoom's build (another repository: agree on one HTML snippet
  and copy it, as simoom's pages are copied in today).

### 3.2 Per-microsite navigation

- **Cameleer:** Docs · Use cases · MCP · Healthcare (packs) · Compare · Benchmarks · GitHub, CTA
  "Run your first route". Fold "Trust" (attestation) into Use cases or MCP: it is a feature, and
  the word "Trust" is better used for supply chain at family level.
- **simoom:** keep as is (Docs, Use cases, Benchmarks, Compare, GitHub); its "Cameleering" link
  moves into the family bar.
- Both microsites keep a **Benchmarks** item that goes to their own detailed benchmarks, and each
  benchmark page links to its ledger row. The ledger is the summary; the microsite holds the
  detail.

### 3.3 Footer

One shared footer with four columns: **Cameleering** (practice, method, ledger, FinOps, essays),
**Projects** (each with status), **Community** (GitHub org, contributing, security, issues,
contact), **Legal** (licence, ASF disclaimer on any page that names Apache Camel). Keep the brand
guide's endorsement line on project pages: "Cameleer is a Cameleering project: carry more on less,
no claim without a number."

### 3.4 Breadcrumbs

The Cameleering pages already show "Home / Cameleering / FinOps". Make "Home" always mean the
Cameleering root and add the project level: "Cameleering / Cameleer / Compare / Apache Camel vs
Cameleer". Emit `BreadcrumbList` JSON-LD with the same path. Docs pages: "Cameleering / Cameleer /
Docs / Components / kafka".

### 3.5 Cross-links: practice ↔ projects ↔ ledger

| from | to | form |
|---|---|---|
| every principle on the practice page | the project evidence for it | "In Cameleer: …" one line each (the current "How the principles show up in it" section, extended to simoom) |
| each ledger row | the project home, its benchmarks page, its raw results | three links per row |
| each project benchmark page | its ledger row | "This result in the ledger" |
| FinOps worked examples | the ledger rows they use | already done; keep |
| when-to-rewrite, Wirth's law | the ledger and both projects as data points | one paragraph each, not a pitch |
| project home | ledger row + practice | endorsement line under the hero numbers |

### 3.6 Showing "a Cameleering project"

- A small **endorsement chip** under each project's hero: the ledger tile, "A Cameleering project ·
  measured 2026-11-06 · ledger row →". It carries the date, so it also shows freshness.
- Never merge marks (brand guide: never combine the tile with Cameleer's "C").
- karez and ksar get no microsite and no chip until the brand guide's rule is met (public repo and
  a ledger row).

### 3.7 Mobile

- The family bar collapses to the tile, "Cameleering" and a "Projects" button; the project header
  keeps its own menu button. Two menu buttons on one screen confuse, so merge them into one sheet
  on small screens: project links first (the visitor is on a project page), family links below a
  divider.
- Tables (ledger, benchmarks) scroll inside their wrapper, as today; the ledger also needs a card
  layout under about 600 px, because its "result" column is the one that matters and it is the
  last column.
- Hero stats go 1×3 or 2×2 with the baseline in the same card, never in a tooltip.

## 4. Home page outlines

### 4.1 New Cameleering root (`/`)

| # | section | must achieve | content and proof | CTA |
|---|---|---|---|---|
| 1 | Hero | Say what Cameleering is in one sentence and prove it is not talk | H1 "Carry more on less."; the verbatim definition; sub-line "No claim without a number."; a three-row mini-ledger with project names attached: **Cameleer** 3 ms vs 1.1 s start-up (Apache Camel 4.23, Java 21); **simoom** 1st of 21 HTTP servers on start-up, 43% less CPU per request than the server it replaced; **losses** "Cameleer: p99 HTTP→Kafka 8.6 vs 7.8 ms; simoom: 43% behind H2O on HTTP/3" | primary "Read the ledger"; secondary "Looking for Cameleer or simoom?" with both links |
| 2 | The projects | Route product seekers in one click | Cards for Cameleer and simoom (one line, one number, status, link); karez and ksar as smaller rows with "design" and "planned", no numbers | "Go to Cameleer", "Go to simoom" |
| 3 | Who it is for | Let P2, P3, P1 self-select | Three short columns: "You pay for the machines" (→ FinOps), "You run JVM integration" (→ Cameleer compare), "You build in Rust" (→ simoom) | one link each |
| 4 | What it saves | Turn milliseconds into money for P2 | Two FinOps examples, compressed: 20 vs 1,024 integrations per 8 GiB node (arithmetic, labelled so); $9.17 vs $0.03 per million Lambda cold starts at 512 MB (with the "measured on VMs, not in Lambda" caveat) | "Code efficiency as a FinOps lever" |
| 5 | Principles and process | The method in one screen | Eight principles as one-liners; the seven steps as a horizontal strip | "How we measure" (method) |
| 6 | Why now | Keep the responsibility argument, short | Three cards: people who pay, people who wait, shared power (IEA 945 TWh, cited); the line "about waste, not about the people who build features" | "When a rewrite pays off" |
| 7 | Further reading | Feed P7 | Wirth's law, AI code performance, when to rewrite, each with one finding | — |
| 8 | Get involved | Convert P10 and P2 | GitHub org, contributing, contact | "Contact", "Contribute" |

What to leave off the root: the component catalog, the runtime toggle, MCP details, the
benchmark table per scenario. They belong to `/cameleer/`.

### 4.2 New Cameleer microsite home (`/cameleer/`)

Keep today's `site/index.html` almost as it is. It already follows the plan's order (identity →
numbers → agents → use cases → catalog → quick start). Changes:

1. **Replace the Cameleering band** ("The first Cameleering project: carry more on less.", section 2 of
   today's page) with the endorsement chip under the hero stats. The "where it fits" cards in that
   band (edge, serverless, agents) duplicate the use-case strip; drop them.
2. **Keep the H1** device and the three hero stats; each stat already names its baseline.
3. **Add a losses line** under the throughput table: "One loss: p99 for HTTP into Kafka, 8.6 ms vs
   7.8 ms" (today only the ledger and `/cameleering/` say it on the root).
4. **Make healthcare a first-class entry** in the nav (it is under "Packs" today), because Mirth
   traffic is a distinct audience.
5. **Mention simoom** where HTTP comes up ("HTTP engine: simoom, measured →") so the two
   microsites reinforce each other.
6. Title keeps "Apache Camel in Rust" at the front; the brand goes at the end ("| Cameleer").

## 5. Discoverability and trust UX

- **Proof next to the claim.** Every number shows its baseline and date in the same element
  (today's hero stats do this well: "3 ms vs 1.1 s"). The root's mini-ledger must keep that format.
- **Losses on the first screen.** The practice's principle 8 is its strongest differentiator. Put
  at least one loss per project on the root and on each microsite home, not only on the ledger.
- **Versions and freshness.** Each ledger row and each microsite hero shows "measured YYYY-MM-DD
  with version X". Cameleer's ledger row says 0.4.0 measured on 2026-11-06 while the release is
  0.6.0; say so explicitly ("not remeasured since 0.4.0") rather than leaving the reader to notice.
  The brand guide's 12-month staleness rule should show as a visible "stale" label.
- **Attribution of numbers.** Never let a project's figure float on the root without the project
  name (brand guide: "Numbers belong to the project that measured them").
- **ASF care.** The disclaimer stays on every page that names Apache Camel, including the root if
  its mini-ledger names Camel. "Camel" never alone in a name. Prefer "in the style of Apache Camel"
  in family-level copy; keep "Apache Camel in Rust" only in Cameleer's search titles where it is
  the query.
- **No overclaiming.** The root should say "two measured projects", not "a proven method". Keep
  "design targets only" for karez in the hollow style the brand guide sets.
- **Accessibility.** Keep the `aria-label` on the struck-through H1; ledger tables need `<th scope>`
  and a text label for win/loss, not colour alone; the family bar must be reachable by keyboard and
  announced as a separate `nav` landmark with its own `aria-label` ("Cameleering"); the mobile
  sheet must trap focus and close on Escape.
- **Page speed.** The root is 62 KB of HTML and simoom's home 38 KB, with one Google Fonts request
  and shared CSS. Keep the new root under about 60 KB, reuse `assets/site.css`, avoid images in the
  hero, and do not load simoom's 210 KB `c4.json` or chart code on the root. A practice that sells
  "carry more on less" will be judged on its own page weight; publish it (a "this page: N KB" line in
  the footer is cheap and on-brand).

## 6. Risks

| risk | why it matters | mitigation |
|---|---|---|
| Cameleer vs Cameleering confusion | One letter pair apart, same domain. A visitor who typed `cameleer.ing` for the engine meets a practice page | Hero link "Looking for Cameleer, the integration engine?"; distinct marks (tile vs "C"); the canonical sentence "Cameleer is the first Cameleering project" on both homes; `llms.txt` opens with both names and the difference |
| Losing Cameleer's rankings | The root currently ranks for Cameleer's queries; 345 URLs move. GitHub Pages cannot send real 301s, only meta-refresh stubs (`write_moved` in `scripts/build-docs.py`) | Move everything in one change, not in stages (one period of ranking noise, as the plan's 3.4 advised); stubs with `rel=canonical` to the new URL; new sitemap; Search Console; do not change titles in the same window; update README, crate `homepage`, Docker Hub and `CITATION.cff` links. Consider leaving `/docs/` deep URLs in place if the SEO plan judges the loss too high |
| Brand query split | "cameleer" may now surface the practice page first | Root title names Cameleer and simoom; strong internal links to `/cameleer/` with the anchor "Cameleer" |
| Practice pages read as preachy | P7 and P1 are allergic to sermons; "Why now" and the responsibility section are long on `/cameleering/` | Lead with the ledger, not the manifesto; keep "why now" to three cards on the root and the essay below the fold; keep the logbook voice; no adjectives the numbers do not back |
| Over-claiming the practice from two projects | Both projects come from one maintainer and were measured with one harness | Say "two projects so far" and show karez and ksar as unmeasured; link the method so others can check |
| simoom built in another repo | The family bar and footer must stay identical across two builds | One shared snippet with a version comment; a check in CI that the snippet matches |
| Contact gap | P2 and P9 cannot reach anyone | Add a contact path (an address or a GitHub Discussions link) before launch |
| "Trust" used for attestation | Reviewers look for supply-chain trust | Rename the Cameleer page to "Confidential computing" or similar; add family-level security and supply-chain information |
