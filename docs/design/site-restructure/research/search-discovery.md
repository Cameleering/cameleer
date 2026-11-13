# Search discovery for the Cameleering restructure

_Research note, 2026-11-13. Input to the site restructure: Cameleering becomes the root site,
Cameleer moves to a microsite like `/simoom/`. Read-only research; nothing on the site was
changed._

**How the SERP checks were done.** Web searches were run on 2026-11-13 with a US-only search
API whose index is not Google's. Results differ by location, by engine and by
personalisation, so treat the SERP notes as a sample. They are not rankings. No search included
the owner's name. Repeat the brand checks in Search Console and in Google and Bing incognito
before the move (section 6).

## 0. The finding that shapes everything else

**The site has almost no search presence yet, so the move is cheap now and gets more
expensive every week.**

- `cameleer`, `cameleering`, `simoom`, `site:cameleer.ing` and `cameleer rust apache camel MCP`
  returned **no cameleer.ing result**. The engine "corrected" the brand to *cameleon* (a Rust
  GenICam crate), *chameleonizing* and *simbol-server*.
- `apache camel rust` is owned by **kennycallado/rust-camel** (`camel-core`, `camel-cli`,
  `camel-api` on docs.rs). The engine called it "the closest option" and said it found no MCP
  in it.
- The custom domain went live on 2026-11-07, six days ago. Most sitemap `lastmod`s are
  2026-11-12. Whatever authority exists still sits mostly on GitHub (`cameleering/cameleer`),
  crates.io, Docker Hub and the old `pmsudhi.github.io/cameleer/` redirect.

So "ranking preservation" is mostly about *not losing the little that is being built now*:
crawl budget, links from crates.io, Docker Hub, gitmcp/context7, the README and llms.txt. There
are no positions to defend. **Recommendation: do the whole URL move once, soon, and do not
split it into phases.** Every month of delay adds inbound links that point at the old paths.

## 1. Query universe by intent and persona

Personas:
- **IA**: integration architect or Camel user (Java shop, Kubernetes, cost pressure).
- **AB**: AI agent and platform builder (MCP, tool calls).
- **HI**: healthcare interface engineer (HL7, FHIR, DICOM; Mirth refugees).
- **RD**: Rust developer choosing an HTTP stack.
- **EL**: engineering leader, FinOps or platform lead (cost, rewrite decisions).
- **CU**: curious reader or HN crowd (bloat, Wirth, AI code).

Difficulty is a judgement from the SERP sample: **L** means forums, directories and thin blogs
rank; **M** means vendor blogs plus a few strong pages; **H** means vendors with big domains and
G2-style aggregators.

### 1.1 Apache Camel alternatives, Camel in Rust, Camel performance, Camel on Kubernetes and serverless

| example queries | intent | persona | best page today | target page (new) | difficulty and notes |
|---|---|---|---|---|---|
| apache camel alternatives; alternative to apache camel; camel vs spring integration; kafka connect vs camel | commercial | IA | `/alternatives/apache-camel-alternatives/` | `/cameleer/alternatives/apache-camel-alternatives/` | **M-H.** The SERP is alternativeto, G2, FitGap, StackShare, Red Hat Developers and a 2011 Kai Wähner post. It is old and directory-heavy, which is beatable with a current, honest page. G2's top list is IBM App Connect, Azure Service Bus, TIBCO, webMethods and WSO2. Shuttle (Rust) shows up as an "alternative" through StackShare noise. |
| apache camel rust; camel in rust; rust integration framework; rust enterprise integration patterns | info/commercial | IA, RD | `/alternatives/apache-camel-in-rust/` | `/cameleer/alternatives/apache-camel-in-rust/` | **L, but taken.** rust-camel's docs.rs pages own it. We win by being the page that *compares* the options with measurements (it already does). Claim "Camel YAML routes unchanged" and "MCP", which rust-camel lacks. |
| apache camel vs …; camel benchmark; camel memory usage; camel startup time; camel quarkus native memory | info/commercial | IA | `/compare/apache-camel-vs-cameleer/` | `/cameleer/compare/apache-camel-vs-cameleer/` | **L-M.** The SERP is Red Hat 2021 marketing, Quarkus guides, a Camel 2020 blog post and an unsourced Camel K blog. The engine said no independent, current benchmark exists. **That gap is ours.** We have 1.7-8.7x throughput, 3 ms startup vs 1.1 s (native 30 ms) and up to 54x less memory, against Camel 4.23 and Camel Quarkus 3.40. |
| camel k alternative; camel kubernetes memory; integration on kubernetes small footprint | commercial | IA, EL | none (spread over DEPLOYMENT and compare) | **new** `/cameleer/use-cases/kubernetes-integration/` | **L-M.** Camel K is now a hard sell in the Camel community, so its searchers are looking. Use the numbers: 20.4 MB image, a running HTTP route at 2.6 MiB and 3 threads. |
| serverless camel; camel aws lambda cold start; integration lambda cold start | commercial | IA | `/use-cases/serverless-aws-lambda/` | `/cameleer/use-cases/serverless-aws-lambda/` | **L.** Cold start 1.17 ms (all components) and 0.69 ms (musl core); init 3.5-5.2 ms in the RIE. Say plainly that it has not been measured on the Lambda service yet. |
| migrate from apache camel; camel yaml dsl without jvm | info | IA | `/docs/guides/migrating-from-apache-camel.html` | `/cameleer/docs/guides/migrating-from-apache-camel.html` | **L.** A doc page with a "\| Cameleer" title. Give it a stronger title (section 3). |

