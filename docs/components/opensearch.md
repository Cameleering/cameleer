# OpenSearch component

> The `opensearch` component indexes, gets, deletes and searches documents in OpenSearch (including Amazon OpenSearch Service with basic authentication) over its REST API.

**Scheme:** `opensearch:` · **Crate:** `cameleer-datastores` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [OpenSearch](https://camel.apache.org/components/next/opensearch-component.html)

## When to use it

- Ship application logs or events into OpenSearch in batches with one bulk request.
- Full-text search as the retrieval step of an AI agent or a support chatbot.
- Keep a search index in step with a database: index on insert, delete on removal.

The `opensearch` scheme is the [elasticsearch](elasticsearch.md) component registered under a
second name: the operations, options and headers are the same, and so is the REST API they use.

## URI format

```text
opensearch://clusterName?hostAddresses=host:9200&operation=<op>[&indexName=<index>][&options]
```

## Example

`POST /logs` takes a JSON array of log records and indexes them with one `_bulk` request into
the `app-logs` index. The answer is the list of per-document results.

<!-- example: opensearch-bulk-logs -->
```yaml
- route:
    id: bulk-logs
    from:
      uri: "platform-http:/logs?httpMethodRestrict=POST"
      steps:
        - unmarshal:
            json: {}
        - to: "opensearch://logs?hostAddresses={{env:OPENSEARCH_HOSTS:localhost:9200}}&user={{env:OPENSEARCH_USER:admin}}&password={{env:OPENSEARCH_PASSWORD:admin}}&operation=Bulk&indexName=app-logs"
        - marshal:
            json: {}
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("direct:logs")
        .to("opensearch://logs?hostAddresses=localhost:9200&operation=Bulk&indexName=app-logs");
    b.from("direct:find")
        .to("opensearch://logs?hostAddresses=localhost:9200&operation=Search&indexName=app-logs&size=10&enableDocumentOnlyMode=true");
})?;
```

## Options

The most used options are below; the full list, with scrolling, sniffing and typed documents,
is on the [elasticsearch](elasticsearch.md#options) page.

| option | default | description |
|---|---|---|
| `hostAddresses` | `localhost:9200` | comma-separated `host:port` list, tried in order |
| `operation` | | `Index`, `Update`, `Bulk`, `GetById`, `MultiGet`, `Delete`, `DeleteIndex`, `Exists`, `Search`, `MultiSearch`, `Ping` (header `operation` overrides it) |
| `indexName` | | index (header `indexName` overrides it) |
| `from`, `size` | | paging for searches |
| `enableDocumentOnlyMode` | `false` | return `_source` documents instead of response objects |
| `user`, `password` | | basic authentication |
| `apiKey` | | sent as `Authorization: ApiKey <value>` |
| `enableSSL` | `false` | HTTPS; trust from `sslContextParameters=#bean`, the global `camel.ssl.*` parameters or the system store |
| `useScroll` | `false` | run `Search` as a scroll search and return all hits |

## Message headers

| header | direction | description |
|---|---|---|
| `operation` | in | operation for this exchange |
| `indexName` | in | index for this exchange |
| `indexId` | in | document id for `Index`, `Update`, `GetById`, `Delete` |
| `size`, `from` | in | paging for searches |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 \
  -e OPENSEARCH_HOSTS=search.example.com:9200 -e OPENSEARCH_USER=app -e OPENSEARCH_PASSWORD \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/logs.camel.yaml
```

## Common questions

### Does it sign requests for Amazon OpenSearch Service (SigV4)?

No. Use a domain with fine-grained access control and basic authentication (`user` and
`password`), or an API key. AWS SigV4 request signing is not implemented for this component.

### What does a bulk body look like?

A list of documents (each is indexed with a generated id), or a list of actions such as
`{"index": {"_id": "1"}, "doc": {...}}`, `{"update": {"_id": "1"}, "doc": {...}}` and
`{"delete": {"_id": "2"}}`. The two forms can be mixed.

### What is the difference from the elasticsearch component?

Only the scheme name. Both send the same REST requests, which OpenSearch and Elasticsearch
share for these operations.

## Related

- [elasticsearch](elasticsearch.md): all operations and options.
- [Apache Camel OpenSearch component](https://camel.apache.org/components/next/opensearch-component.html)
