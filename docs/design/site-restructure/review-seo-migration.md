# Review: site restructure plan, SEO and migration

_Adversarial review of `plan.md` (draft, 2026-11-13), checked against `site/` at `bde29d8`,
`scripts/build-docs.py`, `.github/workflows/{pages,ci}.yml` and the live site. Internal._

Findings are ranked blocker, major, minor. Each gives the evidence, why it matters, and the change
to the plan.

## Blockers

### B1. The cut-over (phase 4) can't pass its own gates. The plan defers the fixes to phases 5 and 6

**Evidence.**
- In-repo links to paths that move: `grep -rE 'https://cameleer\.ing/(compare|use-cases|alternatives|mcp|trust|healthcare|cameleering)'`
  finds 33 links in `docs/README.md`, `docs/guides/{faq,mcp,agent-tool-calls}.md`,
  `docs/BENCHMARKS.md`, `docs/packs/health/index.md`, `docs/release-notes/0.6.0.md`, `README.md`
  and `docker/README.dockerhub.md`.
- simoom links to `/cameleering/`: the copied simoom pages hold 66 relative links to
  `/cameleering/` (`../../../cameleering/` ×52, `../../cameleering/` ×11, ...), plus 14 absolute
  `https://cameleer.ing/cameleering/` links.
- simoom "Cameleer" links: 34 links whose anchor text is "Cameleer" point at the root
  (`<a href="../../../">Cameleer`). After the cut-over the root is Cameleering.

**Why it matters.**
- The §8.7 known-URL test runs in `cargo test`, inside the required `ubuntu` and `windows`
  checks. It fails on the phase 4 PR, so that PR can't merge. If the test is softened to let it
  merge, `main` goes red and blocks every other PR.
- The §8.6 check "no internal link to a stub" fails in `pages.yml` because of the docs pages and
  `site/simoom/`, so the deploy aborts.
- If both checks are bypassed, simoom sends visitors who click "Cameleer" to the practice page
  until phase 6. That is exactly the mislabel the inventory warned about.

**Change.**
- Move every in-repo rewrite into the cut-over PR: docs markdown, `README.md`, the Docker Hub
  README source, `AGENTS.md`, the org profile source and llms.
- Do phase 6 before phase 4. The simoom repository prepares a build that targets the new paths,
  and that copy lands in the same cut-over PR.
- Keep the outward-facing actions in phase 5: Docker Hub sync, the live org profile, Context7 and
  release metadata.
- Exclude `docs/release-notes/**` and `CHANGELOG.md` from the known-URL test. Published release
  bodies can't change.
- The simoom snippet-version check can only start once simoom carries the snippet. It can't be in
  phase 0.

### B2. No site check runs before a merge

**Evidence.**
- `ci.yml` runs only cargo, the SBOMs and the container image.
- `build-docs.py` runs only in `pages.yml` (line 29), after the push to `main`.
- The ruleset requires exactly three checks: ubuntu, windows and container.

**Why it matters.**
- The plan's safety rests on "checks that fail the build", but none of them gate a PR.
- A broken cut-over merges, fails in the deploy, and leaves `main` and the live site out of step.
- With in-place partials, a page committed without running the build deploys different HTML than
  the repository holds.

**Change.**
- In phase 0, add a step to the `ubuntu` job in `ci.yml`: `python3 scripts/build-docs.py`, then
  all the §8.6 checks, then `git diff --exit-code site/`. The step is then covered by an existing
  required check, so the ruleset doesn't change.

## Major

### M1. Drop `noindex` from the new stubs

**Evidence.** `write_moved` (build-docs.py:1253) and plan §8.2 emit a canonical, `noindex` and a
refresh together.

**Why it matters.**
- Google's canonicalization guidance says not to combine `noindex` with `rel=canonical`. They are
  conflicting hints.
- An instant meta refresh plus the JS line is already the strongest redirect signal Pages can
  give. `noindex` adds nothing when Google honours the redirect. When Google doesn't, the URL is
  dropped without its signals being consolidated.
- It also breaks the plan's own measurement (§10, "stubs treated as redirects vs Not found"): in
  Search Console, the stubs will show up as "Excluded by 'noindex' tag" instead of "Page with
  redirect".

**Change.** Stubs carry the absolute canonical, `location.replace` (keeping the query and the
hash), the 0-second refresh and a visible link. No `noindex`. Retarget the 5 existing docs stubs
the same way.

### M2. Phase 2 puts `noindex` duplicates into production

**Evidence.**
- Each phase is its own PR to `main`.
- `pages.yml` deploys every push that touches `site/**`.
- Phase 2 ships `/cameleer/*` with `noindex` and self-canonicals.

**Why it matters.**
- Anything that finds those URLs records them as noindex. Google re-crawls noindexed URLs less
  often, so lifting `noindex` in phase 4 is noticed slowly. That hits exactly the 22 pages being
  moved.
- For one to three weeks, every content edit has to be made in two trees.

