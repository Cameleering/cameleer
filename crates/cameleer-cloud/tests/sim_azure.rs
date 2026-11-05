//! Third-party simulator: Cameleer's Azure Storage components against Microsoft's Azurite
//! (`scripts/simulators.sh`: Blob 20000, Queue 20001, Table 20002 over HTTP; HTTPS on
//! 20010-20012, see `sim_cloud_tls.rs`). Runs only with `CAMEL_SIMULATORS=1`.

use std::time::{Duration, Instant};

use cameleer_cloud::azure::AccountSas;
use cameleer_core::prelude::*;
use cameleer_tls::testing::sim;

/// Azurite's well-known development account.
const ACCOUNT: &str = "devstoreaccount1";
const KEY: &str = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
const BLOB: &str = "http://127.0.0.1:20000";
const QUEUE: &str = "http://127.0.0.1:20001";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

fn blob_opts() -> String {
    format!("accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", KEY, BLOB)
}

fn queue_opts() -> String {
    format!("accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", KEY, QUEUE)
}

fn s(v: &Value) -> String {
    v.to_string()
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let body = body.into();
    t.request(uri, |ex| {
        ex.set_body(body.clone());
        for (k, v) in headers {
            ex.set_header(*k, v.clone());
        }
    })
    .unwrap_or_else(|e| panic!("{}: {}", uri, e))
}

