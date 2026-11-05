# Google Cloud Functions component

> The `google-functions` component calls Google Cloud Functions (1st gen) and manages them: list, get, create, update, delete and source upload URLs.

**Scheme:** `google-functions:` · **Crate:** `cameleer-cloud` ·
**Consumer:** no · **Producer:** yes · **Apache Camel:** [Google Functions](https://camel.apache.org/components/next/google-functions-component.html)

## When to use it

- Call a function from a route with the message body as input and its result as the new body.
- Offload one heavy step (image resize, scoring, an ML model) to a serverless function.
- Give an AI agent a tool: the agent's request goes through a route that calls the function.
- Deploy or update a function from a pipeline (`createFunction`, `updateFunction`).

## URI format

```text
google-functions://functionName[?options]
```

## Example

Expose a function behind an HTTP endpoint; the request body is the call data and the response is
the function's result:

<!-- example: google-functions-call-from-http -->
```yaml
- route:
    id: http-to-function
    from:
      uri: "platform-http:/greet?httpMethodRestrict=POST"
      steps:
        - to: "google-functions://{{env:FUNCTION_NAME:greet}}?project={{env:GCP_PROJECT:my-project}}&location={{env:FUNCTION_LOCATION:europe-west1}}"
        - log: "execution ${header.CamelGoogleCloudFunctionsExecutionId}: ${body}"
```

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("platform-http:/greet")
        .to("google-functions://greet?project=my-project&location=europe-west1");
})?;
```

## Options

| option | default | description |
|---|---|---|
| `project` | the key's project | Google Cloud project. |
| `location` | `us-central1` | Region of the function. |
| `operation` | `callFunction` | `callFunction`, `listFunctions`, `getFunction`, `deleteFunction`, `generateUploadUrl`, `generateDownloadUrl`, `createFunction`, `updateFunction`. Header `CamelGoogleCloudFunctionsOperation`. |
| `entryPoint` | | `createFunction` / `updateFunction`: entry point. |
| `runtime` | | `createFunction` / `updateFunction`: runtime, for example `nodejs20`. |

`client` and `pojoRequest` are accepted for Camel compatibility and ignored. Credentials are
shared by all Google components: see [credentials](google-storage.md#credentials).

### Operations

- `callFunction`: the body (text, or a map / list sent as JSON) is the call data; the body
  becomes the result. A function error raises an `ApiException`.
- `listFunctions`: body = list of function names. `getFunction`: the function resource.
  `deleteFunction`: deletes it.
- `generateUploadUrl`: body = a signed upload URL; with a byte body the zip archive is uploaded
  to it right away. `generateDownloadUrl`: body = the URL of the function's source.
- `createFunction` / `updateFunction`: from the entry point, runtime and source headers (HTTPS
  trigger by default), or a map body that is the CloudFunction resource. The long-running
  operation is polled until done; body = the function.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelGoogleCloudFunctionsOperation` | in | Operation. |
| `CamelGoogleCloudFunctionsExecutionId` | out | Cameleer: execution id of `callFunction`. |
| `CamelGoogleCloudFunctionsEntryPoint` | in | Entry point for create / update. |
| `CamelGoogleCloudFunctionsRuntime` | in | Runtime for create / update. |
| `CamelGoogleCloudFunctionsSourceArchiveUrl` | in | `gs://` source archive for create / update. |
| `CamelGoogleCloudFunctionsSourceUploadUrl` | in | Upload URL (from `generateUploadUrl`) for create / update. |

## Run it with Docker

```sh
docker run --rm -p 8080:8080 -v "$PWD:/routes:ro" \
  -v "$HOME/keys/sa.json:/keys/sa.json:ro" -e GOOGLE_APPLICATION_CREDENTIALS=/keys/sa.json \
  -e GCP_PROJECT=my-project -e FUNCTION_NAME=greet \
  pmsudhi/cameleer run /routes/http-to-function.camel.yaml
```

## Common questions

### How do I call a Cloud Function with JSON?
Send a map or list body (for example after `unmarshal: json`); it is serialized as the call data.

### Does it work with 2nd gen functions or Cloud Run?
It uses the Cloud Functions v1 API (`functions:call`). For 2nd gen functions and Cloud Run
services call their HTTPS URL with the [HTTP component](http.md) and a bearer token.

### What happens when the function throws?
The exchange fails with an `ApiException` carrying the function's error message, so the route's
error handler decides what to do.

## Related

- [AWS Lambda](aws2-lambda.md), [Azure Functions](azure-functions.md)
- [Apache Camel Google Functions](https://camel.apache.org/components/next/google-functions-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
