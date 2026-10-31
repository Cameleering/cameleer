//! Azure Storage components end-to-end against the embedded Shared Key / SAS verifying emulator.

use std::time::{Duration, Instant};

use camel_cloud::testing::azure::ACCOUNT;
use camel_cloud::testing::AzuriteMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn blob_container_upload_get_list_delete() {
    let az = AzuriteMock::start().unwrap();
    let ctx = context();
    let base = format!("azure-storage-blob://{}/docs?{}", ACCOUNT, az.blob_options());
    let t = ctx.create_producer_template();
    t.send_body_and_header(&base, "", "CamelAzureStorageBlobOperation", "createBlobContainer").unwrap();
    assert!(az.container_exists("docs"));
    // upload with blobName option, content type and metadata
    let ex = t
        .request(&format!("{}&operation=uploadBlockBlob&blobName=reports/q1.txt", base), |ex| {
            ex.set_body("quarterly report");
            ex.set_header("CamelAzureStorageBlobContentType", "text/plain");
            ex.set_header("CamelAzureStorageBlobMetadata", Value::object([("author", "camel")]));
        })
        .unwrap();
    assert!(ex.header("CamelAzureStorageBlobETag").is_some());
    assert_eq!(az.blob("docs", "reports/q1.txt").unwrap(), b"quarterly report");
    assert_eq!(az.blob_info("docs", "reports/q1.txt").unwrap(), ("text/plain".to_string(), vec![("author".to_string(), "camel".to_string())]));
    // blob name from header
    t.send_body_and_headers(
        &format!("{}&operation=uploadBlockBlob", base),
        "second blob",
        vec![("CamelAzureStorageBlobBlobName", Value::from("b with space.txt"))],
    )
    .unwrap();
    // getBlob
    let ex = t
        .request(&format!("{}&operation=getBlob", base), |ex| ex.set_header("CamelAzureStorageBlobBlobName", "reports/q1.txt"))
        .unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "quarterly report");
    assert_eq!(ex.header("CamelAzureStorageBlobContentType").unwrap().to_string(), "text/plain");
    assert_eq!(ex.header("CamelAzureStorageBlobBlobSize").unwrap(), &Value::Int(16));
    assert_eq!(ex.header("CamelAzureStorageBlobMetadata").unwrap().get("author").unwrap(), &Value::from("camel"));
    // listBlobs (all, then with prefix)
    let all = t.request_body(&format!("{}&operation=listBlobs", base), "").unwrap();
    let names: Vec<String> = all.as_array().unwrap().iter().map(|b| b.get("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["b with space.txt", "reports/q1.txt"]);
    let some = t.request_body_and_header(&format!("{}&operation=listBlobs", base), "", "CamelAzureStorageBlobPrefix", "reports/").unwrap();
    assert_eq!(some.as_array().unwrap().len(), 1);
    assert_eq!(some.as_array().unwrap()[0].get("size").unwrap(), &Value::Int(16));
    // listBlobContainers is the default operation
    let containers = t.request_body(&format!("azure-storage-blob://{}?{}", ACCOUNT, az.blob_options()), "").unwrap();
    assert_eq!(containers.as_array().unwrap()[0].get("name").unwrap(), &Value::from("docs"));
    // deleteBlob; getBlob of a missing blob -> BlobStorageException
    t.send_body_and_header(&format!("{}&operation=deleteBlob", base), "", "CamelAzureStorageBlobBlobName", "b with space.txt").unwrap();
    assert_eq!(az.blob_names("docs"), vec!["reports/q1.txt"]);
    let e = t.request_body_and_header(&format!("{}&operation=getBlob", base), "", "CamelAzureStorageBlobBlobName", "nope").unwrap_err();
    let e = e.root_cause();
    assert!(e.is_a("BlobStorageException") && e.message.contains("BlobNotFound"), "{}", e);
    assert_eq!(az.auth_failures(), 0);
}

#[test]
fn blob_consumer_with_delete_after_read() {
    let az = AzuriteMock::start().unwrap();
    az.put_blob("inbox", "in/a.csv", b"1,2");
    az.put_blob("inbox", "in/b.csv", b"3,4");
    az.put_blob("inbox", "skip/c.csv", b"x");
    let ctx = context();
    let opts = az.blob_options();
    ctx.add_routes(move |b| {
        b.from(&format!("azure-storage-blob://{}/inbox?prefix=in/&deleteAfterRead=true&delay=20&initialDelay=0&{}", ACCOUNT, opts)).to("mock:blobs");
    })
    .unwrap();
    let blobs = ctx.mock_endpoint("mock:blobs").unwrap();
    blobs.expected_bodies_received_in_any_order(vec!["1,2", "3,4"]);
    blobs.all_messages(|ex| {
        ex.header("CamelAzureStorageBlobBlobName").is_some_and(|n| n.to_string().starts_with("in/"))
            && ex.header("CamelAzureStorageBlobContainerName").map(|c| c.to_string()).as_deref() == Some("inbox")
            && ex.header("CamelAzureStorageBlobETag").is_some()
            && ex.header("CamelAzureStorageBlobBlobSize").and_then(|v| v.as_i64()) == Some(3)
    });
    ctx.start().unwrap();
    blobs.assert_is_satisfied().unwrap();
    wait_until("consumed blobs deleted", || az.blob_names("inbox") == vec!["skip/c.csv"]);
    ctx.stop().unwrap();
    assert_eq!(blobs.received_counter(), 2);
}

#[test]
fn blob_with_sas_token() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("sas");
    let ctx = context();
    let uri = format!(
        "azure-storage-blob://{}/sas?credentialType=AZURE_SAS&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true",
        ACCOUNT,
        az.sas_token(),
        az.blob_url()
    );
    let t = ctx.create_producer_template();
    t.send_body(&format!("{}&operation=uploadBlockBlob&blobName=s.txt", uri), "via sas").unwrap();
    assert_eq!(t.request_body(&format!("{}&operation=getBlob&blobName=s.txt", uri), "").unwrap().to_string(), "via sas");
    assert!(az.sas_requests() >= 2);
    // a read-only SAS cannot write
    let ro = camel_cloud::azure::AccountSas { permissions: "rl".into(), ..camel_cloud::azure::AccountSas::full_access(3600) }
        .token(ACCOUNT, camel_cloud::testing::azure::ACCOUNT_KEY)
        .unwrap();
    let ro_uri = format!("azure-storage-blob://{}/sas?sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, ro, az.blob_url());
    assert_eq!(t.request_body(&format!("{}&operation=getBlob&blobName=s.txt", ro_uri), "").unwrap().to_string(), "via sas");
    let e = t.request_body(&format!("{}&operation=uploadBlockBlob&blobName=w.txt", ro_uri), "x").unwrap_err();
    assert!(e.root_cause().message.contains("AuthorizationPermissionMismatch"), "{}", e);
    // a forged signature is rejected
    let forged = az.sas_token().replace("sp=rwdlacup", "sp=rwdlacupx");
    let bad_uri = format!("azure-storage-blob://{}/sas?sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, forged, az.blob_url());
    let e = t.request_body(&format!("{}&operation=getBlob&blobName=s.txt", bad_uri), "").unwrap_err();
    assert!(e.root_cause().message.contains("AuthenticationFailed"), "{}", e);
}

#[test]
fn wrong_account_key_is_rejected() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("c");
    let ctx = context();
    let wrong = camel_cloud::util::base64_encode(b"definitely-not-the-account-key!!");
    let uri = format!("azure-storage-blob://{}/c?operation=listBlobs&accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, wrong, az.blob_url());
    let e = ctx.create_producer_template().request_body(&uri, "").unwrap_err();
    let e = e.root_cause();
    assert!(e.is_a("BlobStorageException") && e.message.contains("403") && e.message.contains("AuthenticationFailed"), "{}", e);
    assert_eq!(az.auth_failures(), 1);
    // AZURE_IDENTITY is not supported
    let e = ctx.create_producer_template().request_body(&format!("azure-storage-blob://{}/c?credentialType=AZURE_IDENTITY", ACCOUNT), "").unwrap_err();
    assert!(e.root_cause().message.contains("not supported") || e.message.contains("not supported"), "{}", e);
}

#[test]
fn queue_send_receive_peek_delete_update() {
    let az = AzuriteMock::start().unwrap();
    let ctx = context();
    let base = format!("azure-storage-queue://{}/tasks?{}", ACCOUNT, az.queue_options());
    let t = ctx.create_producer_template();
    // sendMessage (default) with createQueue
    let ex = t.request(&format!("{}&createQueue=true", base), |ex| ex.set_body("task <1> & more")).unwrap();
    assert!(az.queue_exists("tasks"));
    assert!(ex.header("CamelAzureStorageQueueMessageId").is_some());
    assert!(ex.header("CamelAzureStorageQueuePopReceipt").is_some());
    assert!(ex.header("CamelAzureStorageQueueInsertionTime").is_some());
    t.send_body(&base, "task 2").unwrap();
    assert_eq!(az.queue_messages("tasks"), vec!["task <1> & more", "task 2"]);
    // peek does not change visibility
    let peeked = t.request_body_and_header(&format!("{}&operation=peekMessages", base), "", "CamelAzureStorageQueueMaxMessages", 5).unwrap();
    assert_eq!(peeked.as_array().unwrap().len(), 2);
    // receiveMessages hides the messages
    let got = t.request_body(&format!("{}&operation=receiveMessages&maxMessages=1&visibilityTimeout=30", base), "").unwrap();
    let m = got.as_array().unwrap()[0].clone();
    assert_eq!(m.get("messageText").unwrap(), &Value::from("task <1> & more"));
    assert_eq!(m.get("dequeueCount").unwrap(), &Value::Int(1));
    // updateMessage then deleteMessage with the new pop receipt
    let ex = t
        .request(&format!("{}&operation=updateMessage", base), |ex| {
            ex.set_body("task 1 updated");
            ex.set_header("CamelAzureStorageQueueMessageId", m.get("messageId").unwrap().clone());
            ex.set_header("CamelAzureStorageQueuePopReceipt", m.get("popReceipt").unwrap().clone());
            ex.set_header("CamelAzureStorageQueueVisibilityTimeout", 30);
        })
        .unwrap();
    assert_eq!(az.queue_messages("tasks"), vec!["task 1 updated", "task 2"]);
    let new_receipt = ex.header("CamelAzureStorageQueuePopReceipt").unwrap().clone();
    // the old pop receipt is no longer valid
    let e = t
        .request(&format!("{}&operation=deleteMessage", base), |ex| {
            ex.set_header("CamelAzureStorageQueueMessageId", m.get("messageId").unwrap().clone());
            ex.set_header("CamelAzureStorageQueuePopReceipt", m.get("popReceipt").unwrap().clone());
        })
        .unwrap_err();
    let root = e.root_cause();
    assert!(root.is_a("QueueStorageException") && root.message.contains("PopReceiptMismatch"), "{}", root);
    t.request(&format!("{}&operation=deleteMessage", base), |ex| {
        ex.set_header("CamelAzureStorageQueueMessageId", m.get("messageId").unwrap().clone());
        ex.set_header("CamelAzureStorageQueuePopReceipt", new_receipt.clone());
    })
    .unwrap();
    assert_eq!(az.queue_messages("tasks"), vec!["task 2"]);
    // listQueues, clearQueue, deleteQueue
    let qs = t.request_body(&format!("azure-storage-queue://{}?operation=listQueues&{}", ACCOUNT, az.queue_options()), "").unwrap();
    assert_eq!(qs.as_array().unwrap()[0].get("name").unwrap(), &Value::from("tasks"));
    t.send_body(&format!("{}&operation=clearQueue", base), "").unwrap();
    assert!(az.queue_messages("tasks").is_empty());
    t.send_body(&format!("{}&operation=deleteQueue", base), "").unwrap();
    assert!(!az.queue_exists("tasks"));
}

#[test]
fn queue_consumer_deletes_processed_messages() {
    let az = AzuriteMock::start().unwrap();
    for i in 0..3 {
        az.put_queue_message("jobs", &format!("job-{}", i));
    }
    let ctx = context();
    let opts = az.queue_options();
    ctx.add_routes(move |b| {
        b.from(&format!("azure-storage-queue://{}/jobs?maxMessages=2&visibilityTimeout=PT30S&delay=10&initialDelay=0&{}", ACCOUNT, opts)).to("mock:jobs");
    })
    .unwrap();
    let jobs = ctx.mock_endpoint("mock:jobs").unwrap();
    jobs.expected_bodies_received(vec!["job-0", "job-1", "job-2"]);
    jobs.all_messages(|ex| {
        ex.header("CamelAzureStorageQueueMessageId").is_some()
            && ex.header("CamelAzureStorageQueuePopReceipt").is_some()
            && ex.header("CamelAzureStorageQueueDequeueCount").and_then(|v| v.as_i64()) == Some(1)
    });
    ctx.start().unwrap();
    jobs.assert_is_satisfied().unwrap();
    wait_until("messages deleted", || az.queue_messages("jobs").is_empty());
    ctx.stop().unwrap();
}

#[test]
fn queue_with_sas_token() {
    let az = AzuriteMock::start().unwrap();
    az.create_queue("q");
    let ctx = context();
    let uri = format!("azure-storage-queue://{}/q?credentialType=AZURE_SAS&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, az.sas_token(), az.queue_url());
    ctx.create_producer_template().send_body(&uri, "sas message").unwrap();
    assert_eq!(az.queue_messages("q"), vec!["sas message"]);
    assert_eq!(az.sas_requests(), 1);
}
