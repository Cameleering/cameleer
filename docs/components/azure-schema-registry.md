# Azure Schema Registry component

> The `azure-schema-registry` component registers and looks up Avro, JSON and custom schemas in the Azure Event Hubs Schema Registry.

**Scheme:** `azure-schema-registry:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** none (Apache Camel offers the registry only as a Kafka serializer library; this is a Cameleer component)

## When to use it

- Register the schema of the events a route produces, as part of a deployment.
- Look up a schema by id when a message carries only the schema id.
- List the versions of a schema for compatibility checks.

## URI format

```text
azure-schema-registry://namespace/group[?options]
```

`namespace` is the Event Hubs namespace (`myns` means `https://myns.servicebus.windows.net`, or give a
fully qualified name); `group` is the schema group.

## Example

Register an Avro schema when the route starts:

<!-- example: azure-schema-registry-register -->
```yaml
- route:
    id: register-order-schema
    from:
      uri: "timer:register-schema?repeatCount=1"
      steps:
        - setBody:
            constant: '{"type":"record","name":"Order","namespace":"com.example","fields":[{"name":"id","type":"string"},{"name":"total","type":"double"}]}'
        - to: "azure-schema-registry://{{env:EVENTHUBS_NAMESPACE:my-namespace}}/orders?operation=registerSchema&schemaName=com.example.Order&tenantId={{env:AZURE_TENANT_ID:}}&clientId={{env:AZURE_CLIENT_ID:}}&clientSecret=RAW({{env:AZURE_CLIENT_SECRET:}})"
        - log: "schema id ${body}, version ${header.CamelAzureSchemaRegistrySchemaVersion}"
        - to: "log:schemas"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("timer:register-schema?repeatCount=1")
        .set_body(constant(r#"{"type":"record","name":"Order","namespace":"com.example","fields":[{"name":"id","type":"string"},{"name":"total","type":"double"}]}"#))
        .to("azure-schema-registry://{{env:EVENTHUBS_NAMESPACE:my-namespace}}/orders?operation=registerSchema&schemaName=com.example.Order&tenantId={{env:AZURE_TENANT_ID:}}&clientId={{env:AZURE_CLIENT_ID:}}&clientSecret=RAW({{env:AZURE_CLIENT_SECRET:}})")
        .log("schema id ${body}, version ${header.CamelAzureSchemaRegistrySchemaVersion}")
        .to("log:schemas");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `operation` | `getSchema` | `registerSchema`, `getSchemaProperties` (id of the body's definition), `getSchema` (by id), `getSchemaByVersion`, `listVersions`, `listGroups`. |
| `schemaName` | | Schema name. |
| `schemaId` | | Schema id of `getSchema`. |
| `format` | `Avro` | `Avro`, `Json` or `Custom`. |
| `credentialType` | `CLIENT_SECRET` | `CLIENT_SECRET` (`tenantId`, `clientId`, `clientSecret`, `authorityHost`) or `AZURE_IDENTITY`. |
| `uriEndpointOverride`, `proxyHost`, `proxyPort`, `trustAllCertificates` | | Connection settings. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureSchemaRegistryOperation` | in | Overrides `operation`. |
| `CamelAzureSchemaRegistrySchemaName`, `CamelAzureSchemaRegistryFormat` | in/out | Name and format. |
| `CamelAzureSchemaRegistrySchemaId` | in/out | Schema id. |
| `CamelAzureSchemaRegistrySchemaVersion` | in/out | Version (input of `getSchemaByVersion`). |
| `CamelAzureSchemaRegistrySchemaGroup` | out | Group of the schema. |

## Run it with Docker

```sh
docker run --rm -e EVENTHUBS_NAMESPACE -e AZURE_TENANT_ID -e AZURE_CLIENT_ID -e AZURE_CLIENT_SECRET \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/schemas.camel.yaml
```

## Common questions

### Does it serialize Kafka or Event Hubs messages?

No. It manages schemas through the REST API; serialize with the `avro` or `json` data formats and use
the schema id in a header.

## Related

- [Azure Event Hubs](azure-eventhubs.md)
- [Cloud credentials](../guides/cloud-credentials.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
