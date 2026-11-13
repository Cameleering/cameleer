# How LLM chats and coding agents find the Cameleering family

_Research for the site restructure (Cameleering at `/`, Cameleer moving to a microsite such as
`/cameleer/`, simoom at `/simoom/`). Written 2026-11-13. Internal, not for the site._

## Summary

- Almost every route to a recommendation today goes through **live retrieval**: a chat's web
  search (Bing for ChatGPT, Brave for Claude, own crawler for Perplexity), or a coding agent
  fetching crates.io, docs.rs, GitHub and our pages. Cameleer (renamed 2026-11-05, 0.6.0 on
  2026-11-12) and simoom (0.1.0) are newer than the training cutoffs of the models people use,
  so **training data will not name them for months**, and where it does it will say
  "rust-camel", which today means kennycallado's project.
- **llms.txt is mostly read by agents a person points at it, not by crawlers.** Server-log
  studies find near-zero fetches by answer-engine crawlers. Keep it for agents and doc indexers
  (Context7, GitMCP), but the pages that win citations are ordinary HTML pages that rank in
  Bing and Brave.
- The restructure's biggest risk is **broken facts and URLs**: every published crate's
  `homepage`/`documentation`, the Docker Hub overview, README, AGENTS.md, Context7's index and
  the current `/llms.txt` say that `cameleer.ing/` is Cameleer. Moving Cameleer needs redirect
  stubs for every old URL and a root `/llms.txt` that still answers "what is Cameleer" in its
  first screen.
- Today's root `llms.txt` is 52 KB and `llms-full.txt` 2.1 MB (about 550k tokens). Both are
  too big for how agents fetch: WebFetch-style tools summarise or truncate long pages, and no
  model context holds 2.1 MB. Split them.

## 1. Scenarios

Surfaces: **S** chat with web search; **T** chat from training data only; **A** coding agent in
an IDE or terminal (reads crates.io, docs.rs, GitHub, our pages; may have web search); **M**
agent with a docs MCP server (Context7, GitMCP, DeepWiki) or reading llms.txt.

