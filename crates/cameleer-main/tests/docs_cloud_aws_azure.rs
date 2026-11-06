//! The examples of the AWS and Azure component pages (`docs/components/aws*.md`,
//! `docs/components/azure*.md`) and of `docs/guides/cloud-credentials.md`.
//!
//! Every YAML block is taken from the markdown (by its `<!-- example: id -->` marker), its
//! `{{env:...}}` placeholders are substituted, and every endpoint uri it uses is resolved with
//! `ctx.get_endpoint` (unknown options fail, as in Apache Camel). The examples then run end to
//! end against the embedded emulators of `cameleer_cloud::testing` (`AwsMock`, `AzuriteMock`,
//! `AzureMock`) and `cameleer_amqp10::testing::Amqp10Broker`. The emulators are reached through
//! component options (`camel.component.<scheme>.uriEndpointOverride`, `accessKey`, ...), so the
//! routes themselves are unchanged; only `log:<name>` sinks are swapped for `mock:<name>` where
//! a test asserts on what reached them.
//!
//! The Rust DSL snippets of the pages are copied verbatim into the `rust_dsl_*` functions at
//! the end, which a test builds on a context with every component.

// needs every component family (the default features)
#![cfg(feature = "full")]

use std::time::{Duration, Instant};

use cameleer_amqp10::testing::Amqp10Broker;
use cameleer_cloud::testing::azure as azurite;
use cameleer_cloud::testing::azure_services;
use cameleer_cloud::testing::{AwsMock, AzureMock, AzuriteMock};
use cameleer_core::prelude::*;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn docs_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs")
}

/// The fenced block that follows `<!-- example: id -->` in a page (`components/x.md`).
fn block(page: &str, id: &str) -> String {
    let path = docs_dir().join(page);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
    let marker = format!("<!-- example: {} -->", id);
    let start = text.find(&marker).unwrap_or_else(|| panic!("{}: no marker {}", page, marker));
    let rest = &text[start + marker.len()..];
    let open = rest.find("```").unwrap_or_else(|| panic!("{}: no block after {}", page, marker));
    let body = &rest[open..];
    let body = &body[body.find('\n').unwrap() + 1..];
    let end = body.find("\n```").unwrap_or_else(|| panic!("{}: unterminated block {}", page, id));
    body[..end + 1].to_string()
}

/// Replace `{{env:NAME:default}}` / `{{env:NAME}}`: by the value given for NAME, else the default.
fn env_subst(text: &str, vals: &[(&str, &str)]) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find("{{env:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let end = after.find("}}").expect("unterminated placeholder");
        let inner = &after[..end];
        let (name, default) = match inner.split_once(':') {
            Some((n, d)) => (n, d),
            None => (inner, ""),
        };
        match vals.iter().find(|(n, _)| *n == name) {
            Some((_, v)) => out.push_str(v),
            None => out.push_str(default),
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    out
}

/// Endpoint uris of a YAML route (`uri:`, `to:`, `toD:` values).
fn endpoint_uris(yaml: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in yaml.lines() {
        let t = line.trim().trim_start_matches("- ");
        for key in ["uri:", "to:", "toD:"] {
            if let Some(v) = t.strip_prefix(key) {
                let v = v.trim().trim_matches('"').trim_matches('\'').to_string();
                if !v.is_empty() {
                    out.push(v);
                }
            }
        }
    }
    out
}

/// The schemes of the cloud endpoints of a route.
fn cloud_schemes(yaml: &str, prefix: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for uri in endpoint_uris(yaml) {
        let scheme = uri.split(':').next().unwrap().to_string();
        if scheme.starts_with(prefix) && !out.contains(&scheme) {
            out.push(scheme);
        }
    }
    out
}

/// Resolve every endpoint of a route on the context (validates the option names).
fn resolve_all(ctx: &CamelContext, id: &str, yaml: &str) {
    for uri in endpoint_uris(yaml) {
        if let Err(e) = ctx.get_endpoint(&uri) {
            panic!("example {}: {} does not resolve: {}", id, uri, e);
        }
    }
}

/// Point every AWS endpoint of the route at the emulator (component options, so the uris
/// stay as written in the page).
fn aws_options(ctx: &CamelContext, aws: &AwsMock, yaml: &str) {
    for scheme in cloud_schemes(yaml, "aws") {
        ctx.set_component_option(&scheme, "accessKey", AwsMock::ACCESS_KEY);
        ctx.set_component_option(&scheme, "secretKey", AwsMock::SECRET_KEY);
        ctx.set_component_option(&scheme, "uriEndpointOverride", aws.url());
        ctx.set_component_option(&scheme, "overrideEndpoint", "true");
    }
}

/// A Main with the example loaded and resolved (not started).
fn main_for(id: &str, yaml: &str, configure: impl Fn(&CamelContext)) -> cameleer_main::Main {
    let mut main = cameleer_main::new_main();
    configure(main.context());
    resolve_all(main.context(), id, yaml);
    main.add_route_source(format!("{}.camel.yaml", id), yaml.to_string());
    main
}

/// An AWS example: placeholders substituted, `log:` sinks swapped, endpoints resolved against
/// the emulator, started.
fn run_aws(aws: &AwsMock, page: &str, id: &str, env: &[(&str, &str)], swap: &[(&str, &str)]) -> cameleer_main::Main {
    run_with(page, id, env, swap, |ctx, yaml| aws_options(ctx, aws, yaml))
}

/// An example with placeholders substituted, `log:` sinks swapped, `configure` applied to the
/// context, every endpoint resolved, then started.
fn run_with(page: &str, id: &str, env: &[(&str, &str)], swap: &[(&str, &str)], configure: impl Fn(&CamelContext, &str)) -> cameleer_main::Main {
    let mut yaml = env_subst(&block(page, id), env);
    for (from, to) in swap {
        assert!(yaml.contains(from), "{} does not contain {}", id, from);
        yaml = yaml.replace(from, to);
    }
    let mut main = main_for(id, &yaml, |ctx| configure(ctx, &yaml));
    main.start().unwrap_or_else(|e| panic!("example {} does not start: {}", id, e));
    main
}

/// Point the Azure Storage endpoints at the Azurite emulator (path-style addressing).
fn azurite_options(ctx: &CamelContext, az: &AzuriteMock) {
    for (scheme, url) in [("azure-storage-blob", az.blob_url()), ("azure-storage-queue", az.queue_url()), ("azure-storage-datalake", az.dfs_url()), ("azure-storage-files", az.file_url())] {
        ctx.set_component_option(scheme, "uriEndpointOverride", url);
        ctx.set_component_option(scheme, "overrideEndpoint", "true");
    }
}

const STORAGE_ENV: [(&str, &str); 2] = [("AZURE_STORAGE_ACCOUNT", azurite::ACCOUNT), ("AZURE_STORAGE_KEY", azurite::ACCOUNT_KEY)];
const ENTRA_ENV: [(&str, &str); 3] = [("AZURE_TENANT_ID", azure_services::TENANT_ID), ("AZURE_CLIENT_ID", azure_services::CLIENT_ID), ("AZURE_CLIENT_SECRET", azure_services::CLIENT_SECRET)];

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn mock(main: &cameleer_main::Main, name: &str) -> MockEndpoint {
    main.context().mock_endpoint(&format!("mock:{}", name)).unwrap()
}

/// Wait until the mock received `n` exchanges; their bodies as strings.
fn mock_bodies(main: &cameleer_main::Main, name: &str, n: usize) -> Vec<String> {
    let m = mock(main, name);
    wait_for(&format!("{} message(s) on mock:{}", n, name), || m.received_exchanges().len() >= n);
    m.received_exchanges().iter().map(|e| e.body().to_string()).collect()
}

fn template(main: &cameleer_main::Main) -> ProducerTemplate {
    main.context().create_producer_template()
}

fn page(scheme: &str) -> String {
    format!("components/{}.md", scheme)
}

// ---------------------------------------------------------------------------
// AWS
// ---------------------------------------------------------------------------

#[test]
fn aws2_sqs_send_and_consume() {
    let aws = AwsMock::start().unwrap();
    let main = run_aws(&aws, &page("aws2-sqs"), "aws2-sqs-send-order", &[], &[]);
    wait_for("the order on the queue", || aws.queue_messages("orders").len() == 1);
    assert!(aws.queue_messages("orders")[0].contains("o-1001"));
    assert_eq!(aws.queue_message_attributes("orders")[0].get("orderType").map(|s| s.as_str()), Some("express"));
    main.stop().unwrap();

    let main = run_aws(&aws, &page("aws2-sqs"), "aws2-sqs-consume-orders", &[], &[("log:orders", "mock:orders")]);
    let bodies = mock_bodies(&main, "orders", 1);
    assert!(bodies[0].contains("camel saddle"), "{:?}", bodies);
    assert_eq!(mock(&main, "orders").exchange(0).unwrap().header("orderType").map(|v| v.to_string()).as_deref(), Some("express"));
    wait_for("the message to be deleted", || aws.queue_messages("orders").is_empty());
    main.stop().unwrap();
}

#[test]
fn aws2_s3_upload_and_consume() {
    let aws = AwsMock::start().unwrap();
    let main = run_aws(&aws, &page("aws2-s3"), "aws2-s3-upload-report", &[], &[]);
    wait_for("the report object", || aws.object("my-reports", "reports/daily.csv").is_some());
    assert_eq!(aws.object("my-reports", "reports/daily.csv").unwrap(), b"region,total;eu,42;us,17");
    assert_eq!(aws.object_info("my-reports", "reports/daily.csv").unwrap().0, "text/csv");
    main.stop().unwrap();

    aws.create_bucket("incoming-invoices");
    aws.create_bucket("processed-invoices");
    aws.put_object("incoming-invoices", "invoices/inv-1.pdf", b"%PDF-invoice");
    aws.put_object("incoming-invoices", "other/skip.txt", b"not an invoice");
    let main = run_aws(&aws, &page("aws2-s3"), "aws2-s3-consume-invoices", &[], &[("log:invoices", "mock:invoices")]);
    assert_eq!(mock_bodies(&main, "invoices", 1), vec!["%PDF-invoice".to_string()]);
    wait_for("the move", || aws.object("processed-invoices", "invoices/inv-1.pdf").is_some() && aws.object("incoming-invoices", "invoices/inv-1.pdf").is_none());
    assert!(aws.object("incoming-invoices", "other/skip.txt").is_some(), "only the prefix is consumed");
    main.stop().unwrap();
}

