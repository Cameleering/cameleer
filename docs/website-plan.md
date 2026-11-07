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
