# TLS and security: HTTPS, mutual TLS, trust stores and secrets

> Cameleer has its own TLS 1.3 and 1.2 implementation and is configured with Camel's names: `camel.server.ssl.*` gives the HTTP server a certificate, `camel.ssl.*` (with `camel.ssl.enabled=true`) sets the certificates and trust store for every TLS client endpoint, `sslContextParameters` sets them per endpoint, and `clientAuthentication=REQUIRE` turns on mutual TLS. Certificates are always verified unless you explicitly turn verification off for a test. Secrets come from environment variables or AWS, Azure and Google Cloud secret stores through placeholders.

**Applies to:** every TLS endpoint (HTTP, Kafka, AMQP, MQTT, SMTP/IMAP, FTPS, databases, cloud services) and the embedded HTTP server · **Apache Camel:** [Camel Configuration Utilities (JSSE)](https://camel.apache.org/manual/camel-configuration-utilities.html)

| you want | set |
|---|---|
| HTTPS for REST and `platform-http` routes | `camel.server.sslEnabled=true`, `camel.server.ssl.certFile`, `camel.server.ssl.keyFile` |
| a client endpoint to trust a private CA | `camel.ssl.enabled=true`, `camel.ssl.trustStore=ca.crt` |
| a client certificate (mutual TLS as the client) | `camel.ssl.certFile` and `camel.ssl.keyFile` (or `keyStore`) |
| clients to present a certificate (mutual TLS as the server) | `camel.server.ssl.clientAuthentication=REQUIRE`, `camel.server.ssl.trustStore` |
| different settings for one endpoint | an `sslContextParameters` bean, or the component's own SSL options |
| public CAs on Windows | `SSL_CERT_FILE` pointing at a PEM bundle |
| secrets out of route files | `{{env:NAME}}`, `{{aws:name}}`, `{{azure:name}}`, `{{gcp:name}}` |

## HTTPS for your routes

REST DSL and `platform-http` routes are served by the embedded HTTP server on
`camel.server.port`. Give it a certificate and turn TLS on:

```properties
camel.server.sslEnabled = true
camel.server.ssl.certFile = server.crt
camel.server.ssl.keyFile = server.key
```

The certificate and key are PEM files; `camel.server.ssl.keyStore` takes a combined PEM file, a
PKCS#12 file or a JKS keystore instead (with `keystorePassword`, and `keyStoreType` when the file
name does not say). `camel.server.useGlobalSslContextParameters=true` uses the `camel.ssl.*`
parameters for the server too. One endpoint can serve TLS on its own with `ssl=true` or
`sslContextParameters=#bean` on its URI.

For a development certificate:

```bash
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout server.key -out server.crt -days 30 \
  -subj "/CN=localhost" -addext "subjectAltName=DNS:localhost,IP:127.0.0.1"
```