#[test]
fn aws2_sns_publish_alerts() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("alerts");
    aws.send_sqs("alerts", "disk almost full on edge-7");
    let main = run_aws(&aws, &page("aws2-sns"), "aws2-sns-publish-alerts", &[], &[]);
    wait_for("the published alert", || aws.published("ops-alerts").len() == 1);
    let m = &aws.published("ops-alerts")[0];
    assert_eq!(m.message, "disk almost full on edge-7");
    assert_eq!(m.subject.as_deref(), Some("Ops alert"));
    main.stop().unwrap();
}

#[test]
fn aws2_lambda_invoke_function() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("images-to-resize");
    aws.register_function("resize-image", |payload| {
        let input = String::from_utf8_lossy(payload).to_string();
        Ok(format!("{{\"resized\":{},\"width\":256}}", input).into_bytes())
    });
    aws.send_sqs("images-to-resize", r#"{"key":"cat.jpg"}"#);
    let main = run_aws(&aws, &page("aws2-lambda"), "aws2-lambda-invoke-function", &[], &[]);
    wait_for("the function result", || aws.queue_messages("resized-images").len() == 1);
    let out = &aws.queue_messages("resized-images")[0];
    assert!(out.contains("\"resized\"") && out.contains("cat.jpg") && out.contains("256"), "{}", out);
    main.stop().unwrap();
}

#[test]
fn aws2_eventbridge_put_order_event() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("orders");
    aws.send_sqs("orders", r#"{"orderId":"o-7","total":99}"#);
    let main = run_aws(&aws, &page("aws2-eventbridge"), "aws2-eventbridge-put-order-event", &[], &[]);
    wait_for("the event", || aws.events().len() == 1);
    let e = aws.events()[0].to_json();
    assert!(e.contains("com.example.shop") && e.contains("OrderPlaced") && e.contains("o-7"), "{}", e);
    main.stop().unwrap();
}

#[test]
fn aws2_kinesis_put_and_consume() {
    let aws = AwsMock::start().unwrap();
    aws.create_stream("clickstream", 1);
    let main = run_aws(&aws, &page("aws2-kinesis"), "aws2-kinesis-put-click", &[], &[]);
    wait_for("the record", || aws.kinesis_records("clickstream").len() == 1);
    assert_eq!(aws.kinesis_records("clickstream")[0].0, "user-42");
    main.stop().unwrap();

    let main = run_aws(&aws, &page("aws2-kinesis"), "aws2-kinesis-consume-clickstream", &[], &[("log:clicks", "mock:clicks")]);
    let bodies = mock_bodies(&main, "clicks", 1);
    assert_eq!(bodies[0], r#"{"user":"user-42","page":"/pricing"}"#);
    main.stop().unwrap();
}

#[test]
fn aws2_kinesis_firehose_ship_logs() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("app-logs");
    aws.create_delivery_stream("app-logs-to-s3");
    aws.send_sqs("app-logs", "GET /health 200");
    aws.send_sqs("app-logs", "GET /orders 500");
    let main = run_aws(&aws, &page("aws2-kinesis-firehose"), "aws2-kinesis-firehose-ship-logs", &[], &[]);
    wait_for("two firehose records", || aws.firehose_records("app-logs-to-s3").len() == 2);
    assert_eq!(aws.firehose_records("app-logs-to-s3")[1], b"GET /orders 500");
    main.stop().unwrap();
}

#[test]
fn aws2_ddb_save_order() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("orders");
    aws.create_table("orders", "orderId", None);
    aws.send_sqs("orders", r#"{"orderId":"o-1001","total":42,"express":true}"#);
    let main = run_aws(&aws, &page("aws2-ddb"), "aws2-ddb-save-order", &[], &[]);
    wait_for("the item", || aws.table_items("orders").len() == 1);
    let item = &aws.table_items("orders")[0];
    assert_eq!(item.get("orderId").map(|v| v.to_string()).as_deref(), Some("o-1001"));
    assert_eq!(item.get("total").and_then(|v| v.as_i64()), Some(42));
    main.stop().unwrap();
}

#[test]
fn aws2_ddbstream_new_orders() {
    let aws = AwsMock::start().unwrap();
    aws.create_table("orders", "orderId", None);
    aws.enable_ddb_stream("orders", "NEW_AND_OLD_IMAGES");
    let main = run_aws(&aws, &page("aws2-ddbstream"), "aws2-ddbstream-new-orders", &[], &[]);
    let mut item = cameleer_core::value::ObjectMap::new();
    item.insert("orderId".into(), Value::from("o-55"));
    item.insert("total".into(), Value::from(12));
    template(&main).request_body_and_header(&format!("aws2-ddb://orders?operation=PutItem&{}", aws.endpoint_options()), "", "CamelAwsDdbItem", Value::Object(item)).unwrap();
    wait_for("the change record", || aws.queue_messages("new-orders").len() == 1);
    let rec = &aws.queue_messages("new-orders")[0];
    assert!(rec.contains("INSERT") && rec.contains("o-55"), "{}", rec);
    main.stop().unwrap();
}

#[test]
fn aws_bedrock_summarize_tickets() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("support-tickets");
    aws.send_sqs("support-tickets", "The checkout page times out for all EU customers");
    let main = run_aws(&aws, &page("aws-bedrock"), "aws-bedrock-summarize-tickets", &[], &[]);
    wait_for("the summary", || aws.published("ticket-summaries").len() == 1);
    // the emulator echoes the user message, prefixed with the system prompt
    assert_eq!(aws.published("ticket-summaries")[0].message, "[Summarize the support ticket in one sentence.] Echo: The checkout page times out for all EU customers");
    assert_eq!(aws.bedrock_requests()[0].0, "anthropic.claude-3-haiku-20240307-v1:0");
    main.stop().unwrap();
}

#[test]
fn aws_bedrock_agent_runtime_rag_answers() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("questions");
    aws.add_knowledge_base("KB12345678", &["Returns are accepted within 30 days of delivery.", "Shipping to Norway takes 5 days."]);
    aws.send_sqs("questions", "How many days do I have for returns?");
    let main = run_aws(&aws, &page("aws-bedrock-agent-runtime"), "aws-bedrock-agent-runtime-rag-answers", &[], &[("log:answers", "mock:answers")]);
    let bodies = mock_bodies(&main, "answers", 1);
    assert!(bodies[0].contains("Returns are accepted within 30 days"), "{:?}", bodies);
    assert!(mock(&main, "answers").exchange(0).unwrap().header("CamelAwsBedrockAgentRuntimeSessionId").is_some());
    main.stop().unwrap();
}

#[test]
fn aws2_comprehend_sentiment_routing() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("reviews");
    aws.send_sqs("reviews", "Great saddle, I love it");
    aws.send_sqs("reviews", "Terrible quality, I hate it");
    let main = run_aws(&aws, &page("aws2-comprehend"), "aws2-comprehend-sentiment-routing", &[], &[]);
    wait_for("the reviews to be processed", || aws.queue_messages("reviews").is_empty() && aws.queue_exists("escalations"));
    wait_for("the escalation", || aws.queue_messages("escalations").len() == 1);
    assert_eq!(aws.queue_messages("escalations"), vec!["Terrible quality, I hate it".to_string()]);
    main.stop().unwrap();
}

#[test]
fn aws2_textract_invoices() {
    let aws = AwsMock::start().unwrap();
    aws.create_bucket("scanned-invoices");
    aws.put_object("scanned-invoices", "inv-1.txt", b"Vendor: ACME Corp\nTotal: 42.00");
    let main = run_aws(&aws, &page("aws2-textract"), "aws2-textract-invoices", &[], &[]);
    wait_for("the extracted fields", || aws.queue_messages("extracted-invoices").len() == 1);
    let json = &aws.queue_messages("extracted-invoices")[0];
    assert!(json.contains("ExpenseDocuments") && json.contains("ACME Corp") && json.contains("42.00"), "{}", json);
    main.stop().unwrap();
}

#[test]
fn aws2_translate_replies() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("replies-en");
    aws.add_translation("Your order has shipped", "de", "Ihre Bestellung wurde versandt");
    aws.send_sqs("replies-en", "Your order has shipped");
    let main = run_aws(&aws, &page("aws2-translate"), "aws2-translate-replies", &[], &[]);
    wait_for("the translation", || aws.queue_messages("replies-de").len() == 1);
    assert_eq!(aws.queue_messages("replies-de")[0], "Ihre Bestellung wurde versandt");
    main.stop().unwrap();
}

#[test]
fn aws2_polly_announcements() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("announcements");
    aws.send_sqs("announcements", "The train to Bergen leaves from platform 3");
    let main = run_aws(&aws, &page("aws2-polly"), "aws2-polly-announcements", &[], &[]);
    wait_for("the audio file", || aws.object_keys("announcement-audio").len() == 1);
    let key = aws.object_keys("announcement-audio")[0].clone();
    assert!(key.starts_with("audio/") && key.ends_with(".mp3"), "{}", key);
    let audio = aws.object("announcement-audio", &key).unwrap();
    assert_eq!(String::from_utf8_lossy(&audio), "MOCKAUDIO:Joanna:mp3:The train to Bergen leaves from platform 3");
    main.stop().unwrap();
}

#[test]
fn aws2_rekognition_label_photos() {
    let aws = AwsMock::start().unwrap();
    aws.create_bucket("uploaded-photos");
    // the emulator derives labels from the words in the image bytes
    aws.put_object("uploaded-photos", "cat.jpg", b"a cat sitting on a sofa");
    let main = run_aws(&aws, &page("aws2-rekognition"), "aws2-rekognition-label-photos", &[], &[]);
    wait_for("the labels", || aws.queue_messages("photo-labels").len() == 1);
    let json = &aws.queue_messages("photo-labels")[0];
    assert!(json.contains("\"Cat\"") && json.contains("\"Sitting\"") && !json.contains("\"Sofa\""), "minConfidence=80 drops the third label: {}", json);
    assert_eq!(aws.queue_message_attributes("photo-labels")[0].get("photo").map(|s| s.as_str()), Some("cat.jpg"));
    main.stop().unwrap();
}

