//! # rust-camel cloud components
//!
//! Camel's most used cloud components, implemented over the services' REST APIs with a
//! small std-only HTTPS client ([`http`], TLS from `camel-tls`):
//!
//! | scheme | module | service / protocol | auth |
//! |---|---|---|---|
//! | `aws2-s3` | [`aws::s3`] | Amazon S3 REST (XML), multipart / streaming | SigV4 |
//! | `aws2-sqs` | [`aws::sqs`] | Amazon SQS (AWS JSON 1.0) | SigV4 |
//! | `aws2-sns` | [`aws::sns`] | Amazon SNS (Query API) | SigV4 |
//! | `aws2-ddb` | [`aws::ddb`] | Amazon DynamoDB (AWS JSON 1.0) | SigV4 |
//! | `aws2-lambda` | [`aws::lambda`] | AWS Lambda REST | SigV4 |
//! | `aws2-kinesis` | [`aws::kinesis`] | Kinesis Data Streams (AWS JSON 1.1) | SigV4 |
//! | `aws2-kinesis-firehose` | [`aws::firehose`] | Kinesis Data Firehose (AWS JSON 1.1) | SigV4 |
//! | `aws2-eventbridge` | [`aws::eventbridge`] | EventBridge (AWS JSON 1.1) | SigV4 |
//! | `aws-secrets-manager` | [`aws::secretsmanager`] | Secrets Manager (AWS JSON 1.1), `{{aws:...}}` | SigV4 |
//! | `aws2-ses` | [`aws::ses`] | SES v1 (Query API) | SigV4 |
//! | `aws2-cw` | [`aws::cw`] | CloudWatch (Query API) | SigV4 |
//! | `aws2-sts` | [`aws::sts`] | STS (Query API) | SigV4 |
//! | `aws2-step-functions` | [`aws::stepfunctions`] | Step Functions (AWS JSON 1.0) | SigV4 |
//! | `aws2-iam` | [`aws::iam`] | IAM (Query API) | SigV4 |
//! | `aws2-ec2` | [`aws::ec2`] | EC2 (Query API, XML) | SigV4 |
//! | `aws2-msk` | [`aws::msk`] | MSK REST; `AWS_MSK_IAM` SASL for `kafka` | SigV4 |
//! | `aws2-athena` | [`aws::athena`] | Athena (AWS JSON 1.1), polling + pagination | SigV4 |
//! | `aws2-textract` | [`aws::textract`] | Textract (AWS JSON 1.1) | SigV4 |
//! | `aws2-translate` | [`aws::translate`] | Translate (AWS JSON 1.1) | SigV4 |
//! | `aws2-comprehend`, `aws-comprehend` | [`aws::comprehend`] | Comprehend (AWS JSON 1.1) | SigV4 |
//! | `aws2-polly` | [`aws::polly`] | Polly REST | SigV4 |
//! | `aws2-rekognition` | [`aws::rekognition`] | Rekognition (AWS JSON 1.1) | SigV4 |
//! | `aws2-transcribe` | [`aws::transcribe`] | Transcribe (AWS JSON 1.1) | SigV4 |
//! | `aws-bedrock`, `aws2-bedrock`, `aws-bedrock-agent-runtime` | [`aws::bedrock`] | Bedrock Runtime / Agent Runtime REST | SigV4 |
//! | `aws2-mq` | [`aws::mq`] | Amazon MQ REST | SigV4 |
//! | `aws2-ecs`, `aws2-eks` | [`aws::ecs`] | ECS (AWS JSON 1.1), EKS REST | SigV4 |
//! | `aws2-kms` | [`aws::kms`] | KMS (AWS JSON 1.1) | SigV4 |
//! | `aws2-timestream` | [`aws::timestream`] | Timestream Write / Query (AWS JSON 1.0), endpoint discovery | SigV4 |
//! | `aws-cloudtrail` | [`aws::cloudtrail`] | CloudTrail LookupEvents (consumer) | SigV4 |
//! | `aws-config` | [`aws::config`] | AWS Config (AWS JSON 1.1) | SigV4 |
//! | `aws2-redshift-data` | [`aws::redshiftdata`] | Redshift Data API (AWS JSON 1.1) | SigV4 |
//! | `azure-storage-blob` | [`azure::blob`] | Azure Blob Storage REST (block/append/page, leases) | Shared Key / SAS |
//! | `azure-storage-queue` | [`azure::queue`] | Azure Queue Storage REST | Shared Key / SAS |
//! | `azure-storage-datalake` | [`azure::datalake`] | ADLS Gen2 DFS REST | Shared Key / SAS |
//! | `azure-key-vault` | [`azure::keyvault`] | Key Vault secrets REST, `{{azure:...}}` | Azure AD client credentials |
//! | `azure-cosmosdb` | [`azure::cosmosdb`] | Cosmos DB SQL REST + change feed | master key HMAC |
//! | `azure-storage-files` | [`azure::files`] | Azure Files REST | Shared Key / SAS |
//! | `azure-functions` | [`azure::functions`] | HTTP-triggered Functions | function keys / Entra ID |
//! | `azure-eventgrid` | [`azure::eventgrid`] | Event Grid topics (CloudEvents / Event Grid schema) | access key / SAS / Entra ID |
//! | `azure-schema-registry` | [`azure::schemaregistry`] | Event Hubs Schema Registry REST | Entra ID |
//! | `azure-openai` | [`azure::openai`] | Azure OpenAI chat completions / embeddings | API key / Entra ID |
//! | `google-storage` | [`gcp::storage`] | Google Cloud Storage JSON API | service account JWT → OAuth2 |
//! | `google-pubsub` | [`gcp::pubsub`] | Pub/Sub REST; gRPC streaming pull ([`gcp::pubsub_stream`]) | service account JWT → OAuth2 |
//! | `google-bigquery`, `google-bigquery-sql` | [`gcp::bigquery`] | BigQuery v2 REST | service account JWT → OAuth2 |
//! | `google-secret-manager` | [`gcp::secretmanager`] | Secret Manager REST, `{{gcp:...}}` | service account JWT → OAuth2 |
//! | `google-functions` | [`gcp::functions`] | Cloud Functions v1 REST | service account JWT → OAuth2 |
//! | `google-firestore` | [`gcp::firestore`] | Firestore REST | service account JWT → OAuth2 |
//! | `google-sheets` | [`gcp::sheets`] | Sheets v4 REST | service account JWT → OAuth2 |
//! | `google-mail`, `google-calendar`, `google-drive` | [`gcp::workspace`] | Gmail / Calendar / Drive REST | service account + domain-wide delegation |
//! | `google-vertexai`, `google-speech-to-text`, `google-text-to-speech` | [`gcp::ai`] | Vertex AI Gemini, Speech v1, Text-to-Speech v1 REST | service account JWT → OAuth2 |
//!
//! `azure-servicebus` / `azure-eventhubs` (AMQP 1.0) live in `camel-amqp10`. [`register`] also
//! installs the `aws:`, `azure:` and `gcp:` properties functions (secret placeholders) and the
//! `AWS_MSK_IAM` SASL mechanism of the `kafka` component (`camel-kafka-mqtt`).
//!
//! Every component accepts `uriEndpointOverride` + `overrideEndpoint=true` to talk to a
//! local emulator. [`testing`] contains embedded in-memory emulators of all these services
//! (in the spirit of LocalStack / Azurite / the GCP emulators) that verify request signatures
//! and tokens.
//!
//! ```no_run
//! use camel_core::prelude::*;
//!
//! let ctx = CamelContext::new();
//! camel_cloud::register(&ctx);
//! ctx.add_routes(|b| {
//!     b.from("aws2-s3://inbox?region=eu-west-1&accessKey=RAW(AK)&secretKey=RAW(SK)&prefix=orders/")
//!         .to("aws2-sqs://orders?region=eu-west-1&useDefaultCredentialsProvider=true");
//!     b.from("google-pubsub://my-project:orders-sub?serviceAccountKey=file:/etc/sa.json")
//!         .to("azure-storage-blob://account/archive?operation=uploadBlockBlob&accessKey=RAW(KEY)");
//! }).unwrap();
//! ctx.start().unwrap();
//! ```

