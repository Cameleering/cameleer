# Keeping Cameleer std-only while it uses simoom and karez

_Internal plan, 2026-11-09. Companion to [github-organization.md](github-organization.md)._

## What the rule protects

"Std-only" is a claim about the dependency graph: **no code written by a third party**. Every
line that runs is written, reviewed, tested and released under Cameleer's rules: the Rust
standard library and nothing else. Users get a small supply-chain surface, one audit scope,
builds without crates.io (`cargo build --offline` works today), and fixes that never wait on
another project. The public wording ("No third-party crates: every protocol is written on the
Rust standard library", site home page) says exactly that.

simoom already follows the same rule (its CLAUDE.md, rule 1: no external crates, not even
dev-dependencies; OS access through `extern` declarations in `simoom-sys`). karez is at the design
stage and plans to. So **depending on them keeps the claim true**, as long as three things hold:

1. **A closed family.** Cameleer may depend only on crates of the family (`cameleer-*`,
   `simoom*`, `karez*`), and every family crate is itself std-only, recursively.
2. **One set of rules.** The family crates follow the same rules Cameleer does: std-only, zero
   warnings, every fix gets a test, hermetic tests, measured claims, the same licence
   (Apache-2.0).
3. **Enforced, not promised.** CI fails if anything else appears in the graph.

The rule changes from "no external crates" to "**no third-party crates; only the Cameleer family,
which is std-only too**". The claim on the site stays word for word.

## How Cameleer consumes the family crates

| situation | how | why |
|---|---|---|
| releases and `main` | published versions from crates.io: `simoom = "0.3"` | `cargo install cameleer-main` must work; crates.io refuses path or git dependencies |
| developing both at once | `[patch.crates-io] simoom = { path = "../simoom/crates/simoom" }` in an uncommitted `.cargo/config.toml` (or `cargo --config`) | change both sides without publishing; nothing leaks into commits |
| catching breakage early | a scheduled CI job builds and tests Cameleer against simoom's `main` (patch to a git checkout); simoom's CI runs Cameleer's HTTP tests against the PR | neither side learns about a break at release time |

Rejected: **git submodules or a subtree copy** (two copies drift, and the published crate still
needs a crates.io dependency); **git dependencies** (crates.io rejects them); **a monorepo**
(simplest syncing, but simoom and karez lose their own identity, issues and contributors; simoom
also keeps competitor benchmark programs with external crates under `bench/competitors/`, which
would sit inside Cameleer's repository).

## The enforcement check

A test in `crates/cameleer-main/tests/` (so `cargo test --workspace` runs it everywhere) and a CI
step:

- run `cargo metadata --format-version 1 --locked`, and for every package in the resolved graph
  require that its name is on the family allowlist and its source is the workspace or crates.io
  (never another registry or git);
- for crates.io packages, require that the crate's crates.io owners include the org's release team
  (checked in CI with the crates.io API, not in the offline test);
- fail on any build script or proc macro from outside the family.

Then the claim is checked on every commit, the same way documentation examples are.

## The real problem: the same code in two places

simoom was started by porting Cameleer code, and the copies have diverged:

| piece | in Cameleer | in simoom | state |
|---|---|---|---|
| TLS 1.3/1.2, X.509, crypto | `cameleer-tls` | `simoom-tls` | simoom's is faster (P-256 and Ed25519 match or beat OpenSSL) and adds QUIC; Cameleer's has the Camel glue (keystores, `SslContextParameters`, SASL, OCSP) |
| HTTP/2 and HPACK | `cameleer-grpc` `h2`, `hpack` | `simoom-h2`, `simoom-hpack` | separate implementations; Cameleer's had the connection leak fixed on 2026-11-07 (INTEROP bug 19) |
| HTTP/1.1 server | `cameleer-http` | `simoom-http1` | simoom's is the planned replacement |
| sockets and OS calls | `cameleer-tls` `net.rs` (Windows quirks) | `simoom-sys` | both carry the Windows socket fixes |
| PostgreSQL client | `cameleer-sql` `pg` | karez (planned) | karez is design-only today |

Two copies means two security surfaces to audit, fixes made in one and forgotten in the other, and
benchmark claims that depend on which copy a route uses. **Rule: every algorithm has one home.**
The plan to get there:

1. **Freeze the duplicates now.** A security or correctness fix in either copy is ported to the
   other in the same week, listed in both changelogs. Track open ports in one issue per pair.
2. **Pick the home per piece** (proposal):
   - crypto and the TLS state machine: `simoom-tls` (ahead on speed, has QUIC). `cameleer-tls`
     keeps the Camel layer (keystores, `SslContextParameters`, `camel.ssl.*`, SASL, OCSP glue)
     and calls simoom-tls underneath. Its public API stays, so components don't change.
   - HTTP/2, HPACK, HTTP/1.1 server, sockets: simoom. `cameleer-http` and `cameleer-grpc` become
     Camel adapters over simoom (the routing, `platform-http`, REST DSL and gRPC stay in
     Cameleer).
   - database wire protocols: karez, once it passes its go/no-go milestone. Until then Cameleer
     keeps its PostgreSQL client; the embedded `rustsql` engine stays in Cameleer.
3. **Switch one piece at a time, behind a feature**, with the hermetic suite, the live, official
   and simulator tiers, and the DigitalOcean benchmarks before and after. Each switch is its own
   Cameleer release; the old copy is deleted one release later.

## Release and version logistics

- **Order:** a family crate is published before the Cameleer release that needs it. Cameleer's
  `scripts/publish-crates.sh` refuses to run if a family dependency version is not on crates.io.
- **Versions:** family crates stay 0.x with semver. Cameleer uses caret requirements
  (`simoom = "0.3"`) and upgrades across a breaking version on purpose, in its own commit with the
  benchmarks rerun. `Cargo.lock` stays committed, so the binary and image are reproducible.
- **Rust version:** one MSRV policy for the family (simoom says 1.89; align Cameleer's
  `rust-version` with it).
- **Features and size:** simoom's HTTP/3, QUIC and HTTP/2 stay optional features. Cameleer's
  `core` build pulls only HTTP/1.1, so its size budget (5.8 MB) still holds; `http2`/`full` turn on
  the rest.
- **`unsafe`:** simoom confines it to `simoom-sys`. Adopt the same rule in Cameleer over time (its
  allocator and Windows socket code), so the family can claim it as a whole.
- **Offline builds:** today Cameleer builds with no network. With family crates from crates.io,
  the first build fetches them. Keep offline builds possible: a release source tarball with
  `cargo vendor` output, and document `cargo build --offline` after one fetch. The Docker build
  fetches from crates.io in the builder stage, as any Rust image does.
- **Issues:** a bug found through Cameleer in the HTTP layer is filed in simoom and linked from
  Cameleer's issue; the Cameleer release that picks up the fix says so.
- **Claims:** Cameleer's HTTP numbers change when it switches to simoom. Rerun the published
  benchmarks and update `docs/BENCHMARKS.md` and the site in the same release.

## What changes in the rules files

- Cameleer `CLAUDE.md` rule 1: "Std-only. No third-party crates. Dependencies are allowed only on
  Cameleer family crates (`cameleer-*`, `simoom*`, `karez*`), which are std-only under the same
  rules; the `family_only_dependencies` test enforces it. Any other dependency needs the owner's
  approval."
- simoom's and karez's `CLAUDE.md`: the same wording, plus "a fix to code that also exists in
  Cameleer is ported there in the same week until the duplicate is removed".
- Site and README: keep "No third-party crates"; add one line where the family is presented:
  "simoom and karez are Cameleer's own std-only crates."

## Order of work

1. Now: add the enforcement test to Cameleer (it passes today: the graph is workspace-only), and
   the rule text to all three `CLAUDE.md` files.
2. Now: the duplicate list above as tracked issues, with the port-within-a-week rule.
3. When simoom has a working alpha: publish simoom 0.1 from the org, then switch Cameleer's
   HTTP/1.1 server behind a feature, benchmark, release.
4. Then TLS (largest audit win), then HTTP/2. karez after its milestone.
