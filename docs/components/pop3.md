# POP3 component

> The `pop3` component downloads email from a POP3 mailbox on a schedule and turns each message into an exchange.

**Scheme:** `pop3:` · **Also:** `pop3s:` · **Crate:** `cameleer-protocols` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Mail](https://camel.apache.org/components/next/mail-component.html)

`pop3s:` uses implicit TLS (port 995); `pop3:` can upgrade with STLS
(`mail.pop3.starttls.enable=true`). For folders and flags use [imap](imap.md).

## When to use it

- Archive every incoming message of a mailbox to disk or object storage.
- Collect mail from devices or old systems that report by email.
- Simple mail intake where messages are deleted from the server once handled.

## URI format

```text
pop3://[username@]host[:port][?options]
pop3s://[username@]host[:port][?options]
```

Default ports: 110 (`pop3`), 995 (`pop3s`).

## Example

Archive each new message as a raw `.eml` file and delete it from the server.

<!-- example: pop3-archive-mail -->
```yaml
- route:
    id: archive-mail
    from:
      uri: "pop3s://{{env:POP3_HOST:pop.example.com}}?username={{env:POP3_USER:archive@example.com}}&password={{env:POP3_PASSWORD:changeme}}&mapMailMessage=false&delete=true&delay=60000"
      steps:
        - to: "file:{{env:ARCHIVE_DIR:mail-archive}}?fileName=${date:now:yyyyMMdd}/${exchangeId}.eml"
```

## Rust DSL

```rust
fn archive_mail_route(b: &mut RouteBuilder, pop3: &str, archive_dir: &str) {
    b.from(pop3)
        .route_id("archive-mail")
        .to(&format!("file:{}?fileName=${{date:now:yyyyMMdd}}/${{exchangeId}}.eml", archive_dir));
}
```

## Options

| option | default | description |
|---|---|---|
| `username`, `password` | | Login. |
| `delete` | `false` | Delete messages after processing. |
| `unseen` | `true` | Only messages not seen before (tracked by `UIDL`, in memory). |
| `maxMessagesPerPoll` / `fetchSize` | no limit | Messages per poll. |
| `mapMailMessage` | `true` | `true`: the body is the text content. `false`: the raw message. |
| `delay` | `60000` | Milliseconds between polls (`initialDelay` 1000 and the other scheduler options). |
| `connectionTimeout` | `30s` | Connect timeout. |
| `mail.pop3.starttls.enable`, `mail.pop3.starttls.required` | `false` | STLS on `pop3:`. |
| `sslContextParameters` | | `#bean` with trust material for TLS. |
| `useGlobalSslContextParameters` | `false` | Use the global `camel.ssl.*` parameters. |

## Message headers

| header | direction | description |
|---|---|---|
| `Subject`, `From`, `To`, `Date`, `Message-ID`, ... | out | Every mail header; repeated headers become a list. |

MIME parts other than the body go to the `CamelAttachments` exchange property.

## Run it with Docker

```sh
docker run --rm --user "$(id -u):$(id -g)" -e POP3_HOST=pop.example.com -e POP3_USER -e POP3_PASSWORD \
  -e ARCHIVE_DIR=/data/mail-archive -v "$PWD:/data" \
  -v "$PWD/archive-mail.camel.yaml:/routes/archive-mail.camel.yaml:ro" \
  pmsudhi/cameleer run /routes/archive-mail.camel.yaml
```

## Common questions

### Does pop3 remember which messages it has read?
Without `delete=true`, it remembers message `UIDL`s in memory, so a restart reads them again. Use
`delete=true` or IMAP for durable tracking.

### How do I get only the text?
Keep `mapMailMessage=true` (the default): the body is the first `text/plain` part.

## Related

- [imap](imap.md), [smtp](smtp.md), [file](file.md).
- Apache Camel: [Mail component](https://camel.apache.org/components/next/mail-component.html).