#[test]
fn aws2_transcribe_start_jobs() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("recordings");
    aws.send_sqs("recordings", "s3://recordings/call-17.wav");
    let main = run_aws(&aws, &page("aws2-transcribe"), "aws2-transcribe-start-jobs", &[], &[("log:jobs", "mock:jobs")]);
    mock_bodies(&main, "jobs", 1);
    let ex = mock(&main, "jobs").exchange(0).unwrap();
    assert_eq!(ex.header("CamelAwsTranscribeTranscriptionJobStatus").map(|v| v.to_string()).as_deref(), Some("IN_PROGRESS"));
    let job = ex.body().to_json();
    assert!(job.contains("call-") && job.contains("s3://recordings/call-17.wav"), "{}", job);
    main.stop().unwrap();
}

/// Call an AWS endpoint of the emulator directly (test setup).
fn aws_call(main: &cameleer_main::Main, aws: &AwsMock, uri: &str, body: impl Into<Value>, headers: Vec<(&str, Value)>) -> Exchange {
    let sep = if uri.contains('?') { '&' } else { '?' };
    let t = template(main);
    let mut ex = Exchange::new(Some(main.context().clone()));
    ex.set_body(body.into());
    for (k, v) in headers {
        ex.set_header(k, v);
    }
    let out = t.send(&format!("{}{}{}", uri, sep, aws.endpoint_options()), ex);
    if let Some(e) = &out.exception {
        panic!("{}: {}", uri, e);
    }
    out
}

