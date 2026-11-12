# AWS Lambda component

> The `aws2-lambda` component invokes AWS Lambda functions and manages functions, versions, aliases, tags and event source mappings.

**Scheme:** `aws2-lambda:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Lambda](https://camel.apache.org/components/next/aws2-lambda-component.html)

## When to use it

- Call serverless business logic from a route and continue with its result.
- Offload heavy work (image resizing, PDF rendering, model inference) from an edge device to Lambda.
- Expose a Lambda function to an AI agent as a tool behind a queue or an HTTP route.
- Deploy or update functions from a CI route (`createFunction`, `updateFunction`, `publishVersion`).

## URI format

```text
aws2-lambda://functionNameOrArn[?options]
```

## Example

Invoke a function for each message of a queue and send its response to another queue:

<!-- example: aws2-lambda-invoke-function -->
```yaml
- route:
    id: resize-images
    from:
      uri: "aws2-sqs://images-to-resize?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - to: "aws2-lambda://resize-image?region={{env:AWS_REGION:us-east-1}}&operation=invokeFunction"
        - to: "aws2-sqs://resized-images?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
```

The body is sent as the payload; the response payload becomes the body. When the function fails,
the `CamelAwsLambdaFunctionError` header is set and the body holds the error document.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://images-to-resize?region={{env:AWS_REGION:us-east-1}}")
        .to("aws2-lambda://resize-image?region={{env:AWS_REGION:us-east-1}}&operation=invokeFunction")
        .to("aws2-sqs://resized-images?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `invokeFunction` | `invokeFunction`, `listFunctions`, `getFunction`, `deleteFunction`, `createFunction`, `updateFunction`, `createEventSourceMapping`, `deleteEventSourceMapping`, `listEventSourceMapping`, `listTags`, `tagResource`, `untagResource`, `publishVersion`, `listVersions`, `createAlias`, `deleteAlias`, `getAlias`, `listAliases`. The `CamelAwsLambdaOperation` header overrides it. |

Every operation except `invokeFunction` sets the response document (a map, or a list for the list
operations) as the body.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsLambdaOperation` | in | Overrides `operation`. |
| `CamelAwsLambdaInvocationType` | in | `RequestResponse` (default), `Event` (asynchronous) or `DryRun`. |
| `CamelAwsLambdaFunctionError` | out | Set when the function returned an error. |
| `CamelAwsLambdaStatusCode` | out | HTTP status of the invocation. |
| `CamelAwsLambdaS3Bucket`, `CamelAwsLambdaS3Key`, `CamelAwsLambdaS3ObjectVersion` | in | Code location of `createFunction` / `updateFunction` (otherwise the body is the zip). |
| `CamelAwsLambdaRole`, `CamelAwsLambdaRuntime`, `CamelAwsLambdaHandler`, `CamelAwsLambdaDescription`, `CamelAwsLambdaTargetArn`, `CamelAwsLambdaMemorySize`, `CamelAwsLambdaKMSKeyArn`, `CamelAwsLambdaPublish`, `CamelAwsLambdaTimeout`, `CamelAwsLambdaTracingConfig`, `CamelAwsLambdaEnvironmentVariables`, `CamelAwsLambdaTags`, `CamelAwsLambdaSecurityGroupIds`, `CamelAwsLambdaSubnetIds` | in | Function settings of `createFunction`. |
| `CamelAwsLambdaEventSourceArn`, `CamelAwsLambdaEventSourceBatchSize`, `CamelAwsLambdaEventSourceUuid`, `CamelAwsLambdaEventSourceStartingPosition` | in | Event source mappings (`StartingPosition` is a Cameleer extension). |
| `CamelAwsLambdaResourceArn`, `CamelAwsLambdaResourceTags`, `CamelAwsLambdaResourceTagKeys` | in | Tag operations. |
| `CamelAwsLambdaVersionDescription`, `CamelAwsLambdaFunctionVersion`, `CamelAwsLambdaAliasFunctionName`, `CamelAwsLambdaAliasFunctionDescription` | in | Versions and aliases. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" cameleering/cameleer run /routes/resize.camel.yaml
```

## Common questions

### How do I invoke a function asynchronously?

Set the header `CamelAwsLambdaInvocationType` to `Event`. Lambda queues the event and returns at once.

### How do I call a specific version or alias?

Use the qualified ARN as the path, for example `aws2-lambda://arn:aws:lambda:eu-west-1:123456789012:function:resize-image:prod`.

### How does the route know the function failed?

A function error does not throw: check `${header.CamelAwsLambdaFunctionError}` (for example in a
`choice`) and read the error document from the body.

## Related

- [AWS SQS](aws2-sqs.md), [AWS Step Functions](aws2-step-functions.md), [AWS EventBridge](aws2-eventbridge.md)
- [Azure Functions](azure-functions.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Lambda](https://camel.apache.org/components/next/aws2-lambda-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/cameleering/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/cameleering/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
