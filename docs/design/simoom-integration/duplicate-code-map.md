# Duplicate code between Cameleer and simoom

_Internal analysis, 2026-11-09. Trees measured: Cameleer `main` at `b91bd70`, simoom `main` at
`1e50579`. karez has no Rust code yet (design documents only), so it is not in the map._

We plan to make simoom Cameleer's HTTP engine and to give every algorithm one home. This page
has three parts:

1. a method for finding duplicated code across the repositories that can run again and again
   without adding a dependency to either one;
2. the first duplicate map, with the fixes that exist in one copy and not the other;
3. the design of a repeatable check for Cameleer's CI.

## 1. Method

### Tools

| tool | status on this machine |
|---|---|
| `ripwire` (duplicate-body detection) | not on `PATH` |
| jscpd, PMD CPD | not installed; nothing was downloaded |
| Python 3.12 (standard library only) | used |

The measurement uses a throwaway, standard-library-only Python detector kept outside both
repositories:

- `C:\Users\SUDHEE~1\AppData\Local\Temp\claude\db2e2ca09febcb70\3f5a9334-3de8-4b69-b298-43e8afd1fffe\scratchpad\dup\dupmap.py`
  does the whole-tree scan;
- `...\scratchpad\dup\pair.py` (same folder) compares chosen files or directories;
- the raw results are in `...\scratchpad\dup\out\` (`files.tsv`, `functions.tsv`,
  `fn_by_file.tsv`).

A full run over both trees (641 Cameleer files and 2.8 M tokens; 134 simoom files and 0.37 M
tokens) takes 21 s.

### Algorithm

1. **Scope.** The detector reads every `crates/*/src/**/*.rs` file in both trees. It leaves out
   `tests/`, `benches/`, `examples/`, `fuzz/`, `bench/competitors/` and the `target*`
   directories. Inside each file it removes `#[cfg(test)]` modules and `#[test]` functions, so
   the map covers production code only.
2. **Tokenize** with a small Rust lexer. It handles nested block comments, raw strings, byte
   strings, and the difference between lifetimes and characters. Comments and whitespace are
   dropped. Each token is kept in two forms:
   - *exact*: the token text, used for type-1 clones;
   - *normalized*: every identifier that isn't a keyword or primitive type becomes `I`, numbers
     become `N`, strings `S`, characters `C` and lifetimes `'L`. Macro names are kept. This form
     is used for type-2 clones (renamed) and type-3 clones (near-miss).
3. **Functions.** For every `fn` that has a body, the detector extracts the token span and drops
   bodies under 40 tokens. Each body gets three fingerprints:
   - a hash of its exact tokens;
   - a hash of its normalized tokens;
   - the set of normalized 25-token k-grams.

   A simoom function counts as:
   - an **exact** clone when its exact hash appears in Cameleer;
   - a **renamed** clone when only its normalized hash matches;
   - a **near-miss** clone when at least 50% of the smaller function's k-grams appear in one
     Cameleer function. The search goes through an inverted index, and k-grams that appear in
     more than 50 functions are ignored.

   Roll-ups count only functions of 60 tokens or more, so `fmt` impls and one-line getters don't
   count.
4. **Files.** Each file's normalized k-grams (k = 25) are *winnowed* as in MOSS (window w = 8,
   rightmost minimum). Fingerprints that occur in more than 20 files are treated as boilerplate
   and dropped. For each simoom file the detector:
   - ranks the Cameleer files that share fingerprints with it;
   - reports the shared fraction on each side (**containment**);
   - reports **exact containment** from the same procedure run on exact tokens;
   - reports **lines in common**: the source lines covered by the shared k-grams.

   Pairs under 8% containment are noise: shared idioms such as `match` ladders and
   `Result<Vec<u8>, String>` plumbing. They are filtered out.

**How to read the numbers.** Containment is computed for the smaller side, so a small file
copied whole into a large file scores 1.0. "Lines in common" is an upper bound when a pair
scores low, because one shared 25-token window covers several lines. Exact containment that
stays close to normalized containment means the code was copied. Normalized containment far above
exact containment means it was copied and then renamed or reformatted.