#[test]
fn aws_cloudtrail_s3_audit() {
    let aws = AwsMock::start().unwrap();
    aws.add_cloudtrail_event("s3.amazonaws.com", "DeleteBucket", "alice");
    aws.add_cloudtrail_event("iam.amazonaws.com", "CreateUser", "bob");
    let main = run_aws(&aws, &page("aws-cloudtrail"), "aws-cloudtrail-s3-audit", &[], &[("log:s3-audit", "mock:s3-audit")]);
    let bodies = mock_bodies(&main, "s3-audit", 1);
    assert!(bodies[0].contains("DeleteBucket"), "{:?}", bodies);
    let ex = mock(&main, "s3-audit").exchange(0).unwrap();
    assert_eq!(ex.header("CamelAwsCloudTrailEventUsername").map(|v| v.to_string()).as_deref(), Some("alice"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(mock(&main, "s3-audit").received_counter(), 1, "only S3 events");
    main.stop().unwrap();
}

#[test]
fn aws_config_put_rule() {
    let aws = AwsMock::start().unwrap();
    let main = run_aws(&aws, &page("aws-config"), "aws-config-put-rule", &[], &[]);
    wait_for("the config rule", || aws.config_rule_names().contains(&"s3-bucket-public-read-prohibited".to_string()));
    main.stop().unwrap();
}

#[test]
fn aws_secrets_manager_create_and_placeholder() {
    let aws = AwsMock::start().unwrap();
    let main = run_aws(&aws, &page("aws-secrets-manager"), "aws-secrets-manager-create-secret", &[("PARTNER_TOKEN", "t0k3n")], &[]);
    wait_for("the secret", || aws.secret("partner-api").is_some());
    assert_eq!(aws.secret("partner-api").unwrap(), r#"{"token":"t0k3n"}"#);
    main.stop().unwrap();

    // the {{aws:...}} placeholder, configured with camel.vault.aws.* properties
    let id = "aws-secrets-manager-placeholder";
    let yaml = block(&page("aws-secrets-manager"), id).replace("log:partner-api", "mock:partner-api");
    let mut main = main_for(id, &yaml, |_| {});
    for (k, v) in [("accessKey", AwsMock::ACCESS_KEY.to_string()), ("secretKey", AwsMock::SECRET_KEY.to_string()), ("region", "us-east-1".to_string()), ("uriEndpointOverride", aws.url()), ("overrideEndpoint", "true".to_string())] {
        main.set_property(format!("camel.vault.aws.{}", k), v);
    }
    main.start().unwrap();
    mock_bodies(&main, "partner-api", 1);
    assert_eq!(mock(&main, "partner-api").exchange(0).unwrap().header("Authorization").map(|v| v.to_string()).as_deref(), Some("Bearer t0k3n"));
    main.stop().unwrap();
}

#[test]
fn aws2_athena_daily_report() {
    let aws = AwsMock::start().unwrap();
    aws.set_athena_result("FROM orders", vec![vec!["region", "orders"], vec!["eu", "42"], vec!["us", "17"]]);
    let main = run_aws(&aws, &page("aws2-athena"), "aws2-athena-daily-report", &[], &[("log:report", "mock:report")]);
    mock_bodies(&main, "report", 1);
    let rows = mock(&main, "report").exchange(0).unwrap().body().to_json();
    assert!(rows.contains("\"eu\"") && rows.contains("\"42\"") && rows.contains("\"us\""), "{}", rows);
    assert_eq!(aws.athena_query_count(), 1);
    main.stop().unwrap();
}

#[test]
fn aws2_cw_count_orders_by_channel() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("orders");
    aws.send_sqs("orders", r#"{"channel":"web"}"#);
    let main = run_aws(&aws, &page("aws2-cw"), "aws2-cw-count-orders-by-channel", &[], &[]);
    wait_for("the metric", || aws.metrics().len() == 1);
    let m = &aws.metrics()[0];
    assert_eq!((m.namespace.as_str(), m.name.as_str(), m.value, m.unit.as_str()), ("Shop/Orders", "OrdersReceived", 1.0, "Count"));
    assert_eq!(m.dimensions, vec![("channel".to_string(), "web".to_string())]);
    main.stop().unwrap();
}

#[test]
fn aws2_ec2_stop_dev_instances() {
    let aws = AwsMock::start().unwrap();
    let id = aws.run_instance("ami-12345678", "t3.micro");
    let main = run_aws(&aws, &page("aws2-ec2"), "aws2-ec2-stop-dev-instances", &[("DEV_INSTANCE_IDS", &id)], &[]);
    wait_for("the instance to stop", || aws.ec2_instances().iter().any(|(i, s)| i == &id && s == "stopped"));
    main.stop().unwrap();
}

#[test]
fn aws2_ecs_and_eks_list_clusters() {
    let aws = AwsMock::start().unwrap();
    let setup = cameleer_main::new_main();
    aws_call(&setup, &aws, "aws2-ecs://setup?operation=createCluster", "", vec![("CamelAwsECSClusterName", Value::from("edge-workers"))]);
    let main = run_aws(&aws, &page("aws2-ecs"), "aws2-ecs-list-clusters", &[], &[("log:ecs-clusters", "mock:ecs-clusters")]);
    mock_bodies(&main, "ecs-clusters", 1);
    let arns = mock(&main, "ecs-clusters").exchange(0).unwrap().body().to_json();
    assert!(arns.contains("arn:aws:ecs") && arns.contains("edge-workers"), "{}", arns);
    main.stop().unwrap();

    let mut vpc = cameleer_core::value::ObjectMap::new();
    vpc.insert("subnetIds".into(), Value::Array(vec![Value::from("subnet-1")]));
    aws_call(
        &setup,
        &aws,
        "aws2-eks://setup?operation=createCluster",
        "",
        vec![("CamelAwsEKSClusterName", Value::from("prod-k8s")), ("CamelAwsEKSRoleARN", Value::from("arn:aws:iam::000000000000:role/eks")), ("CamelAwsEKSVPCConfig", Value::Object(vpc))],
    );
    let main = run_aws(&aws, &page("aws2-eks"), "aws2-eks-list-clusters", &[], &[("log:eks-clusters", "mock:eks-clusters")]);
    mock_bodies(&main, "eks-clusters", 1);
    assert_eq!(mock(&main, "eks-clusters").exchange(0).unwrap().body().to_json(), r#"["prod-k8s"]"#);
    main.stop().unwrap();
}

#[test]
fn aws2_iam_onboard_users() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("onboarding");
    aws.send_sqs("onboarding", "alice");
    let main = run_aws(&aws, &page("aws2-iam"), "aws2-iam-onboard-users", &[], &[]);
    wait_for("the IAM user", || aws.iam_users().contains(&"alice".to_string()));
    main.stop().unwrap();
}

#[test]
fn aws2_kms_encrypt_to_s3() {
    let aws = AwsMock::start().unwrap();
    let setup = cameleer_main::new_main();
    let key = aws_call(&setup, &aws, "aws2-kms://setup?operation=createKey", "", vec![("CamelAwsKMSDescription", Value::from("customer data"))]).header("CamelAwsKMSKeyId").unwrap().to_string();
    aws.create_queue("customer-records");
    aws.send_sqs("customer-records", r#"{"name":"Ada","iban":"NO9386011117947"}"#);
    let main = run_aws(&aws, &page("aws2-kms"), "aws2-kms-encrypt-to-s3", &[("KMS_KEY_ID", &key)], &[]);
    wait_for("the encrypted record", || aws.object_keys("encrypted-archive").len() == 1);
    let k = aws.object_keys("encrypted-archive")[0].clone();
    let cipher = aws.object("encrypted-archive", &k).unwrap();
    assert!(!String::from_utf8_lossy(&cipher).contains("Ada"), "stored encrypted");
    let plain = aws_call(&setup, &aws, "aws2-kms://setup?operation=decrypt", cipher, vec![]);
    assert_eq!(plain.body().to_string(), r#"{"name":"Ada","iban":"NO9386011117947"}"#);
    main.stop().unwrap();
}

#[test]
fn aws2_mq_list_brokers() {
    let aws = AwsMock::start().unwrap();
    let setup = cameleer_main::new_main();
    aws_call(&setup, &aws, "aws2-mq://setup?operation=createBroker", "", vec![("CamelAwsMQBrokerName", Value::from("orders-broker")), ("CamelAwsMQBrokerInstanceType", Value::from("mq.t3.micro"))]);
    let main = run_aws(&aws, &page("aws2-mq"), "aws2-mq-list-brokers", &[], &[("log:brokers", "mock:brokers")]);
    mock_bodies(&main, "brokers", 1);
    let brokers = mock(&main, "brokers").exchange(0).unwrap().body().to_json();
    assert!(brokers.contains("orders-broker"), "{}", brokers);
    main.stop().unwrap();
}

#[test]
fn aws2_msk_bootstrap_brokers() {
    let aws = AwsMock::start().unwrap();
    let setup = cameleer_main::new_main();
    let mut group = cameleer_core::value::ObjectMap::new();
    group.insert("instanceType".into(), Value::from("kafka.m5.large"));
    group.insert("clientSubnets".into(), Value::Array(vec![Value::from("subnet-1"), Value::from("subnet-2")]));
    let created = aws_call(
        &setup,
        &aws,
        "aws2-msk://setup?operation=createCluster",
        "",
        vec![("CamelAwsMSKClusterName", Value::from("events")), ("CamelAwsMSKClusterKafkaVersion", Value::from("3.6.0")), ("CamelAwsMSKBrokerNodesNumber", Value::from(2)), ("CamelAwsMSKBrokerNodesGroupInfo", Value::Object(group))],
    );
    let arn = created.header("CamelAwsMSKClusterArn").unwrap().to_string();
    let main = run_aws(&aws, &page("aws2-msk"), "aws2-msk-bootstrap-brokers", &[("MSK_CLUSTER_ARN", &arn)], &[("log:bootstrap", "mock:bootstrap")]);
    mock_bodies(&main, "bootstrap", 1);
    let brokers = mock(&main, "bootstrap").exchange(0).unwrap().body().clone();
    assert!(brokers.get("bootstrapBrokerStringSaslIam").unwrap().to_string().ends_with(":9098"), "{}", brokers.to_json());
    main.stop().unwrap();

    // the kafka uri of the page resolves with the AWS_MSK_IAM mechanism
    let text = std::fs::read_to_string(docs_dir().join(page("aws2-msk"))).unwrap();
    let start = text.find("kafka:orders?").unwrap();
    let uri = &text[start..start + text[start..].find('\n').unwrap()];
    setup.context().get_endpoint(uri).unwrap_or_else(|e| panic!("{}: {}", uri, e));
}

#[test]
fn aws2_redshift_data_sales_report() {
    let aws = AwsMock::start().unwrap();
    aws.add_redshift_table("sales", &["region", "total"], vec![vec![Value::from("eu"), Value::from(42)], vec![Value::from("us"), Value::from(17)]]);
    let main = run_aws(&aws, &page("aws2-redshift-data"), "aws2-redshift-data-sales-report", &[], &[("log:sales", "mock:sales")]);
    mock_bodies(&main, "sales", 1);
    let ex = mock(&main, "sales").exchange(0).unwrap();
    let rows = ex.body().to_json();
    assert!(rows.contains("\"region\"") && rows.contains("\"eu\"") && rows.contains("42"), "{}", rows);
    assert_eq!(ex.header("CamelAwsRedshiftDataTotalNumRows").and_then(|v| v.as_i64()), Some(2));
    main.stop().unwrap();
}

#[test]
fn aws2_ses_mail_alerts() {
    let aws = AwsMock::start().unwrap();
    aws.create_queue("alerts");
    aws.send_sqs("alerts", "Disk almost full on edge-7");
    let main = run_aws(&aws, &page("aws2-ses"), "aws2-ses-mail-alerts", &[], &[]);
    wait_for("the mail", || aws.sent_emails().len() == 1);
    let m = &aws.sent_emails()[0];
    assert_eq!((m.source.as_str(), m.to.clone(), m.subject.as_str()), ("alerts@example.com", vec!["oncall@example.com".to_string()], "Production alert"));
    assert_eq!(m.text.as_deref(), Some("Disk almost full on edge-7"));
    main.stop().unwrap();
}

#[test]
fn aws2_step_functions_start_orders() {
    let aws = AwsMock::start().unwrap();
    let setup = cameleer_main::new_main();
    let arn = aws_call(
        &setup,
        &aws,
        "aws2-step-functions://setup?operation=createStateMachine",
        r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#,
        vec![("CamelAwsStepFunctionsStateMachineName", Value::from("order-fulfilment")), ("CamelAwsStepFunctionsStateMachineRoleArn", Value::from("arn:aws:iam::000000000000:role/sfn"))],
    )
    .body()
    .to_string();
    aws.create_queue("orders");
    aws.send_sqs("orders", r#"{"orderId":"o-1"}"#);
    let main = run_aws(&aws, &page("aws2-step-functions"), "aws2-step-functions-start-orders", &[("STATE_MACHINE_ARN", &arn)], &[("log:executions", "mock:executions")]);
    let bodies = mock_bodies(&main, "executions", 1);
    assert!(bodies[0].starts_with("arn:aws:states:") && bodies[0].contains(":execution:order-fulfilment:"), "{:?}", bodies);
    main.stop().unwrap();
}

#[test]
fn aws2_sts_caller_identity() {
    let aws = AwsMock::start().unwrap();
    let main = run_aws(&aws, &page("aws2-sts"), "aws2-sts-caller-identity", &[], &[("log:identity", "mock:identity")]);
    mock_bodies(&main, "identity", 1);
    let id = mock(&main, "identity").exchange(0).unwrap().body().clone();
    assert!(id.get("arn").is_some() && id.get("account").is_some(), "{}", id.to_json());
    main.stop().unwrap();
}

#[test]
fn aws2_timestream_write_readings() {
    let aws = AwsMock::start().unwrap();
    let setup = cameleer_main::new_main();
    aws_call(&setup, &aws, "aws2-timestream://write:setup?operation=createDatabase", "", vec![("CamelAwsTimestreamDatabaseName", Value::from("iot"))]);
    aws_call(&setup, &aws, "aws2-timestream://write:setup?operation=createTable", "", vec![("CamelAwsTimestreamDatabaseName", Value::from("iot")), ("CamelAwsTimestreamTableName", Value::from("readings"))]);
    aws.create_queue("sensor-readings");
    aws.send_sqs("sensor-readings", r#"{"Dimensions":[{"Name":"device","Value":"edge-7"}],"MeasureName":"temperature","MeasureValue":"21.5","MeasureValueType":"DOUBLE","Time":"1730800000000"}"#);
    let main = run_aws(&aws, &page("aws2-timestream"), "aws2-timestream-write-readings", &[], &[]);
    wait_for("the record", || aws.timestream_records("iot", "readings").len() == 1);
    assert!(aws.timestream_records("iot", "readings")[0].to_json().contains("edge-7"));
    main.stop().unwrap();
}

//@@AWS_TESTS@@

// ---------------------------------------------------------------------------
// guides/cloud-credentials.md
// ---------------------------------------------------------------------------

const GUIDE: &str = "guides/cloud-credentials.md";

#[test]
fn cloud_credentials_localstack_override() {
    let aws = AwsMock::start().unwrap();
    aws.add_credentials("test", "test");
    let url = aws.url();
    let main = run_with(GUIDE, "cloud-credentials-localstack", &[("AWS_ENDPOINT_URL", &url)], &[], |_, _| {});
    wait_for("the message", || aws.queue_messages("orders") == vec!["hello from Cameleer".to_string()]);
    main.stop().unwrap();
}

#[test]
fn cloud_credentials_azurite_override() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("uploads");
    let url = az.blob_url();
    let main = run_with(GUIDE, "cloud-credentials-azurite", &[("AZURITE_BLOB_URL", &url)], &[], |_, _| {});
    wait_for("the blob", || az.blob("uploads", "hello.txt").is_some());
    assert_eq!(az.blob("uploads", "hello.txt").unwrap(), b"hello from Cameleer");
    main.stop().unwrap();
}

/// The route without any AWS option runs with component defaults given as `CAMEL_COMPONENT_*`
/// environment variables, then as `camel.component.*` properties (the page's properties block).
#[test]
fn cloud_credentials_component_defaults() {
    let id = "cloud-credentials-component-defaults";
    let yaml = block(GUIDE, id);

    let aws = AwsMock::start().unwrap();
    let mut main = cameleer_main::new_main();
    let env: std::collections::BTreeMap<String, String> = [
        ("CAMEL_COMPONENT_AWS2_SQS_REGION", "us-east-1".to_string()),
        ("CAMEL_COMPONENT_AWS2_SQS_ACCESS_KEY", AwsMock::ACCESS_KEY.to_string()),
        ("CAMEL_COMPONENT_AWS2_SQS_SECRET_KEY", AwsMock::SECRET_KEY.to_string()),
        ("CAMEL_COMPONENT_AWS2_SQS_URI_ENDPOINT_OVERRIDE", aws.url()),
        ("CAMEL_COMPONENT_AWS2_SQS_OVERRIDE_ENDPOINT", "true".to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    main.set_environment(env);
    main.add_route_source(format!("{}.camel.yaml", id), yaml.clone());
    main.start().unwrap();
    resolve_all(main.context(), id, &yaml);
    wait_for("the message (environment variables)", || aws.queue_messages("orders") == vec!["order o-1001".to_string()]);
    main.stop().unwrap();

    let aws = AwsMock::start().unwrap();
    let text = std::fs::read_to_string(docs_dir().join(GUIDE)).unwrap().replace("\r\n", "\n");
    let after = &text[text.find(&format!("<!-- example: {} -->", id)).unwrap()..];
    let start = after.find("```properties\n").unwrap() + 14;
    let props = after[start..start + after[start..].find("```").unwrap()].replace("eu-west-1", "us-east-1").replace("http://localhost:4566", &aws.url());
    let mut main = cameleer_main::new_main();
    main.set_environment(Default::default());
    main.load_properties_text(&format!("{}camel.component.aws2-sqs.accessKey={}\ncamel.component.aws2-sqs.secretKey={}\n", props, AwsMock::ACCESS_KEY, AwsMock::SECRET_KEY), false).unwrap();
    main.add_route_source(format!("{}.camel.yaml", id), yaml.clone());
    main.start().unwrap();
    wait_for("the message (properties)", || aws.queue_messages("orders") == vec!["order o-1001".to_string()]);
    main.stop().unwrap();
}

/// Placeholder defaults may contain `:` (model ids, ARNs, connection strings).
#[test]
fn env_placeholder_defaults_keep_colons() {
    let main = cameleer_main::new_main();
    let p = main.context().properties();
    assert_eq!(p.resolve("{{env:CAMELEER_DOCS_UNSET_A:anthropic.claude-3-haiku-20240307-v1:0}}").unwrap(), "anthropic.claude-3-haiku-20240307-v1:0");
    assert_eq!(p.resolve("{{env:CAMELEER_DOCS_UNSET_B:arn:aws:kafka:us-east-1:123456789012:cluster/events/abcd}}").unwrap(), "arn:aws:kafka:us-east-1:123456789012:cluster/events/abcd");
    assert_eq!(p.resolve("x{{env:CAMELEER_DOCS_UNSET_C:}}y").unwrap(), "xy");
}

/// Every YAML block of the AWS / Azure pages and of the guide has a unique example marker,
/// parses, and its endpoints resolve (with test values for the placeholders).
#[test]
fn all_examples_marked_parsed_and_resolved() {
    let mut pages: Vec<String> = std::fs::read_dir(docs_dir().join("components"))
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .filter(|n| (n.starts_with("aws") || n.starts_with("azure-")) && n.ends_with(".md"))
        .map(|n| format!("components/{}", n))
        .collect();
    pages.sort();
    pages.push(GUIDE.to_string());
    assert_eq!(pages.len(), 47, "34 AWS pages, 12 Azure pages and the guide: {:?}", pages);
    let env: Vec<(&str, &str)> = STORAGE_ENV.iter().chain(ENTRA_ENV.iter()).copied().chain([("COSMOS_KEY", azure_services::COSMOS_KEY), ("EVENTGRID_ACCESS_KEY", "c2VjcmV0LWV2ZW50Z3JpZC1rZXk="), ("AZURE_OPENAI_API_KEY", "test-key"), ("FUNCTION_KEY", "test-key"), ("PARTNER_TOKEN", "test-token")]).collect();
    let mut ids = std::collections::BTreeSet::new();
    let mut count = 0;
    for p in &pages {
        let text = std::fs::read_to_string(docs_dir().join(p)).unwrap().replace("\r\n", "\n");
        let lines: Vec<&str> = text.lines().collect();
        for (i, l) in lines.iter().enumerate() {
            if *l != "```yaml" {
                continue;
            }
            let prev = if i > 0 { lines[i - 1] } else { "" };
            let id = prev.strip_prefix("<!-- example: ").and_then(|r| r.strip_suffix(" -->")).unwrap_or_else(|| panic!("{} line {}: yaml block without an example marker", p, i + 1));
            assert!(ids.insert(id.to_string()), "duplicate example id {}", id);
            let yaml = env_subst(&block(p, id), &env);
            let main = cameleer_main::new_main();
            let routes = cameleer_yaml::load_routes(main.context(), &yaml).unwrap_or_else(|e| panic!("{}: {} does not parse: {}", p, id, e));
            assert!(!routes.is_empty(), "{}: no routes", id);
            resolve_all(main.context(), id, &yaml);
            count += 1;
        }
    }
    assert_eq!(count, ids.len());
    assert!(count >= 55, "{} examples", count);
}

// ---------------------------------------------------------------------------
// Azure
// ---------------------------------------------------------------------------

#[test]
fn azure_storage_blob_upload_and_consume() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("reports");
    let main = run_with(&page("azure-storage-blob"), "azure-storage-blob-upload-report", &STORAGE_ENV, &[], |ctx, _| azurite_options(ctx, &az));
    wait_for("the report blob", || az.blob_names("reports").len() == 1);
    let name = az.blob_names("reports")[0].clone();
    assert!(name.starts_with("reports/20") && name.ends_with(".csv"), "{}", name);
    assert_eq!(az.blob("reports", &name).unwrap(), b"region,total;eu,42;us,17");
    assert_eq!(az.blob_info("reports", &name).unwrap().0, "text/csv");
    main.stop().unwrap();

    az.put_blob("incoming", "orders/o-1.json", br#"{"orderId":"o-1"}"#);
    let main = run_with(&page("azure-storage-blob"), "azure-storage-blob-consume-incoming", &STORAGE_ENV, &[("log:incoming", "mock:incoming")], |ctx, _| azurite_options(ctx, &az));
    assert_eq!(mock_bodies(&main, "incoming", 1), vec![r#"{"orderId":"o-1"}"#.to_string()]);
    wait_for("the blob to be deleted", || az.blob_names("incoming").is_empty());
    main.stop().unwrap();
}

#[test]
fn azure_storage_queue_send_and_work() {
    let az = AzuriteMock::start().unwrap();
    let main = run_with(&page("azure-storage-queue"), "azure-storage-queue-send-job", &STORAGE_ENV, &[], |ctx, _| azurite_options(ctx, &az));
    wait_for("the job message", || az.queue_messages("jobs").len() == 1);
    assert_eq!(az.queue_messages("jobs")[0], r#"{"job":"resize","image":"cat.jpg"}"#);
    main.stop().unwrap();

    let main = run_with(&page("azure-storage-queue"), "azure-storage-queue-worker", &STORAGE_ENV, &[("log:jobs", "mock:jobs")], |ctx, _| azurite_options(ctx, &az));
    assert_eq!(mock_bodies(&main, "jobs", 1), vec![r#"{"job":"resize","image":"cat.jpg"}"#.to_string()]);
    wait_for("the message to be deleted", || az.queue_messages("jobs").is_empty());
    main.stop().unwrap();
}

#[test]
fn azure_storage_datalake_upload() {
    let az = AzuriteMock::start().unwrap();
    az.create_container("raw");
    let main = run_with(&page("azure-storage-datalake"), "azure-storage-datalake-upload", &STORAGE_ENV, &[], |ctx, _| azurite_options(ctx, &az));
    wait_for("the file", || az.blob("raw", "sensors/readings.json").is_some_and(|b| !b.is_empty()));
    assert_eq!(az.blob("raw", "sensors/readings.json").unwrap(), br#"{"device":"edge-7","temperature":21.5}"#);
    main.stop().unwrap();
}

#[test]
fn azure_storage_files_upload_export() {
    let az = AzuriteMock::start().unwrap();
    az.create_share("exports");
    let main = run_with(&page("azure-storage-files"), "azure-storage-files-upload-export", &STORAGE_ENV, &[], |ctx, _| azurite_options(ctx, &az));
    wait_for("the file on the share", || az.share_file("exports", "daily/stock.csv").as_deref() == Some(&b"sku,stock;A-1,12;B-7,0"[..]));
    assert_eq!(az.share_file("exports", "daily/stock.csv").unwrap(), b"sku,stock;A-1,12;B-7,0");
    main.stop().unwrap();
}

#[test]
fn azure_cosmosdb_upsert_and_change_feed() {
    let azm = AzureMock::start().unwrap();
    let url = azm.url();
    let env = [("COSMOS_ENDPOINT", url.as_str()), ("COSMOS_KEY", azure_services::COSMOS_KEY)];
    let main = run_with(&page("azure-cosmosdb"), "azure-cosmosdb-upsert-order", &env, &[], |_, _| {});
    wait_for("the item", || azm.cosmos_items("shop", "orders").len() == 1);
    let item = azm.cosmos_items("shop", "orders")[0].to_json();
    assert!(item.contains("o-1001") && item.contains("c-42"), "{}", item);
    main.stop().unwrap();

    let main = run_with(&page("azure-cosmosdb"), "azure-cosmosdb-change-feed", &env, &[("log:order-changes", "mock:order-changes")], |_, _| {});
    mock_bodies(&main, "order-changes", 1);
    let batch = mock(&main, "order-changes").exchange(0).unwrap().body().to_json();
    assert!(batch.starts_with('[') && batch.contains("o-1001"), "{}", batch);
    main.stop().unwrap();
}

#[test]
fn azure_eventgrid_publish_order() {
    let azm = AzureMock::start().unwrap();
    let key = "c2VjcmV0LWV2ZW50Z3JpZC1rZXk=";
    azm.create_eventgrid_topic("orders", key);
    let topic = azm.eventgrid_topic_url("orders");
    let main = run_with(&page("azure-eventgrid"), "azure-eventgrid-publish-order", &[("EVENTGRID_TOPIC_ENDPOINT", &topic), ("EVENTGRID_ACCESS_KEY", key)], &[], |_, _| {});
    wait_for("the event", || azm.eventgrid_events("orders").len() == 1);
    let e = azm.eventgrid_events("orders")[0].to_json();
    assert!(e.contains("Shop.OrderPlaced") && e.contains("orders/o-1001") && e.contains("99.5"), "{}", e);
    main.stop().unwrap();
}

#[test]
fn azure_functions_score_leads() {
    let az = AzuriteMock::start().unwrap();
    let azm = AzureMock::start().unwrap();
    azm.register_function("my-function-app", "score-lead", Some("fn-key-1"), |method, _query, body| {
        assert_eq!(method, "POST");
        let score = if String::from_utf8_lossy(body).contains("enterprise") { 87 } else { 20 };
        (200, "application/json".to_string(), format!("{{\"score\":{}}}", score).into_bytes())
    });
    az.put_queue_message("leads", r#"{"company":"ACME","plan":"enterprise"}"#);
    let env = [STORAGE_ENV[0], STORAGE_ENV[1], ("FUNCTION_KEY", "fn-key-1")];
    let main = run_with(&page("azure-functions"), "azure-functions-score-leads", &env, &[("log:scores", "mock:scores")], |ctx, _| {
        azurite_options(ctx, &az);
        ctx.set_component_option("azure-functions", "uriEndpointOverride", format!("{}/functions/my-function-app", azm.url()));
    });
    mock_bodies(&main, "scores", 1);
    let ex = mock(&main, "scores").exchange(0).unwrap();
    assert_eq!(ex.body().get("score").and_then(|v| v.as_i64()), Some(87));
    assert_eq!(ex.header("CamelAzureFunctionsStatusCode").and_then(|v| v.as_i64()), Some(200));
    main.stop().unwrap();
}

#[test]
fn azure_key_vault_create_secret_and_placeholder() {
    let azm = AzureMock::start().unwrap();
    let env = [ENTRA_ENV[0], ENTRA_ENV[1], ENTRA_ENV[2], ("PARTNER_TOKEN", "t0k3n")];
    let main = run_with(&page("azure-key-vault"), "azure-key-vault-create-secret", &env, &[], |ctx, _| {
        ctx.set_component_option("azure-key-vault", "authorityHost", azm.url());
        ctx.set_component_option("azure-key-vault", "uriEndpointOverride", azm.url());
        ctx.set_component_option("azure-key-vault", "overrideEndpoint", "true");
    });
    wait_for("the secret", || azm.secret("partner-api-token").is_some());
    assert_eq!(azm.secret("partner-api-token").unwrap(), "t0k3n");
    main.stop().unwrap();

    let id = "azure-key-vault-placeholder";
    let yaml = block(&page("azure-key-vault"), id).replace("log:partner-api", "mock:partner-api");
    let mut main = main_for(id, &yaml, |_| {});
    for (k, v) in [
        ("vaultName", "my-vault".to_string()),
        ("tenantId", azure_services::TENANT_ID.to_string()),
        ("clientId", azure_services::CLIENT_ID.to_string()),
        ("clientSecret", azure_services::CLIENT_SECRET.to_string()),
        ("authorityHost", azm.url()),
        ("uriEndpointOverride", azm.url()),
    ] {
        main.set_property(format!("camel.vault.azure.{}", k), v);
    }
    main.start().unwrap();
    mock_bodies(&main, "partner-api", 1);
    assert_eq!(mock(&main, "partner-api").exchange(0).unwrap().header("Authorization").map(|v| v.to_string()).as_deref(), Some("Bearer t0k3n"));
    main.stop().unwrap();
}

#[test]
fn azure_openai_answer_questions() {
    let az = AzuriteMock::start().unwrap();
    let azm = AzureMock::start().unwrap();
    azm.add_openai_deployment("gpt-4o-mini");
    let opts = azm.openai_options();
    let api_key = opts.split("apiKey=RAW(").nth(1).unwrap().split(')').next().unwrap().to_string();
    az.put_queue_message("questions", "How long does shipping to Norway take?");
    let env = [STORAGE_ENV[0], STORAGE_ENV[1], ("AZURE_OPENAI_API_KEY", api_key.as_str())];
    let main = run_with(&page("azure-openai"), "azure-openai-answer-questions", &env, &[("log:answers", "mock:answers")], |ctx, _| {
        azurite_options(ctx, &az);
        ctx.set_component_option("azure-openai", "uriEndpointOverride", azm.url());
    });
    let bodies = mock_bodies(&main, "answers", 1);
    // the emulator echoes the last user message
    assert!(bodies[0].contains("How long does shipping to Norway take?"), "{:?}", bodies);
    let req = azm.openai_requests()[0].to_json();
    assert!(req.contains("You answer customer questions about shipping in two sentences.") && req.contains("0.2"), "{}", req);
    main.stop().unwrap();
}

#[test]
fn azure_schema_registry_register() {
    let azm = AzureMock::start().unwrap();
    azm.create_schema_group("orders", "Avro");
    let main = run_with(&page("azure-schema-registry"), "azure-schema-registry-register", &ENTRA_ENV, &[("log:schemas", "mock:schemas")], |ctx, _| {
        ctx.set_component_option("azure-schema-registry", "authorityHost", azm.url());
        ctx.set_component_option("azure-schema-registry", "uriEndpointOverride", azm.url());
    });
    let bodies = mock_bodies(&main, "schemas", 1);
    assert!(!bodies[0].is_empty());
    assert!(mock(&main, "schemas").exchange(0).unwrap().header("CamelAzureSchemaRegistrySchemaVersion").is_some());
    main.stop().unwrap();
}

const SB_KEY_NAME: &str = "RootManageSharedAccessKey";
const SB_KEY: &str = "c2VjcmV0K2tleS8xMjM=";

#[test]
fn azure_servicebus_send_and_process() {
    let b = Amqp10Broker::builder().azure(SB_KEY_NAME, SB_KEY).start().unwrap();
    let cs = b.connection_string(SB_KEY_NAME, SB_KEY);
    let env = [("SERVICEBUS_CONNECTION_STRING", cs.as_str())];
    let main = run_with(&page("azure-servicebus"), "azure-servicebus-send-order", &env, &[], |_, _| {});
    assert!(b.wait_for(Duration::from_secs(10), |b| b.queue_depth("orders") == 1));
    main.stop().unwrap();

    let main = run_with(&page("azure-servicebus"), "azure-servicebus-process-orders", &env, &[("log:orders", "mock:orders")], |_, _| {});
    assert_eq!(mock_bodies(&main, "orders", 1), vec![r#"{"orderId":"o-1001","item":"camel saddle"}"#.to_string()]);
    let ex = mock(&main, "orders").exchange(0).unwrap();
    assert_eq!(ex.header("CamelAzureServiceBusContentType").map(|v| v.to_string()).as_deref(), Some("application/json"));
    assert!(b.wait_for(Duration::from_secs(10), |b| b.queue_depth("orders") == 0 && b.inflight_count() == 0), "completed");
    main.stop().unwrap();
}

#[test]
fn azure_eventhubs_send_and_consume() {
    let b = Amqp10Broker::builder().azure(SB_KEY_NAME, SB_KEY).event_hub("telemetry", 3).start().unwrap();
    let cs = format!("{};EntityPath=telemetry", b.connection_string(SB_KEY_NAME, SB_KEY));
    let env = [("EVENTHUBS_CONNECTION_STRING", cs.as_str())];
    let main = run_with(&page("azure-eventhubs"), "azure-eventhubs-send-telemetry", &env, &[], |_, _| {});
    assert!(b.wait_for(Duration::from_secs(10), |b| (0..3).map(|p| b.partition_size("telemetry", p)).sum::<usize>() == 1));
    main.stop().unwrap();

    let main = run_with(&page("azure-eventhubs"), "azure-eventhubs-consume-telemetry", &env, &[("log:telemetry", "mock:telemetry")], |_, _| {});
    assert_eq!(mock_bodies(&main, "telemetry", 1), vec![r#"{"device":"edge-7","temperature":21.5}"#.to_string()]);
    let ex = mock(&main, "telemetry").exchange(0).unwrap();
    assert_eq!(ex.header("CamelAzureEventHubsPartitionKey").map(|v| v.to_string()).as_deref(), Some("edge-7"));
    main.stop().unwrap();
}

// ---------------------------------------------------------------------------
// Rust DSL snippets (verbatim from the pages)
// ---------------------------------------------------------------------------

fn rust_dsl_aws2_sqs(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}&maxMessagesPerPoll=10&visibilityTimeout=30")
            .log("order ${header.CamelAwsSqsMessageId} (${header.orderType}): ${body}")
            .to("log:orders");
    })?;
    Ok(())
}

fn rust_dsl_aws2_s3(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-s3://incoming-invoices?region={{env:AWS_REGION:us-east-1}}&prefix=invoices/&moveAfterRead=true&destinationBucket=processed-invoices")
            .log("received ${header.CamelAwsS3Key} (${header.CamelAwsS3ContentLength} bytes)")
            .to("log:invoices");
    })?;
    Ok(())
}

fn rust_dsl_aws2_sns(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://alerts?region={{env:AWS_REGION:us-east-1}}")
            .to("aws2-sns://ops-alerts?region={{env:AWS_REGION:us-east-1}}&subject=Ops alert&autoCreateTopic=true")
            .log("published ${header.CamelAwsSnsMessageId}");
    })?;
    Ok(())
}

fn rust_dsl_aws2_lambda(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://images-to-resize?region={{env:AWS_REGION:us-east-1}}")
            .to("aws2-lambda://resize-image?region={{env:AWS_REGION:us-east-1}}&operation=invokeFunction")
            .to("aws2-sqs://resized-images?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
    })?;
    Ok(())
}

fn rust_dsl_aws2_eventbridge(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsEventbridgeEventSource", constant("com.example.shop"))
            .set_header("CamelAwsEventbridgeEventDetailType", constant("OrderPlaced"))
            .to("aws2-eventbridge://default?region={{env:AWS_REGION:us-east-1}}&operation=putEvent")
            .log("event entry ids: ${body}");
    })?;
    Ok(())
}

fn rust_dsl_aws2_kinesis(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-kinesis://clickstream?region={{env:AWS_REGION:us-east-1}}&iteratorType=TRIM_HORIZON&maxResultsPerRequest=100")
            .convert_body_to("String")
            .log("click from ${header.CamelAwsKinesisPartitionKey}: ${body}")
            .to("log:clicks");
    })?;
    Ok(())
}

fn rust_dsl_aws2_kinesis_firehose(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://app-logs?region={{env:AWS_REGION:us-east-1}}&maxMessagesPerPoll=10")
            .to("aws2-kinesis-firehose://app-logs-to-s3?region={{env:AWS_REGION:us-east-1}}")
            .log("record ${header.CamelAwsKinesisFirehoseRecordId}");
    })?;
    Ok(())
}

fn rust_dsl_aws2_ddb(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
            .unmarshal("json")
            .set_header("CamelAwsDdbItem", simple("${body}"))
            .to("aws2-ddb://orders?region={{env:AWS_REGION:us-east-1}}&operation=PutItem")
            .log("saved order ${header.CamelAwsDdbItem[orderId]}");
    })?;
    Ok(())
}

fn rust_dsl_aws2_ddbstream(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-ddbstream://orders?region={{env:AWS_REGION:us-east-1}}&streamIteratorType=FROM_START")
            .filter(simple("${header.CamelAwsDdbStreamEventName} == 'INSERT'"))
            .marshal("json")
            .to("aws2-sqs://new-orders?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
    })?;
    Ok(())
}

fn rust_dsl_aws_bedrock(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://support-tickets?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsBedrockConverseSystem", constant("Summarize the support ticket in one sentence."))
            .to("aws-bedrock://summarizer?region={{env:AWS_REGION:us-east-1}}&modelId={{env:BEDROCK_MODEL_ID:anthropic.claude-3-haiku-20240307-v1:0}}&operation=converse")
            .log("summary (${header.CamelAwsBedrockConverseStopReason}): ${body}")
            .to("aws2-sns://ticket-summaries?region={{env:AWS_REGION:us-east-1}}&autoCreateTopic=true");
    })?;
    Ok(())
}