fn req_err(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> CamelError {
    let body = body.into();
    t.request(uri, |ex| {
        ex.set_body(body.clone());
        for (k, v) in headers {
            ex.set_header(*k, v.clone());
        }
    })
    .err()
    .unwrap_or_else(|| panic!("{} should fail", uri))
    .root_cause()
    .clone()
}

fn http_get(url: &str) -> (u16, Vec<u8>) {
    let r = cameleer_cloud::http::HttpClient::new().send(&cameleer_cloud::http::Request::new("GET", cameleer_cloud::http::Url::parse(url).unwrap())).unwrap();
    (r.status, r.body)
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Container names: lower case letters, digits and dashes.
fn name(prefix: &str) -> String {
    sim::unique(prefix).to_ascii_lowercase()
}

fn blob_names(t: &ProducerTemplate, container: &str) -> Vec<String> {
    let all = t.request_body(&format!("azure-storage-blob://{}/{}?operation=listBlobs&{}", ACCOUNT, container, blob_opts()), "").unwrap();
    all.as_array().unwrap().iter().map(|b| s(b.get("name").unwrap())).collect()
}

#[test]
fn sim_azure_blob_containers_blobs_metadata_and_errors() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let c = name("docs");
    let base = format!("azure-storage-blob://{}/{}?{}", ACCOUNT, c, blob_opts());
    req(&t, &base, "", &[("CamelAzureStorageBlobOperation", Value::from("createBlobContainer"))]);
    let ex = req(&t, &format!("{}&operation=uploadBlockBlob&blobName=reports/q1.txt", base), "quarterly report", &[("CamelAzureStorageBlobContentType", Value::from("text/plain")), ("CamelAzureStorageBlobMetadata", Value::object([("Author", "camel")]))]);
    assert!(ex.header("CamelAzureStorageBlobETag").is_some());
    req(&t, &format!("{}&operation=uploadBlockBlob", base), "second blob ü", &[("CamelAzureStorageBlobBlobName", Value::from("b with space+plus.txt"))]);
    let ex = req(&t, &format!("{}&operation=getBlob", base), "", &[("CamelAzureStorageBlobBlobName", Value::from("reports/q1.txt"))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "quarterly report");
    assert_eq!(s(ex.header("CamelAzureStorageBlobContentType").unwrap()), "text/plain");
    assert_eq!(ex.header("CamelAzureStorageBlobBlobSize").unwrap(), &Value::Int(16));
    let meta = ex.header("CamelAzureStorageBlobMetadata").unwrap();
    assert_eq!(meta.get("Author").or_else(|| meta.get("author")).unwrap(), &Value::from("camel"));
    let ex = req(&t, &format!("{}&operation=getBlob", base), "", &[("CamelAzureStorageBlobBlobName", Value::from("b with space+plus.txt"))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "second blob ü");
    assert_eq!(blob_names(&t, &c), vec!["b with space+plus.txt", "reports/q1.txt"]);
    let some = t.request_body_and_header(&format!("{}&operation=listBlobs", base), "", "CamelAzureStorageBlobPrefix", "reports/").unwrap();
    assert_eq!(some.as_array().unwrap().len(), 1);
    assert_eq!(some.as_array().unwrap()[0].get("size").unwrap(), &Value::Int(16));
    // paging through listings
    let paged = t.request_body(&format!("{}&operation=listBlobs&maxResultsPerPage=1", base), "").unwrap();
    assert_eq!(paged.as_array().unwrap().len(), 2, "all pages are read");
    let containers = t.request_body(&format!("azure-storage-blob://{}?{}", ACCOUNT, blob_opts()), "").unwrap();
    assert!(containers.as_array().unwrap().iter().any(|x| s(x.get("name").unwrap()) == c));
    // errors as Azurite reports them
    let e = req_err(&t, &format!("{}&operation=getBlob", base), "", &[("CamelAzureStorageBlobBlobName", Value::from("nope"))]);
    assert!(e.is_a("BlobStorageException") && e.message.contains("BlobNotFound"), "{}", e);
    let e = req_err(&t, &format!("{}&operation=createBlobContainer", base), "", &[]);
    assert!(e.message.contains("ContainerAlreadyExists"), "{}", e);
    let wrong = cameleer_cloud::util::base64_encode(b"definitely-not-the-account-key!!");
    let e = req_err(&t, &format!("azure-storage-blob://{}/{}?operation=listBlobs&accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, c, wrong, BLOB), "", &[]);
    assert!(e.is_a("BlobStorageException") && e.message.contains("403") && e.message.contains("AuthorizationFailure") || e.message.contains("AuthenticationFailed"), "{}", e);
    // deletes
    req(&t, &format!("{}&operation=deleteBlob", base), "", &[("CamelAzureStorageBlobBlobName", Value::from("b with space+plus.txt"))]);
    assert_eq!(blob_names(&t, &c), vec!["reports/q1.txt"]);
    req(&t, &format!("{}&operation=deleteBlobContainer", base), "", &[]);
    let e = req_err(&t, &format!("{}&operation=listBlobs", base), "", &[]);
    assert!(e.message.contains("ContainerNotFound") || e.message.contains("ContainerBeingDeleted"), "{}", e);
}

#[test]
fn sim_azure_blob_block_lists_append_page_blobs_leases_copies_and_links() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let c = name("blocks");
    let o = blob_opts();
    req(&t, &format!("azure-storage-blob://{}/{}?operation=createBlobContainer&{}", ACCOUNT, c, o), "", &[]);
    let uri = |op: &str, blob: &str, extra: &str| format!("azure-storage-blob://{}/{}?operation={}&blobName={}{}&{}", ACCOUNT, c, op, blob, extra, o);
    let get = |blob: &str| req(&t, &uri("getBlob", blob, ""), "", &[]).body_as::<Vec<u8>>().unwrap();
    // block lists
    let ids = req(&t, &uri("stageBlockBlobList", "big.txt", ""), Value::Array(vec![Value::from("hello "), Value::from("world")]), &[]).body().clone();
    let ids: Vec<String> = ids.as_array().unwrap().iter().map(s).collect();
    assert_eq!(ids.len(), 2);
    let un = req(&t, &uri("getBlobBlockList", "big.txt", "&blockListType=UNCOMMITTED"), "", &[]).body().clone();
    assert_eq!(un.as_array().unwrap().len(), 2);
    assert_eq!(un.as_array().unwrap()[1].get("size").and_then(|v| v.as_i64()), Some(5));
    req(&t, &uri("commitBlobBlockList", "big.txt", ""), Value::from(ids.clone()), &[("CamelAzureStorageBlobContentType", Value::from("text/plain"))]);
    assert_eq!(get("big.txt"), b"hello world");
    let committed = req(&t, &uri("getBlobBlockList", "big.txt", ""), "", &[]).body().clone();
    assert_eq!(committed.as_array().unwrap().iter().map(|b| s(b.get("name").unwrap())).collect::<Vec<_>>(), ids);
    let new_ids = req(&t, &uri("stageBlockBlobList", "big.txt", ""), "!", &[]).body().clone();
    req(&t, &uri("commitBlobBlockList", "big.txt", ""), Value::from(vec![ids[0].clone(), s(&new_ids.as_array().unwrap()[0])]), &[]);
    assert_eq!(get("big.txt"), b"hello !");
    let e = req_err(&t, &uri("commitBlobBlockList", "big.txt", ""), Value::from(vec!["bm9wZQ=="]), &[]);
    assert!(e.message.contains("InvalidBlockList"), "{}", e);
    // append blobs
    for line in ["line 1\n", "line 2\n", "line 3\n"] {
        req(&t, &uri("commitAppendBlob", "log.txt", ""), line, &[]);
    }
    assert_eq!(get("log.txt"), b"line 1\nline 2\nline 3\n");
    let e = req_err(&t, &uri("commitAppendBlob", "missing.txt", "&createAppendBlob=false"), "x", &[]);
    assert!(e.message.contains("BlobNotFound"), "{}", e);
    // page blobs
    req(&t, &uri("createPageBlob", "disk.vhd", ""), "", &[("CamelAzureStorageBlobPageBlobSize", Value::Int(1024))]);
    req(&t, &uri("uploadPageBlob", "disk.vhd", ""), Value::Bytes(vec![7u8; 512]), &[("CamelAzureStorageBlobPageBlobRange", Value::from("512-1023"))]);
    let ranges = req(&t, &uri("getPageBlobRanges", "disk.vhd", ""), "", &[]).body().clone();
    assert_eq!(ranges, Value::Array(vec![Value::object([("start", 512), ("end", 1023)])]));
    let data = get("disk.vhd");
    assert_eq!((data.len(), data[0], data[600]), (1024, 0, 7));
    req(&t, &uri("resizePageBlob", "disk.vhd", ""), "", &[("CamelAzureStorageBlobPageBlobSize", Value::Int(2048))]);
    assert_eq!(get("disk.vhd").len(), 2048);
    req(&t, &uri("clearPageBlob", "disk.vhd", ""), "", &[("CamelAzureStorageBlobPageBlobRange", Value::object([("start", 512), ("end", 1023)]))]);
    assert_eq!(req(&t, &uri("getPageBlobRanges", "disk.vhd", ""), "", &[]).body().clone(), Value::Array(vec![]));
    // leases
    req(&t, &uri("uploadBlockBlob", "locked.txt", ""), "v1", &[]);
    let lease = s(req(&t, &uri("acquireBlobLease", "locked.txt", ""), "", &[("CamelAzureStorageBlobLeaseDuration", Value::Int(-1))]).header("CamelAzureStorageBlobLeaseId").unwrap());
    let e = req_err(&t, &uri("uploadBlockBlob", "locked.txt", ""), "v2", &[]);
    assert!(e.message.contains("LeaseIdMissing") && e.message.contains("412"), "{}", e);
    req(&t, &uri("uploadBlockBlob", "locked.txt", ""), "v2", &[("CamelAzureStorageBlobLeaseId", Value::from(lease.clone()))]);
    assert_eq!(get("locked.txt"), b"v2");
    req(&t, &uri("renewBlobLease", "locked.txt", ""), "", &[("CamelAzureStorageBlobLeaseId", Value::from(lease.clone()))]);
    req(&t, &uri("releaseBlobLease", "locked.txt", ""), "", &[("CamelAzureStorageBlobLeaseId", Value::from(lease.clone()))]);
    req(&t, &uri("uploadBlockBlob", "locked.txt", "&leaseBlob=true&leaseDurationInSeconds=15"), "v3", &[]);
    assert_eq!(get("locked.txt"), b"v3");
    req(&t, &uri("acquireBlobLease", "locked.txt", ""), "", &[]);
    req(&t, &uri("breakBlobLease", "locked.txt", ""), "", &[]);
    req(&t, &uri("deleteBlob", "locked.txt", "&leaseBlob=true"), "", &[]);
    assert!(!blob_names(&t, &c).contains(&"locked.txt".to_string()));
    // download links (service SAS) checked by Azurite
    let link = s(req(&t, &uri("downloadLink", "big.txt", "&downloadLinkExpiration=60000"), "", &[]).body());
    assert!(link.contains("sr=b") && link.contains("sp=r"), "{}", link);
    assert_eq!(http_get(&link), (200, b"hello !".to_vec()));
    assert_eq!(http_get(&link.replace("sp=r", "sp=rw")).0, 403, "tampered SAS");
    // server-side copy
    let backup = name("backup");
    req(&t, &format!("azure-storage-blob://{}/{}?operation=createBlobContainer&{}", ACCOUNT, backup, o), "", &[]);
    let ex = req(&t, &format!("azure-storage-blob://{}/{}?operation=copyBlob&blobName=copy.txt&{}", ACCOUNT, backup, o), "", &[("CamelAzureStorageBlobSourceBlobContainerName", Value::from(c.clone())), ("CamelAzureStorageBlobSourceBlobName", Value::from("big.txt"))]);
    assert_eq!(s(ex.header("CamelAzureStorageBlobCopyStatus").unwrap()), "success");
    let copied = req(&t, &format!("azure-storage-blob://{}/{}?operation=getBlob&blobName=copy.txt&{}", ACCOUNT, backup, o), "", &[]);
    assert_eq!(copied.body_as::<String>().unwrap(), "hello !");
}

#[test]
fn sim_azure_blob_consumer_and_sas_tokens() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let c = name("inbox");
    let o = blob_opts();
    req(&t, &format!("azure-storage-blob://{}/{}?operation=createBlobContainer&{}", ACCOUNT, c, o), "", &[]);
    for (b, v) in [("in/a.csv", "1,2"), ("in/b.csv", "3,4"), ("skip/c.csv", "x")] {
        req(&t, &format!("azure-storage-blob://{}/{}?operation=uploadBlockBlob&blobName={}&{}", ACCOUNT, c, b, o), v, &[]);
    }
    let (c2, o2) = (c.clone(), o.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("azure-storage-blob://{}/{}?prefix=in/&deleteAfterRead=true&delay=50&initialDelay=0&{}", ACCOUNT, c2, o2)).to("mock:blobs");
    })
    .unwrap();
    let blobs = ctx.mock_endpoint("mock:blobs").unwrap();
    blobs.expected_bodies_received_in_any_order(vec!["1,2", "3,4"]);
    blobs.all_messages(|ex| ex.header("CamelAzureStorageBlobBlobName").is_some_and(|n| n.to_string().starts_with("in/")) && ex.header("CamelAzureStorageBlobETag").is_some() && ex.header("CamelAzureStorageBlobBlobSize").and_then(|v| v.as_i64()) == Some(3));
    ctx.start().unwrap();
    blobs.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    wait_until("consumed blobs deleted", || blob_names(&t, &c) == vec!["skip/c.csv"]);
    ctx.stop().unwrap();

    // account SAS made by Cameleer, verified by Azurite
    let sas = AccountSas::full_access(3600).token(ACCOUNT, KEY).unwrap();
    let sas_uri = format!("azure-storage-blob://{}/{}?credentialType=AZURE_SAS&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, c, sas, BLOB);
    t.send_body(&format!("{}&operation=uploadBlockBlob&blobName=s.txt", sas_uri), "via sas").unwrap();
    assert_eq!(s(&t.request_body(&format!("{}&operation=getBlob&blobName=s.txt", sas_uri), "").unwrap()), "via sas");
    // a read-only SAS cannot write; a forged one is refused
    let ro = AccountSas { permissions: "rl".into(), ..AccountSas::full_access(3600) }.token(ACCOUNT, KEY).unwrap();
    let ro_uri = format!("azure-storage-blob://{}/{}?sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, c, ro, BLOB);
    assert_eq!(s(&t.request_body(&format!("{}&operation=getBlob&blobName=s.txt", ro_uri), "").unwrap()), "via sas");
    let e = req_err(&t, &format!("{}&operation=uploadBlockBlob&blobName=w.txt", ro_uri), "x", &[]);
    assert!(e.message.contains("403") && e.message.contains("AuthorizationPermissionMismatch"), "{}", e);
    let forged = sas.replacen("sp=", "sp=x", 1);
    let bad = format!("azure-storage-blob://{}/{}?sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, c, forged, BLOB);
    let e = req_err(&t, &format!("{}&operation=getBlob&blobName=s.txt", bad), "", &[]);
    assert!(e.message.contains("403"), "{}", e);
    // an expired SAS
    let expired = AccountSas { expiry: "2020-01-01T00:00:00Z".into(), ..AccountSas::full_access(3600) }.token(ACCOUNT, KEY).unwrap();
    let e = req_err(&t, &format!("azure-storage-blob://{}/{}?operation=getBlob&blobName=s.txt&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, c, expired, BLOB), "", &[]);
    assert!(e.message.contains("403"), "{}", e);
}

#[test]
fn sim_azure_queue_messages_visibility_consumer_and_sas() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let q = name("tasks");
    let base = format!("azure-storage-queue://{}/{}?{}", ACCOUNT, q, queue_opts());
    let ex = req(&t, &format!("{}&createQueue=true", base), "task <1> & more ü", &[]);
    assert!(ex.header("CamelAzureStorageQueueMessageId").is_some());
    assert!(ex.header("CamelAzureStorageQueuePopReceipt").is_some());
    assert!(ex.header("CamelAzureStorageQueueInsertionTime").is_some());
    t.send_body(&base, "task 2").unwrap();
    let peeked = t.request_body_and_header(&format!("{}&operation=peekMessages", base), "", "CamelAzureStorageQueueMaxMessages", 5).unwrap();
    let texts: Vec<String> = peeked.as_array().unwrap().iter().map(|m| s(m.get("messageText").unwrap())).collect();
    assert_eq!(texts, vec!["task <1> & more ü", "task 2"]);
    let got = t.request_body(&format!("{}&operation=receiveMessages&maxMessages=1&visibilityTimeout=30", base), "").unwrap();
    let m = got.as_array().unwrap()[0].clone();
    assert_eq!(m.get("messageText").unwrap(), &Value::from("task <1> & more ü"));
    assert_eq!(m.get("dequeueCount").unwrap(), &Value::Int(1));
    // hidden now: peek sees only the second one
    let peeked = t.request_body_and_header(&format!("{}&operation=peekMessages", base), "", "CamelAzureStorageQueueMaxMessages", 5).unwrap();
    assert_eq!(peeked.as_array().unwrap().len(), 1);
    let ex = req(
        &t,
        &format!("{}&operation=updateMessage", base),
        "task 1 updated",
        &[("CamelAzureStorageQueueMessageId", m.get("messageId").unwrap().clone()), ("CamelAzureStorageQueuePopReceipt", m.get("popReceipt").unwrap().clone()), ("CamelAzureStorageQueueVisibilityTimeout", Value::Int(0))],
    );
    let new_receipt = ex.header("CamelAzureStorageQueuePopReceipt").unwrap().clone();
    let e = req_err(&t, &format!("{}&operation=deleteMessage", base), "", &[("CamelAzureStorageQueueMessageId", m.get("messageId").unwrap().clone()), ("CamelAzureStorageQueuePopReceipt", m.get("popReceipt").unwrap().clone())]);
    assert!(e.is_a("QueueStorageException") && e.message.contains("PopReceiptMismatch"), "{}", e);
    req(&t, &format!("{}&operation=deleteMessage", base), "", &[("CamelAzureStorageQueueMessageId", m.get("messageId").unwrap().clone()), ("CamelAzureStorageQueuePopReceipt", new_receipt)]);
    let peeked = t.request_body_and_header(&format!("{}&operation=peekMessages", base), "", "CamelAzureStorageQueueMaxMessages", 5).unwrap();
    assert_eq!(peeked.as_array().unwrap().iter().map(|m| s(m.get("messageText").unwrap())).collect::<Vec<_>>(), vec!["task 2"]);
    let qs = t.request_body_and_header(&format!("azure-storage-queue://{}?operation=listQueues&{}", ACCOUNT, queue_opts()), "", "CamelAzureStorageQueuePrefix", q.as_str());
    if let Ok(qs) = qs {
        assert!(qs.to_json().contains(&q), "{}", qs.to_json());
    }
    t.send_body(&format!("{}&operation=clearQueue", base), "").unwrap();
    assert!(t.request_body_and_header(&format!("{}&operation=peekMessages", base), "", "CamelAzureStorageQueueMaxMessages", 5).unwrap().as_array().unwrap().is_empty());

    // consumer: processed messages are deleted, failed ones come back with a higher dequeue count
    for i in 0..3 {
        t.send_body(&base, format!("job-{}", i)).unwrap();
    }
    t.send_body(&base, "poison").unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a2 = attempts.clone();
    let b2 = base.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("{}&maxMessages=5&visibilityTimeout=PT1S&delay=50&initialDelay=0", b2.replacen('?', "?", 1)))
            .process(move |ex: &mut Exchange| {
                if ex.body_as::<String>()? == "poison" {
                    a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(CamelError::runtime("poison"));
                }
                Ok(())
            })
            .to("mock:jobs");
    })
    .unwrap();
    let jobs = ctx.mock_endpoint("mock:jobs").unwrap();
    jobs.expected_bodies_received_in_any_order(vec!["job-0", "job-1", "job-2"]);
    ctx.start().unwrap();
    jobs.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    wait_until("poison redelivered", || attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    ctx.stop().unwrap();
    let peek = || t.request_body_and_header(&format!("{}&operation=peekMessages", base), "", "CamelAzureStorageQueueMaxMessages", 5).unwrap();
    wait_until("poison visible again", || peek().as_array().unwrap().len() == 1);
    let left = peek();
    let left = left.as_array().unwrap();
    assert_eq!(left.len(), 1);
    assert!(left[0].get("dequeueCount").and_then(|v| v.as_i64()).unwrap_or(0) >= 2);

    // SAS
    let sas = AccountSas::full_access(3600).token(ACCOUNT, KEY).unwrap();
    t.send_body(&format!("azure-storage-queue://{}/{}?credentialType=AZURE_SAS&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, q, sas, QUEUE), "sas message").unwrap();
    t.send_body(&format!("{}&operation=deleteQueue", base), "").unwrap();
    let e = req_err(&t, &base, "x", &[]);
    assert!(e.message.contains("QueueNotFound"), "{}", e);
}
