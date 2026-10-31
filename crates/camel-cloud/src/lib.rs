//! # rust-camel cloud components
//!
//! Camel's most used cloud components, implemented over the services' REST APIs with a
//! small std-only HTTPS client ([`http`], TLS from `camel-tls`):
//!
//! | scheme | module | service / protocol | auth |
//! |---|---|---|---|
//! | `aws2-s3` | [`aws::s3`] | Amazon S3 REST (XML) | SigV4 |
//! | `aws2-sqs` | [`aws::sqs`] | Amazon SQS (AWS JSON 1.0) | SigV4 |
//! | `aws2-sns` | [`aws::sns`] | Amazon SNS (Query API) | SigV4 |
//! | `aws2-ddb` | [`aws::ddb`] | Amazon DynamoDB (AWS JSON 1.0) | SigV4 |
//! | `aws2-lambda` | [`aws::lambda`] | AWS Lambda REST | SigV4 |
//! | `azure-storage-blob` | [`azure::blob`] | Azure Blob Storage REST | Shared Key / SAS |
//! | `azure-storage-queue` | [`azure::queue`] | Azure Queue Storage REST | Shared Key / SAS |
//! | `google-storage` | [`gcp::storage`] | Google Cloud Storage JSON API | service account JWT → OAuth2 |
//! | `google-pubsub` | [`gcp::pubsub`] | Google Pub/Sub REST | service account JWT → OAuth2 |
//!
//! `azure-servicebus` is not provided: it requires AMQP 1.0.
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
    ("azure-storage-blob", azure::blob::create),
    ("azure-storage-queue", azure::queue::create),
    ("google-storage", gcp::storage::create),
    ("google-pubsub", gcp::pubsub::create),
];

/// Register the cloud components on a context (lazily created): `aws2-s3`, `aws2-sqs`,
/// `aws2-sns`, `aws2-ddb`, `aws2-lambda`, `azure-storage-blob`, `azure-storage-queue`,
/// `google-storage` and `google-pubsub`. Schemes that already have a component are left untouched.
pub fn register(ctx: &CamelContext) {
    util::ensure_kinds();
    for (scheme, factory) in COMPONENTS {
        if !ctx.has_component(scheme) {
            let (scheme, factory) = (*scheme, *factory);
            ctx.add_component_factory(scheme, move || Arc::new(CloudComponent::new(scheme, factory)) as Arc<dyn Component>);
        }
    }
}
