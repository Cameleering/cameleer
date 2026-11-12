# Draft: bringing Cameleer's attestation design to the MCP Security Interest Group

Internal. A draft for the owner to post under their own name; nothing here has been posted.

## Steps

1. **Wait until the code is public.** The SEP rules require a working prototype, and the post
   links to code and tests. Push `main` (with `crates/cameleer-attest`, the MCP `_meta` support and
   `docs/guides/attestation.md`) before posting.
2. **Join the MCP Contributors Discord** and the `#security-ig` channel:
   https://discord.com/channels/1358869848138059966/1379811011669921883 (the charter:
   https://modelcontextprotocol.io/community/interest-groups/security).
3. **Post the message below** in `#security-ig` as a new thread. It covers the IG's in-scope item
   "Server identity, attestation, and admission" and the related "Runtime drift and post-admission
   change".
4. **Ask for a slot on the monthly Forum call.** The facilitators (Den Delimarsky and Paul
   Carleton) post a call for agenda in the channel before each call; reply there with a one-line
   topic: "Hardware attestation in the MCP handshake: a working `_meta` design and implementation
   (Cameleer), feedback wanted".
5. **Present for 5 minutes** if given a slot: the problem, the binding, the `_meta` shape, the
   limits (the text below is the script).
6. **Only if the IG wants a SEP:** find a sponsor (a Maintainer or WG Lead, listed in
   https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/MAINTAINERS.md), then
   a PR to the spec repo titled `sep: ...`, from `seps/TEMPLATE.md`, linking the Discord thread
   and the meeting notes (a SEP without prior discussion is not accepted). Don't open a SEP first.
7. **Afterwards:** link the public thread from the `/trust/` page and the attestation guide (a claim
   we can point to), and keep the vendor prefix `ing.cameleer/attestation` until a standard
   name exists.

## The post (for `#security-ig`)

> **Hardware attestation in the MCP handshake: a working design, feedback wanted**
>
> Hi all. I maintain Cameleer, an open-source (Apache-2.0) Rust reimplementation of Apache Camel
> that also serves integration routes as MCP tools. We've implemented server attestation for
> MCP servers running in confidential VMs and would like the IG's feedback on the design. It
> sits under the charter's "server identity, attestation, and admission" and "runtime drift".
>
> **The problem.** A client that dispatches tool calls to a remote server can check its TLS
> certificate, but not what the server runs: which tools, which code behind them, or whether
> anything changed since it was reviewed. SEP-2809 (signed clearance assertions) was closed, and
> there is no hardware-backed option in the spec.
>
> **What we built** (inside the current spec, no new methods):
> - In an Intel TDX or AMD SEV-SNP confidential VM (Linux configfs-tsm) or a dstack CVM, the
>   server produces a hardware report whose REPORTDATA is
>   `SHA-512("cameleer-attest-v1" || 0x00 || manifest digest || SHA-256(server TLS SPKI) || nonce length || nonce)`.
> - The **manifest** covers the server version, the files that define its tools (SHA-256),
>   its configuration with secret values redacted, and every published tool's fingerprint. A
>   **hash-chained log** records changes after start-up (a tool added or disabled), so drift is
>   visible.
> - Binding to the **TLS SPKI** means a terminating proxy breaks the check, so the evidence is
>   about the connection the client is actually using.
> - **In the handshake:** the server advertises `_meta["ing.cameleer/attestation"]` (platform,
>   manifest digest, SPKI hash) in `server/discover` and `initialize`. A client that sends
>   `params._meta["ing.cameleer/attestation"] = {"nonce": "..."}` gets the full report inline in
>   the same round trip. One round trip, no new endpoints, and clients that don't care ignore it.
> - Also: an HTTP endpoint for scripts and verifiers, and an opt-in tool whose description says
>   plainly that a model *displaying* evidence is not *verifying* it.
> - If attestation is configured and no evidence source works, the server refuses to start.
>
> **What it does not do (yet):** the server doesn't verify Intel or AMD chains itself (clients use
> the platform's verifier); there are no sealed secrets yet; Nitro, Azure vTPM and Confidential
> Space aren't evidence sources yet; it is tested against a simulated configfs-tsm and dstack
> agent and hasn't run on real TEE hardware yet. Physical attacks (TEE.fail) are out of scope, as
> for any TEE.
>
> **Questions for the IG:**
> 1. Is `_meta` on `server/discover`/`initialize` the right place for this, or should it be a
>    capability or extension the client opts into?
> 2. Should the manifest have a standard shape (tools and their fingerprints at least), so a
>    client policy can be written once for any server?
> 3. How should this relate to Server Cards and registry provenance: evidence at connect time
>    versus claims at publish time?
> 4. Is there interest in a common evidence wrapper (for example IETF RATS CMW) so clients can
>    verify TDX, SEV-SNP and Nitro the same way?
>
> Code, tests and the design: https://github.com/cameleering/cameleer (crate `cameleer-attest`, guide
> `docs/guides/attestation.md`). Happy to present it in five minutes on a Forum call.

## Notes for the owner

- **What we gain:** the IG's members build MCP clients and platforms (the buyers of an attested
  server); a public thread is a credible, linkable claim; if the eventual standard follows this
  shape, Cameleer already complies.
- **What it costs:** answering questions in public under your name, and possible rework if the
  group chooses a different shape.
- **Facts to keep straight in the discussion:** everything in the post matches
  `docs/guides/attestation.md` and the code session's limits (2026-11-09). Don't claim it runs on
  real hardware until a `CAMEL_REAL_TEE=1` run exists.
