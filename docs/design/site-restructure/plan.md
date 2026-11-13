# cameleer.ing restructure: Cameleering at the root, Cameleer and simoom as microsites

_Plan, 2026-11-13. Status: **v2, approved 2026-11-13.** The owner accepted D1-D4 and D6 as
recommended. D5 (what Cameleering offers) is still open and is needed before phase 3. Phase 0 is
in progress. Internal (under `docs/design/`, not published)._

**Owner's brief:**
- Cameleering becomes the main site.
- The practice comes first, with the FinOps content on the main page.
- A visitor first understands cameleering, then goes to Cameleer and simoom to see how it is done.
- Cameleer's current site moves into a microsite, as simoom's already is.
- The plan covers structure, interlinking, SEO, LLM discovery and coding-agent discovery.

**How this plan was made.**
1. Four research reports, all on the site at `bde29d8`:

   | report | question |
   |---|---|
   | [research/personas-ux.md](research/personas-ux.md) | who lands, why, what they need; navigation; outlines for the home pages |
   | [research/search-discovery.md](research/search-discovery.md) | queries and search results; keeping rankings; on-page rules; missing content |
   | [research/llm-agent-discovery.md](research/llm-agent-discovery.md) | chats and coding agents: scenarios, sources, llms.txt, prompt test |
   | [research/site-inventory-migration.md](research/site-inventory-migration.md) | inventory, link graph, URL options, redirects, build changes, sequence |

2. A draft plan (v1).
3. Two adversarial reviews, each checked against the real files:
   - [review-seo-migration.md](review-seo-migration.md): 2 blockers, 7 majors, 15 minors;
   - [review-ux-content.md](review-ux-content.md): 8 majors, 7 minors.
4. This version. The change log in §14 records what each finding changed.

---

## 1. What we found

1. **There are almost no rankings to defend yet.** cameleer.ing has been the custom domain since
   2026-11-07. A sampled search found no cameleer.ing result for `cameleer`, `cameleering`,
   `simoom` or `site:cameleer.ing`. "apache camel rust" is held by kennycallado/rust-camel. The old
   address `pmsudhi.github.io/cameleer/…` now returns 404. **So move once, and soon.**
2. **Three personas bring most of the product traffic, and they must never be routed through a
   manifesto.** They are the Camel engineer, the MCP/agent engineer and the healthcare (Mirth)
   engineer, and they arrive by deep links. The practice-first root is for:
   - the CTO;
   - the FinOps practitioner;
   - the efficiency-minded reader;
   - and permanently, anyone who follows the frozen `homepage` link on crates.io or Docker Hub.
3. **Chats and coding agents find Cameleer and simoom only through live retrieval.** Both are newer
   than the models' training data.
   - ChatGPT searches mainly through Bing, Claude through Brave.
   - Crawlers rarely fetch llms.txt. People point agents at it, and doc indexers (Context7,
     GitMCP) read it.
4. **The build doesn't scale to a family site.**
   - The header is hand-copied 31 times, and the footer comes in 25 variants.
   - Nothing checks links on the designed pages, and there is no 404 page.
   - The sitemap is built from a hard-coded list.
   - `site/llms.txt` is both the source and the output.
   - **No site check runs before a merge.** `build-docs.py` runs only in `pages.yml`, after the
     merge.
5. **Some URLs and assets are frozen forever.** Published crates keep `homepage =
   https://cameleer.ing/` and `documentation = https://cameleer.ing/docs/`. Published images keep
   `org.opencontainers.image.url = https://cameleer.ing/`. These never move:
   - the identifiers `https://cameleer.ing/sbom/` and `ing.cameleer`;
   - the IndexNow key file, `/og.png`, `/favicon-*.png`, `/apple-touch-icon.png`, `/brand/` and
     `/assets/`. simoom's copy and the docs load them.

---

## 2. Decisions

### 2.1 URL structure