**Change.**
- Phases 2, 3 and 4 become stacked commits on one branch, reviewed commit by commit and merged as
  one PR.
- Phases 0 and 1 merge earlier, because they change no URLs.
- Freeze content edits to the 22 pages while that branch is open.

### M3. The sitemap index has scope and IndexNow mistakes

**Evidence.**
- Plan §2.1: `/cameleer/sitemap.xml` lists the designed pages and `/docs/`.
- `pages.yml` collects URLs with `re.findall(r"<loc>([^<]+)</loc>", open("site/sitemap.xml"))`
  and posts them all on every deploy.

**Why it matters.**
- The sitemaps.org location rule says a sitemap at `/cameleer/` may only list `/cameleer/…`
  URLs. Google relaxes the rule only for cross-submission through robots.txt or Search Console.
  Bing and others apply it.
- Once `sitemap.xml` is an index, the regex submits three sitemap URLs to IndexNow instead of
  pages.
- Resubmitting all 345 URLs on every push is the pattern IndexNow asks senders to avoid.

**Change.**
- Put the docs in a root-level child sitemap: `/sitemap-root.xml` or `/sitemap-docs.xml`.
  `/cameleer/sitemap.xml` keeps only `/cameleer/` URLs.
- Make IndexNow expand the children.
- Submit only URLs whose `lastmod` changed since the last deploy, plus the old URLs once at the
  cut-over.

### M4. Every result on the host will show Cameleering's site name and icon

**Evidence.**
- Google picks one site name per domain or subdomain. Subdirectories are not supported.
- Google picks one favicon per host, from the home page's `<link rel=icon>`.
- Today the root links `favicon-32.png`, the Cameleer mark (`site/index.html:19-21`). simoom
  pages and the docs load the same files (`../../../favicon-32.png` ×26).

**Why it matters.**
- From the cut-over day, every `/cameleer/`, `/docs/` and `/simoom/` result shows "Cameleering"
  and its tile.
- If the root favicon files are overwritten in place, simoom and the docs switch icons silently.
- §13 covers branded landing pages but not how results look in search.

**Change.**
- State the trade-off in §13.
- Set the `WebSite` `name` to "Cameleering" and the `alternateName` to "cameleer.ing".
- The new root points at the existing `brand/cameleering/` icons. Never overwrite
  `/favicon-*.png`, `/og.png` or `/apple-touch-icon.png`: social caches, simoom and the docs
  reference them.
- Add a `/favicon.ico`.

### M5. Rollback is slow, and it stops working after phase 5

**Evidence.** A revert is a PR that needs three checks (a 90-minute Windows timeout) and an
up-to-date branch. The `github-pages` environment normally limits deploys to `main`.

**Why it matters.**
- A rollback takes about 1-2 hours, not minutes.
- By then IndexNow has announced the new URLs.
- A revert does not undo the Docker Hub README, the live org profile, Context7, the simoom
  repository or a release's `homepage`.

**Change.**
- Record the last good commit.
- Open the revert branch before merging the cut-over.
- Freeze other merges on the cut-over day.
- Wait one to two weeks after the cut-over before the outward-facing phase 5 work.
- Say in the plan that rollback is only cheap during that window.

### M6. `pmsudhi.github.io/cameleer/` is dead, which research contradicts

**Evidence.**
- `search-discovery.md:129` says the old URL is mapped "path for path".
- Live check: `curl https://pmsudhi.github.io/cameleer/compare/apache-camel-vs-cameleer/` returns
  **404**, while `cameleering.github.io/cameleer/compare/` returns a 301 to cameleer.ing.
- `CHANGELOG.md:265` still links the pmsudhi URL.
- That host was the site's address until 2026-11-07, so any backlinks or index entries point
  there.

**Change.**
- Add an owner action: if pmsudhi has no user site, a `pmsudhi/pmsudhi.github.io` repository with
  stubs under `cameleer/…` pointing at the final URLs.
- Record the lost URLs in the baseline.

### M7. Webmaster verification is a prerequisite, not an optional action

**Evidence.** A1 sits with the owner actions. Search Console and Bing show no data from before
verification.

**Why it matters.** The before/after comparison in §10 and the "URL inspection on the top 5"
check need verified properties weeks before the cut-over.

**Change.**
- Make A1 the first step of phase 0: Search Console by DNS, and Bing imported from Search
  Console.
- Accept that the baseline is mostly SERP and prompt checks, and don't hold the move for data.

## Minor

1. **The counts don't agree.**
   - §2.1 says "It changes 24 URLs". That is the inventory's option C, before D2.
   - With D2, 30 URLs change: the root swap, 22 section pages and 7 `/cameleering/` pages, giving
     **29 stubs**.
   - §2.1 says "six stubs" for the practice pages. It is seven: `/cameleering/` → `/` is missing
     from the URL table.
   - Phase 4 says "30 URLs, all stubbed", but the root is not a stub.
