# XPath language

> The `xpath` language selects values from an XML message with XPath 1.0 expressions such as `/order/@id` or `count(//item)`.

**Name:** `xpath` · **Crate:** `cameleer-xml` (a smaller subset in `cameleer-core`) · **Apache Camel:** [XPath](https://camel.apache.org/components/next/languages/xpath-language.html)

## When to use it

- Read a field from an XML document: an id attribute, a customer name, a status.
- Route or filter XML messages (`/order/@status = 'new'`).
- Split an XML document into one message per element (`//item`).
- Pull values out of SOAP responses or legacy XML feeds before handing them to a JSON API or
  an AI agent tool.

## Syntax

With `cameleer_main::new_main()` (and in the container image) the `xpath` language is the full
XPath 1.0 engine of `cameleer-xml`. It replaces the subset in `cameleer-core`, which ignores
namespaces and is only used when `cameleer-xml` is not registered.

Supported: location paths with all XPath 1.0 axes, predicates, unions `|`, `and`/`or`,
comparisons, arithmetic (`+ - * div mod`), the XPath 1.0 function library (`count`, `sum`,
`concat`, `contains`, `starts-with`, `substring-before`, `translate`, `normalize-space`,
`string-length`, `number`, `round`, `name`, `local-name`, ...) plus `upper-case`,
`lower-case`, `ends-with` and `matches`.

Camel functions and variables:

| syntax | value |
|---|---|
| `$name` | header `name`, else exchange property, else variable |
| `in:body()`, `in:header('name')` | the body, a header |
| `function:properties('key')` | a property placeholder value |
| `function:simple('${...}')` | a [Simple](simple.md) expression |
| `$env:NAME`, `$system:NAME` | an environment variable |
| `$exchangeProperty:NAME` | an exchange property |

**Namespaces.** Name tests match namespace URIs, so a prefixed document needs a `namespace`
binding (`p=uri`) or `local-name()` tests.

**Result.** Without `resultType`, a node-set with one node becomes a string (elements are
serialized as XML, attributes and text as their value), a larger node-set becomes an array,
and numbers, strings and booleans keep their type (XPath numbers are doubles, so `count(...)`
is `2.0`). Use `resultType: String` for the trimmed string value.

### Syntax reference, tested

<!-- fixture: xpath-order -->
```json
{"body": "<order id=\"42\" status=\"new\"><customer>Ada</customer><item sku=\"A1\" qty=\"2\" price=\"9.50\"/><item sku=\"B2\" qty=\"1\" price=\"20\"/></order>",
 "headers": {"region": "eu"}}
```

<!-- eval: xpath-syntax fixture=xpath-order language=xpath -->
| expression | result |
|---|---|
| `/order/@id` | `42` |
| `/order/customer/text()` | `Ada` |
| `/order/customer` | `<customer>Ada</customer>` |
| `//item/@sku` | `["A1","B2"]` |
| `//item[1]` | `<item sku="A1" qty="2" price="9.50"/>` |
| `//item[last()]/@sku` | `B2` |
| `//item[@price > 10]/@sku` | `B2` |
| `//item/following-sibling::item/@sku` | `B2` |
| `count(//item)` | `2.0` |
| `sum(//item/@qty)` | `3.0` |
| `//item[@sku='A1']/@qty * //item[@sku='A1']/@price` | `19.0` |
| `concat(/order/customer, '-', /order/@id)` | `Ada-42` |
| `translate(/order/customer, 'a', 'A')` | `AdA` |
| `upper-case(/order/customer)` | `ADA` |
| `name(/*)` | `order` |
| `$region` | `eu` |
| `in:header('region')` | `eu` |
| `function:simple('${header.region}')` | `eu` |

<!-- eval: xpath-predicates fixture=xpath-order language=xpath predicate -->
| expression | result |
|---|---|
| `/order/@status = 'new'` | `true` |
| `count(//item) > 2` | `false` |
| `//item[@sku='B2']` | `true` |
| `//refund` | `false` |

## Examples

### Route XML orders by status

<!-- example: xpath-route-by-status -->
```yaml
- route:
    id: xpath-route
    from:
      uri: direct:xpath-route
      steps:
        - setHeader:
            name: orderId
            xpath:
              expression: /order/@id
              resultType: String
        - choice:
            when:
              - xpath: "/order/@status = 'new'"
                steps:
                  - setBody:
                      simple: "new order ${header.orderId}"
            otherwise:
              steps:
                - setBody:
                    simple: "ignored order ${header.orderId}"
```

<!-- input: xpath-route-by-status -->
```xml
<order id="42" status="new"><customer>Ada</customer></order>
```

<!-- output: xpath-route-by-status -->
```text
new order 42
```

<!-- input: xpath-route-by-status#2 -->
```xml
<order id="43" status="shipped"><customer>Bob</customer></order>
```

<!-- output: xpath-route-by-status#2 -->
```text
ignored order 43
```

### Namespaces

<!-- example: xpath-namespaces -->
```yaml
- route:
    id: xpath-ns
    from:
      uri: direct:xpath-ns
      steps:
        - setBody:
            xpath:
              expression: /o:order/o:total
              namespace: o=urn:example:orders
              resultType: Double
```

<!-- input: xpath-namespaces -->
```xml
<order xmlns="urn:example:orders"><total>12.5</total></order>
```

<!-- output: xpath-namespaces -->
```text
12.5
```

### Split an XML document into items

<!-- example: xpath-split-items -->
```yaml
- route:
    id: xpath-split
    from:
      uri: direct:xpath-split
      steps:
        - split:
            xpath: //item
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - setBody:
                  xpath:
                    expression: /item/@sku
                    resultType: String
```

<!-- input: xpath-split-items -->
```xml
<order><item sku="A1"/><item sku="B2"/><item sku="C3"/></order>
```

<!-- output: xpath-split-items -->
```text
["A1","B2","C3"]
```

## Options

| option | default | description |
|---|---|---|
| `resultType` | (none) | `String` (trimmed string value), `NodeList`/`nodeset` (array), `Node` (first node), `Number`/`Double`, `Integer`/`Long`, `Boolean`. Java class names and `XPathConstants` names are accepted. |
| `namespace` / `namespaces` | (none) | Prefix bindings: `p=uri,q=uri2` (also `;` or newline separated), a JSON object, or a list of `{key, value}`. Options named `namespace.<prefix>` also bind a prefix. |
| `headerName` | (none) | Evaluate this header instead of the body. |
| `propertyName`, `variableName` | (none) | Evaluate this exchange property or variable. |
| `source` | (none) | `header:x`, `exchangeProperty:x`, `variable:x` or `body`. |
| `trim` | `true` | Trim the `String` result. |
| `saxon` | `false` | `true` fails: there is no Saxon or XPath 2.0 engine. |
| `threadSafety`, `preCompile`, `logNamespaces`, `documentType`, `factoryRef`, `objectModel` | | Accepted for compatibility, no effect. |

## Rust DSL

```rust
b.from("direct:xpath-rust")
    .filter(xpath("/order/@status = 'new'"))
    .set_body(xpath_as("/order/@id", "String"));
```

With namespaces, use `cameleer_xml::language::xpath_ns("/c:order/@id", &[("c", "urn:orders")])`.

## Common questions

### Why does my XPath return nothing for a document with a default namespace?

Name tests match the namespace URI. Bind a prefix (`namespace: o=urn:...`) and use it in the
expression, or match with `*[local-name()='order']`.

### Why is `count(//item)` `2.0` and not `2`?

XPath numbers are doubles. Use `resultType: Integer` to get `2`.

### Does Cameleer support XPath 2.0 or 3.1?

No. The `xpath` language is XPath 1.0 with a few 2.0 string functions (`upper-case`,
`lower-case`, `ends-with`, `matches`). For FLWOR and sequences use the
[XQuery language](xquery.md).

## Related

- [XQuery language](xquery.md), [xslt component](../components/xslt.md),
  [xj component](../components/xj.md), [validator component](../components/validator.md)
- [tokenize language](tokenize.md) (`xml: true` streams large XML files element by element)
- [JacksonXML data format](../dataformats/jacksonxml.md)
- [Apache Camel XPath language](https://camel.apache.org/components/next/languages/xpath-language.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