## 2. The duplicate map

### Size of the overlap

| measure | value |
|---|---|
| simoom production code | 41,429 lines, of which simoom-tls is 8,655 |
| Cameleer production code in the ported crates | cameleer-tls 11,403; cameleer-grpc 6,532; cameleer-http 4,634; cameleer-bench 2,086 lines |
| file pairs above the noise floor | 25 |
| simoom lines in those pairs | about 3,480, of which 3,235 are in simoom-tls (37% of the crate) |
| function clones of 60 tokens or more | 82 exact, 12 renamed, 55 near-miss |
| lines in exact and renamed function clones | 1,489 |
| lines in near-miss function clones | 1,065 |

**In short:** the X.509, ASN.1, PEM, key-format and public-key arithmetic layer was copied almost
whole (about 2,500 lines at 50-100% containment). The symmetric primitives and the TLS 1.3
handshake were partly copied and then rewritten. HTTP/1.1, HTTP/2 connection handling, sockets,
the OS calls and the benchmark tools were rewritten from scratch, and share almost nothing but a
few helpers.

### Crypto primitives

Columns: exact and normalized containment; lines in common out of the simoom file's lines.

| simoom (`simoom-tls/src/`) | Cameleer | exact | norm | lines | ahead | notes |
|---|---|---|---|---|---|---|
| `crypto/rsa.rs` | `cameleer-tls/src/crypto/rsa.rs` | 0.93 | 0.97 | 354/359 | **simoom** for speed, **Cameleer** for features | simoom: CRT reduction without byte round-trips. Cameleer only: RSAES-OAEP and SHA-1 DigestInfo (XML Encryption, PGP) |
| `crypto/ec.rs` | `cameleer-tls/src/crypto/ec.rs` | 0.84 | 0.84 | 361/396 | simoom | Generic curve code; simoom now uses it for P-384 only |
| `crypto/p256.rs` | (`ec.rs`) | 0.00 | 0.00 | 0/674 | **simoom** | New dedicated P-256 with MULX/ADX field arithmetic in `simoom-sys::arch` |
| `crypto/bigint.rs` | `cameleer-tls/src/crypto/bigint.rs` | 0.83 | 0.60 | 233/301 | simoom | Montgomery multiplication moved to `simoom-sys::arch::bigint_mont_mul` (assembly-level intrinsics) |
| `crypto/ed25519.rs` | `cameleer-tls/src/crypto/ed25519.rs` (and its copy in `cameleer-ssh`) | 0.31 | 0.37 | 180/309 | simoom | Point encoding and scalar code shared; field arithmetic moved to `f25519.rs` |
| `crypto/f25519.rs` | `cameleer-tls` `ed25519.rs`, `x25519.rs` | 0.11-0.15 | 0.10 | 56/221 | simoom | Radix 2^64 field elements with `arch::fe25519_*` |
| `crypto/x25519.rs` | `cameleer-tls/src/crypto/x25519.rs` | 0.07 | 0.11 | 32/140 | simoom | Ladder rewritten over `f25519` |
| `crypto/sha2.rs` | `cameleer-tls/src/crypto/hash.rs` | 0.26 | 0.17 | 101/368 | simoom | SHA-NI path (`arch::sha256_blocks`). Cameleer's TLS SHA-256 is scalar; Cameleer has SHA-NI only in `cameleer-core/src/language/support.rs` |
| `crypto/chacha.rs` | `cameleer-tls/src/crypto/chacha.rs` | 0.16 | 0.10 | 77/374 | simoom | SIMD ChaCha20 and 4-way Poly1305 in `arch` |
| `crypto/aes.rs`, `gcm.rs` | `cameleer-tls/src/crypto/aes.rs` | 0.01-0.02 | 0.02-0.04 | 18/330 | **simoom** | Rewritten: AES-NI, CLMUL and VAES GCM. Cameleer only: CBC/CTR and the other legacy modes the data formats use |
| `crypto/rng.rs` | `cameleer-tls/src/crypto/rng.rs` | 0.24 | 0.36 | 27/57 | even | Both seed from the OS; simoom goes through `arch::os_random` |
| `util.rs` | `cameleer-tls/src/util.rs` | 0.38 | 0.34 | 20/47 | even | Hex, constant-time compare |

