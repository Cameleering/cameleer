//! The newer Azure components end-to-end against the verifying emulators: Azure Files (Shared
//! Key / SAS), Functions (function keys), Event Grid (access key, SAS token, Entra ID; CloudEvents
//! and Event Grid schemas), Schema Registry and Azure OpenAI (API key / Entra ID), with negative
//! authentication tests.

use std::time::Duration;

use cameleer_cloud::azure::AccountSas;
use cameleer_cloud::testing::azure::{ACCOUNT, ACCOUNT_KEY};
use cameleer_cloud::testing::azure_services::{CLIENT_ID, CLIENT_SECRET, FUNCTIONS_MASTER_KEY, TENANT_ID};
use cameleer_cloud::testing::{AzureMock, AzuriteMock};
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

fn s(v: &Value) -> String {
    v.to_string()
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
    .unwrap_or_else(|e| panic!("{}: {:?}", uri, e))
}

fn req_err(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> CamelError {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
    .unwrap_err()
    .root_cause()
    .clone()
}

#[test]
fn azure_files_shares_directories_and_files() {
    let az = AzuriteMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = az.files_options();
    let f = |op: &str| format!("azure-storage-files://{}/docs?operation={}&{}", ACCOUNT, op, opts);
    req(&t, &f("createShare"), "", &[]);
    let e = req_err(&t, &f("createShare"), "", &[]);
    assert!(e.is_a("ShareStorageException") && e.message.contains("ShareAlreadyExists"), "{:?}", e);
    req(&t, &f("createDirectory"), "", &[("CamelAzureStorageFilesDirectoryName", "reports".into())]);
    req(&t, &f("createDirectory"), "", &[("CamelAzureStorageFilesDirectoryName", "reports/2026".into())]);
    // a file larger than one 4 MiB range is written in several Put Range calls
    let big: Vec<u8> = (0..(4 * 1024 * 1024 + 100)).map(|i| (i % 251) as u8).collect();
    let ex = req(&t, &f("uploadFile"), Value::Bytes(big.clone()), &[("CamelAzureStorageFilesDirectoryName", "reports/2026".into()), ("CamelAzureStorageFilesFileName", "big.bin".into())]);
    assert!(ex.header("CamelAzureStorageFilesETag").is_some());
    assert_eq!(az.share_file("docs", "reports/2026/big.bin").unwrap(), big);
    req(&t, &format!("{}&directoryName=reports&fileName=q3.txt", f("uploadFile")), "quarter three", &[("CamelAzureStorageFilesContentType", "text/plain".into())]);
    let list = req(&t, &format!("{}&directoryName=reports", f("listDirectoriesAndFiles")), "", &[]);
    let mut entries: Vec<(String, String)> = list.body().as_array().unwrap().iter().map(|e| (s(e.get("name").unwrap()), s(e.get("type").unwrap()))).collect();
    entries.sort();
    assert_eq!(entries, vec![("2026".to_string(), "directory".to_string()), ("q3.txt".to_string(), "file".to_string())]);
    let dl = req(&t, &format!("{}&directoryName=reports&fileName=q3.txt", f("downloadFile")), "", &[]);
    assert_eq!(dl.body().to_bytes(), b"quarter three".to_vec());
    assert_eq!(s(dl.header("CamelAzureStorageFilesContentType").unwrap()), "text/plain");
    assert_eq!(dl.header("CamelAzureStorageFilesContentLength").unwrap().as_i64(), Some(13));
    let props = req(&t, &format!("{}&directoryName=reports/2026&fileName=big.bin", f("getFileProperties")), "", &[]);
    assert_eq!(props.header("CamelAzureStorageFilesContentLength").unwrap().as_i64(), Some(big.len() as i64));
    // a non-empty directory cannot be deleted
    let e = req_err(&t, &f("deleteDirectory"), "", &[("CamelAzureStorageFilesDirectoryName", "reports".into())]);
    assert!(e.message.contains("DirectoryNotEmpty"), "{}", e);
    req(&t, &format!("{}&directoryName=reports&fileName=q3.txt", f("deleteFile")), "", &[]);
    let e = req_err(&t, &format!("{}&directoryName=reports&fileName=q3.txt", f("downloadFile")), "", &[]);
    assert!(e.message.contains("ResourceNotFound"), "{}", e);
    let shares = req(&t, &format!("azure-storage-files://{}?operation=listShares&{}", ACCOUNT, opts), "", &[]);
    assert_eq!(shares.body(), &Value::from(vec!["docs"]));
    // SAS with the file service
    let sas = AccountSas { services: "f".into(), ..AccountSas::full_access(3600) }.token(ACCOUNT, ACCOUNT_KEY).unwrap();
    let sas_uri = format!("azure-storage-files://{}/docs?operation=listDirectoriesAndFiles&credentialType=AZURE_SAS&sasToken=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, sas, az.file_url());
    assert_eq!(req(&t, &sas_uri, "", &[]).body().as_array().unwrap().len(), 1);
    // a blob-only SAS or a wrong key is rejected
    let blob_sas = AccountSas::full_access(3600).token(ACCOUNT, ACCOUNT_KEY).unwrap();
    let e = req_err(&t, &sas_uri.replace(&sas, &blob_sas), "", &[]);
    assert!(e.message.contains("AuthorizationPermissionMismatch"), "{}", e);
    let wrong = format!("azure-storage-files://{}/docs?accessKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ACCOUNT, cameleer_cloud::util::base64_encode(b"wrong-key-wrong-key"), az.file_url());
    let e = req_err(&t, &wrong, "", &[]);
    assert!(e.message.contains("AuthenticationFailed"), "{}", e);
}

#[test]
fn azure_files_consumer() {
    let az = AzuriteMock::start().unwrap();
    az.put_share_file("inbox", "in/a.txt", b"alpha");
    az.put_share_file("inbox", "in/b.txt", b"beta");
    az.put_share_file("inbox", "in/sub/c.txt", b"nested is skipped");
    let ctx = context();
    let opts = az.files_options();
    ctx.add_routes(move |b| {
        b.from(&format!("azure-storage-files://{}/inbox?directoryName=in&delete=true&delay=20&initialDelay=0&{}", ACCOUNT, opts)).to("mock:files");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:files").unwrap();
    m.expected_bodies_received_in_any_order(vec![b"alpha".to_vec(), b"beta".to_vec()]);
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let start = std::time::Instant::now();
    while az.share_files("inbox").len() != 1 && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(az.share_files("inbox"), vec!["in/sub/c.txt"]);
    assert!(m.received_exchanges().iter().all(|e| e.header("CamelFileName").is_some()));
    ctx.stop().unwrap();
}

#[test]
fn azure_functions_with_keys() {
    let az = AzureMock::start().unwrap();
    az.register_function("orders-app", "process", Some("fn-key"), |method, query, body| (200, "application/json".into(), format!("{{\"method\":\"{}\",\"query\":\"{}\",\"echo\":\"{}\"}}", method, query, String::from_utf8_lossy(body)).into_bytes()));
    az.register_function("orders-app", "health", None, |_, _, _| (200, "text/plain".into(), b"ok".to_vec()));
    az.register_function("orders-app", "boom", None, |_, _, _| (500, "text/plain".into(), b"failure".to_vec()));
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = az.functions_options("orders-app");
    let ex = req(&t, &format!("azure-functions://orders-app/process?functionKey=RAW(fn-key)&parseJson=true&{}", o), "order-1", &[("CamelAzureFunctionsQuery", "priority=high".into())]);
    assert_eq!(ex.header("CamelAzureFunctionsStatusCode").unwrap().as_i64(), Some(200));
    assert_eq!(s(ex.body().get("echo").unwrap()), "order-1");
    assert_eq!(s(ex.body().get("method").unwrap()), "POST");
    assert_eq!(s(ex.body().get("query").unwrap()), "priority=high");
    // key as the code query parameter, and the master key
    let ex = req(&t, &format!("azure-functions://orders-app/process?functionKey=RAW(fn-key)&keyInQuery=true&httpMethod=GET&{}", o), "", &[]);
    assert!(s(ex.body()).contains("\"method\":\"GET\""));
    assert!(s(ex.body()).contains("code=fn-key"));
    req(&t, &format!("azure-functions://orders-app/process?masterKey=RAW({})&{}", FUNCTIONS_MASTER_KEY, o), "x", &[]);
    assert_eq!(s(req(&t, &format!("azure-functions://orders-app/health?{}", o), "", &[]).body()), "ok");
    // wrong / missing key → 401
    let e = req_err(&t, &format!("azure-functions://orders-app/process?functionKey=RAW(nope)&{}", o), "x", &[]);
    assert!(e.is_a("HttpResponseException") && e.message.contains("Status code 401"), "{:?}", e);
    let e = req_err(&t, &format!("azure-functions://orders-app/process?{}", o), "x", &[]);
    assert!(e.message.contains("401"), "{}", e);
    let e = req_err(&t, &format!("azure-functions://orders-app/boom?{}", o), "x", &[]);
    assert!(e.message.contains("Status code 500") && e.message.contains("failure"), "{}", e);
    assert!(ctx.get_endpoint("azure-functions://only-app").is_err());
}

#[test]
fn eventgrid_publish_with_key_sas_and_entra_id() {
    let az = AzureMock::start().unwrap();
    let key = cameleer_cloud::util::base64_encode(b"event-grid-topic-key-0123456789");
    az.create_eventgrid_topic("orders", &key);
    let ctx = context();
    let t = ctx.create_producer_template();
    let topic = az.eventgrid_topic_url("orders");
    // CloudEvents (default) with the access key
    let ex = req(
        &t,
        &format!("azure-eventgrid:{}?accessKey=RAW({})", topic, key),
        Value::object([("orderId", 1)]),
        &[("CamelAzureEventGridEventType", "com.acme.order.created".into()), ("CamelAzureEventGridSubject", "orders/1".into()), ("CamelAzureEventGridEventId", "evt-1".into())],
    );
    assert_eq!(s(ex.header("CamelAzureEventGridEventId").unwrap()), "evt-1");
    let ev = az.eventgrid_events("orders").pop().unwrap();
    assert_eq!(s(ev.get("specversion").unwrap()), "1.0");
    assert_eq!(s(ev.get("type").unwrap()), "com.acme.order.created");
    assert_eq!(s(ev.get("source").unwrap()), topic);
    assert_eq!(ev.get("data").unwrap().get("orderId").unwrap().as_i64(), Some(1));
    // Event Grid schema with a generated SAS token, list body = several events
    let ex = req(&t, &format!("azure-eventgrid:{}?credentialType=AZURE_SAS&accessKey=RAW({})&eventSchema=EventGrid", topic, key), Value::from(vec!["a", "b"]), &[]);
    assert_eq!(ex.header("CamelAzureEventGridEventId").unwrap().as_array().unwrap().len(), 2);
    let evs = az.eventgrid_events("orders");
    assert_eq!(evs.len(), 3);
    assert_eq!(s(evs[1].get("eventType").unwrap()), "org.apache.camel.event");
    assert_eq!(s(evs[2].get("data").unwrap()), "b");
    // Entra ID client credentials
    let aad = format!("azure-eventgrid:{}?tenantId={}&clientId={}&clientSecret=RAW({})&authorityHost={}", topic, TENANT_ID, CLIENT_ID, CLIENT_SECRET, az.url());
    req(&t, &aad, "via aad", &[]);
    assert_eq!(az.eventgrid_events("orders").len(), 4);
    // wrong key, forged SAS token
    let e = req_err(&t, &format!("azure-eventgrid:{}?accessKey=RAW(bad)", topic), "x", &[]);
    assert!(e.message.contains("Status code 401"), "{}", e);
    let other = cameleer_cloud::util::base64_encode(b"another-key-another-key-0000");
    let e = req_err(&t, &format!("azure-eventgrid:{}?credentialType=AZURE_SAS&accessKey=RAW({})", topic, other), "x", &[]);
    assert!(e.message.contains("401"), "{}", e);
    assert_eq!(az.eventgrid_events("orders").len(), 4);
    assert!(ctx.get_endpoint(&format!("azure-eventgrid:{}?accessKey=k&eventSchema=Nope", topic)).is_err());
}

#[test]
fn schema_registry_register_and_lookup() {
    let az = AzureMock::start().unwrap();
    az.create_schema_group("payments", "Avro");
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = az.schema_registry_options();
    let sr = |op: &str| format!("azure-schema-registry://myns/payments?operation={}&schemaName=Payment&{}", op, o);
    let v1 = r#"{"type":"record","name":"Payment","fields":[{"name":"id","type":"string"}]}"#;
    let ex = req(&t, &sr("registerSchema"), v1, &[]);
    let id1 = s(ex.body());
    assert_eq!(ex.header("CamelAzureSchemaRegistrySchemaVersion").unwrap().as_i64(), Some(1));
    // registering the same definition is idempotent, a new one creates version 2
    assert_eq!(s(req(&t, &sr("registerSchema"), v1, &[]).body()), id1);
    let v2 = r#"{"type":"record","name":"Payment","fields":[{"name":"id","type":"string"},{"name":"amount","type":"double"}]}"#;
    let ex = req(&t, &sr("registerSchema"), v2, &[]);
    assert_eq!(ex.header("CamelAzureSchemaRegistrySchemaVersion").unwrap().as_i64(), Some(2));
    assert_eq!(s(req(&t, &sr("getSchemaProperties"), v1, &[]).body()), id1);
    let got = req(&t, &sr("getSchema"), "", &[("CamelAzureSchemaRegistrySchemaId", id1.clone().into())]);
    assert_eq!(s(got.body()), v1);
    assert_eq!(s(got.header("CamelAzureSchemaRegistryFormat").unwrap()), "Avro");
    assert_eq!(s(got.header("CamelAzureSchemaRegistrySchemaName").unwrap()), "Payment");
    assert_eq!(req(&t, &sr("listVersions"), "", &[]).body(), &Value::Array(vec![Value::Int(1), Value::Int(2)]));
    assert_eq!(s(req(&t, &sr("getSchemaByVersion"), "", &[("CamelAzureSchemaRegistrySchemaVersion", Value::Int(2))]).body()), v2);
    assert_eq!(req(&t, &sr("listGroups"), "", &[]).body(), &Value::from(vec!["payments"]));
    // a JSON schema in an Avro group is refused, unknown ids are 404
    let e = req_err(&t, &format!("{}&format=Json", sr("registerSchema")), "{}", &[]);
    assert!(e.message.contains("InvalidSchemaType"), "{}", e);
    let e = req_err(&t, &sr("getSchema"), "", &[("CamelAzureSchemaRegistrySchemaId", "nope".into())]);
    assert!(e.message.contains("404"), "{}", e);
    // a wrong client secret fails the token request
    let bad = format!("azure-schema-registry://myns/payments?schemaName=Payment&tenantId={}&clientId={}&clientSecret=wrong&authorityHost={}&uriEndpointOverride={}", TENANT_ID, CLIENT_ID, az.url(), az.url());
    let e = req_err(&t, &format!("{}&operation=listGroups", bad), "", &[]);
    assert!(e.is_a("ClientAuthenticationException"), "{:?}", e);
}

#[test]
fn azure_openai_chat_and_embeddings() {
    let az = AzureMock::start().unwrap();
    az.add_openai_deployment("gpt-4o");
    az.add_openai_deployment("text-embedding-3-small");
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = az.openai_options();
    let ex = req(&t, &format!("azure-openai://res/gpt-4o?systemMessage=Be%20terse&temperature=0.2&maxTokens=50&{}", o), "Hello Camel", &[]);
    assert_eq!(s(ex.body()), "[Be terse] Echo: Hello Camel");
    assert_eq!(s(ex.header("CamelAzureOpenAIFinishReason").unwrap()), "stop");
    assert_eq!(ex.header("CamelAzureOpenAIUsage").unwrap().get("prompt_tokens").unwrap().as_i64(), Some(2));
    let sent = az.openai_requests().pop().unwrap();
    assert_eq!(sent.get("max_tokens").unwrap().as_i64(), Some(50));
    assert_eq!(sent.get("temperature").unwrap().as_f64(), Some(0.2));
    // tool calls surface in a header
    let req_map = Value::object([
        ("messages", Value::Array(vec![Value::object([("role", "user"), ("content", "weather?")])])),
        ("tools", Value::Array(vec![Value::object([("type", Value::from("function")), ("function", Value::object([("name", "get_weather")]))])])),
    ]);
    let ex = req(&t, &format!("azure-openai://res/gpt-4o?{}", o), req_map, &[]);
    assert_eq!(s(ex.header("CamelAzureOpenAIFinishReason").unwrap()), "tool_calls");
    assert_eq!(s(ex.header("CamelAzureOpenAIToolCalls").unwrap().as_array().unwrap()[0].get("function").unwrap().get("name").unwrap()), "get_weather");
    // embeddings: one text → one vector, a list → vectors in input order
    let ex = req(&t, &format!("azure-openai://res/text-embedding-3-small?operation=embeddings&{}", o), "abc", &[]);
    assert_eq!(ex.body().as_array().unwrap().len(), 4);
    let ex = req(&t, &format!("azure-openai://res/text-embedding-3-small?operation=embeddings&{}", o), Value::from(vec!["a", "bbbbb"]), &[]);
    let vs = ex.body().as_array().unwrap().clone();
    assert_eq!(vs.len(), 2);
    assert_eq!(vs[0].as_array().unwrap()[0].as_f64(), Some(0.1));
    // Entra ID instead of the API key
    let aad = format!("azure-openai://res/gpt-4o?tenantId={}&clientId={}&clientSecret=RAW({})&authorityHost={}&uriEndpointOverride={}", TENANT_ID, CLIENT_ID, CLIENT_SECRET, az.url(), az.url());
    assert_eq!(s(req(&t, &aad, "hi", &[]).body()), "Echo: hi");
    let e = req_err(&t, &format!("azure-openai://res/gpt-4o?apiKey=wrong&uriEndpointOverride={}", az.url()), "hi", &[]);
    assert!(e.message.contains("Status code 401"), "{}", e);
    let e = req_err(&t, &format!("azure-openai://res/missing?{}", o), "hi", &[]);
    assert!(e.message.contains("DeploymentNotFound"), "{}", e);
    assert!(az.auth_failures() >= 1);
}