### 1.2 Integration platform comparisons

| queries | intent | persona | best today | target | notes |
|---|---|---|---|---|---|
| mulesoft alternatives; open source mulesoft alternative; mulesoft pricing alternative | commercial | IA, EL | `/alternatives/mulesoft-alternatives/` | `/cameleer/alternatives/mulesoft-alternatives/` | **H.** Vendors (Boomi, Workato, WSO2) and aggregators. Win the long tail, for example "open source mulesoft alternative self hosted". |
| mulesoft vs camel; wso2 vs camel; wso2 vs mulesoft | commercial | IA | the two three-way `/compare/` pages | `/cameleer/compare/...` | **M.** Three-way pages are a good format here. |
| mirth connect alternatives; mirth connect open source fork; nextgen connect license change; bridgelink vs oie | commercial | HI | `/alternatives/mirth-connect-alternatives/` | `/cameleer/alternatives/mirth-connect-alternatives/` | **M, and rising.** The 4.6 licence change drives it. The SERP is vendor pages (Rhapsody, WSO2 `/mirth-connect-migration/`, nirmitee, Tactionsoft), plus BridgeLink and OIE. Ours is the only "interfaces as code" angle. |
| sap cpi alternative; sap integration suite camel; edge integration cell alternative | commercial | IA (SAP) | none | later: `/cameleer/alternatives/sap-integration-suite/` | **M.** Wait until something is built (the gap analysis is research only). Publishing now would break "no claim without a number". |

### 1.3 MCP servers, agent tools, LLM gateways

| queries | intent | persona | best today | target | notes |
|---|---|---|---|---|---|
| mcp server for existing apis; expose rest api as mcp tools; mcp server integration routes | commercial | AB | `/use-cases/mcp-tool-server/` | `/cameleer/use-cases/mcp-tool-server/` | **M.** Lead with `--mcp-expose`: existing routes as tools, no edits. |
| apache camel mcp server; camel-mcp; camel ai tool | nav/commercial | AB, IA | `/compare/camel-mcp-server-vs-cameleer/` | `/cameleer/compare/camel-mcp-server-vs-cameleer/` | **L-M.** The SERP has Camel's own MCP plugin, the Quarkus extension and DScope `camel-mcp`. We have the measured comparison: 39,922 vs 22,604 calls/s, 56 vs 289 MB. |
| enterprise mcp gateway; best mcp gateway 2026; open source mcp gateway | commercial | AB, EL | `/compare/enterprise-mcp-platforms/` | `/cameleer/compare/enterprise-mcp-platforms/` | **H.** Vendor listicles dominate (Maxim/Bifrost publishes many near-identical pages, TrueFoundry, MintMCP, Obot, Tyk). The engine said it found no independent, head-to-head benchmark. Our measured run is the differentiator. |
| mcp server benchmark; mcp typescript sdk performance; fastest mcp server | info | AB, RD | the BENCHMARKS doc section | **new** `/cameleer/benchmarks/mcp/` (or a section of the ledger) | **L.** A Medium post "where does Rust land in an MCP benchmark" ranks, so the field is thin. We have 1.77x Camel and 17.7x the TS SDK (zod 4 rerun), and we already publish that Camel's p99 is lower. |
| reliable ai agent tool calls; idempotent tool calls; agent tool approval; human in the loop mcp | info | AB | `/compare/ai-agent-tool-calls-direct-vs-cameleer/`, `/mcp/` | `/cameleer/mcp/` and the compare page | **M.** Field test: 157/160 on Opus in sessions. |
| llm gateway in process; llm fallback retries caching library | commercial | AB | `/use-cases/llm-gateway-in-process/` | `/cameleer/use-cases/llm-gateway-in-process/` | **H** (LiteLLM, Portkey, Kong). Keep it as long tail. |

