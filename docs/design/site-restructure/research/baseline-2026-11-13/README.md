# Baseline before the restructure, 2026-11-13

_Phase 0 of [the plan](../../plan.md). This is what cameleer.ing looked like before any URL changed,
so that the move can be judged against it. Site at `179ebb3` (main after PR #32)._

## Captured here

| file | what |
|---|---|
| [sitemap-urls.txt](sitemap-urls.txt) | the 345 URLs in `site/sitemap.xml`: 30 designed pages, 279 docs pages, 34 simoom pages and the 2 llms files |
| [live-status.txt](live-status.txt) | the HTTP status of each of those URLs on the live site: all 345 answered 200 |
| [old-hosts.txt](old-hosts.txt) | the site's earlier addresses |

**Old hosts.**
- `cameleering.github.io/cameleer/…` answers **301** to cameleer.ing, path for path.
- `pmsudhi.github.io/cameleer/…`, which was the address until 2026-11-07, answers **404** for every
  path checked. The plan's owner action A6 (a `pmsudhi/pmsudhi.github.io` repository with stubs)
  would recover those links.

**Downloads and pulls** (public APIs, 2026-11-13):

| where | name | version | count |
|---|---|---|---|
| crates.io | `cameleer-main` | 0.6.0 | 70 downloads |
| crates.io | `cameleer-core` | 0.6.0 | 531 downloads |
| crates.io | `simoom` | 0.1.0 | 9 downloads |
| Docker Hub | `cameleering/cameleer` | | 225 pulls |
| Docker Hub | `pmsudhi/cameleer` (old namespace, stops at 0.5.1) | | 842 pulls |

**Search sample.** [search-discovery.md](../search-discovery.md) §0 is from 2026-11-13 and used one
search tool, US results, not Google's index. It found no cameleer.ing result for `cameleer`,
`cameleering`, `simoom` or `site:cameleer.ing`. "apache camel rust" is held by
kennycallado/rust-camel.

## Not captured yet (needs the owner or accounts)

- **Search Console and Bing Webmaster Tools** (plan A1). Neither shows data from before verification,
  so verify now. The move doesn't wait for this data.
- **GitHub traffic and referrers** (plan A2). They need a new `TRAFFIC_TOKEN`. GitHub keeps 14 days.
- **The 25-prompt set** ([llm-agent-discovery.md](../llm-agent-discovery.md) §6), run three times each
  against:
  - ChatGPT, Claude, Gemini and Perplexity, with search on and off;
  - two coding agents.

  Logged-out or fresh sessions are needed, and that is manual work. Run it before the cut-over
  (phase 4), and record mention rate, correct facts, confusion and first-try build results here.
- **Manual SERP checks in Google and Bing** for the brand and the top 20 queries in search-discovery
  §1.