### X.509, ASN.1, keys and PEM

| simoom (`simoom-tls/src/`) | Cameleer (`cameleer-tls/src/`) | exact | norm | lines | ahead | notes |
|---|---|---|---|---|---|---|
| `asn1.rs` | `asn1.rs` | 0.97 | 0.97 | 310/312 | even | simoom: `Cow<'static, str>` errors (smaller binary) |
| `pem.rs` | `pem.rs` | 1.00 | 1.00 | 96/98 | even | Identical |
| `x509.rs` | `x509.rs` | 0.91 | 0.92 | 511/527 | **Cameleer** for features, simoom for one check | Cameleer only: CRL parsing and revocation (`verify_chain_with_crls`), `CertError::Revoked`. simoom only: the chain length is checked before any certificate is parsed |
| `certgen.rs` | `certgen.rs` | 0.85 | 0.89 | 180/194 | even | Test certificate generator |
| `keys.rs` | `keys.rs` | 0.59 | 0.50 | 277/435 | mixed | SPKI, PKCS#8, SEC1 and PKCS#1 parsing shared (13 functions exact or renamed, 6 near-miss). simoom adds key-type features (`rsa`, `p384`, `ed25519`). Cameleer's keystores (`keystore/jks.rs`, `pkcs12.rs`), `pbe.rs` and `ocsp.rs` have no simoom counterpart |

### TLS handshake and record layer

| simoom (`simoom-tls/src/tls/`) | Cameleer (`cameleer-tls/src/tls/`) | exact | norm | lines | ahead | notes |
|---|---|---|---|---|---|---|
| `mod.rs` | `mod.rs` | 0.10 | 0.14 | ≤164/400 | — | Constants, names, `cv_content` |
| `codec.rs` | `codec.rs` | 0.33 | 0.33 | 85/160 | even | Length-prefixed writers and ALPN lists are exact copies |
| `keyschedule.rs` | `keyschedule.rs` | 0.06 | 0.27 | 47/85 | even | Same HKDF schedule, renamed |
| `server.rs` | `server.rs` | 0.05 | 0.10 | ≤124/511 | split | simoom: sans-io TLS 1.3 only. Cameleer: TLS 1.2 and 1.3, blocking |
| `client.rs` | `client.rs` | — | 0.07 | ≤87/436 | Cameleer | Cameleer: TLS 1.2, OCSP stapling, resumption against Go. simoom's client exists for tests and the load generator |
| `conn.rs`, `engine.rs` | `conn.rs`, `stream.rs` | 0.02 | 0.02 | 24/620 | simoom (hardening) | Rewritten as a sans-io engine. See the divergence table: record-flood bound |
| `quic.rs` | — | — | — | 0/328 | simoom | QUIC TLS interface, new |

### HPACK and HTTP/2

| simoom | Cameleer | exact | norm | lines | ahead | notes |
|---|---|---|---|---|---|---|
| `simoom-hpack/src/huffman.rs` | `cameleer-grpc/src/hpack/huffman_table.rs` | 1.00 (Cameleer side) | 1.00 | 67/174 | even | The RFC 7541 Huffman table, copied byte for byte |
| `simoom-hpack/src/huffman.rs` | `cameleer-grpc/src/hpack/huffman.rs` | 0.15 | 0.19 | 24/174 | simoom | Encoder near-miss, decoder rewritten |
| `simoom-hpack/src/lib.rs` | `cameleer-grpc/src/hpack/mod.rs` | 0.12 | 0.24 | 44/129 | simoom | Integer coding (near-miss 0.72/0.79). Tables, encoder and decoder rewritten |
| `simoom-h2/src/frame.rs` | `cameleer-grpc/src/h2/frame.rs` | 0.18 | 0.14 | 70/181 | simoom | Frame header layout |
| `simoom-h2/src/conn.rs` | `cameleer-grpc/src/h2/conn.rs` | 0.00 | 0.00 | 0/1393 | **simoom** | Rewritten sans-io, with DoS budgets (`budget.rs`). Cameleer has a CONTINUATION size cap only |
| `simoom-h2/src/upgrade.rs` | `cameleer-http/src/http.rs` | 0.05 | 0.19 | 18/42 | — | Only the base64url helper |