### 1.4 Healthcare: HL7, FHIR, DICOM

| queries | intent | persona | best today | target | notes |
|---|---|---|---|---|---|
| open source hl7 interface engine; hl7 integration engine comparison; hl7 mllp server | commercial | HI | `/healthcare/`, `/use-cases/hl7-mllp-healthcare-integration/` | `/cameleer/healthcare/` (the health pack hub) | **M.** A dedicated "mllp-server" comparison site ranks, as do Tactionsoft listicles. Proof: the health live tier passes 10/10 against DCMTK, Orthanc, HAPI FHIR and python-hl7. |
| dicom router open source; dimse to dicomweb gateway; dicom ai inference routing | commercial | HI | `/use-cases/dicom-imaging-gateway/` | `/cameleer/use-cases/dicom-imaging-gateway/` | **L-M.** A niche. The DICOM Conformance Statement is a trust asset. Publish it as an HTML page and link it. |
| fhir integration engine; hl7 v2 to fhir | info/commercial | HI | the HL7 use case | same | **M.** Don't claim v2-to-FHIR mapping unless it ships. |

### 1.5 Rust HTTP servers (simoom)

| queries | intent | persona | best today | target | notes |
|---|---|---|---|---|---|
| rust http server without tokio; sync rust web server; rust web server no async | info/commercial | RD | `/simoom/alternatives/rust-http-server-without-tokio/` | unchanged | **L.** The SERP is Astra, std-only tutorials and toy repos. Winnable now. |
| zero dependency rust http server; rust http server no dependencies; supply chain minimal rust server | commercial | RD | `/simoom/use-cases/zero-dependency-supply-chain/` | unchanged | **L.** |
| rust http/3 server; http3 rust without quinn | commercial | RD | `/simoom/use-cases/http3-server/` | unchanged | **M** (quinn, h3 and Cloudflare quiche own it). Publish the loss to H2O honestly. |
| rust http server benchmark; fastest rust web framework 2026; axum vs actix benchmark | info | RD | `/simoom/benchmarks/` | unchanged | **M.** Self-promoting Hyperlane posts, DEV posts and TechEmpower. A 20-server, use-case-by-use-case run with losses is stronger than anything in the sample. |

### 1.6 Efficiency engineering: the practice (new root)

| queries | intent | persona | best today | target | notes |
|---|---|---|---|---|---|
| software bloat; why is software slow; wirth's law; wirth's law 2026 | info | CU, EL | `/cameleering/wirths-law/` | `/wirths-law/` | **M.** Wikipedia, mirrors and blogs; a 2026 "LLMs and Wirth's law" news angle exists. Our page has measurements, which the mirrors lack. A good HN and AI-overview citation target. |
| code efficiency finops; reduce cloud cost by optimizing code; cloud waste in code; finops engineering | info/commercial | EL | `/cameleering/finops/` | `/finops/` (hub) plus spokes | **H** for head terms (McKinsey, DoiT, Flexera, New Relic, finops.org). Win "memory per instance to dollars" calculators and "JVM vs native cost" long tail. Our edge is arithmetic shown against measured RSS. |
| green software; software carbon intensity; SCI score; energy efficient programming | info | EL, CU | none | **new** `/green-software/` (SCI worked example) | **M.** The Green Software Foundation is the authority (SCI is now ISO). Only enter with a worked SCI calculation from our own measured figures; otherwise skip. |
| should we rewrite in rust; rust rewrite worth it; rewrite cost benefit | info | EL | `/cameleering/when-to-rewrite/` | `/when-to-rewrite/` | **M.** Case studies dominate: Grab 70%, TikTok, The New Stack. A checklist page that cites them, with our ledger as a worked example, fits AI overviews well. |
| is ai generated code slower; ai code performance; llm code efficiency study | info | EL, CU | `/cameleering/ai-code-performance/` | `/ai-code-performance/` | **M** and rising. Keep the evidence page current (dated). |
| how to benchmark honestly; benchmark methodology; measure software efficiency | info | RD, EL | `/cameleering/method/` | `/method/` | **L-M.** |