Everything on `camel.server.port` speaks what the port speaks: with `camel.server.sslEnabled=true`,
the management endpoints (`/q/health`, `/q/metrics`, `/q/dev`), `/q/attestation` and the MCP
server are HTTPS too, next to the HTTPS routes. To keep health probes and metrics on plain HTTP
(for example for a Kubernetes probe or a Prometheus scraper inside the cluster), move them to
Camel's separate management port with `camel.management.enabled=true` (port 9876 by default,
see [Observability](observability.md#a-separate-management-port)).

## TLS for client endpoints

A TLS endpoint (`https:`, `kafka:` with `securityProtocol=SSL`, `ftps:`, `smtps:`, PostgreSQL with
`sslmode` in its JDBC URL, cloud services, ...) verifies the server's certificate chain
and host name against the system trust store by default. For a private CA or a client
certificate, set the global parameters, which every TLS endpoint then uses:

```properties
camel.ssl.enabled = true
camel.ssl.trustStore = ca.crt
# mutual TLS: the client's certificate and key
camel.ssl.certFile = client.crt
camel.ssl.keyFile = client.key
```

`camel.ssl.*` has no effect without `camel.ssl.enabled=true`, as in Camel. An endpoint with
`useGlobalSslContextParameters=true` uses them even when the flag is off.

| key (under `camel.ssl.` or `camel.server.ssl.`) | what it sets |
|---|---|
| `keyStore` | a combined PEM file (certificate chain and key), a PKCS#12 file or a JKS keystore |
| `keystorePassword`, `keyPassword`, `keyStoreType`, `certAlias` | how to open the keystore and which entry to use |
| `certFile`, `keyFile` | the certificate chain and private key as separate PEM files |
| `trustStore` | CA certificates to trust: PEM files (comma separated), inline PEM, or a PKCS#12 or JKS file with `trustStorePassword` and `trustStoreType` |
| `useSystemTrustStore` | also trust the system store when a `trustStore` is given |
| `secureSocketProtocol`, `secureSocketProtocols`, `minVersion`, `maxVersion` | the TLS versions: `TLSv1.2`, `TLSv1.3` (both by default; older versions are not implemented) |
| `clientAuthentication` | for a server: `NONE` (default), `WANT` or `REQUIRE` a client certificate |
| `crlFile` | a certificate revocation list for client certificates |
| `serverName`, `hostnameVerification` | the SNI name to send, and whether to check the host name (on by default) |
| `alpn`, `sessionTimeout`, `sessionResumption` | ALPN protocols and session resumption |
| `trustAllCertificates` | turn off certificate verification: for tests only, never in production |

**Per endpoint.** An `sslContextParameters=#name` option on the URI, or
`camel.component.<scheme>.sslContextParameters=#name`, uses a bean registered in Rust
(`cameleer_tls::SslContextParameters`). Several components also take their own SSL options, with
Camel's names: Kafka's `sslTruststoreLocation` and `sslKeystoreLocation`, the mail components'
`sslContextParameters`, PostgreSQL's `sslmode` and `sslrootcert` in the JDBC URL. Each component page lists them.

## The system trust store

Without a `trustStore`, Cameleer trusts the CA bundle named by `SSL_CERT_FILE`, or else the first
of `/etc/ssl/certs/ca-certificates.crt`, `/etc/pki/tls/certs/ca-bundle.crt`,
`/etc/ssl/ca-bundle.pem`, `/etc/ssl/cert.pem` and
`/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem`. The Docker image, most Linux distributions
and macOS have one. Windows has none of these paths and Cameleer does not read the Windows
certificate store, so set `SSL_CERT_FILE` there (for example to the `cacert.pem` bundle from
curl's website).

## Mutual TLS

As the server: require a client certificate signed by a CA you trust.

```properties
camel.server.sslEnabled = true
camel.server.ssl.certFile = server.crt
camel.server.ssl.keyFile = server.key
camel.server.ssl.clientAuthentication = REQUIRE
camel.server.ssl.trustStore = clients-ca.crt
```

As the client: present a certificate with `camel.ssl.certFile` and `camel.ssl.keyFile` (above), or
the component's own options (Kafka: `sslKeystoreLocation`). A client without a certificate gets
`SSLHandshakeException: TLS alert received from peer: certificate_required (116)`.

## What a TLS failure looks like

TLS errors use Camel's exception kinds, so `onException` and `doCatch` can handle them:

| exception | when |
|---|---|
| `SSLPeerUnverifiedException` | the server's certificate is not trusted, expired or for another host (`unable to find trusted issuer 'CN=localhost' for 'CN=localhost'`) |
| `SSLHandshakeException` | the handshake failed, for example the peer sent an alert (`certificate_required`, `handshake_failure`) |
| `SSLException` | other TLS errors; the parent of the two above |

## A worked example

Cameleer on both sides: a server route over HTTPS, and a client route that calls it and trusts
its certificate. It runs in the test suite (`tls_guide_worked_example` in
`crates/cameleer-main/tests/docs_reference.rs`), with a certificate made like the one above.

<!-- file: server.camel.yaml -->
<!-- example: tls-hello-server -->
```yaml
- route:
    id: hello-tls
    from:
      uri: platform-http:/hello
      steps:
        - setBody:
            constant: hello over TLS
```

<!-- file: client.camel.yaml -->
<!-- example: tls-hello-client -->
```yaml
- route:
    id: call-hello
    from:
      uri: direct:call
      steps:
        - to: "https://localhost:{{server.port}}/hello"
```

```bash
cameleer run server.camel.yaml --port 8443 \
  --prop camel.server.sslEnabled=true \
  --prop camel.server.ssl.certFile=server.crt --prop camel.server.ssl.keyFile=server.key
curl --cacert server.crt https://localhost:8443/hello      # hello over TLS
```

The client side, as a [`cameleer test`](testing.md) file: with the trust store the call
succeeds, without it the certificate is refused.

<!-- file: client.test.yaml -->
<!-- not-camel -->
```yaml
- test:
    name: the client trusts the server's certificate
    routes: [client.camel.yaml]
    properties:
      server.port: 8443
      camel.ssl.enabled: true
      camel.ssl.trustStore: server.crt
    steps:
      - send: {to: direct:call}
        expect: {body: hello over TLS}
- test:
    name: without the trust store the certificate is refused
    routes: [client.camel.yaml]
    properties:
      server.port: 8443
    steps:
      - send: {to: direct:call}
        expect: {error: SSLPeerUnverifiedException}
```

## Secrets

Keep keys and passwords out of route files and images:

| source | placeholder | configured with |
|---|---|---|
| an environment variable | `{{env:DB_PASSWORD}}`, `{{env:DB_PASSWORD:default}}` | the process environment (Kubernetes secrets, Docker `-e`, `--env-file`) |
| AWS Secrets Manager | `{{aws:orders/db-password}}` | `camel.vault.aws.*` |
| Azure Key Vault | `{{azure:db-password}}` | `camel.vault.azure.*` |
| Google Secret Manager | `{{gcp:db-password}}` | `camel.vault.gcp.*` |

`CAMEL_*` environment variables also set configuration keys directly
(`CAMEL_SSL_KEYSTORE_PASSWORD` sets `camel.ssl.keystorePassword`). The vault placeholders and their options are in
[cloud credentials](cloud-credentials.md#secrets-in-routes); the AWS and Azure vaults can
restart routes when a secret changes. Cameleer does not decrypt Jasypt `ENC(...)` values.

## Securing what Cameleer serves

| surface | built in | what to do |
|---|---|---|
| REST and `platform-http` routes | TLS and mutual TLS; no user authentication | check a header or token in the route (`choice` on `${header.Authorization}`, or a call to your identity provider), or put an API gateway or proxy in front |
| the MCP server | API keys, JWT (JWKS or HMAC), OAuth token introspection, protected resource metadata, audience checks, `Origin` checks | `camel.server.mcp-*` options: see [MCP server authentication](mcp-server.md#authentication) |
| AI tools | authorization policies, approvals, rate limits, redaction, an audit log | [`ai-tool`](../components/ai-tool.md) options |
| management endpoints (`/q/*`) | none | keep them on a private network, or move them to the management port bound to loopback (`camel.management.enabled=true`, `camel.management.host=127.0.0.1`); `/q/dev` can stop routes |

**Messages.** The [crypto](../dataformats/crypto.md) and [PGP](../dataformats/pgp.md) data
formats encrypt and sign payloads, and [XML Security](../components/xmlsecurity-sign.md) signs
and verifies XML documents.

**The container.** The image runs as a non-root user on a distroless base with no shell, and works
with a read-only root filesystem (`--read-only`, Kubernetes `readOnlyRootFilesystem: true`).

## Evidence of what runs (confidential VMs)

In an Intel TDX or AMD SEV-SNP confidential VM, or a dstack CVM, Cameleer can prove to a client
what it is running: a hardware report bound to the client's nonce, to the server's TLS key and to a
manifest of the routes, configuration and MCP tools. It ships since 0.6.0; see
[Attestation in confidential VMs](attestation.md), including what it does not prove yet.

## Trust in the implementation

Cameleer's TLS, X.509, SSH and cryptography are written from scratch in Rust with no third-party
libraries. They are tested against OpenSSL (`s_client` and `s_server`), the JDK's JSSE, Go's
`crypto/tls`, and real servers such as PostgreSQL, RabbitMQ, ActiveMQ, Mosquitto, vsftpd,
Postfix and Dovecot ([INTEROP.md](../INTEROP.md)). The code has not had an independent security
audit yet; it is on the [roadmap](../ROADMAP.md#25-verification-and-trust). Report a
vulnerability privately as described in [SECURITY.md](../../SECURITY.md).

## Common questions

### How do I enable HTTPS for a REST API?

Set `camel.server.sslEnabled=true`, `camel.server.ssl.certFile` and `camel.server.ssl.keyFile`
(or `camel.server.ssl.keyStore`). Every REST and `platform-http` route on that port is then
served over TLS.

### How do I call a service that uses a private or self-signed CA?

Set `camel.ssl.enabled=true` and `camel.ssl.trustStore` to the CA's PEM file. Do not turn off
verification: `trustAllCertificates` exists for tests only.

### Why does a `camel.ssl.*` setting have no effect?

The global parameters apply only with `camel.ssl.enabled=true` (or on an endpoint with
`useGlobalSslContextParameters=true`).

### Why does HTTPS fail on Windows with "unable to find trusted issuer"?

Cameleer does not read the Windows certificate store. Set `SSL_CERT_FILE` to a PEM bundle of CA
certificates.

### Does Cameleer support TLS 1.0 or 1.1?

No, only TLS 1.2 and 1.3.

### How do I keep a password out of my route file?

Use `{{env:NAME}}` or a vault placeholder (`{{aws:...}}`, `{{azure:...}}`, `{{gcp:...}}`), and
pass the value through the environment or the secret store.

## Related

- [Configuration](configuration.md), [cloud credentials](cloud-credentials.md),
  [MCP server](mcp-server.md#authentication)
- [kafka](../components/kafka.md) (SSL and SASL), [http](../components/http.md),
  [platform-http](../components/platform-http.md)
- [FAQ: which CA certificates does Cameleer trust?](faq.md#which-ca-certificates-does-cameleer-trust)
- [Attestation in confidential VMs](attestation.md): hardware evidence bound to the server's TLS key

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
