# A GitHub organization for the Cameleer family

_Internal. Written 2026-11-09 in answer to the owner's question about moving cameleer, simoom and
karez under one organization. Nothing on GitHub, crates.io, Docker Hub or DNS was changed._

Every fact below is tagged as one of these:

- **[verified]**: checked today with the command or URL given.
- **[docs]**: taken from the cited documentation, not tested here.
- **[judgment]**: a recommendation.

## 1. Recommendation in one page

1. **Create a GitHub organization named `cameleer-rs`.** The plain `cameleer` name is held by a
   dormant user account, and GitHub won't release it (section 2.1).
2. **Move the three family repos into it:** `cameleer`, `simoom` and `karez`. Leave the unrelated
   personal repos (process mining, knowdb and the rest) on `pmsudhi`.
3. **Keep three separate repos.** Don't merge them into a monorepo. Cameleer depends on simoom and
   karez through published versions, the way axum depends on hyper.
4. **Keep the names simoom and karez.** Don't rename them to `cameleer-http-server` or
   `cameleer-sql`. Every repo, crate page and README carries the same line: "Part of the Cameleer
   family". This is the model used by Tokio (hyper, axum, mio, tracing), Bytecode Alliance
   (Wasmtime, Cranelift) and Apache Arrow (DataFusion).
5. **Position Cameleer as the product, with a family of crates behind it.** The home page still
   sells the integration and agent action layer. A new `/family/` (or `/crates/`) page and the org
   README present simoom, karez and the reusable `cameleer-*` crates (tls, http, xml and so on) as
   standalone std-only building blocks. This is how tokio.rs presents "the stack".
6. **Order of work:** create and secure the org, verify the domain, transfer the private repos,
   then transfer `cameleer` after 0.5.0 has fully shipped. Then publish 0.5.1 with the new
   `repository` URLs, add a crates.io team owner and set up trusted publishing.
   - Existing pulls of `ghcr.io/pmsudhi/cameleer` and `pmsudhi/cameleer` on Docker Hub keep
     working throughout.
   - The URLs and git remotes redirect.
7. **Docker Hub stays at `pmsudhi/cameleer` for now.** The `cameleer` namespace there is taken by an
   unrelated user. ghcr.io moves to `ghcr.io/cameleer-rs/cameleer`, and the old path is frozen with
   a notice.

## 2. Verified facts about names and the current setup

### 2.1 Names

