# Crypto data format

> The `crypto` data format encrypts a message body with a symmetric key (AES, DES or Triple DES) on marshal and decrypts it on unmarshal, with an optional HMAC to detect tampering.

**Name:** `crypto` · **Crate:** `cameleer-dataformats` · **Cargo feature:** [`dataformats`](../guides/build-features.md) ·
**Apache Camel:** [Crypto (Java Cryptographic Extension)](https://camel.apache.org/components/next/dataformats/crypto-dataformat.html)

## When to use it

- Encrypt payloads on an edge device before they leave it (to a broker, a bucket or a
  gateway you do not fully trust), and decrypt them in the back end with the same key.
- Store sensitive message bodies (prompts, transcripts, personal data) encrypted at rest.
- Exchange data with a Java Camel application that uses `CryptoDataFormat`: the wire format
  (inline IV and appended HMAC) is the same.

For public-key encryption and signatures use the [PGP data format](pgp.md) instead.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| marshal | text or bytes | ciphertext bytes: `[4-byte IV length + IV]` when `inline=true`, then the ciphertext, then the HMAC when `shouldAppendHMAC=true` |
| unmarshal | ciphertext bytes in the same layout | plaintext bytes |

Without an IV (`initVector`, `initVectorRef` or the `CamelCryptoInitVector` header) a random
IV is generated for each message; decrypting it then needs `inline=true` so the IV travels
with the data. `AES/GCM/NoPadding` uses a 12-byte IV and appends a 16-byte tag.

## Example

AES-256-GCM with a random IV carried inline. The ciphertext is base64-encoded so it can be
printed or put into JSON; the second half of the route reverses everything.

<!-- example: crypto-aes-gcm-round-trip -->
```yaml
- route:
    id: crypto-gcm
    from:
      uri: direct:crypto-gcm
      steps:
        - marshal:
            crypto:
              algorithm: AES/GCM/NoPadding
              key: "{{env:PAYLOAD_KEY:hex:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f}}"
              inline: true
        - marshal:
            base64:
              lineLength: 0
        - setHeader:
            name: encrypted
            simple: "${body}"
        - unmarshal:
            base64: {}
        - unmarshal:
            crypto:
              algorithm: AES/GCM/NoPadding
              key: "{{env:PAYLOAD_KEY:hex:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f}}"
              inline: true
        - convertBodyTo: String
```

<!-- input: crypto-aes-gcm-round-trip -->
```text
patient 4711: blood pressure 120/80
```

<!-- output: crypto-aes-gcm-round-trip -->
```text
patient 4711: blood pressure 120/80
```

With a fixed IV and no HMAC the output is deterministic, which makes it easy to check against
another implementation (here AES-128-CBC with PKCS#5 padding, the same as
`openssl enc -aes-128-cbc -K 2b7e151628aed2a6abf7158809cf4f3c -iv 000102030405060708090a0b0c0d0e0f`).
Use a random IV in production.

<!-- example: crypto-aes-cbc-fixed-iv -->
```yaml
- route:
    id: crypto-cbc
    from:
      uri: direct:crypto-cbc
      steps:
        - marshal:
            crypto:
              algorithm: AES/CBC/PKCS5Padding
              key: hex:2b7e151628aed2a6abf7158809cf4f3c
              initVector: hex:000102030405060708090a0b0c0d0e0f
              shouldAppendHMAC: false
        - marshal:
            base64:
              lineLength: 0
```

<!-- input: crypto-aes-cbc-fixed-iv -->
```text
hello edge
```

<!-- output: crypto-aes-cbc-fixed-iv -->
```text
nplqU6Egyd3gr5/mxqpKZg==
```

## Rust DSL

```rust
let aes = DataFormatDefinition::new("crypto")
    .option("algorithm", "AES/GCM/NoPadding")
    .option("key", "hex:000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
    .option("inline", "true");
b.from("direct:crypto-rust")
    .marshal(aes.clone())
    .unmarshal(aes)
    .convert_body_to("String");
```

## Options

| option | default | description |
|---|---|---|
| `algorithm` | `DES/CBC/PKCS5Padding` | `AES`, `DES` or `DESede` (`TripleDES`), with mode `ECB`, `CBC`, `CTR`, `CFB`, `OFB` or `GCM` (AES only) and padding `PKCS5Padding`, `PKCS7Padding` or `NoPadding`. A bare `AES` means `AES/ECB/PKCS5Padding`, as in the JCE. |
| `key` | (none) | The key inline: `hex:...`, `base64:...` or raw text. AES keys are 16, 24 or 32 bytes. |
| `keyRef` | (none) | Registry bean holding the key (bytes, or a string in the `key` syntax). |
| `initVector` | (none) | The IV inline, same syntax as `key`. Alias `initializationVector`. |
| `initVectorRef` | (none) | Registry bean holding the IV. |
| `inline` | `false` | Write the IV in front of the ciphertext (4-byte big-endian length, then the IV) and read it from there on unmarshal. |
| `shouldAppendHMAC` | `true` | Append `HMAC(key, ciphertext)` on marshal and verify it on unmarshal. |
| `macAlgorithm` | `HmacSHA1` | `HmacSHA1`, `HmacSHA256`, `HmacSHA384` or `HmacSHA512`. |

`buffersize`, `cryptoProvider` and `algorithmParameterRef` are accepted for compatibility and
ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelCryptoKey` | in | Key for this message (overrides `key` / `keyRef`). |
| `CamelCryptoInitVector` | in | IV for this message (overrides `initVector` / `initVectorRef`). |

## Common questions

### How do I encrypt a message with AES in a Camel route?

`marshal: crypto: {algorithm: AES/GCM/NoPadding, key: "hex:...", inline: true}`. Decrypt
with the same options under `unmarshal`. Keep the key in an environment variable through a
`{{env:NAME}}` placeholder rather than in the route file.

### Why does decryption fail with "An initialization vector is required"?

The message was encrypted with a random IV that was not stored. Use `inline: true` on both
sides, or give the same `initVector` to both.

### What does "Input stream was tampered with" mean?

The appended HMAC does not match: the data was changed, the key differs, or one side has
`shouldAppendHMAC: false`. Both sides must use the same HMAC settings.

### Is it compatible with Apache Camel's CryptoDataFormat?

Yes for the supported algorithms: the inline IV layout and the appended HMAC follow Camel.
Algorithms outside AES, DES and Triple DES (for example Blowfish) are not implemented.

## Related

- [PGP data format](pgp.md): public-key encryption and signing
- [Base64 data format](base64.md): to carry ciphertext in text formats
- [Apache Camel Crypto data format](https://camel.apache.org/components/next/dataformats/crypto-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