| path | serves | change |
|---|---|---|
| `/` | **Cameleering**: the practice home | new content |
| `/ledger/`, `/method/`, `/finops/`, `/when-to-rewrite/`, `/wirths-law/`, `/ai-code-performance/` | practice pages | moved up from `/cameleering/<x>/`. `/cameleering/` itself becomes a stub to `/`. Its source list (IEA and others) moves with the "why now" text |
| `/projects/` | every project with its ledger status | new |
| `/about/` | maintainer, independence from the ASF, licence, how decisions are made, contact, press kit (marks, one-line definitions) | new |
| `/security/` | family supply chain: Apache-2.0, std-only, SBOMs, signing, private vulnerability reporting (`SECURITY.md`), and a plain statement that the crypto has had no independent audit | new |
| `/cameleer/` | **Cameleer microsite** home (today's `/`, revised) | moved |
| `/cameleer/compare/…`, `/cameleer/alternatives/…`, `/cameleer/use-cases/…`, `/cameleer/mcp/`, `/cameleer/healthcare/`, `/cameleer/confidential-computing/` (today `/trust/`) | Cameleer's 22 section pages | moved |
| `/docs/` | **Cameleer's docs**, same URLs; branded "Cameleer docs" in the header | kept |
| `/simoom/` | simoom microsite (built in the simoom repository) | kept; its family links change |
| `/llms.txt`, `/cameleer/llms.txt`, `/cameleer/llms/*.txt`, `/llms-full.txt` | see §7 | rewritten or split |
| `/sitemap.xml` | index → `/sitemap-root.xml` (practice pages **and `/docs/`**), `/cameleer/sitemap.xml` (only `/cameleer/` URLs), `/simoom/sitemap.xml` | split |
| `/404.html` | branded links to `/`, `/cameleer/`, `/docs/`, `/simoom/`; no prefix map (every old URL has a stub) | new |

**Count:** 30 URLs change. That is the root swap, the 22 section pages and 7 `/cameleering/`
pages, giving **29 stubs**.

**Reserved-slug rule:** projects take their own name (`/cameleer/`, `/simoom/`, later `/karez/`,
`/ksar/`). The practice takes common nouns (`/ledger/`, `/method/`, `/about/`, …).

**D1, docs stay at `/docs/`.**
- 30 URLs change instead of about 578.
- Every published crate's `documentation` field points at `/docs/`.
- The 275 `.md` twins can't redirect.
- No other project competes for `/docs/`: simoom's docs are at `/simoom/docs/`.
- Moving them later stays a costed, reversible option. Both reviews agree.

**D2, practice pages move up.** Their authority is near zero, and the `/cameleering/` prefix is
redundant under a Cameleering root. The SEO review agrees.

**D3, `/trust/` is renamed.** A security reviewer looks for "trust", and today the page means
attestation. The word moves to the family `/security/` page.

### 2.2 Navigation: one bar with a lockup

v1 proposed two stacked bars. The UX review showed the problem: they put "Cameleering" above
"Cameleer", two names one letter-group apart. **v2 uses one bar per page with a breadcrumb
lockup:**

```
[tile] Cameleering / [C] Cameleer   Docs  Use cases  MCP  Healthcare  Compare  Benchmarks   Cameleering ▾
```

- **The "/" shows the hierarchy.** The project mark is the larger of the two.
- **The `Cameleering ▾` menu** holds Ledger, Method, FinOps, Projects (each project with its ledger
  status: Cameleer · measured, simoom · measured, karez · design, ksar · planned) and About.
- **On the root**, the bar is `[tile] Cameleering   Ledger  Method  FinOps  Projects ▾  About
  GitHub`, with the button "Read the ledger".
- **One shared partial.** The lockup's family part is one partial. simoom's build copies it with a
  version comment.
- **Mobile:** one menu sheet, with the project's links first and the family links below a divider.
  It traps focus and closes on Escape.
- **Accessibility:** the bar is one `nav` landmark.
- **Fallback:** if the lockup fails the phase 3 browser pass at 375 px, fall back to two bars, with
  the family bar capped at 32 px and "Method" in place of "Practice".

### 2.3 Rules that protect product traffic

1. **No interstitials.** A deep link lands on the product page.
2. **On a project page, the practice is one dated chip, never a band.** For example: "A Cameleering
   project · measured 2026-11-06 against Apache Camel 4.23 · ledger row →".
3. **Branded queries reach the product in one click.**
   - The `/cameleer/` title starts with "Cameleer".
   - The root hero has a door strip inside the first 375×667 viewport, measured in the browser pass.
   - The root JSON-LD links each project to the `#org` `Organization`.
4. **Every number travels with its baseline and its project.** Never two "43%" figures side by
   side. simoom has three different 43% figures.
5. **ASF care.** Cameleer is described as "Apache Camel-compatible" or as running "Apache Camel
   routes", never "Apache Camel in Rust" as a title or H1. Other rules:
   - no `isBasedOn` Camel in the JSON-LD;
   - docs titles stay `<topic> | Cameleer docs`;
   - the disclaimer goes on every page that names Apache Camel.

### 2.4 How results look in search

Google shows **one site name and one favicon per host**. From the cut-over, every `/cameleer/`,
`/docs/` and `/simoom/` result shows "Cameleering" and its tile. That is accepted, and it fits the
endorsed-brand model. How it is handled:

- **Site name:** `WebSite` `name` "Cameleering", with `alternateName` "cameleer.ing".
- **Root icons:** the root links the existing `brand/cameleering/` icons and adds a `/favicon.ico`.
- **Shared files:** `/favicon-*.png` and `/og.png` are **never overwritten**, because simoom and the
  docs use them.

---

## 3. The new root (`/`)

**Title:** `Software Efficiency, Measured | Cameleering` (43 characters, query first).
**H1:** "Software efficiency, measured against the original." "Carry more on less." is a visual line
with `aria-hidden`. **JSON-LD:**
- `Organization` `#org`: slogan, a PNG logo of 112 px or more (`brand/cameleering/…-512.png`),
  `sameAs` the GitHub org, Docker Hub and crates.io.
- `WebSite`.
- `ItemList` of the projects.
- `DefinedTerm` in a `DefinedTermSet` with the current definition.
- Only one `#org` node across the site.

**Weight:** under 60 KB, no hero image, and a footer line giving the page size.

### 3.1 Hero (draft)

- **Eyebrow:** Cameleering
- **H1:** Software efficiency, measured against the original.
- **Definition (verbatim from the brand guide):** Cameleering is the practice of making software
  carry the same load on less (less time, memory, size and dependencies) and proving every gain
  against the original, on the same hardware, with the method published.
- **House rule:** No claim without a number.
- **Mini-ledger:**

  | project | measured against | gain | loss we publish |
  |---|---|---|---|
  | **Cameleer** 0.4.0, integration engine (2026-11-06) | Apache Camel 4.23.0 on Java 21, and on Quarkus native; same DigitalOcean c-4 machines | peak memory 7-67 MB vs 214-440 MB (4.8-54x less); start-up 3 ms vs 1.1 s on the JVM and 30 ms native | p99 HTTP into Kafka 8.6 ms vs 7.8 ms |
  | **simoom** 0.1.0, HTTP engine (2026-11-09 to 12) | cameleer-http, the server it replaces (same workstation); then 20 other servers on c-4 droplets | CPU per request about 13 µs vs about 23 µs (43% less) | plain HTTP 2nd of 19, 6% behind may-minihttp |

  Under the table: "Two projects so far. karez and ksar are not measured yet." and "Every row, its
  method and its raw data → the ledger".
- **CTAs:** "Read the ledger" (primary); "How we measure".
- **Door strip,** directly below and inside the first phone screen. It is also the way out for
  visitors from crates.io and Docker Hub:
  - "I pay for the machines" → what code efficiency saves;
  - "I run Apache Camel" → Cameleer;
  - "I need a Rust HTTP server" → simoom;
  - "I build AI agents" → Cameleer as an MCP tool server;
  - "I want to measure my own software" → the method.

The mini-ledger has a card layout under 600 px. The baseline sits in the same card, never in a
tooltip.

### 3.2 Sections, in order

| # | section | what it must do | content |
|---|---|---|---|
| 1 | Hero | above | above |
| 2 | **What it saves** (FinOps) | turn milliseconds into money, right after the proof | **Integrations per 8 GiB node:** both routes, direct 20 → 1,024 and HTTP into Kafka 24 → 122, labelled "arithmetic, not a measurement". **Lambda cold starts:** cost per million at 512 MB, with the "measured on VMs, not in Lambda" caveat. CTA "Code efficiency as a FinOps lever". **Fix first:** the FinOps page's "5 to 54 times" becomes "5 to 51 times", to match its own table |
| 3 | **One crossing, step by step** | show the practice being done, not only described | the seven steps, each with Cameleer's real instance and a link to its evidence:<br>• Pick: Apache Camel<br>• Baseline: 4.23.0 on Java 21 and Quarkus native, c-4<br>• Budget: the CI gates<br>• Cut: std-only, one binary<br>• Prove: `scripts/cloud-bench`<br>• Publish: the ledger row and the Kafka loss<br>• Guard: budgets in CI<br>This replaces a bare strip of principles |
| 4 | **Projects as proof** | the product cards, after the reader knows why they exist | Cameleer and simoom: one line, one number with its baseline, status, link. karez and ksar as small rows: "design", "planned", no numbers |
| 5 | **Why now** (kept prominent, as the owner chose) | the responsibility argument | three cards in the live page's wording:<br>• to the people who pay;<br>• to the people who wait;<br>• to the power we all share (IEA, cited).<br>Its humility line stays ("One project cannot change that"), as does "the thing to fight is waste, not the people who build features" |
| 6 | Principles | the eight principles as one-liners | link to `/method/` |
| 7 | Further reading | serve the essay reader | Wirth's law, AI code performance, when to rewrite, each with one finding |
| 8 | Get involved | contacts and contributors | GitHub org, contributing, contact (D4), what Cameleering offers (D5) in one line |

Old home anchors (`#start`, `#bench`, `#numbers`, `#agents`, `#uses`, `#yaml`, `#runner`, `#same`,
`#cameleering`) are forwarded to `/cameleer/#…` by a three-line script. The new root doesn't reuse
those ids.

## 4. The Cameleer microsite (`/cameleer/`)

Keep today's home almost as it is.

- **Title:** `Cameleer: Run Apache Camel Routes in Rust, MCP for Agents` (57 characters).
- **H1:** keep today's wording ("Integration routes, in Rust instead of on the JVM"). Keep the
  struck-through words out of the heading's accessible text, which today's `aria-label` already
  provides.
- **Endorsement chip:** it replaces the Cameleering band. Drop the "where it fits" cards, which
  duplicate the use cases.
- **Losses line:** add one under the throughput table.
- **Navigation:** Healthcare goes into the top nav.
- **simoom:** link it where HTTP comes up.
- **Facts box:** version, date, licence, install lines, image, crate, repo, platforms, and "not
  affiliated with the ASF".
- **JSON-LD:** `publisher` and `maintainer` → `#org`; keep `alternateName` "rust-camel".
- **"Apache Camel and us" note:** what Cameleer owes Camel, factual comparisons only, how to report
  a semantic mismatch.
- **The ledger's Cameleer row is labelled "measured on 0.4.0".** It gets remeasured on the current
  release (§9, item 0).

simoom's repository changes its family part of the bar, adds the chip, changes the "Cameleer" links
from `../` to `../cameleer/` and the `/cameleering/` links to the new practice paths, and adds
`BreadcrumbList` and the `#org` link (§11, phase 3 prerequisite).

---

## 5. Interlinking

The model is hub and spoke, with a proof loop.

| from | to | how |
|---|---|---|
| root | each microsite home, plus that project's strongest proof page | descriptive anchors such as "Cameleer, Apache Camel-compatible routes in Rust" and "Rust HTTP server benchmark" |
| each practice essay | at least one ledger row | inline, with the number |
| each ledger row | the project home, its benchmark page and its raw results | three links per row |
| each project benchmark or compare page | its ledger row ("where we lost") and `/method/` ("how we measured") | one line each |
| the "one crossing" steps on the root | the evidence for each step | one link per step |
| microsite ↔ microsite | only where the topics overlap | simoom `integration-engine` ↔ Cameleer; `serverless-aws-lambda` ↔ simoom `serverless-cold-start` |
| docs footer (279 pages) | Cameleer's money pages | "Use cases · Compare · Benchmarks · MCP · Healthcare", plus one "Cameleering" link |

**Anchor rules:**
- Anchors contain the target's query.
- Brand anchors go only to the microsite homes.
- No exact-match anchor is used for two different targets.
- No "read more".

**Weak pages to fix:**
- FinOps, when-to-rewrite and the Mirth alternatives page have two inbound links each.
- Wirth's law, AI code performance and `apache-camel-in-rust` have one each.
- A build check requires every sitemap URL to have at least one inbound link outside the
  navigation.

## 6. SEO rules for every page

The rules for each page type are in search §3.
- **Title:** at most 60 characters, query first, brand last.
- **Description:** at most 158 characters, with one number.
- **Headings:** one H1.
- **Canonical:** absolute, with a trailing slash.
- **JSON-LD:** one `@graph`, with stable `@id`s. A `BreadcrumbList` everywhere except the root.
- **No `aggregateRating`, ever.**

**Fixes found:**
- 8 titles are over 65 characters, and 20 descriptions are over 165.
- Meta keywords appear on the root only. Remove them.
- The alternatives pages have no `ItemList`.
- The `Organization` without an `@id` on `/cameleering/` gets replaced by `#org`.
- Every Cameleer page shares one OG image. Make one card per page type, and number cards for the
  compare, benchmark and ledger pages, as new files that don't overwrite `/og.png`.
- The old definition ("running Camel routes on Cameleer") is still present in places. Remove it.

**`lastmod` comes from the content, not the file.** With partials, every header tweak would bump
all 30 dates. So `write_sitemap` dates each page from its content outside the partial regions.

## 7. LLM and coding-agent discovery

1. **`/llms.txt` is the family index, 8-15 KB.**
   - The definition comes first, then the disambiguation line: Cameleering is the practice and the
     organization. Cameleer is a product, and it is neither Apache Camel nor kennycallado/rust-camel.
     It was called rust-camel up to v0.1.0.
   - Then one block per project: version, install line, link to the project's index. **The Cameleer
     block sits in the first 15 lines** for at least a year.
   - Then the practice pages, a line on what Cameleering offers (D5), and the rules for quoting
     numbers.
2. **Cameleer files:**
   - `/cameleer/llms.txt` is 15-25 KB and starts with a "when to recommend / when not" section.
   - Section bundles go under `/cameleer/llms/`: guides, components by category, languages, data
     formats, MCP and health. Each is at most 150 KB.
   - `/llms-full.txt` stays at its URL. Its first line says it covers Cameleer's docs only, and
     `/llms.txt` says the same.
   - A `.md` twin exists only where one source generates both forms: docs, llms bundles, and
     designed pages only if they move to a source format. **No hand-made duplicates.**
3. **`facts.json` per project** (`/cameleer/facts.json`, `/simoom/facts.json`).
   - It holds the version, date, install lines, image tags, schema URL, llms URLs and the headline
     numbers with their baseline URLs.
   - The build generates the facts box, the llms.txt project blocks and the JSON-LD from it.
   - It is a source for our own generator first. Whether search engines read it is unproven.
4. **Quotable facts.** These go everywhere: site, README, crates.io, Docker Hub and JSON-LD.
   - the same one-sentence definition;
   - exact versions and dates;
   - install lines in code, plus the wrong guesses an agent might make (checked before writing);
   - each number with its baseline and a loss;
   - one fact per bullet.
5. **For coding agents:**
   - **A first-screen example:** ten lines that work, on each home, the README and the docs.rs root.
   - **A hosted, versioned YAML schema** at `/cameleer/schema/0.6/camel-yaml.schema.json`, plus a
     `latest` alias. Register it in SchemaStore for `*.camel.yaml`, and put a `$schema` line in
     every example.
   - **"Did you mean" hints and doc URLs in `cameleer validate`.** This is a code change for the
     code session.
   - **Pasteable agent-rules blocks** at `/cameleer/agents.md` and `/simoom/agents.md`.
     `cameleer init` writes one when asked.
   - **Context7:** refresh it, and add an entry for simoom.
6. **Indexing:**
   - Verify Search Console and Bing first (§11, phase 0).
   - IndexNow reads the child sitemaps and submits only the URLs whose `lastmod` changed, plus the
     old URLs once at the cut-over.
   - Brave has no way to submit URLs, so rely on links and crawling.
   - `robots.txt` stays `User-agent: *` / `Allow: /`. Named groups for AI bots would be redundant,
     and they would override `*` later.

## 8. Build changes (`scripts/build-docs.py`, std-only Python)

1. **The site build runs before merge** (phase 0). The `ubuntu` job in `ci.yml` gets three new
   steps:
   - `python3 scripts/build-docs.py`;
   - the checks in item 6;
   - `git diff --exit-code site/`, which fails if the committed site differs from the built one.

   An existing required check then covers it, so the ruleset doesn't change.
2. **Partials by markers.** The header and footer sit between `<!-- partial:NAME -->` markers. The
   build rewrites the text between them. The family-bar CSS gets its own file (`assets/family.css`),
   so `site.css`, which simoom loads, doesn't change.
3. **One redirect table, `scripts/redirects.json`.** It lives outside `site/`, so it is never
   published, and today's `MOVED` entries fold into it.
   - **What a stub holds:** an absolute canonical, the JS `location.replace` (keeps the query and
     the `#fragment`), a 0-second meta refresh and a visible link. **It has no `noindex`**, because
     that conflicts with the canonical, and Search Console would report "Excluded by noindex"
     instead of "Page with redirect". Retarget the 5 existing docs stubs to match.
   - **When the build fails:** a real page sits at an old path, a target is missing, or a chain
     forms.
   - **Lifetime:** stubs are kept forever.
4. **`designed_pages()` walks `site/`.** It skips `docs/`, `simoom/`, `assets/`, `brand/`, the
   IndexNow key and the icons.
5. **The sitemap index and child sitemaps**, scoped as in §2.1.
6. **Checks that fail the build** (CI and `pages.yml`):
   - links and `#fragments` across `site/`;
   - no internal link to a stub;
   - JSON-LD parses, has `@context`, and only one `#org` exists;
   - the canonical matches the path;
   - one H1;
   - warnings for over-long titles and descriptions;
   - the sitemap matches the pages;
   - every sitemap URL has an inbound link outside the navigation;
   - the simoom snippet version (from phase 3, once simoom carries the snippet).
7. **llms from sources** (`scripts/llms/…`), so the build is idempotent. The root copy is written by
   the build. Add the source paths to the `pages.yml` `paths` filter.
8. **The Rust known-URL test** in `docs_harness.rs`: every `https://cameleer.ing/<path>` in
   `docs/**/*.md` (except `docs/release-notes/**`), `README.md`, `docker/README.dockerhub.md` and
   `AGENTS.md` must be a current URL. `CHANGELOG.md` is excluded too.

## 9. Content programme, after the move

Each item is its own PR. **No page publishes a claim without a ledger row behind it.**

| # | page | target | proof |
|---|---|---|---|
| 0 | **Remeasure Cameleer on the current release** (`scripts/cloud-bench`, droplets deleted after) and supersede the 0.4.0 ledger row | every Cameleer claim | needs the owner's go (it spends DigitalOcean money) |
| 1 | **Code efficiency cost calculator** (`/finops/calculator/`) | "reduce cloud cost by optimizing code" | inputs only from ledger rows; output labelled "arithmetic, not a measurement"; no carbon output |
| 2 | **A crossing, start to finish** (long form of root §3) | "how to benchmark a rewrite", "software efficiency engineering" | the Cameleer crossing with its scripts and raw data |
| 3 | **Apache Camel performance and memory** (`/cameleer/benchmarks/`) | "apache camel memory usage", "camel startup time", "camel quarkus native vs jvm" | the ledger row; the search found no independent, current benchmark |
| 4 | **MCP ledger row, then an MCP benchmark page** | "mcp server benchmark" | 39,922 vs 22,604 vs 2,257 calls/s; Camel's lower p99 as the loss |
| 5 | Rust HTTP server benchmark: title and summary table (simoom repo) | "rust http server benchmark" | campaign 4 |
| 6 | Mirth Connect alternatives, refreshed | "mirth connect alternatives 2026" | the 10/10 live health tier |
| 7 | Apache Camel alternatives, refreshed (`ItemList`, a 2026 date) | "apache camel alternatives" | — |
| 8 | Wirth's law with an original chart | "wirth's law" | the ledger as a dataset |
| 9 | When-to-rewrite payback worksheet | "should we rewrite in rust" | the Cameleer example |
| 10 | Kubernetes **footprint** page, with no "alternative" claim until a Camel K baseline exists | "small integration runtime kubernetes" | 20.4 MB image, 2.6 MiB running route |

Not yet:
- the DICOM conformance statement as HTML (few visitors);
- SAP CPI, karez and ksar pages;
- an example of the SCI carbon metric without an energy measurement;
- an MCP-gateway listicle.

## 10. Measurement

**Before the move** (snapshot in `research/baseline-YYYY-MM-DD/`):
- Search Console and Bing, verified now (phase 0), so data accumulates before the cut-over.
- The sitemap and the redirect map.
- The lost `pmsudhi.github.io/cameleer/…` URLs.
- Manual search checks for the brand and the top 20 queries.
- The **25-prompt set** (agents §6), run three times each against:
  - ChatGPT, Claude, Gemini and Perplexity, with search on and off;
  - two coding agents.
- crates.io downloads, Docker Hub pulls and GitHub traffic, which needs a new `TRAFFIC_TOKEN`
  because GitHub keeps only 14 days.

Don't hold the move for data. The baseline is mostly searches and prompts.

**After the move:**
- weekly for 8 weeks, then monthly: stubs reported as "Page with redirect", brand position,
  impressions per cluster, referrers from chat assistants;
- the prompt set every month. Target: no regression on the Cameleer prompts, and first mentions on
  the practice prompts.

## 11. Sequence

`main` is protected (a PR plus three checks). **Phases 0 and 1 merge on their own, because they
change no URLs. Phases 2, 3 and 4 are stacked commits on one branch, reviewed commit by commit and
merged as one PR,** so `noindex` duplicates never reach production and the site is never half moved.
While that branch is open, edits to the 22 moving pages are frozen.

**After every phase:**
- the build and its checks;
- `cargo test -p cameleer-main --test docs_harness --test docs_reference --test docs_guides --test
  features`;
- a browser pass at 1280 and 375 px on the root, a `/cameleer/` page, a docs page and a `/simoom/`
  page;
- after deploy: `curl` the new URLs, fetch five stubs, and check that `#fragment` survives in a real
  browser.

| phase | what | URLs | done when |
|---|---|---|---|
| 0 | **Guards and baseline:** Search Console and Bing verification (owner, A1); the site build and checks in `ci.yml`; link, JSON-LD and redirect checks; `404.html`; the known-URL test against today's URLs; the baseline snapshot | none | CI green on today's site |
| 1 | **Partials:** markers on all 30 designed pages and the docs template | none | the rendered output is identical (normalized diff) |
| 2 | **`/cameleer/` built** (a commit on the move branch): the home and 22 pages copied, with depth prefixes, canonicals, OG and JSON-LD fixed | on the branch | link check passes |
| 3 | **New root and practice** (a commit on the move branch):<br>• the new `/`, the practice pages at the top level, `/projects/`, `/about/`, `/security/`<br>• the lockup bar, `facts.json`, the llms split<br>• the brand guide updated: Domains is now done; positioning order; footer line<br>• **every in-repo link rewritten:** docs markdown, `README.md`, the Docker Hub README source, `AGENTS.md`, the org profile source, `CITATION.cff`, OCI labels, `Cargo.toml` `homepage` for the next release<br>• **the simoom repository** builds against the new paths, and that copy lands here | on the branch | browser pass; door strip inside 375×667; all checks green |
| 4 | **Cut-over** (the last commit, then merge): stubs for the 29 old URLs; sitemap index; docs header switched; IndexNow with the old URLs once | 30 changed | live checks pass |
| 5 | **Outward-facing, 1-2 weeks after the cut-over:**<br>• Docker Hub description sync<br>• the live `Cameleering/.github` profile<br>• Context7<br>• `pmsudhi.github.io` stubs (owner, A6)<br>• third-party posts (owner, A4) | none | each confirmed live |
| 6 | **Content programme** (§9) and the monthly measurement | additive | per page |

**Phase 0 as built (2026-11-13).**

**Built:**
- `scripts/check-site.py`, which fails the build on any of these:
  - a link, anchor or absolute site URL that doesn't resolve;
  - a link through a stub;
  - JSON-LD that doesn't parse or has no `@context`;
  - a designed page whose canonical isn't its own URL, or that doesn't have exactly one H1;
  - a sitemap entry that is missing or a stub;
  - a redirect table entry that is wrong.

  It warns on long titles and descriptions: 29 on today's site.
- `scripts/redirects.txt`, which replaces `MOVED` in `build-docs.py`. The stubs carry no `noindex`,
  and the JS line keeps the fragment.
- `site/404.html`.
- The Rust test `absolute_site_links_point_at_current_urls`.
- The site build and check in the `ubuntu` CI job and in `pages.yml`.
- The baseline in `research/baseline-2026-11-13/`.

**Checks:** each guard was proved by planting faults:
- five planted faults for the site check, all caught;
- a missing target, a chain and a stub over a real page for the table, all refused;
- a moved and a missing URL for the Rust test, both caught.

**Deferred to phase 1:** `git diff --exit-code site/`. It matters once partials exist. Today the
committed `site/sitemap.xml` takes its dates from git history, so a shallow CI checkout would differ
from it for no real reason.

**Phase 4 reminder:** update the 404 page's ledger link to `/ledger/`.

**Rollback:**
- Before merging the cut-over, record the last good commit and open the revert branch.
- No other merges on cut-over day.
- A revert takes about 1-2 hours (PR plus checks). It restores the old pages, and the stubs go with
  it.
- A revert can't undo phase 5. That is why phase 5 waits.

## 12. Owner decisions and actions

| # | decision or action | recommendation |
|---|---|---|
| D1 | Docs stay at `/docs/` | yes (§2.1) |
| D2 | Practice pages at the top level (`/ledger/`, …) | yes |
| D3 | `/trust/` → `/cameleer/confidential-computing/`; a family `/security/` page | yes |
| D4 | The contact path | GitHub Discussions on the org, with security reports through the private vulnerability reporting that `SECURITY.md` already names. Add an address if you want one |
| D5 | What Cameleering offers: services, reviews, or nothing beyond open source? It goes in one line on the root and in llms.txt | your call; "practice" can read as a consultancy, so the answer has to be stated |
| D6 | One bar with a lockup (fallback: two bars) | the lockup |
| A1 | Search Console (DNS) and Bing verification, **first** | needs your accounts |
| A2 | A new `TRAFFIC_TOKEN` before the baseline | needs you |
| A3 | Check and maybe reserve `cameleer` and `cameleer-cli` on crates.io | your call |
| A4 | Third-party posts (Show HN for the simoom benchmark, r/rust, the Camel community), after the move | your call |
| A5 | Approve each PR's push and merge | as usual |
| A6 | A `pmsudhi/pmsudhi.github.io` repository with stubs for the old `/cameleer/…` URLs | recommended; only you can create it |
| A7 | Remeasuring Cameleer on DigitalOcean (§9, item 0) | recommended before the Camel benchmark page |

## 13. Risks

| risk | mitigation |
|---|---|
| "cameleer" searches land on the practice page | the `/cameleer/` title starts with "Cameleer"; the door strip is on the first screen; the `#org` entity graph; consistent nav labels |
| Every result shows "Cameleering" as the site name | accepted and stated (§2.4); fits the endorsed brands |
| The thin authority there is gets lost | one move, now; stubs forever, without `noindex`; scoped sitemaps; IndexNow with the old URLs once; Search Console verified first |
| The cut-over fails its own checks | every in-repo rewrite and the simoom copy land in the same PR; release notes and the CHANGELOG are excluded from the known-URL test |
| A broken site merges | the site build and checks run in `ci.yml` before merge |
| The root reads as a portfolio rather than a practice | FinOps comes second and "one crossing" third; the projects follow as proof |
| The root reads as a sermon | it leads with the ledger; logbook voice; "why now" in three cards with its humility line |
| Over-claiming from two projects, one maintainer, one harness | "two projects so far"; karez and ksar unmeasured; `/about/` says who maintains it; the method is published |
| The ASF trademark | "Apache Camel-compatible" wording; no `isBasedOn`; the disclaimer on every page that names Camel |
| The two repositories drift | one snippet with a version comment; a CI check on the copy |
| Rollback is slow | the revert branch is opened ahead; phase 5 waits 1-2 weeks |

## 14. Change log from the reviews

| finding | change in v2 |
|---|---|
| SEO B1: cut-over fails its own gates | in-repo rewrites and the simoom copy moved into the cut-over PR (phase 3); release notes and CHANGELOG excluded from the test |
| SEO B2: no site check before merge | site build, checks and `git diff --exit-code site/` in `ci.yml` (phase 0) |
| SEO M1: `noindex` on stubs | removed |
| SEO M2: phase 2 deploys `noindex` copies | phases 2-4 are one branch, one PR; content freeze |
| SEO M3: sitemap scope, IndexNow | docs in the root child sitemap; IndexNow expands children, changed URLs only |
| SEO M4: one site name and favicon per host | stated in §2.4; shared icons never overwritten |
| SEO M5: slow rollback | revert branch ahead; phase 5 waits 1-2 weeks |
| SEO M6: `pmsudhi.github.io` dead | owner action A6; recorded in the baseline |
| SEO M7: verification first | A1 is the first step of phase 0 |
| SEO minors | counts fixed (30 URLs, 29 stubs); H1 claim corrected; source paths in the `pages.yml` filter; 404 prefix map dropped; `lastmod` from content; no hand-made `.md` twins; `llms-full.txt` scope stated; robots `*` only; Brave not "submitted"; JSON-LD details; table in `scripts/`; `family.css`; never-moving files named; D4 points to `SECURITY.md`; reserved-slug rule |
| UX M1: FinOps fourth | FinOps second, "one crossing" third, projects fourth; one door strip in the hero |
| UX M2: simoom row has no baseline | CPU per request against cameleer-http as the gain; 2nd of 19 on plain HTTP as the loss |
| UX M3: flattering baselines | both Camel baselines, the peak-memory range, both FinOps routes; FinOps "5 to 54" fixed to "5 to 51" |
| UX M4: practice not shown | "one crossing, step by step"; fifth door; D5 |
| UX M5: two stacked names | one bar with a lockup (D6), two bars as the fallback |
| UX M6: "Apache Camel in Rust" | "Apache Camel-compatible" wording; title rewritten; no `isBasedOn`; docs titles unchanged |
| UX M7: claims without ledger rows | item 0 remeasure; MCP ledger row before the MCP page; the Camel K claim dropped; practice items promoted |
| UX M8: missing personas | door strip for crates.io/Docker Hub visitors; `/about/`; "Apache Camel and us" |
| UX minors | title 43 characters; brand guide updated in phase 3; `/cameleering/` index mapped; "why now" moved up and worded as on the live page; mobile ledger cards; no two 43% side by side; calculator guardrails |
