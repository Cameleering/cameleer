# Bindy data formats

> The Bindy data formats (`bindyCsv`, `bindyFixed`, `bindyKvp`) map delimited, fixed-length and key-value records to typed fields described by a record descriptor, in both directions.

**Name:** `bindyCsv` · **Also:** `bindy` (with `type: Csv`, `Fixed` or `KeyValue`), `bindyFixed`,
`bindyKvp` (and the `bindy-csv`, `bindycsv`... spellings) · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [Bindy](https://camel.apache.org/components/next/dataformats/bindyCsv-dataformat.html)

## When to use it

- Read and write fixed-width mainframe and banking files, where every field has a position and a
  width.
- Parse CSV with typed columns (integers, decimals, dates in a given pattern) instead of strings.
- Read FIX-style key-value messages (`8=FIX.4.1<SOH>35=0...`).

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `unmarshal` | record text | a list of objects (field name -> typed value; dates as ISO-8601 strings). A single record gives one object unless `unwrapSingleInstance: false`. |
| `marshal` | a list of objects or one object | record text (padded, aligned and formatted per the descriptor) |

**Difference from Apache Camel:** Rust has no annotations. The `@CsvRecord`, `@FixedLengthRecord`,
`@Message`, `@DataField` and `@KeyValuePairField` settings are written in a YAML or JSON
*descriptor* instead (`descriptor`, `descriptorFile`, or a registry entry named by `classType`).

## Descriptor

```text
className: com.acme.Order   # informational
separator: ","              # CSV: field separator (default ",")
skipFirstLine: true         # CSV: skip a header line
generateHeaderColumns: true # CSV: write a header line on marshal
crlf: UNIX                  # WINDOWS (default), UNIX, MAC or a literal
length: 30                  # fixed: record length check (0 = none)
paddingChar: " "            # fixed: record padding
keyValuePairSeparator: "="  # kvp
pairSeparator: "\u0001"     # kvp
fields:
  - name: orderNr
    pos: 1                  # CSV column or fixed start position (1 based)
    type: int               # string, int, long, double, float, decimal, boolean, char, date
    pattern: dd-MM-yyyy     # date or decimal pattern
    precision: 2            # decimals written for double/decimal
    length: 10              # fixed: width
    align: R                # fixed: R (default) or L
    trim: true
    defaultValue: "0"
    required: true
    columnName: Order Nr    # header text (default: name)
    tag: 8                  # kvp: the key
```

Other field and record settings: `paddingChar` per field, `delimiter` and `clip` (fixed),
`quote`, `quoting`, `removeQuotes`, `endWithLineBreak` (CSV), `ignoreTrailingChars`,
`ignoreMissingChars`, `skipHeader`, `skipFooter` (fixed).

## Example

A CSV order file with typed columns:

<!-- file: order-csv.yaml -->
```text
className: com.acme.Order
separator: ","
skipFirstLine: true
fields:
  - name: orderNr
    pos: 1
    type: int
  - name: customer
    pos: 2
  - name: amount
    pos: 3
    type: decimal
    precision: 2
  - name: orderDate
    pos: 4
    type: date
    pattern: dd-MM-yyyy
```

<!-- example: bindy-csv-unmarshal -->
```yaml
- route:
    id: bindy-csv
    from:
      uri: direct:bindy-csv
      steps:
        - unmarshal:
            bindyCsv:
              descriptorFile: order-csv.yaml
        - marshal:
            json: {}
```

<!-- input: bindy-csv-unmarshal -->
```text
nr,customer,amount,date
1,Ada,49.5,02-01-2024
2,Grace,120,15-03-2024
```

<!-- output: bindy-csv-unmarshal -->
```json
[{"orderNr":1,"customer":"Ada","amount":49.5,"orderDate":"2024-01-02"},{"orderNr":2,"customer":"Grace","amount":120.0,"orderDate":"2024-03-15"}]
```

Write fixed-length records, the classic mainframe layout:

<!-- file: order-fixed.yaml -->
```text
className: com.acme.FixedOrder
crlf: UNIX
fields:
  - name: orderNr
    pos: 1
    length: 5
    type: int
    paddingChar: "0"
  - name: customer
    pos: 6
    length: 10
    align: L
  - name: amount
    pos: 16
    length: 8
    type: decimal
    precision: 2
```

<!-- example: bindy-fixed-marshal -->
```yaml
- route:
    id: bindy-fixed
    from:
      uri: direct:bindy-fixed
      steps:
        - unmarshal:
            json: {}
        - marshal:
            bindyFixed:
              descriptorFile: order-fixed.yaml
```

<!-- input: bindy-fixed-marshal -->
```json
[{"orderNr":1,"customer":"Ada","amount":49.5},{"orderNr":42,"customer":"Grace","amount":120}]
```

<!-- output: bindy-fixed-marshal -->
```text
00001Ada          49.50
00042Grace       120.00
```

## Rust DSL

```rust
b.from("direct:bindy-rust")
    .unmarshal(DataFormatDefinition::new("bindyCsv").option("descriptorFile", "order-csv.yaml"))
    .marshal(DataFormatDefinition::json());
```

Descriptors can also be built in Rust with `cameleer_dataformats::bindy::BindyDescriptor` and
bound in the registry, then referenced with `classType`.

## Options

| option | default | description |
|---|---|---|
| `descriptor` | (none) | Inline descriptor text (YAML or JSON). |
| `descriptorFile` | (none) | Path of a descriptor file. |
| `classType` (`className`) | (none) | A descriptor bound in the registry, a registry string holding descriptor text, or a `.yaml`/`.yml`/`.json` path. |
| `type` | `Csv` | For the generic `bindy` name: `Csv`, `Fixed` or `KeyValue`. |
| `unwrapSingleInstance` | `true` | A single record unmarshals to an object instead of a one-element list. |
| `allowEmptyStream` | `false` | An empty body unmarshals to an empty list instead of failing. |

`locale` is accepted and ignored.

## Common questions

### How do I parse a fixed-width file?

Write a descriptor with `pos` and `length` for each field and `unmarshal: {bindyFixed: {descriptorFile: layout.yaml}}`.

### How do I convert dates while parsing CSV?

Give the field `type: date` and a `pattern` such as `dd-MM-yyyy`. The value becomes an ISO-8601
date string, and `marshal` writes it back in the pattern.

### Should I use bindyCsv or csv?

Use [csv](csv.md) when strings are enough. Use Bindy for typed fields, fixed widths or key-value
records.

## Related

- [CSV data format](csv.md), [uniVocity data formats](univocity.md)
- [Apache Camel Bindy](https://camel.apache.org/components/next/dataformats/bindyCsv-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
