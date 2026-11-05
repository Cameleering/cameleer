# ASN.1 data format

> The `asn1` data format encodes values as ASN.1 BER/DER and decodes BER/DER back into values, either as a generic tree or typed by an ASN.1 module compiled at runtime; it can also split a stream of BER records.

**Name:** `asn1` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [ASN.1 File](https://camel.apache.org/components/next/dataformats/asn1-dataformat.html)

## When to use it

- Read telecom files: call detail records (CDRs), 3GPP charging data and SS7/MAP traces are
  streams of BER records.
- Look inside X.509 certificates, PKCS structures, SNMP or LDAP messages without a dedicated
  parser.
- Exchange typed records with systems that publish an ASN.1 module, without generating code
  with jASN1 or ASN1bean.

## Marshal and unmarshal

Three modes, chosen by the options:

| mode | marshal | unmarshal |
|---|---|---|
| `usingIterator=true` (no schema) | an array of records (bytes or values) → the records concatenated | a stream of BER records → an array with the bytes of each top-level element (for `split`) |
| generic (no schema) | a value tree → DER bytes | BER/DER bytes → a value tree |
| typed (`schema` or `moduleFile` plus `clazzName`) | an object (or JSON text) of the named type → DER bytes | BER/DER bytes → an object of the named type; with `usingIterator=true` an array of them |

A body that is already bytes is passed through unchanged by marshal.

**Generic tree.** `SEQUENCE` and `SET` become arrays, `INTEGER`/`ENUMERATED` integers,
`BOOLEAN`, `NULL`, `OCTET STRING` bytes, character strings strings, `OBJECT IDENTIFIER` a
dotted string, times strings. Other tags become `{"tag": "[0]", "value": ...}` (constructed)
or `{"tag": "[APPLICATION 1]", "bytes": ...}` (primitive). On marshal, strings are written as
`UTF8String`, and `{"oid": "1.2.3"}`, `{"set": [...]}`, `{"printable": ".."}`, `{"ia5": ".."}`,
`{"utcTime": ".."}` and `{"generalizedTime": ".."}` select other universal types.

**Typed mode.** The module compiler supports `DEFINITIONS [EXPLICIT|IMPLICIT|AUTOMATIC TAGS]`,
type and value assignments, `BOOLEAN`, `INTEGER`, `ENUMERATED`, `REAL`, `NULL`,
`OCTET STRING`, `BIT STRING`, `OBJECT IDENTIFIER`, `RELATIVE-OID`, character strings, times,
`SEQUENCE`/`SET` (with `OPTIONAL`, `DEFAULT`, `COMPONENTS OF`, extension markers),
`SEQUENCE OF`/`SET OF`, `CHOICE`, `ANY` and tagged types. Constraints are parsed and ignored.
`SEQUENCE` values are objects keyed by component name, `CHOICE` is `{"alternative": value}`,
`ENUMERATED` is the item name.

## Example

A typed record: a module inline in the route, JSON in, DER out (shown as hex), and back.

<!-- example: asn1-typed-round-trip -->
```yaml
- route:
    id: asn1-typed
    from:
      uri: direct:asn1-typed
      steps:
        - marshal:
            asn1:
              clazzName: Reading
              schema: |
                Telemetry DEFINITIONS AUTOMATIC TAGS ::= BEGIN
                  Reading ::= SEQUENCE {
                    sensor  UTF8String,
                    celsius INTEGER,
                    status  ENUMERATED { ok(0), warn(1), fail(2) } DEFAULT ok
                  }
                END
        - marshal:
            hex: {}
        - setHeader:
            name: der
            simple: "${body}"
        - unmarshal:
            hex: {}
        - unmarshal:
            asn1:
              clazzName: Reading
              schema: |
                Telemetry DEFINITIONS AUTOMATIC TAGS ::= BEGIN
                  Reading ::= SEQUENCE {
                    sensor  UTF8String,
                    celsius INTEGER,
                    status  ENUMERATED { ok(0), warn(1), fail(2) } DEFAULT ok
                  }
                END
        - setBody:
            simple: "${header.der}\n${body}"
```

<!-- input: asn1-typed-round-trip -->
```json
{"sensor": "t1", "celsius": -5, "status": "warn"}
```

<!-- output: asn1-typed-round-trip -->
```text
300a800274318101fb820101
{"sensor":"t1","celsius":-5,"status":"warn"}
```

With automatic tagging each component gets a context tag: `80 02 7431` is `[0]` `t1`,
`81 01 fb` is `[1]` -5 and `82 01 01` is `[2]` `warn` (1), all inside a `SEQUENCE` (`30 0a`).

The generic tree needs no module. Here a JSON array becomes a DER `SEQUENCE` with an
`INTEGER`, an `OBJECT IDENTIFIER` and a context-tagged string:

<!-- example: asn1-generic-tree -->
```yaml
- route:
    id: asn1-generic
    from:
      uri: direct:asn1-generic
      steps:
        - unmarshal:
            json: {}
        - marshal:
            asn1: {}
        - marshal:
            hex: {}
```

<!-- input: asn1-generic-tree -->
```json
[5, {"oid": "1.2.840.113549.1.1.11"}, {"tag": "[0]", "value": "x"}]
```

<!-- output: asn1-generic-tree -->
```text
301302010506092a864886f70d01010ba0030c0178
```

Split a file of concatenated BER records (for example a CDR file) into one exchange per
record:

<!-- example: asn1-split-records -->
```yaml
- route:
    id: asn1-split
    from:
      uri: direct:asn1-split
      steps:
        - unmarshal:
            hex: {}
        - unmarshal:
            asn1:
              usingIterator: true
        - split:
            simple: "${body}"
            aggregationStrategy: GroupedBodyAggregationStrategy
            steps:
              - marshal:
                  hex: {}
        - setBody:
            simple: "${body}"
```

<!-- input: asn1-split-records -->
```text
3003020101300302010230030201033003020104
```

<!-- output: asn1-split-records -->
```text
["3003020101","3003020102","3003020103","3003020104"]
```

## Rust DSL

```rust
b.from("direct:asn1-rust")
    .unmarshal(DataFormatDefinition::new("asn1").option("usingIterator", "true"))
    .split(simple("${body}"))
    .to("mock:asn1-record")
    .end();
```

## Options

| option | default | description |
|---|---|---|
| `usingIterator` | `false` | Unmarshal a stream of records into an array (bytes per record, or typed values with a schema); marshal concatenates an array. |
| `schema` | (none) | ASN.1 module text inline. Aliases `module`, `asn1Module`. |
| `moduleFile` | (none) | Path of an ASN.1 module file. Aliases `schemaFile`, `asn1File`. |
| `clazzName` | (none) | The ASN.1 type to encode/decode (optionally `Module.Type`); required with a schema. Aliases `className`, `typeName`. |

## Differences from Apache Camel

Camel's `asn1` data format splits record streams and otherwise needs classes generated by
jASN1/ASN1bean (`clazzName` is a Java class). Here `clazzName` names a type in a module that
is compiled when the route starts, and without a module you get a generic value tree.

## Common questions

### How do I decode a BER file without the ASN.1 module?

Use `unmarshal: asn1: {}`. You get the generic tree: arrays for sequences and tag objects for
context-specific fields. It is enough to inspect a file or pick a few known positions.

### How do I process a CDR file record by record?

`unmarshal: asn1: {usingIterator: true}` and then `split: simple: "${body}"`; decode each
record with a typed `asn1` step inside the split if you have the module.

### Are definite and indefinite lengths supported?

Both are read. Marshal always writes DER (definite lengths, sorted `SET OF` per the
canonical rules for the generic tree).

## Related

- [Hex data format](hex.md), [Split EIP](../guides/eips.md)
- [Protobuf data format](protobuf.md), [Thrift data format](thrift.md)
- [Apache Camel ASN.1 data format](https://camel.apache.org/components/next/dataformats/asn1-dataformat.html)
