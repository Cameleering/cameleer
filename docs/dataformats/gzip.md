# GZip data format

> The `gzipDeflater` data format compresses the message body to gzip (marshal) and decompresses gzip data (unmarshal).

**Name:** `gzipDeflater` · **Also:** `gzip`, `gzipdeflater` · **Crate:** `cameleer-core` ·
**Apache Camel:** [GZip Deflater](https://camel.apache.org/components/next/dataformats/gzipDeflater-dataformat.html)

## When to use it

- Shrink payloads before sending them over slow or metered links, for example from an edge
  gateway to the cloud.
- Read `.gz` files, or gzip-compressed messages from Kafka, S3 or HTTP.
- Archive logs and exports in a format every tool understands (`gunzip`, `zcat`).

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | any body (text is taken as UTF-8 bytes) | gzip bytes (RFC 1952) |
| `unmarshal` | gzip bytes | the decompressed bytes |

Compression uses Cameleer's built-in DEFLATE implementation (no zlib library). Decompression reads
every DEFLATE block type, so files written by `gzip`, Java or Python decompress fine.

## Example

Compress a body, record the compressed size, and decompress it again:

<!-- example: gzip-round-trip -->
```yaml
- route:
    id: gzip-round-trip
    from:
      uri: direct:gzip-round-trip
      steps:
        - marshal:
            gzipDeflater: {}
        - setHeader:
            name: compressedSize
            simple: "${body.length()}"
        - unmarshal:
            gzip: {}
        - convertBodyTo:
            type: String
        - setBody:
            simple: "${body} (${header.compressedSize} bytes compressed)"
```

<!-- input: gzip-round-trip -->
```text
sensor=42;sensor=42;sensor=42;sensor=42;sensor=42;sensor=42;sensor=42;sensor=42
```

<!-- output: gzip-round-trip -->
```text
sensor=42;sensor=42;sensor=42;sensor=42;sensor=42;sensor=42;sensor=42;sensor=42 (32 bytes compressed)
```

The 79-byte body compresses to 32 bytes, including the 18 bytes of gzip header and trailer.

## Rust DSL

```rust
b.from("direct:gzip-rust")
    .marshal(DataFormatDefinition::gzip().compression_level(9))
    .unmarshal(DataFormatDefinition::gzip());
```

## Options

| option | default | description |
|---|---|---|
| `compressionLevel` (`level`) | `6` | `0` stores the data uncompressed; `1`-`9` compress (higher values search harder). |

## Common questions

### How do I read a .gz file?

Read it with the `file` component and `unmarshal: {gzip: {}}`. Add `convertBodyTo: {type: String}`
if the content is text.

### What is the difference between gzip and zipDeflater?

Both use DEFLATE. `gzipDeflater` writes the gzip container (`.gz` files, HTTP
`Content-Encoding: gzip`); [`zipDeflater`](zip-deflater.md) writes the zlib container
(`java.util.zip.Deflater`). For `.zip` archives use [`zipFile`](zipfile.md).

### Does compression make sense for small messages?

Not below about 100 bytes: the gzip header and trailer alone take 18 bytes.

## Related

- [Zip Deflater data format](zip-deflater.md), [Zip File data format](zipfile.md),
  [Tar File data format](tarfile.md)
- [Base64 data format](base64.md) to carry compressed data in text
- [Apache Camel GZip Deflater data format](https://camel.apache.org/components/next/dataformats/gzipDeflater-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
