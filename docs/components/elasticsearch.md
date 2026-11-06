# Elasticsearch component

> The `elasticsearch` component indexes, updates, gets, deletes and searches documents in Elasticsearch over its REST API.

**Scheme:** `elasticsearch:` · **Also:** `opensearch:` (same component, see [opensearch](opensearch.md)) · **Crate:** `cameleer-datastores` · **Cargo feature:** [`datastores`](../guides/build-features.md) ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Elasticsearch](https://camel.apache.org/components/next/elasticsearch-component.html)

## When to use it

- Index documents, logs or events as they flow through a route.
- Retrieval for AI agents: run a full-text search and pass the top documents to an LLM as
  context (retrieval-augmented generation).
- Bulk-load or re-index data, and scroll through large result sets.

## URI format

```text
elasticsearch://clusterName?hostAddresses=host:9200&operation=<op>[&indexName=<index>][&options]
```

`clusterName` is a label. `hostAddresses` is a comma-separated list tried in order (default
`localhost:9200`). Requests use HTTP, or HTTPS with `enableSSL=true`.

## Example

`POST /articles` indexes the JSON body into the `articles` index and returns the document id.

<!-- example: elasticsearch-index-documents -->
```yaml
- route:
    id: index-article
    from:
      uri: "platform-http:/articles?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "elasticsearch://search?hostAddresses={{env:ES_HOSTS:localhost:9200}}&user={{env:ES_USER:elastic}}&password={{env:ES_PASSWORD:changeme}}&operation=Index&indexName=articles"
```

`GET /search?q=rust` returns the three best-matching articles. Put the result in a prompt and
an LLM can answer from your own documents. Queries with characters other than letters, digits,
spaces and simple punctuation are rejected, because the query text is placed inside JSON.

<!-- example: elasticsearch-search-for-rag -->
```yaml
- route:
    id: search-articles
    from:
      uri: "platform-http:/search?httpMethodRestrict=GET"
      steps:
        - filter:
            simple: "${header.q} !regex '^[A-Za-z0-9 ,.?_-]+$'"
            steps:
              - setHeader:
                  name: CamelHttpResponseCode
                  constant: 400
              - setBody:
                  constant: "invalid query"
              - stop: {}
        - setBody:
            simple: '{"query": {"match": {"text": "${header.q}"}}}'
        - to: "elasticsearch://search?hostAddresses={{env:ES_HOSTS:localhost:9200}}&user={{env:ES_USER:elastic}}&password={{env:ES_PASSWORD:changeme}}&operation=Search&indexName=articles&size=3&enableDocumentOnlyMode=true"
        - marshal:
            json: {}
```

A body that is JSON text, like the one built by `setBody` above, is sent as the full search
request, so it must contain `query` (and can add `knn`, `aggs` or `sort`). A body that is a map
is treated as the query and wrapped into `{"query": ...}`, unless it already has a `query` key.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:index")
        .to("elasticsearch://search?hostAddresses=localhost:9200&operation=Index&indexName=articles");
    b.from("direct:search")
        .to("elasticsearch://search?hostAddresses=localhost:9200&operation=Search&indexName=articles&enableDocumentOnlyMode=true");
})?;
```

## Operations

| operation | body | result |
|---|---|---|
| `Index` | document | the id (header `indexId` sets it, else generated) |
| `Update` | partial document (wrapped in `{"doc": ...}` unless it has `doc`, `script` or `upsert`) | the id |
| `Bulk` | list of documents, or of `{"index"/"create"/"update"/"delete": {...}}` actions | the item results |
| `GetById` | the id (or header `indexId`) | the response, or the `_source` with `enableDocumentOnlyMode` |
| `MultiGet` | list of ids | the responses or documents |
| `Delete` | the id | the result, for example `deleted` |
| `DeleteIndex` | | `acknowledged` |
| `Exists` | | whether the index exists |
| `Search` | query object or full request | the `hits` object, or the documents with `enableDocumentOnlyMode` |
| `MultiSearch` | list of search requests | the responses or documents |
| `Ping` | | whether the cluster answers |

Operation names are case-insensitive; `BulkIndex` is accepted for `Bulk`.

## Options

| option | default | description |
|---|---|---|
| `hostAddresses` | `localhost:9200` | comma-separated `host:port` list, tried in order |
| `operation` | | operation (header `operation` overrides it) |
| `indexName` | | index (header `indexName` overrides it) |
| `from`, `size` | | paging for searches |
| `enableDocumentOnlyMode` | `false` | return `_source` documents instead of response objects |
| `user`, `password` | | basic authentication |
| `apiKey` | | sent as `Authorization: ApiKey <value>` |
| `enableSSL` | `false` | HTTPS; trust from `sslContextParameters=#bean`, the global `camel.ssl.*` parameters or the system store |
| `connectionTimeout` | `30s` | connect timeout |
| `socketTimeout` | `30s` | read timeout |
| `useScroll` | `false` | run `Search` as a scroll search and return all hits |
| `scrollKeepAliveMs` | `60000` | scroll context keep-alive |
| `enableSniffing` | `false` | replace `hostAddresses` with the nodes from `GET /_nodes/http` |
| `sniffInterval` | `300000` | milliseconds between sniffs |
| `sniffAfterFailureDelay` | `60000` | milliseconds before sniffing again after a connection failure |
| `documentClass` | | map `_source` documents to declared fields: an inline spec such as `title:string,year:int,price:double`, or a `#bean` |

`maxRetryTimeout`, `certificatePath` (use `sslContextParameters`), `waitForActiveShards` and
`connectionManagerMaxConnections` are accepted and ignored.

## Message headers

| header | direction | description |
|---|---|---|
| `operation` | in | operation for this exchange |
| `indexName` | in | index for this exchange |
| `indexId` | in | document id for `Index`, `Update`, `GetById`, `Delete` |
| `size`, `from` | in | paging for searches |
| `documentClass` | in | typed documents for this exchange |

With `useScroll=true` the exchange property `CamelElasticsearchScrollQueryCount` holds the number
of scroll requests.

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -e ES_HOSTS=es1.example.com:9200 -e ES_USER=app -e ES_PASSWORD \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/articles.camel.yaml
```

## Common questions

### How do I connect to Elastic Cloud?

Use `hostAddresses=<deployment>.es.<region>.cloud.es.io:443&enableSSL=true&apiKey=<encoded key>`.

### Can I run vector (kNN) search?

Send a full search request with a `knn` section as the body; it is passed to Elasticsearch as is.
Computing the query vector is up to you, for example with the [openai](openai.md) embeddings
endpoint. The embedded test server does not implement kNN, so this path is not covered by tests.

### How do I get all results, not just the first page?

Set `useScroll=true`. Every page is fetched with `_search/scroll` and the scroll context is
cleared at the end.

## Related

- [opensearch](opensearch.md): the same component for OpenSearch.
- [AI components guide](../guides/ai-components.md)
- [Apache Camel Elasticsearch component](https://camel.apache.org/components/next/elasticsearch-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