### HTTP/1.1, sockets and OS calls, benchmark tools

| simoom | Cameleer | exact | norm | notes |
|---|---|---|---|---|
| `simoom-http1/src/*` | `cameleer-http/src/*` | 0.00 | 0.01 | Rewritten (SIMD scan, strict parser). Shared helpers only: the reason-phrase table (near-miss 1.0 with `cameleer-cloud/src/http.rs` `reason`), SHA-1 (0.73 with `cameleer-protocols/src/websocket/sha1.rs`) and base64 |
| `simoom-sys/src/net.rs`, `os/windows.rs` | `cameleer-tls/src/net.rs` | 0.00 | 0.00 | Rewritten: raw `extern` declarations instead of std sockets |
| `simoom-sys/src/alloc.rs` | `cameleer-core/src/alloc.rs` | 0.00 | 0.01 | Unrelated designs |
| `simoom-sys/src/driver/iocp.rs` | `cameleer-bench/src/rawiocp.rs` | 0.00 | 0.01 | Pattern reused, no code |
| `simoom-bench/src/hdr.rs` | `cameleer-bench/src/hist.rs` | 0.08 | 0.10 | HdrHistogram rewritten. Contains `adler32`, an **exact** copy from `cameleer-core/src/dataformat/compress.rs`, and `base64_encode` (near-miss 0.93 with `cameleer-http/src/http.rs`) |
| `simoom-bench/src/load.rs`, `raw.rs`, `coldstart.rs` | `cameleer-bench/src/loadgen.rs`, `rawhttp.rs`, `rawiocp.rs`, `coldstart.rs` | 0.00 | 0.00 | Rewritten (open-loop wrk2 semantics) |

### Algorithms duplicated within each repository

The goal is one home per algorithm, so copies inside each tree count too. Each of these came up
because it matched a simoom file. The counts come from a scan for each algorithm's constants:
the SHA-256 K table, the SHA-1 initial state, the base64 alphabet, the ChaCha constant, and `fn
hmac`.

| algorithm | Cameleer copies | simoom copies |
|---|---|---|
| Ed25519 | `cameleer-tls/src/crypto/ed25519.rs` and `cameleer-ssh/src/crypto/ed25519.rs`: **0.98 identical, 392 lines each** | `simoom-tls` |
| SHA-256/512 | `cameleer-tls/src/crypto/hash.rs`, `cameleer-sql/src/pg/crypto.rs` (SCRAM), `cameleer-core/src/language/support.rs` (SHA-NI) | `simoom-tls/src/crypto/sha2.rs`, `simoom-sys/src/arch.rs` (SHA-NI) |
| SHA-1 | `cameleer-tls/src/crypto/hash.rs`, `cameleer-core/src/language/support.rs`, `cameleer-cloud/src/util.rs`, `cameleer-protocols/src/websocket/sha1.rs` | `simoom-http1/src/sha1.rs` |
| HMAC | `cameleer-tls`, `cameleer-sql`, `cameleer-cloud`, `cameleer-grpc` (`auth.rs`), `cameleer-ssh`, `cameleer-script` | `simoom-tls`, `simoom-quic` (two) |
| base64 | 8: `cameleer-tls` pem, `cameleer-core` dataformat, `cameleer-http`, `cameleer-amqp10`, `cameleer-saas`, `cameleer-script` (two), `cameleer-yaml` | 4: `simoom-tls` pem, `simoom-http1/src/b64.rs`, `simoom-h2` upgrade, `simoom-bench` hdr |
| adler32 | `cameleer-core/src/dataformat/compress.rs` | `simoom-bench/src/hdr.rs` (exact copy) |

### Divergence: fixes in one copy and not the other

