# Azure Cosmos DB component

> The `azure-cosmosdb` component creates, reads, upserts, queries and deletes items in Azure Cosmos DB (SQL API) and consumes a container's change feed.

**Scheme:** `azure-cosmosdb:` · **Crate:** `cameleer-cloud` · **Consumer:** yes · **Producer:** yes ·
**Apache Camel:** [Azure CosmosDB](https://camel.apache.org/components/next/azure-cosmosdb-component.html)

## When to use it

- Store orders, device twins or user profiles from routes.
- Query items with SQL and parameters to enrich messages.
- Change data capture: react to every inserted or updated item through the change feed, with
  leases shared between several instances.
- Keep the conversation state or memory of an AI agent in a globally distributed store.

## URI format

```text
azure-cosmosdb://databaseName[/containerName][?options]
```

## Example

Upsert an order, creating the database and the container on first use:

<!-- example: azure-cosmosdb-upsert-order -->
```yaml
- route:
    id: save-order
    from:
      uri: "timer:order?repeatCount=1"
      steps:
        - setBody:
            constant: '{"id":"o-1001","customerId":"c-42","total":99.5}'
        - unmarshal:
            json: {}
        - setHeader:
            name: CamelAzureCosmosDbItemPartitionKey
            simple: "${body[customerId]}"
        - to: "azure-cosmosdb://shop/orders?databaseEndpoint={{env:COSMOS_ENDPOINT:https://myaccount.documents.azure.com:443/}}&accountKey=RAW({{env:COSMOS_KEY:}})&operation=upsertItem&createDatabaseIfNotExists=true&createContainerIfNotExists=true&containerPartitionKeyPath=/customerId"
        - log: "saved order (status ${header.CamelAzureCosmosDbStatusCode})"
```

Consume the change feed of the container from the beginning. Each non-empty batch arrives as a
list of the changed items:

<!-- example: azure-cosmosdb-change-feed -->
```yaml
- route:
    id: order-changes
    from:
      uri: "azure-cosmosdb://shop/orders?databaseEndpoint={{env:COSMOS_ENDPOINT:https://myaccount.documents.azure.com:443/}}&accountKey=RAW({{env:COSMOS_KEY:}})&changeFeedStartFromBeginning=true"
      steps:
        - log: "changed orders: ${body}"
        - to: "log:order-changes"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-cosmosdb://shop/orders?databaseEndpoint={{env:COSMOS_ENDPOINT:https://myaccount.documents.azure.com:443/}}&accountKey=RAW({{env:COSMOS_KEY:}})&changeFeedStartFromBeginning=true")
        .log("changed orders: ${body}")
        .to("log:order-changes");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `databaseEndpoint` | | Account endpoint, for example `https://myaccount.documents.azure.com:443/` (or the emulator). |
| `accountKey` | | Primary or secondary key (master key authorization). |
| `credentialType` | `SHARED_ACCOUNT_KEY` | `SHARED_ACCOUNT_KEY`, or `AZURE_IDENTITY` for Microsoft Entra ID (client secret, workload or managed identity; see [Cloud credentials](../guides/cloud-credentials.md#azure)). |
| `operation` | `listDatabases` | Databases: `listDatabases`, `createDatabase`, `queryDatabases`, `deleteDatabase`. Containers: `createContainer`, `listContainers`, `queryContainers`, `deleteContainer`. Items: `createItem`, `upsertItem`, `replaceItem`, `deleteItem`, `readItem`, `readAllItems`, `queryItems`. Throughput: `replaceDatabaseThroughput`, `replaceContainerThroughput`, `readDatabaseThroughput`, `readContainerThroughput` (the read ones are Cameleer extensions). |
| `query` | | SQL of `queryItems` (parameters from `CamelAzureCosmosDbQueryParameters`). |
| `itemId`, `itemPartitionKey` | | Item of `readItem` / `deleteItem`, and the partition key value. |
| `containerPartitionKeyPath` | | Partition key path of a created container, for example `/customerId`. |
| `createDatabaseIfNotExists`, `createContainerIfNotExists` | `false` | Create them when the endpoint starts. |
| `throughputProperties` | | RU/s of created resources (a number, or autoscale settings via the header). |
| `consistencyLevel` | | `STRONG`, `BOUNDED_STALENESS`, `SESSION`, `CONSISTENT_PREFIX`, `EVENTUAL`. |
| `contentResponseOnWriteEnabled` | `true` | `false` sends `Prefer: return=minimal` (writes return no item). |
| `changeFeedStartFromBeginning` | `false` | Cameleer: the consumer starts at the beginning of the change feed instead of now. |
| `leaseContainerName`, `leaseDatabaseName`, `createLeaseContainerIfNotExists`, `createLeaseDatabaseIfNotExists`, `hostName`, `leaseExpirationInterval` | | Consumer as a change feed processor: one lease per partition key range, balanced between hosts (`leaseExpirationInterval` ms, default 60000). Without a lease container the continuations are kept in memory. |
| `uriEndpointOverride`, `proxyHost`, `proxyPort`, `trustAllCertificates` | | Connection settings. |

Cameleer always talks to the REST gateway; the SDK's direct (TCP) connection mode options do not apply.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureCosmosDbOperation` | in | Overrides `operation`. |
| `CamelAzureCosmosDbDatabaseName`, `CamelAzureCosmosDbContainerName` | in | Override the database and container. |
| `CamelAzureCosmosDbQuery`, `CamelAzureCosmosDbQueryParameters` | in | SQL and its parameters (map). |
| `CamelAzureCosmosDbItemId`, `CamelAzureCosmosDbItemPartitionKey` | in | Item and partition key value. |
| `CamelAzureCosmosDbContainerPartitionKeyPath` | in | Partition key path of `createContainer`. |
| `CamelAzureCosmosDbThroughputProperties` | in | RU/s (number) or `{throughput}` / `{autoscaleMaxThroughput}`. |
| `CamelAzureCosmosDbResourceId`, `CamelAzureCosmosDbEtag`, `CamelAzureCosmosDbStatusCode`, `CamelAzureCosmosDbResponseHeaders` | out | Response details. |

## Run it with Docker

```sh
docker run --rm -e COSMOS_ENDPOINT -e COSMOS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### How do I query with parameters?

Set `query=SELECT * FROM c WHERE c.customerId = @customer` and the header
`CamelAzureCosmosDbQueryParameters` to `{"@customer": "c-42"}`; all result pages are returned as a list.

### How do I run the change feed on several instances?

Add `leaseContainerName=leases&createLeaseContainerIfNotExists=true`: each instance takes a fair
share of the partition key ranges and checkpoints its progress in the lease container.

### Does it work with the Cosmos DB emulator?

Yes: point `databaseEndpoint` at the emulator, for example `https://localhost:8081/`, with the
emulator's key.

## Related

- [AWS DynamoDB](aws2-ddb.md), [Azure Storage Blob](azure-storage-blob.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel Azure Cosmos DB](https://camel.apache.org/components/next/azure-cosmosdb-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
