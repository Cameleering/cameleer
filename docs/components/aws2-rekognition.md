# AWS Rekognition component

> The `aws2-rekognition` component analyses images and stored videos with Amazon Rekognition: labels, text, faces, moderation labels, celebrities and face collections.

**Scheme:** `aws2-rekognition:` · **Crate:** `cameleer-cloud` · **Consumer:** no · **Producer:** yes ·
**Apache Camel:** none (a Cameleer component that follows the naming of the other `aws2-*` components)

## When to use it

- Tag uploaded photos with labels for search.
- Moderate user images before they are published.
- Read text in images (licence plates, labels, signs) from edge cameras.
- Match faces against a collection (access control, photo organisation).

## URI format

```text
aws2-rekognition://label[?options]
```

The image is the S3 object named by `s3Bucket` / `s3Object` (options or headers) or the body bytes.

## Example

Label every photo uploaded to a bucket and queue the labels as JSON:

<!-- example: aws2-rekognition-label-photos -->
```yaml
- route:
    id: label-photos
    from:
      uri: "aws2-s3://uploaded-photos?region={{env:AWS_REGION:us-east-1}}"
      steps:
        - setProperty:
            name: photo
            simple: "${header.CamelAwsS3Key}"
        - to: "aws2-rekognition://photos?region={{env:AWS_REGION:us-east-1}}&operation=detectLabels&minConfidence=80"
        - marshal:
            json: {}
        - setHeader:
            name: photo
            simple: "${exchangeProperty.photo}"
        - to: "aws2-sqs://photo-labels?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true"
```

`detectLabels` returns the list of labels (`Name`, `Confidence`, ...) as the body. The photo key is
sent along as the `photo` message attribute.

## Rust DSL

```rust
ctx.add_routes(|b| {
    b.from("aws2-s3://uploaded-photos?region={{env:AWS_REGION:us-east-1}}")
        .set_property("photo", simple("${header.CamelAwsS3Key}"))
        .to("aws2-rekognition://photos?region={{env:AWS_REGION:us-east-1}}&operation=detectLabels&minConfidence=80")
        .marshal("json")
        .set_header("photo", simple("${exchangeProperty.photo}"))
        .to("aws2-sqs://photo-labels?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
})?;
```

## Options

Common AWS options (region, credentials, endpoint override, proxy) are listed in
[Cloud credentials](../guides/cloud-credentials.md#aws-options).

| option | default | description |
|---|---|---|
| `operation` | `detectLabels` | Images: `detectLabels`, `detectText`, `detectFaces`, `detectModerationLabels`, `recognizeCelebrities`, `compareFaces`. Collections: `createCollection`, `deleteCollection`, `describeCollection`, `listCollections`, `indexFaces`, `listFaces`, `deleteFaces`, `searchFaces`, `searchFacesByImage`. Stored video: `startLabelDetection`, `startFaceDetection`, `startTextDetection`, `startFaceSearch`, `getLabelDetection`, `getFaceDetection`, `getTextDetection`, `getFaceSearch`. |
| `s3Bucket`, `s3Object` | | Image or video in S3 (otherwise the body bytes). |
| `minConfidence` | | Minimum confidence of `detectLabels`. |
| `maxLabels` | | Maximum labels of `detectLabels`. |
| `collectionId` | | Face collection. |

## Message headers

| header | direction | description |
|---|---|---|
| `CamelAwsRekognitionOperation` | in | Overrides `operation`. |
| `CamelAwsRekognitionS3Bucket`, `CamelAwsRekognitionS3Object` | in | Image or video in S3. |
| `CamelAwsRekognitionMaxLabels`, `CamelAwsRekognitionMinConfidence` | in | `detectLabels` limits. |
| `CamelAwsRekognitionCollectionId` | in | Face collection. |
| `CamelAwsRekognitionExternalImageId`, `CamelAwsRekognitionMaxFaces` | in | `indexFaces`, searches. |
| `CamelAwsRekognitionFaceId`, `CamelAwsRekognitionFaceIds`, `CamelAwsRekognitionFaceMatchThreshold` | in | Face searches and deletion. |
| `CamelAwsRekognitionTargetS3Bucket`, `CamelAwsRekognitionTargetS3Object`, `CamelAwsRekognitionTargetImage` | in | Target image of `compareFaces`. |
| `CamelAwsRekognitionUnmatchedFaces` | out | Unmatched faces of `compareFaces`. |
| `CamelAwsRekognitionJobId`, `CamelAwsRekognitionJobStatus` | in/out | Stored video jobs. |

## Run it with Docker

```sh
docker run --rm -e AWS_REGION -e AWS_ACCESS_KEY_ID -e AWS_SECRET_ACCESS_KEY \
  -v "$PWD:/routes:ro" pmsudhi/cameleer run /routes/photos.camel.yaml
```

## Common questions

### How do I analyse an image without downloading it?

Pass the S3 location with `s3Bucket` and `s3Object` (or the headers); Rekognition reads it directly.
With the S3 consumer, `ignoreBody=true` avoids downloading the image.

### How do I analyse a video?

Start a job (`startLabelDetection` with the video in S3), keep `CamelAwsRekognitionJobId`, then call
the matching `get...` operation until `CamelAwsRekognitionJobStatus` is `SUCCEEDED`.

## Related

- [AWS S3](aws2-s3.md), [AWS Textract](aws2-textract.md)
- [Cloud credentials](../guides/cloud-credentials.md)