| # | fix | Cameleer | simoom | action |
|---|---|---|---|---|
| D1 | **TLS record flood**: no bound on records that deliver nothing (empty application data, `user_canceled` or TLS 1.2 warning alerts, KeyUpdate requests). simoom SECURITY.md item 13 | **Missing.** `cameleer-tls/src/tls/conn.rs` ignores these in a loop with no counter, and answers every KeyUpdate request | Fixed: `MAX_IDLE_RECORDS = 32` (`tls/conn.rs` `idle_record`) | **Port to cameleer-tls.** It affects every Cameleer TLS server and client, not only HTTP |
| D2 | **HTTP/2 DoS budgets**: Rapid Reset (CVE-2023-44487), RST/PING/SETTINGS/WINDOW_UPDATE floods, MadeYouReset, 431 instead of GOAWAY for an oversized header block, late frames on forgotten closed streams, the peer's `MAX_HEADER_LIST_SIZE` on output (simoom bugs 9, 10; SECURITY 4, 22) | **Missing.** `cameleer-grpc/src/h2/conn.rs` checks frame shapes and caps the CONTINUATION block, but counts nothing. It starts a handler per stream, so Rapid Reset is likely to hurt (not tested) | Fixed: `simoom-h2/src/budget.rs`, `Limits::h2_frame_budgets`; h2spec 146/146 | Move the gRPC and `H2Server` servers to simoom-h2 first, or port the budgets now. This is the largest security gap the map found |
| D3 | X.509: refuse an oversized chain before parsing it (`chain.len() > MAX_PATH_DEPTH + 2`) | Missing. The depth is checked during path building, after every certificate is parsed (still bounded by the handshake message cap) | Present (`x509.rs` `verify_chain`) | Port; one line plus a test |
| D4 | TLS alert lost to a TCP reset (Cameleer INTEROP bug 18, `d799bd3`) | Fixed: `net::lingering_close`, `accept_tls` | Fixed on its own (simoom BUGS 20, `Config::linger` in simoom-rt) | None. Keep both regression tests. Cameleer's `lingering_close` stays for its non-HTTP servers |
| D5 | HTTP/2 writer thread leak on peer close (INTEROP bug 19, `07ef8d2`) | Fixed | Not applicable: sans-io, no writer thread | None |
| D6 | RST_STREAM(NO_ERROR) after a complete response (RFC 9113 §8.1, INTEROP bug 20, `b91bd70`) | Fixed in the client | simoom-h2 has a client role (used by the load generator). It delivers the response events before `Event::Reset`, and the load generator drops a stream once it ends, so the response is kept. No test covers it, and the reset counts against `Budget::RstReceived` | Port Cameleer's regression test to simoom-h2 in the client role |
| D7 | `psk_key_exchange_modes` in the first ClientHello (INTEROP official-client bug 19) | Fixed | Present (`tls/client.rs`) | None |
| D8 | TLS 1.3 downgrade sentinels in the client (simoom SECURITY 14) | Was always present (`tls/client.rs`, `DOWNGRADE_TLS12`) | Lost in the rewrite, then fixed | None. It shows that rewrites drop fixes; see section 3 |
| D9 | Alert written under the wrong key after a refused client certificate (simoom SECURITY 12) | Not affected: `mutual_tls_rejects_missing_and_untrusted_certificates` reads the alert | Bug introduced by the rewrite, fixed | None |
| D10 | Features only in Cameleer: CRL revocation, RSA-OAEP, SHA-1 signatures, TLS 1.2, OCSP, JKS and PKCS#12 keystores, PBE, legacy AES modes | Present | Removed (simoom known gaps) | The merged home keeps them, behind cargo features |
| D11 | Speed only in simoom: AES-NI, CLMUL and VAES GCM; SHA-NI in TLS; P-256 with MULX/ADX; radix 2^64 Curve25519; SIMD ChaCha20 and Poly1305; Montgomery multiplication; leaner RSA CRT | Absent (except the SHA-NI SHA-256 in `cameleer-core`) | Present | Port direction: simoom to the home |