### 1.7 Brand and navigational

| query | today | target | risk |
|---|---|---|---|
| cameleer; cameleer rust; cameleer camel | nothing found in this engine | `/cameleer/` must win it. Root (Cameleering) is acceptable second. | "cameleer" is an English noun (camel driver); spell-correction to "cameleon". Needs entity signals (section 3). |
| cameleering | nothing | `/` | Coined term. Easy once indexed; the risk is the engine correcting it to "chameleon". |
| simoom; simoom rust | nothing (simoom is also a desert wind) | `/simoom/` | Always pair it with "Rust HTTP server" in titles. |
| karez, ksar | n/a | stub "project" cards on `/projects/` only | Don't create thin pages before there is a number to publish. |

## 2. Ranking preservation for the move

### 2.1 What exists

- **Indexed URLs in the root sitemap: 345.** That is 1 home, 279 `/docs/`, 34 `/simoom/`, 8
  `/use-cases/`, 7 `/compare/`, 7 `/cameleering/`, 4 `/alternatives/`, plus `/mcp/`,
  `/healthcare/`, `/trust/` and two llms files.
- **Published references that cannot be edited:**
  - the crates.io `homepage` and `documentation` of 0.5.x and 0.6.0
  - the Docker Hub overview
  - image labels
  - `CITATION.cff`
  - released READMEs and CHANGELOG
  - gitmcp and context7 indexes
  - the old `pmsudhi.github.io/cameleer/` redirect, which GitHub maps path for path to
    cameleer.ing

  All of these point at `/` or `/docs/...`.
- **No `site/404.html`.** GitHub Pages serves its generic 404, so a moved URL without a stub is a
  dead end. Add a branded 404 that links the practice, the two microsites and search.

### 2.2 GitHub Pages has no server-side 301s

Use **redirect stubs** at every old path: a small HTML page per old URL containing

```html
<link rel="canonical" href="https://cameleer.ing/cameleer/compare/apache-camel-vs-cameleer/">
<meta http-equiv="refresh" content="0; url=/cameleer/compare/apache-camel-vs-cameleer/">
<meta name="robots" content="noindex,follow">
<script>location.replace("/cameleer/compare/apache-camel-vs-cameleer/" + location.hash)</script>
<a href="/cameleer/compare/apache-camel-vs-cameleer/">Moved here</a>
```

Google treats an instant meta refresh as a permanent redirect, and the canonical consolidates
signals. Bing does too, more slowly. Don't use `noindex` alone without the refresh. Keep
`follow`.

Rules:

1. **Generate the stubs from a mapping table in `scripts/build-docs.py`**, never by hand. The
   table is `old path -> new path` and covers all 345 URLs. Test it: every old sitemap URL
   resolves to a stub whose target is in the new sitemap.
2. **Keep the stubs forever.** They cost nothing, and crates.io metadata of published versions
   will point at the old paths for years.
3. **Keep the old URLs out of the new sitemap.** List only the new ones.
4. **Update every internal link** in the same commit (relative links in hand-written pages, the
   docs template, `llms.txt`, `llms-full.txt`, the simoom microsite's cross-links, the README,
   the Docker Hub overview, the crate `homepage`/`documentation` for the next release). An
   internal link to a stub wastes crawl and dilutes the signal.
5. **JSON-LD `url`, `og:url` and canonical** must name the new URL on every moved page.
6. **Search Console:** there is no change-of-address for a move inside one property. Submit the
   new sitemap, use URL Inspection to request indexing of the about 15 key new URLs, and watch
   the Pages report for "Page with redirect" (expected for the stubs) vs "Not found" (a bug).
   Bing Webmaster Tools: submit the sitemap. The existing IndexNow step in `pages.yml` will ping
   the new URLs; also ping the old ones once so engines refetch the stubs.
