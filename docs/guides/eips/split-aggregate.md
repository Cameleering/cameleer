# Splitting and aggregating messages

> The splitter breaks one message into many, the aggregator combines many messages into one,
> and the resequencer and sort EIPs put messages and lists back in order.

These are the patterns for batch files, JSON arrays, order lines, sensor readings and document
chunks. Every example runs in the test suite as YAML and as Rust DSL. See
[the EIP guide](../eips.md) for the other patterns.

## Splitter

`split` evaluates an expression that returns a list (a JSON array, lines of text, XML nodes)
and runs its steps once per item. Each part has the exchange properties `CamelSplitIndex`,
`CamelSplitSize` and `CamelSplitComplete`.

<!-- example: eip-split-json -->
```yaml
- route:
    from:
      uri: direct:order
      steps:
        - split:
            jsonpath: "$.items"
            steps:
              - setHeader:
                  name: sku
                  simple: "${body[sku]}"
              - to: mock:order-line
```

```rust
b.from("direct:order")
    .split(jsonpath("$.items"))
        .set_header("sku", simple("${body[sku]}"))
        .to("mock:order-line")
    .end();
```

The order `{"items":[{"sku":"A1"},{"sku":"B2"}]}` gives two messages on `mock:order-line`,
with `sku` set to `A1` and `B2`.

Options: `parallelProcessing`, `streaming` (split a large text body lazily, one part at a time),
`stopOnException`, `timeout`, `shareUnitOfWork`, `delimiter` and `aggregationStrategy`. Without
an aggregation strategy the original message continues after the split, as in Camel.

### Split and aggregate the results

With an aggregation strategy, the split combines the outputs of its parts, and the route
continues with the combined message. This is the "composed message processor": split a request,
handle each part, and put the answers back together.

<!-- example: eip-split-aggregate -->
```yaml
- route:
    from:
      uri: direct:lines
      steps:
        - split:
            tokenize: "\n"
            aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
            steps:
              - setBody:
                  simple: "${body.toUpperCase()}"
        - to: mock:upper-lines
```

```rust
b.from("direct:lines")
    .split(tokenize("\n"))
        .aggregation_strategy(strategies::grouped_body())
        .set_body(simple("${body.toUpperCase()}"))
    .end()
    .to("mock:upper-lines");
```

The body `a\nb\nc` reaches `mock:upper-lines` as the list `["A", "B", "C"]`. Built-in
strategies by name: `GroupedBodyAggregationStrategy`, `GroupedExchangeAggregationStrategy`,
`UseLatestAggregationStrategy`, `UseOriginalAggregationStrategy` and
`StringAggregationStrategy`, written as `#class:` references like in Camel or bound in the
registry under a name.

## Aggregator

`aggregate` collects messages with the same correlation key and releases them as one message
when a completion condition is met: `completionSize`, `completionTimeout` (an inactivity
timeout), `completionInterval`, `completionPredicate`, or
`completionFromBatchConsumer`. The exchange property `CamelAggregatedCompletedBy` tells which condition
fired (`size`, `timeout`, `predicate`, `interval`, `force`...).

<!-- example: eip-aggregate -->
```yaml
- route:
    from:
      uri: direct:readings
      steps:
        - aggregate:
            correlationExpression:
              simple: "${header.device}"
            aggregationStrategy: "#class:org.apache.camel.processor.aggregate.GroupedBodyAggregationStrategy"
            completionSize: 3
            completionTimeout: 5000
            steps:
              - to: mock:device-batch
```

```rust
b.from("direct:readings")
    .aggregate(header("device"), strategies::grouped_body())
        .completion_size(3)
        .completion_timeout(Duration::from_millis(5000))
        .to("mock:device-batch")
    .end();
```

Three readings from device `t1` are released together as `["20", "21", "22"]`; readings from
other devices are kept in their own groups. A group that stops receiving messages is released
after five seconds, so a quiet device still sends what it has.

Notes for Camel users: aggregation repositories are in memory by default; JDBC and Redis
repositories are available in `cameleer-sql` and `cameleer-datastores`, there is no LevelDB
repository. `forceCompletionOnStop`, `discardOnCompletionTimeout`,
`closeCorrelationKeyOnCompletion`, `eagerCheckCompletion` and
`completionOnNewCorrelationGroup` work as in Camel.

## Resequencer

The resequencer puts messages back in order by an expression, usually a sequence number. The
batch mode collects messages for `batchTimeout` milliseconds or `batchSize` messages, sorts
them and sends them on; the stream mode releases each message as soon as the gap before it is
filled.

<!-- example: eip-resequence -->
```yaml
- route:
    from:
      uri: direct:out-of-order
      steps:
        - resequence:
            simple: "${header.seq}"
            batchConfig:
              batchSize: 3
              batchTimeout: 2000
            steps:
              - to: mock:in-order
```

```rust
b.from("direct:out-of-order")
    .resequence(header("seq"))
        .batch()
        .batch_size(3)
        .batch_timeout(Duration::from_millis(2000))
        .to("mock:in-order")
    .end();
```

Messages with `seq` 3, 1 and 2 leave in the order 1, 2, 3. The stream mode is set with
`streamConfig` (`capacity`, `timeout`, `rejectOld`).

## Sort

`sort` sorts the list an expression returns and replaces the body with the sorted list. It
works on the body of one message, unlike the resequencer, which orders several messages.

<!-- example: eip-sort -->
```yaml
- route:
    from:
      uri: direct:names
      steps:
        - sort:
            tokenize: ","
        - to: mock:sorted
```

```rust
b.from("direct:names")
    .sort(tokenize(","))
    .to("mock:sorted");
```

The body `pear,apple,fig` becomes `["apple", "fig", "pear"]`. A custom comparator can be bound
in the registry and named with `comparator`.

## Common questions

### How do I split a JSON array into separate messages?

Use `split` with `jsonpath: "$.items"` (or `simple: "${body}"` when the body already is a
list). Each item becomes the body of its own message.

### How do I split a large file without loading it all?

Set `streaming: true` on the split with a `tokenize` expression. The text is split one part at
a time and each part's exchange is created when it is needed.

### How do I batch messages, for example 100 at a time or every 10 seconds?

Use `aggregate` with a constant correlation expression (`constant: "all"`), the
`GroupedBodyAggregationStrategy`, `completionSize: 100` and `completionInterval: 10000`.

### Why does my split not return the processed parts?

Without an `aggregationStrategy` the original message continues after the split, as in Apache
Camel. Add a strategy such as `GroupedBodyAggregationStrategy` to get the parts back.

## Related

- [All EIPs](../eips.md), [routing patterns](routing.md), [transformation](transformation.md)
- [Edge cookbook: aggregate sensor readings](../edge.md#filter-and-aggregate-mqtt-sensor-readings)
- [Agents cookbook: RAG ingestion](../agents.md#rag-ingestion-chunk-embed-and-store)
- Apache Camel: [Split](https://camel.apache.org/components/next/eips/split-eip.html),
  [Aggregate](https://camel.apache.org/components/next/eips/aggregate-eip.html),
  [Resequence](https://camel.apache.org/components/next/eips/resequence-eip.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