2. **The root H1 claim is wrong.** `site/index.html:276-280` has an `<h1>` with an `aria-label`
   and `aria-hidden` line spans, so its text is in the DOM. The real fix is to keep the
   struck-through words out of the heading's accepted text, not to add a missing H1. Verified
   claims: 8 titles over 65 characters, 20 descriptions over 165, meta keywords only on the root,
   no `ItemList` on the alternatives pages.
3. **New source directory doesn't trigger a deploy.** `site-src/llms/` is not in the `pages.yml`
   `paths` filter (line 6), so llms edits wouldn't deploy. Add it, and any `scripts/*.json`
   table.
4. **The 404 prefix map is wrong.**
   - It maps `/trust/x` → `/cameleer/trust/x`, which doesn't exist.
   - It leaves out `/cameleering/`.
   - The research's `$0` keeps the leading slash, which gives `/cameleer//…`.
   - Every known old URL has a stub, so the map only fires for paths that never existed.
   - Drop the map and show links only, or fix it and test it.
5. **`/sitemap.xml` and the `lastmod` churn.**
   - `write_sitemap` takes each designed page's date from the `git log` of the output file
     (build-docs.py:1180). With in-place partials, every header tweak bumps all 30 dates.
   - Engines learn to ignore inaccurate `lastmod`.
   - Take the date from content outside the partial regions.
6. **Hidden cost: `.md` twins for designed pages.** There is no HTML-to-markdown converter.
   Writing one std-only, or keeping hand-made duplicates, means drift. Limit this to the llms
   bundles, or generate both forms from one source.
7. **`/llms-full.txt` no longer matches `/llms.txt`.** It stays Cameleer-only while `/llms.txt`
   becomes the family index, which breaks the "full version of llms.txt" convention. State the
   scope in the first lines of both files.
8. **robots.txt.** Named AI agent groups with `Allow: /` are redundant with `User-agent: *`
   (`site/robots.txt`). A named group also overrides `*`, so a later `Disallow` under `*` would
   silently skip those agents. Keep `*` only.
9. **Brave can't be "submitted to".** It has no IndexNow and no webmaster submission (§7.6).
   Rely on links and crawling.
10. **JSON-LD.**
    - Use a PNG logo of at least 112 px for `#org`: `brand/cameleering/cameleering-mark-512.png`.
    - Remove the existing `Organization` without an `@id` from `/cameleering/` when it moves, so
      that only one `#org` node exists.
    - `SoftwareSourceCode` earns no rich result. Never add `aggregateRating`, because no reviews
      exist. `offers` with price 0 is optional.
    - Give `DefinedTerm` an `inDefinedTermSet`.
    - simoom's `BreadcrumbList` and `#org` links are simoom-repository work. List them in phase 6.
11. **Files that would be published by accident.** `site/_redirects.json` and `_partials/` get
    published because `.nojekyll` is set, and `_redirects` means something else on Netlify and
    Cloudflare. Keep the table in `scripts/`.
12. **simoom and the docs load the shared CSS.** They load `/assets/site.css` (×26 in simoom).
    Family-bar CSS added there ripples into the simoom copy. Put it in its own file, and add
    `/simoom/` to the browser pass.
13. **Files the plan should name as never moving:** the IndexNow key file, `og.png`, favicons,
    `/brand/` and `/assets/`. The `designed_pages()` walk and the sitemap check must ignore them.
14. **D4.** `SECURITY.md:8-10` names GitHub private vulnerability reporting, not an address.
    `/security/` should link to it.
15. **Top-level practice slugs.** `/method/`, `/ledger/`, `/projects/` and `/security/` share the
    namespace with project microsites. Add a reserved-slug rule: projects take their own name,
    the practice takes common nouns. The move is sound otherwise: authority is near zero and the
    prefix is redundant.

## What the plan gets right (keep it)

- **Keep `/docs/`.** That keeps 279 pages and 275 `.md` twins in place, and the published crates'
  `documentation` field keeps working. Search engines don't reward directory symmetry.
- **One move, soon.** No rankings to defend yet (§1.1), and stubs kept forever.
- **The JS `location.replace` before the meta refresh.** It keeps the fragment and the query.
- **Old home anchors** forwarded from `/` to `/cameleer/#id`, with those ids banned on the new
  root.
- **The build fails** on a stub over a real page, on a missing target, or on a chain.
- **The `/cameleer/` title starts with "Cameleer"**, with no interstitials, the endorsement chip
  instead of a band, and numbers kept with the project that measured them.
- **Phase 1 proves the partials leave the output unchanged** before any URL moves.
- **`/llms.txt` answers "what is Cameleer" in its first 15 lines.** The 2.1 MB file is split into
  bundles of at most 150 KB.
- **The known-URL Rust test.** Its scope needs fixing (B1), but the idea is right.
- **Frozen identifiers are left alone:** `https://cameleer.ing/sbom/`, `ing.cameleer`, old
  crates' `homepage` and the OCI labels.
