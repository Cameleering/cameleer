# Site inventory and migration mechanics: Cameleering at the root, Cameleer as a microsite

_Research for the site restructure, 2026-11-13. This report covers the site as it is in the
repository at `bde29d8` on branch `simoom-campaign4`: `site/`, plus `site/docs/`, which
`scripts/build-docs.py` generates. It describes; nothing here has been built. Counts come from
scripts run over the tree, and how each was computed is noted where it matters._

## 0. Summary

- The site has **30 designed pages**: the home, 7 `/cameleering/` pages and 22 Cameleer section
  pages. It also has **279 generated docs pages**, each with a `.md` twin (275 of them; the 4
  generated section indexes have none), 5 redirect stubs, and **32 simoom pages** copied in from
  the simoom repository.
- `site/sitemap.xml` lists **345 URLs**: 30 designed pages, 279 docs pages, 34 simoom URLs and
  2 llms files.
- Every page carries **one logical header**, kept as **31 hand-maintained copies**: 30 designed
  pages plus the docs `TEMPLATE` in `build-docs.py`. The copies differ only in the relative
  prefix and `aria-current`. Footers come in **25 variants across 30 pages**, because each footer
  holds the page's own related links.
- **No Rust test asserts a site path.** `docs_harness.rs` treats `https://cameleer.ing/...` links
  as external and never checks them. `build-docs.py` checks docs-to-docs links only. Nothing
  checks links inside or between designed pages. Today that finds 0 broken internal links on
  designed pages.
- **Recommendation: option C (hybrid).** Move the home page and the 22 Cameleer section pages
  under `/cameleer/` (22 redirect stubs, plus one for `/cameleering/` → `/`). Keep `/docs/` where
  it is as Cameleer's documentation. Move docs later only if a family docs hub is ever needed;
  the cost of that move is given in section 3.

## 1. Inventory

### 1.1 Designed pages

Columns:

- **Title len**: the length of the whole `<title>`, including the ` | Cameleer` or
  ` | Cameleering` suffix.
- **Desc**: the length of the meta description.
- **Words**: rough count of the text in `<main>`.
- **Hdr**: the header variant. a = depth 2, b = depth 1, h = home; the links are identical in
  all three.
- **CSS**: s = `site.css`, c = `compare.css`, g = `cameleering.css`.
- **In**: inbound links from the `<main>` content of other designed pages (header and footer
  excluded).
- **Nav**: whether the page is also linked from the global header on all 30 pages.

Every canonical equals `https://cameleer.ing/<path>`, with no mismatches.

