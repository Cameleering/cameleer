//! The `alibaba-sls` component end to end against the embedded Simple Log Service emulator,
//! which recomputes every signature (V1 and V4), checks Content-MD5, decompresses and decodes
//! the protobuf LogGroup, and serves the queries.

use cameleer_cloud::testing::alibaba::{StoredLog, ACCESS_KEY_ID};
use cameleer_cloud::testing::SlsMock;
use cameleer_core::prelude::*;
use cameleer_core::value::Value;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

/// The message of the root cause of a failed request.
fn fail(t: &ProducerTemplate, uri: &str) -> String {
    let e = t.request(uri, |ex| ex.set_body("x")).unwrap_err();
    format!("{} / {}", e, e.root_cause())
}

fn mock() -> SlsMock {
    let m = SlsMock::start().unwrap();
    m.create_project("orders", &["app-logs", "audit"]);
    m
}

fn uri(m: &SlsMock, op: &str, extra: &str) -> String {
    format!("alibaba-sls:{}?project=orders&{}{}{}", op, m.endpoint_options(), if extra.is_empty() { "" } else { "&" }, extra)
}

#[test]
fn put_logs_in_every_compression_and_both_signatures() {
    let m = mock();
    let ctx = context();
    let t = ctx.create_producer_template();
    for (i, extra) in ["", "compressType=deflate", "compressType=none", "signatureVersion=v4", "signatureVersion=v4&compressType=deflate"].iter().enumerate() {
        let ex = t
            .request(&uri(&m, "putLogs", &format!("logStoreName=app-logs&{}", extra)), |ex| {
                ex.set_body(format!(r#"{{"topic":"camel","source":"10.0.0.7","logTags":{{"env":"test"}},"logs":[{{"time":1700000000,"contents":{{"message":"order {} placed","level":"INFO"}}}}]}}"#, i));
            })
            .unwrap();
        assert_eq!(ex.header("CamelAlibabaSlsStatusCode"), Some(&Value::Int(200)));
        assert!(ex.header("CamelAlibabaSlsRequestId").is_some());
        assert_eq!(ex.body().get("statusCode"), Some(&Value::Int(200)));
        assert!(ex.body().get("headers").and_then(|h| h.get("x-log-requestid")).is_some());
    }
    let logs = m.logs("orders", "app-logs");
    assert_eq!(logs.len(), 5);
    assert_eq!(logs[0], StoredLog { time: 1_700_000_000, topic: "camel".into(), source: "10.0.0.7".into(), tags: vec![("env".into(), "test".into())], contents: vec![("message".into(), "order 0 placed".into()), ("level".into(), "INFO".into())] });
    let seen: Vec<(String, String)> = m.requests().into_iter().map(|(_, _, v, c)| (v, c)).collect();
    assert_eq!(seen, vec![("v1".into(), "lz4".into()), ("v1".into(), "deflate".into()), ("v1".into(), "".into()), ("v4".into(), "lz4".into()), ("v4".into(), "deflate".into())]);
}

#[test]
fn put_logs_accepts_text_lists_and_maps() {
    let m = mock();
    let ctx = context();
    let t = ctx.create_producer_template();
    let u = uri(&m, "putLogs", "logStoreName=app-logs");
    t.send_body(&u, "a plain line").unwrap();
    let mut map = cameleer_core::value::ObjectMap::new();
    map.insert("event".into(), Value::from("login"));
    map.insert("user".into(), Value::from("ann"));
    t.send_body(&u, Value::Object(map)).unwrap();
    t.send_body(&u, r#"[{"time":1700000001,"n":"1"},{"time":1700000002,"n":"2"}]"#).unwrap();
    let logs = m.logs("orders", "app-logs");
    assert_eq!(logs.len(), 4);
    assert_eq!(logs[0].contents, vec![("message".into(), "a plain line".into())]);
    assert!((logs[0].time as i64 - cameleer_cloud::util::now_secs()).abs() < 60);
    assert_eq!(logs[1].contents, vec![("event".into(), "login".into()), ("user".into(), "ann".into())]);
    assert_eq!((logs[2].time, logs[3].time), (1_700_000_001, 1_700_000_002));
}

fn seed(m: &SlsMock) {
    let log = |time: u32, level: &str, msg: &str, topic: &str| StoredLog { time, topic: topic.into(), source: "host-1".into(), tags: vec![], contents: vec![("level".into(), level.into()), ("message".into(), msg.into())] };
    m.add_logs(
        "orders",
        "app-logs",
        vec![log(1_700_000_010, "INFO", "order 1 placed", "web"), log(1_700_000_020, "ERROR", "payment declined", "web"), log(1_700_000_030, "ERROR", "stock missing", "batch"), log(1_700_009_999, "ERROR", "too late", "web")],
    );
}

#[test]
fn get_logs_with_query_paging_and_headers() {
    let m = mock();
    seed(&m);
    let ctx = context();
    let t = ctx.create_producer_template();
    let u = uri(&m, "getLogs", "logStoreName=app-logs&from=1700000000&to=1700003600");
    let ex = t.request(&u, |ex| ex.set_header("CamelAlibabaSlsQuery", "level: ERROR")).unwrap();
    let body = ex.body().get("body").and_then(|b| b.get("data")).cloned().unwrap();
    let Value::Array(logs) = body else { panic!("{:?}", ex.body()) };
    assert_eq!(logs.len(), 2);
    assert_eq!(logs[0].get("message"), Some(&Value::from("payment declined")));
    assert_eq!(logs[0].get("__time__"), Some(&Value::from("1700000020")));
    assert_eq!(ex.body().get("body").and_then(|b| b.get("meta")).and_then(|h| h.get("progress")), Some(&Value::from("Complete")));
    assert_eq!(ex.header("CamelAlibabaSlsStatusCode"), Some(&Value::Int(200)));

    // headers override the options: topic, line, offset, reverse, and the time range
    let ex = t
        .request(&uri(&m, "getLogs", "logStoreName=app-logs&query=*&from=0&to=1"), |ex| {
            ex.set_header("CamelAlibabaSlsFrom", 1_700_000_000i64);
            ex.set_header("CamelAlibabaSlsTo", 1_700_003_600i64);
            ex.set_header("CamelAlibabaSlsReverse", true);
            ex.set_header("CamelAlibabaSlsLine", 1i64);
            ex.set_header("CamelAlibabaSlsOffset", 1i64);
            ex.set_header("CamelAlibabaSlsTopic", "web");
        })
        .unwrap();
    let Some(Value::Array(logs)) = ex.body().get("body").and_then(|b| b.get("data")).cloned() else { panic!() };
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].get("message"), Some(&Value::from("order 1 placed")));

    // V4 and a free-text search
    let ex = t.request(&uri(&m, "getLogs", "logStoreName=app-logs&from=1700000000&to=1700003600&signatureVersion=v4&query=stock"), |_| {}).unwrap();
    let Some(Value::Array(logs)) = ex.body().get("body").and_then(|b| b.get("data")).cloned() else { panic!("{:?}", ex.body()) };
    assert_eq!(logs.len(), 1);

    // from/to are required
    assert!(fail(&t, &uri(&m, "getLogs", "logStoreName=app-logs")).contains("start time"));
}

#[test]
fn list_log_stores_with_filters() {
    let m = mock();
    m.create_log_store("orders", "metrics-store", "standard", "Metrics");
    m.create_log_store("orders", "cheap-logs", "query", "None");
    let ctx = context();
    let t = ctx.create_producer_template();
    let ex = t.request(&uri(&m, "listLogStores", ""), |_| {}).unwrap();
    assert_eq!(ex.body().get("total"), Some(&Value::Int(4)));
    assert_eq!(ex.body().get("logstores"), Some(&Value::Array(vec!["app-logs".into(), "audit".into(), "cheap-logs".into(), "metrics-store".into()])));
    let ex = t
        .request(&uri(&m, "listLogStores", ""), |ex| {
            ex.set_header("CamelAlibabaSlsListLogStoreName", "logs");
            ex.set_header("CamelAlibabaSlsMode", "standard");
        })
        .unwrap();
    assert_eq!(ex.body().get("logstores"), Some(&Value::Array(vec!["app-logs".into()])));
    let ex = t.request(&uri(&m, "listLogStores", ""), |ex| ex.set_header("CamelAlibabaSlsTelemetryType", "Metrics")).unwrap();
    assert_eq!(ex.body().get("logstores"), Some(&Value::Array(vec!["metrics-store".into()])));
    let ex = t
        .request(&uri(&m, "listLogStores", ""), |ex| {
            ex.set_header("CamelAlibabaSlsSize", 2i64);
            ex.set_header("CamelAlibabaSlsListOffset", 1i64);
        })
        .unwrap();
    assert_eq!(ex.body().get("count"), Some(&Value::Int(2)));
    assert_eq!(ex.body().get("logstores"), Some(&Value::Array(vec!["audit".into(), "cheap-logs".into()])));
}

#[test]
fn operation_project_and_logstore_from_headers_and_properties() {
    let m = mock();
    m.create_project("billing", &["invoices"]);
    let ctx = context();
    let t = ctx.create_producer_template();
    let u = format!("alibaba-sls:listLogStores?{}", m.endpoint_options());
    t.request(&u, |ex| {
            ex.set_header("CamelAlibabaSlsOperation", "putLogs");
            ex.set_header("CamelAlibabaSlsProject", "billing");
            ex.set_header("CamelAlibabaSlsLogStoreName", "invoices");
            ex.set_body("invoice 7 sent");
        })
        .unwrap();
    assert_eq!(m.logs("billing", "invoices").len(), 1);
    let ex = t
        .request(&u, |ex| {
            ex.set_property("CamelAlibabaSlsOperation", "listLogStores");
            ex.set_property("CamelAlibabaSlsProject", "billing");
        })
        .unwrap();
    assert_eq!(ex.body().get("logstores"), Some(&Value::Array(vec!["invoices".into()])));
}

#[test]
fn service_errors_and_bad_credentials() {
    let m = mock();
    let ctx = context();
    let t = ctx.create_producer_template();
    let e = t.request(&uri(&m, "putLogs", "logStoreName=missing"), |ex| ex.set_body("x")).unwrap_err();
    let e = e.root_cause();
    assert_eq!(e.kind, "TeaException");
    assert!(e.message.contains("LogStoreNotExist") && e.message.contains("status: 404"), "{}", e.message);
    assert!(e.is_a(cameleer_core::error::kinds::RUNTIME));

    let wrong = format!("alibaba-sls:listLogStores?project=orders&endpoint=cn-hangzhou.log.aliyuncs.com&accessKey={}&secretKey=wrong&uriEndpointOverride={}&overrideEndpoint=true", ACCESS_KEY_ID, m.url());
    let e = fail(&t, &wrong);
    assert!(e.contains("SignatureNotMatch"), "{}", e);
    let wrong = format!("alibaba-sls:listLogStores?project=nosuch&{}", m.endpoint_options());
    assert!(fail(&t, &wrong).contains("ProjectNotExist"));
}

#[test]
fn sts_token_is_sent_and_checked() {
    let m = mock();
    m.add_sts_token("STS.tmp-id", "tmp-secret", "token-123");
    let ctx = context();
    let t = ctx.create_producer_template();
    let base = format!("alibaba-sls:listLogStores?project=orders&endpoint=cn-hangzhou.log.aliyuncs.com&accessKey=STS.tmp-id&secretKey=tmp-secret&uriEndpointOverride={}&overrideEndpoint=true", m.url());
    for sig in ["v1", "v4"] {
        t.request(&format!("{}&securityToken=token-123&signatureVersion={}", base, sig), |_| {}).unwrap();
    }
    let e = fail(&t, &format!("{}&securityToken=other", base));
    assert!(e.contains("SecurityTokenExpired"), "{}", e);
    let e = fail(&t, &base);
    assert!(e.contains("security token"), "{}", e);
}

#[test]
fn over_https_with_a_trusted_certificate() {
    let m = SlsMock::start_tls().unwrap();
    m.create_project("orders", &["app-logs"]);
    // the self-signed certificate is not trusted by default, and verification cannot be turned off
    let ctx = context();
    let t = ctx.create_producer_template();
    let e = t.request(&uri(&m, "putLogs", "logStoreName=app-logs"), |ex| ex.set_body("x")).unwrap_err();
    assert!(e.root_cause().is_a("SSLHandshakeException") || format!("{}", e.root_cause()).contains("certificate"), "{}", e.root_cause());
    let e = fail(&t, &uri(&m, "putLogs", "logStoreName=app-logs&trustAllCertificates=true"));
    assert!(e.contains("always verifies"), "{}", e);
    // trusting the server's certificate
    let ctx = context();
    ctx.properties().set("camel.ssl.enabled", "true");
    ctx.properties().set("camel.ssl.trustStore", m.cert_pem().unwrap());
    let t = ctx.create_producer_template();
    t.request(&uri(&m, "putLogs", "logStoreName=app-logs"), |ex| ex.set_body("over tls")).unwrap();
    assert_eq!(m.logs("orders", "app-logs").len(), 1);
}

#[test]
fn endpoint_configuration_errors() {
    let ctx = context();
    let t = ctx.create_producer_template();
    let err = |u: &str| fail(&t, u);
    assert!(err("alibaba-sls:putLogs?project=p&accessKey=a&secretKey=b").contains("region"));
    assert!(err("alibaba-sls:deleteLogs?region=cn-hangzhou&accessKey=a&secretKey=b").contains("Unsupported"));
    assert!(err("alibaba-sls:putLogs?region=cn-hangzhou&project=p&serviceKeys=#keys&accessKey=a&secretKey=b").contains("serviceKeys"));
    assert!(err("alibaba-sls:putLogs?endpoint=sls.example.com&project=p&accessKey=a&secretKey=b&signatureVersion=v4").contains("region"));
    assert!(err("alibaba-sls:putLogs?region=cn-hangzhou&project=p&accessKey=a&secretKey=b&compressType=zstd").contains("compressType"));
    // no AccessKey: the endpoint resolves (as Camel's lazily created client) and the request fails
    if std::env::var("ALIBABA_CLOUD_ACCESS_KEY_ID").is_err() {
        assert!(err("alibaba-sls:listLogStores?region=cn-hangzhou&project=p").contains("accessKey and secretKey are required"));
    }
    assert_eq!(cameleer_cloud::alibaba::sls::region_of_endpoint("cn-shanghai-intranet.log.aliyuncs.com").as_deref(), Some("cn-shanghai"));
}
