# Google Drive component

> The `google-drive` component uploads, downloads, lists, copies and deletes Google Drive files through the Drive v3 REST API, and can poll a file listing to emit created, modified and removed files.

**Scheme:** `google-drive:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** yes (file changes of `drive-files/list`) · **Producer:** yes · **Apache Camel:** [Google Drive](https://camel.apache.org/components/next/google-drive-component.html)

## When to use it

- Upload generated reports, invoices or exports to a shared Drive folder.
- Watch a folder and start a workflow when someone drops a file in it, for example to have an
  LLM summarize a new document or to load a CSV into a database.
- Download files for processing at the edge, then move or delete them.
- Manage sharing (`drive-permissions/*`) and comments (`drive-comments/*`) from a route.

## URI format

```text
google-drive://apiName/methodName[?options]
```

Core methods: `drive-files/list`, `drive-files/get`, `drive-files/download`,
`drive-files/create` (alias `drive-files/insert`), `drive-files/update`, `drive-files/copy`,
`drive-files/delete`. Also: `drive-about/get`, `drive-changes/getStartPageToken|list|watch`,
`drive-files/watch|emptyTrash|generateIds`, `drive-channels/stop`,
`drive-permissions/create|list|get|update|delete`, `drive-comments/create|list|get|update|delete`,
`drive-replies/create|list|get|delete`, `drive-revisions/list|get|delete`,
`drive-drives/create|list|get|update|delete`, `drive-teamdrives/list`.

## Example

Upload every PDF that appears in a local `reports` directory to a Drive folder. The Drive file
name is the local file name (`CamelFileName`); uploaded files are moved to `reports/.uploaded`.

<!-- example: google-drive-upload-reports -->
```yaml
- route:
    id: reports-to-drive
    from:
      uri: "file:{{env:REPORTS_DIR:reports}}?antInclude=*.pdf&move=.uploaded"
      steps:
        - to: "google-drive://drive-files/create?parents={{env:DRIVE_FOLDER_ID}}&mimeType=application/pdf"
        - log: "uploaded ${header.CamelFileName} as Drive file ${body[id]}"
```

Watch the same folder: the first poll is skipped (`consumeExisting=false`), then each poll emits
one exchange per created, modified or removed file.

<!-- example: google-drive-watch-folder -->
```yaml
- route:
    id: watch-drive-folder
    from:
      uri: "google-drive://drive-files/list?q='{{env:DRIVE_FOLDER_ID}}' in parents and trashed = false&consumeExisting=false&delay=60000"
      steps:
        - log: "${header[CamelGoogleDrive.changeType]}: ${body[name]} (${header[CamelGoogleDrive.fileId]})"
        - to: "log:drive-changes"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("file:{{env:REPORTS_DIR:reports}}?antInclude=*.pdf&move=.uploaded")
        .to("google-drive://drive-files/create?parents={{env:DRIVE_FOLDER_ID}}&mimeType=application/pdf")
        .log("uploaded ${header.CamelFileName} as Drive file ${body[id]}");
    b.from("google-drive://drive-files/list?q='{{env:DRIVE_FOLDER_ID}}' in parents and trashed = false&consumeExisting=false&delay=60000")
        .log("${header[CamelGoogleDrive.changeType]}: ${body[name]} (${header[CamelGoogleDrive.fileId]})")
        .to("log:drive-changes");
})?;
```

## What the core methods do

| api/method | input | body after the call |
|---|---|---|
| `drive-files/list` | `q`, `pageSize`, `orderBy`, `fields` | list of files (all pages); default fields `id, name, mimeType, size, parents, modifiedTime` |
| `drive-files/get` | `fileId` | file metadata |
| `drive-files/download` | `fileId` | file content as bytes |
| `drive-files/create` | body = content; `name` (or `title`, or the `CamelFileName` header), `mimeType` (default `application/octet-stream`), `parents` | created file (multipart upload) |
| `drive-files/update` | `fileId`, body = new content, `mimeType` | updated file |
| `drive-files/copy` | `fileId`, optional `name` | the copy |
| `drive-files/delete` | `fileId` | unchanged |

Methods that take a request body (`drive-permissions/create`, `drive-comments/create`, ...) read
it from the `CamelGoogleDrive.content` header or a map body. For the other Drive methods,
`fields` defaults to `*`.

## The file-change consumer

`from("google-drive://drive-files/list?...")` polls the listing (with your `q`) and compares it
with the previous poll:

- a file that was not listed before: `created`
- a file whose `modifiedTime` changed: `modified` (content or metadata)
- a file that is no longer listed: `removed` (body = `{id}`)

The first poll emits every listed file as `created`; set `consumeExisting=false` (Cameleer) to
report only later changes. The snapshot is kept in memory, so after a restart the first poll
counts again as the first. Apache Camel's Drive consumer instead re-runs the API call on every
poll and emits its whole result.

## Options

| option | default | description |
|---|---|---|
| `fileId` | | File id. |
| `q` | | Drive search query, for example `'<folderId>' in parents and trashed = false`. |
| `pageSize`, `orderBy` | | Listing options. |
| `fields` | see above | Fields to return. |
| `name` (alias `title`) | `CamelFileName` header | File name for `create` and `copy`. |
| `mimeType` | `application/octet-stream` | Content type for `create` and `update`. |
| `parents` | | Comma separated parent folder ids for `create`. |
| `consumeExisting` | `true` | Consumer: emit the files of the first poll as `created`. |
| `delay` | `500` | Consumer: milliseconds between polls. Scheduled poll options such as `initialDelay` (default `1000`) also apply. |
| `permissionId`, `commentId`, `replyId`, `revisionId`, `driveId`, `pageToken`, `count`, `requestId`, `channelId`, `address`, `token`, `resourceId` | | Parameters of the extra methods. |
| `delegate` (alias `subject`) | | User to impersonate with domain-wide delegation. |
| `scopes` | `https://www.googleapis.com/auth/drive` | Comma separated OAuth scopes. |
| `serviceAccountKey` | `GOOGLE_APPLICATION_CREDENTIALS` | Service account JSON key: `file:path`, a plain path or inline JSON. |
| `clientId`, `clientSecret`, `refreshToken` | | OAuth user credentials (refresh-token flow). |
| `accessToken` | | A fixed OAuth access token. |
| `uriEndpointOverride` + `overrideEndpoint=true` | | Base URL of an emulator or proxy. |

`tokenServerUrl`, `useMetadataServer`, `authenticate`, `proxyHost`, `proxyPort` and
`trustAllCertificates` work as on the [google-sheets](google-sheets.md#options) page.
`applicationName`, `inBody` and `splitResult` are accepted and ignored.

## Message headers

Every option can also be set per message with a `CamelGoogleDrive.<option>` header (for example
`CamelGoogleDrive.fileId`), which wins over the URI option.

| header | direction | description |
|---|---|---|
| `CamelGoogleDrive.content` | in | Request map for methods that take one. |
| `CamelFileName` | in | File name for `drive-files/create` when `name` is not set. |
| `CamelGoogleDrive.changeType` | out (consumer) | `created`, `modified` or `removed`. |
| `CamelGoogleDrive.fileId` | out (consumer) | Id of the changed file. |
| `CamelGoogleDrive.resourceId` | out | Resource id of a created watch channel. |

## Run it with Docker

Share the folder with the service account's e-mail address, then:

```sh
docker run --rm \
  -e DRIVE_FOLDER_ID=1x2y3z... -e REPORTS_DIR=/data/reports \
  -e GOOGLE_APPLICATION_CREDENTIALS=/secrets/key.json \
  -v "$PWD/key.json:/secrets/key.json:ro" \
  -v "$PWD/reports:/data/reports" \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/reports-to-drive.camel.yaml
```

The reports directory is mounted read-write because uploaded files are moved.

## Common questions

### How do I upload a file to a Google Drive folder?
Send the content to `google-drive://drive-files/create?parents=<folderId>&mimeType=...` with a
`name` option or a `CamelFileName` header. A `file:` consumer sets `CamelFileName` for you.

### How do I download a Drive file?
Call `drive-files/download` with `CamelGoogleDrive.fileId`; the body becomes the file's bytes.
Google Docs, Sheets and Slides cannot be downloaded this way (they need an export).

### Where do files uploaded by a service account go?
Without `parents` they go to the service account's own Drive, which no person sees. Share a
folder with the service account and pass its id as `parents`.

### How do I process new files dropped into a folder?
Consume `google-drive://drive-files/list?q='<folderId>' in parents and trashed = false`; each new
file arrives with `CamelGoogleDrive.changeType` = `created`. Download it with
`drive-files/download` in the same route.

## Related

- [google-storage](google-storage.md) for bucket storage, [file](file.md).
- [google-sheets](google-sheets.md), [google-mail](google-mail.md), [google-calendar](google-calendar.md).
- [Apache Camel Google Drive component](https://camel.apache.org/components/next/google-drive-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
