# MongoDB component

> The `mongodb` component inserts, queries, updates and deletes documents in MongoDB, and consumes change streams and tailable cursors.

**Scheme:** `mongodb:` · **Crate:** `cameleer-datastores` · **Cargo feature:** [`datastores`](../guides/build-features.md) · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [MongoDB](https://camel.apache.org/components/next/mongodb-component.html)

## When to use it

- Store JSON documents from HTTP, Kafka or files without defining a schema first.
- Give an AI agent a memory: save notes, tool results or conversation turns per session and
  read the latest ones back before the next model call.
- React to changes: a change stream consumer routes every insert, update and delete.
- Run aggregations and bulk writes from a route.

## URI format

```text
mongodb:<connectionBean>?database=<db>&collection=<coll>&operation=<op>[&options]
```

The connection is one of:

- `connectionUriString=mongodb://host:27017` (or `mongodb+srv://cluster.example.com`, resolved
  with Cameleer's own DNS client), the path is then just a name;
- `hosts=host1:27017,host2:27017` with `username`, `password` and `authSource`;
- a `MongoClient` bound in the registry under the path name.

Put credentials in `username` and `password` rather than in `connectionUriString`: a
connection string with its own `?` options does not fit inside the endpoint URI.

Bodies are documents: maps, JSON text (Extended JSON and shell syntax such as
`ObjectId("...")` are accepted) or lists of them. Results use Extended JSON for BSON-only types
(`{"$oid": "..."}`, `{"$date": "..."}`).

## Example

`POST /notes` stores the JSON body as a document and returns it with its generated `_id`. An
agent can call this as a "remember" tool.

<!-- example: mongodb-save-notes -->
```yaml
- route:
    id: save-note
    from:
      uri: "platform-http:/notes?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "mongodb:agentDb?connectionUriString={{env:MONGODB_URI:mongodb://localhost:27017}}&database=agent&collection=notes&operation=insert"
        - marshal:
            json: {}
```

`GET /notes?session=s1` returns the 20 newest notes of one session. The query document comes
from the body; sort and limit come from headers.

<!-- example: mongodb-recent-notes -->
```yaml
- route:
    id: recent-notes
    from:
      uri: "platform-http:/notes?httpMethodRestrict=GET"
      steps:
        - filter:
            simple: "${header.session} !regex '^[A-Za-z0-9_-]+$'"
            steps:
              - setHeader:
                  name: CamelHttpResponseCode
                  constant: 400
              - setBody:
                  constant: "invalid session"
              - stop: {}
        - setBody:
            simple: '{"session": "${header.session}"}'
        - setHeader:
            name: CamelMongoDbSortBy
            constant: '{"_id": -1}'
        - setHeader:
            name: CamelMongoDbLimit
            constant: 20
        - to: "mongodb:agentDb?connectionUriString={{env:MONGODB_URI:mongodb://localhost:27017}}&database=agent&collection=notes&operation=findAll"
        - marshal:
            json: {}
```

The filter rejects session ids that are not plain words, so a request cannot inject query
operators into the JSON.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:remember")
        .to("mongodb:agentDb?connectionUriString=mongodb://localhost:27017&database=agent&collection=notes&operation=insert");
    b.from("mongodb:agentDb?connectionUriString=mongodb://localhost:27017&database=agent&collection=notes&consumerType=changeStreams")
        .log("${header.CamelMongoDbStreamOperationType}: ${body}");
})?;
```

## Operations

| operation | body | result |
|---|---|---|
| `findById` | the id | the document or null |
| `findOneByQuery` | query document | the first match or null |
| `findAll` | query document (null = all) | list of documents; headers `CamelMongoDbResultTotalSize`, `CamelMongoDbResultPageSize` |
| `findDistinct` | query document | distinct values of the field in `CamelMongoDbDistinctQueryField` |
| `count` | query document | the number of matches |
| `insert` | document or list | the inserted document(s) with `_id`; header `CamelMongoOid` |
| `save` | document with `_id` | replaces or inserts it |
| `update` | `[filter, update]` list, or the update with the filter in `CamelMongoDbCriteria` | `CamelMongoDbRecordsAffected`, `CamelMongoDbRecordsMatched`; `CamelMongoDbMultiUpdate`, `CamelMongoDbUpsert` |
| `remove` | query document | `{deletedCount}`; header `CamelMongoDbRecordsAffected` |
| `bulkWrite` | list of `insertOne`, `updateOne`, `updateMany`, `replaceOne`, `deleteOne`, `deleteMany` models | counts; ordered unless `CamelMongoDbBulkOrdered=false`; write errors raise `MongoBulkWriteException` |
| `aggregate` | pipeline (list of stages) | list of documents; `CamelMongoDbAllowDiskUse` |
| `getDbStats`, `getColStats` | | the stats document |
| `command` | command document | the reply |

## Options

| option | default | description |
|---|---|---|
| `connectionUriString` | | `mongodb://` or `mongodb+srv://` connection string |
| `hosts` | | `host:port` list, used with `username`, `password`, `authSource` |
| `database` | | database name |
| `collection` | | collection name |
| `operation` | | producer operation (header `CamelMongoDbOperation` overrides it) |
| `createCollection` | `true` | create the collection on first use |
| `dynamicity` | `false` | allow `CamelMongoDbDatabase` and `CamelMongoDbCollection` headers to pick the target |
| `writeResultAsHeader` | `false` | put write results in `CamelMongoWriteResult` and keep the body |
| `consumerType` | | `changeStreams` or `tailable` |
| `streamFilter` | | change streams: a `$match` document, for example `{"operationType":"insert"}` |
| `fullDocument` | | change streams: `updateLookup` to receive the whole document on updates |
| `tailTrackIncreasingField` | | tailable: the increasing field to resume from |
| `cursorRegenerationDelay` | `1000` ms | tailable: wait before reopening a dead cursor |
| `sslContextParameters` | | TLS trust (with `tls=true` in the connection string) |

