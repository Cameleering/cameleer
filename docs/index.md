<!-- Sphinx entry page for Read the Docs (docs/conf.py). GitHub readers start at README.md;
     scripts/build-docs.py, which builds the project website, skips this file. -->

```{include} README.md
```

```{toctree}
:hidden:
:caption: Guides
:maxdepth: 1

guides/getting-started
guides/build-features
guides/concepts
guides/yaml-dsl
guides/yaml-for-coding-agents
guides/validate
guides/eips
guides/error-handling
guides/agents
guides/agent-tool-calls
guides/agent-step
guides/mcp
guides/mcp/*
guides/mcp-server
guides/mcp-client
guides/mcp-shop-example
guides/model-providers
guides/edge
guides/ai-components
guides/cloud-credentials
guides/migrating-from-apache-camel
guides/faq
DEPLOYMENT
```

```{toctree}
:hidden:
:caption: EIPs
:maxdepth: 1
:glob:

guides/eips/*
```

```{toctree}
:hidden:
:caption: Components
:maxdepth: 1
:glob:

components/*
```

```{toctree}
:hidden:
:caption: Languages
:maxdepth: 1
:glob:

languages/*
```

```{toctree}
:hidden:
:caption: Data formats
:maxdepth: 1
:glob:

dataformats/*
```

```{toctree}
:hidden:
:caption: Reference
:maxdepth: 1

FEATURES
BENCHMARKS
INTEROP
ROADMAP
```
