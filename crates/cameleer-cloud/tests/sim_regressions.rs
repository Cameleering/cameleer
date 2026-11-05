//! Hermetic regressions for the bugs found with the third-party simulators
//! (`tests/sim_*.rs` against moto, Azurite, fake-gcs-server and pstest).

use cameleer_cloud::testing::azure::ACCOUNT;
use cameleer_cloud::testing::AzuriteMock;
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

/// Azurite: `listBlobs` with `maxResultsPerPage` returned only the first page; the option is
/// the page size and every page is read (as Camel collects the SDK's PagedIterable).
#[test]
fn list_blobs_reads_every_page() {
    let az = AzuriteMock::start().unwrap();
    for b in ["a", "b", "c"] {
        az.put_blob("paged", b, b"x");
    }
    let ctx = context();
    let all = ctx.create_producer_template().request_body(&format!("azure-storage-blob://{}/paged?operation=listBlobs&maxResultsPerPage=1&{}", ACCOUNT, az.blob_options()), "").unwrap();
    let names: Vec<String> = all.as_array().unwrap().iter().map(|b| b.get("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
}

/// fake-gcs-server: with `authenticate=false` (emulators) there is no service account key, so
/// bucket operations had no project; like the Google client libraries the default project
/// comes from `GOOGLE_CLOUD_PROJECT`.
#[test]
fn storage_bucket_operations_use_the_default_project_without_a_key() {
    let mock = cameleer_cloud::testing::GcpMock::start().unwrap();
    mock.set_require_auth(false);
    std::env::set_var("GOOGLE_CLOUD_PROJECT", "default-project");
    let ctx = context();
    let uri = format!("google-storage://fresh-bucket?authenticate=false&uriEndpointOverride={}&overrideEndpoint=true", mock.url());
    ctx.create_producer_template().send_body_and_header(&uri, "x", "CamelGoogleCloudStorageObjectName", "o.txt").unwrap();
    assert!(mock.bucket_exists("fresh-bucket"));
    assert_eq!(mock.object("fresh-bucket", "o.txt").unwrap(), b"x");
}
