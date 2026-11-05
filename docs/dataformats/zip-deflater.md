# Zip Deflater data format

> The `zipDeflater` data format compresses the message body with zlib/DEFLATE (marshal) and decompresses it (unmarshal), like Java's `java.util.zip.Deflater`.

**Name:** `zipDeflater` · **Also:** `zip`, `deflate`, `zlib`, `zipdeflater` · **Crate:** `cameleer-core` ·
**Apache Camel:** [Zip Deflater](https://camel.apache.org/components/next/dataformats/zipDeflater-dataformat.html)

## When to use it

- Exchange compressed messages with Java Camel routes that use `zipDeflater`.
- Compress payloads stored in a database column, a cache or a queue where the zlib format is
  expected (`zlib.decompress` in Python, `Inflater` in Java).
- Cut bandwidth on edge links when both ends are under your control.

## Marshal and unmarshal

| direction | body in | body out |
|---|---|---|
| `marshal` | any body (text is taken as UTF-8 bytes) | zlib bytes (RFC 1950: 2-byte header, DEFLATE data, Adler-32 checksum) |
| `unmarshal` | zlib bytes | the decompressed bytes |

This is not a `.zip` archive: for archives with file entries use [`zipFile`](zipfile.md).

## Example

Compress a body and show it as Base64, so it fits in a JSON field:

<!-- example: zip-deflater-compress -->
```yaml
- route:
    id: zlib-compress
    from:
      uri: direct:zlib-compress
      steps:
        - marshal:
            zipDeflater: {}
        - marshal:
            base64: {}
```

<!-- input: zip-deflater-compress -->
```text
hello hello hello hello
```

<!-- output: zip-deflater-compress -->
```text
eJzLSM3JyVfAIAFoAwix
```

And decompress it again:

<!-- example: zip-deflater-decompress -->
```yaml
- route:
    id: zlib-decompress
    from:
      uri: direct:zlib-decompress
      steps:
        - unmarshal:
            base64: {}
        - unmarshal:
            zipDeflater: {}
        - convertBodyTo:
            type: String
```

<!-- input: zip-deflater-decompress -->
```text
eJzLSM3JyVfAIAFoAwix
```

<!-- output: zip-deflater-decompress -->
```text
hello hello hello hello
```

## Rust DSL

```rust
b.from("direct:zlib-rust")
    .marshal(DataFormatDefinition::zip_deflater())
    .unmarshal(DataFormatDefinition::zip_deflater());
```

## Options

| option | default | description |
|---|---|---|
| `compressionLevel` (`level`) | `6` | `0` stores the data uncompressed; `1`-`9` compress. |

## Common questions

### Is the output compatible with Python's zlib module?

Yes. `zlib.decompress(data)` reads it, and `zlib.compress(...)` output unmarshals here.

### Why is the name zip if it does not make a .zip file?

Apache Camel named it after `java.util.zip.Deflater`. Use [`zipFile`](zipfile.md) for archives.

## Related

- [GZip data format](gzip.md), [Zip File data format](zipfile.md)
- [Apache Camel Zip Deflater data format](https://camel.apache.org/components/next/dataformats/zipDeflater-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