pub mod aws;
pub mod azure;
pub mod endpoint;
pub mod gcp;
pub mod http;
pub mod testing;
pub mod util;

use std::sync::Arc;

use camel_core::context::CamelContext;
use camel_core::spi::Component;

use endpoint::{CloudComponent, ServiceFactory};

/// All schemes and their factories.
const COMPONENTS: &[(&str, ServiceFactory)] = &[
    ("aws2-s3", aws::s3::create),
    ("aws2-sqs", aws::sqs::create),
    ("aws2-sns", aws::sns::create),
    ("aws2-ddb", aws::ddb::create),
    ("aws2-lambda", aws::lambda::create),
    ("aws2-kinesis", aws::kinesis::create),
    ("aws2-kinesis-firehose", aws::firehose::create),
    ("aws2-eventbridge", aws::eventbridge::create),
    ("aws-secrets-manager", aws::secretsmanager::create),
    ("aws2-ses", aws::ses::create),
    ("aws2-cw", aws::cw::create),
    ("aws2-sts", aws::sts::create),
    ("aws2-step-functions", aws::stepfunctions::create),
    ("aws2-iam", aws::iam::create),
    ("aws2-ec2", aws::ec2::create),
    ("aws2-msk", aws::msk::create),
    ("aws2-athena", aws::athena::create),
    ("aws2-textract", aws::textract::create),
    ("aws2-translate", aws::translate::create),
    ("aws2-comprehend", aws::comprehend::create),
    ("aws-comprehend", aws::comprehend::create),
    ("aws2-polly", aws::polly::create),
    ("aws2-rekognition", aws::rekognition::create),
    ("aws2-transcribe", aws::transcribe::create),
    ("aws-bedrock", aws::bedrock::create),
    ("aws2-bedrock", aws::bedrock::create),
    ("aws-bedrock-agent-runtime", aws::bedrock::create_agent_runtime),
    ("aws2-mq", aws::mq::create),
    ("aws2-ecs", aws::ecs::create_ecs),
    ("aws2-eks", aws::ecs::create_eks),
    ("aws2-kms", aws::kms::create),
    ("aws2-timestream", aws::timestream::create),
    ("aws-cloudtrail", aws::cloudtrail::create),
    ("aws-config", aws::config::create),
    ("aws2-redshift-data", aws::redshiftdata::create),
    ("azure-storage-blob", azure::blob::create),
    ("azure-storage-queue", azure::queue::create),
    ("azure-storage-datalake", azure::datalake::create),
    ("azure-key-vault", azure::keyvault::create),
    ("azure-cosmosdb", azure::cosmosdb::create),
    ("azure-storage-files", azure::files::create),
    ("azure-functions", azure::functions::create),
    ("azure-eventgrid", azure::eventgrid::create),
    ("azure-schema-registry", azure::schemaregistry::create),
    ("azure-openai", azure::openai::create),
    ("google-storage", gcp::storage::create),
    ("google-pubsub", gcp::pubsub::create),
    ("google-bigquery", gcp::bigquery::create),
    ("google-bigquery-sql", gcp::bigquery::create_sql),
    ("google-secret-manager", gcp::secretmanager::create),
    ("google-functions", gcp::functions::create),
    ("google-firestore", gcp::firestore::create),
    ("google-sheets", gcp::sheets::create),
    ("google-mail", gcp::workspace::create_mail),
    ("google-calendar", gcp::workspace::create_calendar),
    ("google-drive", gcp::workspace::create_drive),
    ("google-vertexai", gcp::ai::create_vertexai),
    ("google-speech-to-text", gcp::ai::create_speech_to_text),
    ("google-text-to-speech", gcp::ai::create_text_to_speech),
];

/// Register the cloud components on a context (lazily created, see [`schemes`]) and the
/// `aws:` / `azure:` / `gcp:` secret properties functions. Schemes that already have a
/// component (and functions already registered) are left untouched.
pub fn register(ctx: &CamelContext) {
    util::ensure_kinds();
    aws::msk::register_kafka_iam();
    for (scheme, factory) in COMPONENTS {
        if !ctx.has_component(scheme) {
            let (scheme, factory) = (*scheme, *factory);
            ctx.add_component_factory(scheme, move || Arc::new(CloudComponent::new(scheme, factory)) as Arc<dyn Component>);
        }
    }
    let props = ctx.properties();
    if !props.has_function("aws") {
        props.add_function("aws", aws::secretsmanager::properties_function(ctx));
    }
    if !props.has_function("azure") {
        props.add_function("azure", azure::keyvault::properties_function(ctx));
    }
    if !props.has_function("gcp") {
        props.add_function("gcp", gcp::secretmanager::properties_function(ctx));
    }
}

/// The registered component schemes.
pub fn schemes() -> Vec<&'static str> {
    COMPONENTS.iter().map(|(s, _)| *s).collect()
}
