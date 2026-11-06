//! The route files in `examples/` run end to end against embedded servers and mocks, so the
//! examples shown in the README and on the website keep working. Endpoints are redirected by
//! replacing the `{{env:...}}` placeholders (and ports) in the file text; the routes
//! themselves run unchanged.

// needs every component family (the default features)
#![cfg(feature = "full")]

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;

fn example(name: &str, replace: &[(&str, &str)]) -> String {
    let path = format!("{}/../../examples/{}", env!("CARGO_MANIFEST_DIR"), name);
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e));
    for (from, to) in replace {
        assert!(text.contains(from), "{} does not contain {}", name, from);
        text = text.replace(from, to);
    }
    text
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A minimal HTTP/1.1 request; returns (status, body).
fn http(port: u16, method: &str, path: &str, json: Option<&str>) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(e) if Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => panic!("connect {}: {}", port, e),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let body = json.unwrap_or("");
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        method,
        path,
        body.len(),
        body
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn rest_api_example() {
    let port = free_port().to_string();
    let mut main = cameleer_main::new_main();
    main.add_route_source("rest-api.camel.yaml", example("rest-api.camel.yaml", &[("port: 8080", &format!("port: {}", port))]));
    main.start().unwrap();
    let port: u16 = port.parse().unwrap();

    let (status, body) = http(port, "GET", "/api/orders/42", None);
    assert_eq!(status, 200, "{}", body);
    assert!(body.contains("\"id\"") && body.contains("42") && body.contains("shipped"), "{}", body);

    let (status, body) = http(port, "POST", "/api/orders", Some(r#"{"item":"camel","qty":3}"#));
    assert_eq!(status, 201, "{}", body);
    assert!(body.contains("\"created\"") && body.contains("camel"), "{}", body);

    let (status, body) = http(port, "POST", "/api/orders", Some(r#"{"item":"camel","qty":11}"#));
    assert_eq!(status, 400, "{}", body);
    assert!(body.contains("Too many items: 11"), "{}", body);
    main.stop().unwrap();
}

#[test]
fn ai_ticket_triage_example() {
    let broker = cameleer_kafka_mqtt::testing::KafkaBroker::start().unwrap();
    let bootstrap = broker.bootstrap();
    // the model is stubbed with a route that answers like the prompt asks; the openai
    // component itself is covered by the cameleer-saas OpenAI mock tests
    let openai = "openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}";
    let text = example("ai-ticket-triage.camel.yaml", &[("{{env:KAFKA_BROKERS:localhost:9092}}", &bootstrap)]);
    let text = {
        let start = text.find(openai).expect("openai endpoint");
        let end = start + text[start..].find('"').unwrap();
        format!("{}direct:model{}", &text[..start], &text[end..])
    };
    let mut main = cameleer_main::new_main();
    main.add_route_source("ai-ticket-triage.camel.yaml", text);
    let b2 = bootstrap.clone();
    main.configure(move |ctx| {
        ctx.add_routes(move |b| {
            b.from("direct:model").process(|ex: &mut Exchange| {
                let ticket = ex.body().to_string().to_ascii_lowercase();
                ex.set_body(if ticket.contains("down") || ticket.contains("breach") { "P1" } else { "P3" });
                Ok(())
            });
            b.from(&format!("kafka:tickets-urgent?brokers={}&groupId=t1&autoOffsetReset=earliest", b2)).to("mock:urgent");
            b.from(&format!("kafka:tickets-routine?brokers={}&groupId=t2&autoOffsetReset=earliest", b2)).to("mock:routine");
        })
    });
    main.start().unwrap();
    let ctx = main.context().clone();
    let t = ctx.create_producer_template();
    let topic = format!("kafka:support-tickets?brokers={}", bootstrap);
    t.send_body(&topic, "Checkout is down for all EU customers").unwrap();
    t.send_body(&topic, "How do I change my invoice address?").unwrap();

    let urgent = ctx.mock_endpoint("mock:urgent").unwrap();
    let routine = ctx.mock_endpoint("mock:routine").unwrap();
    wait_for("both tickets", || urgent.received_exchanges().len() == 1 && routine.received_exchanges().len() == 1);
    // the ticket is forwarded unchanged, with the model's answer as a header
    let u = &urgent.received_exchanges()[0];
    assert_eq!(u.body().to_string(), "Checkout is down for all EU customers");
    assert_eq!(u.header("priority").map(|v| v.to_string()).as_deref(), Some("P1"));
    assert_eq!(routine.received_exchanges()[0].body().to_string(), "How do I change my invoice address?");
    main.stop().unwrap();
}

#[test]
fn alerts_to_slack_example() {
    let slack = cameleer_saas::testing::SlackMock::start();
    let webhook = format!("{}{}", slack.server.url(), cameleer_saas::testing::SlackMock::WEBHOOK_PATH);
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.set_property("camel.server.port", port.to_string());
    main.add_route_source("alerts-to-slack.camel.yaml", example("alerts-to-slack.camel.yaml", &[("{{env:SLACK_WEBHOOK_URL}}", &webhook)]));
    main.start().unwrap();

    let (status, _) = http(port, "POST", "/alerts", Some(r#"{"service":"checkout","severity":"critical","message":"error rate 12%"}"#));
    assert_eq!(status, 202);
    let (status, _) = http(port, "POST", "/alerts", Some(r#"{"service":"search","severity":"warning","message":"p99 400 ms"}"#));
    assert_eq!(status, 202);

    let posts = slack.server.requests_to(cameleer_saas::testing::SlackMock::WEBHOOK_PATH);
    assert_eq!(posts.len(), 1, "only the critical alert is posted");
    let text = posts[0].json().get("text").map(|t| t.to_string()).unwrap_or_default();
    assert!(text.contains("*checkout*: error rate 12%"), "{}", text);
    main.stop().unwrap();
}

#[test]
fn files_to_s3_example() {
    let aws = cameleer_cloud::testing::AwsMock::start().unwrap();
    let inbox = std::env::temp_dir().join(format!("cameleer-example-inbox-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&inbox);
    std::fs::create_dir_all(&inbox).unwrap();
    let inbox_s = inbox.to_string_lossy().replace('\\', "/");
    let mock_opts = aws.endpoint_options();
    let text = example(
        "files-to-s3.camel.yaml",
        &[
            ("{{env:INBOX_DIR:inbox}}", &inbox_s),
            ("{{env:S3_BUCKET:my-bucket}}", "reports"),
            ("region={{env:AWS_REGION:us-east-1}}&useDefaultCredentialsProvider=true", &mock_opts),
        ],
    );
    let mut main = cameleer_main::new_main();
    main.add_route_source("files-to-s3.camel.yaml", text);
    main.start().unwrap();
    let t = main.context().create_producer_template();
    t.request_body_and_header(&format!("aws2-s3://reports?{}", mock_opts), "", "CamelAwsS3Operation", "createBucket").unwrap();

    std::fs::write(inbox.join("sales.csv"), "region,total\neu,42\n").unwrap();
    std::fs::write(inbox.join("notes.txt"), "not a csv").unwrap();
    wait_for("the upload", || aws.object("reports", "sales.csv").is_some() && inbox.join(".done").join("sales.csv").exists());
    assert_eq!(aws.object("reports", "sales.csv").unwrap(), b"region,total\neu,42\n");
    assert_eq!(aws.object_keys("reports"), vec!["sales.csv".to_string()], "only .csv files are uploaded");
    assert!(inbox.join("notes.txt").exists());
    main.stop().unwrap();
    let _ = std::fs::remove_dir_all(&inbox);
}

/// The triage example's OpenAI endpoint, verbatim, against the OpenAI mock: the model,
/// temperature and system prompt reach the API.
#[test]
fn ai_ticket_triage_example_openai_request() {
    let ai = cameleer_saas::testing::OpenAiMock::start();
    let text = example("ai-ticket-triage.camel.yaml", &[]);
    let start = text.find("openai:chat-completion?").expect("openai endpoint");
    let end = start + text[start..].find('"').unwrap();
    let uri = text[start..end]
        .replace("{{env:OPENAI_MODEL:gpt-4o-mini}}", "gpt-4o-mini")
        .replace("{{env:OPENAI_BASE_URL:https://api.openai.com/v1}}", &ai.base_url())
        + &format!("&apiKey={}", cameleer_saas::testing::OpenAiMock::API_KEY);
    let ctx = CamelContext::new();
    cameleer_saas::register(&ctx);
    ctx.add_routes(move |b| {
        b.from("direct:classify").to(&uri);
    })
    .unwrap();
    ctx.start().unwrap();
    let answer = ctx.create_producer_template().request_body("direct:classify", "Checkout is down").unwrap();
    assert!(answer.to_string().contains("Checkout is down"), "{}", answer);
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    assert_eq!(req.get("model").map(|m| m.to_string()).as_deref(), Some("gpt-4o-mini"));
    assert_eq!(req.get("temperature").and_then(|t| t.as_f64()), Some(0.0));
    let system = req.get("messages").and_then(|m| m.as_array()).and_then(|m| m.first()).and_then(|m| m.get("content")).map(|c| c.to_string()).unwrap_or_default();
    assert!(system.starts_with("Classify the support ticket.") && system.ends_with("Reply with P1 or P3 only."), "{}", system);
    ctx.stop().unwrap();
}

/// The Quick start route in the README and on the website.
#[test]
fn hello_api_example() {
    let port = free_port();
    let mut main = cameleer_main::new_main();
    main.set_property("camel.server.port", port.to_string());
    main.add_route_source("hello-api.camel.yaml", example("hello-api.camel.yaml", &[]));
    main.start().unwrap();
    let (status, body) = http(port, "GET", "/orders/42", None);
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, r#"{"id": "42", "status": "shipped"}"#);
    main.stop().unwrap();
}
