# XML Security Verify component

> The `xmlsecurity-verify` component checks the XML Signature in the message body against a trusted key and, by default, removes the signature; a bad signature fails the exchange.

**Scheme:** `xmlsecurity-verify:` · **Crate:** `cameleer-xml` · **Cargo feature:** [`xml`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [XML Security Verify](https://camel.apache.org/components/next/xmlsecurity-verify-component.html)

## When to use it

- Accept signed XML from partners (invoices, payment files, assertions) only when the signature
  is valid and made with their key.
- Detect tampering of XML reports sent by edge devices before they are processed.
- Check signed configuration or policy files before an automated agent applies them.

Create signatures with [xmlsecurity-sign](xmlsecurity-sign.md).

## URI format

```text
xmlsecurity-verify:name[?options]
```

`name` is a free label. The trusted key comes from `publicKey`, `certificate` or `keyStore`.

## Example

The key pair below was generated for these pages only; never use it for real data.

<!-- file: keys/docs-verify.pem -->
```text
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgr/S1wGY1NANslHGn
cIOGLnu4z9xeORbVQWm3MG3pzSKhRANCAARxi5ySY82uB37ucvRnkz5+fB8LfsKx
7L+7rqAlSeMVXVNAj0CN66ElvjSUk7CGLSLIVz/QY6wSzOkKjDGCDtII
-----END PRIVATE KEY-----
```

<!-- file: keys/docs-verify.pub.pem -->
```text
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEcYuckmPNrgd+7nL0Z5M+fnwfC37C
sey/u66gJUnjFV1TQI9AjeuhJb40lJOwhi0iyFc/0GOsEszpCowxgg7SCA==
-----END PUBLIC KEY-----
```

Sign and verify in one route (a round trip). In practice the two steps run in different
systems: the sender signs with the private key, the receiver verifies with the public key.

<!-- example: xmlsecurity-verify-round-trip -->
```yaml
- route:
    id: xmlsecurity-round-trip
    from:
      uri: direct:xmlsecurity-round-trip
      steps:
        - to: xmlsecurity-sign:report?privateKey=file:keys/docs-verify.pem
        - to: xmlsecurity-verify:report?publicKey=file:keys/docs-verify.pub.pem
```

<!-- input: xmlsecurity-verify-round-trip -->
```xml
<report site="plant-7"><temp>21.5</temp></report>
```

The verified document comes back without the signature (and with an XML declaration):

<!-- output: xmlsecurity-verify-round-trip -->
```xml
<?xml version="1.0" encoding="UTF-8"?><report site="plant-7"><temp>21.5</temp></report>
```

### Tampered documents fail

Here the document is changed after signing; verification fails with
`XmlSignatureInvalidContentHashException` (the reference digest no longer matches), which the
route turns into a rejection message.

<!-- example: xmlsecurity-verify-tampered -->
```yaml
- route:
    id: xmlsecurity-tampered
    from:
      uri: direct:xmlsecurity-tampered
      steps:
        - to: xmlsecurity-sign:report?privateKey=file:keys/docs-verify.pem
        - setBody:
            simple: "${body.replace('21.5','99.9')}"
        - doTry:
            steps:
              - to: xmlsecurity-verify:report?publicKey=file:keys/docs-verify.pub.pem
            doCatch:
              - exception:
                  - XmlSignatureInvalidException
                steps:
                  - setBody:
                      simple: "rejected: ${exception.class}"
```

<!-- input: xmlsecurity-verify-tampered -->
```xml
<report site="plant-7"><temp>21.5</temp></report>
```

<!-- output: xmlsecurity-verify-tampered -->
```text
rejected: XmlSignatureInvalidContentHashException
```

## Rust DSL

```rust
b.from("direct:xmlsecurity-verify-rust")
    .to("xmlsecurity-sign:report?privateKey=file:keys/docs-verify.pem")
    .to("xmlsecurity-verify:report?publicKey=file:keys/docs-verify.pub.pem&removeSignature=false");
```

## Options

| option | default | description |
|---|---|---|
| `publicKey` | | Trusted public key (PEM file, `ref:` bean or inline PEM). |
| `certificate` | | Trusted certificate (PEM). |
| `keyStore` | | Key store with trusted certificates (also `keyAccessor`, `keySelector`); with `keyStorePassword` and `alias`. The signature's `X509Certificate` must be one of the store's certificates, or its `KeyName` an alias; otherwise every key in the store is tried. |
| `trustKeyInfoCertificate` | `false` | Trust the `ds:X509Certificate` or `ds:KeyValue` inside the signature itself. Only for tests or with out-of-band trust. |
| `removeSignature` | `true` | Remove `ds:Signature` from the body after verification; for an enveloping signature the body becomes the content of its `ds:Object`. |
## Exceptions

| exception | when |
|---|---|
| `XmlSignatureFormatException` | No signature, or a malformed one. |
| `XmlSignatureInvalidContentHashException` | A reference digest does not match: the content was changed. |
| `XmlSignatureInvalidValueException` | The signature value does not verify with the key. |
| `XmlSignatureInvalidKeyException` | No trusted key for the signature. |

All of them are `XmlSignatureInvalidException`s (except the format error), so one `doCatch` or
`onException` covers tampering and wrong keys.

## Message headers

None.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/verify.camel.yaml
```

## Common questions

### How do I keep the signature in the verified document?

Set `removeSignature=false`.

### Which key should the receiver configure?

Only the sender's public key or certificate (`publicKey` or `certificate`), never the private
key. Use `trustKeyInfoCertificate=true` only in tests: it accepts any key the document brings.

### Which algorithms are supported?

RSA-SHA256 and ECDSA-SHA256 (P-256) signatures with SHA-256 digests; SHA-1 and SHA-512 digests
are accepted when verifying. `ds:Manifest`, external references and XSLT transforms are not
supported.

## Related

- [xmlsecurity-sign](xmlsecurity-sign.md), [validator](validator.md).
- [Apache Camel XML Security Verify component](https://camel.apache.org/components/next/xmlsecurity-verify-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
