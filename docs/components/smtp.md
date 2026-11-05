# SMTP component

> The `smtp` component sends email: the message body becomes the mail text and headers such as `To` and `Subject` set the recipients and subject.

**Scheme:** `smtp:` · **Also:** `smtps:` · **Crate:** `cameleer-protocols` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Mail](https://camel.apache.org/components/next/mail-component.html)

`smtps:` uses implicit TLS (port 465). Plain `smtp:` can upgrade with STARTTLS
(`mail.smtp.starttls.enable=true`). To read mail, use [imap](imap.md) or [pop3](pop3.md).

## When to use it

- Email a daily report produced at an edge site or by a batch job.
- Send alerts and notifications from any route.
- Let an AI agent send its summary or answer by mail.

## URI format

```text
smtp://[username@]host[:port][?options]
smtps://[username@]host[:port][?options]
```

Default ports: 25 (`smtp`), 465 (`smtps`).

## Example

Mail every text report that appears in the reports folder, with the file name in the subject.

<!-- example: smtp-mail-reports -->
```yaml
- route:
    id: mail-reports
    from:
      uri: "file:{{env:REPORTS_DIR:reports}}?include=.*\\.txt&move=.sent"
      steps:
        - setHeader:
            name: Subject
            simple: "Daily report ${file:name.noext}"
        - convertBodyTo:
            type: String
        - to: "smtp://{{env:SMTP_USER:reports}}@{{env:SMTP_HOST:localhost:25}}?password={{env:SMTP_PASSWORD:changeme}}&from={{env:MAIL_FROM:reports@example.com}}&to={{env:MAIL_TO:ops@example.com}}&mail.smtp.starttls.enable=true"
```

`mail.smtp.starttls.enable=true` upgrades to TLS when the server offers it; use
`mail.smtp.starttls.required=true` to refuse plain connections, or `smtps://` for port 465.

## Rust DSL

```rust
fn mail_reports_route(b: &mut RouteBuilder, reports_dir: &str, smtp: &str) {
    b.from(&format!("file:{}?include=.*\\.txt&move=.sent", reports_dir))
        .route_id("mail-reports")
        .set_header("Subject", simple("Daily report ${file:name.noext}"))
        .convert_body_to("String")
        .to(smtp);
}
```

## Options

| option | default | description |
|---|---|---|
| `username`, `password` | | Authenticate with `AUTH PLAIN` or `AUTH LOGIN`. |
| `from` | `camel@localhost` | Sender. |
| `to` | `username@host` | Recipients (comma-separated). |
| `cc`, `bcc` | | Copy recipients. |
| `replyTo` | | `Reply-To` address. |
| `subject` | | Subject. |
| `contentType` | `text/plain` | Content type of the body, e.g. `text/html`. |
| `connectionTimeout` | `30s` | Connect timeout. |
| `mail.smtp.starttls.enable` | `false` | Upgrade with STARTTLS when offered. |
| `mail.smtp.starttls.required` | `false` | Fail when STARTTLS is not offered. |
| `sslContextParameters` | | `#bean` with trust material for TLS. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters. |

Other `mail.*` properties are accepted and ignored, as are `debugMode`, `peek` and similar
Apache Camel options.

## Message headers

| header | direction | description |
|---|---|---|
| `To`, `Cc`, `Bcc` | in | Recipients; override the options. |
| `From`, `Reply-To` | in | Sender and reply address. |
| `Subject` | in | Subject. |
| `Content-Type` | in | Content type of the body. |

Other headers with simple values (except `Camel*` ones) are added to the mail as headers.
Attachments come from the `CamelAttachments` exchange property (a map of file name to content)
and are sent as `multipart/mixed`.

## Run it with Docker

```sh
docker run --rm --user "$(id -u):$(id -g)" -e SMTP_HOST=smtp.example.com:587 -e SMTP_USER -e SMTP_PASSWORD \
  -e MAIL_TO=ops@example.com -e REPORTS_DIR=/data/reports -v "$PWD:/data" \
  -v "$PWD/mail-reports.camel.yaml:/routes/mail-reports.camel.yaml:ro" \
  pmsudhi/cameleer run /routes/mail-reports.camel.yaml
```

## Common questions

### How do I send HTML mail?
Set `contentType=text/html` or the `Content-Type` header to `text/html; charset=UTF-8`.

### How do I send to several people?
Comma-separate the addresses in `to` or in the `To` header; `Bcc` recipients are not shown in
the mail.

### How do I add an attachment?
Put a map of file name to content in the `CamelAttachments` exchange property.

### Which port: 25, 465 or 587?
465 is `smtps://` (implicit TLS). 587 and 25 are `smtp://` with
`mail.smtp.starttls.enable=true` (or `.required=true`).

## Related

- [imap](imap.md), [pop3](pop3.md): read mail.
- Apache Camel: [Mail component](https://camel.apache.org/components/next/mail-component.html).
