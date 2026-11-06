# Google Firestore component

> The `google-firestore` component reads, writes, queries and listens to documents in Google Cloud Firestore, with plain maps as message bodies.

**Scheme:** `google-firestore:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) ·
**Consumer:** yes · **Producer:** yes · **Apache Camel:** none (Cameleer component; Camel has no Firestore component)

## When to use it

- Save incoming JSON (orders, form posts, device state) as Firestore documents.
- React to document changes: a polled snapshot listener emits `ADDED`, `MODIFIED` and
  `REMOVED` events.
- Keep small state for AI agents, such as conversation memory or task status, keyed by id.
- Run atomic batched writes or read-write transactions.

## URI format

```text
google-firestore://projectId[:collection][?options]
```

Sub-collections are written as `parent/docId/child`. Documents are plain maps; values are
converted to and from Firestore typed values (strings, integers, doubles, booleans, null,
bytes, arrays and maps).

## Example

Store every JSON document posted to an HTTP endpoint, using its `id` field as the document id:

<!-- example: google-firestore-save-from-http -->
```yaml
- route:
    id: http-to-firestore
    from:
      uri: "platform-http:/devices?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - setHeader:
            name: CamelGoogleFirestoreDocumentId
            simple: "${body[id]}"
        - to: "google-firestore://{{env:GCP_PROJECT:my-project}}:devices?operation=setDocument"
        - setBody:
            simple: "saved ${header.CamelGoogleFirestoreDocumentId}"
```

Listen to a collection and write one line per change to a file:

<!-- example: google-firestore-listen-changes -->
```yaml
- route:
    id: firestore-changes
    from:
      uri: "google-firestore://{{env:GCP_PROJECT:my-project}}:devices?delay=2000"
      steps:
        - setBody:
            simple: "${header.CamelGoogleFirestoreChangeType} ${header.CamelGoogleFirestoreDocumentId}\n"
        - to: "file:{{env:OUT_DIR:out}}?fileName=changes.log&fileExist=Append"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("google-firestore://my-project:devices")
        .log("${header.CamelGoogleFirestoreChangeType}: ${body}");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `getDocument` | `getDocument`, `listDocuments`, `createDocument`, `setDocument`, `updateDocument`, `deleteDocument`, `runQuery`, `commit`, `batchWrite`, `beginTransaction`, `rollback`. Header `CamelGoogleFirestoreOperation`. |
| `databaseId` | `(default)` | Firestore database. |
| `orderBy` | | `runQuery`: field to order by. |
| `descending` | `false` | `runQuery`: descending order. |
| `limit` | | `runQuery`: maximum documents. |
| `skipInitialSnapshot` | `false` | Consumer: do not report the documents that exist when the route starts. |
| `delay`, `initialDelay`, ... | `500`, `1000` | Consumer poll schedule (milliseconds) and the other scheduled-poll options. |

Credentials are shared by all Google components: see [credentials](google-storage.md#credentials).
Without a project in the URI, the project of the service account key is used.

### Operations

- `getDocument`: body = the fields map. `listDocuments`: a list of maps with `_id`.
- `createDocument`: the body map; the id is generated unless `CamelGoogleFirestoreDocumentId`
  is set, and the header is set to the new id. `setDocument` creates or replaces.
  `updateDocument` merges the body's top-level fields into an existing document.
  `deleteDocument` deletes.
- `runQuery`: the body (or `CamelGoogleFirestoreQuery`) is a `structuredQuery` object, passed
  through, or a map of field to value equality filters. Body = a list of maps with `_id`.
- `commit` (atomic) and `batchWrite` (independent writes, body = per-write `{code, message}`):
  a list body of `{op: set | update | create | delete, collection, id, data}` maps.
- Transactions: `beginTransaction` sets `CamelGoogleFirestoreTransaction`; `getDocument` with
  that header reads inside the transaction; `commit` with it commits (an `ABORTED` error when
  a document read in it changed meanwhile: retry), `rollback` abandons it.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleFirestoreOperation` | in | Operation. |
| `CamelGoogleFirestoreCollection` | in | Collection (overrides the URI). |
| `CamelGoogleFirestoreDocumentId` | in / out | Document id; set by `createDocument` and the consumer. |
| `CamelGoogleFirestoreQuery` | in | Query for `runQuery`. |
| `CamelGoogleFirestoreOrderBy`, `CamelGoogleFirestoreLimit` | in | `runQuery` ordering and limit. |
| `CamelGoogleFirestoreReadOnly` | in | `beginTransaction`: read-only transaction. |
| `CamelGoogleFirestoreTransaction` | in / out | Transaction id. |
| `CamelGoogleFirestoreUpdateTime` | out | Update time of the document. |
| `CamelGoogleFirestoreCommitTime` | out | Commit time of `commit`. |
| `CamelGoogleFirestoreChangeType` | out | Consumer: `ADDED`, `MODIFIED` or `REMOVED`. |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project \
  pmsudhi/cameleer run /routes/http-to-firestore.camel.yaml
```

## Common questions

### How do I query Firestore documents by a field?
Send a map such as `{"city": "Rome"}` to `operation=runQuery`, optionally with `orderBy`,
`descending` and `limit`. For range filters send a full `structuredQuery` object.

### Is the consumer a real-time listener?
It is polled: every poll lists the collection and compares it with the previous snapshot. The
gRPC `Listen` stream is not used.

### How do I write several documents atomically?
Send a list of write maps to `operation=commit`. Either all writes apply or none.

## Related

- [Google Pub/Sub](google-pubsub.md), [Google Sheets](google-sheets.md), [MongoDB](mongodb.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
