//! Remaining Camel Google capabilities against the verifying emulator: workload identity
//! federation / metadata server / authorized-user credentials, resumable uploads, Pub/Sub
//! exactly-once streaming acks, BigQuery tables, Cloud Functions management, Firestore batched
//! writes / transactions / listeners, Sheets batchUpdate, Workspace sync tokens and watch
//! channels, Vertex AI streaming / embeddings / partner models and Pub/Sub Lite.

use camel_cloud::testing::gcp::{CLIENT_EMAIL, PROJECT};
use camel_cloud::testing::GcpMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

struct Env {
    mock: GcpMock,
    key: std::path::PathBuf,
}

impl Env {
    fn new() -> Env {
        let mock = GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        Env { mock, key }
    }
    fn opts(&self) -> String {
        self.mock.endpoint_options(&self.key)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
    }
}

fn s(v: &Value) -> String {
    v.to_string()
}

fn call(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Result<Exchange> {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    call(t, uri, body, headers).unwrap_or_else(|e| panic!("{}: {:?}", uri, e))
}

fn temp_file(name: &str, content: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("camel-gcp-{}-{}", camel_cloud::util::random_hex(6), name));
    std::fs::write(&p, content).unwrap();
    p
}

fn wait(f: &dyn Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !f() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    f()
}