| check | result | how |
|---|---|---|
| GitHub `cameleer` | **Taken.** A user account created 2024-10-28, with 0 public repos, 0 followers and no bio. Not an org. | `gh api users/cameleer` returns 200 with `type: User`; `gh api orgs/cameleer` returns 404 [verified] |
| GitHub `cameleer-rs`, `cameleerhq`, `cameleer-ing`, `cameleer-dev` | free | `gh api users/<n>` and `gh api orgs/<n>` both return 404 [verified] |
| Can GitHub release the dormant `cameleer` account? | **No.** GitHub doesn't release usernames for inactivity; only a valid trademark complaint can free one. | [GitHub username policy](https://docs.github.com/en/site-policy/other-site-policies/github-username-policy) [docs] |
| crates.io `simoom`, `karez`, `simoom-http`, `karez-core` | free | `https://crates.io/api/v1/crates/<n>` returns 404 [verified] |
| crates.io bare `cameleer` | **free** (the project publishes `cameleer-main` and others, but not `cameleer`) | same API, 404 [verified] |
| crates.io `cameleer-core` owners | only the user `pmsudhi`, with no team | `/api/v1/crates/cameleer-core/owners` [verified] |
| Docker Hub `cameleer` | **Taken** by an unrelated user (joined 2020-09-16, 2 images, for example `cameleer/platformservice`) | `hub.docker.com/v2/users/cameleer/` and `/v2/repositories/cameleer/` [verified] |
| Docker Hub `cameleerrs`, `cameleerhq` | no such user (likely free; the orgs endpoint wasn't checked) | `hub.docker.com/v2/users/<n>/` returns 404 [verified] |

Why `cameleer-rs` [judgment]:

- It follows the most common Rust org pattern: `tokio-rs`, `pola-rs`, `embassy-rs`.
- It reads as "the Rust Cameleer".
- It is free.

The alternatives are `cameleer-ing` (matches the domain, but reads oddly) and `cameleerhq` (sounds
like a company).

Don't wait on the dormant `cameleer` account [judgment]. The only route to it is a trademark claim,
which needs a registered mark. If the owner registers one later, the org can still be renamed: org
renames redirect like repo renames.

### 2.2 Current setup of `pmsudhi/cameleer` [verified]

**Repository**

- 2 stars, 2 forks, 15 open issues, and Discussions is off (`gh api repos/pmsudhi/cameleer`).
- Pages:
  - built by a workflow;
  - `cname: cameleer.ing`;
  - **`https_enforced: false`**;
  - **`protected_domain_state: null`**, so the domain is not verified (`gh api repos/pmsudhi/cameleer/pages`).

  Both should be fixed whatever is decided about the org.
- Actions secrets: `DOCKERHUB_TOKEN` and `TRAFFIC_TOKEN`. Environments: `copilot` and `github-pages`.

**Container workflow (`.github/workflows/container.yml`)**

- It pushes to `ghcr.io/${{ github.repository_owner }}/cameleer`, and the attestation
  `subject-name` uses the same path.
- After a transfer this **silently switches** to `ghcr.io/cameleer-rs/cameleer`. The old path would
  stop receiving new tags without any error.
- Docker Hub is hardcoded as `DOCKERHUB_USERNAME: pmsudhi`, so it keeps working.

**Other workflows**

- `traffic.yml` reads `hub.docker.com/.../pmsudhi/cameleer`.
- `TRAFFIC_TOKEN` is a fine-grained PAT scoped to `pmsudhi/cameleer`. It needs reissuing for the
  org: a fine-grained PAT's resource owner must be the org, and the org must allow such tokens.

**References in the repo**

- `git grep` finds **928 occurrences of `pmsudhi/cameleer` in 318 tracked files**, plus 3 of
  `pmsudhi.github.io`.
- These include the workspace `Cargo.toml` `repository` field, `CITATION.cff`
  `repository-code`, the workflows, the docs, the site and `llms.txt`.

**simoom and karez**

- Both are private, and karez's description already says "Part of the Cameleer family".

## 3. Repo organization: best practice

### 3.1 Personal account or organization

An org is the standard choice for a multi-project family [judgment, backed by the survey in 3.3].
It brings:

- **A lower bus factor.** An org can have several owners. A personal account has one person, and
  if that account is lost or locked, everything goes with it.
- **Teams.** CODEOWNERS can name teams, and crates.io can list a GitHub team as an owner (section 4).
- **Settings at org level.** Secrets, the verified Pages domain, rules that require 2FA, and
  security settings.
- **A clear identity.** Separating the family from the owner's unrelated repos makes the org page
  itself a product page.

What it costs is nothing on the Free plan for public repos. One caveat from GitHub: transferring
a **private** repo to a Free org removes protected branches and Pages for that repo
([transfer docs](https://docs.github.com/en/repositories/creating-and-managing-repositories/transferring-a-repository))
[docs]. That's acceptable for simoom and karez while they are developed by one person. They get
those features back when they go public.

### 3.2 One umbrella org or separate orgs

**Use one org** [judgment]. Separate orgs only make sense for projects with independent
governance, maintainers or audiences:

- **hyperium vs tokio-rs:** hyper predates the tokio-rs org and has its own maintainers.
- **rustls:** a separate org with its own funding (ISRG/Prossimo).

simoom and karez share the owner, the rules (std-only, measured claims) and the release
engineering. One org also means one set of secrets, one verified domain and one crates.io team.
If one of them later attracts its own maintainer group, GitHub lets you move a repo to a new org
with the same redirects.

### 3.3 How comparable Rust families organize themselves

Repo counts and top repos by stars come from `gh api orgs/<o>/repos` [verified]. The patterns are
[judgment].

| org | repos | top repos | pattern |
|---|---|---|---|
| tokio-rs | 48 | tokio, axum, mio, tracing, prost | one org holds a stack of **distinct names**; tokio itself is a monorepo of a few crates |
| hyperium | 14 | hyper, h2, http, h3 | a separate org for the HTTP family, with distinct short names |
| rustls | 27 | rustls, rcgen, webpki, tokio-rustls, rustls-ffi | product-prefixed adapters (`tokio-rustls`, `rustls-ffi`), distinct names for independent tools (`rcgen`) |
| RustCrypto | 41 | hashes, AEADs, elliptic-curves | multi-repo by **algorithm family**, each repo a workspace of many crates; shared `.github` repo |
| bytecodealliance | 104 | wasmtime, wasm-tools, wit-bindgen, rustix | foundation umbrella; projects keep their own names; `governance` repo; Core/Hosted tiers |
| tikv | 65 | tikv, raft-rs, grpc-rs, pprof-rs | product org that also publishes reusable `*-rs` libraries extracted from the product |
| apache | n/a | datafusion (9.6k), arrow-rs (3.7k), datafusion-comet, datafusion-ballista | subprojects keep their own names; related repos use a `datafusion-*` prefix |
| embassy-rs | 60 | embassy, trouble, embedded-tls, reqwless | a framework monorepo plus distinct side projects |
| bevyengine | 22 | bevy, bevy-website, rfcs | **monorepo** for the engine; side repos for the website and RFCs |
| pola-rs | 30 | polars, nodejs-polars, r-polars, pyo3-polars | a product monorepo; bindings in prefixed repos |

The closest match for Cameleer is **tikv** (a product whose org also publishes reusable libraries)
combined with **tokio-rs** (a stack of distinct names under one brand).

### 3.4 Monorepo or multi-repo

[judgment]

- **Keep `cameleer` as the workspace monorepo it already is.** Its 19 crates release together and
  share CI, which is the bevy/tokio pattern.
- **Keep simoom and karez as separate repos.** Reasons:
  - They have their own release cadence and semver.
  - They have their own audiences: people who want an HTTP server or database drivers and nothing
    about Camel.
  - They have their own issue trackers.
  - Their CI stays small. Cameleer's suite has 2,800 tests; simoom's CI shouldn't run them.
- **Cameleer should depend on them through crates.io versions** (as axum depends on hyper), not
  through git or path dependencies. Path dependencies can't be published, and git dependencies
  are refused by crates.io.
- **Clarify the std-only rule in CLAUDE.md.** Rule 1 reads "No external crates... Don't add a
  dependency to any `Cargo.toml` without explicit approval from the owner." Before Cameleer depends
  on simoom or karez, the owner should write the exception down: "std plus Cameleer family
  crates, which follow the same std-only rule".

### 3.5 Naming: distinct names or `cameleer-*` prefixes

[judgment]

- **Keep `simoom` and `karez`.**
  - Both names are free on crates.io (verified above).
  - They are short and unique, so search results won't be shared with anything else.
  - They let each project stand alone. Someone looking for an HTTP server is more likely to adopt
    "simoom" than "cameleer-http-server", because the prefix suggests they need the integration
    framework.
  - Tokio (hyper, axum, mio), Bytecode Alliance (Cranelift) and Arrow (DataFusion) all do this.
- **Get discoverability from consistent cross-linking rather than from the prefix.** Each README,
  the crates.io description, the GitHub description and the docs.rs front page carry a "Part of
  the Cameleer family" line with a link to the family page.
- **Keep the `cameleer-*` prefix** for crates that are pieces of the Cameleer product
  (`cameleer-core`, `cameleer-yaml`, `cameleer-main`), and for adapters that connect a family crate
  to Cameleer, in the `tokio-rustls` style. For example, a `cameleer-simoom` crate if the glue
  ever needs to be separate.
- **Reusable crates that already carry the prefix** (`cameleer-tls`, `cameleer-xml`) keep their
  names. Renaming published crates breaks users. If one of them is ever offered as a standalone
  product, give it a distinct name then.
- **Publishing the names early.** crates.io allows some reservation for good reason, but its 2023
  policy targets names held "without genuine functionality"
  ([policy update](https://blog.rust-lang.org/2023/09/22/crates-io-usage-policy-rfc/),
  [RFC 3463](https://rust-lang.github.io/rfcs/3463-crates-io-policy-update.html)) [docs].
  - Publish `simoom` and `karez` once there is a working 0.1.0-alpha. That needn't wait for the
    full launch.
  - Don't publish empty placeholders.
- **Consider claiming the bare `cameleer` crate** for the CLI, so that `cargo install cameleer`
  works. That is real functionality, not squatting.

### 3.6 Org files

[docs for where the files go, judgment for what goes in them]

- **The `cameleer-rs/.github` repo** holds the following.
  - `profile/README.md` is the public org page
    ([GitHub docs](https://docs.github.com/en/organizations/collaborating-with-groups-in-organizations/customizing-your-organizations-profile)).
    Put in it:
    - one sentence on what Cameleer is;
    - a table of the family: the product, simoom, karez and the reusable crates, with status
      badges;
    - links to cameleer.ing and the benchmarks;
    - the std-only, measured-claims stance.
  - Default community health files, which apply to every repo that lacks its own. RustCrypto uses
    this repo too (`gh api repos/RustCrypto/.github` returns 200) [verified]:
    - `SECURITY.md`;
    - `CODE_OF_CONDUCT.md` (the Contributor Covenant or the Rust CoC);
    - `CONTRIBUTING.md`;
    - `SUPPORT.md`;
    - issue and PR templates.
- **A private `.github-private` repo** is optional. Its `profile/README.md` is shown to members only.
- **SECURITY.md** should point to GitHub private vulnerability reporting, enabled on every repo.
  The from-scratch TLS, SSH and crypto code makes this matter more than usual.
- **GOVERNANCE.md** can stay short for a single-maintainer project:
  - who decides (the owner, as BDFL);
  - how a contributor becomes a maintainer;
  - the stated rules: std-only, Camel names, measured claims only.

  Bytecode Alliance keeps a whole `governance` repo (verified above). That's far more than this
  family needs today.
- **Teams:**
  - `maintainers` (admin on all repos);
  - `release` (the crates.io team owner and the people allowed to run release workflows);
  - one team per project later (`simoom`, `karez`) if the projects get their own maintainers.
- **CODEOWNERS** in each repo names teams, not people. For example:
  - `* @cameleer-rs/maintainers`
  - `crates/cameleer-tls/ @cameleer-rs/security`, once such a team exists.
- **Settings:**
  - Require 2FA for org members.
  - Add a second owner: a trusted person, or at least a separate recovery account the owner
    controls.

## 4. Migration mechanics and risks

### 4.1 What a repo transfer keeps

[docs, [transfer docs](https://docs.github.com/en/repositories/creating-and-managing-repositories/transferring-a-repository)]

- **Kept:** issues, PRs, wiki, stars, watchers, commit history, webhooks, secrets and deploy keys.
- **Forks:** they stay associated with the repo.
- **Redirects:** web links and `git clone/fetch/push` redirect to the new location.
  - **The redirect is lost forever if anyone creates a repo or fork named `pmsudhi/cameleer`.**
  - Whether the older `pmsudhi/rust-camel` redirect chains through a second move isn't
    documented. Test it with `curl -I` and `git ls-remote` after the transfer.
- **Pages:** GitHub doesn't redirect the `pmsudhi.github.io/cameleer` URL. The custom domain
  avoids this, but only if the domain is set up correctly (4.2).
- **Packages:** "may be transferred, or may lose their link to the repository, depending on the
  registry". For ghcr.io, "the account associated with the package does not change when you
  transfer a repository"
  ([packages permissions](https://docs.github.com/en/packages/learn-github-packages/about-permissions-for-github-packages)).
  So `ghcr.io/pmsudhi/cameleer` stays where it is, and **every existing tag keeps resolving**.
- **Environments** aren't mentioned in the docs. Check `github-pages` after the transfer and
  recreate its protection rules if needed.
- **Retired names:** an owner/repo name is retired if the repo had more than 100 clones or Actions
  uses in the week before the transfer. That's harmless here, because nothing should take the old
  name anyway.

### 4.2 GitHub Pages and cameleer.ing

The domain isn't verified today (`protected_domain_state: null`) [verified]. That leaves it open to
takeover whenever Pages is off or the repo is moving
([verification docs](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/verifying-your-custom-domain-for-github-pages))
[docs].

A domain verified on an account can only be used by repos owned by that account. So **verify
cameleer.ing on the org, not on `pmsudhi`.** If you verify it on the user account first, the site
breaks after the transfer until you move the verification.

The steps:

1. Create the org.
2. Add the TXT record under the org's Pages settings.
3. Transfer the repo.
4. Confirm the CNAME.
5. Turn on "Enforce HTTPS", which is off today.

### 4.3 ghcr.io

A container package's namespace is fixed when it's published [docs, see 4.1]. There's no
documented user-to-org move. The documented route is to push again under the new namespace
[judgment based on those docs].

- **The safe path:**
  1. Leave `ghcr.io/pmsudhi/cameleer` with its tags, so old pulls keep working.
  2. Push 0.5.1 and later to `ghcr.io/cameleer-rs/cameleer`.
  3. Write in DEPLOYMENT.md, the release notes and the old package's description that new
     versions are published under the new path.
- **To keep `:latest` or `:0.5` moving on the old path for a while,** the org repo's
  `GITHUB_TOKEN` can't push to a package owned by a user. That needs a classic PAT from `pmsudhi`
  with `write:packages`, stored as an org secret, for one or two releases. This is optional. A
  frozen old path with a notice is simpler, and with 2 stars there are few users to migrate.
- **Change `container.yml`** so the ghcr namespace is explicit (`ghcr.io/cameleer-rs/cameleer`)
  rather than `${{ github.repository_owner }}`. Change the attestation `subject-name` the same way.
- **Set `org.opencontainers.image.source`** to the new repo URL, so the new package links to the repo.
- **Org setting:** members must be allowed to create packages, and the new package needs Actions
  write access granted to the `cameleer` repo.

### 4.4 Docker Hub

- **Namespaces don't move.** A Docker Hub user can be converted into an org, but the Docker ID, and
  so the namespace, stays the same
  ([Docker docs](https://docs.docker.com/admin/organization/convert-account/)) [docs].
- **`cameleer` is taken** by someone else [verified].
- **Recommendation [judgment]:**
  - Keep publishing to `pmsudhi/cameleer`. It's tied to the Docker account, not the GitHub repo,
    so the transfer doesn't affect it.
  - If a branded namespace is wanted later, create a Docker Hub org (`cameleerrs` appears free).
    Docker IDs are lowercase letters and digits, so `cameleer-rs` is not possible; this rule is
    from general knowledge and should be confirmed at signup.
  - Then push to both namespaces for a deprecation window.
- **Secrets:** `DOCKERHUB_TOKEN` moves with the repo. If several repos will push, re-create it as
  an org secret limited to selected repos.

### 4.5 crates.io

[docs, [Cargo publishing docs](https://doc.rust-lang.org/cargo/reference/publishing.html)]

- **Crate ownership is independent of GitHub repo ownership.** Nothing breaks on transfer.
- **Add a team owner:** `cargo owner --add github:cameleer-rs:release` on each of the 19 crates.
  - The person adding the team must be a member of it.
  - Team owners can publish and yank, but can't change owners. That's a security benefit.
  - Keep `pmsudhi` as a named owner, and preferably a second named human owner, for continuity.
- **Org access:** crates.io needs the `read:org` scope. If the org restricts OAuth apps, approve
  crates.io in the org's third-party access settings.
- **Trusted publishing** (OIDC from GitHub Actions, without a long-lived token)
  ([RFC 3691](https://rust-lang.github.io/rfcs/3691-trusted-publishing-cratesio.html),
  [crates.io update, July 2025](https://blog.rust-lang.org/2025/07/11/crates-io-development-update-2025-07),
  [January 2026](https://blog.rust-lang.org/2026/01/21/crates-io-development-update)).
  - Each crate is configured with an owner, repo and workflow, and optionally an environment.
  - Releases use `rust-lang/crates-io-auth-action`.
  - Owners can now require trusted publishing.
  - Configure it **after** the transfer, against `cameleer-rs/cameleer` and a `release`
    environment. If it were configured before, the owner/repo claim would no longer match.
  - New crates (simoom, karez) need their first publish done by hand.
- **`repository` fields:** crates.io shows the `repository` field of the published version, so
  the crate pages keep pointing at `pmsudhi/cameleer` until a new version is published. The old
  URL still redirects, so nothing is broken in the meantime. Ship 0.5.1 soon after the transfer
  with the new URLs.

### 4.6 Docs, badges, CITATION.cff

- **Mechanical rewrites:**
  - `pmsudhi/cameleer` → `cameleer-rs/cameleer`: 928 occurrences in 318 files [verified count].
  - The 3 `pmsudhi.github.io` occurrences.
- **Exceptions:** `docker.io/pmsudhi/cameleer` and the Docker Hub references in
  `dockerhub-description.yml` and `traffic.yml` stay as they are (4.4). So the rewrite is not a
  blind `sed`: exclude `docker.io/pmsudhi`, `hub.docker.com/.../pmsudhi` and `username: pmsudhi`.
- **`CITATION.cff`:** update `repository-code`. Leave `version: 0.5.0` and `date-released`
  alone, since they describe that release.
- **Badges** using shields.io or GitHub URLs redirect. Update them anyway, so the redirect isn't
  load-bearing.
- **The `rust-camel` history note in STATUS.md** stays. It's history.

## 5. Product positioning

[judgment, with the models cited]

**The models:**

- **Tokio.** tokio.rs presents "the stack": Runtime, Hyper, Tonic, Tower, Mio, Tracing and Bytes,
  each with a one-line role ([tokio.rs](https://tokio.rs/)). The brand is the stack; each piece
  keeps its own name and can be adopted alone.
- **Bytecode Alliance.** It groups projects as Core or Hosted. Projects keep their own names, and
  Cranelift is listed separately even though its code lives in the Wasmtime repo
  ([projects](https://bytecodealliance.org/projects)) [docs].
- **Apache Arrow and DataFusion.** A format brand with engine subprojects. DataFusion now
  out-stars arrow-rs, which shows that a sub-brand can outgrow its parent without harm.
- **rustls and RustCrypto.** They are known for one quality: memory-safe, audited, no C. They put
  security policy, audits and MSRV up front.

**What this means for Cameleer:**

1. **There's one brand with two kinds of thing under it:**
   - The product: Cameleer, the Camel-compatible integration runtime and the action layer for AI
     agents.
   - The building blocks: high-performance, std-only Rust crates.

   Both share one promise: **std-only, from scratch, measured**. Make that the family's tagline.
   It's the thread that rustls and RustCrypto show a family can be known for.
2. **Site:**
   - Leave the home page about the product. Don't dilute the integration and agent message.
   - Add one "Family" section or page in the tokio.rs style. Each entry gives the name, a one-line
     role, its status (stable, preview or coming), a benchmark link, and links to crates.io and
     docs.rs.
   - simoom and karez get pages under cameleer.ing (`/simoom/`, `/karez/`) before they get their
     own domains. That way they gain from the main domain's search ranking.
3. **READMEs:**
   - Each family README opens with the crate's own pitch.
   - A consistent footer or badge says: "Part of the Cameleer family: std-only Rust, measured
     performance".
   - Cameleer's README gets a short "Built on" section once it actually uses simoom and karez.
4. **crates.io:**
   - Use the keywords `cameleer` and `std-only` (crates.io allows 5 keywords; `no-dependencies`
     is another option).
   - Use the same category style across crates.
   - Set `homepage` to the family page.
   - Use `authors` and the team owner consistently, so that "owned by cameleer-rs/release" is
     visible on every crate page.
5. **Launch under their own names, linked to the family.** simoom's audience (anyone embedding an
   HTTP server) and karez's audience (anyone needing database connectivity) are far larger than
   Camel users. A `cameleer-` prefix would tell them it's a framework piece. "Part of the Cameleer
   family" gives them credibility without asking them to adopt the framework.
6. **Keep these private until they're ready:**
   - **simoom and karez** until each has a usable alpha, a README with real benchmark numbers
     (measured claims only), a SECURITY.md, CI and green tests.
   - **Unmeasured performance claims.** No public page claims performance before a reproducible
     benchmark exists.
   - **`docs/design/`** stays internal as it is today.
   - **Benchmark infrastructure secrets** (DigitalOcean tokens and the like) never go into org
     secrets accessible to all repos. Use secrets limited to selected repos, or environments.

## 6. Plan

### Now (this week, after 0.5.0 is fully out)

1. Create the org `cameleer-rs` on the Free plan.
   - Set its display name to "Cameleer", its URL to https://cameleer.ing, and a description that
     names the family.
   - Require 2FA.
   - Add a second owner or a recovery account.
2. Create the teams `maintainers` and `release`.
3. Verify cameleer.ing at **org** level (DNS TXT record).
4. Create the `.github` repo with:
   - `profile/README.md`;
   - `SECURITY.md`, `CODE_OF_CONDUCT.md`, `CONTRIBUTING.md` and `SUPPORT.md`;
   - issue templates.
5. Transfer `simoom` and `karez`. They're private, have no users and no Pages, so there's no risk.
6. Prepare a branch in `cameleer`. Don't merge it until the transfer:
   - `container.yml`: explicit `ghcr.io/cameleer-rs/cameleer`, attestation subject and image source label;
   - the `repository` field in the workspace `Cargo.toml`;
   - `CITATION.cff` `repository-code`;
   - the doc, site and `llms.txt` rewrite, with the Docker Hub exceptions from 4.6;
   - a DEPLOYMENT.md note about the new ghcr path.

### Transfer day

7. Make sure no release workflow is running.
8. Transfer `pmsudhi/cameleer` to `cameleer-rs`.
9. Check right away:
   - `curl -I https://github.com/pmsudhi/cameleer` (expect a 301);
   - `git ls-remote https://github.com/pmsudhi/cameleer` and the same for `.../rust-camel`;
   - the old API URL;
   - https://cameleer.ing loads, its CNAME is intact, and "Enforce HTTPS" is now on;
   - the `github-pages` environment and the issues are intact.
10. Locally, run `git remote set-url origin https://github.com/cameleer-rs/cameleer.git`. Do the
    same in every worktree and on the droplets.
11. Reissue `TRAFFIC_TOKEN` as a fine-grained PAT whose resource owner is `cameleer-rs`, and
    approve it in the org. Confirm that `DOCKERHUB_TOKEN` is still present.
12. Merge the prepared branch. Run `cargo build` (zero warnings) and `cargo test --workspace`, as
    the CLAUDE.md rules require.

### Within days

13. crates.io:
    - Authorize crates.io for the org.
    - Run `cargo owner --add github:cameleer-rs:release` for all 19 crates.
    - Set up trusted publishing for `cameleer-rs/cameleer` on the release workflow and environment.
14. Release 0.5.1: new `repository` URLs on crates.io, and the first images under
    `ghcr.io/cameleer-rs/cameleer`. Update the old ghcr package's description to point to the new
    path.
15. Update the Docker Hub overview (`docker/README.dockerhub.md`) with the new GitHub links.
16. Update the GitHub profile of `pmsudhi`: pin the org and remove the family repos from its pins.

### Later

17. Add the family page on cameleer.ing.
18. Publish simoom and karez when each meets the readiness bar in section 5.6. The first publish
    is manual, then trusted publishing.
19. Write down the std-only exception for family crates before Cameleer depends on them.
20. Optional:
    - a `cameleerrs` Docker Hub org;
    - claiming the bare `cameleer` crate for the CLI;
    - a trademark if the brand grows. That's also the only path to the GitHub `cameleer` name.

## 7. Risks and how to avoid each

| risk | avoid it by |
|---|---|
| Redirects lost forever | Never create a repo or fork named `pmsudhi/cameleer` (or `rust-camel`). Merge the URL rewrite soon, so the redirects aren't load-bearing. |
| ghcr.io silently stops updating the old path, because `repository_owner` changes | Use an explicit namespace in `container.yml`. Announce the new path. Old tags keep resolving. |
| Site down or domain hijacked during the move | Verify cameleer.ing at **org** level before the transfer. Never verify it at user level. Turn on HTTPS enforcement. |
| `TRAFFIC_TOKEN` and other PATs break | Reissue them with the org as resource owner. Prefer `GITHUB_TOKEN` or OIDC where possible. |
| Trusted publishing claim mismatch | Configure it after the transfer, against the new repo path. |
| Release blocked because crates.io can't see the org team | Grant crates.io org access and re-login with `read:org` before relying on the team. Keep `pmsudhi` as a named owner. |
| A single owner locked out of the org | Add a second owner or a recovery account, require 2FA, and store recovery codes offline. |
| Private repos lose branch protection in a Free org | Acceptable while private. It comes back when they go public, or with a paid Team plan if it's needed earlier. |
| Brand confusion with `cameleer` on GitHub and Docker Hub (unrelated holders) | Always write `cameleer-rs` in full. The README and site state the official locations: `github.com/cameleer-rs`, `ghcr.io/cameleer-rs/cameleer`, `docker.io/pmsudhi/cameleer` and crates.io `cameleer-*`. |
| Crate names taken before launch | Publish a real alpha of simoom and karez soon rather than placeholders. The names are free today, so check again just before publishing. |
| The std-only rule gets fuzzy | Write the "std plus family crates" exception into CLAUDE.md and ARCHITECTURE.md before the first cross-repo dependency. |
