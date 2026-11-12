# RSS component

> The `rss` component polls an RSS feed (RSS 2.0, RSS 1.0 or Atom) and emits new entries as exchanges.

**Scheme:** `rss:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [RSS](https://camel.apache.org/components/next/rss-component.html)

Feeds and entries are maps. With `splitEntries=true` (the default) each exchange carries the feed
with only one entry in `entries`. For entry bodies, see [atom](atom.md).

## When to use it

- Build a news or security-advisory digest that an AI agent summarizes.
- Watch vendor status pages, blogs or release feeds and alert a channel.
- Turn any RSS feed into events on a queue.

## URI format

```text
rss:feedUrl[?options]
```

The feed URL can be `http:`, `https:` or `file:`. Options the component does not know are added
to the feed URL's query, as in Apache Camel.

## Example

Every five minutes, take the new entries of a feed and queue their title and link for a
summarizing agent.

<!-- example: rss-news-digest -->
```yaml
- route:
    id: news-digest
    from:
      uri: "rss:{{env:FEED_URL:https://example.com/feed.xml}}?delay=300000&throttleEntries=false"
      steps:
        - setBody:
            simple: "${body[entries][0][title]} - ${body[entries][0][link]}"
        - to: seda:digest
- route:
    id: summarizer
    from:
      uri: seda:digest
      steps:
        - log: "new item: ${body}"
```

## Rust DSL

```rust
fn news_digest_routes(b: &mut RouteBuilder, feed_url: &str) {
    b.from(&format!("rss:{}?delay=300000&throttleEntries=false", feed_url))
        .route_id("news-digest")
        .set_body(simple("${body[entries][0][title]} - ${body[entries][0][link]}"))
        .to("seda:digest");
    b.from("seda:digest").route_id("summarizer").log("new item: ${body}");
}
```

## Options

| option | default | description |
|---|---|---|
| `splitEntries` | `true` | One exchange per entry; `false` sends the whole feed. |
| `filter` | `true` | Only entries newer than the last one delivered (entries without a date are delivered once). |
| `lastUpdate` | | Initial filter date, `yyyy-MM-ddTHH:mm:ss` (UTC). |
| `sortEntries` | `false` | Sort entries by date. |
| `throttleEntries` | `true` | Deliver one entry per poll; `false` delivers all new entries at once. |
| `feedHeader` | `true` | Put the whole feed in the `CamelRssFeed` header. |
| `delay` | `60000` | Milliseconds between polls (`initialDelay` 500 and the other scheduler options). |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelRssFeed` | out | The whole feed (with `feedHeader=true`). |

Feed map: `title`, `link`, `description`, `author`, `publishedDate`, `type` (`rss_2.0`,
`rss_1.0`, `atom_1.0`), `entries`. Entry map: `title`, `link`, `uri` (guid / id), `description`,
`contents`, `author`, `publishedDate`, `updatedDate`, `categories`.

## Run it with Docker

```sh
docker run --rm -e FEED_URL=https://www.example.com/news/rss.xml \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/news-digest.camel.yaml
```

## Common questions

### How do I get every new entry, not one per poll?
Set `throttleEntries=false`.

### Does it remember entries after a restart?
No. It remembers the last date in memory; use `lastUpdate` to skip old entries after a restart.

### Can it read Atom feeds?
Yes; `rss` parses Atom too. Use [atom](atom.md) if you want the entry itself as the body.

## Related

- [atom](atom.md), [http](http.md).
- Apache Camel: [RSS component](https://camel.apache.org/components/next/rss-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
