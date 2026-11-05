# Azure Functions component

> The `azure-functions` component invokes HTTP-triggered Azure Functions and returns their response.

**Scheme:** `azure-functions:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** none (a Cameleer component)

## When to use it

- Call serverless business logic on Azure from a route and continue with the result.
- Expose an Azure Function to an AI agent as a tool behind a queue.
- Offload heavy processing from an edge gateway to a function.

## URI format

```text
azure-functions://functionApp/functionName[?options]
```

The request goes to `https://{functionApp}.azurewebsites.net/api/{functionName}`.

## Example

Score each lead of a queue with a function and parse its JSON answer:

<!-- example: azure-functions-score-leads -->
```yaml
- route:
    id: score-leads
    from:
      uri: "azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/leads?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})"
      steps:
        - to: "azure-functions://{{env:FUNCTION_APP:my-function-app}}/score-lead?functionKey=RAW({{env:FUNCTION_KEY:}})&parseJson=true"
        - log: "lead score ${body[score]} (HTTP ${header.CamelAzureFunctionsStatusCode})"
        - to: "log:scores"
```

The body is sent as is (maps and lists as JSON) with `POST`; the response text becomes the body, or
a map/list with `parseJson=true`.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/leads?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
        .to("azure-functions://{{env:FUNCTION_APP:my-function-app}}/score-lead?functionKey=RAW({{env:FUNCTION_KEY:}})&parseJson=true")
        .log("lead score ${body[score]} (HTTP ${header.CamelAzureFunctionsStatusCode})")
        .to("log:scores");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `functionKey` | | Function key, sent in the `x-functions-key` header. |
| `masterKey` | | Host master key (instead of a function key). |
| `keyInQuery` | `false` | Send the key as the `code` query parameter instead. |
| `httpMethod` | `POST` | HTTP method (`GET` and `DELETE` send no body). |
| `parseJson` | `false` | Parse JSON responses into maps and lists. |
| `credentialType`, `tenantId`, `clientId`, `clientSecret`, `scope` | | Microsoft Entra ID tokens for functions protected by App Service authentication. |
| `uriEndpointOverride` | | Base URL instead of `https://{functionApp}.azurewebsites.net` (for example the local Functions host). |
| `proxyHost`, `proxyPort`, `trustAllCertificates` | | Connection settings. |

Anonymous functions need no key. A non-2xx response raises an `HttpResponseException` with the status
in the message.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAzureFunctionsHttpMethod` | in | Overrides `httpMethod`. |
| `CamelAzureFunctionsQuery` | in | Extra query parameters (`a=1&b=2`). |
| `CamelAzureFunctionsStatusCode` | out | HTTP status. |
| `CamelAzureFunctionsContentType` | out | Content type of the response. |

## Run it with Docker

```sh
docker run --rm -e AZURE_STORAGE_ACCOUNT -e AZURE_STORAGE_KEY -e FUNCTION_APP -e FUNCTION_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/leads.camel.yaml
```

## Common questions

### How do I call a function running locally?

Run `func start` and set `uriEndpointOverride=http://localhost:7071`; the path `/api/{functionName}`
is added.

### How do I handle errors from the function?

Non-2xx responses throw, so `onException` or `doTry` in the route can catch them and retry or route
the message elsewhere.

## Related

- [AWS Lambda](aws2-lambda.md), [Azure Storage Queue](azure-storage-queue.md)
- [Cloud credentials](../guides/cloud-credentials.md)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