#[test]
fn storage_credentials_and_resumable_uploads() {
    let env = Env::new();
    let m = &env.mock;
    m.create_bucket("data");
    let ctx = context();
    let t = ctx.create_producer_template();
    let ep = format!("uriEndpointOverride={}&overrideEndpoint=true&autoCreateBucket=false", m.url());
    // workload identity federation: subject token file -> STS -> impersonated service account
    let audience = "//iam.googleapis.com/projects/123/locations/global/workloadIdentityPools/pool/providers/github";
    let token_file = temp_file("oidc.jwt", "eyJhbGciOiJSUzI1NiJ9.github-actions.sig\n");
    let cred = temp_file("wif.json", &m.external_account_json(audience, &token_file.display().to_string(), Some(CLIENT_EMAIL)));
    let uri = |cred_opts: &str, name: &str| format!("google-storage://data?objectName={}&{}&{}", name, cred_opts, ep);
    let wif = format!("serviceAccountKey=file:{}", cred.display());
    let e = call(&t, &uri(&wif, "a.txt"), "x", &[]).unwrap_err();
    assert!(e.message.contains("token exchange failed"), "{}", e);
    m.add_workload_identity_provider(audience, "eyJhbGciOiJSUzI1NiJ9.github-actions.sig");
    req(&t, &uri(&wif, "a.txt"), "via wif", &[]);
    assert_eq!(m.object("data", "a.txt").unwrap(), b"via wif");
    assert_eq!(m.sts_requests(), 1);
    assert_eq!(m.impersonations(), vec![CLIENT_EMAIL.to_string()]);
    // the federated token alone (no impersonation)
    let direct = temp_file("wif2.json", &m.external_account_json(audience, &token_file.display().to_string(), None));
    req(&t, &uri(&format!("serviceAccountKey=file:{}", direct.display()), "b.txt"), "direct", &[]);
    assert_eq!(m.sts_requests(), 2);
    // metadata server (GCE / GKE workload identity)
    req(&t, &uri(&format!("metadataServerUrl={}", m.metadata_server_url()), "c.txt"), "metadata", &[]);
    assert_eq!(m.metadata_requests(), 1);
    // authorized-user credentials file
    m.add_user_credentials("cid", "csecret", "rtoken", "dev@example.com", &["https://www.googleapis.com/auth/cloud-platform"]);
    let user = temp_file("adc.json", &format!(r#"{{"type":"authorized_user","client_id":"cid","client_secret":"csecret","refresh_token":"rtoken","token_uri":"{}"}}"#, m.token_uri()));
    req(&t, &uri(&format!("serviceAccountKey=file:{}", user.display()), "d.txt"), "user", &[]);
    assert_eq!(m.object("data", "d.txt").unwrap(), b"user");
    // resumable upload in 256 KiB chunks, with a failed chunk resumed from the committed size
    let data: Vec<u8> = (0..(700 * 1024)).map(|i| (i % 251) as u8).collect();
    let key_opts = format!("serviceAccountKey=file:{}&resumableUploadThreshold=0&resumableChunkSize=262144", env.key.display());
    m.fail_next_resumable_chunk();
    let ex = req(&t, &uri(&key_opts, "big.bin"), Value::Bytes(data.clone()), &[]);
    assert!(ex.exception.is_none());
    assert_eq!(m.object("data", "big.bin").unwrap(), data);
    assert!(m.resumable_chunks() >= 4, "3 chunks + the failed one: {}", m.resumable_chunks());
    // small bodies below the threshold use simple uploads
    let before = m.resumable_chunks();
    req(&t, &uri(&format!("serviceAccountKey=file:{}", env.key.display()), "small.txt"), "tiny", &[]);
    assert_eq!(m.resumable_chunks(), before);
    for f in [token_file, cred, direct, user] {
        let _ = std::fs::remove_file(f);
    }
    let _ = env.opts();
}

#[test]
fn pubsub_streaming_exactly_once_confirmations_and_dead_letters() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let admin = |dest: &str, extra: &str| format!("google-pubsub://{}:{}?synchronousPull=true&{}{}", PROJECT, dest, env.opts(), extra);
    let stream = |dest: &str, extra: &str| format!("google-pubsub://{}:{}?{}{}", PROJECT, dest, env.opts(), extra);
    req(&t, &admin("jobs", "&operation=createTopic"), "", &[]);
    req(&t, &admin("jobs-dlq", "&operation=createTopic"), "", &[]);
    req(&t, &admin("dlq-sub", "&operation=createSubscription&topic=jobs-dlq"), "", &[]);
    for sub in ["eo-retry", "eo-expired"] {
        req(&t, &admin(sub, "&operation=createSubscription&topic=jobs&enableExactlyOnceDelivery=true&ackDeadlineSeconds=1"), "", &[]);
    }
    req(&t, &admin("poison-sub", "&operation=createSubscription&topic=jobs&deadLetterTopic=jobs-dlq&maxDeliveryAttempts=5"), "", &[]);
    env.mock.fail_next_streaming_acks(1);
    let retry_seen = Arc::new(AtomicUsize::new(0));
    let expired_seen = Arc::new(AtomicUsize::new(0));
    let poison_seen = Arc::new(AtomicUsize::new(0));
    let (r2, e2, p2) = (retry_seen.clone(), expired_seen.clone(), poison_seen.clone());
    let (u1, u2, u3, u4) = (
        stream("eo-retry", "&ackDeadlineSeconds=10"),
        // no lease extension: the first delivery's ack arrives after the 1 s lease expired
        stream("eo-expired", "&ackDeadlineSeconds=1&maxAckExtensionPeriod=0"),
        stream("poison-sub", "&ackDeadlineSeconds=10"),
        stream("dlq-sub", ""),
    );
    ctx.add_routes(move |b| {
        b.from(&u1).process(move |_ex: &mut Exchange| -> Result<()> {
            r2.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        b.from(&u2).process(move |ex: &mut Exchange| -> Result<()> {
            e2.fetch_add(1, Ordering::SeqCst);
            if ex.header("CamelGooglePubsubDeliveryAttempt").and_then(|a| a.as_i64()) == Some(1) {
                std::thread::sleep(Duration::from_millis(1400));
            }
            Ok(())
        });
        b.from(&u3).process(move |ex: &mut Exchange| -> Result<()> {
            p2.fetch_add(1, Ordering::SeqCst);
            assert!(ex.header("CamelGooglePubsubDeliveryAttempt").is_some());
            Err(CamelError::illegal_state("cannot process"))
        });
        b.from(&u4).convert_body_to("String").to("mock:dlq");
    })
    .unwrap();
    let dlq = ctx.mock_endpoint("mock:dlq").unwrap();
    ctx.start().unwrap();
    t.send_body(&admin("jobs", ""), "job-1").unwrap();
    let sub = |n: &str| format!("projects/{}/subscriptions/{}", PROJECT, n);
    // a temporarily failed acknowledgement is retried and then confirmed
    assert!(wait(&|| camel_cloud::gcp::pubsub_stream::exactly_once_stats(&sub("eo-retry")).confirmed == 1), "{:?}", camel_cloud::gcp::pubsub_stream::exactly_once_stats(&sub("eo-retry")));
    assert_eq!(camel_cloud::gcp::pubsub_stream::exactly_once_stats(&sub("eo-retry")).retried, 1);
    // an acknowledgement of an expired lease is invalid: the message is redelivered and confirmed
    assert!(wait(&|| camel_cloud::gcp::pubsub_stream::exactly_once_stats(&sub("eo-expired")).confirmed == 1));
    assert_eq!(camel_cloud::gcp::pubsub_stream::exactly_once_stats(&sub("eo-expired")).invalid, 1);
    assert_eq!(expired_seen.load(Ordering::SeqCst), 2);
    let stats = env.mock.streaming_stats();
    assert_eq!((stats.invalid_acks, stats.temporary_failed_acks), (1, 1));
    // nacked messages go to the dead-letter topic after maxDeliveryAttempts streaming deliveries
    assert!(wait(&|| dlq.received_exchanges().len() == 1));
    assert_eq!(poison_seen.load(Ordering::SeqCst), 5);
    assert_eq!(s(dlq.received_exchanges()[0].body()), "job-1");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(retry_seen.load(Ordering::SeqCst), 1, "confirmed: not redelivered");
    ctx.stop().unwrap();
}

#[test]
fn bigquery_datasets_and_tables() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let bq = |table: &str, extra: &str| format!("google-bigquery://{}:sales{}?{}{}", PROJECT, table, env.opts(), extra);
    req(&t, &bq("", "&operation=createDataset&datasetLocation=EU"), "", &[]);
    assert!(call(&t, &bq("", "&operation=createDataset"), "", &[]).unwrap_err().message.contains("Already Exists"));
    let schema = Value::Array(vec![Value::object([("name", "id"), ("type", "STRING"), ("mode", "REQUIRED")]), Value::object([("name", "amount"), ("type", "FLOAT")]), Value::object([("name", "ts"), ("type", "TIMESTAMP")])]);
    let table = req(&t, &bq(":orders", "&operation=createTable&timePartitioningField=ts&clusteringFields=id&tableDescription=Orders"), "", &[("CamelGoogleBigQueryTableSchema", schema)]).body().clone();
    assert_eq!(s(table.get("id").unwrap()), format!("{}:sales.orders", PROJECT));
    assert_eq!(s(table.get("timePartitioning").unwrap().get("field").unwrap()), "ts");
    req(&t, &bq(":orders", ""), Value::object([("id", Value::from("o1")), ("amount", Value::Float(9.5)), ("ts", Value::from("2026-01-01T00:00:00Z"))]), &[]);
    assert_eq!(env.mock.bq_rows(PROJECT, "sales", "orders").len(), 1);
    let got = req(&t, &bq(":orders", "&operation=getTable"), "", &[]).body().clone();
    assert_eq!(s(got.get("numRows").unwrap()), "1");
    assert_eq!(got.get("schema").unwrap().get("fields").unwrap().as_array().unwrap().len(), 3);
    assert!(call(&t, &bq(":bad", "&operation=createTable&tableSchema=RAW(x:FLOAT)&timePartitioningField=x"), "", &[]).unwrap_err().message.contains("TIMESTAMP or DATE"));
    // streaming insert creating a missing table from the configured schema
    req(&t, &bq(":events", "&createTableIfNotExists=true&tableSchema=RAW(name:STRING,count:INTEGER)"), Value::object([("name", Value::from("click")), ("count", Value::Int(3))]), &[]);
    assert_eq!(env.mock.bq_rows(PROJECT, "sales", "events").len(), 1);
    let mut tables: Vec<String> = req(&t, &bq("", "&operation=listTables"), "", &[]).body().as_array().unwrap().iter().map(s).collect();
    tables.sort();
    assert_eq!(tables, vec!["events", "orders"]);
    assert!(req(&t, &bq("", "&operation=listDatasets"), "", &[]).body().as_array().unwrap().iter().any(|d| s(d) == "sales"));
    req(&t, &bq(":events", "&operation=deleteTable"), "", &[]);
    assert!(call(&t, &bq(":events", "&operation=getTable"), "", &[]).is_err());
    assert!(call(&t, &bq("", "&operation=deleteDataset"), "", &[]).unwrap_err().message.contains("still in use"));
    req(&t, &bq("", "&operation=deleteDataset&deleteContents=true"), "", &[]);
    assert!(req(&t, &bq("", "&operation=listTables"), "", &[]).body().as_array().unwrap().is_empty());
}

#[test]
fn cloud_functions_management() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let f = |name: &str, op: &str| format!("google-functions://{}?project={}&operation={}&{}", name, PROJECT, op, env.opts());
    // upload a source archive through a signed upload URL, then deploy
    let url = s(req(&t, &f("hello", "generateUploadUrl"), Value::Bytes(b"PK\x03\x04zip".to_vec()), &[]).body());
    assert!(url.contains("/fn-uploads/"));
    let e = call(&t, &f("hello", "createFunction"), "", &[("CamelGoogleCloudFunctionsRuntime", Value::from("nodejs20"))]).unwrap_err();
    assert!(e.message.contains("entryPoint and runtime"), "{}", e);
    let created = req(&t, &f("hello", "createFunction"), "", &[("CamelGoogleCloudFunctionsEntryPoint", Value::from("handler")), ("CamelGoogleCloudFunctionsRuntime", Value::from("nodejs20")), ("CamelGoogleCloudFunctionsSourceUploadUrl", Value::from(url.as_str()))]).body().clone();
    assert_eq!(s(created.get("status").unwrap()), "ACTIVE");
    assert!(s(created.get("httpsTrigger").unwrap().get("url").unwrap()).ends_with("/hello"));
    assert_eq!(s(req(&t, &f("hello", "callFunction"), "ping", &[]).body()), "ping");
    let dl = s(req(&t, &f("hello", "generateDownloadUrl"), "", &[]).body());
    let r = camel_cloud::http::HttpClient::new().send(&camel_cloud::http::Request::new("GET", camel_cloud::http::Url::parse(&dl).unwrap())).unwrap();
    assert_eq!(r.body, b"PK\x03\x04zip");
    // update from a gs:// archive
    let updated = req(&t, &f("hello", "updateFunction"), "", &[("CamelGoogleCloudFunctionsRuntime", Value::from("nodejs22")), ("CamelGoogleCloudFunctionsSourceArchiveUrl", Value::from("gs://src/hello-v2.zip"))]).body().clone();
    assert_eq!((s(updated.get("runtime").unwrap()), s(updated.get("versionId").unwrap())), ("nodejs22".to_string(), "2".to_string()));
    assert_eq!(s(req(&t, &f("hello", "getFunction"), "", &[]).body().get("sourceArchiveUrl").unwrap()), "gs://src/hello-v2.zip");
    assert!(call(&t, &f("hello", "createFunction"), "", &[("CamelGoogleCloudFunctionsEntryPoint", Value::from("h")), ("CamelGoogleCloudFunctionsRuntime", Value::from("go122")), ("CamelGoogleCloudFunctionsSourceArchiveUrl", Value::from("gs://x/y.zip"))]).unwrap_err().message.contains("already exists"));
    assert!(req(&t, &f("hello", "listFunctions"), "", &[]).body().as_array().unwrap().iter().any(|n| s(n).ends_with("/hello")));
    req(&t, &f("hello", "deleteFunction"), "", &[]);
}

#[test]
fn firestore_batched_writes_transactions_and_listener() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let fs = |op: &str| format!("google-firestore://{}:accounts?operation={}&{}", PROJECT, op, env.opts());
    let w = |op: &str, id: &str, data: Value| Value::object([("op", Value::from(op)), ("id", Value::from(id)), ("data", data)]);
    // atomic commit
    let r = req(&t, &fs("commit"), Value::Array(vec![w("create", "alice", Value::object([("balance", 100)])), w("create", "bob", Value::object([("balance", 50)]))]), &[]);
    assert_eq!(r.body().as_array().unwrap().len(), 2);
    assert!(r.header("CamelGoogleFirestoreCommitTime").is_some());
    // a failing precondition aborts the whole commit
    let e = call(&t, &fs("commit"), Value::Array(vec![w("update", "alice", Value::object([("balance", 0)])), w("create", "bob", Value::object([("balance", 1)]))]), &[]).unwrap_err();
    assert!(e.message.contains("already exists"), "{}", e);
    assert_eq!(env.mock.firestore_doc(PROJECT, "accounts/alice").unwrap().get("balance").and_then(|b| b.as_i64()), Some(100));
    // batchWrite: independent writes with per-write status
    let st = req(&t, &fs("batchWrite"), Value::Array(vec![w("update", "nobody", Value::object([("x", 1)])), w("set", "carol", Value::object([("balance", 7)])), w("delete", "bob", Value::Null)]), &[]).body().clone();
    let codes: Vec<i64> = st.as_array().unwrap().iter().map(|x| x.get("code").and_then(|c| c.as_i64()).unwrap()).collect();
    assert_eq!(codes, vec![5, 0, 0]);
    assert!(env.mock.firestore_doc(PROJECT, "accounts/bob").is_none());
    // transaction: read, then commit a transfer
    let tx = s(req(&t, &fs("beginTransaction"), "", &[]).body());
    let alice = req(&t, &fs("getDocument"), "", &[("CamelGoogleFirestoreDocumentId", Value::from("alice")), ("CamelGoogleFirestoreTransaction", Value::from(tx.as_str()))]).body().clone();
    let bal = alice.get("balance").and_then(|b| b.as_i64()).unwrap();
    req(&t, &fs("commit"), Value::Array(vec![w("update", "alice", Value::object([("balance", bal - 30)])), w("update", "carol", Value::object([("balance", 37)]))]), &[("CamelGoogleFirestoreTransaction", Value::from(tx.as_str()))]);
    assert_eq!(env.mock.firestore_doc(PROJECT, "accounts/alice").unwrap().get("balance").and_then(|b| b.as_i64()), Some(70));
    // a concurrent change of a document read in the transaction aborts it
    let tx = s(req(&t, &fs("beginTransaction"), "", &[]).body());
    req(&t, &fs("getDocument"), "", &[("CamelGoogleFirestoreDocumentId", Value::from("alice")), ("CamelGoogleFirestoreTransaction", Value::from(tx.as_str()))]);
    std::thread::sleep(std::time::Duration::from_millis(5));
    req(&t, &fs("setDocument"), Value::object([("balance", 1)]), &[("CamelGoogleFirestoreDocumentId", Value::from("alice"))]);
    let e = call(&t, &fs("commit"), Value::Array(vec![w("update", "alice", Value::object([("balance", 999)]))]), &[("CamelGoogleFirestoreTransaction", Value::from(tx.as_str()))]).unwrap_err();
    assert!(e.message.contains("ABORTED"), "{}", e);
    assert_eq!(env.mock.firestore_aborted_transactions(), 1);
    // read-only transaction + rollback
    let tx = s(req(&t, &fs("beginTransaction"), "", &[("CamelGoogleFirestoreReadOnly", Value::Bool(true))]).body());
    assert!(call(&t, &fs("commit"), Value::Array(vec![w("set", "x", Value::object([("a", 1)]))]), &[("CamelGoogleFirestoreTransaction", Value::from(tx.as_str()))]).is_err());
    let tx = s(req(&t, &fs("beginTransaction"), "", &[]).body());
    req(&t, &fs("rollback"), "", &[("CamelGoogleFirestoreTransaction", Value::from(tx.as_str()))]);
    // snapshot listener
    let from = format!("google-firestore://{}:accounts?delay=30&initialDelay=0&{}", PROJECT, env.opts());
    let c2 = context();
    c2.add_routes(move |r| {
        r.from(&from).to("mock:changes");
    })
    .unwrap();
    let m = c2.mock_endpoint("mock:changes").unwrap();
    c2.start().unwrap();
    assert!(wait(&|| m.received_exchanges().len() == 2), "initial snapshot: alice + carol");
    std::thread::sleep(std::time::Duration::from_millis(50));
    req(&t, &fs("setDocument"), Value::object([("balance", 5)]), &[("CamelGoogleFirestoreDocumentId", Value::from("dave"))]);
    std::thread::sleep(std::time::Duration::from_millis(80));
    req(&t, &fs("updateDocument"), Value::object([("balance", 6)]), &[("CamelGoogleFirestoreDocumentId", Value::from("carol"))]);
    std::thread::sleep(std::time::Duration::from_millis(80));
    req(&t, &fs("deleteDocument"), "", &[("CamelGoogleFirestoreDocumentId", Value::from("alice"))]);
    assert!(wait(&|| m.received_exchanges().len() == 5), "{}", m.received_exchanges().len());
    let kinds: Vec<(String, String)> = m.received_exchanges().iter().map(|e| (s(e.header("CamelGoogleFirestoreChangeType").unwrap()), s(e.header("CamelGoogleFirestoreDocumentId").unwrap()))).collect();
    assert_eq!(&kinds[2..], &[("ADDED".to_string(), "dave".to_string()), ("MODIFIED".to_string(), "carol".to_string()), ("REMOVED".to_string(), "alice".to_string())]);
    assert_eq!(m.received_exchanges()[3].body().get("balance").and_then(|b| b.as_i64()), Some(6));
    c2.stop().unwrap();
}

