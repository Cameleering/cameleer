# Atom component

> The `atom` component polls an Atom feed and emits each new entry as an exchange whose body is the entry.

**Scheme:** `atom:` · **Crate:** `cameleer-saas` · **Cargo feature:** [`saas`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Atom](https://camel.apache.org/components/next/atom-component.html)

`atom` shares its implementation with [rss](rss.md) and parses Atom 1.0, RSS 2.0 and RSS 1.0.
The difference: with `splitEntries=true` the body is the entry map itself (`${body[title]}`),
not a feed holding one entry.

## When to use it

- Watch GitHub release feeds (`https://github.com/<owner>/<repo>/releases.atom`) and start an
  upgrade check or notify a team.
- Follow blogs and changelogs that publish Atom.
- Feed new entries to an AI agent for summaries or tagging.

## URI format

```text
atom:feedUrl[?options]
```

## Example

Check a project's release feed every hour and queue each new release.

<!-- example: atom-release-watch -->
```yaml
- route:
    id: release-watch
    from:
      uri: "atom:{{env:RELEASES_FEED:https://github.com/apache/camel/releases.atom}}?delay=3600000&throttleEntries=false"
      steps:
        - setBody:
            simple: "new release ${body[title]}: ${body[link]}"
        - to: seda:releases
- route:
    id: notify-release
    from:
      uri: seda:releases
      steps:
        - log: "${body}"
```

## Rust DSL

```rust
fn release_watch_routes(b: &mut RouteBuilder, feed_url: &str) {
    b.from(&format!("atom:{}?delay=3600000&throttleEntries=false", feed_url))
        .route_id("release-watch")
        .set_body(simple("new release ${body[title]}: ${body[link]}"))
        .to("seda:releases");
    b.from("seda:releases").route_id("notify-release").log("${body}");
}
```

## Options

The same as [rss](rss.md#options): `splitEntries`, `filter`, `lastUpdate`, `sortEntries`,
`throttleEntries`, `feedHeader`, and the scheduler options (`delay` 60000 ms). Unknown options
are added to the feed URL's query.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAtomFeed` | out | The whole feed (with `feedHeader=true`). |

Entry map: `title`, `link`, `uri` (the Atom `id`), `description` (summary), `contents`,
`author`, `publishedDate`, `updatedDate`, `categories`.

## Run it with Docker

```sh
docker run --rm -e RELEASES_FEED=https://github.com/apache/kafka/releases.atom \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/release-watch.camel.yaml
```

## Common questions

### Why do I only get one entry per poll?
`throttleEntries` defaults to `true`, as in Apache Camel. Set it to `false` to get all new
entries at once.

### How do I skip old entries on the first poll?
Set `lastUpdate=2026-11-01T00:00:00`; only newer entries are delivered.

## Related

- [rss](rss.md), [http](http.md).
- Apache Camel: [Atom component](https://camel.apache.org/components/next/atom-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
