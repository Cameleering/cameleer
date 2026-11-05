# Google Sheets Stream component

> The `google-sheets-stream` component polls a range of a Google Sheets spreadsheet and emits the rows that were appended since the previous poll.

**Scheme:** `google-sheets-stream:` · **Crate:** `cameleer-cloud` ·
**Consumer:** yes · **Producer:** no · **Apache Camel:** [Google Sheets Stream](https://camel.apache.org/components/next/google-sheets-stream-component.html)

## When to use it

- Trigger a route when someone adds a row to a shared sheet: a new order, a support request, a
  task for an AI agent to work on.
- Feed rows entered by a business team into a database, a queue or an API.
- Run a "sheet as inbox" workflow at the edge without a webhook or a public URL (it only polls).

## URI format

```text
google-sheets-stream://spreadsheetId?range=Sheet1!A2:D1000[&options]
```

The spreadsheet id is the path (or the `spreadsheetId` option). `range` is required; the endpoint
fails to start without it.

## Example

Every 30 seconds, read `Orders!A2:D1000` and send each new row on its own, as JSON, to the log.
The first poll emits all rows already in the range.

<!-- example: google-sheets-stream-new-rows -->
```yaml
- route:
    id: new-orders
    from:
      uri: "google-sheets-stream://{{env:SHEET_ID}}?range=Orders!A2:D1000&splitResults=true&delay=30000"
      steps:
        - log: "row ${header[CamelGoogleSheets.valueIndex]}: ${body}"
        - marshal:
            json: {}
        - to: "log:new-orders"
```

With `splitResults=true` the body of each exchange is one row, for example
`["1042","Ada","3","shipped"]`, and `CamelGoogleSheets.valueIndex` is its 1-based row number
inside the range. Without it, one exchange per poll carries all new rows as a `ValueRange` map
(`range`, `majorDimension`, `values`).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-sheets-stream://{{env:SHEET_ID}}?range=Orders!A2:D1000&splitResults=true&delay=30000")
        .log("row ${header[CamelGoogleSheets.valueIndex]}: ${body}")
        .marshal("json")
        .to("log:new-orders");
})?;
```

## How new rows are detected

The consumer remembers how many rows the range had at the previous poll and emits only the rows
after that position. If rows are deleted so that the range becomes shorter, it starts counting
again from the new end. Edits to rows that were already emitted are not detected.

This differs from Apache Camel, whose consumer emits the whole range on every poll. Use a
filter or an idempotent consumer there; with Cameleer you get only the appended rows. The
position is kept in memory, so after a restart the first poll emits the whole range again.

## Options

| option | default | description |
|---|---|---|
| `spreadsheetId` | path | The spreadsheet id (instead of the path). |
| `range` | | A1 range to read, for example `Orders!A2:D1000`. Required. |
| `splitResults` | `false` | One exchange per new row instead of one per poll. |
| `majorDimension` | `ROWS` | `ROWS` or `COLUMNS`. |
| `valueRenderOption` | | `FORMATTED_VALUE`, `UNFORMATTED_VALUE` or `FORMULA`. |
| `delay` | `500` | Milliseconds between polls (scheduled poll consumer options such as `initialDelay` (default `1000`), `repeatCount` and `backoffMultiplier` also apply). |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key (`file:path`, path or inline JSON). |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow). |
| `accessToken` | | A fixed OAuth access token. |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. |
| `scopes` | `https://www.googleapis.com/auth/spreadsheets` | Comma separated OAuth scopes. |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Base URL of an emulator or proxy. |

The other shared Google options (`tokenServerUrl`, `useMetadataServer`, `authenticate`,
`proxyHost`, `proxyPort`, `trustAllCertificates`) work as on the
[google-sheets](google-sheets.md#options) page. `includeGridData` and `maxResults` are accepted
and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleSheets.spreadsheetId` | out | The spreadsheet id. |
| `CamelGoogleSheets.range` | out | The range returned by the API (for example `Orders!A2:D1000`). |
| `CamelGoogleSheets.majorDimension` | out | `ROWS` or `COLUMNS`. |
| `CamelGoogleSheets.valueIndex` | out | With `splitResults=true`: the 1-based row number within the range. |

Header names contain dots, so read them in Simple with the bracket form:
`${header[CamelGoogleSheets.valueIndex]}`.

## Run it with Docker

```sh
docker run --rm \
  -e SHEET_ID=1AbC...xyz \
  -e GOOGLE_APPLICATION_CREDENTIALS=/secrets/key.json \
  -v "$PWD/key.json:/secrets/key.json:ro" \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/new-orders.camel.yaml
```

## Common questions

### How do I trigger a route when a row is added to a Google Sheet?
Use `google-sheets-stream://<spreadsheetId>?range=...&splitResults=true`. Each appended row
arrives as one exchange with the row as a list.

### Why do I get all rows again after a restart?
The last row position is kept in memory only. Skip the first batch with a filter, or record
processed ids with an idempotent repository.

### Can I watch several tabs?
Use one route per range, each with its own `google-sheets-stream` endpoint.

## Related

- [google-sheets](google-sheets.md): read and write values.
- [Idempotent consumer](../guides/eips.md)
- [Apache Camel Google Sheets Stream component](https://camel.apache.org/components/next/google-sheets-stream-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