7. **Freeze titles for 4-6 weeks** after the move (the earlier plan's rule). Make the title
   rewrites of section 3 *in the same commit as the move*, not after: with near-zero rankings
   there is nothing to destabilise, and a second change later would be worse.

### 2.3 Keep or move the Cameleer spokes?

| option | for | against |
|---|---|---|
| **A. Move everything Cameleer under `/cameleer/`**: home, compare, alternatives, use-cases, mcp, healthcare, trust and docs (`/cameleer/docs/...`) | Mirrors `/simoom/` and `/simoom/docs/` exactly. Every URL says which project it belongs to. Root topical signal stays "efficiency practice", and `/cameleer/` becomes a clean topical cluster. The root `/docs/` would otherwise be ambiguous ("whose docs?"). Authority is near zero, so this is the cheapest it will ever be. | About 300 stubs. One-time build work. A few weeks of recrawl. |
| **B. Move only the home; keep `/compare/`, `/use-cases/`, `/alternatives/`, `/docs/` at root** | Fewer stubs. External `/docs/` links stay direct. | The root hosts practice pages and Camel docs side by side, which mixes clusters. Breadcrumbs say "Cameleer" under a root that is not Cameleer. simoom is inconsistent. The move happens anyway later, when it costs more. |
| **C. Hybrid: move the landing pages, keep `/docs/`** | External doc links stay direct. | Same ambiguity as B for 279 pages, which are the majority of indexed URLs. |

**Recommendation: A, in one release.**

Two exceptions stay at root, because they are Cameleering-level pages that compare
*approaches*, not Cameleer:
- `/compare/` becomes a root hub that lists both microsites' comparisons.
- Vendor-neutral pages such as `/alternatives/apache-camel-alternatives/` could arguably stay at
  root as "practice" content. **Don't do it.** They sell Cameleer, and the brand query wants them
  in the Cameleer cluster.

Practice pages move from `/cameleering/<x>/` to `/<x>/`. `/cameleering/` becomes a stub to `/`.

### 2.4 The brand-query risk: "cameleer" landing on a practice page

Once the root is Cameleering, an engine may show `/` for "cameleer" (same domain name, the
higher-authority page). That is acceptable *if* `/` routes the visitor in one click. Do all of
these:

- **`/cameleer/` title starts with "Cameleer".** The root title does not contain "Cameleer"
  alone. It says "Cameleering", and the projects block names "Cameleer" with a prominent link.
- **Root JSON-LD:**
  - `Organization` (Cameleering) and `WebSite`
  - `"owns"`/`"brand"` is not right for software, so use `subjectOf`/`hasPart`, or better, an
    `ItemList` of the `SoftwareSourceCode` entities, each with its `url` at the microsite
  - `sameAs` on the Organization: GitHub org, Docker Hub org, crates.io team
- **`/cameleer/` JSON-LD:** keep `alternateName: "rust-camel"`. Add `"publisher"` and
  `"maintainer"` pointing at the Cameleering Organization `@id`
  (`https://cameleer.ing/#org`). This entity graph is what lets Google show `/cameleer/` as the
  sitelink for the brand.
- **Root hero:** a visible "Projects: Cameleer (Apache Camel in Rust), simoom (Rust HTTP
  server)" row above the fold.
- **Sitelinks:** consistent nav labels ("Cameleer", "simoom", "Ledger", "FinOps") on every page
  help Google build sitelinks for `cameleer.ing`.

## 3. Page-type templates and on-page rules

Rules for every page type:
- Title of 60 characters or less, with the primary query first and the brand last.
- Meta description of 150-158 characters or less, with one number.
- One H1, matching the title's topic.
- The canonical is the absolute self URL with a trailing slash.
- No hreflang (English only).
- One `@graph` per page with stable `@id`s (`https://cameleer.ing/#org`,
  `https://cameleer.ing/cameleer/#software`, `https://cameleer.ing/simoom/#software`).
- `BreadcrumbList` on everything except the root.

