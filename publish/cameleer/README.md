This crate is a thin wrapper of [`cameleer-main`](https://crates.io/crates/cameleer-main): it installs the `cameleer` command and re-exports the `cameleer-main` API.

# Cameleer

Camel routes in Rust. The reliable action layer for AI agents.

Cameleer is Apache Camel's integration model, rebuilt in Rust: if you know Apache Camel, you already
know Cameleer. The same endpoint URIs, Enterprise Integration Patterns, Simple expressions, YAML
routes, headers and error handling, with about 230 components, languages and data formats, and
**no dependencies beyond the Rust standard library**.

```sh
cargo install cameleer          # the `cameleer` command
cameleer run hello.camel.yaml   # run Camel YAML, XML or Rust routes
cameleer run tools.camel.yaml --mcp   # serve routes as MCP tools
```

```sh
cargo add cameleer              # or use it as a library (the cameleer-main API)
```

Features are forwarded to `cameleer-main`: the default is `full`; for a smaller build use
`cargo install cameleer --no-default-features --features cloud`, and `--features health` adds the
health pack (HL7, FHIR, DICOM).

- Documentation: <https://cameleer.ing/docs/>
- Home: <https://cameleer.ing/cameleer/>
- Source: <https://github.com/cameleering/cameleer> (Apache-2.0)
