# Website and docs plan

_Written 2026-11-06 against Cameleer 0.4.0. This is a strategy document: it plans for the
product as it will be once [docs/ROADMAP.md](ROADMAP.md) ships, and marks what is shipped
(✅) and what is on the roadmap (🚧). The live site, the README and `llms.txt` follow a
stricter rule: every claim there is true of the code today, and roadmap items appear only
where they are labelled as planned and link to the roadmap._

## 1. Summary

Cameleer has three things worth saying, and the site said all three at once with equal weight:

1. It runs Apache Camel routes unchanged, without the JVM.
2. It starts in 3 ms, runs in megabytes and moves 1.7–8.7x the messages of Apache Camel.
3. It turns an AI agent's decisions into reliable actions: validated, idempotent, retried,
   rolled back, audited.

The recommended order is **1 → 3 → 2**: lead with what it is (Camel routes in Rust), say what
is new (reliable actions for AI agents), and let the numbers prove the "in Rust" part. The
performance numbers are the best-evidenced claims the project has, and they stay in the hero,
but they argue for a decision; they are not the decision.

**Verdict on "the action layer for AI agents"** (section 3): agree with the direction, refine
the wording. "Action" is the right verb for the agent audience, and once the roadmap's agent
tool server (MCP), agent EIP and tool calls in every model component ship, "action layer" is a
category Cameleer can own with a distinct claim: *reliability* (every action runs once, validated,
audited, rolled back) rather than *connector count* (Composio, viaSocket and Paragon sell
"action layers" as hosted catalogs of 500–7,000 connectors with managed OAuth). Until the tool
server ships, the live site uses the phrase as a section title and a claim about what a route
does for an agent ("reliable actions for AI agents"), not as the one-line identity of the
product, because the first thing an agent builder looks for is MCP, and the honest answer today
is "HTTP tools now, MCP planned".

## 2. Findings

### 2.1 Positioning and SEO of the current site

- **Three messages with equal weight in the hero.** H1 "Integration routes. ~~On the JVM.~~ In
  Rust.", a lede about running Camel routes, then a bordered "integration layer for AI agents"
  paragraph. Each is fine; stacked, none wins. The agent line reads as an afterthought.
- **The `<title>` undersells.** "Cameleer: Camel-compatible integration routes in Rust" has no
  "Apache Camel" (the phrase people search), no agents, no serverless. Meta description is 330
  characters and gets cut at about 155.
- **JSON-LD says `"version": "0.1.0"`.** The release is 0.4.0. `alternateName: rust-camel` is
  good and stays.
- **Two `llms.txt` files disagree.** `site/llms.txt` is current; the repository-root `llms.txt`
  (the one gitmcp.io serves to coding agents) still says the Lambda runtime is "not yet
  packaged" and quotes the old "2.5-8x" figure.
- **Primary CTA is "View on GitHub".** GitHub is a reference, not a conversion. The product's
  conversion is running a route (Docker one-liner) or reading the agent patterns.
- **Navigation has no entry for agents or use cases.** "How it works" and "Use cases" are
  anchors to sections, and the use-case section ("Small enough to go where the JVM can't") is
  about footprint, not use cases.
- **The compare and alternatives pages are strong** (structured data, FAQ markup, measured
  numbers, honest "when to stay on Camel"). The alternatives table misses the tools people
  actually weigh against Camel today for pipelines (Redpanda Connect/Benthos) and for automation
  (n8n, Zapier), and the agent page does not mention that Apache Camel 4.22 ships its own
  `camel-ai-tool` and `camel-mcp-server`, which a reader will find within minutes.
- **The brand mark PNG is 137 KB for a 32-px icon**, loaded on every page.
- **The H1's struck-through "On the JVM." is read aloud as part of the heading** ("Integration
  routes. On the JVM. In Rust."), which inverts the meaning for screen-reader users.

### 2.2 Feature coverage (site and README against the code)

Checked against `docs/FEATURES.md`, `docs/components/`, `docs/guides/`, `examples/` and
`CHANGELOG.md`.