Authentication supports SCRAM-SHA-256, SCRAM-SHA-1 and MONGODB-X509. Replica sets are
discovered with `hello` and followed after a primary change. `readPreference`, `writeConcern`,
`collectionIndex`, `outputType` and the persistent tail-tracking options are accepted and
ignored; tail tracking is kept in memory only.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelMongoDbOperation` | in | operation for this exchange |
| `CamelMongoDbCriteria` | in | query document (instead of the body) |
| `CamelMongoDbFieldsProjection` | in | projection document |
| `CamelMongoDbSortBy` | in | sort document |
| `CamelMongoDbLimit`, `CamelMongoDbNumToSkip`, `CamelMongoDbBatchSize` | in | paging |
| `CamelMongoDbMultiUpdate`, `CamelMongoDbUpsert` | in | update flags |
| `CamelMongoDbDistinctQueryField` | in | field for `findDistinct` |
| `CamelMongoDbAllowDiskUse` | in | aggregation flag |
| `CamelMongoDbBulkOrdered` | in | `false` for unordered bulk writes |
| `CamelMongoDbDatabase`, `CamelMongoDbCollection` | in | target, with `dynamicity=true` |
| `CamelMongoOid` | out | id(s) of inserted documents |
| `CamelMongoDbRecordsAffected`, `CamelMongoDbRecordsMatched` | out | write counts |
| `CamelMongoDbResultTotalSize`, `CamelMongoDbResultPageSize` | out | query counts |
| `CamelMongoWriteResult` | out | write result with `writeResultAsHeader=true` |
| `CamelMongoDbStreamOperationType` | out | change streams: `insert`, `update`, `delete`, ... |
| `_id` | out | change streams: the document key |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e MONGODB_URI=mongodb://mongo.example.com:27017 \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/notes.camel.yaml
```

## Common questions

### How do I connect with a username and password?

Use `hosts=mongo.example.com:27017&username=app&password={{env:MONGODB_PASSWORD}}&authSource=admin`
instead of putting them in `connectionUriString`.

### How do I sort and limit a query?

Set the headers `CamelMongoDbSortBy` (for example `{"_id": -1}`) and `CamelMongoDbLimit` before a
`findAll`.

### How do I react to new documents?

Consume with `consumerType=changeStreams`; add `streamFilter={"operationType":"insert"}` to see
inserts only. Change streams need a replica set on the server.

### Does it work with Azure Cosmos DB or FerretDB?

It speaks the MongoDB wire protocol (`OP_MSG`) and was verified against FerretDB 1.24 and an
embedded server. Other compatible servers are not tested.

## Related

- [mongodb-gridfs](mongodb-gridfs.md): store files in MongoDB.
- [couchdb](couchdb.md), [elasticsearch](elasticsearch.md): other document stores.
- [Apache Camel MongoDB component](https://camel.apache.org/components/next/mongodb-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