**Which copy is ahead:**

- **simoom** leads on speed in every primitive and on hardening in the TLS record layer and
  HTTP/2.
- **Cameleer** leads on breadth: TLS 1.2, revocation, legacy algorithms, keystores and
  interoperability fixes against Go and JSSE.
- **Even:** the ASN.1, PEM and certificate-generation code is the same on both sides.

## 3. A repeatable check for Cameleer's CI

### Shape

The check is a standard-library Python script, `scripts/check-duplicates.py`. Python is already
used by `scripts/build-docs.py` and `scripts/update-tzdata.py`. A script beats a Rust test here:

- it needs no build, and runs in about 20 s;
- it never touches `cargo test --workspace`, which must stay hermetic and would otherwise need a
  simoom checkout;
- the detector above is the prototype.

Cameleer could also add a gated Rust test (`CAMEL_DUPLICATES=<path>`) in the same style as
`CAMEL_LIVE_INTEROP`, but that is optional.

```
python3 scripts/check-duplicates.py \
    --tree cameleer=. \
    --tree simoom=../simoom            # repeatable; karez=... once it has code
    [--baseline scripts/duplicates/baseline.json] \
    [--update-baseline] [--json] [--self]   # --self: clones inside one tree too
```

A `--tree` can also point at published crate sources: `~/.cargo/registry/src/*/simoom-tls-X.Y.Z`
after a `cargo fetch` of the version in `Cargo.lock`. This matters once Cameleer depends on
simoom, because the copy that counts is then the version Cameleer ships, not simoom's `main`.

### Inputs

- **The trees.** The same scope rules as the prototype: `src/` only, tests and `cfg(test)`
  removed, `target*` and `bench/competitors` skipped.
- **`scripts/duplicates/baseline.json`.** This is the tracked list. It is committed in Cameleer
  and reviewed like code. One entry per known duplicate looks like this:

```json
{
  "id": "x509-verify-chain",
  "area": "x509",
  "home": "simoom-tls",                 // where the algorithm should end up
  "status": "tracked",                  // tracked | allowed | removing
  "sides": {
    "cameleer": {"file": "crates/cameleer-tls/src/x509.rs", "fn": "verify_chain",
                 "exact": "b3:9f1c...", "norm": "b3:77a0...", "commit": "b91bd70"},
    "simoom":   {"file": "crates/simoom-tls/src/x509.rs",  "fn": "verify_chain",
                 "exact": "b3:2d4e...", "norm": "b3:77a0...", "commit": "1e50579"}
  },
  "similarity": 0.92,
  "note": "simoom checks chain length first (D3); Cameleer has CRLs"
}
```

  An entry can also name a whole file instead of a function, for tables and other code that is
  not inside a function. The hashes are BLAKE2b of the token streams (`hashlib`, standard
  library).
- **Allow rules.** These live in the same file: generic functions (`fmt`, `name`, `reason`,
  one-line getters), and fingerprints shared by more than N files.

### Thresholds (starting values, tuned on this map)

| signal | rule |
|---|---|
| function clone | ≥ 60 normalized tokens, and ≥ 0.80 k-gram containment (exact or renamed always counts) |
| file clone | ≥ 0.30 normalized containment on either side and ≥ 50 lines in common |
| noise floor | k = 25 tokens, w = 8; fingerprints in more than 20 files are ignored; pairs under 0.08 are never reported |
| divergence warning | a tracked pair whose similarity falls by more than 0.15 since the baseline |

### Algorithm

1. Tokenize, normalize, extract functions and winnow, as in section 1.
2. **Match every baseline entry to the current trees.** Look up the function by name in the
   named file first, then anywhere by normalized hash, then by best k-gram match above 0.5. This
   way a function that moves or is renamed is followed, not reported as lost.
3. For every tracked entry, compare the current **exact-token** hashes with the baseline. Exact
   tokens ignore comments and formatting but notice a renamed variable or a changed constant.

   | result | meaning |
   |---|---|
   | both sides unchanged | OK |
   | **one side changed** | **port-the-fix alarm**. The report gives the changed side, a unified diff of the function, and `git log --oneline <baseline commit>..HEAD -- <file>` for that side |
   | both sides changed | re-review: warns, but doesn't fail |
   | one side gone | progress (the duplicate was removed). Prompts you to drop the entry |
