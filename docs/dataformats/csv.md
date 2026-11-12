# CSV data format

> The `csv` data format parses CSV text into rows (lists or maps) and writes rows back as CSV, following RFC 4180.

**Name:** `csv` · **Crate:** `cameleer-core` · **Cargo feature:** none, always included ([build features](../guides/build-features.md)) ·
**Apache Camel:** [CSV](https://camel.apache.org/components/next/dataformats/csv-dataformat.html)

## When to use it

- Read CSV exports (orders, sensor logs, CRM dumps) and process each row: split it, filter it,
  insert it into a database or send it to a model as a prompt.
- Turn JSON or a list of maps into a CSV report or a spreadsheet upload.
- Change the delimiter or quoting of a file (semicolon-separated European exports, TSV).

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `unmarshal` | CSV text (string or bytes; a UTF-8 BOM is skipped) | a list of rows. Each row is a list of strings, or a map keyed by the header with `useMaps: true`. |
| `marshal` | a list of rows (lists or maps), one map, one list of values, or JSON text holding those | CSV text. Lines end with CRLF by default. |

Quoted fields may contain the delimiter, quotes (`""`) and line breaks. With maps, the columns
are the `header` option or the keys of the maps in order of first appearance; the header line is
written when `header` is set or `useMaps: true`.

## Example

Turn a CSV export into JSON records. With `useMaps`, the first line names the fields.

<!-- example: csv-rows-to-json -->
```yaml
- route:
    id: csv-to-json
    from:
      uri: direct:csv-to-json
      steps:
        - unmarshal:
            csv:
              useMaps: true
        - marshal:
            json: {}
```

<!-- input: csv-rows-to-json -->
```text
sku,qty,note
A1,2,"red, large"
B2,1,
```

<!-- output: csv-rows-to-json -->
```json
[{"sku":"A1","qty":"2","note":"red, large"},{"sku":"B2","qty":"1","note":""}]
```

Write JSON records as a semicolon-separated file with a header line:

<!-- example: csv-json-to-csv -->
```yaml
- route:
    id: json-to-csv
    from:
      uri: direct:json-to-csv
      steps:
        - marshal:
            csv:
              delimiter: ";"
              header: sku,qty
              recordSeparator: "\\n"
```

<!-- input: csv-json-to-csv -->
```json
[{"sku":"A1","qty":2,"price":9.5},{"sku":"B2","qty":1,"price":20}]
```

<!-- output: csv-json-to-csv -->
```text
sku;qty
A1;2
B2;1
```

Columns not listed in `header` (here `price`) are left out.

## Rust DSL

```rust
b.from("direct:csv-rust")
    .unmarshal(DataFormatDefinition::csv().use_maps(true))
    .marshal(DataFormatDefinition::json());
```

## Options

| option | default | description |
|---|---|---|
| `delimiter` | `,` | Field separator. `\t`, `TAB` and `SPACE` are accepted. |
| `quote` | `"` | Quote character. |
| `quoteDisabled` | `false` | No quoting at all. |
| `escape` | (none) | Escape character inside fields. |
| `recordSeparator` | CRLF | Line ending written by `marshal` (`\n`, `\r\n`). |
| `header` | (none) | Comma separated column names: the map keys on unmarshal (the first line is then data unless `skipHeaderRecord`), the columns and header line on marshal. |
| `skipHeaderRecord` | `false` | Unmarshal: drop the first line. Marshal: do not write the header line. |
| `useMaps` | `false` | Unmarshal each row to a map keyed by the header. |
| `useOrderedMaps` | `false` | Same as `useMaps` (maps keep the column order). |
| `ignoreEmptyLines` | `true` | Skip blank lines. |
| `ignoreSurroundingSpaces` (`trim`) | `false` | Trim spaces around fields. |
| `quoteMode` | `MINIMAL` | When to quote on marshal: `MINIMAL`, `ALL`, `NON_NUMERIC`, `NONE`. |
| `nullString` | (none) | Text written for null and read back as null. |
| `commentMarker` | (none) | Lines starting with this character are skipped. |

## Common questions

### How do I read a CSV file with a header row into maps?

Use `unmarshal: {csv: {useMaps: true}}`. Each row becomes a map such as
`{"sku":"A1","qty":"2"}`. Values are strings; convert them where you need numbers.

### How do I process a CSV file row by row?

Unmarshal it, then `split: {simple: "${body}"}` over the list. Each split exchange carries one row,
which you can turn into an LLM prompt with a [template](../components/freemarker.md) or insert
into a table.

### How do I write a semicolon-separated file?

Set `delimiter: ";"` on `marshal`. Add `header: a,b,c` to choose and order the columns.

### What is the difference between csv, univocityCsv and bindyCsv?

They read the same text. [univocityCsv](univocity.md) is `csv` with uniVocity option names, and
[bindyCsv](bindy.md) maps columns to typed fields from a record descriptor.

## Related

- [uniVocity data formats](univocity.md), [Bindy data formats](bindy.md),
  [JSON data format](json.md)
- [Split EIP](../guides/eips.md) to process rows one at a time
- [Apache Camel CSV data format](https://camel.apache.org/components/next/dataformats/csv-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
