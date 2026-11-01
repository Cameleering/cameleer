//! Hermetic regressions for the bugs found with the third-party simulators
//! (`tests/sim_*.rs` against moto, Azurite, fake-gcs-server and pstest).

use camel_cloud::testing::azure::ACCOUNT;
use camel_cloud::testing::AzuriteMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
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