| path | title len | H1 | desc | JSON-LD types | words | hdr | css | in | nav |
|---|---|---|---|---|---|---|---|---|---|
| `/` | 55 | Integration routes. On the JVM. In Rust. | 152 | Person, SoftwareSourceCode×2 | 2180 | h | s | – | brand |
| `/cameleering/` | 53 | Carry more on less. | 181 | TechArticle, FAQPage(8), DefinedTermSet, DefinedTerm, ItemList, SoftwareSourceCode×3, Organization×2, BreadcrumbList | 2111 | b | s c g | 7 | yes |
| `/cameleering/ledger/` | 63 | The ledger: every claim, its baseline and its method | 164 | TechArticle, BreadcrumbList | 1782 | a | s c g | 5 | yes |
| `/cameleering/method/` | 57 | How to measure, so the numbers hold up | 174 | TechArticle, BreadcrumbList | 677 | a | s c g | 6 | – |
| `/cameleering/finops/` | 47 | Code efficiency as a FinOps lever | 172 | TechArticle, FAQPage(5), BreadcrumbList | 1306 | a | s c g | 2 | – |
| `/cameleering/wirths-law/` | 43 | Wirth's law in 2026: measured | 181 | TechArticle, FAQPage(4), BreadcrumbList | 1000 | a | s c g | **1** | – |
| `/cameleering/ai-code-performance/` | 61 | AI-generated code and performance: what the evidence says | 166 | TechArticle, FAQPage(4), BreadcrumbList | 1260 | a | s c g | **1** | – |
| `/cameleering/when-to-rewrite/` | 59 | When a rewrite pays off: a decision checklist | 162 | TechArticle, FAQPage(4), BreadcrumbList | 1272 | a | s c g | 2 | – |
| `/mcp/` | 51 | The model decides what. A tested route decides how. | 156 | TechArticle, FAQPage(9), Thing×3, BreadcrumbList | 3095 | b | s c | 10 | yes |
| `/trust/` | 61 | Cameleer can prove what it runs. | **204** | TechArticle, FAQPage(6), Thing×3, BreadcrumbList | 1382 | b | s c | 10 | yes |
| `/healthcare/` | **70** | An open-source healthcare integration engine: HL7, FHIR and DICOM… | **213** | TechArticle, FAQPage(6), SoftwareApplication, Offer, Thing×4, BreadcrumbList | 1554 | b | s c | 4 | yes |
| `/use-cases/` | **75** | What people run on Cameleer | **261** | CollectionPage, TechArticle×9, BreadcrumbList | 960 | b | s c | 11 | yes |
| `/use-cases/mcp-tool-server/` | 61 | Integration routes as MCP tools | 191 | TechArticle, FAQPage(7), SoftwareSourceCode, Thing×2, BreadcrumbList | 2400 | a | s c | 8 | – |
| `/use-cases/llm-gateway-in-process/` | 62 | An LLM gateway that is not another hop | 198 | TechArticle, FAQPage(4), Thing×3, BreadcrumbList | 1088 | a | s c | 3 | – |
| `/use-cases/hl7-mllp-healthcare-integration/` | 63 | HL7 v2 over MLLP, FHIR, and an acknowledgement that means it | 203 | TechArticle, FAQPage(6), Thing×3, BreadcrumbList | 1080 | a | s c | 6 | – |
| `/use-cases/dicom-imaging-gateway/` | 62 | A DICOM router that is also an integration engine | **257** | TechArticle, FAQPage(6), Thing×3, BreadcrumbList | 1640 | a | s c | 6 | – |
| `/use-cases/mqtt-iot-edge-gateway/` | **79** | MQTT edge gateways that keep working when the uplink doesn't | 206 | TechArticle, FAQPage(5), Thing×3, BreadcrumbList | 1115 | a | s c | 5 | – |
| `/use-cases/serverless-aws-lambda/` | 68 | Integration routes as functions, without the cold start | 227 | TechArticle, FAQPage(5), Thing×3, BreadcrumbList | 1203 | a | s c | 4 | – |
| `/use-cases/file-sftp-pipelines/` | **73** | File and SFTP pipelines that run in megabytes | 217 | TechArticle, FAQPage(5), Thing×3, BreadcrumbList | 859 | a | s c | 2 | – |
| `/compare/` | 55 | Compare Cameleer | 147 | CollectionPage, TechArticle×8 | 363 | b | s c | 6 | yes |
| `/compare/apache-camel-vs-cameleer/` | **78** | Apache Camel vs Cameleer | 199 | TechArticle, FAQPage(5), Dataset, SoftwareSourceCode×2, BreadcrumbList | 1997 | a | s c | **12** | – |
| `/compare/camel-mcp-server-vs-cameleer/` | 46 | Apache Camel's MCP server vs Cameleer | 185 | TechArticle, FAQPage(4), SoftwareSourceCode×2, Thing, BreadcrumbList | 1373 | a | s c | 7 | – |
| `/compare/ai-agent-tool-calls-direct-vs-cameleer/` | **79** | AI agent tool calls: direct function calling vs Cameleer | 200 | TechArticle, FAQPage(8), SoftwareSourceCode, Thing, BreadcrumbList | 1309 | a | s c | 5 | – |
| `/compare/enterprise-mcp-platforms/` | 44 | Enterprise MCP platforms compared | 151 | TechArticle, FAQPage(4), Thing×3, BreadcrumbList | 1269 | a | s c | 7 | – |
| `/compare/mulesoft-vs-apache-camel-vs-cameleer/` | 47 | MuleSoft vs Apache Camel vs Cameleer | 142 | TechArticle, FAQPage(4), SoftwareApplication×3, BreadcrumbList | 1068 | a | s c | 3 | – |
| `/compare/wso2-vs-apache-camel-vs-cameleer/` | 43 | WSO2 vs Apache Camel vs Cameleer | 132 | TechArticle, FAQPage(4), SoftwareApplication×3, BreadcrumbList | 971 | a | s c | 3 | – |
| `/alternatives/apache-camel-alternatives/` | 58 | Apache Camel alternatives: which one fits why you are looking? | 147 | TechArticle, FAQPage(6), BreadcrumbList | 1758 | a | s c | 5 | – |
| `/alternatives/apache-camel-in-rust/` | 55 | Apache Camel in Rust: the options, and what has been measured | 191 | TechArticle, FAQPage(5), BreadcrumbList | 1146 | a | s c g | **1** | – |
| `/alternatives/mirth-connect-alternatives/` | 68 | Mirth Connect alternatives: the open-source forks, the commercial engines… | 194 | TechArticle, FAQPage(5), BreadcrumbList | 1670 | a | s c | 2 | – |
| `/alternatives/mulesoft-alternatives/` | 60 | MuleSoft alternatives: replace the part you need to | 143 | TechArticle, FAQPage(4), BreadcrumbList | 1030 | a | s c | 3 | – |

Every article page except the home and the two collection pages also carries `Organization` and
`Person` (author and publisher) inside its TechArticle.

**Hygiene notes to fix during the move:**

- Titles: 8 are longer than 65 characters, and 4 of those are 75 or more.
- Meta descriptions: 20 are longer than 165 characters, up to 261 on `/use-cases/`.
- The home page has neither `WebSite` nor `Organization` JSON-LD. The new root needs both, for
  Cameleering, and `/cameleer/` keeps `SoftwareSourceCode`.

### 1.2 Generated docs (`site/docs/`, gitignored, built in CI)

