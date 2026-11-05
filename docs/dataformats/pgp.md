# PGP data format

> The `pgp` data format encrypts and signs message bodies with OpenPGP public keys on marshal, and decrypts and verifies them on unmarshal, compatible with GnuPG.

**Name:** `pgp` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [PGP](https://camel.apache.org/components/next/dataformats/pgp-dataformat.html)

## When to use it

- Send files to a partner who publishes a PGP public key (banks, payroll and EDI exchanges
  often require it), and decrypt the files they send you.
- Encrypt data on an edge device with only the public key on the device: a stolen device
  cannot read what it already sent.
- Sign messages so the receiver can verify who produced them, for example reports generated
  by an AI agent that a human approves downstream.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| marshal | text or bytes | an OpenPGP message (binary, or ASCII armored with `armored=true`), encrypted to every key selected by `keyUserid(s)`; signed too when a signature key is configured |
| unmarshal | an OpenPGP message (binary or armored) | the decrypted literal data as bytes; signatures are verified as `signatureVerificationOption` says |

Supported: v4 keys with RSA, Ed25519 (EdDSA), ECDSA and ECDH (Curve25519, P-256, P-384);
armored or binary keyrings with several keys; secret keys protected with S2K and AES or
Triple DES; SEIPD with MDC (`integrity=true`) or legacy SED; AES-128/192/256 and Triple DES
session keys; ZIP, ZLIB or no compression; one-pass signatures with SHA-1/256/384/512; and
decryption of GnuPG's OCB packets.

Not supported (differences from Apache Camel's Bouncy Castle based format): CAST5, IDEA,
Blowfish, Twofish and Camellia ciphers, BZip2, v5/v6 keys, AEAD encryption, password-only
(symmetric) messages, cleartext signatures, DSA and ElGamal keys.

## Example

The keys below were generated for this page only (user id `Docs Example <docs@example.com>`,
passphrase `docs-only`). Never use them for real data; export your own with
`gpg --armor --export` and `gpg --armor --export-secret-keys`.

<!-- file: keys/docs-pub.asc -->
```text
-----BEGIN PGP PUBLIC KEY BLOCK-----
xjMEauxXFBYJKwYBBAHaRw8BAQdA1ourTLIXH/gvewCFoEk+Zeluy9g5QtTUD2d7
jTQZnkvNH0RvY3MgRXhhbXBsZSA8ZG9jc0BleGFtcGxlLmNvbT7CiQQTFggAMQUC
auxXUBYhBC5W5U4q6l3GlHxQWpoY/ccB7VZSAhsDBAsJCAcDFQoIBBYCAQACHgEA
CgkQmhj9xwHtVlLN+QD/S6dEFukTVVGHktFz54g8umX64hMm3VaoLCRm6IKCSmIB
ANsjzFVSfydkrSeXqY0hSNi90YGGcVsLUHXlGoNCMp8JzjgEauxXFBIKKwYBBAGX
VQEFAQEHQNeft5uDzzZM0ty6J/8WaidH2WFLtu0bYhwXrKeyVIYbAwEIB8J4BBgW
CAAgBQJq7FdQFiEELlblTirqXcaUfFBamhj9xwHtVlICGwwACgkQmhj9xwHtVlJH
twEAqGFdYQ0F93Tf2etQ+eaRDzDqjv81PlMM10KQtA4PbrEA/jUbGyTjVX4nGrOd
q8mqcquoB/xaXFevJET2PddE/AgH
=cSUI
-----END PGP PUBLIC KEY BLOCK-----
```

<!-- file: keys/docs-sec.asc -->
```text
-----BEGIN PGP PRIVATE KEY BLOCK-----
xYYEauxXFBYJKwYBBAHaRw8BAQdA1ourTLIXH/gvewCFoEk+Zeluy9g5QtTUD2d7
jTQZnkv+CQMISJFaVaoY7xTAmcEZrgXrBWzxny3f4PtuP7AzqU2HesMC7VsHsu3u
Qqluxdr7M3wjuCAR60wBJb8JQykPK3CPENu1LXB0gXm2sqMlLwi4bs0fRG9jcyBF
eGFtcGxlIDxkb2NzQGV4YW1wbGUuY29tPsKJBBMWCAAxBQJq7FdQFiEELlblTirq
XcaUfFBamhj9xwHtVlICGwMECwkIBwMVCggEFgIBAAIeAQAKCRCaGP3HAe1WUs35
AP9Lp0QW6RNVUYeS0XPniDy6ZfriEybdVqgsJGbogoJKYgEA2yPMVVJ/J2StJ5ep
jSFI2L3RgYZxWwtQdeUag0IynwnHiwRq7FcUEgorBgEEAZdVAQUBAQdA15+3m4PP
NkzS3Lon/xZqJ0fZYUu27RtiHBesp7JUhhsDAQgH/gkDCEIXTf2HaNcOwJ3qRFDB
n0hyzw04BGCfHj6NRflCFOcQFVH8q/8vKHMfIhJEQzHbe8u42rCJxSU+vniy0YjA
qpgPuDT2C2ncvkjQp9me+5vCeAQYFggAIAUCauxXUBYhBC5W5U4q6l3GlHxQWpoY
/ccB7VZSAhsMAAoJEJoY/ccB7VZSR7cBAKhhXWENBfd039nrUPnmkQ8w6o7/NT5T
DNdCkLQOD26xAP41Gxsk41V+JxqznavJqnKrqAf8WlxXryRE9j3XRPwIBw==
=i1KD
-----END PGP PRIVATE KEY BLOCK-----
```

Encrypt to the public key as ASCII armor, then decrypt with the secret key. In practice the
two halves run in different places: the sender only needs `docs-pub.asc`.

<!-- example: pgp-encrypt-decrypt -->
```yaml
- route:
    id: pgp-round-trip
    from:
      uri: direct:pgp-round-trip
      steps:
        - marshal:
            pgp:
              keyFileName: keys/docs-pub.asc
              keyUserid: docs@example.com
              armored: true
        - setHeader:
            name: armoredStart
            simple: "${body.substring(0, 27)}"
        - unmarshal:
            pgp:
              keyFileName: keys/docs-sec.asc
              keyUserid: docs@example.com
              password: "{{env:PGP_PASSWORD:docs-only}}"
        - setBody:
            simple: "${header.armoredStart} -> ${bodyAs(String)}"
```

<!-- input: pgp-encrypt-decrypt -->
```text
quarterly payroll batch
```

<!-- output: pgp-encrypt-decrypt -->
```text
-----BEGIN PGP MESSAGE----- -> quarterly payroll batch
```

Sign and encrypt, then decrypt and require a valid signature. A message that is not signed
by an accepted key fails on unmarshal.

<!-- example: pgp-sign-and-verify -->
```yaml
- route:
    id: pgp-signed
    from:
      uri: direct:pgp-signed
      steps:
        - marshal:
            pgp:
              keyFileName: keys/docs-pub.asc
              keyUserid: docs@example.com
              signatureKeyFileName: keys/docs-sec.asc
              signatureKeyUserid: docs@example.com
              signaturePassword: "{{env:PGP_PASSWORD:docs-only}}"
        - setHeader:
            name: signers
            simple: "${header.CamelPGPDataFormatNumberOfSigningKeys}"
        - unmarshal:
            pgp:
              keyFileName: keys/docs-sec.asc
              password: "{{env:PGP_PASSWORD:docs-only}}"
              signatureKeyFileName: keys/docs-pub.asc
              signatureVerificationOption: required
        - setBody:
            simple: "signed by ${header.signers} key(s): ${bodyAs(String)}"
```

<!-- input: pgp-sign-and-verify -->
```text
approved by the review agent
```

<!-- output: pgp-sign-and-verify -->
```text
signed by 1 key(s): approved by the review agent
```

## Rust DSL

```rust
b.from("direct:pgp-rust")
    .marshal(DataFormatDefinition::new("pgp").option("keyFileName", "keys/docs-pub.asc").option("armored", "true"))
    .unmarshal(DataFormatDefinition::new("pgp").option("keyFileName", "keys/docs-sec.asc").option("password", "docs-only"))
    .convert_body_to("String");
```

## Options

| option | default | description |
|---|---|---|
| `keyFileName` | (none) | Keyring file: public keyring on marshal, secret keyring on unmarshal (armored or binary). |
| `encryptionKeyRing` | (none) | The keyring inline (armored text) or `#bean`, instead of `keyFileName`. Alias `keyring`. |
| `keyUserid` | (all keys) | Part of a user id selecting the encryption keys (marshal) or the decryption keys (unmarshal). |
| `keyUserids` | (none) | Several user id parts, comma separated, to encrypt to several recipients. |
| `password` | (none) | Passphrase of the secret key on unmarshal. |
| `armored` | `false` | Write ASCII armor instead of binary on marshal. |
| `integrity` | `true` | Use SEIPD with a modification detection code; `false` writes legacy SED packets. |
| `algorithm` | `9` (AES-256) | Session cipher: `9`/`AES256`, `8`/`AES192`, `7`/`AES128`, `2`/`TRIPLEDES`. |
| `compressionAlgorithm` | `1` (ZIP) | `1` ZIP, `2` ZLIB, `0` none. |
| `hashAlgorithm` | `8` (SHA-256) | Signature hash: `2` SHA-1, `8` SHA-256, `9` SHA-384, `10` SHA-512. |
| `fileName` | `CamelFileName` or `_CONSOLE` | Name stored in the literal data packet. |
| `signatureKeyFileName` | (none) | Secret keyring to sign with (marshal) or public keyring to verify with (unmarshal). |
| `signatureKeyRing` | (none) | The signature keyring inline or `#bean`. |
| `signatureKeyUserid` / `signatureKeyUserids` | (all keys) | User ids of the signing keys (marshal) or of the accepted signers (unmarshal). |
| `signaturePassword` | (none) | Passphrase of the signing key. |
| `signatureVerificationOption` | `optional` | `optional` (verify when signed), `required`, `no_signature_allowed`, `ignore`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelPGPDataFormatKeyUserid`, `CamelPGPDataFormatKeyUserids`, `CamelPGPDataFormatKeyPassword` | in | Per-message recipient selection and passphrase. |
| `CamelPGPDataFormatSignatureKeyUserid`, `CamelPGPDataFormatSignatureKeyUserids`, `CamelPGPDataFormatSignatureKeyPassword` | in | Per-message signer selection and passphrase. |
| `CamelPGPDataFormatKeyFileName`, `CamelPGPDataFormatSignatureKeyFileName`, `CamelPGPDataFormatEncryptionKeyRing`, `CamelPGPDataFormatSignatureKeyRing` | in | Per-message keyrings. |
| `CamelPGPDataFormatEncryptionAlgorithm`, `CamelPGPDataFormatSignatureAlgorithm`, `CamelPGPDataFormatCompressionAlgorithm` | in | Per-message algorithms. |
| `CamelPGPDataFormatNumberOfEncryptionKeys`, `CamelPGPDataFormatNumberOfSigningKeys` | out (marshal) | How many keys the message was encrypted to and signed with. |

## Common questions

### How do I decrypt a file encrypted with gpg?

Point `keyFileName` at your exported secret keyring and give the passphrase:
`unmarshal: pgp: {keyFileName: secring.asc, password: "{{env:PGP_PASSWORD}}"}`. Messages from
GnuPG 2.4 that use OCB packets are decrypted too.

### How do I encrypt for several recipients?

Put all public keys in one keyring and either omit `keyUserid` (every key is used) or list
them in `keyUserids: alice@example.com,bob@example.com`.

### Can I use the keys from Apache Camel's Bouncy Castle setup?

Yes, if they are RSA, Ed25519 or ECDSA/ECDH v4 keys exported as armored or binary keyrings.
DSA and ElGamal keys are not supported.

## Related

- [Crypto data format](crypto.md): symmetric encryption with a shared key
- [File component](../components/file.md), [SFTP component](../components/sftp.md)
- [Apache Camel PGP data format](https://camel.apache.org/components/next/dataformats/pgp-dataformat.html)
