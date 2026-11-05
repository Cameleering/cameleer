# MongoDB GridFS component

> The `mongodb-gridfs` component stores, reads, lists and deletes files in MongoDB GridFS buckets, and polls a bucket for new files.

**Scheme:** `mongodb-gridfs:` · **Crate:** `cameleer-datastores` · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [MongoDB GridFS](https://camel.apache.org/components/next/mongodb-gridfs-component.html)

## When to use it

- Keep documents, images or model outputs (generated reports, audio, embeddings dumps) next
  to the MongoDB data that refers to them.
- Archive files dropped into a folder by an edge device or a scanner.
- Serve stored files over HTTP, or hand every new upload to a processing route.

## URI format

```text
mongodb-gridfs:<connectionBean>?database=<db>[&bucket=fs][&operation=create][&options]
```

The connection is `connectionUriString=mongodb://host:27017` (the path is then just a name) or
a `MongoClient` bound in the registry under the path name. Files use the standard GridFS layout
(`<bucket>.files` and `<bucket>.chunks`), so files written by other drivers can be read and
the other way round.

## Example

This route uploads every file dropped into a folder into the `reports` bucket. The file
component sets `CamelFileName`, which becomes the GridFS file name.

<!-- example: mongodb-gridfs-upload-folder -->
```yaml
- route:
    id: archive-reports
    from:
      uri: "file:{{env:INBOX_DIR:inbox}}?move=.done"
      steps:
        - to: "mongodb-gridfs:archive?connectionUriString={{env:MONGODB_URI:mongodb://localhost:27017}}&database=docs&bucket=reports&operation=create"
        - log: "stored ${header.CamelFileName} as ${header[gridfs.fileid]}"
```

`GET /reports?name=q3.csv` returns the newest file with that name.

<!-- example: mongodb-gridfs-download -->
```yaml
- route:
    id: download-report
    from:
      uri: "platform-http:/reports?httpMethodRestrict=GET"
      steps:
        - setHeader:
            name: CamelFileName
            simple: "${header.name}"
        - to: "mongodb-gridfs:archive?connectionUriString={{env:MONGODB_URI:mongodb://localhost:27017}}&database=docs&bucket=reports&operation=findOne"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("file:inbox?move=.done")
        .to("mongodb-gridfs:archive?connectionUriString=mongodb://localhost:27017&database=docs&bucket=reports");
    b.from("mongodb-gridfs:archive?connectionUriString=mongodb://localhost:27017&database=docs&bucket=reports&queryStrategy=FileAttribute")
        .to("file:outbox");
})?;
```

## Operations

| operation | needs | result |
|---|---|---|
| `create` (default) | body (bytes or text), `CamelFileName` | stores the file; header `gridfs.fileid` |
| `findOne` | `gridfs.objectid` or `CamelFileName` | body = file content (the newest file with that name); headers `CamelFileName`, `CamelFileLength`, `CamelFileLastModified`, `Content-Type`, `gridfs.metadata`, `gridfs.fileid` |
| `remove` | `gridfs.objectid` or `CamelFileName` | deletes the file (or all files with that name) and its chunks |
| `listAll` | optional `CamelFileName` | body = one `filename<TAB>id` line per file |
| `count` | optional `CamelFileName` | body = number of files |

## Options

| option | default | description |
|---|---|---|
| `connectionUriString` | | `mongodb://` connection string (else the path names a `MongoClient` bean) |
| `database` | the connection string's database | database name (required) |
| `bucket` | `fs` | bucket name |
| `operation` | `create` | producer operation (header `gridfs.operation` overrides it) |
| `query` | | consumer: extra filter on the files collection, as JSON |
| `queryStrategy` | `TimeStamp` | consumer: `TimeStamp` (files uploaded after the consumer started), `FileAttribute` (files without the marker attribute) or `TimeStampAndFileAttribute` |
| `fileAttributeName` | `camel-processed` | consumer: the `metadata` field used as marker: `processing` while routed, `done` afterwards |
| `delay`, `initialDelay` | `500` ms, `1000` ms | consumer: the scheduled poll options |

`readPreference`, `writeConcern`, `persistentTSCollection` and `persistentTSObject` are accepted
and ignored. `PersistentTimestamp` is treated as `TimeStamp`: the timestamp is kept in memory,
so after a restart the `TimeStamp` strategy only sees files uploaded after the new start. Use
`FileAttribute` when every file must be processed exactly once across restarts.

## Message headers

| header | direction | description |
|---|---|---|
| `gridfs.operation` | in | operation for this exchange |
| `CamelFileName` | in/out | file name |
| `gridfs.objectid` | in | file id (hex) for `findOne` and `remove` |
| `gridfs.chunksize` | in | chunk size for `create` (default 261120 bytes) |
| `gridfs.metadata` | in/out | metadata as a JSON object |
| `Content-Type` | in/out | content type stored with the file |
| `gridfs.fileid` | out | id of the stored or read file |
| `CamelFileLength`, `CamelFileLastModified` | out | size and upload time (epoch milliseconds) |

## Run it with Docker

```sh
docker run --rm -e MONGODB_URI=mongodb://mongo.example.com:27017 -e INBOX_DIR=/inbox \
  -v "$PWD/inbox:/inbox" -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/archive.camel.yaml
```

The inbox is mounted read-write because `move=.done` moves processed files.

## Common questions

### How do I store metadata with a file?

Set the `gridfs.metadata` header to a JSON object, for example `{"customer": "acme"}`. It is
saved in the file's `metadata` field and returned by `findOne`.

### How do I process every new file once?

Consume with `queryStrategy=FileAttribute`: each file is marked in its metadata while and
after it is routed, so it is not picked up again, even after a restart.

### What chunk size is used?

261120 bytes (255 KiB), like the MongoDB drivers. Override it per file with `gridfs.chunksize`.

## Related

- [mongodb](mongodb.md): documents and queries.
- [file](file.md): read and write local files.
- [Apache Camel MongoDB GridFS component](https://camel.apache.org/components/next/mongodb-gridfs-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