| section | pages | notes |
|---|---|---|
| root | 6 | `index`, `BENCHMARKS`, `DEPLOYMENT`, `FEATURES` (269 KB), `INTEROP`, `ROADMAP` |
| `components/` | 162 + 2 stubs | stubs: `mllp`, `fhir` → `packs/health/…` |
| `dataformats/` | 28 + 2 stubs | stubs: `hl7`, `fhir` |
| `languages/` | 22 + 1 stub | stub: `hl7terser` |
| `guides/` | 46 | includes `guides/mcp/*` |
| `packs/` | 15 | the health pack |
| **total** | **279 pages, 275 `.md` twins, 5 stubs** | source: 278 markdown files under `docs/`, excluding `design/` and `release-notes/` (`INTERNAL_DIRS`) |

### 1.3 Shared assets

| asset | size | used by |
|---|---|---|
| `assets/site.css` | 18.7 KB | all 30 designed pages, docs, simoom |
| `assets/compare.css` | 5.7 KB | 29 designed pages (all but the home), simoom (14 links) |
| `assets/cameleering.css` | 2.4 KB | 7 cameleering pages and `/alternatives/apache-camel-in-rust/` |
| `assets/docs.js` | 3.6 KB | docs, simoom docs (18 links) |
| `brand/` | 8 entries | Cameleer logos and mark, `brand/cameleering/`, `brand/source/`, `datahq-logo.png` |
| root files | | `favicon-32.png`, `favicon-192.png`, `apple-touch-icon.png`, `og.png`, `.nojekyll`, `robots.txt`, IndexNow key `c7c97f8e…293.txt` |

There is no `404.html` and no `CNAME`; the custom domain is set in the Pages settings.

### 1.4 Header, nav and footer as they are actually maintained

**The header.** The links are the same everywhere:

- Cameleer brand → `/`
- Cameleering menu: The practice, The ledger, simoom
- Docs, Use cases, MCP
- Packs menu: Healthcare, All packs
- Trust, Compare, GitHub

It is pasted into 31 places:

- 23 depth-2 pages (prefix `../../`)
- 6 depth-1 pages (prefix `../`)
- the home page (no prefix, multi-line formatting, `href="#"` for the brand)
- the docs `TEMPLATE` in `scripts/build-docs.py`, which uses `{site_root}`/`{root}` and has a
  slightly different item order: Docs, Guides, Components, Packs, Use cases, Trust

Only `aria-current="page"` and the prefix vary between copies. Any nav change today is a
scripted edit across 30 HTML files plus one template, and nothing checks that they agree.

**The footer.** There are 25 distinct footers across 30 pages:

- The 6 cameleering subpages share one Cameleering footer with the columns Cameleering and
  Projects.
- `/cameleering/` has its own.
- Every Cameleer page has a hand-picked "related" list. For example,
  `/use-cases/file-sftp-pipelines/` and `/use-cases/mqtt-iot-edge-gateway/` differ in three
  links.

The docs footer, in the template, is a fourth design.

**simoom** has its own header: brand → `/simoom/`, then Docs, Use cases, Benchmarks, Compare,
Cameleering → `../cameleering/`, GitHub. It is the model for a microsite nav.

### 1.5 llms.txt

`site/llms.txt` (52 KB) is both source and output: `write_llms()` rewrites its
`## Documentation` section in place. The root `llms.txt` is a hand-kept copy that differs only in
line endings (202 CRLF lines).

Sections, in order:

- `# Cameleer` (with key facts)
- `## Use cases`
- `## Cameleering`
- `## Healthcare`
- `## Quick start`
- `## Examples`
- `## Connect a coding agent`
- `## Documentation` (generated)
- `## Project pages`
- `## Related`
- `## Contributing`

It holds 108 absolute `cameleer.ing` URLs. `site/llms-full.txt` (2.1 MB, generated, gitignored)
holds the docs only. simoom ships its own `site/simoom/llms.txt` (12.7 KB) and
`llms-full.txt` (142 KB).

### 1.6 Sitemap

`site/sitemap.xml` is committed and regenerated by the build: **345 URLs**.

| section | URLs |
|---|---|
| `/` + `llms.txt` + `llms-full.txt` | 3 |
| `/cameleering/` | 7 |
| `/compare/` | 7 |
| `/use-cases/` | 8 |
| `/alternatives/` | 4 |
| `/mcp/`, `/trust/`, `/healthcare/` | 3 |
| `/docs/` | 279 |
| `/simoom/` (merged from `site/simoom/sitemap.xml`) | 34 |

How the list is built:

- `designed_pages()` walks a **hard-coded list of top-level directories**: `cameleering`, `mcp`,
  `compare`, `alternatives`, `use-cases`, `trust`, `healthcare`.
- A new top-level directory is silently left out of the sitemap until it is added to that list.
- The deploy step sends every sitemap URL to IndexNow.

## 2. Internal link graph

### 2.1 Designed pages

Every designed page links to 9 others through the header, so header links say nothing about
importance. The counts below are content-only: links inside `<main>`.

- **Most linked:**
  - `/compare/apache-camel-vs-cameleer/` (12)
  - `/use-cases/` (11)
  - `/mcp/` (10)
  - `/trust/` (10)
  - `/use-cases/mcp-tool-server/` (8)
  - `/cameleering/` (7), `/compare/camel-mcp-server-vs-cameleer/` (7) and
    `/compare/enterprise-mcp-platforms/` (7)
