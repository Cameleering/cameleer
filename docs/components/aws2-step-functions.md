# AWS Step Functions component

> The `aws2-step-functions` component starts and manages AWS Step Functions workflows (state machines, executions and activities).

**Scheme:** `aws2-step-functions:` · **Crate:** `cameleer-cloud` · **Cargo feature:** [`cloud`](../guides/build-features.md) · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Step Functions](https://camel.apache.org/components/next/aws2-step-functions-component.html)

## When to use it

- Start a serverless workflow (order fulfilment, document processing) for each incoming event.
- Run a synchronous Express workflow and continue the route with its output (`startSyncExecution`).
- Implement activity workers in a route: fetch a task, do the work, report success or failure.
- Orchestrate long-running AI agent steps (approval, retries, human review) in Step Functions while
  Cameleer handles the connections to your systems.

## URI format

```text
aws2-step-functions://label?operation=...[&options]
```

## Example

Start a workflow execution for every order, with the order JSON as input:

<!-- example: aws2-step-functions-start-orders -->
```yaml
- route:
    id: start-order-workflow
    from:
      uri: "aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setHeader:
            name: CamelAwsStepFunctionsStateMachineArn
            constant: "{{env:STATE_MACHINE_ARN:arn:aws:states:us-east-1:123456789012:stateMachine:order-fulfilment}}"
        - to: "aws2-step-functions://orders?region={{env:AWS_REGION:us-east-1}}&operation=startExecution"
        - log: "started ${body}"
        - to: "log:executions"
```

The body is the execution input (JSON); after `startExecution` the body is the execution ARN (also in
`CamelAwsStepFunctionsExecutionArn`).

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
        .set_header("CamelAwsStepFunctionsStateMachineArn", constant("{{env:STATE_MACHINE_ARN:arn:aws:states:us-east-1:123456789012:stateMachine:order-fulfilment}}"))
        .to("aws2-step-functions://orders?region={{env:AWS_REGION:us-east-1}}&operation=startExecution")
        .log("started ${body}")
        .to("log:executions");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | | Required. State machines: `createStateMachine`, `deleteStateMachine`, `updateStateMachine`, `describeStateMachine`, `listStateMachines`. Executions: `startExecution`, `startSyncExecution`, `stopExecution`, `describeExecution`, `listExecutions`, `getExecutionHistory`. Activities: `createActivity`, `deleteActivity`, `describeActivity`, `listActivities`, `getActivityTask`, and the Cameleer worker callbacks `sendTaskSuccess`, `sendTaskFailure`, `sendTaskHeartbeat`. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsStepFunctionsOperation` | in | Overrides `operation`. |
| `CamelAwsStepFunctionsStateMachineName`, `CamelAwsStepFunctionsStateMachineDefinition`, `CamelAwsStepFunctionsStateMachineRoleArn` | in | State machine settings (the definition may also be the body). |
| `CamelAwsStepFunctionsStateMachineArn` | in | State machine. |
| `CamelAwsStepFunctionsExecutionInput`, `CamelAwsStepFunctionsExecutionName` | in | Input (otherwise the body) and name of an execution. |
| `CamelAwsStepFunctionsExecutionArn` | in/out | Execution. |
| `CamelAwsStepFunctionsExecutionStatusFilter` | in | Status filter of `listExecutions`. |
| `CamelAwsStepFunctionsActivityName`, `CamelAwsStepFunctionsActivityArn`, `CamelAwsStepFunctionsActivityWorkerName` | in/out | Activities. |
| `CamelAwsStepFunctionsTaskToken` | in/out | Token of an activity task (set by `getActivityTask`). |
| `CamelAwsStepFunctionsTaskError`, `CamelAwsStepFunctionsTaskCause` | in | `sendTaskFailure`. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY -e STATE_MACHINE_ARN \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/orders.camel.yaml
```

## Common questions

### How do I write an activity worker?

Poll with `getActivityTask` (body = task input, `CamelAwsStepFunctionsTaskToken` set; empty body when
no task is waiting), do the work, then call `sendTaskSuccess` with the output as body, or
`sendTaskFailure` with an error and cause.

### How do I get the workflow result in the route?

For Express workflows use `startSyncExecution`: the body becomes the execution result.

## Related

- [AWS Lambda](aws2-lambda.md), [AWS SQS](aws2-sqs.md), [AWS EventBridge](aws2-eventbridge.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Step Functions](https://camel.apache.org/components/next/aws2-step-functions-component.html)

## Found a problem?

If something on this page doesn't match what Cameleer does, or you hit a bug while building
with it, please help fix it, whether you are a person or a coding agent:
[open an issue](https://github.com/pmsudhi/cameleer/issues/new) that describes what you saw,
then fork the repository, fix it, add or update a test, run `cargo test --workspace`, and open
a pull request. [CONTRIBUTING.md](https://github.com/pmsudhi/cameleer/blob/main/CONTRIBUTING.md)
explains the ground rules.
