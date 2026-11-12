# XML Security Sign component

> The `xmlsecurity-sign` component adds an XML Signature (XMLDSig, RSA-SHA256 or ECDSA-SHA256) to the XML message body, enveloped, enveloping or detached.

**Scheme:** `xmlsecurity-sign:` · **Crate:** `cameleer-xml` · **Cargo feature:** [`xml`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [XML Security Sign](https://camel.apache.org/components/next/xmlsecurity-sign-component.html)

## When to use it

- Sign XML documents sent to partners, banks or authorities that require XMLDSig (invoices,
  payment files, SAML-style assertions).
- Make an edge device's XML reports tamper-evident before they leave the site.
- Prove that an audit record produced by an automated agent was not changed afterwards.

Verify signatures with [xmlsecurity-verify](xmlsecurity-verify.md).

## URI format

```text
xmlsecurity-sign:name[?options]
```

`name` is a free label. The key comes from `privateKey` (PEM) or a `keyStore`.

## Example

The keys below were generated for this page only (an EC P-256 key pair). Never use them for
real data; create your own with
`openssl ecparam -name prime256v1 -genkey -noout | openssl pkcs8 -topk8 -nocrypt`.

<!-- file: keys/docs-sign.pem -->
```text
-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgr/S1wGY1NANslHGn
cIOGLnu4z9xeORbVQWm3MG3pzSKhRANCAARxi5ySY82uB37ucvRnkz5+fB8LfsKx
7L+7rqAlSeMVXVNAj0CN66ElvjSUk7CGLSLIVz/QY6wSzOkKjDGCDtII
-----END PRIVATE KEY-----
```

Sign an invoice; the signature is appended to the document element (enveloped):

<!-- example: xmlsecurity-sign-invoice -->
```yaml
- route:
    id: xmlsecurity-sign-invoice
    from:
      uri: direct:sign-invoice
      steps:
        - to: xmlsecurity-sign:invoice?privateKey=file:keys/docs-sign.pem&keyName=docs-key
```

<!-- input: xmlsecurity-sign-invoice -->
```xml
<invoice id="INV-1"><amount currency="EUR">99.00</amount></invoice>
```

<!-- output: xmlsecurity-sign-invoice {"contains":true} -->
```xml
<invoice id="INV-1"><amount currency="EUR">99.00</amount><ds:Signature xmlns:ds="http://www.w3.org/2000/09/xmldsig#">
<ds:CanonicalizationMethod Algorithm="http://www.w3.org/2001/10/xml-exc-c14n#"/>
<ds:SignatureMethod Algorithm="http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha256"/>
<ds:Transform Algorithm="http://www.w3.org/2000/09/xmldsig#enveloped-signature"/>
<ds:DigestMethod Algorithm="http://www.w3.org/2001/04/xmlenc#sha256"/>
<ds:KeyName>docs-key</ds:KeyName>
</ds:Signature></invoice>
```

The digest and signature values differ on every run (ECDSA signatures are randomized), so
they are not shown. The [xmlsecurity-verify](xmlsecurity-verify.md) page signs and verifies
this document in one route.

## Rust DSL

```rust
b.from("direct:xmlsecurity-sign-rust")
    .to("xmlsecurity-sign:invoice?privateKey=file:keys/docs-sign.pem&addKeyValue=true");
```

## Options

| option | default | description |
|---|---|---|
| `privateKey` | | Signing key: a PEM file (`file:path` or plain path), `ref:bean` holding PEM text, or inline PEM. PKCS#8, PKCS#1 (RSA) or SEC1 (EC). |
| `certificate` | | PEM certificate added as `ds:X509Certificate` in `ds:KeyInfo`. |
| `keyStore` | | PKCS#12, JKS or JCEKS file, or `#bean` with a key store (alias names `keyAccessor`, `keySelector`). |
| `keyStorePassword`, `keyPassword`, `alias` | | Key store access and the key entry to use. |
| `addKeyValue` | `false` | Add `ds:KeyValue` (`RSAKeyValue` or `dsig11:ECKeyValue`) with the public key. |
| `keyName` | | Add `ds:KeyName`. |
| `signatureId` | | `Id` attribute of `ds:Signature`. |
| `parentLocalName`, `parentNamespace` | | Enveloped: append the signature to this element. |
| `parentXpath` | | Enveloped: append the signature to the element selected by this XPath. |
| `xpathsToIdAttributes` | | Detached: comma-separated XPaths selecting `Id` attributes; one reference per element. |
| `contentObjectId`, `envelopingSignature` | `false` | Enveloping: the body becomes a `ds:Object` inside `ds:Signature`. |
| `xpathFilter` | | XPath Filter transform expression; namespaces from `xpathNamespaces=p=uri,...`. |
| `xpath2Filter` | | XPath Filter 2.0 transforms: `intersect:expr;subtract:expr;union:expr`. |
| `signatureAlgorithm` | from the key | `http://www.w3.org/2001/04/xmldsig-more#rsa-sha256` or `...#ecdsa-sha256`; others are rejected. |
| `digestAlgorithm` | SHA-256 | Only `http://www.w3.org/2001/04/xmlenc#sha256`. |
| `canonicalizationMethod` | exclusive c14n | Exclusive or inclusive canonicalization. |
| `prefixForXmlSignatureNamespace` | `ds` | Prefix of the signature elements; empty for none. |
| `omitXmlDeclaration` | `false` | Leave out the XML declaration in the output. |

Without `parentLocalName`, `parentXpath`, `xpathsToIdAttributes` or enveloping options, the
signature is enveloped into the document element. Accepted and ignored: `transformMethods`,
`contentReferenceUri`, `contentReferenceType`, `addKeyInfoReference`, `clearHeaders`,
`outputXmlEncoding`, `baseUri`, `cryptoContextProperties`, `disallowDoctypeDecl`,
`schemaResourceUri`, `plainText`, `plainTextEncoding` and a few other Java-only settings.

**Differences from Apache Camel:** only RSA-SHA256 and ECDSA-SHA256 (P-256) signatures and
SHA-256 digests; no `ds:Manifest`, no external references, no XSLT transforms.

## Message headers

None.

## Run it with Docker

```sh
docker run --rm -v "$PWD:/routes:ro" cameleering/cameleer run /routes/sign.camel.yaml
```

Mount the key with the routes (read-only), or point `privateKey` at a secret mounted by your
orchestrator.

## Common questions

### How do I sign with an RSA key and a certificate?

Set `privateKey=file:key.pem&certificate=file:cert.pem`; the algorithm follows the key type
(RSA-SHA256), and the certificate goes into `ds:KeyInfo` for the receiver.

### How do I sign only part of a document?

Use `xpathsToIdAttributes` for a detached signature over elements with an `Id` attribute, or
an `xpathFilter` / `xpath2Filter` transform.

### Where should the private key live in production?

Outside the route file: a mounted secret file, or a key store with its password from
`{{env:KEYSTORE_PASSWORD}}`.

## Related

- [xmlsecurity-verify](xmlsecurity-verify.md).
- [Crypto data format](../dataformats/crypto.md), [PGP data format](../dataformats/pgp.md).
- [Apache Camel XML Security Sign component](https://camel.apache.org/components/next/xmlsecurity-sign-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
