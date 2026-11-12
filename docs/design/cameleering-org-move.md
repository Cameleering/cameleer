# Move the Cameleer family under the `cameleering` GitHub organization

_Internal. Approved by the owner on 2026-11-12; supersedes
[github-organization.md](github-organization.md). The references to `pmsudhi` below describe the
state before the move. Branch `org-move` holds step 7 (the rewrite by
`scripts/org-move-rewrite.py`) and is merged only on transfer day._

## Context

The earlier plan (`docs/design/github-organization.md`, 2026-11-09) recommended an org named
`cameleer-rs`, with Docker Hub staying on `pmsudhi` because the `cameleer` namespace there is
taken. Since then:

- the family grew (health pack, attestation, a planned `cameleer-sap` pack, the saltire crypto
  plan);
- the site adopted "cameleering" as its own term (`site/cameleering/`, "What is cameleering?").

The owner proposes the org **Cameleering** for everything, and wants the plan re-checked
against today's facts.

**Facts re-verified today (2026-11-12):**

| check | result |
|---|---|
| GitHub `cameleering` (user or org) | **free** (`gh api users/cameleering` → 404) |
| GitHub `cameleer` | still a dormant user (created 2024-10-28, 0 repos) |
| Docker Hub `cameleering` (user and org) | **free** (404 on both endpoints) |
| Docker Hub `cameleer` | still taken by an unrelated user |
| crates.io `cameleering`, `cameleer`, `simoom`, `karez`, `saltire`, `saltire-pki` | all **free** |
| crates.io `cameleer-core` owners | `pmsudhi` only, no team |
| Family repos | `pmsudhi/cameleer` (public, 3 stars), `pmsudhi/simoom` and `pmsudhi/karez` (private); no saltire repo yet |
| Unrelated personal repos on `pmsudhi` | about 27; they stay |
| Workspace crates | 23 (22 publishable; `cameleer-attest` and `cameleer-health` never published) |
| `pmsudhi/cameleer` references on `main` | 1058 in 353 files (282 in `docs/`, 24 `site/`, 24 `examples/`, 11 root, 6 `crates/`, 3 workflows); about 60 are Docker-style; simoom has 10, karez 31 |
| GitHub Pages | CNAME `cameleer.ing`, **HTTPS not enforced, domain not verified**: still open from the old plan |
| `container.yml` | builds the ghcr path from `github.repository_owner`, so a transfer silently moves it |

**Why `cameleering` wins over `cameleer-rs`:**

- It is the domain: cameleer.ing reads as "cameleering".
- It is free on GitHub, Docker Hub and crates.io alike, so one namespace works everywhere: no
  split brand, no Docker Hub exception.
- It matches the site's own term.

**Owner decisions (asked today):**

- The Docker image moves to `cameleering/cameleer`.
- The org is owned by `pmsudhi` plus a second owner.
- Move first, then release 0.6.0 from the org.

## Target

- **GitHub org `cameleering`**, display name "Cameleering", URL https://cameleer.ing. Repos:
  - `cameleering/cameleer`: the product and its workspace, including the packs (`cameleer-health`,
    later `cameleer-sap`);
  - `cameleering/simoom`, `cameleering/karez` (private until their readiness bar);
  - later `cameleering/saltire`, created when the crypto move starts;
  - `cameleering/.github`: the org profile README and default community files.
- **Containers:** `docker.io/cameleering/cameleer` and `ghcr.io/cameleering/cameleer`, with the
  tags `latest`, `<v>`, `health`, `<v>-health` and `edge`.
- **crates.io:** the GitHub team `github:cameleering:release` co-owns every crate, `pmsudhi` stays
  a named owner, and trusted publishing runs from `cameleering/cameleer`.
- **Unchanged:** product and crate names (`cameleer-*`, simoom, karez, saltire) and the domain
  cameleer.ing.

## Plan

### Phase 0: owner steps, outside the repo (only the owner can do these)

