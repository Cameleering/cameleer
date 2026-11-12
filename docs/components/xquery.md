# XQuery component

> The `xquery` component runs an XQuery file against the XML message body and replaces the body with the query result.

**Scheme:** `xquery:` · **Crate:** `cameleer-xml` · **Cargo feature:** [`xml`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [XQuery](https://camel.apache.org/components/next/xquery-component.html)

## When to use it

- Query and reshape XML with FLWOR expressions (`for`, `let`, `where`, `order by`, `return`).
- Build a new XML document from parts of a large one, for example only the overdue invoices.
- Extract a short text answer from an XML API response before handing it to an agent.

For a single expression inside a route step, use the [XQuery language](../languages/xquery.md).

## URI format

```text
xquery:queryResource[?options]
```

`queryResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName`.

## Example

Select overdue invoices, sort them by amount and build a new document. The `days` header is
bound to an external variable.

<!-- file: xq/overdue.xq -->
```text
declare variable $in.headers.days as xs:integer external;
<overdue days="{$in.headers.days}">{
  for $i in /invoices/invoice
  where xs:integer($i/@age) gt $in.headers.days
  order by xs:decimal($i/amount) descending
  return <invoice id="{$i/@id}">{data($i/amount)}</invoice>
}</overdue>
```

<!-- example: xquery-overdue-invoices -->
```yaml
- route:
    id: xquery-overdue
    from:
      uri: direct:xquery-overdue
      steps:
        - setHeader:
            name: days
            constant: "30"
        - to: xquery:xq/overdue.xq
```

<!-- input: xquery-overdue-invoices -->
```xml
<invoices>
  <invoice id="1" age="10"><amount>100</amount></invoice>
  <invoice id="2" age="45"><amount>75.5</amount></invoice>
  <invoice id="3" age="90"><amount>300</amount></invoice>
</invoices>
```

<!-- output: xquery-overdue-invoices -->
```xml
<overdue days="30"><invoice id="3">300</invoice><invoice id="2">75.5</invoice></overdue>
```

### Text results

`resultsFormat=String` concatenates the string values of the result:

<!-- file: xq/names.xq -->
```text
string-join(for $p in //person order by $p/@name return string($p/@name), ', ')
```

<!-- example: xquery-string-result -->
```yaml
- route:
    id: xquery-names
    from:
      uri: direct:xquery-names
      steps:
        - to: xquery:xq/names.xq?resultsFormat=String
```

<!-- input: xquery-string-result -->
```xml
<team><person name="zoe"/><person name="ali"/><person name="max"/></team>
```

<!-- output: xquery-string-result -->
```text
ali, max, zoe
```

## Rust DSL

```rust
b.from("direct:xquery-rust")
    .to("xquery:xq/names.xq?resultsFormat=String");
```

## Options

| option | default | description |
|---|---|---|
| `resultsFormat` | `DOM` | `DOM` (the result serialized as XML text), `String`, `Bytes` or `List`. `BytesSource`, `StringSource` and `DOMSource` map to `Bytes`, `String` and `DOM`. |
| `stripsAllWhiteSpace` | `true` | Drop whitespace-only text nodes of the input, as Saxon does for Camel. |
| `contentCache` | `true` | Compile the query once. |
| `headerName` | (none) | Read the input document from this header instead of the body. |
| `source` | (none) | Input source, as in Camel (`header:name`, `variable:name`, `property:name`). |
| `namespacePrefixes` | (none) | Prefix bindings, for example `p=urn:p`. |

Accepted and ignored (Saxon settings): `configuration`, `configurationProperties`,
`staticQueryContext`, `moduleURIResolver`, `parameters`, `properties`, `allowStAX`,
`resultType`, `lazyStartProducer`. Relative `doc()` URIs resolve against the query file's
directory.

### External variables

As in camel-saxon: `declare variable $in.body external;` is the body,
`$in.headers.NAME` a header, and `$NAME` an exchange property, then a header, then a variable.
A declared atomic type casts the value (`as xs:integer`); a node type (`as element()`) parses
it as XML.

## Message headers

| header | direction | description |
|---|---|---|
| any header | in | Available as `$in.headers.NAME` (or `$NAME`) when declared `external`. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/overdue.camel.yaml
```

## Common questions

### How do I pass a parameter to the query?

Set a header and declare it in the query: `declare variable $in.headers.days as xs:integer external;`.

### How do I get plain text instead of XML?

Add `resultsFormat=String`.

### Which XQuery version is supported?

XQuery 1.0 plus common 3.x conveniences such as `string-join`. Schema-aware features and
Saxon extensions are not available.

## Related

- [XQuery language](../languages/xquery.md), [XPath language](../languages/xpath.md).
- [xslt](xslt.md), [xj](xj.md).
- [Apache Camel XQuery component](https://camel.apache.org/components/next/xquery-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
