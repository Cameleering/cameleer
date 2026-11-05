# XJ component

> The `xj` component converts XML to JSON and JSON to XML, either directly (`xj:identity`) or through an XSLT stylesheet.

**Scheme:** `xj:` · **Crate:** `cameleer-xml` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [XJ](https://camel.apache.org/components/next/xj-component.html)

## When to use it

- Expose a legacy XML or SOAP backend to an AI agent or a JSON API: convert the XML answer to
  JSON the model's tool-calling layer can read.
- Accept JSON from a web client and turn it into XML for an older system.
- Combine conversion and mapping in one step by naming an XSLT stylesheet.

## URI format

```text
xj:identity?transformDirection=XML2JSON|JSON2XML[&options]
xj:stylesheetResource?transformDirection=XML2JSON|JSON2XML[&options]
```

`identity` converts without a stylesheet. Otherwise the stylesheet is loaded like an
[xslt](xslt.md) resource (path, `file:`, `classpath:`, `ref:`).

## Example

Convert an XML customer record from a legacy system to JSON for an agent tool.

<!-- example: xj-xml-to-json -->
```yaml
- route:
    id: xj-xml2json
    from:
      uri: direct:xj-xml2json
      steps:
        - to: xj:identity?transformDirection=XML2JSON
```

<!-- input: xj-xml-to-json -->
```xml
<customer id="c-7">
  <name>Ada</name>
  <tier>gold</tier>
  <phones><phone>111</phone><phone>222</phone></phones>
</customer>
```

<!-- output: xj-xml-to-json -->
```json
{"@id":"c-7","name":"Ada","tier":"gold","phones":["111","222"]}
```

The rules for plain XML: the root element's content is the result; an element with only text
becomes a string; an element with child elements or attributes becomes an object (attributes
as `@name` keys, mixed text as `#text`, repeated names as arrays); an element whose children
all share one name (two or more) becomes an array.

### JSON to XML

JSON becomes Camel's xj representation: every value is an `object` element with `xj:name` and
`xj:type` attributes.

<!-- example: xj-json-to-xml -->
```yaml
- route:
    id: xj-json2xml
    from:
      uri: direct:xj-json2xml
      steps:
        - to: xj:identity?transformDirection=JSON2XML
```

<!-- input: xj-json-to-xml -->
```json
{"name":"camel","tags":["a"],"stars":5}
```

<!-- output: xj-json-to-xml {"contains":true} -->
```xml
<object xj:name="name" xj:type="string">camel</object>
<object xj:name="tags" xj:type="array"><object xj:type="string">a</object></object>
<object xj:name="stars" xj:type="int">5</object>
```

`xj:type` is one of `object`, `array`, `string`, `int`, `float`, `boolean` or `null`.

### With a stylesheet

With a stylesheet, JSON → XML feeds the xj document into the stylesheet; XML → JSON converts
the stylesheet's result.

<!-- file: xsl/xj-person.xsl -->
```xml
<xsl:stylesheet version="1.0" xmlns:xsl="http://www.w3.org/1999/XSL/Transform"
    xmlns:xj="http://camel.apache.org/component/xj" exclude-result-prefixes="xj">
  <xsl:output method="xml" omit-xml-declaration="yes"/>
  <xsl:template match="/object">
    <person name="{object[@xj:name='name']}" stars="{object[@xj:name='stars']}"/>
  </xsl:template>
</xsl:stylesheet>
```

<!-- example: xj-json-to-xml-stylesheet -->
```yaml
- route:
    id: xj-json2xml-xsl
    from:
      uri: direct:xj-json2xml-xsl
      steps:
        - to: xj:xsl/xj-person.xsl?transformDirection=JSON2XML
```

<!-- input: xj-json-to-xml-stylesheet -->
```json
{"name":"camel","stars":5}
```

<!-- output: xj-json-to-xml-stylesheet -->
```xml
<person name="camel" stars="5"/>
```

## Rust DSL

```rust
b.from("direct:xj-rust")
    .to("xj:identity?transformDirection=XML2JSON");
```

## Options

| option | default | description |
|---|---|---|
| `transformDirection` | (required) | `XML2JSON` or `JSON2XML`. |
| `output` | `string` | `string` or `bytes`. |
| `contentCache` | `true` | Compile the stylesheet once. |
| `failOnNullBody` | `true` | Fail when the body is null. |

The other [xslt](xslt.md#options) options are accepted and ignored.

## Message headers

None of its own. With a stylesheet, headers and exchange properties are passed as
`xsl:param` values, as in the xslt component.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/xj.camel.yaml
```

## Common questions

### How do I turn an XML API response into JSON?

Send it to `xj:identity?transformDirection=XML2JSON`. Attributes become `@name` keys and
repeated elements become arrays.

### Why is a single repeated element not an array?

An element with one child is converted as an object (or a string). Only two or more children
with the same name become an array. If a consumer always needs an array, map it with a
stylesheet or with the [JSON data format](../dataformats/json.md) and Simple afterwards.

### What is the `xj:` namespace in the JSON2XML output?

It is Camel's xj namespace (`http://camel.apache.org/component/xj`), so stylesheets written for
Camel's xj component work unchanged.

## Related

- [xslt](xslt.md), [xquery](xquery.md).
- [Jackson XML data format](../dataformats/jacksonxml.md): a configurable value ⇄ XML mapping.
- [Apache Camel XJ component](https://camel.apache.org/components/next/xj-component.html)