1. **Create the org** `cameleering` (Free plan) from `pmsudhi`. Done 2026-11-12 (login
   `Cameleering`; name, URL and the teams set). Optional, not required by GitHub:
   - require 2FA for members: only matters once others join (it removes members without 2FA);
   - a second owner, for continuity (GitHub's docs recommend at least two). Without one, give
     `pmsudhi` several 2FA methods (passkey or security key) and keep the recovery codes offline.
2. **Teams:** `maintainers` and `release`.
3. **Domain:** verify cameleer.ing at **org** level (DNS TXT). Never at user level. **Lesson
   (2026-11-12):** an org-verified domain serves only the org's repositories, so verifying it
   before the transfer removed the custom domain from `pmsudhi/cameleer` and the site answered
   404 until the transfer. Verify right after the transfer instead, then set the domain again.
4. **Docker Hub:** create the `cameleering` namespace. A Docker Hub organization may need a paid
   plan (check at signup). If it does, a free Docker account named `cameleering` works as the
   namespace. Create an access token for CI.

### Phase 1: prepare (no transfer yet)

5. **The `.github` repo:**
   - `profile/README.md` (the family, links to cameleer.ing, the official locations);
   - `SECURITY.md`, `CODE_OF_CONDUCT.md`, `SUPPORT.md`;
   - issue templates.
6. **Transfer the private repos** `simoom` and `karez`: no users and no Pages, so no risk.
   Rewrite their 10 and 31 `pmsudhi` references in their own commits.
7. **Prepare a branch `org-move` in `cameleer`** (merged only after the transfer). It holds a
   scripted rewrite: a Python script in `scripts/`, run once, with its exception list in the
   commit. The rewrite covers:
   - `github.com/pmsudhi/cameleer` → `github.com/cameleering/cameleer`, everywhere: docs, site,
     examples, README, CONTRIBUTING, AGENTS.md, llms.txt, the "Found a problem?" footers;
   - Docker refs, `pmsudhi/cameleer` → `cameleering/cameleer`: docker/README.dockerhub.md,
     DEPLOYMENT.md, the docs and site pages, the examples;
   - `Cargo.toml` `repository` (the workspace and every crate), CITATION.cff `repository-code`;
   - `.github/workflows/container.yml`: an **explicit** `ghcr.io/cameleering/cameleer`, the
     attestation subject, the image source label, and `DOCKERHUB_USERNAME`;
   - `dockerhub-description.yml` repository and username, and `traffic.yml`;
   - the site JSON-LD `codeRepository`/`sameAs`, sitemap and canonical links if any point to
     GitHub;
   - CLAUDE.md, STATUS.md (where the repo name is stated) and the old design doc's
     recommendation, superseded by a note pointing to this plan.

   **Exceptions:** `CHANGELOG.md` history and `benchmarks/results/` stay as recorded (the
   redirects cover them).

   **Check:** `git grep pmsudhi/cameleer` afterwards shows only the listed exceptions.

### Phase 2: transfer day

8. Check that no release or Pages workflow is running, then **transfer `pmsudhi/cameleer` →
   `cameleering`**.
9. **Check right away:**
   - `curl -I https://github.com/pmsudhi/cameleer` gives a 301;
   - `git ls-remote` works on both old URLs (`cameleer` and the older `rust-camel`);
   - cameleer.ing still loads with its CNAME intact; turn on **Enforce HTTPS**;
   - issues, stars and the `github-pages` environment are intact.
10. `git remote set-url origin https://github.com/cameleering/cameleer.git` in this checkout, in
    every worktree (dicom, seo-expert, sap-integration) and in each session's clones. Tell the
    linked sessions.
11. **Secrets:**
    - add `DOCKERHUB_TOKEN` for the new namespace;
    - reissue `TRAFFIC_TOKEN` as a fine-grained PAT owned by the org, and approve it in the org;
    - check the remaining secrets survived the transfer.
12. **Merge `org-move`:** build with zero warnings, run `cargo test --workspace`, rebuild the docs,
    push. Pages redeploys from the org.

### Phase 3: release 0.6.0 from the org

13. **crates.io:**
    - authorize crates.io for the org;
    - run `cargo owner --add github:cameleering:release` on the 20 already-published crates;
    - set up trusted publishing for `cameleering/cameleer`.
14. **Release 0.6.0**, following the 0.5.0 release steps:
    - versions, CHANGELOG, roadmap, README, the GitHub About;
    - the first publishes of `cameleer-attest` and `cameleer-health`, with org URLs from day one;
      add the team as owner right after each first publish;
    - images to both new namespaces, health variant included;
    - the archives workflow's first real run.
15. **Freeze the old image paths:**
    - update the `pmsudhi/cameleer` Docker Hub overview and the old ghcr package description to
      point to `cameleering/cameleer`;
    - stop pushing there; old tags keep resolving.
16. **Profiles:** pin the org on `pmsudhi`'s profile, and fill in the GitHub About and topics on
    `cameleering/cameleer`.

### Later

- Create `cameleering/saltire` when the crypto move starts.
- Publish simoom and karez when they meet their readiness bar.
- The family page on the site (SEO session).
- Optionally claim the bare `cameleer` crate for the CLI, and consider a trademark: the only way to
  the GitHub name `cameleer`.

## Risks

| risk | how to avoid it |
|---|---|
| ghcr stops updating silently | Use an explicit namespace in `container.yml` (step 7) before the transfer merges. |
| Domain hijack or downtime | Verify at org level before the transfer (step 3), and enforce HTTPS right after. |
| The redirects get broken | Never create `pmsudhi/cameleer` or `pmsudhi/rust-camel` again. Merge the rewrite the same day. |
| Sessions push to the old remote | Update every worktree's remote, and message the linked sessions on transfer day. |
| crates.io can't see the team | Grant crates.io org access first, and keep `pmsudhi` as a named owner. |
| Docker Hub org needs a paid plan | Fall back to a free account named `cameleering`. |
| A single owner locked out | A second owner if possible; otherwise several 2FA methods and offline recovery codes. |
| Branch protection is lost on private repos in a Free org | Acceptable while they're private. |

## Critical files

- `.github/workflows/container.yml`, `dockerhub-description.yml`, `traffic.yml`, `archives.yml`
- `Cargo.toml` (workspace `repository`) and `crates/*/Cargo.toml`, `CITATION.cff`
- `docker/README.dockerhub.md`, `docs/DEPLOYMENT.md`, `README.md`, `CONTRIBUTING.md`, `AGENTS.md`,
  `llms.txt`, `site/llms.txt`
- `docs/**` (282 files, mostly the "Found a problem?" footer), `site/**`, `examples/**`
- `CLAUDE.md`, `docs/STATUS.md`, `docs/design/github-organization.md` (marked superseded, with a
  link to this plan)

## Verification

- `git grep -n "pmsudhi/cameleer"` after the rewrite: only the listed exceptions remain.
- `cargo build --workspace` with zero warnings, `cargo test --workspace` green (the doc harness
  checks links and examples), and `python scripts/build-docs.py` clean.
- After the transfer:
  - the old GitHub URLs give 301 and `git ls-remote` works on them;
  - https://cameleer.ing serves over enforced HTTPS;
  - a manual `container.yml` run (`edge`) pushes to `ghcr.io/cameleering/cameleer` and
    `docker.io/cameleering/cameleer`, and `docker pull` works from both;
  - `cargo owner --list cameleer-core` shows the team;
  - the 0.6.0 crates on crates.io link to `github.com/cameleering/cameleer`.
