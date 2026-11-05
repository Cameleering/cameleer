# MIME multipart data format

> The `mimeMultipart` data format writes a message body and its attachments as one MIME multipart document, and splits a multipart document back into a body and attachments.

**Name:** `mimeMultipart` · **Also:** `mime-multipart` · **Crate:** `cameleer-dataformats` ·
**Apache Camel:** [MIME Multipart](https://camel.apache.org/components/next/dataformats/mimeMultipart-dataformat.html)

## When to use it

- Send a document together with its attachments over HTTP, JMS or a queue as one payload
  (the format email uses).
- Receive `multipart/mixed` or `multipart/related` payloads (SOAP with attachments, AS2-like
  exchanges, email bodies) and work with the parts.
- Package a generated report and its data files for a downstream system in one message.

## Marshal and unmarshal

Attachments live in the `CamelAttachments` exchange property, an object mapping a file name
to its content (the convention of the mail component).

| direction | input | output |
|---|---|---|
| marshal | body plus `CamelAttachments` | multipart body: the body is the first part (type from the `Content-Type` header, default `text/plain; charset=UTF-8` for text, `application/octet-stream` for bytes), each attachment a further part with `Content-Disposition: attachment; filename=...` and a type guessed from the extension |
| unmarshal | multipart body (MIME headers inline, or the boundary in the `Content-Type` header) | the first part as body (with its `Content-Type` header), the other parts in `CamelAttachments` |

Without attachments marshal leaves the body unchanged unless `multipartWithoutAttachment=true`.
Non-text parts are base64 encoded unless `binaryContent=true`. With `headersInline=false`
(the default) `MIME-Version` and `Content-Type` (with the boundary) become message headers;
with `headersInline=true` they are written at the top of the body. Unmarshal leaves
non-multipart content unchanged.

## Example

Attach a CSV file to a short text, write one multipart document with inline headers, then
read it back.

<!-- example: mime-multipart-round-trip -->
```yaml
- route:
    id: mime-round-trip
    from:
      uri: direct:mime
      steps:
        - setProperty:
            name: CamelAttachments
            simple: "${map('totals.csv','region;sum')}"
        - marshal:
            mimeMultipart:
              headersInline: true
        - setHeader:
            name: firstLine
            simple: "${body.split('\r\n')[0]}"
        - removeProperty:
            name: CamelAttachments
        - unmarshal:
            mimeMultipart:
              headersInline: true
        - setBody:
            simple: "${header.firstLine} | body: ${body} | attachments: ${exchangeProperty.CamelAttachments.keySet()}"
```

<!-- input: mime-multipart-round-trip -->
```text
Weekly totals attached.
```

<!-- output: mime-multipart-round-trip -->
```text
MIME-Version: 1.0 | body: Weekly totals attached. | attachments: ["totals.csv"]
```

## Rust DSL

```rust
b.from("direct:mime-rust")
    .marshal(DataFormatDefinition::new("mimeMultipart").option("headersInline", "true").option("multipartWithoutAttachment", "true"))
    .unmarshal(DataFormatDefinition::new("mimeMultipart").option("headersInline", "true"));
```

## Options

| option | default | description |
|---|---|---|
| `multipartSubType` | `mixed` | The subtype: `multipart/mixed`, `related`, `alternative`... |
| `multipartWithoutAttachment` | `false` | Also wrap a body that has no attachments. |
| `headersInline` | `false` | Write (marshal) and read (unmarshal) the MIME headers in the body instead of message headers. |
| `includeHeaders` | (none) | With `headersInline=true`, a regex: message headers whose names match are written into the MIME headers. |
| `binaryContent` | `false` | Write non-text parts as raw bytes instead of base64. |

## Message headers and properties

| name | kind | description |
|---|---|---|
| `CamelAttachments` | exchange property | Attachments: `{file name: content}`. Read on marshal, set on unmarshal. |
| `Content-Type` | header | Type of the body part on marshal; with `headersInline=false` it carries the multipart type and boundary after marshal and is read on unmarshal. |
| `MIME-Version` | header | Set on marshal when `headersInline=false`. |

## Differences from Apache Camel

Camel keeps attachments in `AttachmentMessage` (`DataHandler`s). Here they are a plain object
in the `CamelAttachments` property, shared with the mail component, so you can set them with
Simple (`${map(...)}`) or read them with `${exchangeProperty.CamelAttachments}`.

## Common questions

### How do I add an attachment before marshalling?

Set the `CamelAttachments` property to an object, for example
`setProperty: {name: CamelAttachments, simple: "${map('report.pdf', ${header.pdf})}"}`.

### Why is my body unchanged after marshal?

There were no attachments. Set `multipartWithoutAttachment: true` to always produce a
multipart document.

## Related

- [Mail component](../components/mail.md), [HTTP component](../components/http.md)
- [Base64 data format](base64.md)
- [Apache Camel MIME Multipart data format](https://camel.apache.org/components/next/dataformats/mimeMultipart-dataformat.html)