- **Weakly linked** (1–2 content links, and not in the nav):
  - `/cameleering/wirths-law/` (1, from `/cameleering/` only)
  - `/cameleering/ai-code-performance/` (1)
  - `/alternatives/apache-camel-in-rust/` (1)
  - `/cameleering/finops/` (2)
  - `/cameleering/when-to-rewrite/` (2)
  - `/alternatives/mirth-connect-alternatives/` (2)
  - `/use-cases/file-sftp-pipelines/` (2)

  The **FinOps page, which the owner wants to lead the root, is among the weakest-linked pages
  on the site.** The restructure fixes that by construction.
- **Orphans:** none. **Broken internal links** among designed pages: 0.
- **Into docs:** 273 content links from designed pages (418 counting headers and footers). The
  biggest sources are:
  - the home page (93)
  - `/use-cases/file-sftp-pipelines/` (26)
  - `/mcp/` (23)
  - `/healthcare/` (18) and `/use-cases/mqtt-iot-edge-gateway/` (18)

  All of them are relative (`../../docs/...`), so moving a page one level deeper breaks every
  one of them unless it is rewritten.
- **From docs into designed pages:** only about 10 distinct pages link out, all through absolute
  URLs in the markdown, plus the template's nav and footer, which link to `/`, `/cameleering/`,
  `/cameleering/ledger/`, `/simoom/`, `/healthcare/`, `/use-cases/`, `/trust/` and `/compare/`.
  The markdown links are in:
  - `docs/README.md` (use-cases, compare, alternatives)
  - `docs/guides/faq.md` (cameleering, ledger, compare)
  - `docs/guides/mcp.md` and `docs/guides/agent-tool-calls.md` (compare pages)
  - `docs/packs/health/index.md` (healthcare)
  - `docs/BENCHMARKS.md` (compare)
- **simoom → the rest of the site.** These are relative links baked in by simoom's
  `scripts/build-site.py`:
  - `index.html`: 36 links, labelled "Cameleer"
  - `cameleering/`: 66
  - `cameleering/ledger/`: 4
  - `compare/`: 1
  - 2 use-case pages
  - `assets/site.css`: 32, `assets/docs.js`: 18, `assets/compare.css`: 14
  - favicons: 64

  After the swap, simoom's "Cameleer" links (`../`) would land on the Cameleering root. That is
  a semantic break, not a 404.

### 2.2 External references to current URLs

These come from `git grep cameleer.ing`, excluding `site/` copies.