Claimed and supported (keep): about 230 components, languages and data formats (157 + 26 + 29
pages, all of Camel's EIPs), YAML/XML/Rust DSLs, REST DSL with OpenAPI 3, Kamelets, route
templates, `cameleer` CLI (`run`, `init`, `export`, `ps`, `stop`, `get`, `trace`, `validate`,
`schema`, `catalog`), AWS Lambda custom runtime (`provided.al2023`, `scripts/package-lambda.sh`,
`crates/cameleer-main/tests/lambda.rs`), build features (`core` 5.8 MB), four model components
(`openai`, `aws-bedrock`, `azure-openai`, `google-vertexai`), the agent patterns (eight tested
routes in `examples/agents/`), the edge patterns (five in `examples/edge/`), health checks,
metrics, tracing, hot reload in dev mode, Docker image on distroless for amd64 and arm64.

Capabilities the site did not show, all present in the code and documented:

| capability | evidence | where it now appears |
|---|---|---|
| HL7 v2 over MLLP, HL7 terser, FHIR | `mllp`, `fhir` components, `hl7`/`fhir` data formats, `examples/edge/hl7-mllp-to-http.camel.yaml` | use case: healthcare |
| MQTT 3.1.1 and 5.0, store and forward, last will, syslog | `paho`, `paho-mqtt5`, `syslog`, `examples/edge/*` | use case: edge and IoT |
| File, FTP, FTPS, SFTP pipelines with CSV, Bindy, uniVocity, zip, PGP | `file`, `ftp`, `sftp` components, data formats, `examples/files-to-s3.camel.yaml`, CSV batch benchmark | use case: files and SFTP |
| Change data capture | `aws2-ddbstream`, `mongodb` change streams, `azure-cosmosdb` change feed, `sql` polling with `onConsume` | use-case hub (data pipelines card) |
| SaaS webhooks and chat | `webhook` (GitHub, Telegram), `slack`, `github`, `jira`, `twilio`, `examples/alerts-to-slack.camel.yaml` | use-case hub |
| Scheduled jobs | `cron`, `quartz`, `scheduler`, `timer`; skeleton in `yaml-for-coding-agents.md` | use-case hub |
| Human approval of agent actions | `examples/agents/human-approval.camel.yaml` | agents section, use-case hub |
| RAG ingestion, summarization, output guards, multi-model fallback | `examples/agents/*` | agents section, use-case hub |
| `cameleer validate` and `schema` for coding agents | `docs/guides/validate.md`, `AGENTS.md` | home (agents band), llms.txt |
| REST APIs with OpenAPI, CORS, JSON binding | `rest` component, `examples/rest-api.camel.yaml` | use-case hub (APIs card) |

Claims to avoid or qualify (and how the live copy handles them):

- **MCP.** No MCP server exists (`grep -ri mcp crates/*/src` finds nothing). Tools are HTTP
  endpoints, which every agent framework supports. The site says so and links to the roadmap.
- **Vector stores.** No dedicated vector components; pgvector through `sql`, OpenSearch/Elasticsearch
  through `elasticsearch`, others through `http`. Phrased as "the stores you already run".
- **Tool calls in `openai` and `aws-bedrock`.** Only `azure-openai` returns tool calls today.
  The site does not claim an agent loop.
- **"API gateway".** Cameleer has the REST DSL, CORS, OpenAPI, throttle and circuit breaker, but
  no built-in JWT/OAuth validation. The hub calls it "REST APIs and backends for frontends",
  not a gateway.
- **CDC.** No Debezium or PostgreSQL logical replication. The hub names the three stream
  sources and polling, nothing more.
- **Windows and macOS CLI**: true (tests pass on Windows 11); the image is Linux only.

### 2.3 Performance numbers

All numbers come from `benchmarks/results/2026-11-06-linux/summary.json` and
`docs/BENCHMARKS.md`; none were changed, and rounding goes against Cameleer (startup 2.45 ms is
shown as 3 ms; peak memory 7.37 MB as 8 MB; 451x as 450x).

- Headlines: **450x faster startup (3 ms vs 1.1 s)**, **up to 54x less memory (8 MB vs 399 MB on
  the same route)**, **1.7–8.7x the throughput in every scenario**. These stay in the hero facts
  and on every compare page, with the machine details only in "How we tested".
- Presentation changes: the home-page runtime toggle gains a third side, **Apache Camel on
  Quarkus native** (30 ms, 113 MB on the same route), because that is the comparison an informed
  Camel user asks for first; the bar chart's labels name the scenario in the words a buyer uses
  (REST API, HTTP into PostgreSQL, order ingestion into Kafka, Kafka stream processing, CSV batch);
  the README keeps its text chart.
- Not done on purpose: no claims about HTTP being a ceiling (BENCHMARKS.md says it is not), no
  per-core figures in headlines, no latency headline (HTTP into Kafka p99 is slightly higher than
  Camel's).

### 2.4 Use cases the software supports and the site did not show

Verified against `examples/`, `docs/guides/` and `docs/components/` (table in 2.2). New pages
were justified where (a) a tested example exists, (b) the keyword has a distinct audience, and
(c) no existing guide already is the landing page:

| use case | page | status |
|---|---|---|
| AI agent tools (validation, idempotency, saga, approval, audit) | existing: compare page + agents guide; hub card | ✅ shipped; 🚧 MCP server |
| LLM pipelines and RAG ingestion | agents guide; hub card | ✅ |
| Healthcare: HL7 v2 over MLLP, FHIR | **new** `use-cases/hl7-mllp-healthcare-integration/` | ✅ |
| Edge and IoT: MQTT gateways, store and forward | **new** `use-cases/mqtt-iot-edge-gateway/` | ✅ |
| Serverless: AWS Lambda, Cloud Run, scale to zero | **new** `use-cases/serverless-aws-lambda/` | ✅ |
| File, FTP and SFTP pipelines, CSV and EDI-style batches | **new** `use-cases/file-sftp-pipelines/` | ✅ |
| Migrating Camel routes off the JVM | existing guide + compare page; hub card | ✅ |
| Change data capture | hub card (three stream sources + polling) | ✅ partial |
| SaaS webhooks to chat and tickets | hub card | ✅ |
| Scheduled jobs | hub card | ✅ |
| REST APIs and backends for frontends | hub card | ✅ (no auth gateway) |
| Human approval of agent actions | agents section, hub card | ✅ |

### 2.5 Search friendliness (crawlers, humans, LLMs)

- Titles and descriptions rewritten to carry the primary keyword first and stay under 60/155
  characters (home, compare hub, new pages). Docs titles are generated and already follow the
  "question → answer" convention.
- Internal linking: a `Use cases` entry in every header, hub pages that link down and pages that
  link across (healthcare ↔ edge, serverless ↔ build features, agents ↔ tool-calls compare).
- Structured data: `SoftwareSourceCode` on the home page updated (version 0.4.0, `sameAs` for
  crates.io and Docker Hub); each new page has `TechArticle` + `BreadcrumbList` + `FAQPage`; the
  hubs are `CollectionPage`. The docs generator already emits these for every doc page.
- `llms.txt`: one-paragraph identity, key facts, quick start, comparisons, **use cases**, the
  generated documentation section, and an explicit "planned, not shipped" line, so an LLM never
  answers "Cameleer has an MCP server".
- Page speed: a 64-px brand mark replaces the 137 KB PNG in headers; everything else is already
  system-font-fallback, one CSS file, no framework.
- Accessibility: H1 gets an `aria-label` that reads the intended sentence; the runtime toggle is
  a `role=group` with `aria-pressed`; colour contrast of the muted text (#5b6479 on white) is
  5.9:1, fine; focus styles exist.
- `sitemap.xml`: generated; `scripts/build-docs.py` now also includes `site/use-cases/**`.

### 2.6 The competitive space

Keywords to cover, what each competitor owns, and where Cameleer wins. Sources at the end.

| competitor | what they own (keywords) | where Cameleer wins | where it does not |
|---|---|---|---|
| **Apache Camel (JVM)** | "enterprise integration patterns", "camel route", 300+ components, Java DSL, Spring Boot, commercial support | same routes, no JVM: 450x startup, up to 54x memory, 1.7–8.7x throughput, 20 MB image, Lambda runtime | ecosystem size, Java beans, long-tail connectors, two decades of production use |
| **Camel on Quarkus native** | "camel native image", "camel quarkus startup" | measured: faster start (3 ms vs 30 ms), less memory (8 vs 113 MB), higher throughput (native Camel has no JIT) | it is still Camel: every component, Java code |
| **Camel K / JBang** | "camel k kubernetes", "camel jbang run" | the same `run`/`ps`/`stop` workflow as one binary, no operator or JDK | Kubernetes operator, Kamelet catalog size |
| **Camel 4.22 AI** (`camel-ai-tool`, `camel-mcp-server`, `langchain4j-agent`) | "camel mcp server", "camel ai agent" | 🚧 the roadmap's tool server closes this; today: tested reliability patterns (saga, idempotency, approval) as routes | Camel ships MCP today (Main/JBang, Quarkus, Spring Boot); Cameleer does not |
| **Spring Integration** | "spring integration vs camel" | not a Spring shop's choice either way; wins when integration is the product, not a library in an app | Spring Boot applications |
| **MuleSoft / WSO2 / iPaaS** | "mulesoft alternative", "agent fabric", "agent control plane", API management | open source, self-hosted, routes in Git, no per-core licence; sidecar-sized | visual designers, support contracts, API management suites |
| **Apache NiFi** | "dataflow", visual canvas, provenance | code in Git, 8 MB vs a server, request-reply and sagas | operator UI, provenance graph |
| **Kafka Connect** | "kafka source connector", "sink connector" | routing logic, request-reply, non-Kafka endpoints, one binary | Kafka-native offsets and converters, connector hub |
| **Redpanda Connect (Benthos)** | "benthos alternative", single Go binary, YAML, Bloblang, "stream processor" | the EIP vocabulary (saga, aggregate, idempotent consumer, circuit breaker), Camel compatibility, request-reply APIs, agent tool semantics | Bloblang's transformation ergonomics, Redpanda's backing |
| **n8n / Zapier** | "workflow automation", "ai agent workflow", visual nodes, SaaS triggers | throughput (hundreds of thousands of messages per second), protocols (MQTT, MLLP, AMQP, Kafka), routes as code, 8 MB | visual editor, non-developer users, marketplace |
| **Composio / Arcade / viaSocket / Paragon ActionKit** | "action layer", "tools for agents", managed OAuth, 500–7,000 connectors, MCP runtime | self-hosted, open source, tested routes with idempotency, sagas, approvals and audit; no per-execution pricing; 3 ms sidecar | connector count, managed per-user OAuth, hosted convenience; MCP (🚧) |
| **Agent frameworks** (LangChain, LlamaIndex, OpenAI Agents SDK, Bedrock Agents) | "function calling", "tool use" | complementary: the framework decides, Cameleer executes; one tool call instead of four | not a framework; does not run the model loop (🚧 agent EIP) |

Keywords Cameleer should own, in order of attainability: "apache camel alternative", "apache
camel rust", "camel without jvm", "camel quarkus native vs", "camel yaml dsl runtime", "mllp
rust", "hl7 integration engine open source", "mqtt edge gateway routes", "aws lambda integration
routes", "ai agent tool calls idempotency", "agent tool reliability saga", "mcp server
integration patterns" (🚧), "benthos alternative" (secondary), "spring integration alternative"
(secondary).

### 2.7 UX and message

- Hierarchy: the hero now carries one identity line, one agent promise and the three facts; the
  rest of the page is in the order a reader decides: see the route run (toggle) → numbers →
  agents → use cases → catalog → quick start.
- Tone: short, concrete, no superlatives that are not measured. "Reliable" is used only where a
  mechanism backs it (idempotent consumer, saga, redelivery, audit).
- CTAs: primary "Run your first route" (anchor to quick start), secondary "Agent patterns"; GitHub
  stays in the header as a pill.
- Mobile: all new pages are one column below 900 px; stat grids go 2×2; tables scroll inside
  their wrapper; nav keeps Docs, Use cases and GitHub.

## 3. Positioning

### 3.1 The question: "integration layer" or "action layer"?

**For the agent audience, "action" beats "integration".** Integration is a means; an agent's
user cares that something got *done*, once, correctly. The tool-calls page already makes this
case well ("the model decides what; a route decides how"). "Action layer" also describes the
roadmap's destination (MCP tool server, agent EIP, tool calls in every model component): once
those ship, Cameleer is where an agent's actions execute, and the phrase is accurate as a
category.

**Three reasons not to make it the whole identity today:**

1. The phrase is being defined by hosted connector catalogs. Composio calls itself the "Agent
   Action & Integration Layer" and cites 500–1,500 tools with managed OAuth; viaSocket is "the
   action layer for AI products" with 2,300 apps; a startup named ActionLayer launched in May
   2026; Paragon's ActionKit is "the integration layer beneath your agent framework". A buyer who
   searches the term will compare connector counts and OAuth; Cameleer loses that comparison and
   wins a different one (reliability semantics, self-hosting, footprint). The copy must set up the
   comparison Cameleer wins.
2. The first thing an agent builder looks for is MCP. Apache Camel 4.22 ships `camel-mcp-server`;
   Cameleer does not yet. Leading the whole site with "the action layer for AI agents" invites the
   question the product cannot yet answer with "yes". Once the tool server ships, this objection
   disappears.
3. Half of Cameleer's search demand comes from Camel users ("apache camel alternative", "camel
   rust", "camel quarkus native"). For them "action layer" means nothing; "Camel routes in Rust"
   means everything.

**Resolution: use both, in this order.** Identity line for the Camel audience, action line for
the agent audience, and reserve "the action layer" as the name of the agent pillar and the agents
section. When the tool server ships, promote the pillar's name to the tagline.

### 3.2 Tagline, subhead, pillars

- **Tagline (today):** *Camel routes in Rust. Reliable actions for AI agents.*
- **Tagline (when the agent tool server ships):** *The reliable action layer for AI agents, on
  Apache Camel's integration patterns, in Rust.*
- **Subhead:** Cameleer runs Apache Camel's routes, patterns and endpoint URIs unchanged as one
  native binary: 3 ms to start, megabytes to run, 1.7–8.7x the throughput. Give an agent a tool
  and the route validates the call, runs it once, retries, rolls back and writes the audit line.
- **Pillars:**
  1. **Act reliably.** Agent tool calls become tested routes: JSON-schema validation, idempotency,
     retries with back-off, sagas, human approval, rate limits, circuit breakers, audit. ✅ over
     HTTP today; 🚧 MCP tool server, agent EIP, tool calls in `openai` and `aws-bedrock`, vector
     components.
  2. **Keep your routes.** Camel's YAML and XML DSLs, URIs, EIPs, Simple, headers and error
     handling run unchanged; the team's Camel knowledge carries over. ✅
  3. **Run anywhere.** One binary: 3 ms, 8 MB, 20 MB image, built-in Lambda runtime, build
     features down to 5.8 MB; serverless, edge, sidecar. ✅; 🚧 static arm64 builds tested, more
     deployment examples.

### 3.3 The category name

Short term the category is the one Camel defined: **integration framework** (with "runtime" as
the differentiator: one binary, no JVM). Long term, with the roadmap shipped, the category
sentence is: *"Cameleer is an integration runtime and the action layer for AI agents: Camel's
patterns, in Rust, with an MCP tool server."* The plan keeps both phrases on the site so the
switch is a copy change, not a restructure.

### 3.4 cameleer.ing and "cameleering"

The word *cameleer* (a camel driver) already explains the name to anyone who knows Camel, and
`cameleer.ing` turns it into a verb for free. Assessment:

- **Update 2026-11-07 (owner's decision): cameleering is part of the positioning.** It has its
  own definitional page, `/cameleering/` ("What is cameleering?", `DefinedTerm` + `FAQPage`
  schema), a band on the home page right after the hero, a line under the README pillars, a
  paragraph in `llms.txt` and an entry in the docs FAQ. The definition is fixed so every surface
  and every answer engine repeats the same sentence: *"Cameleering is running Apache Camel
  integration routes on Cameleer: the same Enterprise Integration Patterns, endpoint URIs and YAML
  DSL, executed by one native Rust binary instead of the JVM."* The coined term has no search
  volume yet; the point is that a brand-owned term gets defined by its owner before anyone else
  does, and that answer engines asked "what is cameleering?" find one consistent answer. The
  original advice below still applies to titles of other pages and to reference docs.
- **Use it, lightly.** One verb phrase per surface is memorable; more is cute. Recommended uses:
  the quick-start heading ("Start cameleering"), the footer line, talk slides and the Docker Hub
  overview. Never in `<title>` tags or H1s (zero search volume) and not in reference docs.
- **Taglines that use the wordplay** (pick one for merchandise or a launch post, not for the hero):
  "Go cameleering." / "Cameleering: driving Camel routes without the JVM." / "Your routes, driven."
- **The domain story.** `cameleer.ing` is short, pronounceable and reads as a sentence
  ("cameleer-ing"). Use it first as a **short link**: `cameleer.ing` → the home page,
  `cameleer.ing/docs`, `/agents`, `/compare`, `/lambda` as 301 redirects, in talks, READMEs of
  examples and social cards. The .ing TLD is young; some corporate link filters flag unfamiliar
  TLDs, so the GitHub Pages URL stays canonical until the domain has history.
- **Done on 2026-11-07:** DNS is set up, `cameleer.ing` is the GitHub Pages custom domain and
  `pmsudhi.github.io/cameleer/` answers 301 to it. Step 3 below was done in one commit: canonicals,
  `og:url`, JSON-LD, the sitemap (`SITE_URL` in `scripts/build-docs.py`), `robots.txt`,
  `llms.txt`, the IndexNow host, README, crate `homepage`/`documentation`, the image label, the
  Docker Hub overview and `CITATION.cff` all name `https://cameleer.ing/`. `CHANGELOG.md` keeps
  the old links of released versions (they redirect). Left for the owner: enforce HTTPS in the
  Pages settings, add `cameleer.ing` to Search Console (the existing verification meta tag also
  verifies a URL-prefix property; a DNS Domain property is better), submit the sitemap, and use
  the change-of-address tool from the old property. Do not change page titles for a few weeks.
- **Migration plan:**
  1. Serve the site at `cameleer.ing` with HTTPS and the same paths; keep `pmsudhi.github.io/
     cameleer/` canonical for at least one indexing cycle while the new host builds history.
  2. Add `cameleer.ing` to Search Console; verify both properties.
  3. Switch: set `<link rel="canonical">`, the sitemap `<loc>`s, `SITE_URL` in
     `scripts/build-docs.py`, `og:url`, JSON-LD `url`, `llms.txt` links and the Docker Hub and
     crates.io metadata to the new host in one commit; add the `CNAME` file; 301 every old URL
     (GitHub Pages does this when `CNAME` is set); use Search Console's change-of-address.
  4. Keep the repository links (`github.com/pmsudhi/cameleer`) unchanged; only the site moves.
  5. Expect a few weeks of ranking noise; do not change page titles in the same window.

## 4. Keyword map per page

| page | primary keyword | secondary | intent |
|---|---|---|---|
| `/` | apache camel in rust | camel routes without jvm, integration layer for ai agents, reliable agent tool calls | brand, overview |
| `/compare/apache-camel-vs-cameleer/` | apache camel vs cameleer | camel benchmark, camel quarkus native startup memory, camel throughput | evaluation |
| `/alternatives/apache-camel-alternatives/` | apache camel alternatives | camel vs spring integration, kafka connect vs camel, redpanda connect vs camel, nifi vs camel, mulesoft alternative | evaluation |
| `/compare/ai-agent-tool-calls-direct-vs-cameleer/` | ai agent tool calls reliability | idempotent tool calls, agent saga compensation, function calling vs integration layer, action layer for ai agents | problem-aware |
| `/compare/` | compare cameleer | — | hub |
| `/use-cases/` | cameleer use cases | integration use cases rust | hub |
| `/use-cases/hl7-mllp-healthcare-integration/` | hl7 mllp integration | hl7 v2 interface engine open source, mllp rust, fhir integration routes | solution |
| `/use-cases/mqtt-iot-edge-gateway/` | mqtt edge gateway | iot gateway integration routes, store and forward mqtt, syslog to kafka | solution |
| `/use-cases/serverless-aws-lambda/` | aws lambda integration routes | serverless camel routes, cold start integration, cloud run integration | solution |
| `/use-cases/file-sftp-pipelines/` | sftp file transfer pipeline | csv batch processing routes, file to s3 rust, ftp polling | solution |
| `/docs/guides/agents.html` | cameleer ai agents | llm routing patterns, rag ingestion route, human approval agent | how-to |
| `/docs/guides/migrating-from-apache-camel.html` | migrate from apache camel | camel yaml dsl rust | how-to |
| `/docs/guides/build-features.html` | small integration binary | cargo features cameleer | how-to |
| 🚧 `/use-cases/mcp-tool-server/` (when shipped) | mcp server integration routes | expose routes as mcp tools, reliable mcp tools | solution |

## 5. Site structure

```
/                                   home (hand-written)
/use-cases/                         hub (new)
  hl7-mllp-healthcare-integration/  new
  mqtt-iot-edge-gateway/            new
  serverless-aws-lambda/            new
  file-sftp-pipelines/              new
  [mcp-tool-server/]                🚧 slot: add when camel-mcp-style server ships
/compare/                           hub
  apache-camel-vs-cameleer/
  ai-agent-tool-calls-direct-vs-cameleer/
  [cameleer-vs-redpanda-connect/]   🚧 slot, if the keyword proves out
/alternatives/apache-camel-alternatives/
/docs/…                             generated from docs/
/llms.txt, /llms-full.txt, /sitemap.xml
```

**Navigation (every hand-written page):** Docs · Use cases · Agents · Compare · GitHub. The home
page adds "Quick start". Docs pages keep their own header (Docs · Guides · Components) and get a
"Use cases" link in the footer.

**Roadmap slots built into the pages:**

- Home, agents band: step 4 "Reliable actions" lists the mechanisms; a sentence after the band
  links to the roadmap for MCP. When the server ships, the sentence becomes a fifth item.
- Agent tool-calls compare page: FAQ "Does Cameleer speak MCP?" answers "not yet" and links to
  the roadmap; replace the answer, keep the question.
- Use-case hub: the "AI agent tools" card names what ships and ends with "MCP server: planned".
- Camel-vs-Cameleer glance table: an "AI agents" row compares Camel's `ai-tool`/MCP server with
  Cameleer's HTTP tools and marks MCP planned.
- Alternatives page: the Cameleer row's trade-offs column carries "no MCP server yet".
- `llms.txt`: a "Planned, not shipped" line listing the roadmap's agent items.

## 6. Content changes per page

- **Home** (`site/index.html`): new title and description; JSON-LD version and `sameAs`; nav
  with Use cases, Agents, Compare; H1 keeps the strike device with an `aria-label`; lede carries
  the two-audience subhead; the agent line becomes "Reliable actions for AI agents" with a link to
  the compare page; CTAs are "Run your first route" and "Agent patterns"; the runtime toggle gets
  a third side, Apache Camel on Quarkus native; the agents band's heading becomes "The action
  layer for AI agents" with a roadmap line; the footprint section is retitled "Where it runs" and
  its three cards link to the new use-case pages; a new "Use cases" strip links the hub; the
  quick-start heading uses the verb once ("Start cameleering"); footer gains Use cases.
- **Compare hub**: adds the use-case hub and a list of the comparisons by name for crawlers.
- **Apache Camel vs Cameleer**: an "AI agents" row in the glance table; nav with Use cases.
- **AI agent tool calls**: kicker and FAQ adopt the action-layer wording; new FAQ entries on MCP
  and on hosted tool catalogs; nav with Use cases; schema updated.
- **Apache Camel alternatives**: two new rows (Redpanda Connect/Benthos; n8n and Zapier-style
  automation) and a FAQ entry each; the Cameleer row's trade-offs mention MCP.
- **Use cases** (new hub + four pages): each page has kicker, H1 with the keyword, lede, four
  stats, "what the route does" with the tested example excerpt, a component list with links into
  the docs, "why not the JVM / why Cameleer", a FAQ (FAQPage schema) and a run-it CTA with the
  exact Docker command from the example header.
- **README**: new headline block (tagline, three pillars), "Rebuilt for agents" retitled "The
  action layer for agents" with the 🚧 markers unchanged, a "Use cases" link row, a smaller
  wording fix in the logo alt text. Benchmarks, quick start and the rest unchanged.
- **docs/README.md**: a "use cases" row in the "where to find what" table.
- **`site/llms.txt` and root `llms.txt`**: rewritten identity paragraph, key facts, use cases,
  comparisons, "planned, not shipped" line; the generated documentation section stays.
- **`scripts/build-docs.py`**: `use-cases/` counted as designed pages for the sitemap; docs
  footer gains Use cases.
- **Assets**: `brand/cameleer-mark-64.png` for headers.

## 7. What is not changed, and why

- **Canonical URLs, `CNAME`, links** (superseded 2026-11-07, see 3.4): stayed on the old address
  until DNS for `cameleer.ing` was set up; moving canonicals early would have split signals.
- **Benchmark numbers, `docs/BENCHMARKS.md`, `CHANGELOG.md`, Rust code**: untouched.
- **The visual language** (`site.css`, `compare.css`, the type pair, the signal blue): it is
  distinctive and consistent; the new pages reuse the compare stylesheet.
- **The docs generator's page conventions**: the question-answer blockquote, FAQ markup and
  markdown twins already serve humans and LLMs.
- **The README's text benchmark chart and tables**: they render in every Git viewer and are
  correct.
- **The "not affiliated with or endorsed by the ASF" disclaimers**: kept on every page; Apache
  Camel is named only to identify it and its logo is never used.
- **No "cameleering" in titles or reference docs**: the verb appears once on the home page.

## 8. Recommended, not done

- Generate a proper OG image per landing page (the single `og.png` works, but per-page images
  lift click-through on social).
- A short demo recording of the runtime toggle (startup race) for social and the README.
- A "Cameleer vs Redpanda Connect" page once Search Console shows the query.
- When the MCP tool server ships: the `/use-cases/mcp-tool-server/` page, the tagline switch
  (3.2), the agents FAQ answers, and a `SoftwareApplication` `featureList` entry.
- Submit the site to the Rust community lists (This Week in Rust, awesome-rust under
  "integration") and the Camel community (Camel's "Articles" page accepts links).
- Ask Context7 and DeepWiki to re-index after this change (they read `llms.txt` and the README).

## 9. Sources used for the competitive analysis

- Apache Camel 4.22 AI tools and MCP server: https://camel.apache.org/blog/2026/08/camel-ai-tools-mcp-422/
  and the LangChain4j Agent component page, https://camel.apache.org/components/next/langchain4j-agent-component.html
- Composio, "The 2026 Guide to AI Agent Builders (And Why They All Need an Action Layer)":
  https://composio.dev/blog/best-ai-agent-builders-and-integrations
- viaSocket, "The action layer for AI products": https://viasocket.com/developers
- Paragon, tool infrastructure for enterprise AI agents: https://www.useparagon.com/blog/tool-infrastructure-enterprise-ai-agents
- Merge, Composio vs Arcade: https://www.merge.dev/blog/composio-vs-arcade; Respan market map:
  https://www.respan.ai/market-map/compare/arcade-ai-vs-composio
- Redpanda, Kafka Connect vs Redpanda Connect: https://www.redpanda.com/blog/kafka-connect-vs-redpanda-connect;
  AutoMQ on Redpanda Connect alternatives: https://www.automq.com/blog/redpanda-connect-alternatives-for-kafka-pipelines
- Salesforce MuleSoft positioning (agent control plane, Agent Fabric): https://www.salesforce.com/mulesoft/;
  Composio's MuleSoft alternatives round-up (WSO2 placement): https://composio.dev/blog/mulesoft-alternatives
- Spring Integration vs Camel: https://codemia.io/knowledge-hub/path/when_to_use_spring_integration_vs_camel,
  https://www.javacodegeeks.com/?p=135385
- NiFi vs Camel: https://www.stackshare.io/stackups/apache-nifi-vs-apache-camel,
  https://projectpro.io/compare/apache-nifi-vs-apache-camel
- n8n's agent feature and positioning: https://www.createwith.com/tool/n8n/updates/n8n-launches-ai-agent-in-early-preview-to-run-alongside-workflows,
  https://www.mindstudio.ai/blog/n8n-vs-ai-native-automation-platforms

Most of these are vendor-written; they were used for positioning language and feature claims,
not for independent performance figures.

## 10. Competitive placement: what G2 shows for "Apache Camel alternatives" (2026-11-09)

### 10.1 What the searcher sees

G2's [Apache Camel alternatives](https://www.g2.com/products/apache-camel/competitors/alternatives)
page (G2 blocks automated readers; the list below is from search results and G2's comparison
pages) is not a list of developer frameworks. It is enterprise integration software:
IBM App Connect, Azure Service Bus, TIBCO Integration (BusinessWorks and Flogo), IBM webMethods
Hybrid Integration, WSO2 Integration Platform, MuleSoft Anypoint, Boomi, Workato, SAP Integration
Suite, CData Arc, NServiceBus, elastic.io, Apache Synapse and Apache ServiceMix. G2 also has
"Apache Camel vs X" pages for most of them. Camel itself has about 20 G2 reviews, 4.2/5; reviewers
on G2 and TrustRadius praise its cost and testing support and complain about documentation, a
learning curve and the lack of a visual designer.

So "Apache Camel alternative" has three different searchers:

| searcher | wants | today they land on | Cameleer's opening |
|---|---|---|---|
| a developer running Camel on the JVM | the same model, lighter and faster | directory lists, Quarkus native | the only runtime that runs Camel routes unchanged; measured 1.7-8.7x throughput, 3 ms start |
| an enterprise buyer comparing integration platforms | governance, connectors, support, AI agents | G2, vendor listicles (MuleSoft, Boomi, Workato, WSO2) | open source with no per-core licence; runs where an iPaaS can't (edge, Lambda, sidecar); routes in Git |
| an agent builder | connect agents to business systems safely | MCP platforms of the same vendors, Composio, Arcade | reliability enforced on every call, both MCP eras, tested with Claude, self-hosted |

The current alternatives page serves the first searcher only: it compares Camel on Quarkus,
Spring Integration, Kafka Connect, Redpanda Connect, NiFi and n8n, and names MuleSoft in
passing. None of G2's other names (IBM, TIBCO, WSO2, Boomi, Workato, Azure, SAP) appear.

### 10.2 Everyone has MCP now

Every vendor on that list announced MCP or agent features in 2025-26, so "has an MCP server" is
table stakes, not a differentiator:

| vendor | agent and MCP features (vendor claims, checked 2026-11-09) | model | where Cameleer differs |
|---|---|---|---|
| MuleSoft (Salesforce) | MCP server and connector, Agent Fabric, Agentforce | quote-only; third parties report vCore and usage-based contracts from tens to hundreds of thousands of dollars a year | open source, one binary, enforced envelope; no API management suite, no Exchange, no DataWeave |
| Boomi | governed MCP servers to 1000+ applications, Boomi agents | from about $99/month pay-as-you-go per third parties; enterprise by quote | self-hosted, no per-connection pricing; far fewer SaaS connectors, no visual designer |
| Workato | Enterprise MCP, agent recipes | credits; enterprise by quote | as for Boomi |
| WSO2 Integrator (BI, MI, SI) | MCP servers from APIs, agents with memory and RAG, durable agentic workflows; Apache-2.0 | open source plus subscription | the closest open-source peer; WSO2 runs on the JVM and has a visual designer and API management; Cameleer is Camel-compatible, 3 ms, megabytes |
| TIBCO Flogo | Go engine, single binary for edge, serverless and IoT; MCP connector (preview 2025) with elicitation and sampling; Project Flogo core is BSD | Flogo Enterprise is commercial | the closest lightweight peer; its claims (about 5 MB, about 1 s start, 2017 vendor slides) are not measured by us: benchmark before publishing numbers |
| IBM webMethods Hybrid Integration, App Connect | Integration Agent (natural language to flows), AI Gateway, MCP Gateway | commercial | a self-hosted single binary against a control plane; no natural-language builder (a coding agent writes YAML checked by `cameleer validate`) |
| Azure Logic Apps (Standard) | workflows as MCP servers, GA June 2026 per Azure Updates | Azure consumption | runs anywhere, not only on Azure; connects to Azure Service Bus, Event Hubs and Cosmos DB |
| Apache Camel 4.22+ | `camel-ai-tool`, `camel-mcp-server` (Preview) | Apache-2.0 | the existing compare page |

Category mismatches on G2's list: Azure Service Bus and NServiceBus are messaging (Cameleer
connects to Service Bus; it is not a broker); CData Arc is B2B/EDI file exchange; Synapse and
ServiceMix are older Apache ESBs (migration candidates).

### 10.3 Placement

Lead with what no competitor can say, in this order:

1. **Runs Camel routes unchanged, without the JVM** (only Cameleer; measured against Camel).
2. **Agent actions that are safe by construction**: schema, run-once, approval, a timeout that
   cancels, compensation and audit, enforced by the server on every MCP or model call. Vendors
   describe governance; Camel documents its hints as advisory. Backed by the field test.
3. **One binary that goes where platforms can't**: 3 ms, megabytes, Lambda, edge, sidecar, a
   std-only supply chain.
4. **Open source with no per-core or per-credit licence.**

Do not compete on connector count, visual designers, API management suites, managed hosting or
commercial support. Say so on every comparison page ("when to choose them"): it is what makes the
rest credible, and buyers on G2 weigh exactly those things.

### 10.4 Pages

Template for every comparison page: an answer line; a glance table; when to choose them; when to
choose Cameleer; how to move (or how to use both); numbers only where we measured them; vendor
claims cited to the vendor with a "checked on" date; FAQ (FAQPage); a trademark line. Vendor
names only to identify; no logos.

| priority | page | target searches | why |
|---|---|---|---|
| P1 | refresh `/alternatives/apache-camel-alternatives/` | apache camel alternatives | it must name what G2 names (MuleSoft, IBM, TIBCO, WSO2, Boomi, Workato, Azure, SAP), grouped by searcher |
| P1 | `/compare/enterprise-mcp-platforms/` | mulesoft mcp, boomi mcp server, workato mcp, wso2 mcp, logic apps mcp | one honest table of the MCP offerings in 10.2 against Cameleer: the agent pillar's comparison page |
| P1 | `/compare/wso2-vs-apache-camel-vs-cameleer/` | apache camel vs wso2, wso2 alternative | G2 has a Camel vs WSO2 page; a three-way page answers that search and adds Cameleer |
| P1 | `/compare/mulesoft-vs-apache-camel-vs-cameleer/` and `/alternatives/mulesoft-alternatives/` | mulesoft alternative, mulesoft vs camel | the largest-volume query in the space, crowded by listicles; the angle is open source plus Camel-compatible plus agents |
| P2 | `/compare/tibco-flogo-vs-cameleer/` | flogo alternative, lightweight integration engine | the direct lightweight competitor; needs a measured run (Project Flogo on the droplets) first |
| P2 | `/compare/redpanda-connect-vs-cameleer/`, `/compare/spring-integration-vs-cameleer/` | benthos alternative, spring integration vs camel | developer intent, already in the earlier plan |
| P2 | `/compare/boomi-vs-cameleer/`, `/compare/workato-vs-cameleer/` | boomi alternative open source, workato alternative | iPaaS buyers who want self-hosting or agent safety |
| P3 | `/compare/ibm-webmethods-vs-cameleer/`, `/compare/azure-logic-apps-vs-cameleer/` | webmethods alternative, logic apps alternative | lower volume; the Azure page doubles as "runs outside Azure" |
| P3 | "works with" pages: Azure Service Bus, Event Hubs | azure service bus integration | the right answer to a category mismatch is "Cameleer works with it", with a tested route |
| P3 | migration notes from Synapse and ServiceMix | apache servicemix replacement | small, but nobody else answers it |

The `/compare/` hub groups the pages by the three searchers in 10.1.

**Done 2026-11-09:** all P1 pages (the refreshed alternatives page, `/compare/enterprise-mcp-platforms/`,
`/compare/wso2-vs-apache-camel-vs-cameleer/`, `/compare/mulesoft-vs-apache-camel-vs-cameleer/`,
`/alternatives/mulesoft-alternatives/`) and the regrouped `/compare/` hub. Vendor claims on them
are dated 9 November 2026; re-check them by February 2027.

### 10.5 Off-site (owner's accounts)

- **List Cameleer on G2** (a free vendor profile) in the category Camel is in, with the website,
  repository and screenshots. G2's alternatives pages draw from the same category and rank by
  reviews, so ask real users for reviews; never pay for or write them.
- The same on SourceForge and Slashdot (they already list Camel alternatives), AlternativeTo,
  StackShare, SaaSHub and OpenAlternative (as an open-source alternative to MuleSoft and Boomi).
- Re-check the vendor claims in 10.2 every quarter and date-stamp them on the pages.

### 10.6 Sources (checked 2026-11-09)

- G2: https://www.g2.com/products/apache-camel/competitors/alternatives and comparison pages
  such as https://www.g2.com/compare/apache-camel-vs-wso2-integration-platform
- TrustRadius reviews: https://www.trustradius.com/products/apache-camel/reviews
- MuleSoft pricing reports (third party, unverified): https://www.vendr.com/marketplace/mulesoft,
  https://unmule.com/blog/mulesoft-pricing-2026
- WSO2 Integrator: https://wso2.com/integration-platform/integrator/,
  https://wso2.com/integration-platform/integrator/mi/,
  https://github.com/wso2/product-integrator/releases/tag/v5.1.0
- TIBCO Flogo: https://docs.tibco.com/products/tibco-flogo-latest,
  https://www.tibco.com/blog/2025/08/07/introducing-tibco-flogo-connector-for-model-context-protocol-mcp-developer-preview/,
  https://www.tibco.com/blogs/2026-04/tibco-vision-agentic-enterprise-solutions
- Boomi AI agents: https://boomi.com/platform/ai-agents/; Workato MCP (third party):
  https://eseospace.com/blog/workato-mcp/
- IBM: https://www.ibm.com/products/webmethods-hybrid-integration/agentic-enterprise
- Azure Logic Apps MCP: https://techcommunity.microsoft.com/blog/integrationsonazureblog/stop-writing-plumbing-use-the-new-logic-apps-mcp-server-wizard/4496702

## 11. Attestation: positioning, pages and placement (2026-11-09, proposal)

The owner asked how to position, document and place the attestation work (phase 1, built by
the code session, landing on `main` and shipping in the next release). This section is a proposal;
nothing below is published yet.

### 11.1 What may be claimed, and what may not

**May claim (phase 1):**

- Inside a hardware TEE, Cameleer proves to a client what is running. The TEEs supported are an
  Intel TDX or AMD SEV-SNP confidential VM through Linux configfs-tsm (kernel 6.7 or later), and a
  dstack CVM (Phala Cloud, dstack on GCP TDX).
- The signed hardware report is bound to four things:
  - the client's nonce;
  - the SHA-256 of Cameleer's own TLS public key, so a proxy that terminates TLS breaks the check;
  - a manifest of the version and features, the route files (SHA-256), the routes, the
    configuration (secrets shown as `<secret>`) and the published tools (fingerprints);
  - a hash-chained log of every change after start-up.
- Where clients get it:
  - `GET /q/attestation?nonce=...`;
  - over MCP, `_meta["ing.cameleer/attestation"]` in `server/discover` and `initialize`, with the
    report inline when the client sends a nonce;
  - the opt-in tool `cameleer_attestation`.
- If attestation is on and there is no evidence, start-up fails.
- `cameleer_attest::check_binding` verifies the binding.
- It is in the default build (cargo feature `attest`).

**May not claim:**

- that Cameleer verifies Intel or AMD signature chains (it doesn't yet; phase 3);
- sealed secrets (phase 2);
- AWS Nitro, Azure vTPM or Google Confidential Space;
- that it has run on real TEE hardware (the tests use a fake configfs-tsm and a fake dstack
  agent);
- "this exact release" unless the image is pinned by digest, because reproducible builds aren't
  published yet;
- protection against physical attacks (TEE.fail, 2025);
- that it is released.

**Words:**

- Use "attested", "evidence", "proves what is running", and "verifiable by your client" with the
  note that the hardware signature is checked by a verifier such as Intel Trust Authority or a DCAP
  library.
- Never use "verified", "trustless", "zero trust", "tamper-proof" or "proof of execution". The last
  one is a blockchain and ZK term, and the claim it implies is wider than phase 1.

### 11.2 What the market and search data show

There is no keyword-volume tool in this environment, so the evidence is which words appear across
standards pages, vendors and the top search results (searched 2026-11-09).

| phrase | who uses it | state |
|---|---|---|
| "confidential computing" | Confidential Computing Consortium, Azure, Google, Intel, Red Hat | established category; the buyer's own word in regulated industries |
| "confidential AI" / "confidential agentic AI" | NVIDIA, Fortanix, Opaque, Phala, CCC blog posts (Jan and May 2026), the arXiv survey 2605.03213 | the 2026 growth term; NVIDIA, Microsoft, Cisco, Fortanix and HPE shipped agent-focused features in spring 2026 (one roundup) |
| "attestable / attested MCP server" | `attestable-mcp-server` (SGX, RA-TLS), Glama's "attested" filter, Phala and Oasis posts | small, developer-level; the projects are prototypes or 2025-era |
| "verifiable MCP" / "verifiable tool results" | MCP issue #3354 (closed 2026-09-22), a zk-tokyo course proposal | proposal stage, ZK-flavoured |
| "MCP trust layer" | vendors of credentials, registries and signed receipts (cheqd, Mnemom, AegisZK, ArkForge) | crowded and vague; none of these is hardware evidence |

**Competitors:**

- **Integration engines and iPaaS.** No integration engine or iPaaS advertising attestation turned
  up, in the code session's research or in a fresh search for MuleSoft, Boomi and Camel. That is
  absence of evidence, so the site says "we found none", never "the only".
- **MCP.** Phala's attestation is a platform check: a dashboard button, and mutual quotes between
  its gateway and the CVM. Its MCP tutorial shows the check after the client is configured, not as a
  gate before connecting. The SGX `attestable-mcp-server` binds the evidence to an enclave key over
  RA-TLS. Neither covers routes, configuration and the tool surface the way Cameleer's manifest does.
- **Standards.** The MCP spec has no accepted attestation mechanism. SEP-2809 (ATSA, which is not
  hardware attestation) was closed on 2026-11-08, and the MCP Security Interest Group lists
  "server identity, attestation, and admission" in its scope. The arXiv survey calls client
  verification of an attested MCP server an open problem.
- **Healthcare.** Microsoft publishes a confidential healthcare inference reference architecture
  (AKS confidential nodes with Azure Attestation). For imaging, the search found only one vendor's
  blog post on TDX-sealed radiology inference, and nothing about DICOM routing or de-identification
  inside a confidential VM. That is an open space, but it needs the DICOM work first.
- **The main barrier.** In a CCC-affiliated survey, 85% of respondents named validating
  attestation chains of trust as their top challenge (vendor-affiliated, so cite it with care).
  The message that answers it: the evidence covers the routes and tools, not just the VM, and the
  binding check is one function.

### 11.3 Positioning

One idea, said two ways for two searchers:

> **Cameleer can prove what it runs.** Inside a confidential VM, an agent or a client can ask a
> Cameleer server for hardware evidence of the exact routes, configuration and MCP tools it is
> running, bound to the TLS connection the client is using.

| searcher | angle | page title idea | why |
|---|---|---|---|
| agent builders and MCP platform teams | **an MCP server that can prove what it runs** ("attested MCP server") | "Attested MCP server: prove which tools an agent is talking to" | extends the action layer we already lead with ("the model decides what, a tested route decides how") to "and the client can check which routes those are"; the category is nearly empty and has no standard yet |
| regulated integration (healthcare, finance, public sector) | **confidential integration**: the integration engine itself attests inside a confidential VM | "Confidential integration: run Camel routes in a confidential VM and prove it" | matches the buyer's own words ("confidential computing"); no integration engine says it; the healthcare showcase (DICOM de-identification inside a CVM) lands here later |

Don't make attestation the headline of the home page or the README. Cameleer's lead stays "Apache
Camel in Rust" and "the reliable action layer for AI agents". Attestation is the proof point under
the second, and gets a section when it is released.

### 11.4 Pages

| page | change | when |
|---|---|---|
| `docs/guides/attestation.md` (written by the code session) | review the wording against 11.1; add to `GUIDE_ORDER`, the RTD toctree, docs/README, getting started's "next" list, and cross-links from TLS and security, MCP server and observability; the answer-first opening and a "Common questions" block (what it proves, what it doesn't, which TEEs, does a model verify it) | when it is on `main`, marked "next release" |
| new **`/trust/`** hub: "Attested integrations and MCP servers" | what is in the evidence (a diagram of nonce + TLS key + manifest + change log → REPORTDATA); the three ways to fetch it; the binding check; supported TEEs; a prominent "What it does not prove" box (11.1); how to try it on dstack or a TDX/SEV-SNP VM; FAQ with FAQPage schema | at the release (owner's decision); until a run on real TEE hardware, the page states that the tests use a simulated configfs-tsm and dstack agent |
| `/mcp/` hub | a section "Prove which server and tools an agent is talking to" after the guarantees, linking `/trust/`; one FAQ | with `/trust/` |
| comparisons (`apache-camel-vs-cameleer`, `mulesoft-vs-…`, `wso2-vs-…`, `enterprise-mcp-platforms`) | one table row, "Evidence of the running integration (TEE attestation)": Cameleer "yes, in a confidential VM (phase 1)"; the others "not found in their documentation (searched 2026-11)", which is never written as "no" | with `/trust/` |
| `use-cases/hl7-mllp-healthcare-integration` | a short "in a confidential VM" paragraph linking `/trust/` | with `/trust/` |
| new **`use-cases/confidential-dicom-de-identification/`** | the showcase: DICOM de-identification inside a confidential VM, with evidence for what ran | when DICOM is built (the owner will send a separate brief) |
| home page | one sentence in the agent section, no hero change | with `/trust/` |
| `/compare/attested-mcp-servers/` (Phala, Oasis, Cameleer) | not now: low volume, and the comparison would rest on features we haven't run on hardware | revisit after phase 3 |

### 11.5 README, llms.txt, GitHub About, Docker Hub

| place | proposal | when |
|---|---|---|
| README | one bullet in the AI section: "**Prove what runs** (next release): inside an Intel TDX or AMD SEV-SNP confidential VM, Cameleer returns hardware evidence of its routes, configuration and MCP tools, bound to its TLS key ([attestation](docs/guides/attestation.md))." | when on `main` |
| llms.txt | one key-facts line with the limits in the same line (unreleased; doesn't verify vendor chains; TDX/SEV-SNP via configfs-tsm and dstack only); a Q&A pair "Can an MCP client verify what a Cameleer server runs?" in llms-full through the guide's FAQ | when on `main` |
| GitHub About | keep the description (it's at about 260 of 350 characters, and the lead is right); after the release add ", and proves what it runs inside confidential VMs" if it fits. Topics are at GitHub's maximum of 20, so swap: drop `camel` (duplicates `apache-camel`) and `messaging`, add `confidential-computing` and `attestation` | after the release |
| Docker Hub overview | one line under the features, plus the note that evidence means "this image" only when pinned by digest (`pmsudhi/cameleer@sha256:…`) | after the release |

### 11.6 Before publishing

- Re-check the competitor statements on the day: each vendor's docs for "attestation", "trusted
  execution" or "confidential", the MCP SEP list and the Security IG page.
- Read the final `docs/guides/attestation.md` and the config keys on `main`, and quote nothing that
  isn't there.
- Owner decisions:
  1. ~~publish `/trust/` at the release, or wait for a real-hardware run~~ **Decided 2026-11-09 by
     the owner: at the release.** Until a real-hardware run, the page says plainly that the tests use
     a simulated configfs-tsm and dstack agent;
  2. whether to bring the `_meta` design to the MCP Security Interest Group (it would make "a
     concrete, spec-compatible proposal" a claim we can link to);
  3. the page name: `/trust/`, or `/confidential/`, which matches the buyer's word better but is
     narrower.

### 11.7 Sources

- MCP issue #3354 (verifiable tool results): https://github.com/modelcontextprotocol/modelcontextprotocol/issues/3354
- MCP Security Interest Group: https://modelcontextprotocol.io/community/interest-groups/security;
  SEP-2809: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2809
- Attestable MCP server (SGX, RA-TLS): https://glama.ai/mcp/servers/kontext-security/attestable-mcp-server,
  https://creati.ai/mcp/attestable-mcp-server/
- Phala MCP hosting and checks: https://phala.com/posts/mcp-got-you-worried-build-securely-with-phala-clouds-teepowered-hosting,
  https://phala.com/posts/deploy-an-MCP-server-on-phala-cloud-a-step-by-step-guide,
  https://docs.phala.com/phala-cloud/networking/security.md
- tee.fyi on MCP servers: https://tee.fyi/docs/tee-applications/mcp-servers
- Confidential agentic AI: https://confidentialcomputing.io/2026/01/20/protecting-agentic-ai-workloads-with-confidential-computing/,
  https://confidentialcomputing.io/2026/05/20/your-ai-agents-are-already-in-production-your-security-architecture-isnt-ready/,
  https://www.supercomputing.news/ai/confidential-computing-arrives-for-ai-agents,
  https://arxiv.org/html/2605.03213v1
- Healthcare: https://learn.microsoft.com/en-au/azure/architecture/example-scenario/confidential/healthcare-inference,
  https://learn.microsoft.com/en-sg/azure/confidential-computing/use-cases-scenarios,
  https://dev.to/voltagegpu/medical-imaging-ai-without-a-baa-nightmare-tdx-sealed-inference-for-radiology-28kn
- MCP trust-layer vendors: https://arkforge.tech/en/blog/trust-layer-mcp-fragmentation/,
  https://cheqd.io/blog/pioneering-trust-in-the-age-of-ai-introducing-cheqds-mcp-enabled-agentic-trust-solution/
- Red Hat on attestation models: https://www.redhat.com/en/blog/attestation-confidential-computing
- The code session's research with full sources: `docs/design/attestation/research.md` (internal)