4. **Scan for new clones** between the trees (and inside each tree with `--self`). Any pair above
   the thresholds that is not in the baseline is a **new clone**.
5. Exit codes, in the same style as `cameleer validate`:

   | code | meaning |
   |---|---|
   | 0 | clean, or warnings only |
   | 1 | at least one new clone or port-the-fix alarm |
   | 2 | usage error, or a tree is missing |

   `--update-baseline` writes the current hashes and commits after a human has ported the fix or
   accepted the change.

### Output

Text by default; `--json` gives the same records for tooling. On GitHub Actions each finding is
also printed as `::warning file=...,line=...::` or `::error ...::`, so it shows up on the run.

```
port-the-fix  x509-verify-chain  simoom changed, cameleer unchanged since 1e50579
  simoom  crates/simoom-tls/src/x509.rs:497-540 verify_chain   (similarity 0.92 -> 0.88)
  commits: 1a2b3c4 X.509: refuse chains longer than the depth limit before parsing
  diff: ...
new-clone     0.94 renamed  crates/cameleer-http/src/h1.rs:120-188 parse_chunk_size
                            ~ crates/simoom-http1/src/body.rs:40-101 chunk_size
divergence    hpack-integer  similarity 0.79 -> 0.61 (both sides changed)
summary: 31 tracked (29 ok, 1 port-the-fix, 1 divergence), 1 new clone, 2 removed
```

### Running it in CI

A nightly workflow in Cameleer, `.github/workflows/duplicates.yml`:

1. Check out `cameleering/cameleer` at `main`, and `cameleering/simoom` (public) at `main` into
   `simoom/`. Once Cameleer depends on simoom, also check the published version (`cargo fetch`,
   then point `--tree` at the registry sources).
2. Run `python3 scripts/check-duplicates.py --tree cameleer=. --tree simoom=simoom --self`.
3. On exit 1, fail the job and upload the JSON report as an artifact. The job only reads both
   repositories; it needs no secrets.

Two smaller jobs fit around it:

- **Pull requests.** Cameleer PRs that touch tracked files run the same script in report-only
  mode, so the author sees "this function has a twin in simoom" before merging. simoom can run
  the mirror job against a Cameleer checkout, using the same baseline file copied there or
  fetched from Cameleer.
- **Self-test.** The script carries small fixture trees (`scripts/duplicates/fixtures/a`, `b`)
  and a `--self-test` flag. The flag checks that an exact copy, a renamed copy, a near-miss, a
  one-sided edit and a moved function are each classified correctly. A hermetic test in
  `cameleer-main/tests` runs it whenever `python3` is present, the same way the docs build is
  checked, which keeps the "every feature gets a test" rule.

### Seeding the baseline

Start the baseline with the 94 exact and renamed function clones, the 55 near-misses and the 25
file pairs from this map. Mark the D1-D3 rows `removing`, so they stay visible until they are
fixed. Then:

- each time an algorithm moves to its home (for example, Cameleer's `cameleer-tls/src/asn1.rs`
  becomes a re-export of simoom-tls), its entry disappears and the report counts it as progress;
- the number of tracked entries is the migration's burn-down chart.

### Limits of the method

- The lexer is approximate. Macros are opaque, and code generated by `macro_rules!` is compared
  as the macro call.
- Containment is measured from the smaller side, so a small helper copied into a big file scores
  high. The 60-token minimum and the boilerplate filter keep this rare. Pairs near the threshold
  still need a human look.
- Rewrites with the same behaviour and no shared text, such as the HTTP/2 connection, are
  invisible to any text-based method. For those, the baseline needs hand-written
  `"kind": "semantic"` entries that track whole files. The one-sided-change alarm then works at
  file level ("`simoom-h2/src/conn.rs` changed: check `cameleer-grpc/src/h2/conn.rs`"), with
  the commit list as the signal.