| page type | title pattern | H1 | JSON-LD | OG image |
|---|---|---|---|---|
| **Root (practice home)** | `Cameleering: Software Efficiency, Measured Against the Original` | "Carry more on less." is fine as a visual line, but add a text H1 or subtitle containing "software efficiency" | `Organization` (+`slogan`, `logo`, `sameAs`), `WebSite`, `ItemList` of projects, `DefinedTerm` for "cameleering" (definition updated, see below) | Cameleering brand card |
| **Microsite home** (`/cameleer/`, `/simoom/`) | `Cameleer: Apache Camel in Rust, MCP Tools for AI Agents` (keep today's root title as is); `simoom: Rust HTTP Server with Zero Dependencies` | product claim | `SoftwareSourceCode` (+`SoftwareApplication` with `applicationCategory: DeveloperApplication`, `operatingSystem`, `offers` price 0 if you want rich results), `publisher` → `#org`, `isBasedOn` (Camel) | per-project card |
| **Comparison** | `X vs Y: <metric>, <metric> Benchmarks \| Cameleer` | "X vs Y" | `TechArticle` (+`about` both products), `FAQPage`, `Dataset` when it carries result tables, `BreadcrumbList` | a table card with 2-3 numbers |
| **Alternatives** | `<Product> Alternatives: <angle> (2026)` | question form | `TechArticle`, `ItemList` of the alternatives (today's alternatives pages lack `ItemList`; simoom's has it), `FAQPage` | |
| **Use case** | `<Job to be done> in Rust: <proof>` | outcome | `TechArticle`, `FAQPage`, `HowTo` only if there are real steps | |
| **Practice essay** (Wirth, FinOps, rewrite, AI code) | `<Topic>: <what is measured>` without the "\| Cameleering" suffix if it pushes past 60 characters | same | `TechArticle` (or `ScholarlyArticle`-like via `citation` entries), `FAQPage`, `author` → Person, `publisher` → `#org`, `dateModified` | chart card |
| **Ledger** | `The Cameleering Ledger: Measured Gains and Losses` | as now | `TechArticle` + `Dataset` (each campaign as `distribution`), `ItemList` of claims | |
| **Glossary/definitions** | `What Is <term>?` | same | `DefinedTerm` in a `DefinedTermSet` | |
| **Docs** | `<Topic> \| Cameleer docs` (not "\| Cameleer", so the cluster is clear; component pages: `Kafka Component for Cameleer (Apache Camel in Rust)`) | as now | `TechArticle`, `BreadcrumbList`, `isPartOf` WebSite | default project card |

Specific fixes found on the current pages:

- **Root `index.html` has no text H1**: the extraction found an empty H1, since the lines sit in
  spans. The struck-through "On the JVM." is read as heading text (an old finding). On
  `/cameleer/`, use an H1 with plain text such as "Apache Camel routes, in Rust." and put the
  strike outside the H1 or in `aria-hidden`.
- **Root JSON-LD has no `Organization` or `publisher`.** Only the `SoftwareSourceCode` with a
  `Person` author. Add the `#org` link.
- **`DefinedTerm` on `/cameleering/`** must use the *new* definition (the practice: "the
  practice of making software carry the same load on less ... proving every gain against the
  original"). `docs/website-plan.md` §3.4 still fixes the old definition ("running Apache Camel
  routes on Cameleer"), which is **stale**. Grep `llms.txt`, the docs FAQ and the README for the
  old sentence.
- **simoom pages have no `publisher` Organization** on their home (`CreativeWork`, `Person`
  only). Link them to `#org`. The simoom titles lack a brand suffix on several pages. That is
  fine, but add `| simoom` where it fits.
- **The meta keywords tag** is ignored by Google and Bing and only reveals the target list.
  Drop it.
- **OG:** one 1200×630 card per page type. Generate per-page cards for compare, benchmark and
  ledger pages with the headline number, since they get shared on HN and LinkedIn. Today every
  Cameleer page uses `/og.png`.

## 4. Internal linking model

```
                 /  (Cameleering: practice)
       ┌─────────┼──────────┬───────────┬──────────────┐
   /ledger/   /method/   /finops/   /wirths-law/ ...  /projects/
       │          │          │           │              │
       └──── every claim row links to its proof ────────┤
                                                        │
          /cameleer/  (microsite hub)            /simoom/ (microsite hub)
          ├ compare/  ├ alternatives/            ├ benchmarks/ ├ compare/
          ├ use-cases/├ mcp/ healthcare/ trust/  ├ use-cases/  ├ alternatives/
          └ docs/                                └ docs/
```

Rules:

1. **Hubs link down to every spoke; spokes link up once** (breadcrumb plus one contextual link)
   **and sideways to 2-4 siblings.** Today's FinOps page already does this well: ledger, method
   and when-to-rewrite.
2. **Proof loop.** Each practice essay cites at least one ledger row, and each ledger row links
   the project page that holds the measurement:
   - FinOps → ledger row "HTTP route RSS 15.2 → 10.4 MB" → `/cameleer/docs/BENCHMARKS.html`
     and `/cameleer/use-cases/serverless-aws-lambda/`
   - Wirth's law → simoom benchmarks
   - when-to-rewrite → Apache Camel vs Cameleer as the worked example

   Each project comparison page links back to the method page ("how we measured") and to the
   ledger ("where we lost").
3. **Microsite to microsite only where there is real overlap**: simoom
   `use-cases/integration-engine/` ↔ Cameleer, and Cameleer `serverless` ↔ simoom
   `serverless-cold-start`. A shared "Cameleering" menu (already on every page) covers the rest.
4. **Anchor text.** Use descriptive anchors that contain the target's query, for example
   "Apache Camel vs Cameleer benchmark", "Rust HTTP server without tokio" and "code efficiency
   as a FinOps lever". Avoid "here", "read more" and "the ledger" on its own. Vary the anchors
   slightly, and never use the same exact-match anchor for two different targets. Brand anchors
   ("Cameleer", "simoom") go to the microsite homes only.
5. **Docs → landing pages.** The docs template footer gets "Use cases · Compare · Benchmarks"
   for the current project and a single "Cameleering" link. The 279 doc pages are the biggest
   internal-link source, so use them for the money pages (compare, MCP, healthcare), not for
   the root.
6. **Root → projects.** The root links each microsite home with a descriptive anchor ("Cameleer,
   Apache Camel in Rust"), plus the single strongest proof page of each (the Camel benchmark,
   the simoom benchmark).
7. **No orphans.** Add a test in the doc harness: every sitemap URL has at least one inbound
   internal link that is not navigation.

## 5. Content gaps: the highest-value pages, ranked

Ranked by (winnability × intent value × proof we already hold).

| # | page (new or rewrite) | target query | intent | why we can win |
|---|---|---|---|---|
| 1 | **Rewrite `/` as the practice home** | software efficiency engineering; cameleering | nav/info | Owns the brand. It is the hub that gives every essay and project a parent. Must carry the projects block for brand routing. |
| 2 | **`/cameleer/` home** (moved, H1 fixed, Organization link) | cameleer; apache camel in rust | nav/commercial | Brand plus a query that rust-camel holds today. Our differentiators are unchanged YAML routes, MCP and measured numbers. |
| 3 | **Apache Camel performance and memory** (rewrite the compare page's lede or add `/cameleer/benchmarks/`) | apache camel memory usage; camel startup time; camel quarkus native vs jvm | info | The engine found **no independent, current benchmark**. We have 3 ms vs 1.1 s, up to 54x less memory, 1.7-8.7x throughput and a reproducible `scripts/cloud-bench`. |
| 4 | **Rust HTTP server benchmark** (`/simoom/benchmarks/`, strengthen title and summary table) | rust http server benchmark 2026 | info | The SERP is self-promotional single-framework posts. Ours covers 20 servers with losses. |
| 5 | **MCP server benchmark** (new, `/cameleer/benchmarks/mcp/` or a ledger section) | mcp server benchmark; mcp sdk performance | info | Thin SERP. We hold 39,922/s vs 22,604 vs 2,257 and memory 56/289/197 MB, and we publish where Camel wins (p99). |
| 6 | **Mirth Connect alternatives** (refresh for the licence timeline, add OIE and BridgeLink facts and a `Dataset` of interop results) | mirth connect alternatives 2026 | commercial | Fast-moving vendor SERP. We are the only code-first engine with a 10/10 live interop tier. |
| 7 | **Apache Camel alternatives** (add `ItemList`, a 2026 date, Redpanda Connect, n8n, Camel K's status) | apache camel alternatives | commercial | Aggregators and a 2011 post rank. A current, honest decision page can reach the top 5. |
| 8 | **Code efficiency → cloud bill calculator** (`/finops/calculator/`, interactive, from measured RSS and CPU) | reduce cloud cost by optimizing code; lambda memory cost calculator | commercial | Head terms are McKinsey and vendors; a tool page earns links. Our numbers feed it. |
| 9 | **Kubernetes integration footprint** (`/cameleer/use-cases/kubernetes-integration/`) | camel k alternative; small integration runtime kubernetes | commercial | 20.4 MB image, 2.6 MiB running route, 3 threads, read-only rootfs. |
| 10 | **Wirth's law, measured** (keep, add an original chart and cite simoom and Cameleer as counterexamples) | wirth's law; software bloat | info | The SERP is mirrors. An original dataset gets cited by AI overviews. |
| 11 | **When a Rust rewrite pays off** (keep, add a payback worksheet using our engineering-months vs savings) | should we rewrite in rust | info | The case studies are anecdotal; a checklist that cites them, with a worked example, is a citation magnet. |
| 12 | **Green software: an SCI worked example** (new) | software carbon intensity example | info | Only with a real calculation from measured energy or CPU. Otherwise drop it. |
| 13 | **DICOM conformance statement as HTML** (`/cameleer/healthcare/dicom-conformance/`) | <product> dicom conformance statement; open source dicom router | nav/commercial | Procurement teams search for this artefact; it is already generated and tested. |
| 14 | **`/projects/` hub** (Cameleer, simoom, with karez and ksar listed as "in design, no numbers yet") | cameleering projects | nav | Gives karez and ksar a home without thin pages. Keeps the "no claim without a number" rule visible. |
| 15 | **Branded `404.html`** | n/a | n/a | No 404 page exists today; needed for the move. |

Don't build yet: SAP CPI alternative (nothing built), karez and ksar pages, a "best MCP
gateway" listicle (an H SERP, and off-brand).

## 6. Measurement

### 6.1 Baselines to capture before the move (one dated snapshot, committed under `docs/design/site-restructure/research/baseline-2026-11-xx/`)

1. **Search Console** (the Domain property on cameleer.ing; add it if only the URL prefix
   property exists): 28-day and 3-month exports of queries, pages, countries and devices, and
   the Pages indexing report (indexed vs discovered counts). Expect tiny numbers. Record them
   anyway.
2. **Bing Webmaster Tools**: the same, plus the IndexNow submission log.
3. **Manual SERP checks** in Google and Bing, logged out, US and IN: position of a cameleer.ing
   URL for the about 20 queries in section 1 marked as targets, plus the brand queries
   `cameleer`, `cameleer rust`, `cameleering`, `simoom rust`. Screenshot them.
4. **AI answers**: ask Google AI Overviews, Bing Copilot, Perplexity and ChatGPT search "what is
   cameleer", "apache camel in rust", "rust http server without tokio" and "mcp server
   benchmark". Record whether cameleer.ing is cited.
5. **GitHub traffic** (views, unique visitors, referrers, popular paths). The `traffic.yml`
   archive needs a new `TRAFFIC_TOKEN` after the org move (STATUS lists it as open). **Set it
   before the move**, since GitHub keeps only 14 days.
6. **crates.io** downloads per crate (`cameleer-main`, `simoom`). **Docker Hub and GHCR pulls.**
   Stars and forks. `traffic.yml` collects these once the token is set.
7. **Crawl snapshot**: the current `sitemap.xml` (345 URLs) saved as the input to the redirect
   map test.

### 6.2 What to track after the move (weekly for 8 weeks, then monthly)

- **Indexing:**
  - Search Console's new URLs indexed vs submitted
  - old URLs in "Page with redirect" (good) vs "Not found" or "Duplicate without canonical"
    (fix)
  - time until `/cameleer/` replaces `/` for "cameleer"
- **Brand routing:**
  - the landing page for brand queries
  - CTR from `/` to `/cameleer/` and `/simoom/`. GitHub Pages has no analytics; if the owner
    wants it, add a cookieless, self-hosted counter, or accept Search Console plus GitHub
    referrers as the proxy.
- **Per-cluster impressions and clicks:** group the queries by the section 1 clusters with
  Search Console regex filters (`camel`, `mcp`, `hl7|fhir|dicom|mirth`, `http server|tokio`,
  `wirth|bloat|finops|rewrite`).
- **Downstream:**
  - GitHub referrer `cameleer.ing` sessions
  - crates.io downloads and Docker pulls week over week
  - new stars, and issues opened from site links (the "Found a problem?" footer)
- **AI citation:** re-run the section 6.1 prompt set monthly. Check that `llms.txt` and
  `llms-full.txt` at the root describe Cameleering and link the two microsite llms files, and
  that `/cameleer/llms.txt` exists. Coding agents fetch these by convention.

### Stale items in `docs/website-plan.md` (for whoever updates it)

- **§3.4 definition of cameleering**: superseded by the practice definition.
- **§4 keyword map and §5 structure**: assume Cameleer at `/` and `/cameleering/` as a single
  page. Both are superseded by this restructure.
- **The 🚧 MCP slots in §5** are shipped (the MCP server, `/use-cases/mcp-tool-server/`, the
  compare pages).

**Still valid:**
- §2.1 (H1 strike read aloud, keywords placement)
- the alternatives table gaps (Redpanda Connect, n8n)
- the "don't change titles during the migration window" rule
- §10's observation that every competitor now claims MCP, so differentiate on measured
  reliability and footprint, not on having MCP.