fn rust_dsl_aws_bedrock_agent_runtime(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://questions?region={{env:AWS_REGION:us-east-1}}")
            .to("aws-bedrock-agent-runtime://kb?region={{env:AWS_REGION:us-east-1}}&knowledgeBaseId={{env:KNOWLEDGE_BASE_ID:KB12345678}}&modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=retrieveAndGenerate")
            .log("answer (session ${header.CamelAwsBedrockAgentRuntimeSessionId}): ${body}")
            .to("log:answers");
    })?;
    Ok(())
}

fn rust_dsl_aws2_comprehend(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://reviews?region={{env:AWS_REGION:us-east-1}}")
            .set_property("review", simple("${body}"))
            .to("aws2-comprehend://reviews?region={{env:AWS_REGION:us-east-1}}&operation=detectSentiment&languageCode=en")
            .choice()
                .when(simple("${body} == 'NEGATIVE'"))
                    .set_body(simple("${exchangeProperty.review}"))
                    .to("aws2-sqs://escalations?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true")
                .otherwise()
                    .log("review sentiment: ${body}")
            .end();
    })?;
    Ok(())
}

fn rust_dsl_aws2_textract(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-s3://scanned-invoices?region={{env:AWS_REGION:us-east-1}}")
            .to("aws2-textract://invoices?region={{env:AWS_REGION:us-east-1}}&operation=analyzeExpense")
            .marshal("json")
            .to("aws2-sqs://extracted-invoices?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
    })?;
    Ok(())
}

