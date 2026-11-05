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

use std::time::{Duration, Instant};

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
    let mut yaml = env_subst(&block(page, id), env);
    for (from, to) in swap {
        assert!(yaml.contains(from), "{} does not contain {}", id, from);
        yaml = yaml.replace(from, to);
    }
    let mut main = main_for(id, &yaml, |ctx| aws_options(ctx, aws, &yaml));
    main.start().unwrap_or_else(|e| panic!("example {} does not start: {}", id, e));
    main
}

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

//@@AWS_TESTS@@

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
