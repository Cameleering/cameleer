# Security policy

rust-camel implements its own TLS, X.509 validation, SSH, cryptography and protocol parsers on
the Rust standard library, so security reports are especially welcome.

## Reporting a vulnerability

Please **don't open a public issue**. Report it privately through GitHub:
**Security → Report a vulnerability** on
[github.com/pmsudhi/Rust-camel](https://github.com/pmsudhi/Rust-camel/security/advisories/new).

Include the affected crate and version (or commit), steps or a route to reproduce, and the
impact you expect. We aim to acknowledge reports within a few days, agree on a fix and a
disclosure date with you, and credit you in the advisory unless you prefer otherwise.

## Supported versions

The project is pre-1.0. Fixes go into the `main` branch.

## Scope

In scope: the crates in this repository, including `camel-tls` (TLS 1.3/1.2, X.509),
`camel-ssh`, the XML security and crypto data formats, and the protocol and data-format
parsers. The Java benchmark harness in `benchmarks/java-camel` is out of scope; report
Apache Camel issues to the [Apache Camel project](https://camel.apache.org/security/).

An independent audit and fuzzing of the from-scratch crypto and parsers is planned; see
[docs/ROADMAP.md](docs/ROADMAP.md).