fn rust_dsl_aws2_translate(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://replies-en?region={{env:AWS_REGION:us-east-1}}")
            .to("aws2-translate://replies?region={{env:AWS_REGION:us-east-1}}&sourceLanguage=en&targetLanguage=de")
            .to("aws2-sqs://replies-de?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
    })?;
    Ok(())
}

fn rust_dsl_aws2_polly(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://announcements?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsS3Key", simple("audio/${header.CamelAwsSqsMessageId}.mp3"))
            .to("aws2-polly://speech?region={{env:AWS_REGION:us-east-1}}&operation=synthesizeSpeech&voiceId=Joanna&outputFormat=mp3")
            .to("aws2-s3://{{env:AUDIO_BUCKET:announcement-audio}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true");
    })?;
    Ok(())
}

fn rust_dsl_aws2_rekognition(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-s3://uploaded-photos?region={{env:AWS_REGION:us-east-1}}")
            .set_property("photo", simple("${header.CamelAwsS3Key}"))
            .to("aws2-rekognition://photos?region={{env:AWS_REGION:us-east-1}}&operation=detectLabels&minConfidence=80")
            .marshal("json")
            .set_header("photo", simple("${exchangeProperty.photo}"))
            .to("aws2-sqs://photo-labels?region={{env:AWS_REGION:us-east-1}}&autoCreateQueue=true");
    })?;
    Ok(())
}

