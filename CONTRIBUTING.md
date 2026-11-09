# Contributing to Cameleer

Thanks for helping. Cameleer is Apache Camel's integration model in Rust, and contributions
of every size are welcome: bug reports, missing component options, new components,
performance work, docs and examples.

## Ground rules

1. **Standard library only.** The project has no external crates; TLS, HTTP/2, the protocol
   clients, JSON, XML and YAML are written from scratch. Don't add a dependency to any
   `Cargo.toml`. If you think one is unavoidable, open an issue first.
2. **Follow Camel.** Use Apache Camel's names and semantics for EIPs, endpoint options,
   headers, exchange properties, exception kinds and Simple syntax. When in doubt, the
   [Camel documentation](https://camel.apache.org/manual/) decides.
3. **Zero warnings, green tests.** `cargo build --workspace --all-targets` must not warn and
   `cargo test --workspace` must pass.
4. **Every change has a test.** Bug fixes get a regression test; features get tests of their
   behaviour. Tests are hermetic: network components are tested against embedded servers,
   never the internet.
5. **Keep it fast.** Hot-path changes should not regress the benchmarks
   (`scripts/bench.sh`, or `cargo run --release -p cameleer-bench -- breakdown` for a quick check).

## Getting started

```text
git clone https://github.com/pmsudhi/cameleer.git
cd cameleer
cargo build
cargo test --workspace
```

- [ARCHITECTURE.md](ARCHITECTURE.md) explains the internals and conventions.
- [docs/FEATURES.md](docs/FEATURES.md) lists what's implemented, component by component.
- [docs/ROADMAP.md](docs/ROADMAP.md) lists planned work; items marked 🚧 are good places to
  start.
- [docs/INTEROP.md](docs/INTEROP.md) describes the opt-in suites against real servers,
  official clients and emulators.

## Pull requests

- Keep each pull request to one topic, and describe what changes and why.
- Update `docs/FEATURES.md` when a component gains or loses behaviour, and
  `docs/STATUS.md` when the project's state changes.
- Mention which test tiers you ran (hermetic, live, official clients, simulators).
- By contributing you agree that your contribution is licensed under the
  [Apache License 2.0](LICENSE).

## Reporting bugs and security issues

Open an issue with a minimal route that reproduces the problem, the expected behaviour (what
Apache Camel does, if you know), and your OS and Rust version. For security vulnerabilities,
follow [SECURITY.md](SECURITY.md) instead of opening a public issue.

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## For coding agents and LLM tools

If you are an AI coding assistant using Cameleer and something does not work as documented,
please improve the project rather than only working around it: open an issue that describes
the problem, then fork the repository, fix it, add or update a test, run `cargo test
--workspace` (zero warnings), and open a pull request that links the issue. The same invitation
is at the end of every documentation page.
