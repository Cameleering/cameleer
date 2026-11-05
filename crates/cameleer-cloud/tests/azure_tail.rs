//! Remaining Azure features against the verifying emulators: the blob change feed (Avro chunk
//! files in `$blobchangefeed`, `getChangeFeed` and the change feed consumer), the Azure Blob
//! checkpoint store of `azure-eventhubs` (cameleer-amqp10's broker + Azurite emulator), and Key
//! Vault keys (create / sign / verify) and certificates (create / import / get / delete).

use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_amqp10::eventhubs::CheckpointStore;
use cameleer_amqp10::testing::Amqp10Broker;
use cameleer_cloud::azure::blob_checkpoint::BlobCheckpointStore;
use cameleer_cloud::testing::azure::{ACCOUNT, ACCOUNT_KEY};
use cameleer_cloud::testing::{AzureMock, AzuriteMock};
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    cameleer_amqp10::register(&ctx);
    ctx
}

fn put(t: &ProducerTemplate, az: &AzuriteMock, container: &str, blob: &str, body: &str) {
    let uri = format!("azure-storage-blob://{}/{}?operation=uploadBlockBlob&blobName={}&{}", ACCOUNT, container, blob, az.blob_options());
    let ex = t.request(&uri, |e| e.set_body(body.to_string())).unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
}

