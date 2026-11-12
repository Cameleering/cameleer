# CouchDB component

> The `couchdb` component saves, reads and deletes JSON documents in Apache CouchDB and consumes the database's changes feed.

**Scheme:** `couchdb:` · **Crate:** `cameleer-datastores` · **Cargo feature:** [`datastores`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [CouchDB](https://camel.apache.org/components/next/couchdb-component.html)

## When to use it

- Store JSON documents over CouchDB's HTTP API, including from edge sites that later
  replicate to a central CouchDB.
- React to every change in a database: the consumer follows the `_changes` feed and routes
  each new, updated or deleted document.
- Save batches of documents in one `_bulk_docs` request.

## URI format

```text
couchdb:http://host[:port]/database[?options]
couchdb:https://host[:port]/database[?options]
```

The port defaults to 5984. `https` uses TLS with the system trust store, the global
`camel.ssl.*` parameters (with `camel.ssl.enabled=true`) or `sslContextParameters=#bean`.

**Producer.** Without a `CouchDbMethod` header the body (a map or JSON text) is saved:
`PUT /db/{_id}` when it has an `_id`, else `POST /db`. A body with `_rev` updates that
revision. `CouchDbMethod=GET` reads the document `CouchDbId` (or the id in the body);
`CouchDbMethod=DELETE` deletes the document given by the body's `_id` and `_rev` or by the
`CouchDbId` and `CouchDbRev` headers. A list body is saved with one `_bulk_docs` request.

**Consumer.** Follows `_changes?feed=longpoll&include_docs=true` from the current update
sequence (or `since`). The body is the document.

## Example

`POST /notes` saves the JSON body as a new document and answers with its id. `createDatabase`
creates the database on first use.

<!-- example: couchdb-save-documents -->
```yaml
- route:
    id: save-note
    from:
      uri: "platform-http:/notes?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "couchdb:{{env:COUCHDB_URL:http://localhost:5984}}/notes?createDatabase=true&username={{env:COUCHDB_USER:admin}}&password={{env:COUCHDB_PASSWORD:secret}}"
        - setBody:
            simple: "${header.CouchDbId}"
```

This route follows the changes feed of the same database and logs every change.

<!-- example: couchdb-follow-changes -->
```yaml
- route:
    id: note-changes
    from:
      uri: "couchdb:{{env:COUCHDB_URL:http://localhost:5984}}/notes?username={{env:COUCHDB_USER:admin}}&password={{env:COUCHDB_PASSWORD:secret}}"
      steps:
        - log: "${header.CouchDbMethod} ${header.CouchDbId} rev ${header.CouchDbRev}"
        - to: "log:changes"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:save")
        .to("couchdb:http://localhost:5984/notes?createDatabase=true&username=admin&password=secret");
    b.from("direct:get")
        .set_header("CouchDbMethod", constant("GET"))
        .to("couchdb:http://localhost:5984/notes?username=admin&password=secret");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `createDatabase` | `false` | create the database if it does not exist |
| `username`, `password` | | basic authentication |
| `since` | current sequence | consumer: start the feed after this update sequence |
| `updates` | `true` | consumer: route created and updated documents |
| `deletes` | `true` | consumer: route deletions |
| `heartbeat` | `30000` ms | consumer: longpoll timeout |
| `style` | `main_only` | consumer: `main_only` or `all_docs` (all leaf revisions) |
| `maxMessagesPerPoll` | `0` | consumer: changes per feed request (`limit`; 0 = unlimited) |
| `sslContextParameters` | | TLS trust for `https` |

## Message headers

| header | direction | description |
|---|---|---|
| `CouchDbMethod` | in/out | in: `GET` or `DELETE` (save otherwise); out (consumer): `UPDATE` or `DELETE` |
| `CouchDbId` | in/out | document id (a list of ids after a bulk save) |
| `CouchDbRev` | in/out | document revision (a list after a bulk save) |
| `CouchDbDatabase` | out | consumer: the database name |
| `CouchDbSeq` | out | consumer: the update sequence of the change |

Errors: a revision conflict raises `DocumentConflictException`, a missing document
`NoDocumentException`.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 \
  -e COUCHDB_URL=http://couchdb.example.com:5984 -e COUCHDB_USER=app -e COUCHDB_PASSWORD \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/notes.camel.yaml
```

## Common questions

### How do I update a document?

Send the document with its `_id` and current `_rev`. A stale `_rev` raises
`DocumentConflictException`; read the latest revision with `CouchDbMethod=GET` and retry.

### How do I resume the changes feed after a restart?

Store the last `CouchDbSeq` you processed and start the consumer with `since=<seq>`. Without
`since`, the feed starts at the current sequence and skips older changes.

### Is Couchbase supported?

No. Couchbase uses a binary key-value protocol that Cameleer does not implement.

## Related

- [mongodb](mongodb.md): another document database.
- [Apache Camel CouchDB component](https://camel.apache.org/components/next/couchdb-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