#[test]
fn sheets_batch_updates() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let created = req(&t, &format!("google-sheets://spreadsheets/create?title=Plan&{}", env.opts()), "", &[]);
    let id = s(created.header("CamelGoogleSheets.spreadsheetId").unwrap());
    let uri = |api: &str| format!("google-sheets://{}?spreadsheetId={}&{}", api, id, env.opts());
    // values:batchUpdate / batchClear
    let data = Value::Array(vec![
        Value::object([("range", Value::from("Sheet1!A1:B2")), ("values", Value::Array(vec![Value::Array(vec!["name".into(), "qty".into()]), Value::Array(vec!["apple".into(), "3".into()])]))]),
        Value::object([("range", Value::from("Sheet1!D1")), ("values", Value::Array(vec![Value::Array(vec!["note: apple pie".into()])]))]),
    ]);
    let r = req(&t, &uri("data/batchUpdate"), data, &[]).body().clone();
    assert_eq!(r.get("totalUpdatedCells").and_then(|v| v.as_i64()), Some(5));
    // spreadsheet batchUpdate: rename, add / duplicate / rename sheets, find & replace, insert rows
    let requests = Value::Array(vec![
        Value::object([("updateSpreadsheetProperties", Value::object([("properties", Value::object([("title", "Plan 2027")])), ("fields", Value::from("title"))]))]),
        Value::object([("addSheet", Value::object([("properties", Value::object([("title", "Archive")]))]))]),
        Value::object([("findReplace", Value::object([("find", Value::from("apple")), ("replacement", Value::from("pear")), ("allSheets", Value::Bool(true))]))]),
        Value::object([("duplicateSheet", Value::object([("sourceSheetId", Value::Int(0)), ("sheetId", Value::Int(0)), ("newSheetName", Value::from("Copy"))]))]),
        Value::object([("insertDimension", Value::object([("range", Value::object([("sheetId", Value::Int(0)), ("dimension", Value::from("ROWS")), ("startIndex", Value::Int(1)), ("endIndex", Value::Int(2))]))]))]),
    ]);
    let r = req(&t, &uri("spreadsheets/batchUpdate"), requests, &[]).body().clone();
    let replies = r.get("replies").unwrap().as_array().unwrap().clone();
    assert_eq!(s(replies[1].get("addSheet").unwrap().get("properties").unwrap().get("title").unwrap()), "Archive");
    assert_eq!(replies[2].get("findReplace").unwrap().get("occurrencesChanged").and_then(|v| v.as_i64()), Some(2));
    let meta = req(&t, &uri("spreadsheets/get"), "", &[]).body().clone();
    assert_eq!(s(meta.get("properties").unwrap().get("title").unwrap()), "Plan 2027");
    let titles: Vec<String> = meta.get("sheets").unwrap().as_array().unwrap().iter().map(|x| s(x.get("properties").unwrap().get("title").unwrap())).collect();
    assert_eq!(titles, vec!["Sheet1", "Archive", "Copy"]);
    let rows = req(&t, &uri("data/get"), "", &[("CamelGoogleSheets.range", Value::from("Sheet1!A1:B3"))]).body().clone();
    assert_eq!(rows, Value::Array(vec![Value::Array(vec!["name".into(), "qty".into()]), Value::Array(vec![]), Value::Array(vec!["pear".into(), "3".into()])]));
    // an invalid request leaves everything unchanged
    let bad = Value::Array(vec![Value::object([("deleteSheet", Value::object([("sheetId", Value::Int(1))]))]), Value::object([("deleteSheet", Value::object([("sheetId", Value::Int(42))]))])]);
    assert!(call(&t, &uri("spreadsheets/batchUpdate"), bad, &[]).unwrap_err().message.contains("No grid with id"));
    assert_eq!(req(&t, &uri("spreadsheets/get"), "", &[]).body().get("sheets").unwrap().as_array().unwrap().len(), 3);
    let cleared = req(&t, &uri("data/batchClear"), "", &[("CamelGoogleSheets.ranges", Value::from(vec!["Sheet1!D1", "Copy!A1:B2"]))]).body().clone();
    assert_eq!(cleared.as_array().unwrap().len(), 2);
    assert!(req(&t, &uri("data/get"), "", &[("CamelGoogleSheets.range", Value::from("Copy!A1:B2"))]).body().as_array().unwrap().iter().all(|r| r.as_array().unwrap().is_empty()));
}

