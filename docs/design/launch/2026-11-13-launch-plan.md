# Cameleering launch: using the Microsoft 365 outage window

_2026-11-13. Plan, internal. Status: draft for the owner. Nothing is published until the owner
approves each step._

## 1. The news, as reported

On 12-13 November 2026, Microsoft 365 had a global outage. The affected services included
Exchange Online and Outlook, Teams, SharePoint Online, Copilot 365 and Defender XDR, later also
Planner, To Do, Purview, Intune, Defender for Office 365 and part of Power BI. Outlook alone drew
more than 120,000 Downdetector reports. Microsoft tracked it as advisories MO1550821 and EX1550543.

What Microsoft has said so far:

- **Not more traffic:** "the elevated load isn't driven by an increase in traffic, but by a number of
  servers being unexpectedly unavailable; straining the remaining infrastructure."
- **Restarts:** they were restarting and validating servers, and assessing whether to revert "a small
  number of recent configuration updates".
- **Relapses under load:** "The servers we returned to service can become unresponsive again once
  they receive traffic." They limited which types of traffic reached the affected infrastructure.
- **Health checks:** reports say servers were failing automated health checks.
- **Not an attack:** there was no indication of a cyberattack.
- **No root cause yet:** no post-incident review and no root cause have been published.

Sources: Microsoft 365 Status on X, ABC News, BleepingComputer, Forbes, Tom's Guide (links in the
article's sources).

## 2. Why this is our moment, and where the line is

**The overlap with the practice.** The service fell over under the same traffic on fewer servers,
and restarted servers fell over again once traffic reached them. Headroom, the margin a fleet keeps
when part of it is gone, comes down to what each request costs (CPU, memory, connection setup) and
how fast a server comes back. Those are efficiency properties, and they can be measured before an
incident. That is cameleering's subject. The home page already says "Shrink what your software
needs, not just the instances".

**Where the line is:**

- **No guessing at the cause.** We don't speculate on the root cause, and we say so on the page.
- **No overclaiming.** We never claim Cameleer or simoom would have prevented the outage.
  Efficiency buys margin, not immunity.
- **No gloating.** We say nothing that mocks Microsoft or the people working the incident. The
  brand rule applies: "the thing to fight is waste, not the people".
- **Our losses go in the article.** simoom's overload result is in it: 10th of 19 on answers within
  1 ms. That keeps us credible, and the ledger already publishes it.
- **We update.** When Microsoft publishes its post-incident review, the page gets a dated update. If
  the review points at something unrelated to efficiency (for example a bad configuration push),
  the update says so plainly.

## 3. The launch

The site restructure went live today (Cameleering/cameleer#33). It has a home page with outcomes and
a playbook, the ledger, the method, FinOps, and two project microsites. The launch announces
Cameleering itself, and the outage essay is the hook.

| when | what | who |
|---|---|---|
| **T0, today** | Publish the anchor essay `/headroom/`: "Headroom: what is left when servers drop out". It also adds an articles section (`/articles/`, with an Atom feed at `/feed.xml`) and links from the home page | this session builds; owner approves the PR |
| T0 | Add a "degraded-fleet test" (lose a third of the servers, restart under load, connection storm) as a section of the method page, so the essay's call to action is concrete | this session |
| T0 + 2-4 h | **Owner posts**, in the owner's own words. I draft; the owner edits and sends: LinkedIn (CTO and FinOps audience), Hacker News ("Headroom: what is left when servers drop out", linking the essay, not the home page), Lobsters, the Rust and SRE communities where self-posts are allowed. Don't post in r/sysadmin or r/Office365 threads about the outage: it reads as ambulance-chasing | owner |
| T0 + 1 day | IndexNow already runs on deploy; submit `/headroom/` in Bing Webmaster Tools and Search Console once verified | owner (A1) |
| When Microsoft publishes its review | Dated update on `/headroom/`, plus a short follow-up article if the review teaches something new | this session |
| T0 + 1-2 weeks | Phase 5 of the restructure: Docker Hub description, live org profile, Context7 | this session |

## 4. Content strategy: a series, not a single post

**The pillar:** *headroom and recovery as efficiency.* It sits next to the existing pillars: FinOps
(money), Wirth's law (bloat), and when to rewrite (decisions). Every article ends with the playbook
and the ledger.

| # | article (URL) | target queries | proof we hold | status |
|---|---|---|---|---|
| 1 | **Headroom: what is left when servers drop out** (`/headroom/`) | outage lessons, server headroom, capacity planning after an outage | Cameleer 1.7-8.7x throughput on the same CPUs; simoom 43% less CPU per request; the overload loss | drafting now |
| 2 | **How to test headroom before an incident** (method section, then `/degraded-fleet-test/`) | degraded fleet test, chaos testing capacity, n+1 capacity | the method page; the scripts in `scripts/cloud-bench` | next |
| 3 | **Cold start is a recovery problem** (`/cold-start-recovery/`) | restart under load, thundering herd, warm-up | Cameleer 3 ms vs 1.1 s (30 ms native); Lambda initialization billing | week 1 |
| 4 | **The cost of a secure connection** (`/tls-handshake-cost/`) | tls handshake cpu cost, connection storm | simoom 9,396 / 12,388 handshakes/s, 1.39x / 1.56x H2O, behind axum + rustls | week 1-2 |
| 5 | **Overload: what our own benchmark shows** (`/overload/`) | load shedding, overload p99, admission control | simoom 10th of 19 within 1 ms at 195k req/s; actix-web 22x more in time; what we learn | week 2 |
| 6 | **Retry storms and retry budgets** (`/retry-budgets/`) | retry storm, exponential backoff jitter | public sources only, with a playbook step | week 2-3 |
| 7 | **The outage review, read through headroom** (update to #1) | microsoft 365 outage root cause | Microsoft's review when published | when published |

**Rules for every article:**
- every external fact is cited and dated;
- our numbers are only those on the ledger, each with its project and baseline;
- at least one of our own losses where it's relevant;
- no "rewrite it in Rust" framing;
- one H1, title of 60 characters or fewer, FAQ JSON-LD that matches the visible FAQ;
- linked from `/articles/` and the feed, and from the home page's further-reading cards.

## 5. Site changes in this launch branch (`launch-headroom`)

1. `/headroom/`: the anchor essay.
2. `/articles/`: an index of the essays, newest first: headroom, FinOps, when to rewrite, AI code
   performance, Wirth's law.
3. `/feed.xml`: an Atom feed, generated by `scripts/build-docs.py` from a short list.
4. Navigation: "Articles" in the Cameleering header and in the family menu.
5. Home page: a "Latest" line under the hero pointing to `/headroom/`, and the essay in further
   reading.
6. `llms.txt`: the new pages.
7. The method page: the degraded-fleet test section.

## 6. Owner decisions

| # | decision | recommendation |
|---|---|---|
| L1 | Publish `/headroom/` today, before Microsoft's review, with the "no root cause yet" note and the update promise? | yes: the window is now, and the page claims nothing about the cause |
| L2 | Hacker News and LinkedIn posts in the owner's own words (drafts provided) | yes, owner posts; never from an automated account |
| L3 | Mention Microsoft in the title? | no: the title is about headroom; Microsoft is named in the first paragraph, factually |
| L4 | Approve the PR and merge | after reading the essay |
