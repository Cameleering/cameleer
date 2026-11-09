# Cameleer documentation

> Cameleer is Apache Camel rewritten in Rust; these pages explain how to write, run and deploy its routes, and document every component, language and data format with tested examples.

If you already know Apache Camel, the routes, endpoint URIs, EIP names and the YAML DSL are the
same. Start with a guide, then look up the components your route uses.

## Where to find what

| you want to | go to |
|---|---|
| write and run your first route | [Guides](guides/) |
| configure routes with properties, profiles and environment variables | [Configuration](guides/configuration.md) |
| look up a `cameleer` command or flag | [The cameleer command line](guides/cli.md) |
| send or receive messages with Kafka, HTTP, S3, Slack, SQL, ... | [Components](components/), one page per endpoint scheme, grouped by category |
| write expressions and predicates (`${header.x}`, JSONPath, XPath) | [Languages](languages/) |
| convert message bodies (JSON, CSV, XML, Avro, ...) | [Data formats](dataformats/) |
| give AI agents tools: an MCP server with approvals, audit and a gateway, with ten tested use cases | [Cameleer MCP](guides/mcp.md) |
| run Cameleer in Docker, Kubernetes, Cloud Run or AWS Lambda | [Deployment](DEPLOYMENT.md) |
| build a smaller binary or image with only the components you use | [Build features](guides/build-features.md) |
| check which Camel features are implemented, with their options | [Feature matrix](FEATURES.md) |
| see what people run on it: AI agent tools, HL7 over MLLP, MQTT edge gateways, serverless, file and SFTP pipelines | [Use cases](https://cameleer.ing/use-cases/), the [agent cookbook](guides/agents.md) and the [edge cookbook](guides/edge.md) |
| compare Cameleer with Apache Camel: throughput, memory, startup | [Apache Camel vs Cameleer](https://cameleer.ing/compare/apache-camel-vs-cameleer/) and the [benchmark report](BENCHMARKS.md) |
| choose between Apache Camel alternatives | [Apache Camel alternatives](https://cameleer.ing/alternatives/apache-camel-alternatives/) |
| see what was verified against real servers and clients | [Interop](INTEROP.md) |
| see what is planned | [Roadmap](ROADMAP.md) |

The project overview, the quick start and the examples are in the
[README](../README.md); the ready-to-run route files are in [examples/](../examples/).

## Connect your coding agent

Give Cursor, Claude, Windsurf, VS Code or any MCP-capable assistant Cameleer's real docs, so it
writes routes from the documentation instead of guessing. The docs MCP server is
`https://gitmcp.io/pmsudhi/cameleer` (it serves this repository's `llms.txt`, docs and README).

| tool | add this |
|---|---|
| Claude Code | `claude mcp add --transport http cameleer-docs https://gitmcp.io/pmsudhi/cameleer` |
| Cursor (`~/.cursor/mcp.json`) | `{"mcpServers": {"cameleer-docs": {"url": "https://gitmcp.io/pmsudhi/cameleer"}}}` |
| Windsurf (`~/.codeium/windsurf/mcp_config.json`) | `{"mcpServers": {"cameleer-docs": {"serverUrl": "https://gitmcp.io/pmsudhi/cameleer"}}}` |
| VS Code (`.vscode/mcp.json`) | `{"servers": {"cameleer-docs": {"type": "http", "url": "https://gitmcp.io/pmsudhi/cameleer"}}}` |
| Claude Desktop (`claude_desktop_config.json`) | `{"mcpServers": {"cameleer-docs": {"command": "npx", "args": ["mcp-remote", "https://gitmcp.io/pmsudhi/cameleer"]}}}` |

The docs are also indexed by [Context7](https://context7.com) (ask your assistant to "use
context7" for Cameleer) and [DeepWiki](https://deepwiki.com/pmsudhi/cameleer). Agents that read
files directly can start from [llms.txt](https://cameleer.ing/llms.txt) or
[llms-full.txt](https://cameleer.ing/llms-full.txt).

## How the pages are organized

Every page starts with a one-sentence answer under its title, then shows a complete route you can
copy. Component pages follow the same layout: when to use it, the URI format, an example in YAML
and in the Rust DSL, the options, the message headers, how to run it with Docker, and common
questions.

Every YAML example is a complete Camel YAML DSL file. Save it as `route.camel.yaml` and run it
with the Docker image:

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/route.camel.yaml
```

Hosts and credentials in examples are written as `{{env:NAME:default}}` placeholders: set the
environment variable (`-e NAME=value` with Docker) or the default is used.

## Common questions

### Are the examples tested?

Yes. The test suite parses every YAML example with the real loader and resolves every endpoint URI,
which rejects unknown options the same way Apache Camel does. Many examples also run end to end
against embedded brokers and mock services.

### Is there a version of these docs for LLMs and coding assistants?

Yes. The site publishes `llms.txt` (a summary with links) and `llms-full.txt` (every page as plain
markdown), and every HTML page has its markdown source next to it under the same name with `.md`.

### Where do I report a mistake in the docs?

Open an issue or a pull request on [GitHub](https://github.com/pmsudhi/cameleer). Each page links
to its source file at the bottom.

## Writing documentation

- One page per component at `components/<scheme>.md`; alias schemes (`https` for `http`) go on
  the page's `**Also:**` line instead of getting their own page. Languages go in
  `languages/<name>.md`, data formats in `dataformats/<name>.md`, guides in `guides/`.
- The first line under the H1 is a blockquote with one plain sentence that answers what the
  page is about; it becomes the page's search description.
- Every ` ```yaml ` block is either a complete Camel route preceded by
  `<!-- example: <unique-id> -->` on the line directly above, or something else (a Compose or
  Kubernetes file) preceded by `<!-- not-camel -->`.
- A "Common questions" section with `###` question headings becomes FAQ data for search engines.
- Links between pages are relative markdown links (`../components/kafka.md`).

Check your changes, then build the site:

```sh
cargo test -p cameleer-main --test docs_harness
python3 scripts/build-docs.py        # writes site/docs/, site/sitemap.xml, site/llms*.txt
python3 -m http.server -d site 8000  # then open http://localhost:8000/docs/
```

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
