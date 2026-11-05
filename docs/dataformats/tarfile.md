# Tar file data format

> The `tarFile` data format packs a message body into a tar archive and unpacks tar archives back into one body or a list of entries.

**Name:** `tarFile` · **Also:** `tar`, `tarfile` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [Tar File](https://camel.apache.org/components/next/dataformats/tarFile-dataformat.html)

## When to use it

- Bundle the files a route produces (reports, logs, model outputs) into one `.tar` before
  uploading them to S3, SFTP or an artifact store.
- Unpack a tar archive that arrives by file, FTP or HTTP and process each entry.
- Ship a batch of sensor readings from an edge device as one archive instead of many small
  files.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| marshal | text or bytes: one entry named after the `CamelFileName` header (the exchange id when the header is missing) | tar archive bytes; `CamelFileName` gets a `.tar` suffix |
| marshal | an object `{name: content, ...}` | tar archive with one entry per key |
| unmarshal | tar archive bytes | the single entry's content as bytes, its name in `CamelFileName` (an archive with more than one file fails with `IllegalStateException`, as in Camel) |
| unmarshal with `usingIterator=true` | tar archive bytes | an array with the content of every entry, the names in the `CamelTarFileEntryNames` header (same order) |

The archive is POSIX ustar with pax extended headers for long names; GNU long names are read
too. Entry names keep only the file name unless `preservePathElements=true`.

## Example

Pack one file and unpack it again. In a real route the archive bytes would go to a file,
S3 or SFTP endpoint between the two steps.

<!-- example: tarfile-single-entry -->
```yaml
- route:
    id: tar-single
    from:
      uri: direct:tar-single
      steps:
        - setHeader:
            name: CamelFileName
            constant: reports/daily.txt
        - marshal:
            tarFile: {}
        - setHeader:
            name: archiveName
            simple: "${header.CamelFileName}"
        - unmarshal:
            tarFile: {}
        - setBody:
            simple: "${header.archiveName} contained ${header.CamelFileName}: ${bodyAs(String)}"
```

<!-- input: tarfile-single-entry -->
```text
all systems nominal
```

<!-- output: tarfile-single-entry -->
```text
reports/daily.txt.tar contained daily.txt: all systems nominal
```

Several entries: an object body becomes one entry per key, and `usingIterator=true` gives
every entry back with its name. The route splits the entries and joins one line per file.

<!-- example: tarfile-many-entries -->
```yaml
- route:
    id: tar-many
    from:
      uri: direct:tar-many
      steps:
        - unmarshal:
            json: {}
        - marshal:
            tarFile: {}
        - unmarshal:
            tarFile:
              usingIterator: true
        - setHeader:
            name: names
            simple: "${header.CamelTarFileEntryNames}"
        - setBody:
            simple: "${header.names} -> ${body[0]} / ${body[1]}"
        - convertBodyTo: String
```

<!-- input: tarfile-many-entries -->
```json
{"a.csv": "id,qty\n1,2", "b.csv": "id,qty\n3,4"}
```

<!-- output: tarfile-many-entries -->
```text
["a.csv","b.csv"] -> id,qty
1,2 / id,qty
3,4
```

## Rust DSL

```rust
b.from("direct:tar-rust")
    .set_header("CamelFileName", constant("notes.txt"))
    .marshal(DataFormatDefinition::new("tarFile"))
    .unmarshal(DataFormatDefinition::new("tarFile"))
    .convert_body_to("String");
```

## Options

| option | default | description |
|---|---|---|
| `usingIterator` | `false` | Unmarshal every entry into an array (names in `CamelTarFileEntryNames`) instead of a single body. |
| `allowEmptyDirectory` | `false` | With `usingIterator`, include directory entries as empty bodies. |
| `preservePathElements` | `false` | Keep the directory part of `CamelFileName` (and object keys) as the entry name. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelFileName` | in (marshal) / out | Entry name on marshal; `.tar` is appended to it. Set to the entry name on unmarshal. |
| `CamelTarFileEntryNames` | out (unmarshal) | Entry names when `usingIterator=true`. |

## Common questions

### How do I tar several files into one archive?

Build an object body whose keys are file names and values are contents (for example with
`unmarshal: json` or an aggregation), then `marshal: tarFile`. Each key becomes an entry.

### How do I process every file in a tar archive?

Use `unmarshal: tarFile: {usingIterator: true}` and then `split: simple: "${body}"`; the
names are in the `CamelTarFileEntryNames` header at the same index.

### Does it compress?

No. Tar only bundles. Add `marshal: gzipDeflater` after it for a `.tar.gz`, and
`unmarshal: gzipDeflater` before unpacking one.

## Related

- [Zip file data format](zipfile.md), [GZip data format](gzip.md)
- [Split EIP](../guides/eips.md)
- [Apache Camel Tar File data format](https://camel.apache.org/components/next/dataformats/tarFile-dataformat.html)