#[test]
fn vertexai_streaming_embeddings_images_and_partner_models() {
    let env = Env::new();
    let ctx = context();
    let chunks: std::sync::Arc<std::sync::Mutex<Vec<String>>> = Default::default();
    let c2 = chunks.clone();
    let p: std::sync::Arc<dyn camel_core::spi::Processor> = std::sync::Arc::new(move |ex: &mut Exchange| -> Result<()> {
        c2.lock().unwrap().push(s(ex.body()));
        Ok(())
    });
    ctx.registry().bind("onChunk", p);
    let t = ctx.create_producer_template();
    let v = |model: &str, extra: &str| format!("google-vertexai:{}:us-central1:{}?{}{}", PROJECT, model, env.opts(), extra);
    let ex = req(&t, &v("gemini-1.5-pro", "&operation=generateChatStreaming&streamingChunkProcessor=#onChunk"), "stream this answer", &[]);
    assert_eq!(s(ex.body()), "Echo: stream this answer");
    assert_eq!(ex.header("CamelGoogleVertexAIStreamingChunkCount").and_then(|c| c.as_i64()), Some(4));
    assert_eq!(s(ex.header("CamelGoogleVertexAIFinishReason").unwrap()), "STOP");
    assert!(ex.header("CamelGoogleVertexAITotalTokenCount").is_some());
    assert_eq!(chunks.lock().unwrap().len(), 4);
    let ex = req(&t, &v("gemini-1.5-pro", "&operation=generateTextStreaming&streamOutputMode=chunks"), "hi", &[]);
    assert_eq!(ex.body().as_array().unwrap().len(), 2);
    // embeddings
    let one = req(&t, &v("text-embedding-004", "&operation=generateEmbeddings&outputDimensionality=4&taskType=RETRIEVAL_DOCUMENT"), "hello world", &[]).body().clone();
    assert_eq!(one.as_array().unwrap().len(), 4);
    let many = req(&t, &v("text-embedding-004", "&operation=generateEmbeddings"), Value::from(vec!["a", "bb", "ccc"]), &[]).body().clone();
    assert_eq!(many.as_array().unwrap().len(), 3);
    assert_eq!(many.as_array().unwrap()[0].as_array().unwrap().len(), 8);
    assert_eq!(env.mock.vertex_requests().last().unwrap().get("instances").unwrap().as_array().unwrap().len(), 3);
    // Imagen
    let imgs = req(&t, &v("imagen-3.0-generate-001", "&operation=generateImage&sampleCount=2"), "a lighthouse", &[]).body().clone();
    assert_eq!(imgs.as_array().unwrap().iter().map(|b| b.to_bytes()).collect::<Vec<_>>(), vec![b"PNG#0:a lighthouse".to_vec(), b"PNG#1:a lighthouse".to_vec()]);
    // partner model (Anthropic Claude on Vertex)
    let claude = |op: &str| v("claude-3-5-sonnet@20240620", &format!("&publisher=anthropic&operation={}&maxOutputTokens=256&systemInstruction=RAW(be brief)", op));
    let r = req(&t, &claude("rawPredict"), "what is camel?", &[]);
    assert_eq!(s(r.header("CamelGoogleVertexAIResponseText").unwrap()), "[be brief] Claude says: what is camel?");
    assert_eq!(s(r.header("CamelGoogleVertexAIFinishReason").unwrap()), "end_turn");
    let sent = env.mock.vertex_requests().pop().unwrap();
    assert_eq!(sent.get("max_tokens").and_then(|m| m.as_i64()), Some(256));
    let r = req(&t, &claude("streamRawPredict"), "stream it", &[]);
    assert_eq!(s(r.body()), "[be brief] Claude says: stream it");
    assert_eq!(s(r.header("CamelGoogleVertexAIFinishReason").unwrap()), "end_turn");
    let raw = Value::object([("anthropic_version", Value::from("bad")), ("max_tokens", Value::Int(5)), ("messages", Value::Array(vec![]))]);
    assert!(call(&t, &claude("rawPredict"), raw, &[]).unwrap_err().message.contains("anthropic_version"));
}

