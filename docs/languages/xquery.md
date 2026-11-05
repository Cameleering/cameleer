# XQuery language

> The `xquery` language evaluates an XQuery expression (FLWOR, constructors, XPath) against an XML message and returns the result.

**Name:** `xquery` · **Crate:** `cameleer-xml` · **Apache Camel:** [XQuery](https://camel.apache.org/components/next/languages/xquery-language.html)

## When to use it

- Compute a value from XML that XPath 1.0 cannot express easily: `for ... order by ... return`,
  `string-join`, sums over computed values.
- Build a small XML fragment from an incoming document in a single expression.
- Filter or route XML messages with an XQuery predicate.

For a query kept in a file and run as a step, use the [xquery component](../components/xquery.md).

## Syntax

Cameleer has its own XQuery processor (Apache Camel uses Saxon). The body, or the input named
by `headerName`/`source`, is parsed as the context document.

External variables are bound like camel-saxon does:

| declaration | value |
|---|---|
| `declare variable $in.body external;` | the body |
| `declare variable $in.headers.NAME external;` | header `NAME` |
| `declare variable $NAME external;` | exchange property `NAME`, else header, else variable |

A declared atomic type casts the value (`as xs:integer`); a node type (`as element()`) parses
it as XML.

**Result.** By default (`resultType` `DOM`) the result is serialized as XML text, with atomic
values separated by spaces. Predicates use the effective boolean value.

### Syntax reference, tested

<!-- fixture: xquery-order -->
```json
{"body": "<order id=\"42\"><customer>Ada</customer><item sku=\"A1\" qty=\"2\" price=\"9.50\"/><item sku=\"B2\" qty=\"1\" price=\"20\"/></order>",
 "headers": {"region": "eu"}}
```

<!-- eval: xquery-syntax fixture=xquery-order language=xquery -->
| expression | result |
|---|---|
| `/order/customer/text()` | `Ada` |
| `for $i in //item return string($i/@sku)` | `A1 B2` |
| `string-join(//item/@sku, ',')` | `A1,B2` |
| `sum(for $i in //item return $i/@qty * $i/@price)` | `39` |
| `<skus>{for $i in //item order by $i/@sku descending return <sku>{string($i/@sku)}</sku>}</skus>` | `<skus><sku>B2</sku><sku>A1</sku></skus>` |
| `declare variable $in.headers.region external; concat(/order/customer, '@', $in.headers.region)` | `Ada@eu` |

<!-- eval: xquery-predicates fixture=xquery-order language=xquery predicate -->
| expression | result |
|---|---|
| `count(//item) > 1` | `true` |
| `some $i in //item satisfies $i/@price > 100` | `false` |

## Examples

### Build a summary with FLWOR

<!-- example: xquery-summary -->
```yaml
- route:
    id: xquery-summary
    from:
      uri: direct:xquery-summary
      steps:
        - setBody:
            xquery:
              expression: >-
                declare variable $in.headers.currency external;
                <summary customer="{/order/customer}" currency="{$in.headers.currency}">{
                  for $i in //item
                  let $line := $i/@qty * $i/@price
                  order by $line descending
                  return <line sku="{$i/@sku}">{$line}</line>
                }</summary>
```

<!-- input: xquery-summary {"headers":{"currency":"EUR"}} -->
```xml
<order><customer>Ada</customer><item sku="A1" qty="2" price="9.50"/><item sku="B2" qty="3" price="20"/></order>
```

<!-- output: xquery-summary -->
```xml
<summary customer="Ada" currency="EUR"><line sku="B2">60</line><line sku="A1">19</line></summary>
```

### Filter with an XQuery predicate

<!-- example: xquery-filter -->
```yaml
- route:
    id: xquery-filter
    from:
      uri: direct:xquery-filter
      steps:
        - setHeader:
            name: big
            constant: "no"
        - filter:
            xquery: "sum(//item/@qty) >= 10"
            steps:
              - setHeader:
                  name: big
                  constant: "yes"
        - setBody:
            xquery:
              expression: "sum(//item/@qty)"
              resultType: Integer
        - setBody:
            simple: "qty=${body} big=${header.big}"
```

<!-- input: xquery-filter -->
```xml
<order><item qty="4"/><item qty="7"/></order>
```

<!-- output: xquery-filter -->
```text
qty=11 big=yes
```

## Options

| option | default | description |
|---|---|---|
| `resultType` | `DOM` | `DOM` (XML text), `String` (string values concatenated), `List`/`NodeList` (array), `Node`/`Element` (first item), `Bytes`, `Integer`/`Long`, `Double`, `Boolean`. Java class names are accepted. |
| `namespace` / `namespaces` | (none) | Prefix bindings, same syntax as the [xpath language](xpath.md). |
| `stripsAllWhiteSpace` | `true` | Drop whitespace-only text nodes of the input, as Saxon does for Camel. |
| `headerName` | (none) | Evaluate this header instead of the body. |
| `source` | (none) | `header:x`, `property:x`, `variable:x` or `body`. |

## Rust DSL

```rust
b.from("direct:xquery-rust")
    .set_body(cameleer_xml::xquery_component::xquery_expr("string-join(//item/@sku, ',')"));
```

## Common questions

### How do I pass a header into an XQuery?

Declare it: `declare variable $in.headers.currency external;` and use `$in.headers.currency`.

### What is the difference between the xquery language and the xquery component?

The language evaluates an inline expression wherever an expression fits (`setBody`,
`filter`, `split`...). The component runs a query file as a producer step:
`to: xquery:file:summary.xq`.

### Which XQuery version is supported?

The commonly used XQuery 1.0 features (FLWOR, constructors, quantified expressions,
external variables) with XPath 2.0 functions; there is no Saxon, schema awareness or XQuery
Update.

## Related

- [xquery component](../components/xquery.md), [XPath language](xpath.md),
  [xslt component](../components/xslt.md)
- [Apache Camel XQuery language](https://camel.apache.org/components/next/languages/xquery-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