fn rust_dsl_aws2_transcribe(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://recordings?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsTranscribeTranscriptionJobName", simple("call-${header.CamelAwsSqsMessageId}"))
            .set_header("CamelAwsTranscribeOutputBucketName", constant("{{env:TRANSCRIPT_BUCKET:call-transcripts}}"))
            .to("aws2-transcribe://calls?region={{env:AWS_REGION:us-east-1}}&operation=startTranscriptionJob&languageCode=en-US")
            .log("job ${header.CamelAwsTranscribeTranscriptionJobName}: ${header.CamelAwsTranscribeTranscriptionJobStatus}")
            .to("log:jobs");
    })?;
    Ok(())
}

fn rust_dsl_aws_cloudtrail(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws-cloudtrail://s3-audit?region={{env:AWS_REGION:us-east-1}}&eventSource=s3.amazonaws.com&maxResults=10&delay=60000")
            .convert_body_to("String")
            .log("${header.CamelAwsCloudTrailEventUsername} called ${header.CamelAwsCloudTrailEventName}")
            .to("log:s3-audit");
    })?;
    Ok(())
}

fn rust_dsl_aws_config(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:config-rules?repeatCount=1")
            .set_header("CamelAwsConfigRuleName", constant("s3-bucket-public-read-prohibited"))
            .set_header("CamelAwsConfigRuleSourceIdentifier", constant("S3_BUCKET_PUBLIC_READ_PROHIBITED"))
            .to("aws-config://rules?region={{env:AWS_REGION:us-east-1}}&operation=putConfigRule")
            .log("rule ${header.CamelAwsConfigRuleName} is in place");
    })?;
    Ok(())
}

fn rust_dsl_aws_secrets_manager(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:store-token?repeatCount=1")
            .set_header("CamelAwsSecretsManagerSecretName", constant("partner-api"))
            .set_header("CamelAwsSecretsManagerSecretDescription", constant("Token of the partner API"))
            .set_body(constant(r#"{"token":"{{env:PARTNER_TOKEN:}}"}"#))
            .to("aws-secrets-manager://secrets?region={{env:AWS_REGION:us-east-1}}&operation=createSecret")
            .log("created ${body} (version ${header.CamelAwsSecretsManagerSecretVersionId})");
    })?;
    Ok(())
}

fn rust_dsl_aws2_athena(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:athena-report?repeatCount=1")
            .set_body(constant("SELECT region, count(*) AS orders FROM orders GROUP BY region"))
            .to("aws2-athena://report?region={{env:AWS_REGION:us-east-1}}&database=analytics&outputLocation=s3://{{env:ATHENA_RESULTS_BUCKET:athena-results}}/&waitTimeout=60000&operation=startQueryExecution")
            .to("aws2-athena://report?region={{env:AWS_REGION:us-east-1}}&operation=getQueryResults")
            .log("rows: ${body}")
            .to("log:report");
    })?;
    Ok(())
}

fn rust_dsl_aws2_cw(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
            .unmarshal("json")
            .set_header("CamelAwsCwMetricDimensions", simple("${body}"))
            .to("aws2-cw://Shop/Orders?region={{env:AWS_REGION:us-east-1}}&name=OrdersReceived&value=1&unit=Count");
    })?;
    Ok(())
}

fn rust_dsl_aws2_ec2(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:stop-dev?period=86400000")
            .set_header("CamelAwsEC2InstancesIds", constant("{{env:DEV_INSTANCE_IDS:i-0123456789abcdef0}}"))
            .to("aws2-ec2://ops?region={{env:AWS_REGION:us-east-1}}&operation=stopInstances")
            .log("state changes: ${body}");
    })?;
    Ok(())
}

fn rust_dsl_aws2_ecs(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:ecs-inventory?repeatCount=1")
            .to("aws2-ecs://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listClusters")
            .log("ECS clusters: ${body}")
            .to("log:ecs-clusters");
    })?;
    Ok(())
}

fn rust_dsl_aws2_eks(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:eks-inventory?repeatCount=1")
            .to("aws2-eks://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listClusters")
            .log("EKS clusters: ${body}")
            .to("log:eks-clusters");
    })?;
    Ok(())
}

fn rust_dsl_aws2_iam(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://onboarding?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsIAMUsername", simple("${body}"))
            .to("aws2-iam://iam?operation=createUser")
            .log("created IAM user ${body[userName]} (${body[arn]})");
    })?;
    Ok(())
}

fn rust_dsl_aws2_kms(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://customer-records?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsKMSKeyId", constant("{{env:KMS_KEY_ID:alias/customer-data}}"))
            .set_header("CamelAwsS3Key", simple("records/${header.CamelAwsSqsMessageId}.bin"))
            .to("aws2-kms://crypto?region={{env:AWS_REGION:us-east-1}}&operation=encrypt")
            .to("aws2-s3://{{env:ARCHIVE_BUCKET:encrypted-archive}}?region={{env:AWS_REGION:us-east-1}}&autoCreateBucket=true");
    })?;
    Ok(())
}

fn rust_dsl_aws2_mq(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:mq-inventory?repeatCount=1")
            .to("aws2-mq://inventory?region={{env:AWS_REGION:us-east-1}}&operation=listBrokers")
            .log("brokers: ${body}")
            .to("log:brokers");
    })?;
    Ok(())
}

fn rust_dsl_aws2_msk(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:msk?repeatCount=1")
            .set_header("CamelAwsMSKClusterArn", constant("{{env:MSK_CLUSTER_ARN:arn:aws:kafka:us-east-1:123456789012:cluster/events/abcd}}"))
            .to("aws2-msk://ops?region={{env:AWS_REGION:us-east-1}}&operation=getBootstrapBrokers")
            .log("IAM bootstrap: ${body[bootstrapBrokerStringSaslIam]}")
            .to("log:bootstrap");
    })?;
    Ok(())
}

fn rust_dsl_aws2_redshift_data(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:sales-report?repeatCount=1")
            .set_body(constant("SELECT * FROM sales"))
            .to("aws2-redshift-data://warehouse?region={{env:AWS_REGION:us-east-1}}&database=dev&clusterIdentifier={{env:REDSHIFT_CLUSTER:analytics-cluster}}&dbUser=reporter&operation=executeStatement&waitTimeout=60000")
            .to("aws2-redshift-data://warehouse?region={{env:AWS_REGION:us-east-1}}&operation=getStatementResult")
            .log("${header.CamelAwsRedshiftDataTotalNumRows} rows: ${body}")
            .to("log:sales");
    })?;
    Ok(())
}

fn rust_dsl_aws2_ses(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://alerts?region={{env:AWS_REGION:us-east-1}}")
            .to("aws2-ses://{{env:ALERT_SENDER:alerts@example.com}}?region={{env:AWS_REGION:us-east-1}}&to={{env:ONCALL_ADDRESS:oncall@example.com}}&subject=Production alert")
            .log("sent mail ${header.CamelAwsSesMessageId}");
    })?;
    Ok(())
}

fn rust_dsl_aws2_step_functions(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://orders?region={{env:AWS_REGION:us-east-1}}")
            .set_header("CamelAwsStepFunctionsStateMachineArn", constant("{{env:STATE_MACHINE_ARN:arn:aws:states:us-east-1:123456789012:stateMachine:order-fulfilment}}"))
            .to("aws2-step-functions://orders?region={{env:AWS_REGION:us-east-1}}&operation=startExecution")
            .log("started ${body}")
            .to("log:executions");
    })?;
    Ok(())
}

fn rust_dsl_aws2_sts(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:whoami?repeatCount=1")
            .to("aws2-sts://sts?region={{env:AWS_REGION:us-east-1}}&operation=getCallerIdentity")
            .log("running as ${body[arn]} in account ${body[account]}")
            .to("log:identity");
    })?;
    Ok(())
}

fn rust_dsl_aws2_timestream(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("aws2-sqs://sensor-readings?region={{env:AWS_REGION:us-east-1}}")
            .unmarshal("json")
            .set_header("CamelAwsTimestreamDatabaseName", constant("iot"))
            .set_header("CamelAwsTimestreamTableName", constant("readings"))
            .to("aws2-timestream://write:sensors?region={{env:AWS_REGION:us-east-1}}&operation=writeRecords")
            .log("records ingested: ${body}");
    })?;
    Ok(())
}

fn rust_dsl_azure_storage_blob(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-storage-blob://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/incoming?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})&deleteAfterRead=true")
            .log("received ${header.CamelAzureStorageBlobBlobName} (${header.CamelAzureStorageBlobBlobSize} bytes)")
            .to("log:incoming");
    })?;
    Ok(())
}

