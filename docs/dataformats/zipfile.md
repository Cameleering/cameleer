# Zip File data format

> The `zipFile` data format packs the message body into a `.zip` archive (marshal) and unpacks entries from a `.zip` archive (unmarshal).

**Name:** `zipFile` · **Also:** `zipfile` · **Crate:** `cameleer-dataformats` (replaces the
single-entry version in `cameleer-core` when registered, as `cameleer_main::new_main` does) ·
**Apache Camel:** [Zip File](https://camel.apache.org/components/next/dataformats/zipFile-dataformat.html)

## When to use it

- Zip a file before uploading it to S3, FTP or email.
- Read the files inside an uploaded `.zip`, one by one, and route each entry.
- Bundle several generated documents (reports, invoices) into one archive.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | any body: one entry named after the `CamelFileName` header (default: the exchange id). A map body makes one entry per key (name -> content). | zip bytes. `CamelFileName` gets a `.zip` suffix. |
| `unmarshal` | zip bytes with one entry | the entry content as bytes; `CamelFileName` is set to the entry name. An archive with more than one entry fails unless `usingIterator` is set. |
| `unmarshal` with `usingIterator: true` | zip bytes | a list with the content of every entry; the names are in the `CamelZipFileEntryNames` header |

Directory entries are skipped unless `allowEmptyDirectory: true`. Entry names keep only the file
name unless `preservePathElements: true`.

## Example

Zip a report and unzip it again. The file name travels in `CamelFileName`:

<!-- example: zipfile-round-trip -->
```yaml
- route:
    id: zipfile-round-trip
    from:
      uri: direct:zipfile-round-trip
      steps:
        - setHeader:
            name: CamelFileName
            constant: report.txt
        - marshal:
            zipFile: {}
        - setHeader:
            name: archive
            simple: "${header.CamelFileName}"
        - unmarshal:
            zipFile: {}
        - convertBodyTo:
            type: String
        - setBody:
            simple: "${header.archive} holds ${header.CamelFileName}: ${body}"
```

<!-- input: zipfile-round-trip -->
```text
Q3 revenue up 12%
```

<!-- output: zipfile-round-trip -->
```text
report.txt.zip holds report.txt: Q3 revenue up 12%
```

Build a multi-entry archive from a map, then list the entries with `usingIterator`:

<!-- example: zipfile-multiple-entries -->
```yaml
- route:
    id: zipfile-entries
    from:
      uri: direct:zipfile-entries
      steps:
        - unmarshal:
            json: {}
        - marshal:
            zipFile: {}
        - unmarshal:
            zipFile:
              usingIterator: true
        - setBody:
            simple: "${header.CamelZipFileEntryNames}"
```

<!-- input: zipfile-multiple-entries -->
```json
{"invoice-1.txt":"total 10","invoice-2.txt":"total 20"}
```

<!-- output: zipfile-multiple-entries -->
```json
["invoice-1.txt","invoice-2.txt"]
```

To route each entry on its own, follow the `usingIterator` unmarshal with a `split` over `${body}`.

## Rust DSL

```rust
b.from("direct:zipfile-rust")
    .set_header("CamelFileName", constant("data.csv"))
    .marshal(DataFormatDefinition::zip_file());
```

## Options

| option | default | description |
|---|---|---|
| `usingIterator` | `false` | Unmarshal all entries into a list (names in `CamelZipFileEntryNames`). |
| `allowEmptyDirectory` | `false` | Include directory entries when iterating. |
| `preservePathElements` | `false` | Keep directories in entry names (`a/b/c.txt`) instead of only `c.txt`. |
| `compressionLevel` (`level`) | `6` | `0` stores entries uncompressed; `1`-`9` compress. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelFileName` | in / out | Marshal: the entry name (and the archive name plus `.zip`). Unmarshal: set to the entry name. |
| `CamelZipFileEntryNames` | out | Unmarshal with `usingIterator`: the entry names, in archive order. |

## Common questions

### How do I unzip a file with several entries?

Use `unmarshal: {zipFile: {usingIterator: true}}` and then `split: {simple: "${body}"}`. The
names are in the `CamelZipFileEntryNames` header.

### How do I zip several files into one archive?

Put a map of `name -> content` in the body (for example by aggregating files into a map) and
`marshal: {zipFile: {}}`.

### Is this the same as zipDeflater?

No. [`zipDeflater`](zip-deflater.md) compresses one payload without any archive structure.

## Related

- [Tar File data format](tarfile.md), [GZip data format](gzip.md),
  [Zip Deflater data format](zip-deflater.md)
- [Apache Camel Zip File data format](https://camel.apache.org/components/next/dataformats/zipFile-dataformat.html)
