# FTPS component

> The `ftps` component transfers files with an FTP server over TLS (FTPS), in explicit (`AUTH TLS`) or implicit mode, with encrypted data connections.

**Scheme:** `ftps:` · **Crate:** `cameleer-protocols` · **Cargo feature:** [`protocols`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** [FTPS](https://camel.apache.org/components/next/ftps-component.html)

`ftps` has every option of [ftp](ftp.md) plus the TLS options below. The server certificate and
host name are always verified. FTPS is FTP over TLS; it is not [sftp](sftp.md), which runs over
SSH.

## When to use it

- Send reports or invoices to a partner who requires FTPS.
- Collect files from a bank, a carrier or a public agency that only offers FTPS.
- Upload data from edge sites to a central server without sending credentials in clear text.

## URI format

```text
ftps://[username@]host[:port]/directory[?options]
```

Default port: 21 (explicit), 990 with `isImplicit=true`.

## Example

An edge site uploads finished CSV reports from its outbox to the central server, into a folder
per site, and moves each file to `.sent` locally afterwards.

<!-- example: ftps-upload-reports -->
```yaml
- route:
    id: upload-reports
    from:
      uri: "file:{{env:OUTBOX:outbox}}?include=.*\\.csv&move=.sent"
      steps:
        - to: "ftps://{{env:FTPS_USER:edge}}@{{env:FTPS_HOST:ftp.example.com}}/reports/{{env:SITE_ID:site-1}}?password={{env:FTPS_PASSWORD:changeme}}&passiveMode=true&binary=true&tempPrefix=.part-"
        - log: "uploaded ${file:name}"
```

The server certificate is checked against the system trust store, or against the global
`camel.ssl.*` parameters (for example `camel.ssl.enabled=true` and `camel.ssl.trustStore=/routes/ca.pem`; without `camel.ssl.enabled=true` they are ignored) or an
`sslContextParameters` bean.

## Rust DSL

```rust
fn upload_reports_route(b: &mut RouteBuilder, outbox: &str, ftps: &str, password: &str) {
    b.from(&format!("file:{}?include=.*\\.csv&move=.sent", outbox))
        .route_id("upload-reports")
        .to(&format!("ftps://{}/reports/site-1?password={}&passiveMode=true&binary=true&tempPrefix=.part-", ftps, password))
        .log("uploaded ${file:name}");
}
```

## Options

All [ftp options](ftp.md#options), plus:

| option | default | description |
|---|---|---|
| `isImplicit` | `false` | Implicit TLS (TLS from the first byte, port 990). |
| `execPbsz` | `0` | `PBSZ` value sent after login. |
| `execProt` | `P` | `PROT` level: `P` encrypts data connections, `C` leaves them clear. |
| `disableSecureDataChannelDefaults` | `false` | Send `PBSZ` / `PROT` only when set explicitly. |
| `securityProtocol` | | `TLSv1.2`, `TLSv1.3` or `TLS`. |
| `sslContextParameters` | | `#bean` with trust and key material. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters (with `camel.ssl.enabled=true`). |

Data connections resume the control connection's TLS session, which servers configured like
vsftpd `require_ssl_reuse=YES` demand. The `ftpClient.trustStore.*` / `ftpClient.keyStore.*`
options are accepted and ignored; use `sslContextParameters`.

## Message headers

The same as [ftp](ftp.md#message-headers).

## Run it with Docker

```sh
docker run --rm --user "$(id -u):$(id -g)" -e FTPS_HOST=ftp.example.com -e FTPS_USER -e FTPS_PASSWORD \
  -e SITE_ID=site-7 -e OUTBOX=/data/outbox -v "$PWD:/data" \
  -v "$PWD/upload-reports.camel.yaml:/routes/upload-reports.camel.yaml:ro" \
  cameleering/cameleer run /routes/upload-reports.camel.yaml
```

## Common questions

### Explicit or implicit FTPS?
Most servers use explicit FTPS on port 21 (the default). Use `isImplicit=true` for servers that
expect TLS immediately, usually on port 990.

### The server uses a private CA. How do I trust it?
Set `camel.ssl.enabled=true` and `camel.ssl.trustStore` to the CA's PEM file, or bind an `SSLContextParameters` bean and add
`sslContextParameters=#myTls`. Certificate checks cannot be turned off per endpoint.

### Is FTPS the same as SFTP?
No. FTPS is FTP with TLS; SFTP is a different protocol over SSH. Use [sftp](sftp.md) for SSH
servers.

## Related

- [ftp](ftp.md), [sftp](sftp.md), [file](file.md).
- Apache Camel: [FTPS component](https://camel.apache.org/components/next/ftps-component.html).

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
