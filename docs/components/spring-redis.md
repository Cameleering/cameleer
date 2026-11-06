# Spring Redis component

> The `spring-redis` component runs Redis commands (strings, hashes, lists, sets, sorted sets, geo, pub/sub) and subscribes to Redis channels.

**Scheme:** `spring-redis:` · **Crate:** `cameleer-datastores` · **Cargo feature:** [`datastores`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Spring Redis](https://camel.apache.org/components/next/spring-redis-component.html)

## When to use it

- Short-term memory for an AI agent: append each conversation turn to a list per session and
  read the list back before the next model call.
- Cache expensive results (LLM answers, API lookups) with `SETEX` and read them with `GET`.
- Counters, rate limits and leaderboards with `INCR` and sorted sets.
- Lightweight messaging: publish to a channel, or consume a channel with `SUBSCRIBE` and
  `PSUBSCRIBE`.

The name comes from Apache Camel, where the component is built on Spring Data Redis. Cameleer
has no Spring: it has its own RESP2/RESP3 client, and keeps the scheme, options and headers so
Camel routes run unchanged.

## URI format

```text
spring-redis://host:port[?options]
```

The port defaults to 6379. The producer runs `command` (default `SET`, header
`CamelRedis.Command` overrides it) with arguments from the `CamelRedis.*` headers and puts the
reply in the body. Commands that return nothing in Spring Data Redis (`SET`, `HSET`, `PUBLISH`,
...) keep the body.

## Example

`POST /memory?session=s1` appends the request body to the session's list. The reply of `RPUSH`,
the new length of the list, becomes the response.

<!-- example: spring-redis-remember -->
```yaml
- route:
    id: remember
    from:
      uri: "platform-http:/memory?httpMethodRestrict=POST"
      steps:
        - setHeader:
            name: CamelRedis.Key
            simple: "memory:${header.session}"
        - setHeader:
            name: CamelRedis.Value
            simple: "${body}"
        - to: "spring-redis://{{env:REDIS_HOST:localhost}}:6379?command=RPUSH"
```

`GET /memory?session=s1` returns every turn of the session as a JSON array, oldest first.

<!-- example: spring-redis-recall -->
```yaml
- route:
    id: recall
    from:
      uri: "platform-http:/memory?httpMethodRestrict=GET"
      steps:
        - setHeader:
            name: CamelRedis.Key
            simple: "memory:${header.session}"
        - setHeader:
            name: CamelRedis.Start
            constant: 0
        - setHeader:
            name: CamelRedis.End
            constant: -1
        - to: "spring-redis://{{env:REDIS_HOST:localhost}}:6379?command=LRANGE"
        - marshal:
            json: {}
```

This route subscribes to the `alerts` channel and logs every message.

<!-- example: spring-redis-subscribe -->
```yaml
- route:
    id: alerts
    from:
      uri: "spring-redis://{{env:REDIS_HOST:localhost}}:6379?command=SUBSCRIBE&channels=alerts"
      steps:
        - to: "log:alerts"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:cache-answer")
        .set_header("CamelRedis.Key", simple("answer:${header.questionId}"))
        .set_header("CamelRedis.Value", simple("${body}"))
        .set_header("CamelRedis.Timeout", constant(3600))
        .to("spring-redis://localhost:6379?command=SETEX");
    b.from("spring-redis://localhost:6379?command=PSUBSCRIBE&channels=orders.*")
        .log("${header[CamelRedis.Channel]}: ${body}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `command` | `SET` | command to run (header `CamelRedis.Command` overrides it); `SUBSCRIBE` or `PSUBSCRIBE` for the consumer |
| `channels` | | consumer: comma-separated channels or patterns |
| `password` | | `AUTH` password |
| `username` | | ACL user name (Redis 6+) |
| `database` | `0` | `SELECT` this database |
| `protocol` | `2` | `3` switches to RESP3 with `HELLO 3` |
| `connectTimeout` | `10s` | connect timeout |
| `timeout` | `30s` | reply timeout |
| `serializer` | text | `string`, `json` (values written as JSON, replies parsed), `bytes` (replies stay bytes) or a `#bean` |
| `redisTemplate`, `connectionFactory` | | `#bean` of type `RedisClient` to share a connection |
| `ssl` | `false` | TLS (or `sslContextParameters=#bean`, `useGlobalSslContextParameters=true`) |
| `sentinels` | | Redis Sentinel addresses (`host:port,...`); the master is resolved through them |
| `sentinelMaster` | `mymaster` | Sentinel master name (also `masterName`) |
| `cluster` | `false` | Redis Cluster: route by key slot and follow `MOVED` and `ASK` redirections |

`listenerContainer` is accepted and ignored. Java serialization (Camel's default
`JdkSerializationRedisSerializer`) does not exist here: the default writes text (bytes as is,
lists and maps as JSON) and returns text replies, or bytes when a reply is not UTF-8.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelRedis.Command` | in | command for this exchange |
| `CamelRedis.Key`, `CamelRedis.Keys` | in | key, or keys for `DEL`, `SUNION`, ... |
| `CamelRedis.Field`, `CamelRedis.Fields` | in | hash field(s) |
| `CamelRedis.Value`, `CamelRedis.Values` | in | value, or values (`MSET` takes a map) |
| `CamelRedis.Timeout` | in | seconds for `EXPIRE`, `SETEX`, blocking pops |
| `CamelRedis.Start`, `CamelRedis.End` | in | ranges for `LRANGE`, `ZRANGE`, `GETRANGE` |
| `CamelRedis.Index`, `CamelRedis.Offset`, `CamelRedis.Count` | in | positions and counts |
| `CamelRedis.Destination` | in | target key (`RPOPLPUSH`, `SMOVE`, ...) |
| `CamelRedis.Channel`, `CamelRedis.Message` | in | `PUBLISH` channel and message |
| `CamelRedis.Pivot`, `CamelRedis.Position` | in | `LINSERT` arguments |
| `CamelRedis.Pattern` | in | pattern for `KEYS` |
| `CamelRedis.Timestamp`, `CamelRedis.Db` | in | `EXPIREAT`, `MOVE` arguments |
| `CamelRedis.Score`, `CamelRedis.Min`, `CamelRedis.Max`, `CamelRedis.Increment`, `CamelRedis.WithScore` | in | sorted-set arguments |
| `CamelRedis.Longitude`, `CamelRedis.Latitude`, `CamelRedis.Radius` | in | geo arguments (radius in meters) |
| `CamelRedis.Channel` | out | consumer: channel of the message |
| `CamelRedis.Pattern` | out | consumer: matching pattern for `PSUBSCRIBE` |

Unknown commands are sent as they are, with the key and values as arguments.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e REDIS_HOST=redis.example.com \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/memory.camel.yaml
```

## Common questions

### How do I cache a value with an expiry?

Use `command=SETEX` with the headers `CamelRedis.Key`, `CamelRedis.Value` and
`CamelRedis.Timeout` (seconds). Read it back with `GET`; a missing key gives a null body.

### How do I store JSON objects?

Set `serializer=json`: maps and lists are written as JSON and replies are parsed back into maps
and lists.

### Does it work with Redis Cluster, Sentinel or managed Redis?

Cluster (`cluster=true`) and Sentinel (`sentinels=...`) are supported, with AUTH, ACL users and
TLS (`ssl=true`). Live tests run against Redis 7.0; other RESP servers are not tested.

## Related

- [Idempotent consumer EIP](../guides/eips.md): Cameleer also has a Redis idempotent repository.
- [Apache Camel Spring Redis component](https://camel.apache.org/components/next/spring-redis-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
