# Barcode data format

> The `barcode` data format turns text into a barcode image (QR Code, Data Matrix, PDF417, Aztec, Code 128, EAN and more) on marshal, and reads the text back from a barcode image on unmarshal.

**Name:** `barcode` · **Crate:** `cameleer-dataformats` · **Cargo feature:** [`dataformats`](../guides/build-features.md) ·
**Apache Camel:** [Barcode](https://camel.apache.org/components/next/dataformats/barcode-dataformat.html)

## When to use it

- Print QR codes or Data Matrix codes on shipping labels, tickets or asset tags straight
  from a route (for example with a link to the order).
- Read barcodes from scanned documents or camera images on an edge device (a warehouse
  gateway, a kiosk) without a cloud vision service.
- Let an AI agent attach a scannable code to what it produces, such as a QR code with a
  tracking URL in a generated email.

## Marshal and unmarshal

| direction | input body | output body |
|---|---|---|
| marshal | text | image bytes (PNG by default, also GIF or JPG) |
| unmarshal | a PNG, GIF or baseline JPEG image | the decoded text; headers `BarcodeFormat` and, for QR codes, `ERROR_CORRECTION_LEVEL` |
| unmarshal with `multiple=true` | an image with several symbols | an array of `{text, BarcodeFormat, ...}` objects; headers `BarcodeFormat` (array) and `BarcodeCount` |

Symbologies (`barcodeFormat`):

| format | marshal (encode) | unmarshal (decode) |
|---|---|---|
| `QR_CODE` (default) | yes, versions 1-40, error correction L/M/Q/H | yes |
| `DATA_MATRIX` | yes, square and rectangular | yes |
| `PDF_417` | yes, error correction 0-8 | yes |
| `AZTEC` | yes | yes |
| `CODE_128`, `CODE_39`, `CODE_93`, `CODABAR`, `ITF` | yes | yes |
| `EAN_13`, `EAN_8`, `UPC_A`, `UPC_E` | yes (check digit computed or verified) | yes |
| `MAXICODE`, `UPC_EAN_EXTENSION`, `RSS_14`, `RSS_EXPANDED` | no (ZXing has no writer either) | yes |

With the default `barcodeFormat` (QR_CODE), unmarshal tries every format. Setting another
format restricts decoding to it. The codecs are written from scratch (no ZXing) but follow
ZXing's rendering and option names, and decode photographs (rotation, perspective, noise) as
well as clean generated images.

## Example

Make a QR code for a tracking link, then read it back. A real route would write the PNG to a
file or attach it to an email instead of decoding it.

<!-- example: barcode-qr-round-trip -->
```yaml
- route:
    id: barcode-qr
    from:
      uri: direct:barcode-qr
      steps:
        - marshal:
            barcode:
              barcodeFormat: QR_CODE
              errorCorrectionLevel: M
              width: 200
              height: 200
        - setHeader:
            name: pngSignature
            simple: "${body.substring(1, 4)}"
        - unmarshal:
            barcode: {}
        - setBody:
            simple: "${header.pngSignature} ${header.BarcodeFormat} ${header.ERROR_CORRECTION_LEVEL}: ${body}"
```

<!-- input: barcode-qr-round-trip -->
```text
https://example.com/track/ORD-1042
```

<!-- output: barcode-qr-round-trip -->
```text
PNG QR_CODE M: https://example.com/track/ORD-1042
```

A linear barcode for a product label:

<!-- example: barcode-ean13 -->
```yaml
- route:
    id: barcode-ean
    from:
      uri: direct:barcode-ean
      steps:
        - marshal:
            barcode:
              barcodeFormat: EAN_13
              width: 300
              height: 100
        - unmarshal:
            barcode:
              barcodeFormat: EAN_13
        - setBody:
            simple: "${header.BarcodeFormat} ${body}"
```

<!-- input: barcode-ean13 -->
```text
590123412345
```

<!-- output: barcode-ean13 -->
```text
EAN_13 5901234123457
```

The 12 digits get their check digit (7) computed on marshal.

## Rust DSL

```rust
b.from("direct:barcode-rust")
    .marshal(DataFormatDefinition::new("barcode").option("barcodeFormat", "DATA_MATRIX").option("imageType", "PNG"))
    .unmarshal(DataFormatDefinition::new("barcode").option("barcodeFormat", "DATA_MATRIX"));
```

## Options

| option | default | description |
|---|---|---|
| `barcodeFormat` | `QR_CODE` | The symbology (see the table above). |
| `width` | `100` | Image width in pixels. The symbol is scaled by the largest whole factor that fits, with a quiet zone. |
| `height` | `100` | Image height in pixels. |
| `imageType` | `PNG` | `PNG`, `GIF` or `JPG`. |
| `errorCorrectionLevel` | `L` (QR), `2` (PDF417) | QR: `L`, `M`, `Q`, `H`. PDF417: `0` to `8`. |
| `dataMatrixShape` | `NONE` | Data Matrix shape: `NONE`, `SQUARE` or `RECTANGLE`. Alias `shape`. |
| `pdf417Compaction` | `AUTO` | PDF417 compaction: `AUTO`, `TEXT`, `BYTE`, `NUMERIC`. Alias `compaction`. |
| `multiple` | `false` | Unmarshal every symbol in the image into a list. |

## Message headers

| header | direction | description |
|---|---|---|
| `BarcodeFormat` | out (unmarshal) | The format found, for example `QR_CODE`; an array with `multiple=true`. |
| `ERROR_CORRECTION_LEVEL` | out (unmarshal) | QR error correction level of the symbol read. |
| `BarcodeCount` | out (unmarshal) | Number of symbols with `multiple=true`. |

## Common questions

### How do I generate a QR code PNG in a route?

`marshal: barcode: {width: 300, height: 300}` on a text body, then `to: file:labels` with a
`CamelFileName` ending in `.png`.

### How do I read a barcode from an uploaded image?

`unmarshal: barcode: {}` on the image bytes. Every format is tried when `barcodeFormat` is
not set; the format found is in the `BarcodeFormat` header.

### Which error correction level should I use for QR codes?

`L` (the default) gives the smallest code. Use `M` or `Q` for printed labels that may get
scratched, `H` when a logo covers part of the code.

## Related

- [Base64 data format](base64.md): embed the image in JSON or HTML
- [File component](../components/file.md), [Mail (SMTP) component](../components/smtp.md)
- [Apache Camel Barcode data format](https://camel.apache.org/components/next/dataformats/barcode-dataformat.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
