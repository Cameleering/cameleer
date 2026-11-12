# Validator component

> The `validator` component checks the XML message body against an XML Schema (XSD) and fails the exchange with a `SchemaValidationException` listing every error when it does not conform.

**Scheme:** `validator:` · **Crate:** `cameleer-xml` · **Cargo feature:** none for a well-formedness check; [`xml`](../guides/build-features.md) for full XML Schema validation · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [Validator](https://camel.apache.org/components/next/validator-component.html)

## When to use it

- Reject malformed partner documents (orders, invoices, HL7 v3, UBL) at the edge of a route.
- Check XML produced by an LLM or a transformation before sending it to a strict backend.
- Route invalid documents to a dead letter or error queue with `doTry` / `onException`.

## URI format

```text
validator:schemaResource[?options]
```

`schemaResource` is a file path relative to the working directory, `file:path`,
`classpath:path` (read as a relative path) or `ref:beanName`. `xs:include` and `xs:import`
resolve relative to the schema file.

## Example

Validate orders and answer with the validation errors instead of failing the caller.

<!-- file: xsd/order.xsd -->
```xml
<xs:schema xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <xs:element name="order">
    <xs:complexType>
      <xs:sequence>
        <xs:element name="customer" type="xs:string"/>
        <xs:element name="qty" type="xs:positiveInteger"/>
      </xs:sequence>
      <xs:attribute name="id" type="xs:string" use="required"/>
    </xs:complexType>
  </xs:element>
</xs:schema>
```

<!-- example: validator-order-xsd -->
```yaml
- route:
    id: validator-order
    from:
      uri: direct:validator-order
      steps:
        - doTry:
            steps:
              - to: validator:xsd/order.xsd
              - setBody:
                  constant: valid
            doCatch:
              - exception:
                  - org.apache.camel.ValidationException
                steps:
                  - setBody:
                      simple: "invalid: ${exception.message}"
```

A valid order:

<!-- input: validator-order-xsd -->
```xml
<order id="o-1"><customer>Ada</customer><qty>2</qty></order>
```

<!-- output: validator-order-xsd -->
```text
valid
```

An order with a missing attribute and a negative quantity:

<!-- input: validator-order-xsd#2 -->
```xml
<order><customer>Ada</customer><qty>-1</qty></order>
```

<!-- output: validator-order-xsd#2 {"contains":true} -->
```text
invalid: Validation failed for: xsd/order.xsd
cvc-complex-type.4
cvc-datatype-valid.1.2.1
```

`SchemaValidationException` is a `ValidationException`, so the catch above takes both schema
errors and documents that are not well-formed.

## Rust DSL

```rust
b.from("direct:validator-rust")
    .to("validator:xsd/order.xsd")
    .set_body(constant("valid"));
```

## Options

| option | default | description |
|---|---|---|
| `failOnNullBody` | `true` | Fail with `NoXmlBodyValidationException` when the body is null. |
| `failOnNullHeader` | `true` | Fail with `NoXmlHeaderValidationException` when `headerName` is set and the header is missing. |
| `headerName` | (none) | Validate this header instead of the body. |
| `useSharedSchema` | `true` | Load and compile the schema once. |
| `contentCache` | `true` | Same effect as `useSharedSchema`: `false` reloads the schema per message. |

Accepted and ignored (Java-only settings): `schemaFactory`, `schemaLanguage`,
`resourceResolver`, `resourceResolverFactory`, `errorHandler`, `useDom`,
`schemaReaderFactory`, `lazyStartProducer`.

### Supported XML Schema

A subset of XML Schema 1.0: global and local elements, `ref`, `minOccurs` / `maxOccurs`,
`sequence` / `choice` / `all`, model groups, attributes, simple types with restrictions
(facets), the built-in data types, `include` and `import`. Without `cameleer-xml` registered,
cameleer-core's `validator` only checks well-formedness; `cameleer_main::new_main()` and the
`cameleer` CLI always register the full validator.

## Message headers

| header / property | direction | description |
|---|---|---|
| `CamelValidationErrors` (exchange property) | out | The list of error messages, set when validation fails. |

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/validate.camel.yaml
```

## Common questions

### How do I get all validation errors, not just the first?

The exception message lists every error, and the `CamelValidationErrors` exchange property
holds them as a list.

### How do I validate a header instead of the body?

Set `headerName=myHeader`.

### Does it also catch XML that is not well-formed?

Yes. A parse error fails with `SchemaValidationException` too, as Xerces reports it through
the same error handler in Camel.

## Related

- [json-validator](json-validator.md) for JSON Schema.
- [xslt](xslt.md), [xmlsecurity-verify](xmlsecurity-verify.md).
- [Apache Camel Validator component](https://camel.apache.org/components/next/validator-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