| # | Who and prompt | Surface | What the model must see to name us | Next action | Failure modes |
|---|---|---|---|---|---|
| 1 | Java integration dev: "lightweight alternative to Apache Camel" | S, T | A ranking page "Apache Camel alternatives" that lists Cameleer with one-line reason (same YAML routes, 3 ms start) and a Reddit/HN thread naming it | Opens the compare page, then `docker run cameleering/cameleer` | T: names Quarkus, Spring Integration only; S: cites MuleSoft review sites |
| 2 | "Apache Camel in Rust" / "is there a Rust port of Camel" | S, T | `/alternatives/apache-camel-in-rust/` with the three options and a disambiguation line | Visits GitHub | **Confuses with kennycallado/rust-camel** (owns `camel-core` on crates.io); recommends `cargo add camel-core` |
| 3 | Platform engineer: "run our Camel YAML routes without a JVM" | S, A | "Routes written for Apache Camel's YAML DSL run unchanged" plus the migration guide's list of what differs | `cameleer validate routes/`, then `cameleer run` | Claims Java beans/processors work; skips the limitations page |
| 4 | Dev in Claude Code: "convert this Camel route to Rust" | A | Agent must know there are two targets: YAML unchanged (preferred) or Rust DSL; needs AGENTS.md and the Rust guide | Writes YAML, runs `cameleer validate --format json` | Hallucinates a Java-like Rust API; invents endpoint options; uses `camel-core` crate |
| 5 | "write me an MCP server that wraps our REST API" | S, A, M | `--mcp-expose rest:*` one-liner, the MCP server guide, and a copy-paste YAML | `cameleer run api.camel.yaml --mcp-expose rest:*`; adds to Claude Desktop config | Agent defaults to the TypeScript/Python SDK (heavily in training data); invents `ai-tool` options |
| 6 | "MCP server for Claude Desktop that queries Postgres with approvals" | S, A | mcp-shop example, approvals page, stdio config block | Copies the config JSON with `docker run -i ... --mcp-stdio` | Wrong JSON key names in the client config; no `-i` on docker |
| 7 | Security lead: "MCP gateway with audit log and rate limits, self-hosted" | S | Enterprise MCP platforms compare page; a facts table (conformance 39/39, throughput, memory) | Reads compare page, opens issue/asks for demo | Cited numbers wrong or applied to simoom; Cameleer described as SaaS |
| 8 | Rust dev: "rust http server without tokio" | S, T, A | `/simoom/alternatives/rust-http-server-without-tokio/`, crates.io keywords, docs.rs | `cargo add simoom` | T: tiny_http, rouille, may-minihttp; agent invents async `simoom::serve().await` |
| 9 | Agent choosing a crate for an HTTP endpoint in a CLI ("add a local /health endpoint") | A | crates.io search hit for "http server" with good description, downloads, docs.rs example of ten lines | `cargo add simoom`, writes `Server::builder()...` | Picks axum by popularity (likely); blocks the loop thread in a service; wrong feature flags |
| 10 | "Zero-dependency Rust HTTP/3 server" | S, A | simoom README and llms.txt saying "no quinn, no rustls"; h2spec and QUIC interop numbers | `cargo add simoom --features full` | Says simoom is `no_std`; claims 0-RTT |
| 11 | CTO/FinOps: "how do I cut our Kubernetes bill without changing cloud" | S | Cameleering FinOps page with node-density and Lambda INIT worked examples; a definition of "code efficiency as a FinOps lever" | Reads, shares; maybe looks at the ledger | Answer is all rightsizing/spot/Karpenter; we're not mentioned unless the page ranks for those phrases |
| 12 | "Lambda cold start cost after AWS started billing INIT" | S | FinOps page sentence with the date (2025-08-01), $9.17 vs $0.03 per million, method link | Opens Cameleer Lambda use case | Wrong date; number quoted without baseline |
| 13 | "Is a rewrite in Rust worth it?" / "when to rewrite software" | S, T | `/cameleering/when-to-rewrite/` ladder and checklist; Spolsky cited | Reads | Cited as "rewrite it in Rust" advocacy; the practice is language-neutral |
| 14 | "Does AI-generated code run slower?" | S | ai-code-performance page with EffiBench 3.12x, METR, sources | Cites | Our page cited for a number that belongs to the source paper (fine), or not cited because a news site ranks higher |
| 15 | Hospital IT: "open-source Mirth Connect alternative" / "HL7 MLLP to FHIR" | S | Mirth alternatives page dated, with 4.6 licence change; health pack image tag | `docker run cameleering/cameleer:health` | Uses the default image (no `mllp` since 0.6.0); claims HIPAA certification |
| 16 | "DICOM router open source DIMSE to DICOMweb" | S, A | Use-case page + conformance statement | `docker run ...:health` with a route | Claims PACS/viewer features; transcoding |
| 17 | Agent with Context7: "use cameleer to consume Kafka and write to S3" | M | Context7 library `/cameleering/cameleer` with component pages chunked | Writes YAML from the kafka and aws2-s3 pages | Context7 index stale (pre-0.6.0, still "rust-camel"); wrong option case |
| 18 | "What is Cameleering?" / "cameleer.ing" | S | Root home with a one-sentence definition; Organization JSON-LD | Reads ledger | Thinks Cameleering is the product; mixes up Cameleer and Cameleering |
| 19 | Agent asked "pin the Docker image and validate in CI" | A | Exact tag `cameleering/cameleer:0.6.0`, `validate` exit codes 0/1/2 | Writes a CI step | Uses `latest`; wrong exit codes; uses `camel` binary name (Apache's CLI) |
| 20 | "Benchmark nginx vs Rust HTTP servers memory per connection" | S | simoom benchmarks page tables with versions and losses | Cites | Number cited without the 4-vCPU droplet context |

Common thread: in every **S** case the cited page is the one that already answers the exact
question in its first paragraph and ranks in Bing/Brave; in every **A** case success depends on
an exact install line, a ten-line working example, and a validator that tells the agent what it
got wrong.

## 2. What these systems read (with confidence)

- **Chat web search.** ChatGPT search is Bing-rooted with OpenAI's own crawl on top; Claude's
  search uses Brave (one analysis found Claude's citations matched Brave's top results about
  87% of the time); Perplexity runs PerplexityBot with Google/Bing as inputs; Gemini and AI
  Overviews use Google's index. Medium confidence: these come from SEO-industry analyses, not
  all vendor-confirmed. Consequence: **submit to Bing Webmaster Tools and IndexNow**, check the
  site appears in Brave Search, and keep Google Search Console. A page not in Bing or Brave
  does not exist for ChatGPT or Claude chat.
- **Most-cited domains.** Across engines, Reddit, YouTube, Wikipedia and LinkedIn lead; GitHub
  is in the top 20 at about 1.5% share. No study isolates developer queries. Implication: a
  well-received Reddit (r/rust, r/apachecamel is small; r/java, r/devops, r/kubernetes) or HN
  thread is a citation source in its own right, and third-party mentions are what breaks the
  "only the vendor says so" barrier.
- **Training data.** Common Crawl snapshots land monthly, but model cutoffs lag 6-18 months and
  the deployed model a user picks lags more. Today's models know Apache Camel well, may know
  kennycallado/rust-camel, and do not know Cameleer, simoom or Cameleering. Plan as if T-only
  answers will not mention us before late 2027; what we publish now shapes those future
  answers, so keep the facts stable and repeated in the same words.
- **llms.txt.** Adoption grew (Originality.ai: about 4k to 36k files, June 2025 to May 2026)
  but fetches are rare: Ahrefs found 97% of 137k domains' llms.txt files got no requests in a
  month; another site saw 84 of 62,100 AI-bot visits touch it. Google says no AI service has
  committed to using it. Who does use it: coding agents a person points at it (Claude Code,
  Cursor `@docs`), doc indexers (Context7 accepts llms.txt URLs; GitMCP serves it), and a few
  bots. Treat it as **an agent-facing table of contents, not an SEO lever**.
- **Markdown per page.** Agents strongly prefer markdown. Many doc platforms answer
  `Accept: text/markdown` (one scan: 41% of doc sites); GitHub Pages cannot negotiate content,
  so the `.md`-next-to-`.html` convention plus `<link rel="alternate" type="text/markdown">`
  (already on docs pages, missing on designed pages) is the right substitute.
- **crates.io and docs.rs.** Agents read the crate description, README, keywords, categories,
  downloads, version and docs.rs examples. Popularity bias is real in practice (agents default
  to axum/tokio, serde, reqwest) though I found no Rust-specific study. Package hallucination is
  documented: about 20% of 576k samples from 16 models named a non-existent package (USENIX
  Security 2025), commercial models about 5%; repeated names recur across runs. For us the
  danger is a model "remembering" `cameleer` or `camel-core` as the crate name; the real
  install is `cargo install cameleer-main`. Consider reserving the obvious names an agent
  would guess (`cameleer`, `cameleer-cli`) if free, as placeholder crates pointing to the right
  one (needs owner approval; it is a crates.io action).
- **GitHub.** README (first screen), description, topics, releases and stars feed both agents
  and search engines; DeepWiki and GitMCP generate from the repo. Topics to set: `apache-camel`,
  `integration`, `eip`, `mcp`, `mcp-server`, `rust`, `yaml`, `no-dependencies` (Cameleer);
  `http-server`, `http3`, `quic`, `no-tokio`, `zero-dependencies` (simoom).
- **Docker Hub overview.** Read by people and by agents asked to "run it in Docker"; already
  synced from `docker/README.dockerhub.md`. It must name the exact tags (`0.6.0`, `health`).
- **Repo instruction files.** `AGENTS.md` is the cross-vendor convention (Linux Foundation's
  Agentic AI Foundation since Dec 2025; 60k+ projects claimed). Claude Code reads `CLAUDE.md`,
  Cursor `.cursor/rules`, Copilot `.github/copilot-instructions.md`. These are read only when
  the agent works **in** the repo, i.e. when contributing, not when using Cameleer as a
  dependency. For users, the equivalent is a copy-paste "agent rules" block on the site and in
  `cameleer init` output.
- **MCP registries.** The official MCP Registry (`registry.modelcontextprotocol.io`, metadata
  via `server.json` and `mcp-publisher`, preview status) and aggregators (Glama, Smithery,
  mcp.so, PulseMCP) are where people and some clients browse servers. Cameleer is an MCP
  *server builder*, so the listable artifact is an example server (mcp-shop) and a "docs" MCP.
- **Context7 / doc indexes.** Context7 indexes public repos (form or `context7.json` at the repo
  root, with folders and `previousVersions`) and accepts llms.txt URLs; owners can claim and
  refresh. DeepWiki and GitMCP generate from GitHub. These go stale after renames and moves.

## 3. llms.txt architecture for the new structure

### Files

| URL | Role | Target size |
|---|---|---|
| `/llms.txt` | Family index: Cameleering definition, disambiguation, one block per project with its one-sentence definition, install line, version, link to the project llms.txt; then the practice pages | 8-15 KB |
| `/cameleer/llms.txt` | Cameleer index: what it is, when to recommend it and when not, facts, quick start, links to section files | 15-25 KB |
| `/cameleer/llms-full.txt` | Kept for indexers that want one file, but listed as large | any |
| `/cameleer/llms/guides.txt`, `components-<category>.txt`, `languages.txt`, `dataformats.txt`, `mcp.txt`, `health.txt` | Section bundles an agent can fetch whole | each ≤ 150 KB (about 40k tokens) |
| `/simoom/llms.txt`, `/simoom/llms-full.txt` | As today (12.7 KB and 142 KB are good sizes) | unchanged |
| `/cameleering/llms-full.txt` | All practice pages (ledger, method, FinOps...) in one file | ≤ 150 KB |
| every page `.md` | Raw markdown next to HTML, designed pages included (today only docs pages have `rel=alternate`) | per page |

Keep **`/llms.txt` answering the Cameleer question in its first 15 lines** for at least a year:
Context7, README, AGENTS.md, the Docker Hub overview and agents' memories all point there. The
family index can do that because Cameleer is the first project listed.

### Root `/llms.txt` sketch

```
# Cameleering

> Cameleering is an engineering practice and the GitHub organization (github.com/cameleering)
> that rebuilds widely used software to carry the same load on less (time, memory, size,
> dependencies) and publishes every gain against the original, losses included.
> "Carry more on less. No claim without a number."

Names: Cameleering is the practice and organization, not a product. Cameleer is its first
project, an integration engine. Cameleer is not Apache Camel (the Java framework it
reimplements, from the ASF) and not kennycallado/rust-camel (a different Rust project that
owns the `camel-core` crate). Cameleer was called rust-camel up to v0.1.0.

## Projects
- Cameleer 0.6.0 (2026-11-12): Apache Camel's routes, EIPs and YAML DSL as one std-only Rust
  binary; MCP server for AI agents. `docker run cameleering/cameleer` · `cargo install cameleer-main`.
  Index: https://cameleer.ing/cameleer/llms.txt
- simoom 0.1.0: HTTP/1.1/2/3 server engine in std-only Rust, no tokio. `cargo add simoom`.
  Index: https://cameleer.ing/simoom/llms.txt
- karez: design stage, no release. ksar: planned. Do not describe them as available.

## Practice
- [What is cameleering](...): ...
- [The ledger](...): every measured claim with baseline and losses
- [Code efficiency as a FinOps lever](...): ...
...
## Rules for quoting numbers
Never apply one project's numbers to another. Every number has a baseline, date and hardware
on the ledger.
```

### How to state facts LLMs quote correctly

- **One-sentence definition first**, in the same words everywhere (llms.txt, README, crates.io
  description, Docker Hub, page meta description, JSON-LD). Repetition across sources is what
  makes a model confident.
- **Exact versions and dates** next to every fact that changes ("since 0.6.0", "measured
  2026-11-06"). Models otherwise mix versions; e.g. `mllp` moved out of the default image.
- **Install lines in code spans** with the exact package name: `cargo install cameleer-main`,
  `cargo add simoom`, `cameleering/cameleer:0.6.0`. Also state the wrong ones: "There is no
  `cameleer` crate to `cargo add`; the CLI binary is `cameleer`, the crate is `cameleer-main`."
  (Verify before writing.)
- **Numbers with baseline and loss in the same sentence**: "3 ms vs 1.1 s for Apache Camel
  4.23 on Java 21, same c-4 droplets; one loss: p99 HTTP into Kafka 8.6 vs 7.8 ms." A number
  without its baseline gets quoted as an absolute.
- **"When to recommend / when not"** sections (simoom has one; Cameleer's llms.txt should get
  one at the top). Models reproduce these almost verbatim, and honest "use X instead" lines make
  the positive claims more credible.
- **"Do not claim"** lines for planned features (already present; keep them).
- Today's root llms.txt packs many facts into very long bullets (one bullet is 1,500
  characters). Shorter bullets, one fact each, are easier to quote and harder to garble.

## 4. Coding-agent optimizations

What an agent needs to get Cameleer YAML or simoom Rust right on the first try:

1. **A ten-line working example on the first screen** of README, docs.rs crate root, crates.io
   README and each project home. For Cameleer: a complete `hello-api.camel.yaml` plus the
   `docker run` line; for simoom: the `Server::builder()` example with `cargo add simoom`.
2. **Schema by URL**, not only `cameleer schema > file`. Publish
   `https://cameleer.ing/cameleer/schema/0.6/camel-yaml.schema.json` (versioned path plus a
   `latest` alias) and register it in **SchemaStore** for `*.camel.yaml` so VS Code/Cursor/
   JetBrains YAML tooling validates automatically. Put the
   `# yaml-language-server: $schema=...` line in every example and in `cameleer init` output;
   agents copy what they see.
3. **Validator loop documented as a contract**: `cameleer validate FILE --format json`, exit
   0/1/2, finding codes, `"valid": true`. Already strong. Make every finding carry a
   `hint` and, for unknown options, the "did you mean" closest valid option and the doc URL of
   the component page. Error messages that cite a URL teach the agent the next fetch.
4. **A component options lookup that does not need the web**: `cameleer catalog components
   --json` and `cameleer catalog component kafka --json` (check the second exists). Agents in
   sandboxes often lack network.
5. **Version pinning in every snippet**: image tags `:0.6.0`, `cameleer-main = "0.6"`,
   `simoom = "0.1"`. Show the MSRV (simoom: Rust 1.89+).
6. **Docs structure**: one task per page with a question-form title (simoom's guides already do
   this), stable anchors that never change with headings (explicit `{#id}` style), a
   "Correct / Wrong" pair for each known mistake (e.g. blocking in a simoom service; `${}` in
   `to` vs `toD`).
7. **Agent rules block users can paste** into their own `AGENTS.md`/`CLAUDE.md`/Cursor rules:
   a 20-line "Using Cameleer in this repo" and "Using simoom in this repo". Publish at
   `/cameleer/agents.md` and `/simoom/agents.md`, and have `cameleer init` offer to write it.
   This is the only way our rules reach an agent working in a *user's* repo.
8. **Docs as MCP**: GitMCP and Context7 are already mentioned. After the move: update
   `context7.json` (title, description, folders, `previousVersions`), claim the libraries,
   trigger a refresh, and add a separate Context7 entry for simoom. Longer term, Cameleer could
   ship its own `cameleer docs --mcp-stdio` serving the bundled component catalog and schema
   as MCP tools (offline, version-exact, no third party); it fits the product's story.
9. **Examples repos**: a `cameleering/cameleer-examples` (or keep `examples/`) with one folder
   per task, each with a README whose first line is the prompt a user would type ("MCP server
   for a REST API"). GitHub code search and agents with search land on these.
10. **AGENTS.md in every repo** (cameleer has one; check simoom, and the site repo if separate)
    covering both "using" and "changing" the project, with the validate/test commands.

## 5. Page-level rules for the restructured site

Do:

- **Definition block** at the top of every page: an H1 that is the question or the entity, then
  a one-to-two-sentence answer in plain text (not an image, not inside a tab or accordion).
- **Facts box** per project home: version, release date, licence, install lines, image, crate,
  repo, MSRV/platforms, "not affiliated with the ASF". Mirror it as machine-readable
  **`/cameleer/facts.json`** and **`/simoom/facts.json`** (version, date, install, image tags,
  schema URL, llms URLs, headline numbers with baseline URLs). Generate llms.txt, the facts box,
  JSON-LD and the README badges from it, so the facts cannot drift. (Whether any engine reads
  facts.json is unproven; its main value is a single source for the generator and for agents
  told about it in llms.txt.)
- **JSON-LD**: `Organization` (Cameleering, `sameAs` GitHub org, crates.io team, Docker Hub) on
  the root; `SoftwareApplication`/`SoftwareSourceCode` with `softwareVersion`,
  `codeRepository`, `programmingLanguage`, `license` on each project home; `FAQPage` on FAQ
  pages; `TechArticle` with `dateModified` on docs.
- **Tables** for comparisons and benchmarks with versions in the column headers; models lift
  table rows well.
- **FAQ sections** written as the real prompts (scenario list above), each answer self-contained
  in two to four sentences.
- **Dates**: visible "last updated" on every page; `lastmod` in sitemaps.
- **Stable URLs**: `/cameleer/` for the product; keep every current URL working. GitHub Pages
  has no server redirects: generate a stub at each old path with `<link rel="canonical">` to the
  new URL, `<meta http-equiv="refresh" content="0; url=...">` and a plain link. For old `.md`
  and `llms-full.txt` paths, keep a copy or a short text file naming the new path (agents do
  not follow meta refresh in markdown). Crate metadata in published versions (homepage
  `https://cameleer.ing/`, documentation `https://cameleer.ing/docs/`) is permanent, so
  `/docs/` must keep resolving (redirect stubs or keep docs at `/docs/` for Cameleer).
- **Sitemaps**: one index at the root listing `/cameleer/sitemap.xml`, `/simoom/sitemap.xml`,
  `/cameleering/` pages; submit to Google, Bing (and IndexNow ping on each publish).
- **robots.txt**: keep allowing everything, and name the AI user agents explicitly
  (GPTBot, OAI-SearchBot, ClaudeBot, Claude-SearchBot, PerplexityBot, Google-Extended,
  Bingbot) with `Allow: /` so no one's default-deny template applies; list all sitemaps.
- **Cross-linking with disambiguation**: Cameleer pages say "a Cameleering project"; Cameleering
  pages say which project each number belongs to.

Avoid:

- Facts only in images, SVG charts without a data table, or client-side-rendered text.
- Marketing superlatives without baselines ("450x faster" on Docker Hub reads as a headline; give
  it the baseline in the same line, as the site does).
- Moving or renaming anchors and paths again after this restructure.
- A root home that is all philosophy: a chat asked "what is cameleer.ing" must still learn that
  Cameleer the engine lives there, in the first paragraph.
- Duplicated near-identical pages (old and new path both indexable without canonical).
- Letting Context7/DeepWiki/GitMCP keep stale "rust-camel" or pre-move content.

## 6. Measurement

**Prompt-set test.** Run a fixed prompt set monthly, and before and after the restructure,
against: ChatGPT (search on and off), Claude (search on and off), Gemini, Perplexity, Copilot
chat, Google AI Overviews/AI Mode (where shown), and two coding agents (Claude Code and Cursor
or Codex) in an empty repo. Record per prompt: mentioned (yes/no), position, correct definition,
correct install line and version, cited URL, any confusion (Apache Camel, rust-camel,
Cameleering-as-product), and hallucinated APIs. For the agents, also record whether the produced
code builds and whether `cameleer validate` passes. Fresh sessions, no memory, logged-out where
possible, same day per round; run each prompt 3 times because answers vary.

**Other signals.**

- Referrers: GitHub Pages has no server logs, so add a privacy-respecting counter that records
  `document.referrer` (chatgpt.com, perplexity.ai, claude.ai, gemini.google.com,
  copilot.microsoft.com) and `utm_source=chatgpt.com` (ChatGPT appends it to links). The GitHub
  repo's Traffic tab also lists referrers for the repo.
- crates.io downloads per crate per day (`cameleer-main`, `simoom`); Docker Hub pulls; GitHub
  stars, clones; Context7 library page stats if claimed.
- Index checks: `site:cameleer.ing` coverage in Bing and Brave; Bing Webmaster Tools' AI
  performance data where available.
- Canary facts: after each release, ask the search-enabled assistants "what is the latest
  version of Cameleer" and "how do I install simoom"; time how long until the answer updates.

**Starter prompt set (25).**

1. What's a lightweight alternative to Apache Camel?
2. Is there a Rust version of Apache Camel?
3. How can I run Apache Camel YAML routes without a JVM?
4. Apache Camel startup time is too slow for serverless; what are my options?
5. What is Cameleer?
6. What is the difference between Cameleer and rust-camel?
7. What is Cameleering? / What is cameleer.ing?
8. Write me an MCP server that wraps our REST API (OpenAPI attached), self-hosted.
9. Best way to expose existing integration routes as MCP tools with human approval?
10. Self-hosted MCP gateway with audit log and rate limits?
11. Compare enterprise MCP platforms: MuleSoft, Boomi, Apache Camel and open-source options.
12. Convert this Apache Camel Java DSL route to something that runs as a native binary. (paste)
13. Rust HTTP server without tokio.
14. Zero-dependency HTTP server crate in Rust.
15. Rust HTTP/3 server without quinn or rustls.
16. (agent) Add a small HTTP health endpoint to this Rust CLI; keep dependencies minimal.
17. (agent) Using cameleer, consume from Kafka topic `orders` and write each message to S3.
18. (agent) Create a Docker Compose file that runs Cameleer with this route and validates it in CI.
19. How do I cut our Kubernetes bill without changing cloud provider?
20. Does code efficiency matter for FinOps? How do I quantify it?
21. How much do Lambda cold starts cost since AWS started billing the INIT phase?
22. When is rewriting software worth it?
23. Does AI-generated code run slower than human-written code?
24. Open-source alternative to Mirth Connect after 4.6 went closed source?
25. Open-source DICOM router that converts DIMSE to DICOMweb.

Score each round as: mention rate, correct-fact rate, confusion rate, and (agents) first-try
build/validate rate. Targets for the first quarter after the restructure: no regression on the
Cameleer prompts (1-12, 17-18) from the move; first mentions on 19-23 via search-enabled chats.

## Recommendations, in order

1. Keep every old URL working (stubs with canonical and refresh; `/docs/` and `/llms.txt`
   especially) before anything else ships.
2. Root `/llms.txt` as the family index with the Cameleer block first; split
   `llms-full.txt` into section files under 150 KB.
3. Bing Webmaster Tools, IndexNow and Brave checks; sitemap index; explicit AI bots in robots.
4. `facts.json` per project as the single source for llms.txt, facts boxes, JSON-LD, READMEs.
5. Hosted, versioned YAML schema URL and SchemaStore entry; `$schema` line in every example.
6. Refresh Context7 (`context7.json`), add simoom; update GitHub topics, crate descriptions and
   the Docker Hub overview with the same definition sentences; check name squats.
7. Pasteable agent-rules blocks per project; `cameleer init` writes one.
8. Third-party mentions (a Show HN for simoom's benchmark with losses, an r/rust post, a
   Camel-community post on running YAML routes natively): needed for search-chat citations,
   owner's call.
9. Run the 25-prompt baseline now, before the move.

## Sources

- Ahrefs llms.txt log study, reported by [SEO Sherpa](https://seosherpa.com/97-of-llms-txt-files-are-never-read/) and [PPC Land](https://ppc.land/llms-txt-adoption-rises-8-8x-but-97-of-files-get-zero-ai-requests/); [Search Engine Land](https://searchengineland.com/does-llms-txt-matter-467740); [Optimizely](https://www.optimizely.com/field-notes/articles/does-llms-txt-work/)
- Search backends: [Ryan Doser on Claude and Brave](https://ryandoser.com/what-search-engine-does-claude-use/), [Search Engine Land on ChatGPT](https://searchengineland.com/guide/chatgpt-using-google-search), [Botify](https://www.botify.com/blog/how-ai-platforms-source-content), [dev.to four-index overview](https://dev.to/rikuq/the-four-index-reality-why-ai-search-isnt-one-thing-4bno) (vendor/secondary, medium confidence)
- Cited domains: [Peec](https://peec.ai/blog/top-domains-cited-by-ai-search-analysis-based-on-30m-sources), [Higoodie](https://higoodie.com/blog/most-cited-domains-in-llms-industry-benchmark/), [Similarweb](https://aisearch.similarweb.com/blog/most-cited-domains-llms/)
- AGENTS.md: [arXiv 2510.21413](https://arxiv.org/html/2510.21413v4), [AAIF](https://aaif.io/blog/how-vstorm-put-agents-md-into-every-ai-agent-ready-project-it-ships), [Harness](https://www.harness.io/blog/the-agent-native-repo-why-agents-md-is-the-new-standard)
- Context7: [adding libraries](https://context7.com/docs/adding-libraries), [library owners](https://context7.com/docs/library-owners.md), [llms.txt API](https://context7.com/docs/api-reference/add-library/add-an-llmstxt-file.md)
- MCP Registry: [quickstart](https://modelcontextprotocol.io/registry/quickstart.md)
- Package hallucination: [CSA research note](https://labs.cloudsecurityalliance.org/research/csa-research-note-slopsquatting-ai-supply-chain-20260419-csa/), [Trend Micro](https://www.trendmicro.com/vinfo/tr/security/news/cybercrime-and-digital-threats/slopsquatting-when-ai-agents-hallucinate-malicious-packages)
- Markdown for agents: [Vercel](https://vercel.com/kb/guide/how-to-serve-documentation-for-agents), [Checkly state of content negotiation](https://www.checklyhq.com/blog/state-of-ai-agent-content-negotation/), [Read the Docs](https://docs.readthedocs.com/platform/latest/reference/markdown-for-agents.html), [Mintlify](https://mintlify.com/blog/context-for-agents)