#[test]
fn blob_change_feed_operation_and_consumer() {
    let az = AzuriteMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let feed = format!("azure-storage-blob://{}?operation=getChangeFeed&{}", ACCOUNT, az.blob_options());
    // not enabled
    let e = t.request(&feed, |_| {}).unwrap_err();
    assert!(e.message.contains("change feed is not enabled"), "{}", e.message);
    az.enable_change_feed();
    az.create_container("docs");
    let before = cameleer_cloud::util::now_millis();
    put(&t, &az, "docs", "a.txt", "one");
    put(&t, &az, "docs", "dir/b.txt", "two");
    let del = format!("azure-storage-blob://{}/docs?operation=deleteBlob&blobName=a.txt&{}", ACCOUNT, az.blob_options());
    assert!(t.request(&del, |_| {}).unwrap().exception.is_none());
    assert_eq!(az.change_feed_events(), 3);
    let ex = t.request(&feed, |_| {}).unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    let events = ex.body().as_array().unwrap().clone();
    let types: Vec<String> = events.iter().map(|e| e.get("eventType").unwrap().to_string()).collect();
    assert_eq!(types, vec!["BlobCreated", "BlobCreated", "BlobDeleted"]);
    assert_eq!(events[1].get("subject").unwrap().to_string(), "/blobServices/default/containers/docs/blobs/dir/b.txt");
    assert_eq!(events[0].get("data").unwrap().get("contentLength").unwrap(), &Value::Int(3));
    assert_eq!(events[0].get("data").unwrap().get("api").unwrap().to_string(), "PutBlob");
    // time window (end before everything)
    let ex = t.request(&feed, |e| e.set_header("CamelAzureStorageBlobChangeFeedEndTime", Value::Int(before - 10))).unwrap();
    assert!(ex.body().as_array().unwrap().is_empty());
    // consumer from now: only later changes, each once
    ctx.add_routes({
        let opts = az.blob_options();
        move |b| {
            b.from(&format!("azure-storage-blob://{}?changeFeed=true&delay=20&initialDelay=0&{}", ACCOUNT, opts)).to("mock:feed");
        }
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:feed").unwrap();
    ctx.start().unwrap();
    std::thread::sleep(Duration::from_millis(80));
    put(&t, &az, "docs", "c.txt", "three");
    put(&t, &az, "docs", "c.txt", "three-v2");
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.received_exchanges().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(100));
    let rx = m.received_exchanges();
    assert_eq!(rx.len(), 2, "no duplicates and no events from before the start");
    assert_eq!(rx[0].header("CamelAzureStorageBlobBlobName").unwrap().to_string(), "c.txt");
    assert_eq!(rx[0].header("CamelAzureStorageBlobContainerName").unwrap().to_string(), "docs");
    assert_eq!(rx[1].header("CamelAzureStorageBlobChangeFeedEventType").unwrap().to_string(), "BlobCreated");
    ctx.stop().unwrap();
    // shared key verified
    let bad = format!("azure-storage-blob://{}?operation=getChangeFeed&accessKey=RAW(d3Jvbmc=)&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, az.blob_url());
    assert!(t.request(&bad, |_| {}).unwrap_err().message.contains("AuthenticationFailed"));
}

const KEY_NAME: &str = "RootManageSharedAccessKey";
const KEY: &str = "c2VjcmV0K2tleS8xMjM=";

#[test]
fn eventhubs_blob_checkpoint_store() {
    let az = AzuriteMock::start().unwrap();
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).event_hub("telemetry", 2).start().unwrap();
    let eh = |opts: &str| format!("azure-eventhubs:?connectionString=RAW({};EntityPath=telemetry){}", b.connection_string(KEY_NAME, KEY), opts);
    let blob_opts = format!("&blobAccountName={}&blobContainerName=checkpoints&blobAccessKey=RAW({})&blobEndpoint={}", ACCOUNT, ACCOUNT_KEY, az.blob_url());
    let ctx = context();
    let from = eh(&format!("&partitionId=0&initialPosition=earliest&checkpointBatchSize=1&consumerGroupName=cg1{}", blob_opts));
    ctx.add_routes(move |r| {
        r.from(&from).route_id("ckpt").convert_body_to("String").to("mock:ckpt");
    })
    .unwrap();
    let t = ctx.create_producer_template();
    ctx.start().unwrap();
    let mock = ctx.mock_endpoint("mock:ckpt").unwrap();
    mock.expected_bodies_received(vec!["a", "b"]);
    for x in ["a", "b"] {
        t.send_body(&eh("&partitionId=0"), x).unwrap();
    }
    mock.assert_is_satisfied().unwrap();
    let last_offset: i64 = mock.exchange(1).unwrap().header("CamelAzureEventHubsOffset").unwrap().to_string().parse().unwrap();
    ctx.stop_route("ckpt").unwrap();
    // the checkpoint is an Azurite blob with offset / sequencenumber metadata
    let (_, meta) = az.blob_info("checkpoints", "127.0.0.1/telemetry/cg1/checkpoint/0").expect("checkpoint blob");
    let get = |k: &str| meta.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(get("offset"), Some(last_offset.to_string()));
    assert!(get("sequencenumber").is_some());
    // resume after the checkpoint
    t.send_body(&eh("&partitionId=0"), "c").unwrap();
    mock.reset();
    mock.expected_bodies_received(vec!["c"]);
    ctx.start_route("ckpt").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // the store used directly
    let store = BlobCheckpointStore::new(&cameleer_amqp10::eventhubs::BlobCheckpointOptions {
        account_name: ACCOUNT.into(),
        container_name: "checkpoints".into(),
        access_key: Some(ACCOUNT_KEY.into()),
        endpoint: Some(az.blob_url()),
    })
    .unwrap();
    let store: Arc<dyn CheckpointStore> = Arc::new(store);
    assert_eq!(store.get("127.0.0.1", "telemetry", "cg1", "0").unwrap().offset, last_offset.max(store.get("127.0.0.1", "telemetry", "cg1", "0").unwrap().offset));
    assert!(store.get("ns", "hub", "cg", "9").is_none());
    // incomplete blob options are rejected, wrong key fails on checkpoint
    let c2 = context();
    assert!(c2.get_endpoint(&eh("&blobAccountName=x")).is_err());
    let wrong = BlobCheckpointStore::new(&cameleer_amqp10::eventhubs::BlobCheckpointOptions { account_name: ACCOUNT.into(), container_name: "c".into(), access_key: Some("d3Jvbmc=".into()), endpoint: Some(az.blob_url()) }).unwrap();
    assert!(wrong.update("n", "h", "g", "0", cameleer_amqp10::eventhubs::Checkpoint { offset: 1, sequence_number: 1 }).is_err());
}

#[test]
fn key_vault_keys_and_certificates() {
    let az = AzureMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = |op: &str| format!("azure-key-vault://myvault?operation={}&{}", op, az.key_vault_options());
    let call = |op: &str, body: Value, headers: Vec<(&str, Value)>| -> Result<Exchange> {
        let hs: Vec<(String, Value)> = headers.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        t.request(&uri(op), move |e| {
            e.set_body(body.clone());
            for (k, v) in &hs {
                e.set_header(k.clone(), v.clone());
            }
        })
    };
    let ex = call("createKey", Value::Null, vec![("CamelAzureKeyVaultKeyName", Value::from("signer")), ("CamelAzureKeyVaultKeyCurve", Value::from("P-256"))]).unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    assert_eq!(ex.body().get("key").unwrap().get("crv").unwrap().to_string(), "P-256");
    let v1 = ex.header("CamelAzureKeyVaultKeyVersion").unwrap().to_string();
    assert_eq!(az.key_names(), vec!["signer".to_string()]);
    let sig = call("signData", Value::from("important document"), vec![("CamelAzureKeyVaultKeyName", Value::from("signer"))]).unwrap();
    assert!(sig.exception.is_none(), "{:?}", sig.exception);
    let sig_bytes = sig.body().to_bytes();
    assert_eq!(sig_bytes.len(), 64);
    let ok = call("verifyData", Value::from("important document"), vec![("CamelAzureKeyVaultKeyName", Value::from("signer")), ("CamelAzureKeyVaultSignature", Value::Bytes(sig_bytes.clone()))]).unwrap();
    assert_eq!(ok.body(), &Value::Bool(true));
    let bad = call("verifyData", Value::from("tampered document"), vec![("CamelAzureKeyVaultKeyName", Value::from("signer")), ("CamelAzureKeyVaultKeyVersion", Value::from(v1.clone())), ("CamelAzureKeyVaultSignature", Value::Bytes(sig_bytes))]).unwrap();
    assert_eq!(bad.body(), &Value::Bool(false));
    let wrong_alg = call("signData", Value::from("x"), vec![("CamelAzureKeyVaultKeyName", Value::from("signer")), ("CamelAzureKeyVaultSignatureAlgorithm", Value::from("ES384"))]);
    assert!(wrong_alg.unwrap_err().message.contains("BadParameter"));
    assert_eq!(call("getKey", Value::Null, vec![("CamelAzureKeyVaultKeyName", Value::from("signer")), ("CamelAzureKeyVaultKeyVersion", Value::from(v1))]).unwrap().body().get("key").unwrap().get("kty").unwrap().to_string(), "EC");
    assert_eq!(call("listKeys", Value::Null, vec![]).unwrap().body(), &Value::from(vec!["signer"]));
    assert!(call("deleteKey", Value::Null, vec![("CamelAzureKeyVaultKeyName", Value::from("signer"))]).unwrap().body().to_string().contains("deletedkeys/signer"));
    assert!(call("getKey", Value::Null, vec![("CamelAzureKeyVaultKeyName", Value::from("signer"))]).unwrap_err().message.contains("KeyNotFound"));

    // certificates
    let cert = call("createCertificate", Value::Null, vec![("CamelAzureKeyVaultCertificateName", Value::from("web")), ("CamelAzureKeyVaultCertificateSubject", Value::from("CN=web.example.com"))]).unwrap();
    assert!(cert.exception.is_none(), "{:?}", cert.exception);
    let der = cameleer_cloud::util::base64_decode(&cert.body().get("cer").unwrap().to_string()).unwrap();
    assert_eq!(Some(der.clone()), az.certificate_der("web"));
    let parsed = cameleer_tls::x509::Certificate::from_der(&der).unwrap();
    assert_eq!(parsed.subject.common_name(), Some("web.example.com"));
    let ver = cert.header("CamelAzureKeyVaultCertificateVersion").unwrap().to_string();
    assert_eq!(call("getCertificate", Value::Null, vec![("CamelAzureKeyVaultCertificateName", Value::from("web")), ("CamelAzureKeyVaultCertificateVersion", Value::from(ver))]).unwrap().body().get("cer"), cert.body().get("cer"));
    let pem = cameleer_tls::self_signed("imported.example").cert_pem;
    let imp = call("importCertificate", Value::from(pem), vec![("CamelAzureKeyVaultCertificateName", Value::from("imported"))]).unwrap();
    assert!(imp.exception.is_none(), "{:?}", imp.exception);
    let mut names: Vec<String> = call("listCertificates", Value::Null, vec![]).unwrap().body().as_array().unwrap().iter().map(|v| v.to_string()).collect();
    names.sort();
    assert_eq!(names, vec!["imported", "web"]);
    call("deleteCertificate", Value::Null, vec![("CamelAzureKeyVaultCertificateName", Value::from("web"))]).unwrap();
    assert!(call("getCertificate", Value::Null, vec![("CamelAzureKeyVaultCertificateName", Value::from("web"))]).unwrap_err().message.contains("CertificateNotFound"));
    // bearer token required
    let bad = format!("azure-key-vault://myvault?operation=listKeys&tenantId={}&clientId={}&clientSecret=RAW(wrong)&authorityHost={}&uriEndpointOverride={}&overrideEndpoint=true", cameleer_cloud::testing::azure_services::TENANT_ID, cameleer_cloud::testing::azure_services::CLIENT_ID, az.url(), az.url());
    assert!(t.request(&bad, |_| {}).is_err());
}

#[allow(dead_code)]
fn wait(cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cond() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn eventhubs_blob_checkpoint_store_ownership_and_load_balancing() {
    use cameleer_amqp10::eventhubs::PartitionOwnership;
    let az = AzuriteMock::start().unwrap();
    let opts = cameleer_amqp10::eventhubs::BlobCheckpointOptions { account_name: ACCOUNT.into(), container_name: "owners".into(), access_key: Some(ACCOUNT_KEY.into()), endpoint: Some(az.blob_url()) };
    let store = BlobCheckpointStore::new(&opts).unwrap();
    assert!(store.list_ownership("ns", "hub", "cg").unwrap().is_empty(), "missing container = no ownership");
    let claim = |p: &str, owner: &str, etag: Option<String>| PartitionOwnership { namespace: "NS".into(), hub: "Hub".into(), consumer_group: "cg".into(), partition_id: p.into(), owner_id: owner.into(), last_modified: 0, etag };
    // new claims (If-None-Match: *): only the first of two competing claims wins
    let won = store.claim_ownership(&[claim("0", "a", None), claim("1", "a", None)]).unwrap();
    assert_eq!(won.len(), 2);
    assert!(won.iter().all(|o| o.etag.is_some() && o.last_modified > 0));
    assert!(store.claim_ownership(&[claim("0", "b", None)]).unwrap().is_empty(), "already owned");
    let (_, meta) = az.blob_info("owners", "ns/hub/cg/ownership/0").expect("ownership blob");
    assert!(meta.iter().any(|(k, v)| k == "ownerid" && v == "a"), "{:?}", meta);
    let listed = store.list_ownership("NS", "Hub", "cg").unwrap();
    assert_eq!(listed.iter().map(|o| (o.partition_id.as_str(), o.owner_id.as_str())).collect::<Vec<_>>(), vec![("0", "a"), ("1", "a")]);
    assert_eq!(listed[0].etag, won[0].etag);
    // etag-checked steal: the current etag wins, a stale one loses
    let stolen = store.claim_ownership(&[claim("0", "b", listed[0].etag.clone())]).unwrap();
    assert_eq!(stolen.len(), 1);
    assert_ne!(stolen[0].etag, listed[0].etag);
    assert!(store.claim_ownership(&[claim("0", "a", listed[0].etag.clone())]).unwrap().is_empty(), "stale etag");
    let renewed = store.claim_ownership(&[claim("1", "a", listed[1].etag.clone())]).unwrap();
    assert_eq!(renewed.len(), 1);
    let owners: Vec<String> = store.list_ownership("ns", "hub", "cg").unwrap().into_iter().map(|o| o.owner_id).collect();
    assert_eq!(owners, vec!["b", "a"]);
    // checkpoints and ownership side by side
    store.update("ns", "hub", "cg", "0", cameleer_amqp10::eventhubs::Checkpoint { offset: 7, sequence_number: 3 }).unwrap();
    assert_eq!(store.list_ownership("ns", "hub", "cg").unwrap().len(), 2, "checkpoint blobs are not ownership records");

    // two consumers balance an event hub's partitions through the blob store
    let b = Amqp10Broker::builder().azure(KEY_NAME, KEY).event_hub("lb", 4).start().unwrap();
    let eh = |o: &str| format!("azure-eventhubs:?connectionString=RAW({};EntityPath=lb){}", b.connection_string(KEY_NAME, KEY), o);
    let blob_opts = format!("&blobAccountName={}&blobContainerName=lb&blobAccessKey=RAW({})&blobEndpoint={}", ACCOUNT, ACCOUNT_KEY, az.blob_url());
    let seen: Arc<std::sync::Mutex<Vec<(String, String)>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mk = |owner: &str| {
        let ctx = context();
        let from = eh(&format!("&initialPosition=earliest&checkpointBatchSize=1&loadBalancingUpdateInterval=100&partitionOwnershipExpirationInterval=2000&ownerId={}{}", owner, blob_opts));
        let (seen, owner) = (seen.clone(), owner.to_string());
        ctx.add_routes(move |r| {
            r.from(&from).process(move |ex: &mut Exchange| {
                seen.lock().unwrap().push((owner.clone(), ex.body_as::<String>().unwrap_or_default()));
                Ok(())
            });
        })
        .unwrap();
        ctx
    };
    let lb_store = BlobCheckpointStore::new(&cameleer_amqp10::eventhubs::BlobCheckpointOptions { container_name: "lb".into(), ..opts.clone() }).unwrap();
    let owned = |owner: &str| lb_store.list_ownership("127.0.0.1", "lb", "$Default").map(|v| v.iter().filter(|o| o.owner_id == owner).count()).unwrap_or(0);
    let c1 = mk("owner-a");
    c1.start().unwrap();
    assert!(b.wait_for(Duration::from_secs(10), |_| owned("owner-a") == 4));
    let c2 = mk("owner-b");
    c2.start().unwrap();
    assert!(b.wait_for(Duration::from_secs(15), |_| owned("owner-a") == 2 && owned("owner-b") == 2), "a={} b={}", owned("owner-a"), owned("owner-b"));
    let t = c1.create_producer_template();
    for p in 0..4 {
        t.send_body(&eh(&format!("&partitionId={}", p)), format!("p{}", p)).unwrap();
    }
    assert!(b.wait_for(Duration::from_secs(10), |_| seen.lock().unwrap().len() >= 4));
    std::thread::sleep(Duration::from_millis(200));
    {
        let s = seen.lock().unwrap();
        let mut bodies: Vec<String> = s.iter().map(|x| x.1.clone()).collect();
        bodies.sort();
        assert_eq!(bodies, vec!["p0", "p1", "p2", "p3"], "each event once");
        assert!(s.iter().any(|x| x.0 == "owner-a") && s.iter().any(|x| x.0 == "owner-b"), "{:?}", s);
    }
    c2.stop().unwrap();
    assert!(b.wait_for(Duration::from_secs(15), |_| owned("owner-a") == 4));
    c1.stop().unwrap();
}
