# Message transformation patterns

> Transformation patterns change what a message contains: its body and headers (message
> translator), its format (marshal and unmarshal), extra data fetched from elsewhere (content
> enricher), or a large payload parked aside (claim check).

Every example on this page runs in the test suite as YAML and as Rust DSL. See
[the EIP guide](../eips.md) for the other patterns.

## Message translator: `setBody`, `transform`, `setHeader`

`setBody` (or its synonym `transform`) replaces the body with the value of an expression;
`setHeader`, `setProperty` and `setVariable` set one value; `removeHeader` and `removeHeaders`
(with a wildcard pattern) delete headers.

<!-- example: eip-message-translator -->
```yaml
- route:
    from:
      uri: direct:greet
      steps:
        - setHeader:
            name: customer
            simple: "${body[name]}"
        - transform:
            simple: "Hello ${header.customer}, your order ${body[order]} has shipped."
        - removeHeaders:
            pattern: "internal*"
        - to: mock:greeting
```

```rust
b.from("direct:greet")
    .set_header("customer", simple("${body[name]}"))
    .transform(simple("Hello ${header.customer}, your order ${body[order]} has shipped."))
    .remove_headers("internal*")
    .to("mock:greeting");
```

The body `{"name": "Ana", "order": 17}` (a map) becomes
`Hello Ana, your order 17 has shipped.`, and headers such as `internalTrace` are removed.

## Marshal and unmarshal

`unmarshal` turns bytes or text into structured data with a data format; `marshal` does the
reverse. JSON maps to Cameleer's dynamic `Value` (maps and lists), so Simple can read fields
with `${body[field]}`. Other formats include CSV, XML, YAML, Avro, Protobuf, CBOR, Base64,
GZip and Zip.

<!-- example: eip-marshal-json -->
```yaml
- route:
    from:
      uri: direct:order-json
      steps:
        - unmarshal:
            json: {}
        - setBody:
            simple: '{"order": "${body[id]}", "total": ${body[qty]}}'
        - unmarshal:
            json: {}
        - marshal:
            json: {}
        - to: mock:order-out
```

```rust
b.from("direct:order-json")
    .unmarshal_json()
    .set_body(simple(r#"{"order": "${body[id]}", "total": ${body[qty]}}"#))
    .unmarshal_json()
    .marshal_json()
    .to("mock:order-out");
```

The text `{"id":"A-1","qty":3}` comes out as the JSON text `{"order":"A-1","total":3}`. There
is no POJO binding: data stays a `Value`, which is what Camel users get with
`unmarshal: json: {}` and no `unmarshalType`.

## Convert the body type

`convertBodyTo` converts the body with the type converter registry: `java.lang.String`,
`java.lang.Integer`, `byte[]`, `java.util.Map` and the other types Camel's core converters
know.

<!-- example: eip-convert-body -->
```yaml
- route:
    from:
      uri: direct:reading
      steps:
        - convertBodyTo:
            type: java.lang.Integer
        - filter:
            simple: "${body} > 100"
            steps:
              - to: mock:too-hot
```

```rust
b.from("direct:reading")
    .convert_body_to("java.lang.Integer")
    .filter(simple("${body} > 100"))
        .to("mock:too-hot")
    .end();
```

The text ` 250 ` (or the bytes of an MQTT payload) is converted to the integer `250`, so the
comparison is numeric. With `mandatory: true`, a value that cannot be converted fails the
exchange with `NoTypeConversionAvailableException`, as in Camel.

## Content enricher: `enrich`

`enrich` calls another endpoint with a copy of the message and merges the reply into the
message. Without an aggregation strategy the reply replaces the body. Use it to look up a
customer, a price or a document before calling a model.

<!-- example: eip-enrich -->
```yaml
- route:
    from:
      uri: direct:ticket
      steps:
        - setProperty:
            name: ticket
            simple: "${body}"
        - enrich:
            uri: direct:crm-lookup
        - setBody:
            simple: "${exchangeProperty.ticket} (customer tier: ${body})"
        - to: mock:enriched
- route:
    from:
      uri: direct:crm-lookup
      steps:
        - choice:
            when:
              - simple: "${header.customerId} == 'c-1'"
                steps:
                  - setBody:
                      constant: gold
            otherwise:
              steps:
                - setBody:
                    constant: standard
```