#[test]
fn pubsub_lite_publish_subscribe_and_cursors() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let lite = |dest: &str, extra: &str| format!("google-pubsub-lite:{}:us-central1-a:{}?{}{}", PROJECT, dest, env.opts(), extra);
    req(&t, &lite("orders", "&operation=createTopic&partitionCount=3"), "", &[]);
    assert!(call(&t, &lite("orders", "&operation=createTopic"), "", &[]).unwrap_err().message.contains("already exists"));
    assert_eq!(req(&t, &lite("orders", "&operation=getTopicPartitions"), "", &[]).body().as_i64(), Some(3));
    req(&t, &lite("orders-sub", "&operation=createSubscription&topic=orders"), "", &[]);
    // ordering keys pin partitions, others go round robin
    let a = req(&t, &lite("orders", ""), "a1", &[("CamelGooglePubsubLiteOrderingKey", Value::from("customer-1")), ("CamelGooglePubsubLiteAttributes", Value::object([("kind", "new")]))]);
    let b = req(&t, &lite("orders", ""), "a2", &[("CamelGooglePubsubLiteOrderingKey", Value::from("customer-1"))]);
    assert_eq!(a.header("CamelGooglePubsubLitePartition"), b.header("CamelGooglePubsubLitePartition"));
    let p = a.header("CamelGooglePubsubLitePartition").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(s(a.header("CamelGooglePubsubLiteMessageId").unwrap()), format!("{}:0", p));
    assert_eq!(s(b.header("CamelGooglePubsubLiteMessageId").unwrap()), format!("{}:1", p));
    let ids = req(&t, &lite("orders", ""), Value::Array(vec!["x".into(), "y".into(), "z".into()]), &[]).header("CamelGooglePubsubLiteMessageId").unwrap().clone();
    assert_eq!(ids.as_array().unwrap().len(), 3);
    let topic_path = format!("projects/{}/locations/us-central1-a/topics/orders", PROJECT);
    assert_eq!(env.mock.lite_messages(&topic_path, p as usize)[..2], [b"a1".to_vec(), b"a2".to_vec()]);
    // consumer: every message once, the first failure redelivered after a later poll
    let failed_once = std::sync::Arc::new(AtomicUsize::new(0));
    let f2 = failed_once.clone();
    let from = lite("orders-sub", "&delay=30&initialDelay=0&streamIdleTimeout=100");
    ctx.add_routes(move |r| {
        r.from(&from)
            .process(move |ex: &mut Exchange| -> Result<()> {
                if s(ex.body()) == "y" && f2.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(CamelError::illegal_state("transient"));
                }
                Ok(())
            })
            .to("mock:lite");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:lite").unwrap();
    ctx.start().unwrap();
    assert!(wait(&|| m.received_exchanges().len() >= 5), "{}", m.received_exchanges().len());
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut bodies: Vec<String> = m.received_exchanges().iter().map(|e| s(e.body())).collect();
    bodies.sort();
    assert_eq!(bodies, vec!["a1", "a2", "x", "y", "z"]);
    let first = m.received_exchanges().iter().find(|e| s(e.body()) == "a1").unwrap().clone();
    assert_eq!(s(first.header("CamelGooglePubsubLiteAttributes").unwrap().get("kind").unwrap()), "new");
    assert_eq!(s(first.header("CamelGooglePubsubLiteOrderingKey").unwrap()), "customer-1");
    assert!(first.header("CamelGooglePubsubLitePublishTime").is_some());
    ctx.stop().unwrap();
    let sub_path = format!("projects/{}/locations/us-central1-a/subscriptions/orders-sub", PROJECT);
    let committed: i64 = env.mock.lite_committed(&sub_path).iter().sum();
    assert_eq!(committed, 5, "cursors committed past every message");
    // a new consumer starts from the committed cursors: nothing is redelivered
    let c2 = context();
    let from = lite("orders-sub", "&delay=30&initialDelay=0&streamIdleTimeout=100");
    c2.add_routes(move |r| {
        r.from(&from).to("mock:again");
    })
    .unwrap();
    let m2 = c2.mock_endpoint("mock:again").unwrap();
    c2.start().unwrap();
    t.send_body(&lite("orders", ""), "late").unwrap();
    assert!(wait(&|| m2.received_exchanges().len() == 1));
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(m2.received_exchanges().len(), 1);
    assert_eq!(s(m2.received_exchanges()[0].body()), "late");
    c2.stop().unwrap();
    req(&t, &lite("orders-sub", "&operation=deleteSubscription"), "", &[]);
    req(&t, &lite("orders", "&operation=deleteTopic"), "", &[]);
    assert!(call(&t, &lite("orders", "&operation=getTopicPartitions"), "", &[]).is_err());
}
