# AWS Textract component

> The `aws2-textract` component extracts text, forms, tables and invoice fields from documents and images with Amazon Textract.

**Scheme:** `aws2-textract:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** [AWS Textract](https://camel.apache.org/components/next/aws2-textract-component.html)

## When to use it

- Read invoices and receipts dropped into a bucket and send their fields to an ERP.
- Turn scanned PDFs into text for search or for an LLM.
- Give an AI agent a document-reading tool: the route returns structured fields the model can use.

## URI format

```text
aws2-textract://label?operation=...[&options]
```

The document is the S3 object named by `s3Bucket` / `s3Object` (options or headers) or, for the
synchronous operations, the body bytes.

## Example

Extract invoice fields from every file uploaded to a bucket and queue them as JSON:

<!-- example: aws2-textract-invoices -->
```yaml
- route:
    id: extract-invoices
    from:
      uri: "aws2-s3://scanned-invoices?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - to: "aws2-textract://invoices?region={{env:AWS_REGION:us-east-1}}&operation=analyzeExpense"
        - marshal:
            json: {}
        - to: "aws2-sqs://extracted-invoices?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
```

The S3 consumer passes the object content as the body; `analyzeExpense` returns the Textract
response (with `ExpenseDocuments` and their `SummaryFields`) as a map.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-s3://scanned-invoices?region={{env:AWS_REGION:us-east-1}}")
        .to("aws2-textract://invoices?region={{env:AWS_REGION:us-east-1}}&operation=analyzeExpense")
        .marshal("json")
        .to("aws2-sqs://extracted-invoices?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `detectDocumentText` | `detectDocumentText`, `analyzeDocument`, `analyzeExpense`, `startDocumentTextDetection`, `startDocumentAnalysis`, `startExpenseAnalysis`, `getDocumentTextDetection`, `getDocumentAnalysis`, `getExpenseAnalysis`. |
| `s3Bucket`, `s3Object`, `s3ObjectVersion` | | Document in S3 (required for the asynchronous `start*` operations). |

The synchronous operations return the response map (`Blocks` or `ExpenseDocuments`). The `start*`
operations return the job id (also in `CamelAwsTextractJobId`); the `get*` operations return the
job status and results.

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsTextractOperation` | in | Overrides `operation`. |
| `CamelAwsTextractS3Bucket`, `CamelAwsTextractS3Object`, `CamelAwsTextractS3ObjectVersion` | in | Document in S3. |
| `CamelAwsTextractFeatureTypes` | in | `analyzeDocument` features (default `TABLES,FORMS`). |
| `CamelAwsTextractJobId` | in/out | Job of the asynchronous operations. |
| `CamelAwsTextractMaxResults`, `CamelAwsTextractNextToken` | in | Paging of the `get*` operations. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/invoices.camel.yaml
```

## Common questions

### How do I process multi-page PDFs?

Use the asynchronous operations: `startDocumentAnalysis` with `s3Bucket` / `s3Object`, keep the job
id, then call `getDocumentAnalysis` until `JobStatus` is `SUCCEEDED`.

### How do I get only the text lines?

Use `detectDocumentText` and keep the blocks whose `BlockType` is `LINE`, for example with a
`split` on `${body[Blocks]}` and a `filter`.

## Related

- [AWS S3](aws2-s3.md), [AWS Comprehend](aws2-comprehend.md), [AWS Bedrock](aws-bedrock.md)
- [Cloud credentials](../guides/cloud-credentials.md)
- [Apache Camel AWS Textract](https://camel.apache.org/components/next/aws2-textract-component.html)