```rust
b.from("direct:ticket")
    .set_property("ticket", simple("${body}"))
    .enrich("direct:crm-lookup")
    .set_body(simple("${exchangeProperty.ticket} (customer tier: ${body})"))
    .to("mock:enriched");
b.from("direct:crm-lookup")
    .choice()
        .when(simple("${header.customerId} == 'c-1'"))
            .set_body(constant("gold"))
        .otherwise()
            .set_body(constant("standard"))
    .end();
```

The ticket `Printer on fire` from customer `c-1` reaches `mock:enriched` as
`Printer on fire (customer tier: gold)`. Options: `aggregationStrategy`,
`aggregateOnException`, `ignoreInvalidEndpoint`, `shareUnitOfWork`, and a dynamic URI with
`${...}`.

## Poll enrich: `pollEnrich`

`pollEnrich` consumes one message from a polling endpoint (a seda queue, a file directory, an
FTP folder) and puts it into the current message. `timeout` is in milliseconds: `-1` waits
forever, `0` does not wait.

<!-- example: eip-poll-enrich -->
```yaml
- route:
    from:
      uri: direct:with-latest-config
      steps:
        - pollEnrich:
            uri: seda:config-updates
            timeout: 1000
        - to: mock:with-config
```

```rust
b.from("direct:with-latest-config")
    .poll_enrich("seda:config-updates", 1000)
    .to("mock:with-config");
```

When a message `threshold=40` is waiting on `seda:config-updates`, a request to
`direct:with-latest-config` continues with that body.

## Claim check

A claim check stores the body (or selected headers) under a key and gets it back later, so the
steps in between work with a small message. Operations: `Set`, `Get`, `GetAndRemove`, `Push`,
`Pop`.

<!-- example: eip-claim-check -->
```yaml
- route:
    from:
      uri: direct:big-document
      steps:
        - claimCheck:
            operation: Set
            key: document
        - setBody:
            constant: "summary request"
        - to: mock:light
        - claimCheck:
            operation: Get
            key: document
        - to: mock:restored
```

```rust
b.from("direct:big-document")
    .claim_check(ClaimCheckOperation::Set, "document")
    .set_body(constant("summary request"))
    .to("mock:light")
    .claim_check(ClaimCheckOperation::Get, "document")
    .to("mock:restored");
```

`mock:light` sees only `summary request`; `mock:restored` gets the original document back.

## Script

`script` runs an expression or a script for its side effects; its result is discarded. With
the script languages of `cameleer-script` (Groovy, JavaScript, Python, MVEL, Java), changes the
script makes to the message, headers and properties are kept.

<!-- example: eip-script -->
```yaml
- route:
    from:
      uri: direct:tags
      steps:
        - script:
            groovy: |
              def tags = request.body.split(',')
              request.headers.tagCount = tags.size()
              request.body = tags.collect { it.trim().toLowerCase() }.join('|')
        - to: mock:tags
```

```rust
b.from("direct:tags")
    .process(cameleer_script::script("groovy", "def tags = request.body.split(',')\nrequest.headers.tagCount = tags.size()\nrequest.body = tags.collect { it.trim().toLowerCase() }.join('|')").unwrap())
    .to("mock:tags");
```

The body `Rust, Camel ,EIP` becomes `rust|camel|eip` with the header `tagCount` set to 3. The
scripts run in Cameleer's own interpreters, not on a JVM, so Java libraries are not available
to them.

## Common questions

### How do I change the message body in a route?

Use `setBody` (or `transform`) with an expression: `simple` for text templates, `jsonpath` or
`jq` to pick from JSON, `constant` for a fixed value.

### How do I parse JSON in a route?

Add `unmarshal: json: {}`. The body becomes a map or list, and `${body[field]}`,
`${body[items][0]}` and JSONPath read from it. `marshal: json: {}` turns it back into text.

### How do I call another service and keep my original message?

Store the original in a property (`setProperty`), call the service with `enrich` or `to`, and
build the result from `${exchangeProperty.name}` and `${body}`. An aggregation strategy bound in
the registry can do the merge instead.

## Related

- [All EIPs](../eips.md), [routing](routing.md), [splitting and aggregating](split-aggregate.md)
- Apache Camel: [Message Translator](https://camel.apache.org/components/next/eips/message-translator.html),
  [Content Enricher](https://camel.apache.org/components/next/eips/content-enricher.html),
  [Claim Check](https://camel.apache.org/components/next/eips/claimCheck-eip.html)
