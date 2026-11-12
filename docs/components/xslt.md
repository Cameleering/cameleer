# XSLT component

> The `xslt` component transforms the XML message body with an XSLT stylesheet and sets the result (XML, HTML or text) as the new body.

**Scheme:** `xslt:` · **Crate:** `cameleer-xml` · **Cargo feature:** [`xml`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [XSLT](https://camel.apache.org/components/next/xslt-component.html)

## When to use it

- Turn legacy XML (SOAP payloads, EDI exports, partner feeds) into the JSON an agent tool or a
  REST API expects, with `xsl:output method="text"`.
- Map one XML vocabulary to another (order formats, invoice standards).
- Render XML to HTML reports.
- Reuse XSLT 1.0 stylesheets from an existing Camel or BizTalk-style integration.

## URI format

```text
xslt:templateResource[?options]
```

`templateResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path, there is no classpath) or `ref:beanName` (a
`String` in the registry). `xsl:include`, `xsl:import` and `document()` resolve relative to the
stylesheet file.

## Example

A partner sends orders as XML; an agent tool wants compact JSON. The stylesheet writes JSON
text, and the `currency` header arrives as the `xsl:param` of the same name.

<!-- file: xsl/order-to-json.xsl -->
```xml
<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="text"/>
  <xsl:param name="currency" select="'USD'"/>
  <xsl:template match="/order">
    <xsl:text>{"id":"</xsl:text><xsl:value-of select="@id"/>
    <xsl:text>","customer":"</xsl:text><xsl:value-of select="customer/name"/>
    <xsl:text>","currency":"</xsl:text><xsl:value-of select="$currency"/>
    <xsl:text>","lines":[</xsl:text>
    <xsl:for-each select="line">
      <xsl:sort select="@sku"/>
      <xsl:text>{"sku":"</xsl:text><xsl:value-of select="@sku"/>
      <xsl:text>","qty":</xsl:text><xsl:value-of select="@qty"/>
      <xsl:text>}</xsl:text>
      <xsl:if test="position() != last()">,</xsl:if>
    </xsl:for-each>
    <xsl:text>],"total":</xsl:text>
    <xsl:value-of select="format-number(sum(line/@price), '0.00')"/>
    <xsl:text>}</xsl:text>
  </xsl:template>
</xsl:stylesheet>
```

<!-- example: xslt-order-to-json -->
```yaml
- route:
    id: xslt-order-to-json
    from:
      uri: direct:xslt-order
      steps:
        - setHeader:
            name: currency
            constant: EUR
        - to: xslt:xsl/order-to-json.xsl
```

<!-- input: xslt-order-to-json -->
```xml
<order id="A-17">
  <customer><name>Ada Lovelace</name></customer>
  <line sku="Z9" qty="1" price="12.5"/>
  <line sku="B2" qty="3" price="30"/>
</order>
```

<!-- output: xslt-order-to-json -->
```json
{"id":"A-17","customer":"Ada Lovelace","currency":"EUR","lines":[{"sku":"B2","qty":3},{"sku":"Z9","qty":1}],"total":42.50}
```

### XML to XML

<!-- file: xsl/rename.xsl -->
```xml
<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform">
  <xsl:output method="xml" omit-xml-declaration="yes"/>
  <xsl:template match="/person">
    <contact name="{first} {last}"><xsl:copy-of select="email"/></contact>
  </xsl:template>
</xsl:stylesheet>
```

<!-- example: xslt-xml-to-xml -->
```yaml
- route:
    id: xslt-rename
    from:
      uri: direct:xslt-rename
      steps:
        - to: xslt:xsl/rename.xsl
```

<!-- input: xslt-xml-to-xml -->
```xml
<person><first>Grace</first><last>Hopper</last><email>grace@example.com</email></person>
```

<!-- output: xslt-xml-to-xml -->
```xml
<contact name="Grace Hopper"><email>grace@example.com</email></contact>
```

## Rust DSL

```rust
b.from("direct:xslt-rust")
    .set_header("currency", constant("GBP"))
    .to("xslt:xsl/order-to-json.xsl");
```

## Options

| option | default | description |
|---|---|---|
| `output` | `string` | `string`, `bytes`, `DOM` (serialized XML text, as there is no DOM type) or `file` (written to the file named by the `CamelXsltFileName` header; the body becomes the file path). |
| `contentCache` | `true` | Compile the stylesheet once. Set `false` to re-read it for every message. |
| `failOnNullBody` | `true` | Fail when the body is null. |
| `deleteOutputFile` | `false` | With `output=file`, delete the file when the exchange completes. |
| `allowTemplateFromHeader` | `false` | Use the `CamelXsltStylesheet` / `CamelXsltResourceUri` headers when present. |

Accepted and ignored (Java-only settings): `transformerFactory`, `transformerFactoryClass`,
`transformerFactoryConfigurationStrategy`, `uriResolver`, `uriResolverFactory`,
`resultHandlerFactory`, `entityResolver`, `errorListener`, `xsltMessageLogger`,
`transformerCacheSize`, `allowStAX`, `lazyStartProducer`.

**Parameters:** exchange properties, then message headers, are passed as global `xsl:param`
values, as in Camel.

### What the processor supports

A complete XSLT 1.0 processor: templates with match patterns, priorities and modes,
`apply-templates`, `call-template`, parameters, variables, `for-each`, `choose`, `sort`,
`copy`/`copy-of`, `element`/`attribute`, `number`, `key()`, `format-number()`, `document()`,
`generate-id()`, `exsl:node-set()`, and `xsl:output` with the xml, html and text methods.
Stylesheets with `version="2.0"` get XPath 2.0 and the XSLT 2.0 core; see
[xslt-saxon](xslt-saxon.md).

## Message headers

| header | direction | description |
|---|---|---|
| `CamelXsltFileName` | in | Output file for `output=file`. |
| `CamelXsltStylesheet` | in | Stylesheet text used instead of the resource (needs `allowTemplateFromHeader=true`). |
| `CamelXsltResourceUri` | in | Stylesheet resource used instead of the endpoint's one (needs `allowTemplateFromHeader=true`). |
| any header | in | Available to the stylesheet as an `xsl:param` with the header's name. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/order-to-json.camel.yaml
```

Relative stylesheet paths resolve against `/routes`, the image's working directory.

## Common questions

### How do I pass a value into the stylesheet?

Declare `<xsl:param name="x"/>` at the top level and set a header (or exchange property)
named `x` before the `xslt` step.

### How do I produce JSON with XSLT?

Use `<xsl:output method="text"/>` and write the JSON with `xsl:text` and `xsl:value-of`, as in
the example. For a generic XML-to-JSON mapping without a stylesheet, use [xj](xj.md).

### Is XSLT 2.0 or 3.0 supported?

XSLT 1.0 fully; `version="2.0"` stylesheets get the XSLT 2.0 core (grouping, functions, regex).
XSLT 3.0-only instructions such as `xsl:iterate` and `xsl:try` fail with a
`TransformerConfigurationException` naming the instruction.

## Related

- [xslt-saxon](xslt-saxon.md): the same processor under Camel's Saxon scheme.
- [xj](xj.md), [xquery](xquery.md), [validator](validator.md).
- [XPath language](../languages/xpath.md).
- [Apache Camel XSLT component](https://camel.apache.org/components/next/xslt-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
