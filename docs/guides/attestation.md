# Attestation in confidential VMs

> When Cameleer runs inside a hardware TEE (an Intel TDX or AMD SEV-SNP confidential VM, or a dstack CVM such as Phala Cloud or dstack on Google Cloud TDX), it can prove to a client what is running: a signed hardware report, bound to the client's nonce, to the server's TLS key and to a manifest of the exact routes, configuration and tools. Clients get it over HTTP at `/q/attestation` and over MCP, in the handshake or as a tool.

**Cargo feature:** `attest` (in the default build) · **Apache Camel:** none (Cameleer extension)

## Turn it on

In a confidential VM:

```properties
camel.attestation.enabled=true
```

At start-up Cameleer looks for an evidence source and makes one report. If none works, start-up
fails: a process that asked to be attested must not look attested when it isn't. The sources:

| source | where | platforms |
|---|---|---|
| Linux configfs-tsm | `/sys/kernel/config/tsm/report` (Linux 6.7 and later) | Intel TDX, AMD SEV-SNP |
| dstack guest agent | `/var/run/dstack.sock` | dstack CVMs (Intel TDX): Phala Cloud, dstack on Google Cloud |

Then ask for a report with a nonce of 16 to 64 random bytes, in base64 or base64url:

```bash
curl "https://orders.example.com/q/attestation?nonce=$(openssl rand -base64 32 | tr '+/' '-_' | tr -d '=')"
```

## What a report proves, and how to check it

A report holds:

- `evidence`: the TEE's signed report (a TDX quote or an SEV-SNP report), base64;
- `binding`: what the 64 bytes the hardware signed (REPORTDATA) were made from;
- `manifest`: Cameleer's version and features, every route file with its SHA-256, every route with
  the digest of its definition, the configuration (secret values replaced by `<secret>`), and the
  published tools with their fingerprints;
- `log`: a hash-chained record that starts with the manifest and adds an entry for every change
  after start-up (routes added or removed, a tool disabled with the kill switch).

REPORTDATA is:

```text
SHA-512( "cameleer-attest-v1" || 0x00
       || manifest digest (the log's head, SHA-384, 48 bytes)
       || SHA-256 of the server's TLS public key (32 bytes, zeros without TLS)
       || nonce length (2 bytes, big-endian) || nonce )
```

A client checks the report in two steps:

1. **The binding.** Recompute REPORTDATA from the manifest digest in the report, the TLS key of the
   certificate it saw on its own connection, and the nonce it sent, and check that the log chain
   ends at that digest. A Rust program can call `cameleer_attest::check_binding`. A match means
   the evidence was made for this connection, now, by a process running this manifest. A
   terminating proxy in between changes the TLS key, and the check fails.
2. **The hardware evidence.** Verify the quote's signature chain against Intel's or AMD's roots and
   the platform measurements, with a verifier for the platform: Intel Trust Authority, Microsoft
   Azure Attestation, Google's verifier, or dstack's verifier for dstack CVMs. Cameleer does not do
   this step for you yet.

The hardware vouches only that the *measured* code chose those 64 bytes. So the manifest means
"this exact Cameleer release" when the platform measurement (the container image digest, or the
dstack compose hash in RTMR3) shows a build you trust: pin the image by digest.

## Over MCP

With the MCP server on, the handshake carries the attestation summary (platform, manifest digest,
TLS key hash, endpoint) in `_meta` under `ing.cameleer/attestation`, both in `server/discover`
(2026-07-28) and in `initialize` (earlier versions). A verifying client that sends a nonce gets the
full report back in the same round trip:

```json
{"jsonrpc": "2.0", "id": 1, "method": "server/discover",
 "params": {"_meta": {"ing.cameleer/attestation": {"nonce": "bm9uY2UtMDEyMzQ1Njc4OWFiY2RlZg"}}}}
```

The report is then in `result._meta["ing.cameleer/attestation"].report`.

For clients that only use tools, `camel.attestation.mcp-tool=true` adds the tool
`cameleer_attestation(nonce)`. Its description says plainly that a model showing evidence is not
verifying it: only a program that checks the binding and the signature chain gets the guarantee.

## Configuration

| property | default | description |
|---|---|---|
| `camel.attestation.enabled` | `false` | Produce evidence; start-up fails when no source works |
| `camel.attestation.provider` | `auto` | `auto` (dstack first, then configfs-tsm), `tsm` or `dstack` |
| `camel.attestation.tsm-path` | `/sys/kernel/config/tsm/report` | The configfs-tsm report directory |
| `camel.attestation.dstack-endpoint` | `/var/run/dstack.sock` | The dstack agent: a unix socket, or `http://host:port` for its simulator |
| `camel.attestation.path` | `/q/attestation` | The HTTP path of reports, on `camel.server.port` |
| `camel.attestation.mcp-tool` | `false` | `true` for the tool `cameleer_attestation`, or a tool name |
| `camel.attestation.exe-digest` | `true` | Put the SHA-384 of the running executable in the manifest (self-reported; the platform measurement is what counts) |

The TLS key comes from the embedded server's certificate (`camel.server.ssl.*`). Generate the key
inside the TEE and never write it out, or the binding proves less.

## Limits

- **No verification of other servers yet.** Checking quotes against Intel and AMD roots, and a
  gateway policy that refuses tools from servers that don't attest, are the next phase.
- **No sealed secrets yet.** Releasing secrets only after attestation (`{{sealed:...}}` with keys from
  dstack, AWS KMS, Azure Secure Key Release or Google Cloud) is planned.
- **AWS Nitro Enclaves, Azure vTPM attestation and Google Confidential Space** are not sources yet.
- **Physical attacks.** TEEs trust the cloud's physical security as well as the CPU vendor: memory
  bus interposers (TEE.fail, 2025) have extracted keys from TDX and SEV-SNP machines.
- **Runtime changes are recorded, not prevented.** The log shows a route added or a tool disabled
  after start-up; a policy that refuses them is a verifier's decision.

## Related

- [MCP server](mcp-server.md)
- [Deployment](../DEPLOYMENT.md)
- [ai-tool](../components/ai-tool.md): the tool envelope and its hash-chained audit log

## Found a problem?

If something on this page doesn't match what Cameleer does, please
[open an issue](https://github.com/pmsudhi/cameleer/issues/new), or fork the repository, fix
it, add a test, run `cargo test --workspace` and open a pull request.