fn rust_dsl_azure_storage_queue(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/jobs?maxMessages=10&visibilityTimeout=PT60S&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
            .log("job ${header.CamelAzureStorageQueueMessageId} (dequeued ${header.CamelAzureStorageQueueDequeueCount}x): ${body}")
            .to("log:jobs");
    })?;
    Ok(())
}

fn rust_dsl_azure_storage_datalake(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:readings?repeatCount=1")
            .set_body(constant(r#"{"device":"edge-7","temperature":21.5}"#))
            .set_header("CamelAzureStorageDataLakeFileName", constant("readings.json"))
            .to("azure-storage-datalake://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/raw?operation=upload&directoryName=sensors&accountKey=RAW({{env:AZURE_STORAGE_KEY:}})")
            .log("stored sensors/${header.CamelAzureStorageDataLakeFileName}");
    })?;
    Ok(())
}

fn rust_dsl_azure_storage_files(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:export?repeatCount=1")
            .set_body(constant("sku,stock;A-1,12;B-7,0"))
            .set_header("CamelAzureStorageFilesFileName", constant("stock.csv"))
            .to("azure-storage-files://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/exports?operation=uploadFile&directoryName=daily&accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
            .log("uploaded daily/${header.CamelAzureStorageFilesFileName}");
    })?;
    Ok(())
}

fn rust_dsl_azure_cosmosdb(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-cosmosdb://shop/orders?databaseEndpoint={{env:COSMOS_ENDPOINT:https://myaccount.documents.azure.com:443/}}&accountKey=RAW({{env:COSMOS_KEY:}})&changeFeedStartFromBeginning=true")
            .log("changed orders: ${body}")
            .to("log:order-changes");
    })?;
    Ok(())
}

fn rust_dsl_azure_eventgrid(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:order-event?repeatCount=1")
            .set_body(constant(r#"{"orderId":"o-1001","total":99.5}"#))
            .unmarshal("json")
            .set_header("CamelAzureEventGridEventType", constant("Shop.OrderPlaced"))
            .set_header("CamelAzureEventGridSubject", constant("orders/o-1001"))
            .to("azure-eventgrid:{{env:EVENTGRID_TOPIC_ENDPOINT:https://mytopic.westeurope-1.eventgrid.azure.net/api/events}}?accessKey=RAW({{env:EVENTGRID_ACCESS_KEY:}})")
            .log("published event ${header.CamelAzureEventGridEventId}");
    })?;
    Ok(())
}

fn rust_dsl_azure_functions(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/leads?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
            .to("azure-functions://{{env:FUNCTION_APP:my-function-app}}/score-lead?functionKey=RAW({{env:FUNCTION_KEY:}})&parseJson=true")
            .log("lead score ${body[score]} (HTTP ${header.CamelAzureFunctionsStatusCode})")
            .to("log:scores");
    })?;
    Ok(())
}

fn rust_dsl_azure_key_vault(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:store-token?repeatCount=1")
            .set_header("CamelAzureKeyVaultSecretName", constant("partner-api-token"))
            .set_body(constant("{{env:PARTNER_TOKEN:}}"))
            .to("azure-key-vault://{{env:AZURE_KEY_VAULT_NAME:my-vault}}?operation=createSecret&tenantId={{env:AZURE_TENANT_ID:}}&clientId={{env:AZURE_CLIENT_ID:}}&clientSecret=RAW({{env:AZURE_CLIENT_SECRET:}})")
            .log("stored ${body} (version ${header.CamelAzureKeyVaultSecretVersion})");
    })?;
    Ok(())
}

fn rust_dsl_azure_openai(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-storage-queue://{{env:AZURE_STORAGE_ACCOUNT:mystorageaccount}}/questions?accessKey=RAW({{env:AZURE_STORAGE_KEY:}})")
            .to("azure-openai://{{env:AZURE_OPENAI_RESOURCE:my-openai}}/{{env:AZURE_OPENAI_DEPLOYMENT:gpt-4o-mini}}?apiKey=RAW({{env:AZURE_OPENAI_API_KEY:}})&operation=chatCompletion&temperature=0.2&systemMessage=You answer customer questions about shipping in two sentences.")
            .log("answer (${header.CamelAzureOpenAIFinishReason}): ${body}")
            .to("log:answers");
    })?;
    Ok(())
}

fn rust_dsl_azure_schema_registry(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:register-schema?repeatCount=1")
            .set_body(constant(r#"{"type":"record","name":"Order","namespace":"com.example","fields":[{"name":"id","type":"string"},{"name":"total","type":"double"}]}"#))
            .to("azure-schema-registry://{{env:EVENTHUBS_NAMESPACE:my-namespace}}/orders?operation=registerSchema&schemaName=com.example.Order&tenantId={{env:AZURE_TENANT_ID:}}&clientId={{env:AZURE_CLIENT_ID:}}&clientSecret=RAW({{env:AZURE_CLIENT_SECRET:}})")
            .log("schema id ${body}, version ${header.CamelAzureSchemaRegistrySchemaVersion}")
            .to("log:schemas");
    })?;
    Ok(())
}

fn rust_dsl_azure_servicebus(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-servicebus:orders?connectionString=RAW({{env:SERVICEBUS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me}})&maxConcurrentCalls=4")
            .log("order ${header.CamelAzureServiceBusMessageId} (delivery ${header.CamelAzureServiceBusDeliveryCount}): ${body}")
            .to("log:orders");
    })?;
    Ok(())
}

fn rust_dsl_azure_eventhubs(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("azure-eventhubs:?connectionString=RAW({{env:EVENTHUBS_CONNECTION_STRING:Endpoint=sb://my-namespace.servicebus.windows.net/;SharedAccessKeyName=RootManageSharedAccessKey;SharedAccessKey=change-me;EntityPath=telemetry}})&initialPosition=earliest")
            .convert_body_to("String")
            .log("partition ${header.CamelAzureEventHubsPartitionId} offset ${header.CamelAzureEventHubsOffset}: ${body}")
            .to("log:telemetry");
    })?;
    Ok(())
}

//@@RUST_DSL@@

type DslFn = fn(&CamelContext) -> Result<()>;

/// Every Rust DSL snippet builds its routes on a context with all components.
#[test]
fn rust_dsl_snippets_build() {
    let snippets: &[(&str, DslFn)] = &[
        ("aws2-sqs", rust_dsl_aws2_sqs),
        ("aws2-s3", rust_dsl_aws2_s3),
        ("aws2-sns", rust_dsl_aws2_sns),
        ("aws2-lambda", rust_dsl_aws2_lambda),
        ("aws2-eventbridge", rust_dsl_aws2_eventbridge),
        ("aws2-kinesis", rust_dsl_aws2_kinesis),
        ("aws2-kinesis-firehose", rust_dsl_aws2_kinesis_firehose),
        ("aws2-ddb", rust_dsl_aws2_ddb),
        ("aws2-ddbstream", rust_dsl_aws2_ddbstream),
        ("aws-bedrock", rust_dsl_aws_bedrock),
        ("aws-bedrock-agent-runtime", rust_dsl_aws_bedrock_agent_runtime),
        ("aws2-comprehend", rust_dsl_aws2_comprehend),
        ("aws2-textract", rust_dsl_aws2_textract),
        ("aws2-translate", rust_dsl_aws2_translate),
        ("aws2-polly", rust_dsl_aws2_polly),
        ("aws2-rekognition", rust_dsl_aws2_rekognition),
        ("aws2-transcribe", rust_dsl_aws2_transcribe),
        ("aws-cloudtrail", rust_dsl_aws_cloudtrail),
        ("aws-config", rust_dsl_aws_config),
        ("aws-secrets-manager", rust_dsl_aws_secrets_manager),
        ("aws2-athena", rust_dsl_aws2_athena),
        ("aws2-cw", rust_dsl_aws2_cw),
        ("aws2-ec2", rust_dsl_aws2_ec2),
        ("aws2-ecs", rust_dsl_aws2_ecs),
        ("aws2-eks", rust_dsl_aws2_eks),
        ("aws2-iam", rust_dsl_aws2_iam),
        ("aws2-kms", rust_dsl_aws2_kms),
        ("aws2-mq", rust_dsl_aws2_mq),
        ("aws2-msk", rust_dsl_aws2_msk),
        ("aws2-redshift-data", rust_dsl_aws2_redshift_data),
        ("aws2-ses", rust_dsl_aws2_ses),
        ("aws2-step-functions", rust_dsl_aws2_step_functions),
        ("aws2-sts", rust_dsl_aws2_sts),
        ("aws2-timestream", rust_dsl_aws2_timestream),
        ("azure-storage-blob", rust_dsl_azure_storage_blob),
        ("azure-storage-queue", rust_dsl_azure_storage_queue),
        ("azure-storage-datalake", rust_dsl_azure_storage_datalake),
        ("azure-storage-files", rust_dsl_azure_storage_files),
        ("azure-cosmosdb", rust_dsl_azure_cosmosdb),
        ("azure-eventgrid", rust_dsl_azure_eventgrid),
        ("azure-functions", rust_dsl_azure_functions),
        ("azure-key-vault", rust_dsl_azure_key_vault),
        ("azure-openai", rust_dsl_azure_openai),
        ("azure-schema-registry", rust_dsl_azure_schema_registry),
        ("azure-servicebus", rust_dsl_azure_servicebus),
        ("azure-eventhubs", rust_dsl_azure_eventhubs),
        //@@RUST_DSL_LIST@@
    ];
    let source = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/docs_cloud_aws_azure.rs")).unwrap().replace("\r\n", "\n");
    for (name, f) in snippets {
        let main = cameleer_main::new_main();
        f(main.context()).unwrap_or_else(|e| panic!("Rust DSL of {}: {}", name, e));
        // the page's ```rust block is copied verbatim (indented by four spaces) into this file
        let text = std::fs::read_to_string(docs_dir().join(page(name))).unwrap().replace("\r\n", "\n");
        let start = text.find("```rust\n").unwrap_or_else(|| panic!("{}: no Rust DSL block", name)) + 8;
        let snippet = &text[start..start + text[start..].find("```").unwrap()];
        let indented: String = snippet.lines().map(|l| if l.is_empty() { "\n".to_string() } else { format!("    {}\n", l) }).collect();
        assert!(source.contains(&format!("fn rust_dsl_{}(ctx: &CamelContext) -> Result<()> {{\n{}    Ok(())", name.replace('-', "_"), indented)), "{}: the Rust DSL block differs from rust_dsl_{}", name, name.replace('-', "_"));
    }
}
