# Cameleering brand guide (internal)

_Decided 2026-11-12 with the owner. Public pages: https://cameleer.ing/cameleering/ (definition,
principles, process), `/cameleering/ledger/`, `/cameleering/method/`. This file is the reference for
every repo and session; it is not published on the site._

## The words

- **Cameleering** is the method (a verb) and the organization (GitHub org `Cameleering`, lowercase
  in URLs: `cameleering`).
- **Cameleer** is one product: the integration engine in the style of Apache Camel, and the first
  Cameleering project.
- Canonical sentence: "Cameleer is the first Cameleering project."
- Never write "Cameleering's integration engine" or "Cameleering is 54x leaner". Numbers belong to
  the project that measured them.
- The etymology: a cameleer drives a camel caravan. The camel carries a heavy load a long way on
  little water.

## Definition (use verbatim)

> Cameleering is the practice of making software carry the same load on less (less time, memory,
> size and dependencies) and proving every gain against the original, on the same hardware, with the
> method published.

- **Tagline:** "Carry more on less."
- **House rule** (sub-line everywhere): "No claim without a number."

## Principles

1. Measure before you claim.
2. Baselines over boasts: same machine, exact versions, both sides published.
3. Same job, same contract: keep the names and behaviour users rely on.
4. Pack only what the journey needs.
5. Real waiting over felt waiting.
6. Budgets in the build: CI fails on size, memory, startup and p99 regressions.
7. Rewrite when the numbers say so: a tool, not a creed. Language neutral.
8. Publish the losses.

**Process:** Pick, Baseline, Budget, Cut, Prove, Publish, Guard.

## Positioning

- **Category:** efficiency engineering. Cameleering is the named method inside it.
- **Audiences:** the root (practice first) speaks to CTOs under cloud-cost pressure, FinOps
  practitioners and efficiency-minded engineers; product traffic (platform teams on JVM integration,
  agent builders, healthcare integration, Rust developers) lands on the project microsites by deep
  links and must never be routed through the practice pages.
- **Versus FinOps or buying more cloud:** it fixes the software.
- **Versus "rewrite it in Rust":** every claim is measured against the original.
- **Responsibility:** prominent, by the owner's choice, but always tied to people and measured
  resources: who pays for the machines, who waits, the power drawn. It is never aimed at
  developers ("the thing to fight is waste, not the people who build features"). No carbon
  headline claims; energy figures only as cited context.

## Architecture: endorsed brands

- Each project keeps its own name. Its README's first lines say "A Cameleering project", with a
  status badge linking to its ledger row.
- A project carries the endorsement only once its repo is public and it has a ledger row, even if
  that row says "not measured".
- Footer line on the site and docs: "Cameleer is a Cameleering project: carry more on less, no claim
  without a number."
- ASF: factual comparisons only; "Camel" is never used alone in a name; the disclaimer goes
  everywhere.

## Domains

- **Done (2026-11-13, phase 2 of the plan in `docs/design/site-restructure/plan.md`):** the
  cameleer.ing root is the Cameleering home. The practice pages sit at the top level (`/ledger/`,
  `/method/`, `/finops/`, ...), Cameleer's pages under `/cameleer/` (its docs stay at `/docs/`),
  simoom under `/simoom/`. Every old URL keeps a redirect stub (`scripts/redirects.txt`).
- Reserved slugs: a project takes its own name (`/cameleer/`, `/simoom/`, later `/karez/`, `/ksar/`);
  the practice takes common nouns (`/ledger/`, `/method/`, `/about/`, `/projects/`, `/security/`).
- Navigation: one bar per page. Project pages show the lockup "Cameleering / Cameleer" with the
  project mark larger, and a Cameleering menu (`scripts/build-docs.py` `FAMILY_MENU`); simoom's
  build carries the same menu.
- What Cameleering offers (owner, 2026-11-13): open source only, and open to consulting. Say it in one
  line where it is asked, never as a pitch.

## The ledger rules

