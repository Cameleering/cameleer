# XML data format (Jackson XML)

> The `jacksonXml` data format converts between XML text and a structured body: object keys become elements, lists repeat the element, and options map keys to attributes, text and namespaces.

**Name:** `jacksonXml` · **Also:** `xml`, `jacksonxml` (and `jaxb` when `cameleer-soap` is not
registered) · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Apache Camel:** [Jackson XML](https://camel.apache.org/components/next/dataformats/jacksonXml-dataformat.html)

## When to use it

- Convert JSON from a REST API or an LLM into XML for a legacy or SOAP-style system, and back.
- Read XML documents into a map so Simple, JSONPath or jq can pick fields without XPath.
- Produce XML with attributes, namespaces and wrapper elements without writing a template.

With `cameleer_main::new_main` (the `cameleer` CLI), `jaxb` is the schema-typed
[JAXB data format](jaxb.md) from `cameleer-soap`; use `jacksonXml` or `xml` for this generic
mapping.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | an object (or JSON text of one) | XML text with the root element `rootName`. Sets `Content-Type: application/xml` unless `contentTypeHeader: false`. |
| `unmarshal` | XML text | an object: child elements become keys (repeated elements become lists), attributes become keys too, text-only elements become strings. Namespace prefixes are dropped. |

Values are not typed on unmarshal: `<qty>2</qty>` gives the string `"2"`.

**Difference from Apache Camel:** there is no class binding. Jackson annotations are replaced
by the options `attributes`, `textProperty`, `properties`, `namespaces` and `wrappers`.

## Example

Turn a JSON order into XML, with `id` as an attribute:

<!-- example: jacksonxml-json-to-xml -->
```yaml
- route:
    id: json-to-xml
    from:
      uri: direct:json-to-xml
      steps:
        - unmarshal:
            json: {}
        - marshal:
            jacksonXml:
              rootName: order
              attributes: id
```

<!-- input: jacksonxml-json-to-xml -->
```json
{"id":42,"customer":"Ada","item":[{"sku":"A1","qty":2},{"sku":"B2","qty":1}]}
```

<!-- output: jacksonxml-json-to-xml -->
```xml
<order id="42"><customer>Ada</customer><item><sku>A1</sku><qty>2</qty></item><item><sku>B2</sku><qty>1</qty></item></order>
```

Read XML into a structure and write it as JSON:

<!-- example: jacksonxml-xml-to-json -->
```yaml
- route:
    id: xml-to-json
    from:
      uri: direct:xml-to-json
      steps:
        - unmarshal:
            xml: {}
        - marshal:
            json: {}
```

<!-- input: jacksonxml-xml-to-json -->
```xml
<order id="42"><customer>Ada</customer><item><sku>A1</sku></item><item><sku>B2</sku></item></order>
```

<!-- output: jacksonxml-xml-to-json -->
```json
{"id":"42","customer":"Ada","item":[{"sku":"A1"},{"sku":"B2"}]}
```

Wrap a list in a parent element and declare a namespace:

<!-- example: jacksonxml-wrappers-namespace -->
```yaml
- route:
    id: xml-wrappers
    from:
      uri: direct:xml-wrappers
      steps:
        - unmarshal:
            json: {}
        - marshal:
            jacksonXml:
              rootName: catalog
              namespace: urn:acme:catalog
              wrappers: "books:book"
```

<!-- input: jacksonxml-wrappers-namespace -->
```json
{"books":["Dune","Emma"]}
```

<!-- output: jacksonxml-wrappers-namespace -->
```xml
<catalog xmlns="urn:acme:catalog"><books><book>Dune</book><book>Emma</book></books></catalog>
```

## Rust DSL

```rust
b.from("direct:xml-rust")
    .unmarshal(DataFormatDefinition::json())
    .marshal(DataFormatDefinition::xml().root_name("order"));
```

## Options

| option | default | description |
|---|---|---|
| `rootName` | `root` | Root element name; may be `p:name` or `{uri}name`. |
| `prettyPrint` | `false` | Indent the XML. |
| `xmlDeclaration` | `false` | Start with `<?xml version="1.0" encoding="UTF-8"?>`. |
| `contentTypeHeader` | `true` | Set `Content-Type: application/xml` on marshal. |
| `attributePrefix` | `@` | Keys starting with it are written as attributes. When set explicitly, unmarshal also prefixes attribute keys with it. |
| `attributes` | (none) | Comma separated keys always written as attributes. |
| `textProperty` | `""` | Key holding the element's own text (`#text` and `$` are always accepted on marshal). |
| `properties` | (none) | `key:xmlName,...` renames; `xmlName` may be `p:local` or `{uri}local`. |
| `namespace` | (none) | Default namespace of the root element (`xmlns="..."`). |
| `namespaces` | (none) | `prefix=uri,...` declared on the root element. |
| `wrappers` | (none) | `key:itemName,...`: write the list `key` inside a `<key>` wrapper with `<itemName>` items; unwrapped again on unmarshal. |

## Common questions

### How do I convert JSON to XML?

`unmarshal: {json: {}}` then `marshal: {jacksonXml: {rootName: order}}`. For XML that must match
an XSD, use the [JAXB data format](jaxb.md) or an [XSLT](../components/xslt.md) template instead.

### How do I write an attribute instead of an element?

List the key in `attributes: id,version`, or prefix the key with `@` (`{"@id": 42}`).

### Why are numbers strings after unmarshal?

XML has no types. Convert the fields you need, or use the schema-typed [JAXB](jaxb.md) format.

## Related

- [JAXB data format](jaxb.md), [XJ component](../components/xj.md),
  [XSLT component](../components/xslt.md), [XPath language](../languages/xpath.md)
- [JSON data format](json.md)
- [Apache Camel Jackson XML data format](https://camel.apache.org/components/next/dataformats/jacksonXml-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
