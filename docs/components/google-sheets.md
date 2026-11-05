# Google Sheets component

> The `google-sheets` component reads, writes, appends and clears cell values in Google Sheets spreadsheets, and creates and updates spreadsheets, over the Sheets v4 REST API.

**Scheme:** `google-sheets:` · **Crate:** `cameleer-cloud` ·
**Consumer:** no (see [google-sheets-stream](google-sheets-stream.md)) · **Producer:** yes · **Apache Camel:** [Google Sheets](https://camel.apache.org/components/next/google-sheets-component.html)

## When to use it

- Use a spreadsheet as a lightweight database for an AI agent: one tool appends rows, another
  reads them back, and people can see and edit the data in the browser.
- Append form submissions, orders or leads to a sheet that a business team already uses.
- Export a report into a shared sheet on a schedule (`data/update` or `data/batchUpdate`).
- Create spreadsheets and add or rename tabs (`spreadsheets/create`, `spreadsheets/batchUpdate`).

## URI format

```text
google-sheets://apiName/methodName[?options]
```

`apiName/methodName` is one of `data/get`, `data/update`, `data/append`, `data/clear`,
`data/batchGet`, `data/batchUpdate`, `data/batchClear`, `spreadsheets/get`,
`spreadsheets/create` or `spreadsheets/batchUpdate`. Any other value fails when the endpoint is
created.

## Example

Two HTTP tools for an agent that keeps its leads in a sheet. `POST /tools/save-lead` with
`{"name": "Ada", "email": "ada@example.com", "score": 92}` appends one row to the `Leads` tab;
`GET /tools/list-leads` returns all rows as JSON.

<!-- example: google-sheets-agent-memory -->
```yaml
- route:
    id: save-lead
    from:
      uri: "platform-http:/tools/save-lead?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - setBody:
            simple: '[["${body[name]}", "${body[email]}", "${body[score]}"]]'
        - to: "google-sheets://data/append?spreadsheetId={{env:SHEET_ID}}&range=Leads!A:C&valueInputOption=RAW"
        - marshal:
            json: {}
- route:
    id: list-leads
    from:
      uri: "platform-http:/tools/list-leads?httpMethodRestrict=GET"
      steps:
        - to: "google-sheets://data/get?spreadsheetId={{env:SHEET_ID}}&range=Leads!A2:C1000"
        - marshal:
            json: {}
```

The append returns the update result (`updatedRange`, `updatedRows`, `updatedCells`, ...). The
get returns the rows as a list of lists of strings, for example
`[["Ada","ada@example.com","92"]]`. The values are built with a Simple template here, so a name
that contains a double quote would break the JSON; build the row in a processor or send a JSON
array from the client when the input is untrusted.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("platform-http:/tools/save-lead?httpMethodRestrict=POST")
        .unmarshal("json")
        .set_body(simple(r#"[["${body[name]}", "${body[email]}", "${body[score]}"]]"#))
        .to("google-sheets://data/append?spreadsheetId={{env:SHEET_ID}}&range=Leads!A:C&valueInputOption=RAW")
        .marshal("json");
    b.from("platform-http:/tools/list-leads?httpMethodRestrict=GET")
        .to("google-sheets://data/get?spreadsheetId={{env:SHEET_ID}}&range=Leads!A2:C1000")
        .marshal("json");
})?;
```

## What each method does

| api/method | input | body after the call |
|---|---|---|
| `data/get` | `spreadsheetId`, `range`, optional `majorDimension` | the values (list of rows) |
| `data/update` | `range`, values (see below), `valueInputOption` | the update result |
| `data/append` | `range`, values, `valueInputOption` | the `updates` object of the append result |
| `data/clear` | `range` | the clear result |
| `data/batchGet` | `ranges` (comma separated, or a list in the `CamelGoogleSheets.ranges` header) | a list with the values of each range |
| `data/batchUpdate` | body or `CamelGoogleSheets.data` header: a list of `{range, values}` maps (or a request map with `data`), `valueInputOption` | the response (`totalUpdatedCells`, ...) |
| `data/batchClear` | `ranges` | the cleared ranges |
| `spreadsheets/get` | `spreadsheetId` | the spreadsheet resource (title, sheets, ...) |
| `spreadsheets/create` | `title` (default `Untitled spreadsheet`) | the new spreadsheet; header `CamelGoogleSheets.spreadsheetId` is set |
| `spreadsheets/batchUpdate` | body or `CamelGoogleSheets.batchUpdateSpreadsheetRequest` header: a list of requests, one request map, or a map with `requests` | the response (`replies`) |

Values for `data/update` and `data/append` come from the `CamelGoogleSheets.values` header, or
else the body. Accepted shapes: a list of rows (lists), a single flat list (one row), a
`ValueRange` map with a `values` field, or JSON text of any of these.

## Options

| option | default | description |
|---|---|---|
| `spreadsheetId` | | The spreadsheet id (from the sheet URL). Required except for `spreadsheets/create`. |
| `range` | | A1 range, for example `Sheet1!A1:C10` or `Leads!A:C`. Required for `data/get`, `update`, `append`, `clear`. |
| `ranges` | | Comma separated ranges for `data/batchGet` and `data/batchClear`. |
| `valueInputOption` | `USER_ENTERED` | `USER_ENTERED` (parse like typed input: numbers, dates, formulas) or `RAW`. |
| `majorDimension` | | `ROWS` or `COLUMNS` for `data/get`. |
| `title` | | Title for `spreadsheets/create`. |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key: `file:path`, a plain path or inline JSON. An `authorized_user` or `external_account` (workload identity federation) file also works. |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow), used instead of a service account. |
| `accessToken` | | A fixed OAuth access token. |
| `tokenServerUrl` | `https://oauth2.googleapis.com/token` | Token endpoint for the refresh-token flow (Cameleer). |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. |
| `useMetadataServer` | `false` | Take tokens from the GCE/GKE metadata server (also used automatically when `GCE_METADATA_HOST` is set and no other credentials are given). `metadataServerUrl` overrides its URL (Cameleer). |
| `authenticate` | `true` | `false` sends no credentials (emulators). |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Send requests to another base URL (an emulator or a proxy). `endpoint` (`host:port` or a URL) does the same. |
| `proxyHost`, `proxyPort` | `3128` | HTTP proxy. Without them the `HTTPS_PROXY`/`NO_PROXY` environment variables apply. |
| `trustAllCertificates` | `false` | Only for local emulators with self-signed certificates. |

`applicationName`, `scopes`, `splitResult`, `inBody` and `includeGridData` are accepted for
compatibility with Camel and ignored. The OAuth scope is always
`https://www.googleapis.com/auth/spreadsheets`.

## Message headers

Every option above that the methods read (`spreadsheetId`, `range`, `ranges`,
`valueInputOption`, `majorDimension`, `title`) can also be set per message as a
`CamelGoogleSheets.<option>` header, which wins over the URI option.

| header | direction | description |
|---|---|---|
| `CamelGoogleSheets.values` | in | Values for `data/update` / `data/append` (instead of the body). |
| `CamelGoogleSheets.data` | in | Value ranges for `data/batchUpdate`. |
| `CamelGoogleSheets.batchUpdateSpreadsheetRequest` | in | Requests for `spreadsheets/batchUpdate`. |
| `CamelGoogleSheets.spreadsheetId` | in / out | Spreadsheet id; set after `spreadsheets/create`. |

## Run it with Docker

Share the spreadsheet with the service account's e-mail address (Editor), then:

```sh
docker run --rm -p 8080:8080 \
  -e SHEET_ID=1AbC...xyz \
  -e GOOGLE_APPLICATION_CREDENTIALS=/secrets/key.json \
  -v "$PWD/key.json:/secrets/key.json:ro" \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/leads.camel.yaml
```

## Common questions

### How do I append a row to a Google Sheet?
Send a list (one row) or a list of lists (several rows) to
`google-sheets://data/append?spreadsheetId=...&range=Sheet1!A:C`. Rows are added after the last
row of the table found in the range.

### Why does Sheets return "The caller does not have permission"?
The service account is a separate Google identity. Share the spreadsheet with its e-mail address
(`...@...iam.gserviceaccount.com`), or use `clientId`/`clientSecret`/`refreshToken` to act as a
user.

### How do I add a new tab?
Call `spreadsheets/batchUpdate` with the body `[{"addSheet": {"properties": {"title": "2026"}}}]`.
Requests are passed through to the API unchanged.

### How do I get notified of new rows?
Use the [google-sheets-stream](google-sheets-stream.md) consumer, which emits only the rows
appended since the previous poll.

## Related

- [google-sheets-stream](google-sheets-stream.md): consume new rows.
- [google-drive](google-drive.md), [google-mail](google-mail.md),
  [google-calendar](google-calendar.md).
- [Apache Camel Google Sheets component](https://camel.apache.org/components/next/google-sheets-component.html)
