# AGENTS.md

Instructions for AI coding agents working with Cameleer, Apache Camel rewritten in Rust (same
routes, EIPs, endpoint URIs, Simple language and YAML DSL; one native binary, no JVM).

## Writing routes

- Read [docs/guides/yaml-for-coding-agents.md](docs/guides/yaml-for-coding-agents.md) first: a
  checklist, tested skeletons (consumer, REST API, scheduled job, error handling) and the rules
  that prevent the usual mistakes.
- The full syntax is in [docs/guides/yaml-dsl.md](docs/guides/yaml-dsl.md): every top-level
  element, step, key and expression form the YAML loader accepts.
- Endpoint options: `docs/components/<scheme>.md` (for example
  [docs/components/kafka.md](docs/components/kafka.md)), or `cameleer catalog components`.
  Use only options listed there; never invent one. Languages are in `docs/languages/`, data
  formats in `docs/dataformats/`.
- Secrets and hosts go in placeholders: `{{env:NAME:default}}` or `{{property:default}}`.
- Name route files `<purpose>.camel.yaml`.

## Validate, then run

```sh
cameleer validate route.camel.yaml --format json   # fix every finding, repeat until "valid": true
cameleer run route.camel.yaml --max-seconds 10
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer validate /routes
```

`cameleer validate` never starts routes or connects to anything. Codes, output formats and
exit codes: [docs/guides/validate.md](docs/guides/validate.md).

To give an AI agent tools, make routes `ai-tool` endpoints and serve them with
`cameleer run --mcp` (or `--mcp-stdio` for a desktop client); test them with
`cameleer test` ([docs/guides/mcp.md](docs/guides/mcp.md),
[examples/agents/mcp-shop](examples/agents/mcp-shop/README.md)).

The image and `cargo install cameleer-main` include every component. If a run fails with
``this cameleer binary was built without the `x` feature``, the binary is a reduced build: use the
full image or rebuild with that feature added
([docs/guides/build-features.md](docs/guides/build-features.md)). Each component page names its
feature in its header line.

## Changing Cameleer itself

- **Standard library only.** Do not add a dependency to any `Cargo.toml`.
- Follow Apache Camel's names and semantics for EIPs, options, headers and exception kinds.
- `cargo build --workspace` must produce zero warnings; `cargo test --workspace` must pass.
  Every fix and feature gets a hermetic test (embedded servers, never the network).
- Documentation examples are tests: every ```yaml block in `docs/` is marked
  `<!-- example: id -->` (or `<!-- not-camel -->`) and is checked by
  `cargo test -p cameleer-main --test docs_harness`; the guide examples run end to end in
  `crates/cameleer-main/tests/docs_*.rs`.
- Internals: [ARCHITECTURE.md](ARCHITECTURE.md). Feature list: [docs/FEATURES.md](docs/FEATURES.md).
  Ground rules: [CONTRIBUTING.md](CONTRIBUTING.md).

## Found a problem?

If Cameleer does not do what its docs say, or you hit a bug while building with it, please fix
it rather than only working around it:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request.

## Connect your coding agent to the docs

The docs are served as an MCP server at `https://gitmcp.io/pmsudhi/cameleer` (Claude Code:
`claude mcp add --transport http cameleer-docs https://gitmcp.io/pmsudhi/cameleer`; other tools:
see the README's "Connect your coding agent" table). They are also indexed by Context7 and
DeepWiki, and the full text is in https://cameleer.ing/llms-full.txt.