| place | URLs | change needed under option C |
|---|---|---|
| `Cargo.toml` `homepage` / `documentation` (all 19 published crates inherit them) | `/`, `/docs/` | `homepage` → `/cameleer/` in the next release. **Already-published crate versions keep `/` and `/docs/` forever.** |
| `README.md` (13) | `/`, `/docs/`, `/docs/components/`, `/use-cases/`, `/use-cases/mcp-tool-server/`, `/use-cases/dicom-imaging-gateway/`, `/healthcare/`, `/compare/apache-camel-vs-cameleer/` ×2, `/compare/camel-mcp-server-vs-cameleer/`, `/cameleering/`, `/cameleering/ledger/`, `/llms.txt`, `/llms-full.txt` | rewrite all section links. Copies of the README on crates.io, for published versions, stay old. |
| `docker/README.dockerhub.md` (5), synced to Docker Hub by `dockerhub-description.yml` | `/`, `/cameleering/`, `/cameleering/ledger/`, `/compare/apache-camel-vs-cameleer/`, `/mcp/`, `/docs/guides/build-features.html` | rewrite, and the sync updates Docker Hub on the next run |
| `Dockerfile:71`, `.github/workflows/container.yml:95` | `org.opencontainers.image.url=https://cameleer.ing/` | → `/cameleer/`. Images already published keep `/`. |
| `CITATION.cff:9` | `/` | → `/cameleer/` |
| `CHANGELOG.md:147` | `/docs/guides/mcp-shop-example.html` | none (docs stay) |
| `AGENTS.md:68` | `/llms-full.txt` | depends on the llms split (section 5.4) |
| `docs/README.md`, `docs/guides/faq.md`, `docs/guides/mcp.md`, `docs/guides/agent-tool-calls.md`, `docs/packs/health/index.md`, `docs/BENCHMARKS.md` | compare, use-cases, alternatives, healthcare, cameleering | rewrite. These also become **links from docs into moved pages.** |
| `docs/conf.py:45` (Read the Docs) | `html_baseurl = /docs/` | none |
| `docs/release-notes/0.6.0.md` (8) | various | none: already-published release bodies stay as they are; stubs cover them |
| `docs/design/brand/org-profile-README.md` (6), the source of the org profile | `/`, `/docs/`, `/cameleering/`, `/cameleering/ledger/` (and `#simoom`), `/cameleering/method/`, `/llms.txt` | rewrite: `/cameleering/` → `/`, site → `/cameleer/` |
| `scripts/sbom.py:141` | `https://cameleer.ing/sbom/` + key, the UUIDv5 namespace | **must not change**: it is an identifier, and changing it changes every SBOM serial number |
| `crates/cameleer-ai/src/mcp/mod.rs:45` | reverse-DNS `ing.cameleer` | must not change (protocol identifier) |
| `scripts/build-docs.py` | `SITE_URL`, `TEMPLATE`, `designed_pages()` list | see section 5 |
| `.github/workflows/pages.yml` | IndexNow host and key location | none |
| `site/simoom/**` (sitemap, llms, every page's canonical) | `/simoom/...` | none for simoom URLs. The "Cameleer" links (`../`) are re-labelled in the simoom repository. |
| Internal design notes (`docs/website-plan.md`, `docs/design/*.md`, `docs/STATUS.md`) | various | update when convenient; not published |

## 3. URL plan options

| | A: everything Cameleer under `/cameleer/` (docs too) | B: only the home moves | C (hybrid, **recommended**): designed Cameleer pages under `/cameleer/`, `/docs/` stays |
|---|---|---|---|
| Root `/` | Cameleering | Cameleering | Cameleering |
| Cameleer home | `/cameleer/` | `/cameleer/` | `/cameleer/` |
| `/compare/`, `/use-cases/`, `/alternatives/`, `/mcp/`, `/trust/`, `/healthcare/` | `/cameleer/…` (22 pages) | stay | `/cameleer/…` (22 pages) |
| `/docs/` | `/cameleer/docs/` (279 pages, 275 `.md`) | stays | stays (Cameleer's docs; the header brand → `/cameleer/`) |
| `/cameleering/*` | index → `/`; subpages stay or move (section 3.1) | same | same |
| URLs changed | 1 home swap + 22 + 279 + 275 `.md` + 1 = **about 578** | 1 + 1 | 1 + 22 + 1 = **24** |
| Redirect stubs | 22 + 279 HTML + 1, the 5 existing stubs retargeted; `.md` twins can't redirect (keep copies at the old paths, or drop them) | 1 | **23** |
| Internal links rewritten | 273 links from designed pages to docs, 31 header copies, the docs template, docs `site_root` depth, 108 llms URLs, all of llms-full | header copies, footers | the 22 moved pages' relative prefixes (one level deeper: `../../docs/` → `../../../docs/`); links between moved pages stay relative and unchanged; 31 header copies; about 10 links in docs markdown; about 60 llms URLs |
| simoom | `../` re-labelled; `../assets` still works if assets stay at the root | "Cameleer" links land on Cameleering (wrong) | same as A |
| Rust tests | none assert paths; `docs_harness` external-link rules unchanged | none | none |
| External breakage | every crates.io `documentation` link and every README docs link goes through a stub, permanently | none | links to section pages (README, Docker Hub, org profile) go through stubs until rewritten; docs links untouched |
| SEO risk | high: 300+ indexed pages re-crawled and consolidated through meta refresh, which Google treats as a redirect but more slowly than a 301 | low, but the IA is incoherent: product pages sit under a family root | moderate on 22 pages, of which `/compare/apache-camel-vs-cameleer/` is the main search asset |
| Effort | 2–3 days plus a long tail | half a day | 1–1.5 days |
| IA coherence with `/simoom/` | full | poor | good: `/cameleer/` mirrors `/simoom/`, but docs are at `/docs/` rather than `/cameleer/docs/` |

**Recommendation: C.** Reasons:

1. **It moves the URLs where the IA matters.** The product landing pages are what a family root
   must not appear to own. A Cameleering root that serves `/compare/mulesoft-…-vs-cameleer/` at
   the top level reads as the family comparing itself.
2. **It leaves the 279 docs pages alone.** They are the bulk of indexed URLs. Their address is
   frozen into every published crate's `documentation` field, into `docs/conf.py`, the
   CHANGELOG, release notes and the README, and it is embedded 279 times in `llms-full.txt`,
   which AI tools cache. `/docs/` is unambiguous because simoom already uses `/simoom/docs/`, so
   no second project competes for the namespace.
3. **The mechanical cost of moving docs later is small and known:**
   - `build-docs.py` writes to `os.path.join(self.out, "docs")`, and `abs_url()` assumes
     `docs/`. Parameterize both as `DOCS_PREFIX = "cameleer/docs/"`.
   - Generate one stub per page from `self.by_out`, about 20 lines extending `write_moved`.
   - Retarget the 5 existing stubs so no chain forms.
   - Rewrite 273 relative links in designed pages.

   So moving docs is a reversible later phase (D). It is not a prerequisite. Do it only if a
   family-level `/docs/` hub is actually wanted.
4. **The `.md` twins can't redirect.** GitHub Pages serves them as text. Under A they would have
   to be duplicated at the old paths (they are not in the sitemap, so there is no duplicate-
   content risk) or allowed to 404. C avoids the question.

**Should the market-generic pages stay at the root?** These are
`/alternatives/mulesoft-alternatives/`, `/alternatives/mirth-connect-alternatives/` and
`/compare/enterprise-mcp-platforms/`. All three conclude with Cameleer and carry Cameleer
JSON-LD, so they move with the rest. If the IA research wants a family "compare" hub at the
root, it can link to them.

### 3.1 The `/cameleering/` pages

- `/cameleering/` → `/` (1 stub). The new root takes its content and its `DefinedTermSet` and
  `Organization` JSON-LD.
- **Subpages:**
  - Keeping `/cameleering/{ledger,method,finops,wirths-law,when-to-rewrite,ai-code-performance}/`
    costs nothing. The ledger is linked externally in 4 places, including `#simoom` anchors from
    the org profile and Docker Hub.
  - Moving them to top-level paths (`/ledger/`, `/finops/`, ...) means 6 more stubs and an
    anchor-preserving stub for `/cameleering/ledger/#simoom`.
  - My advice: keep them in phase 1, and decide with the IA report. The `/cameleering/` prefix
    under a Cameleering root is redundant but harmless.

## 4. Redirect mechanics on GitHub Pages

GitHub Pages has no server-side redirects: no `_redirects`, no 301s, and `jekyll-redirect-from`
is off because `.nojekyll` is set. The tools available are an HTML stub at the old path and the
404 page.

**The stub.** Extend today's `write_moved` stub, which has a canonical, `noindex`,
`meta refresh 0` and a link:

```html
<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>Apache Camel vs Cameleer (moved) | Cameleer</title>
<link rel="canonical" href="https://cameleer.ing/cameleer/compare/apache-camel-vs-cameleer/">
<meta name="robots" content="noindex">
<script>location.replace("https://cameleer.ing/cameleer/compare/apache-camel-vs-cameleer/"+location.search+location.hash)</script>
<meta http-equiv="refresh" content="0; url=https://cameleer.ing/cameleer/compare/apache-camel-vs-cameleer/">
</head><body><p>Moved to <a href="…">Apache Camel vs Cameleer</a>.</p></body></html>
```

How to write it:

- **Absolute target URLs.** Today's stubs use relative ones, which is fine, but absolute URLs are
  unambiguous for crawlers.
- **The JS line goes before the meta refresh.** It is what preserves `#fragments` and query
  strings; meta refresh drops the fragment. Without it, `/cameleering/ledger/#simoom` would lose
  `#simoom` if the ledger ever moves.
- **`location.replace`**, so the old URL does not stay in the back-button history.
- **Keep `noindex` and the canonical.** Google treats a 0-second meta refresh as a permanent
  redirect and follows the canonical.

**Generation, not hand-writing.** Put a single `REDIRECTS = {old_url_path: new_url_path}` table in
`build-docs.py`, or in a data file `site/_redirects.json` that the build reads, covering both
designed pages and docs. The build:

- writes a stub for every entry;
- **fails** if a real page exists at an old path;
- **fails** if a target does not exist;
- **fails** if a target is itself an old path, so chains can't form;
- folds the existing `MOVED` docs entries into the same table.

**The home anchors.** `/` gets new content, so it can't be a stub. Old links to `/#start`,
`/#bench`, `/#agents` and similar would land on the Cameleering root. No such link was found in
the repository or the llms files. To be safe, the new root carries a 3-line script mapping the
old home ids to `/cameleer/#id`; the old ids are `start`, `bench`, `numbers`, `agents`, `uses`,
`yaml`, `runner`, `same` and `cameleering`. The new root must not reuse those ids.

**Sitemap.**

- Old URLs leave the sitemap the day the stubs ship. Stubs are never listed, which is already the
  rule.
- New URLs go in.
- IndexNow: in the deploy after the move, also submit the old URLs once, so Bing and Yandex
  re-crawl and see the redirect. That is a one-off list in `pages.yml`, or a `--also` file.
- Google: no change-of-address is needed (same domain). Use Search Console's URL inspection on
  the top 5 pages.

**How long to keep stubs: forever.**

- They are about 500 bytes each, and 23 of them (or about 300 under A) are trivially cheap.
- The external references that can't be edited ensure there will always be traffic to the old
  URLs: published crate metadata, published images' `org.opencontainers.image.url`, release
  bodies, crates.io READMEs and backlinks.
- Review once a year whether any stub targets a page that has since moved again, and repoint it.

**404 page.** Add `site/404.html`; GitHub Pages serves it for any missing path, with a 404
status. It should:

- carry the Cameleering header;
- offer links to `/`, `/cameleer/`, `/docs/` and `/simoom/`;
- include a small JS prefix map as a safety net for anything the table missed, for example
  `^/(compare|use-cases|alternatives|mcp|trust|healthcare)/` → `/cameleer/$0`, keeping the hash;
- use absolute asset URLs (`/assets/site.css`), because the page renders at arbitrary depths.

## 5. Build system changes (`scripts/build-docs.py`, std-only Python)

### 5.1 One header and footer source for designed pages

Make the designed pages source plus partials, rendered by the build:

1. Move each page body to a source file, for example `site-src/cameleer/compare/…/index.html`,
   or keep it in place with markers. The source holds only `<main>…</main>`, a front-matter-like
   JSON block (title, description, canonical path, breadcrumb, JSON-LD, the CSS list, footer
   "related" links) and the nav key.
2. Partials, in `site/_partials/` or as Python string constants next to `TEMPLATE`, filled with
   `str.format` or `string.Template`:
   - `head.html`
   - `header-cameleering.html`
   - `header-cameleer.html`
   - `footer-cameleering.html`
   - `footer-cameleer.html`

   `string.Template` is safer, because `$name` does not collide with CSS braces.
3. The build computes `{prefix}` from the output depth (`relurl()` already exists), sets
   `aria-current` from the nav key, and writes `site/<path>/index.html`.

A less invasive alternative: keep the pages as hand-written files, but put the header and footer
between `<!-- partial:header-cameleer -->` … `<!-- /partial -->` markers. The build rewrites what
sits between the markers. This keeps `git diff` readable, needs no page restructuring, and
reduces the 31 copies to 2–3 sources. **I recommend the markers for this migration**, and full
templating only if pages keep multiplying.

The docs `TEMPLATE` header becomes the Cameleer microsite header from the same partial.

### 5.2 Nav for each microsite

| site | brand → | nav |
|---|---|---|
| Cameleering (root) | `/` | Practice (FinOps, Wirth's law, When to rewrite, AI code), Ledger, Method, Projects menu (Cameleer, simoom), GitHub org |
| Cameleer (`/cameleer/` and `/docs/`) | `/cameleer/` | Docs, Use cases, MCP, Packs/Healthcare, Trust, Compare, GitHub, and a small "Cameleering" family link, like simoom's |
| simoom | `/simoom/` | owned by the simoom repository: labels change there, `../` → `../cameleer/` for "Cameleer" |

Replace `designed_pages()`'s hard-coded directory tuple with a walk of `site/` that skips `docs/`,
`simoom/`, `assets/`, `brand/` and `_partials/`. That way a new page can't silently fall out of
the sitemap.

### 5.3 Sitemap index

Split into:

- `/sitemap.xml`, a `<sitemapindex>`
- `/sitemap-root.xml`, for the Cameleering pages
- `/cameleer/sitemap.xml`, for Cameleer designed pages plus `/docs/`
- `/simoom/sitemap.xml`, already produced by simoom's build

`robots.txt` keeps the single `Sitemap: https://cameleer.ing/sitemap.xml` line. IndexNow in
`pages.yml` must then read the child sitemaps, not `<loc>` from the index. The `for project in
("simoom",)` merge becomes a list of child sitemaps, and the build checks that each child exists.

### 5.4 llms.txt for each project

Split it into three files:

- `/llms.txt`: Cameleering. The practice, the ledger summary, and a "Projects" list linking
  `/cameleer/llms.txt` and `/simoom/llms.txt`.
- `/cameleer/llms.txt`: today's `site/llms.txt` content, URLs rewritten, with `write_llms()`
  writing its `## Documentation` section there.
- `/simoom/llms.txt`: unchanged.

Keep `/llms-full.txt` as Cameleer docs only, at the same URL, because `AGENTS.md`, `README.md` and
`docs/README.md` cite it. Alternatively, add `/cameleer/llms-full.txt` and keep the root one as a
copy; it can't be a stub, because it is plain text.

Two fixes along the way:

- Source and output are the same file today. Make the source `site-src/llms/cameleer.md` (or
  similar) so the build is idempotent.
- The root `/llms.txt` copy with CRLF line endings is hand-maintained. Have the build write it,
  or drop it if nothing serves it; GitHub Pages serves `site/` only.

### 5.5 Checks as build steps (std-only, fail the build)

- **Link checker over all of `site/`**, designed pages plus docs plus simoom:
  - Resolve every relative `href`/`src` against the file tree, including `dir/` →
    `dir/index.html`.
  - Check `#fragments` against the target's `id`s.
  - Fail on a missing target or on a link to a stub (no internal link should go through a
    redirect).
  - Fail on absolute `https://cameleer.ing/...` links in HTML whose path does not exist.
  - About 80 lines with `html.parser`.
  - Today `build-docs.py` checks docs-to-docs links only, and designed pages are unchecked.
- **JSON-LD validator:**
  - Parse every `application/ld+json` block with `json.loads`.
  - Require `@context`.
  - Check that `BreadcrumbList` items resolve to existing pages and that `url` and `@id` match
    the canonical.
  - Check that every `FAQPage` `Question` has an `acceptedAnswer`.
  - Require a canonical equal to the output path.
  - Require one H1.
  - Warn on a title over 65 characters or a description over 165.
- **Redirect table checks** (section 4), and a sitemap check: every listed URL resolves to a
  non-stub page, and every non-stub page is listed.

Run all of them in `pages.yml` after the build and before `upload-pages-artifact`, so a broken
link never deploys. Also run them in the normal CI job.

### 5.6 Rust tests

No Rust test reads `site/` or asserts a site path today. I checked `docs_harness.rs`,
`docs_reference.rs`, `docs_guides.rs` and `features.rs`: `features.rs` checks the
`**Cargo feature:**` lines in the markdown, and `docs_harness.rs` `is_external()` skips `https://`
links. Changes:

- **Add** a check in `docs_harness.rs`: every `https://cameleer.ing/<path>` in `docs/**/*.md`,
  `README.md`, `docker/README.dockerhub.md` and `AGENTS.md` must be a known current URL, read
  from the redirect table and the designed page list. A link to a redirected URL fails with the
  new address. This is what catches the README, docs and Docker Hub links after the move.
- `docs_harness.rs:223` and `:478` only mention `build-docs.py` in comments, so nothing changes
  there.

## 6. Migration sequence

Every phase is a single commit (or PR, given the protected `main`) that can be reverted on its
own. After every phase, run:

| step | what to run |
|---|---|
| build | `python3 scripts/build-docs.py` |
| checks | the link, JSON-LD and redirect checks (from phase 1 on) |
| docs tests | `cargo test -p cameleer-main --test docs_harness --test docs_reference --test docs_guides --test features` |
| local browser | `python3 -m http.server -d site` and the changed pages at 1280 px and 375 px: header, menu toggle, Cameleering submenu, footer |
| live, after the Pages deploy | `curl -sI` the new URLs (200); fetch 5 old URLs and confirm the stub body and canonical; check that the JS redirect preserves `#fragment` in a real browser; spot-check `sitemap.xml` |

**Phase 0: guards first (no URL changes).**

- Add the link checker, JSON-LD checker, redirect table and `404.html`.
- Add the `docs_harness` known-URL test against today's URLs, which must pass.
- Fix the weak links found by the checker.

Check: the build is green, and today's site is unchanged except for `404.html`. Rollback: revert.

**Phase 1: partials (no URL changes).**

- Introduce the header and footer markers.
- Render all 30 designed pages and the docs template from 2 sources.
- The diff must be whitespace or identical in rendered output: compare against `git show HEAD`
  with a normalizing diff script.

Check: the rendered nav is identical on all 31 pages. Rollback: revert.

**Phase 2: build `/cameleer/` alongside (additive).**

- Copy the home and the 22 section pages to `site/cameleer/…`, with the depth prefix rewritten
  and canonicals, `og:url`, `BreadcrumbList` and JSON-LD `url`s updated.
- Old pages stay live, with canonicals still pointing at themselves.
- Don't add the new pages to the sitemap yet; mark them `noindex` until phase 4.

Check: the link checker passes on both trees, and a browser pass at 1280/375 on `/cameleer/`.
Rollback: delete `site/cameleer/`.

**Phase 3: the Cameleering root.**

- Write the new `/index.html` (practice and FinOps first, with WebSite, Organization and
  DefinedTermSet JSON-LD) and the home-anchor shim.
- Add the Cameleering header and footer partials.
- Turn `/cameleering/` into a stub to `/`.

This phase is still unpublished. Do it on the branch and merge it together with phase 4.

**Phase 4: the cut-over (one deploy).**

- Replace the 22 old section pages with generated stubs, and lift `noindex` on `/cameleer/`.
- Switch the docs template brand and nav to the Cameleer partial.
- Write the sitemap index and child sitemaps.
- Split `llms.txt` and write `/cameleer/llms.txt`.
- Run IndexNow with the old URLs too.

Check: the full checklist plus live verification. Watch the Pages deploy log, then crawl the
whole live site once with the link checker against `https://cameleer.ing`.

Rollback: `git revert` the cut-over commit and redeploy (the Pages workflow runs on push to
`main`, about 2 minutes). The old URLs come back with their content, and the `/cameleer/` copies
stay harmless with `noindex`. **Only after a week of stable crawling**, delete the old page
sources so the stubs become the only thing at the old paths.

**Phase 5: external references** (only after phase 4 is live; doing them earlier would point at
404s):

- `README.md` (13 links), `docker/README.dockerhub.md` (the sync workflow pushes it)
- `docs/*.md` absolute links (about 10)
- `CITATION.cff`, the `Dockerfile` and `container.yml` OCI label
- `Cargo.toml` `homepage` → `https://cameleer.ing/cameleer/` (takes effect on the next crate
  release), `AGENTS.md`
- the org profile source `docs/design/brand/org-profile-README.md`, then the live org profile
  repository, with the owner's go-ahead

**Do not touch** `scripts/sbom.py` or the `ing.cameleer` reverse-DNS identifier.

**Phase 6: simoom** (in the simoom repository):

- In `scripts/build-site.py`, point "Cameleer" links at `../cameleer/` and "Cameleering" links at
  `../`.
- Rebuild, and copy into `site/simoom/` as usual, citing the simoom commit.
- `../assets/*` and the favicons keep working, because shared assets stay at the root.

Until this lands, simoom's "Cameleer" links show the Cameleering root and its "Cameleering" links
go through the `/cameleering/` stub. Nothing 404s.

**Phase D (optional, later): docs to `/cameleer/docs/`.**

- Parameterize `DOCS_PREFIX`, generate 279 stubs from `by_out`, and retarget the 5 existing
  `MOVED` stubs directly to their final paths.
- Rewrite the 273 designed-page links and the llms URLs.
- Keep `.md` copies at the old paths.

Same checks and rollback as phase 4. Only worth doing if a family `/docs/` hub is planned.

**Rollback in general:**

- Every phase is a revert of one commit.
- The stubs are generated from a single table, so a bad mapping is fixed by editing that table.
- Never delete a stub once deployed. Repoint it instead.
- Keep `site/cameleer/` behind `noindex` until the cut-over has been verified live, so a rollback
  never leaves two indexable copies.
