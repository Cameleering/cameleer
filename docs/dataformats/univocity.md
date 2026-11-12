# uniVocity data formats

> The uniVocity data formats (`univocityCsv`, `univocityTsv`, `univocityFixed`) parse and write CSV, tab-separated and fixed-width files as rows or maps, with uniVocity's option names.

**Name:** `univocityCsv` · **Also:** `univocityTsv`, `univocityFixed` (and the `univocity-csv`,
`univocitycsv`... spellings) · **Crate:** `cameleer-dataformats` · **Cargo feature:** [`dataformats`](../guides/build-features.md) ·
**Apache Camel:** [uniVocity CSV](https://camel.apache.org/components/next/dataformats/univocityCsv-dataformat.html)

## When to use it

- Run Camel routes that already use `univocityCsv`, `univocityTsv` or `univocityFixed` unchanged.
- Read TSV exports (spreadsheets, bioinformatics, log tools) with escapes such as `\t` and `\n`.
- Read fixed-width files by field lengths without writing a record descriptor.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `unmarshal` | text | a list of rows: lists of strings, or maps with `asMap: true` (keys from `headers`, or the first line with `headerExtractionEnabled: true`) |
| `marshal` | a list of lists or of maps (`headers` selects and orders the columns) | text, one line per row |

`univocityCsv` is the [csv](csv.md) data format with the option names translated. `univocityTsv`
and `univocityFixed` are separate implementations with the same row options.

## Example

Read a TSV file with a header line into maps:

<!-- example: univocity-tsv-to-json -->
```yaml
- route:
    id: tsv-to-json
    from:
      uri: direct:tsv-to-json
      steps:
        - unmarshal:
            univocityTsv:
              headerExtractionEnabled: true
              asMap: true
        - marshal:
            json: {}
```

<!-- input: univocity-tsv-to-json -->
```text
host	status	note
web-1	up	ok
web-2	down	disk\tfull
```

<!-- output: univocity-tsv-to-json -->
```json
[{"host":"web-1","status":"up","note":"ok"},{"host":"web-2","status":"down","note":"disk\tfull"}]
```

The `\t` in the last field is an escaped tab, decoded to a real tab character.

Read fixed-width records by field lengths:

<!-- example: univocity-fixed-width -->
```yaml
- route:
    id: fixed-width
    from:
      uri: direct:fixed-width
      steps:
        - unmarshal:
            univocityFixed:
              fieldLengths: 5,8,4
              headers: code,city,temp
              asMap: true
              ignoreTrailingWhitespaces: true
        - marshal:
            json: {}
```

<!-- input: univocity-fixed-width -->
```text
AMS01Amsterdam12
BLR02Bangalor31
```

<!-- output: univocity-fixed-width -->
```json
[{"code":"AMS01","city":"Amsterda","temp":"m12"},{"code":"BLR02","city":"Bangalor","temp":"31"}]
```

Fields are cut strictly by length, as uniVocity does: `Amsterdam` is 9 characters, so its last
letter spills into the next field. Make the field wide enough for your longest value.

Write CSV with the columns chosen by `headers`:

<!-- example: univocity-csv-marshal -->
```yaml
- route:
    id: univocity-csv-out
    from:
      uri: direct:univocity-csv-out
      steps:
        - unmarshal:
            json: {}
        - marshal:
            univocityCsv:
              headers: host,status
              lineSeparator: "\\n"
              quoteAllFields: true
```

<!-- input: univocity-csv-marshal -->
```json
[{"host":"web-1","status":"up","cpu":12},{"host":"web-2","status":"down","cpu":0}]
```

<!-- output: univocity-csv-marshal -->
```text
"host","status"
"web-1","up"
"web-2","down"
```

## Rust DSL

```rust
b.from("direct:univocity-rust")
    .unmarshal(DataFormatDefinition::new("univocityTsv").option("headerExtractionEnabled", true).option("asMap", true))
    .marshal(DataFormatDefinition::json());
```

## Options

| option | applies to | description |
|---|---|---|
| `headers` | all | Comma separated column names (map keys on unmarshal, column selection on marshal). |
| `headerExtractionEnabled` | all | Read the header names from the first line (the line is skipped). |
| `asMap` | all | Unmarshal rows to maps instead of lists. |
| `headersDisabled` | all | Ignore header names: map keys become `column1`, `column2`... |
| `lineSeparator` | all | Line separator written by marshal (default `\n` for TSV and fixed, CRLF for CSV). |
| `skipEmptyLines` | all | Skip blank lines (default true). |
| `nullValue` | all | Text written for null (CSV: also read as null). |
| `emptyValue` | all | Value of empty fields on unmarshal. |
| `comment` | all | Lines starting with this character are skipped. |
| `ignoreLeadingWhitespaces`, `ignoreTrailingWhitespaces` | all | Trim fields. |
| `numberOfRecordsToRead` | all | Stop after this many records. |
| `lazyLoad`, `normalizedLineSeparator` | all | Accepted; rows are always returned as a list. |
| `delimiter`, `quote`, `quoteEscape`, `quoteAllFields` | CSV | Delimiter, quote character, quote escape, quote every field on marshal. |
| `escapeChar` | TSV | Escape character (default `\`). |
| `fieldLengths` | fixed | Comma separated field widths (required). |
| `padding` | fixed | Padding character written by marshal (default space). |
| `skipTrailingCharsUntilNewline` | fixed | Drop the rest of the line after the last field. |
| `recordEndsOnNewline` | fixed | End a record at a newline even if fields are missing (they become null). |

## Common questions

### How do I parse a tab-separated file?

`unmarshal: {univocityTsv: {headerExtractionEnabled: true, asMap: true}}` gives one map per line.

### How do I read a fixed-width file without a descriptor?

Use `univocityFixed` with `fieldLengths`. For typed fields (numbers, dates) use
[bindyFixed](bindy.md).

## Related

- [CSV data format](csv.md), [Bindy data formats](bindy.md)
- [Apache Camel uniVocity CSV](https://camel.apache.org/components/next/dataformats/univocityCsv-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