- Only measured rows carry numbers.
- Statuses: measured, in progress, design, planned.
- Targets are hollow figures and never sit in the result column.
- Both ends of every range ("4.8-54x").
- A result older than 12 months is stale.
- Rows are never deleted: a newer run supersedes the old row, which stays visible, struck through.
- Losses go in the same table as the wins.
- Every row links to its crossing report: Load, Baseline, Terrain, Water, Distance, Losses, Repeat
  it, Date.

## Labels

- **Cameleered** (measured): a named current baseline, the same hardware, a rerunnable script and
  raw results, losses disclosed, measured within 12 months.
- **Cameleering** (in progress): targets only.

## Naming rule for projects

- One real lowercase word from desert travel, water, wind, terrain or caravan craft, 4-7 letters.
- Its literal meaning matches the function.
- No sacred terms and no names of peoples.
- It must be free on crates.io and GitHub, and pass a basic trademark search.

Current names:
- **simoom:** a desert wind, used for the HTTP engine.
- **karez:** an underground water channel, used for data connectivity.
- **ksar:** a fortified granary that keeps stores safe, used for the cryptography family (primitives
  and PKI). It replaces "saltire": the owner chose it on 2026-11-12. Availability checked
  2026-11-12:
  - **ksar** (a fortified granary): the crate is free; the only clash is kSar, a Java tool that
    plots `sar` statistics.
  - **seif** (a sharp dune; also "sword"): the crate is free, but it sits close to PayPal's crypto
    library "seifnode".
  - **serai** (a caravanserai): the crate is taken, and serai-dex is a crypto project.
  - The owner chose **ksar**.

## Voice

- A field engineer's logbook: plain, specific, calm.
- Use: carry, load, budget, baseline, ledger, measured, crossing.
- Avoid: blazing, revolutionary, game-changing, "Camel killer", bloat as an insult, Orientalist
  imagery, and "replacement for Apache Camel". Say "an independent Rust implementation in the style
  of Apache Camel".

## Sources behind the claims

- The specialist reports of 2026-11-12: brand architecture; creative direction; market and search
  research, with URLs. The sources the site uses are listed on `/cameleering/`.
- Cameleer numbers: `benchmarks/results/2026-11-06-linux/summary.json` (`mem_min_x` 4.87, rounded
  down to 4.8; `mem_max_x` 54.13; `tput` 1.75-8.72; startup 2.45 ms vs 1106 ms; Quarkus native
  30.4 ms).

## Logo: the ledger tile (chosen 2026-11-12)

- **The mark:** a camel's hump drawn as a measured curve (gold `#d99a22`) on three ruled ledger
  lines (`#33406a`), ending in one plotted point (white), on an ink tile (`#18203a`, rounded
  corners). It reads as the camel, and as a measurement written in a ledger.
- **Small sizes:** at 32 px and below, use the simplified mark (no ruled lines, a thicker curve,
  a bigger point).
- **Files** in `site/brand/cameleering/`:
  - `cameleering-mark.svg`, the master;
  - `cameleering-mark-dark.svg`, for dark backgrounds (a white tile);
  - `cameleering-mark-{512,192,64,32}.png` and `cameleering-apple-touch-icon.png`;
  - `cameleering-avatar.svg` and `cameleering-avatar-512.png`, a full-bleed square for the GitHub
    org avatar (GitHub rounds it);
  - `cameleering-lockup.svg` and `cameleering-lockup-dark.svg`, the mark plus the wordmark in
    Schibsted Grotesk 800, a web font, so a page using them must load it;
  - `cameleering-og.png`, the 1280x640 social preview for the Cameleering pages.
- **Rebuild:** `python scripts/brand/cameleering-logo.py <repo-root>` (needs Pillow and the Segoe UI
  fonts on Windows).
- **Rules:**
  - Keep a clear space of a quarter of the tile around the mark.
  - Don't recolour the curve or stretch the tile.
  - Never combine the mark with Cameleer's "C" mark in one symbol. Cameleer keeps its own mark,
    as a Cameleering project.
