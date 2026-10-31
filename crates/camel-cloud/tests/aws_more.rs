//! The additional AWS components end-to-end against the SigV4-verifying emulator: Kinesis,
//! Firehose, EventBridge, Secrets Manager (+ `{{aws:...}}`), SES, CloudWatch, STS, Step
//! Functions, IAM, EC2, S3 multipart / streaming uploads and SQS visibility extension.

use std::time::{Duration, Instant};

use camel_cloud::testing::AwsMock;
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

fn s(v: &Value) -> String {
    v.to_string()
}

fn err_of(r: Result<Value>) -> CamelError {
    r.unwrap_err().root_cause().clone()
}

#[test]
fn kinesis_put_records_and_consume_every_shard() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-kinesis://events?{}", opts);
    t.request_body_and_header(&uri, "", "CamelAwsKinesisOperation", "createStream").unwrap();
    // the emulated stream has a single shard by default: recreate it with 2
    t.request_body_and_header(&uri, "", "CamelAwsKinesisOperation", "deleteStream").unwrap();
    mock.create_stream("events", 2);
    let ex = t.request(&uri, |ex| {
        ex.set_body("first");
        ex.set_header("CamelAwsKinesisPartitionKey", "user-1");
    })
    .unwrap();
    assert!(ex.header("CamelAwsKinesisSequenceNumber").is_some_and(|v| !s(v).is_empty()));
    assert!(s(ex.header("CamelAwsKinesisShardId").unwrap()).starts_with("shardId-"));
    // putRecord requires a partition key
    let e = err_of(t.request_body(&uri, "no key"));
    assert!(e.message.contains("CamelAwsKinesisPartitionKey"), "{}", e);
    // an array body is sent with PutRecords
    let items = Value::Array(vec![Value::from("second"), Value::object([("data", "third"), ("partitionKey", "user-3")]), Value::from("fourth")]);
    let ex = t.request(&uri, |ex| {
        ex.set_body(items.clone());
        ex.set_header("CamelAwsKinesisPartitionKey", "batch");
    })
    .unwrap();
    assert_eq!(ex.header("CamelAwsKinesisFailedRecordCount").unwrap().as_i64(), Some(0));
    assert_eq!(ex.body().as_array().unwrap().len(), 3);
    let recs = mock.kinesis_records("events");
    assert_eq!(recs.len(), 4);
    assert!(recs.iter().any(|(pk, d)| pk == "user-3" && d == b"third"));
    let shards = t.request_body_and_header(&uri, "", "CamelAwsKinesisOperation", "listShards").unwrap();
    assert_eq!(shards.as_array().unwrap().len(), 2);

    // consumer: one iterator per shard (TRIM_HORIZON)
    let o2 = opts.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-kinesis://events?maxResultsPerRequest=10&delay=20&initialDelay=0&{}", o2)).to("mock:records");
    })
    .unwrap();
    let records = ctx.mock_endpoint("mock:records").unwrap();
    records.expected_bodies_received_in_any_order(vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec(), b"fourth".to_vec()]);
    records.all_messages(|ex| {
        ex.header("CamelAwsKinesisSequenceNumber").is_some()
            && ex.header("CamelAwsKinesisPartitionKey").is_some()
            && ex.header("CamelAwsKinesisShardId").is_some_and(|v| v.to_string().starts_with("shardId-"))
            && ex.header("CamelAwsKinesisApproximateArrivalTimestamp").is_some()
    });
    ctx.start().unwrap();
    records.assert_is_satisfied().unwrap();
    // resharding closes the old shards; new records land on the new shard and are consumed too
    mock.reshard_stream("events", 1);
    records.reset();
    records.expected_bodies_received(vec![b"after reshard".to_vec()]);
    records.expected_header_received("CamelAwsKinesisShardId", "shardId-000000000002");
    mock.put_kinesis_record("events", "k", b"after reshard").unwrap();
    records.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    assert_eq!(mock.auth_failures(), 0);
}

#[test]
fn kinesis_after_sequence_number_iterator() {
    let mock = AwsMock::start().unwrap();
    mock.create_stream("seq", 1);
    let (_, first) = mock.put_kinesis_record("seq", "a", b"one").unwrap();
    mock.put_kinesis_record("seq", "a", b"two").unwrap();
    mock.put_kinesis_record("seq", "a", b"three").unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    ctx.add_routes(move |b| {
        b.from(&format!(
            "aws2-kinesis://seq?iteratorType=AFTER_SEQUENCE_NUMBER&sequenceNumber={}&shardId=shardId-000000000000&maxResultsPerRequest=5&delay=20&initialDelay=0&{}",
            first, opts
        ))
        .to("mock:after");
    })
    .unwrap();
    let after = ctx.mock_endpoint("mock:after").unwrap();
    after.expected_bodies_received(vec![b"two".to_vec(), b"three".to_vec()]);
    ctx.start().unwrap();
    after.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();
    // iteratorType validation
    assert!(context().get_endpoint(&format!("aws2-kinesis://seq?iteratorType=AT_SEQUENCE_NUMBER&{}", mock.endpoint_options())).is_err());
}

#[test]
fn firehose_send_and_batch() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-kinesis-firehose://logs?{}", mock.endpoint_options());
    let arn = t.request_body_and_header(&uri, "", "CamelAwsKinesisFirehoseOperation", "createDeliveryStream").unwrap();
    assert!(s(&arn).ends_with("deliverystream/logs"));
    let ex = t.request(&uri, |ex| ex.set_body("line 1")).unwrap();
    assert!(ex.header("CamelAwsKinesisFirehoseRecordId").is_some_and(|v| !s(v).is_empty()));
    let body = t.request_body(&uri, Value::Array(vec![Value::from("line 2"), Value::from("line 3")])).unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);
    assert_eq!(mock.firehose_records("logs"), vec![b"line 1".to_vec(), b"line 2".to_vec(), b"line 3".to_vec()]);
    let desc = t.request_body_and_header(&uri, "", "CamelAwsKinesisFirehoseOperation", "describeDeliveryStream").unwrap();
    assert_eq!(s(desc.get("DeliveryStreamStatus").unwrap()), "ACTIVE");
    t.request_body_and_header(&uri, "", "CamelAwsKinesisFirehoseOperation", "deleteDeliveryStream").unwrap();
    let e = err_of(t.request_body(&uri, "gone"));
    assert!(e.is_a("FirehoseException") && e.message.contains("ResourceNotFoundException"), "{}", e);
}

#[test]
fn eventbridge_rules_targets_and_events_routed_to_sqs() {
    let mock = AwsMock::start().unwrap();
    let queue_arn = format!("arn:aws:sqs:us-east-1:{}:orders-events", camel_cloud::testing::aws::ACCOUNT);
    mock.create_queue("orders-events");
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-eventbridge://default?{}", mock.endpoint_options());
    let arn = t
        .request(&format!("{}&operation=putRule", uri), |ex| {
            ex.set_header("CamelAwsEventbridgeRuleName", "orders");
            ex.set_header("CamelAwsEventbridgeEventPattern", r#"{"source":["shop.orders"],"detail-type":["OrderPlaced"]}"#);
        })
        .unwrap();
    assert!(s(arn.body()).ends_with("rule/orders"));
    t.request(&format!("{}&operation=putTargets", uri), |ex| {
        ex.set_header("CamelAwsEventbridgeRuleName", "orders");
        ex.set_header("CamelAwsEventbridgeTargets", Value::Array(vec![Value::object([("id", Value::from("q")), ("arn", Value::from(queue_arn.clone()))])]));
    })
    .unwrap();
    let send_event = |source: &str, dt: &str, body: Value| {
        t.request(&format!("{}&operation=putEvent", uri), |ex| {
            ex.set_body(body.clone());
            ex.set_header("CamelAwsEventbridgeEventSource", source);
            ex.set_header("CamelAwsEventbridgeEventDetailType", dt);
            ex.set_header("CamelAwsEventbridgeEventResources", "arn:aws:shop:order/1,arn:aws:shop:order/2");
        })
        .unwrap()
    };
    let ex = send_event("shop.orders", "OrderPlaced", Value::object([("id", 42)]));
    assert_eq!(ex.body().as_array().unwrap().len(), 1);
    assert_eq!(ex.header("CamelAwsEventbridgeFailedEntryCount").unwrap().as_i64(), Some(0));
    send_event("shop.billing", "InvoiceSent", Value::object([("id", 7)]));
    assert_eq!(mock.events().len(), 2);
    assert_eq!(mock.events()[0].get("resources").unwrap().as_array().unwrap().len(), 2);
    // only the matching event was delivered to the SQS target
    let delivered = mock.queue_messages("orders-events");
    assert_eq!(delivered.len(), 1, "{:?}", delivered);
    let ev = camel_core::json::parse(&delivered[0]).unwrap();
    assert_eq!(ev.get("detail").unwrap().get("id").unwrap(), &Value::Int(42));
    // listRules / describeRule / listTargetsByRule / listRuleNamesByTarget
    let rules = t.request_body(&format!("{}&operation=listRules", uri), "").unwrap();
    assert_eq!(rules.as_array().unwrap().len(), 1);
    let rule = t.request_body_and_header(&format!("{}&operation=describeRule", uri), "", "CamelAwsEventbridgeRuleName", "orders").unwrap();
    assert_eq!(s(rule.get("State").unwrap()), "ENABLED");
    t.request_body_and_header(&format!("{}&operation=disableRule", uri), "", "CamelAwsEventbridgeRuleName", "orders").unwrap();
    send_event("shop.orders", "OrderPlaced", Value::object([("id", 43)]));
    assert_eq!(mock.queue_messages("orders-events").len(), 1, "disabled rules do not match");
    let targets = t.request_body_and_header(&format!("{}&operation=listTargetsByRule", uri), "", "CamelAwsEventbridgeRuleName", "orders").unwrap();
    assert_eq!(s(targets.as_array().unwrap()[0].get("Arn").unwrap()), queue_arn);
    let names = t.request_body_and_header(&format!("{}&operation=listRuleNamesByTarget", uri), "", "CamelAwsEventbridgeTargetArn", queue_arn.as_str()).unwrap();
    assert_eq!(names, Value::Array(vec![Value::from("orders")]));
    // a rule with targets cannot be deleted
    assert!(t.request_body_and_header(&format!("{}&operation=deleteRule", uri), "", "CamelAwsEventbridgeRuleName", "orders").is_err());
    t.request(&format!("{}&operation=removeTargets", uri), |ex| {
        ex.set_header("CamelAwsEventbridgeRuleName", "orders");
        ex.set_header("CamelAwsEventbridgeTargetsIds", "q");
    })
    .unwrap();
    t.request_body_and_header(&format!("{}&operation=deleteRule", uri), "", "CamelAwsEventbridgeRuleName", "orders").unwrap();
    assert!(mock.rule_names("default").is_empty());
}

#[test]
fn secrets_manager_operations() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws-secrets-manager://vault?{}", mock.endpoint_options());
    let ex = t
        .request(&format!("{}&operation=createSecret", uri), |ex| {
            ex.set_body(r#"{"user":"scott","password":"tiger"}"#);
            ex.set_header("CamelAwsSecretsManagerSecretName", "db");
            ex.set_header("CamelAwsSecretsManagerSecretDescription", "database");
        })
        .unwrap();
    let arn = s(ex.body());
    assert!(arn.starts_with("arn:aws:secretsmanager:us-east-1:"), "{}", arn);
    assert!(ex.header("CamelAwsSecretsManagerSecretVersionId").is_some());
    // getSecret (default operation) by name and by ARN
    let v = t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", "db").unwrap();
    assert_eq!(s(&v), r#"{"user":"scott","password":"tiger"}"#);
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", arn.as_str()).unwrap(), v);
    // updateSecret, then getSecret returns the new version
    t.request(&format!("{}&operation=updateSecret", uri), |ex| {
        ex.set_body("rotated-value");
        ex.set_header("CamelAwsSecretsManagerSecretId", "db");
    })
    .unwrap();
    assert_eq!(mock.secret("db").as_deref(), Some("rotated-value"));
    // describe / list
    let d = t.request_body_and_header(&format!("{}&operation=describeSecret", uri), "", "CamelAwsSecretsManagerSecretId", "db").unwrap();
    assert_eq!(s(d.get("Description").unwrap()), "database");
    let list = t.request_body(&format!("{}&operation=listSecrets", uri), "").unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    // rotateSecret stub
    t.request(&format!("{}&operation=rotateSecret", uri), |ex| {
        ex.set_header("CamelAwsSecretsManagerSecretId", "db");
        ex.set_header("CamelAwsSecretsManagerLambdaRotationFunctionArn", "arn:aws:lambda:us-east-1:000000000000:function:rotator");
    })
    .unwrap();
    assert_eq!(mock.secret_rotation("db"), Some((true, Some("arn:aws:lambda:us-east-1:000000000000:function:rotator".into()))));
    // binary payload
    t.request(&format!("{}&operation=createSecret&binaryPayload=true", uri), |ex| {
        ex.set_body(Value::Bytes(vec![0, 1, 2, 255]));
        ex.set_header("CamelAwsSecretsManagerSecretName", "blob");
    })
    .unwrap();
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", "blob").unwrap(), Value::Bytes(vec![0, 1, 2, 255]));
    // deleteSecret (recoverable), restoreSecret, missing secret
    t.request_body_and_header(&format!("{}&operation=deleteSecret", uri), "", "CamelAwsSecretsManagerSecretId", "db").unwrap();
    assert!(mock.secret_deleted("db"));
    t.request_body_and_header(&format!("{}&operation=restoreSecret", uri), "", "CamelAwsSecretsManagerSecretId", "db").unwrap();
    assert!(!mock.secret_deleted("db"));
    let e = err_of(t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", "nope"));
    assert!(e.is_a("SecretsManagerException") && e.is_a("AwsServiceException") && e.message.contains("ResourceNotFoundException"), "{}", e);
}

#[test]
fn aws_secrets_properties_function() {
    let mock = AwsMock::start().unwrap();
    mock.put_secret("db", r#"{"user":"scott","port":5432}"#);
    mock.put_secret("plain", "s3cr3t");
    let ctx = context();
    let p = ctx.properties();
    p.set("camel.vault.aws.accessKey", AwsMock::ACCESS_KEY);
    p.set("camel.vault.aws.secretKey", AwsMock::SECRET_KEY);
    p.set("camel.vault.aws.region", "us-east-1");
    p.set("camel.vault.aws.uriEndpointOverride", mock.url());
    p.set("camel.vault.aws.overrideEndpoint", "true");
    assert_eq!(ctx.resolve_placeholders("{{aws:plain}}").unwrap(), "s3cr3t");
    assert_eq!(ctx.resolve_placeholders("jdbc://{{aws:db/user}}@host:{{aws:db/port}}").unwrap(), "jdbc://scott@host:5432");
    assert_eq!(ctx.resolve_placeholders("{{aws:missing:fallback}}").unwrap(), "fallback");
    assert_eq!(ctx.resolve_placeholders("{{aws:db/nofield:dflt}}").unwrap(), "dflt");
    assert!(ctx.resolve_placeholders("{{aws:missing}}").is_err());
    // placeholders in routes
    ctx.add_routes(|b| {
        b.from("direct:secret").set_body(simple("user={{aws:db/user}}"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:secret", "").unwrap(), Value::from("user=scott"));
    ctx.stop().unwrap();
    // wrong credentials: the signature is rejected and the error propagates
    p.set("camel.vault.aws.secretKey", "wrong");
    let e = ctx.resolve_placeholders("{{aws:plain}}").unwrap_err();
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
    assert_eq!(mock.auth_failures(), 1);
}

#[test]
fn ses_send_email_and_raw_email() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-ses://noreply@example.com?to=a@example.com,b@example.com&subject=Hello&{}", mock.endpoint_options());
    let ex = t.request(&uri, |ex| ex.set_body("plain text body")).unwrap();
    assert!(ex.header("CamelAwsSesMessageId").is_some_and(|v| !s(v).is_empty()));
    let ex2 = t
        .request(&uri, |ex| {
            ex.set_body("<b>hi</b>");
            ex.set_header("CamelAwsSesTo", Value::Array(vec![Value::from("c@example.com")]));
            ex.set_header("CamelAwsSesCc", "d@example.com");
            ex.set_header("CamelAwsSesSubject", "Override");
            ex.set_header("CamelAwsSesHtmlEmail", true);
            ex.set_header("CamelAwsSesReplyToAddresses", "reply@example.com");
        })
        .unwrap();
    assert_ne!(ex2.header("CamelAwsSesMessageId"), ex.header("CamelAwsSesMessageId"));
    let raw = b"From: noreply@example.com\r\nTo: a@example.com\r\nSubject: raw\r\n\r\nraw body".to_vec();
    t.request_body(&uri, Value::Bytes(raw.clone())).unwrap();
    let sent = mock.sent_emails();
    assert_eq!(sent.len(), 3);
    assert_eq!((sent[0].source.as_str(), sent[0].to.clone(), sent[0].subject.as_str(), sent[0].text.as_deref()), ("noreply@example.com", vec!["a@example.com".to_string(), "b@example.com".to_string()], "Hello", Some("plain text body")));
    assert_eq!((sent[1].to.clone(), sent[1].cc.clone(), sent[1].subject.as_str(), sent[1].html.as_deref()), (vec!["c@example.com".to_string()], vec!["d@example.com".to_string()], "Override", Some("<b>hi</b>")));
    assert_eq!(sent[1].reply_to, vec!["reply@example.com".to_string()]);
    assert_eq!(sent[2].raw.as_deref(), Some(&raw[..]));
    // no recipients
    let e = err_of(t.request_body(&format!("aws2-ses://noreply@example.com?{}", mock.endpoint_options()), "x"));
    assert!(e.message.contains("recipients"), "{}", e);
}

#[test]
fn cloudwatch_put_metric_data() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-cw://shop?name=orders&unit=Count&{}", mock.endpoint_options());
    t.request(&uri, |ex| {
        ex.set_header("CamelAwsCwMetricValue", 3.5);
        ex.set_header("CamelAwsCwMetricDimensions", Value::object([("region", "eu"), ("tier", "gold")]));
    })
    .unwrap();
    t.request(&uri, |ex| {
        ex.set_header("CamelAwsCwMetricName", "latency");
        ex.set_header("CamelAwsCwMetricUnit", "Milliseconds");
        ex.set_header("CamelAwsCwMetricValue", 120);
        ex.set_header("CamelAwsCwMetricDimensionName", "endpoint");
        ex.set_header("CamelAwsCwMetricDimensionValue", "/api");
        ex.set_header("CamelAwsCwMetricTimestamp", 1_700_000_000_000i64);
    })
    .unwrap();
    t.request_body(&uri, Value::Array(vec![Value::object([("name", Value::from("a")), ("value", Value::from(1))]), Value::object([("name", Value::from("b")), ("value", Value::from(2))])]))
        .unwrap();
    let m = mock.metrics();
    assert_eq!(m.len(), 4);
    assert_eq!((m[0].namespace.as_str(), m[0].name.as_str(), m[0].value, m[0].unit.as_str()), ("shop", "orders", 3.5, "Count"));
    assert_eq!(m[0].dimensions, vec![("region".to_string(), "eu".to_string()), ("tier".to_string(), "gold".to_string())]);
    assert_eq!((m[1].name.as_str(), m[1].value, m[1].unit.as_str()), ("latency", 120.0, "Milliseconds"));
    assert_eq!(m[1].dimensions, vec![("endpoint".to_string(), "/api".to_string())]);
    assert_eq!(m[1].timestamp, "2023-11-14T22:13:20.000Z");
    assert_eq!((m[3].name.as_str(), m[3].value), ("b", 2.0));
}

#[test]
fn sts_temporary_credentials_are_verified_with_their_session_token() {
    let mock = AwsMock::start().unwrap();
    mock.create_queue("q");
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-sts://default?{}", mock.endpoint_options());
    let creds = t
        .request(&uri, |ex| {
            ex.set_header("CamelAwsStsRoleArn", "arn:aws:iam::000000000000:role/reader");
            ex.set_header("CamelAwsStsRoleSessionName", "camel");
        })
        .unwrap()
        .body()
        .clone();
    let ak = s(creds.get("accessKeyId").unwrap());
    assert!(ak.starts_with("ASIA"));
    assert_eq!(s(creds.get("assumedRoleArn").unwrap()), "arn:aws:sts::000000000000:assumed-role/reader/camel");
    let sqs = |token: &str| {
        format!(
            "aws2-sqs://q?accessKey=RAW({})&secretKey=RAW({})&sessionToken=RAW({})&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true",
            ak,
            s(creds.get("secretAccessKey").unwrap()),
            token,
            mock.url()
        )
    };
    t.request_body(&sqs(&s(creds.get("sessionToken").unwrap())), "with temporary credentials").unwrap();
    assert_eq!(mock.queue_messages("q"), vec!["with temporary credentials"]);
    let e = err_of(t.request_body(&sqs("forged-token"), "x"));
    assert!(e.message.contains("InvalidClientTokenId"), "{}", e);
    // other operations
    let st = t.request_body_and_header(&uri, "", "CamelAwsStsOperation", "getSessionToken").unwrap();
    assert!(s(st.get("sessionToken").unwrap()).len() > 10);
    let ft = t
        .request(&uri, |ex| {
            ex.set_header("CamelAwsStsOperation", "getFederationToken");
            ex.set_header("CamelAwsStsFederatedName", "bob");
        })
        .unwrap();
    assert_eq!(s(ft.body().get("federatedUserArn").unwrap()), "arn:aws:sts::000000000000:federated-user/bob");
    let denied = t
        .request(&uri, |ex| {
            ex.set_header("CamelAwsStsRoleArn", "arn:aws:iam::999999999999:role/x");
            ex.set_header("CamelAwsStsRoleSessionName", "s");
        })
        .unwrap_err()
        .root_cause()
        .clone();
    assert!(denied.is_a("StsException"), "{}", denied);
}

#[test]
fn step_functions_state_machines_and_executions() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-step-functions://sfn?{}", mock.endpoint_options());
    let def = r#"{"StartAt":"Hello","States":{"Hello":{"Type":"Pass","Result":{"greeting":"hi"},"Next":"Done"},"Done":{"Type":"Succeed"}}}"#;
    let arn = t
        .request(&format!("{}&operation=createStateMachine", uri), |ex| {
            ex.set_header("CamelAwsStepFunctionsStateMachineName", "hello");
            ex.set_header("CamelAwsStepFunctionsStateMachineDefinition", def);
            ex.set_header("CamelAwsStepFunctionsStateMachineRoleArn", "arn:aws:iam::000000000000:role/sfn");
        })
        .unwrap()
        .body()
        .to_string();
    assert!(arn.ends_with(":stateMachine:hello"));
    let ex = t
        .request(&format!("{}&operation=startExecution", uri), |ex| {
            ex.set_body(Value::object([("x", 1)]));
            ex.set_header("CamelAwsStepFunctionsStateMachineArn", arn.as_str());
            ex.set_header("CamelAwsStepFunctionsExecutionName", "run-1");
        })
        .unwrap();
    let exec = s(ex.header("CamelAwsStepFunctionsExecutionArn").unwrap());
    assert!(exec.ends_with(":execution:hello:run-1"));
    let d = t.request_body_and_header(&format!("{}&operation=describeExecution", uri), "", "CamelAwsStepFunctionsExecutionArn", exec.as_str()).unwrap();
    assert_eq!(s(d.get("status").unwrap()), "SUCCEEDED");
    assert_eq!(s(d.get("output").unwrap()), r#"{"greeting":"hi"}"#);
    // a failing machine, run synchronously
    let fail_arn = t
        .request(&format!("{}&operation=createStateMachine", uri), |ex| {
            ex.set_body(Value::object([("StartAt", Value::from("F")), ("States", Value::object([("F", Value::object([("Type", "Fail"), ("Error", "Boom"), ("Cause", "bad input")]))]))]));
            ex.set_header("CamelAwsStepFunctionsStateMachineName", "failing");
            ex.set_header("CamelAwsStepFunctionsStateMachineRoleArn", "arn:aws:iam::000000000000:role/sfn");
        })
        .unwrap()
        .body()
        .to_string();
    let r = t.request_body_and_header(&format!("{}&operation=startSyncExecution", uri), "{}", "CamelAwsStepFunctionsStateMachineArn", fail_arn.as_str()).unwrap();
    assert_eq!((s(r.get("status").unwrap()), s(r.get("error").unwrap())), ("FAILED".to_string(), "Boom".to_string()));
    let machines = t.request_body(&format!("{}&operation=listStateMachines", uri), "").unwrap();
    assert_eq!(machines.as_array().unwrap().len(), 2);
    let execs = t.request_body_and_header(&format!("{}&operation=listExecutions", uri), "", "CamelAwsStepFunctionsStateMachineArn", arn.as_str()).unwrap();
    assert_eq!(execs.as_array().unwrap().len(), 1);
    let hist = t.request_body_and_header(&format!("{}&operation=getExecutionHistory", uri), "", "CamelAwsStepFunctionsExecutionArn", exec.as_str()).unwrap();
    assert_eq!(hist.as_array().unwrap().len(), 2);
    // duplicate execution name
    let e = err_of(t.request(&format!("{}&operation=startExecution", uri), |ex| {
        ex.set_header("CamelAwsStepFunctionsStateMachineArn", arn.as_str());
        ex.set_header("CamelAwsStepFunctionsExecutionName", "run-1");
    }).map(|e| e.body().clone()));
    assert!(e.is_a("SfnException") && e.message.contains("ExecutionAlreadyExists"), "{}", e);
    t.request_body_and_header(&format!("{}&operation=deleteStateMachine", uri), "", "CamelAwsStepFunctionsStateMachineArn", fail_arn.as_str()).unwrap();
    assert!(t.request_body(&format!("{}", uri), "").is_err(), "operation is required");
}

#[test]
fn iam_users_groups_and_access_keys() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-iam://iam?{}", mock.endpoint_options());
    let op = |o: &str, user: Option<&str>, group: Option<&str>| {
        t.request(&format!("{}&operation={}", uri, o), |ex| {
            if let Some(u) = user {
                ex.set_header("CamelAwsIAMUsername", u);
            }
            if let Some(g) = group {
                ex.set_header("CamelAwsIAMGroupName", g);
            }
        })
        .map(|e| e.body().clone())
    };
    let u = op("createUser", Some("alice"), None).unwrap();
    assert_eq!(s(u.get("arn").unwrap()), "arn:aws:iam::000000000000:user/alice");
    op("createUser", Some("bob"), None).unwrap();
    let e = op("createUser", Some("bob"), None).unwrap_err().root_cause().clone();
    assert!(e.is_a("IamException") && e.message.contains("EntityAlreadyExists"), "{}", e);
    let users = op("listUsers", None, None).unwrap();
    let names: Vec<String> = users.as_array().unwrap().iter().map(|u| s(u.get("userName").unwrap())).collect();
    assert_eq!(names, vec!["alice", "bob"]);
    assert_eq!(s(op("getUser", Some("alice"), None).unwrap().get("userName").unwrap()), "alice");
    op("createGroup", None, Some("admins")).unwrap();
    op("addUserToGroup", Some("alice"), Some("admins")).unwrap();
    assert_eq!(op("listGroups", None, None).unwrap().as_array().unwrap().len(), 1);
    let key = op("createAccessKey", Some("alice"), None).unwrap();
    assert!(s(key.get("accessKeyId").unwrap()).starts_with("AKIA"));
    // the new access key is accepted by the emulator
    let ak = s(key.get("accessKeyId").unwrap());
    let sk = s(key.get("secretAccessKey").unwrap());
    let other = format!("aws2-iam://iam?operation=listAccessKeys&accessKey=RAW({})&secretKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ak, sk, mock.url());
    let keys = t.request_body(&other, "").unwrap();
    assert_eq!(keys.as_array().unwrap().len(), 1);
    op("deleteUser", Some("bob"), None).unwrap();
    assert_eq!(mock.iam_users(), vec!["alice"]);
    let e = op("getUser", Some("bob"), None).unwrap_err().root_cause().clone();
    assert!(e.message.contains("NoSuchEntity"), "{}", e);
}

#[test]
fn ec2_instances_lifecycle() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-ec2://ec2?{}", mock.endpoint_options());
    let started = t
        .request(&format!("{}&operation=createAndRunInstances", uri), |ex| {
            ex.set_header("CamelAwsEC2ImageId", "ami-12345678");
            ex.set_header("CamelAwsEC2InstanceType", "t3.micro");
            ex.set_header("CamelAwsEC2InstanceMinCount", 2);
            ex.set_header("CamelAwsEC2InstanceMaxCount", 2);
        })
        .unwrap()
        .body()
        .clone();
    let ids: Vec<String> = started.as_array().unwrap().iter().map(|i| s(i.get("instanceId").unwrap())).collect();
    assert_eq!(ids.len(), 2);
    let all = t.request_body(&format!("{}&operation=describeInstances", uri), "").unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    let one = t.request_body_and_header(&format!("{}&operation=describeInstances", uri), "", "CamelAwsEC2InstancesIds", Value::from(vec![ids[0].clone()])).unwrap();
    let inst = &one.as_array().unwrap()[0];
    assert_eq!((s(inst.get("instanceType").unwrap()), s(inst.get("state").unwrap()), inst.get("stateCode").unwrap().as_i64()), ("t3.micro".to_string(), "running".to_string(), Some(16)));
    let changes = t.request_body_and_header(&format!("{}&operation=stopInstances", uri), "", "CamelAwsEC2InstancesIds", ids[0].as_str()).unwrap();
    assert_eq!(s(changes.as_array().unwrap()[0].get("currentState").unwrap()), "stopped");
    let status = t.request_body(&format!("{}&operation=describeInstancesStatus", uri), "").unwrap();
    assert_eq!(status.as_array().unwrap().len(), 1, "only running instances have a status");
    t.request_body_and_header(&format!("{}&operation=terminateInstances", uri), "", "CamelAwsEC2InstancesIds", Value::from(ids.clone())).unwrap();
    assert!(mock.ec2_instances().iter().all(|(_, st)| st == "terminated"));
    let e = err_of(t.request_body_and_header(&format!("{}&operation=startInstances", uri), "", "CamelAwsEC2InstancesIds", "i-doesnotexist"));
    assert!(e.is_a("Ec2Exception") && e.message.contains("InvalidInstanceID.NotFound"), "{}", e);
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("camel-cloud-{}-{}", camel_cloud::util::random_hex(6), name))
}

#[test]
fn s3_multipart_upload_and_streaming_files() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("big");
    mock.set_min_part_size(10);
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let data: Vec<u8> = (0..35u8).collect();
    let ex = t
        .request(&format!("aws2-s3://big?multiPartUpload=true&partSize=10&{}", opts), |ex| {
            ex.set_body(Value::Bytes(data.clone()));
            ex.set_header("CamelAwsS3Key", "parts.bin");
        })
        .unwrap();
    assert!(s(ex.header("CamelAwsS3ETag").unwrap()).ends_with("-4\""), "{:?}", ex.header("CamelAwsS3ETag"));
    assert_eq!(mock.object("big", "parts.bin").unwrap(), data);
    assert_eq!(mock.multipart_stats(), (0, 1));
    // below partSize: a plain PUT
    t.request(&format!("aws2-s3://big?multiPartUpload=true&partSize=100&{}", opts), |ex| {
        ex.set_body(Value::Bytes(data.clone()));
        ex.set_header("CamelAwsS3Key", "single.bin");
    })
    .unwrap();
    assert_eq!(mock.multipart_stats(), (0, 1));
    // part too small for the server: the upload is aborted
    mock.set_min_part_size(20);
    let e = err_of(t.request_body_and_header(&format!("aws2-s3://big?multiPartUpload=true&partSize=10&{}", opts), Value::Bytes(data.clone()), "CamelAwsS3Key", "small-parts.bin"));
    assert!(e.message.contains("EntityTooSmall"), "{}", e);
    assert_eq!(mock.multipart_stats(), (0, 1), "aborted upload removed");
    assert!(mock.object("big", "small-parts.bin").is_none());
    mock.set_min_part_size(10);

    // streaming upload from a file (multipart, one part in memory at a time) and plain streamed PUT
    let src = temp_path("src.bin");
    let big: Vec<u8> = (0..1000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(&src, &big).unwrap();
    let ex = t
        .request(&format!("aws2-s3://big?multiPartUpload=true&partSize=300&{}", opts), |ex| {
            ex.set_header("CamelAwsS3Key", "from-file.bin");
            ex.set_header("CamelAwsS3SourceFile", src.to_string_lossy().to_string());
        })
        .unwrap();
    assert_eq!(ex.header("CamelAwsS3ContentLength").unwrap().as_i64(), Some(1000));
    assert_eq!(mock.object("big", "from-file.bin").unwrap(), big);
    assert_eq!(mock.multipart_stats(), (0, 2));
    t.request(&format!("aws2-s3://big?{}", opts), |ex| {
        ex.set_header("CamelAwsS3Key", "streamed.bin");
        ex.set_header("CamelAwsS3SourceFile", src.to_string_lossy().to_string());
    })
    .unwrap();
    assert_eq!(mock.object("big", "streamed.bin").unwrap(), big);
    // streaming download into a file
    let dst = temp_path("dst.bin");
    let ex = t
        .request(&format!("aws2-s3://big?operation=getObject&{}", opts), |ex| {
            ex.set_header("CamelAwsS3Key", "from-file.bin");
            ex.set_header("CamelAwsS3DownloadFile", dst.to_string_lossy().to_string());
        })
        .unwrap();
    assert_eq!(s(ex.body()), dst.to_string_lossy());
    assert_eq!(ex.header("CamelAwsS3ContentLength").unwrap().as_i64(), Some(1000));
    assert_eq!(std::fs::read(&dst).unwrap(), big);
    let missing = temp_path("missing.bin");
    assert!(t
        .request(&format!("aws2-s3://big?operation=getObject&{}", opts), |ex| {
            ex.set_header("CamelAwsS3Key", "nope");
            ex.set_header("CamelAwsS3DownloadFile", missing.to_string_lossy().to_string());
        })
        .is_err());
    assert!(!missing.exists() && !std::path::Path::new(&format!("{}.part", missing.display())).exists());
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&dst);
    assert_eq!(mock.auth_failures(), 0);
}

#[test]
fn s3_streaming_upload_mode() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("logs");
    mock.set_min_part_size(10);
    let opts = mock.endpoint_options();
    let ctx = context();
    let o2 = opts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:log").to(&format!("aws2-s3://logs?streamingUploadMode=true&keyName=app.log&batchMessageNumber=3&partSize=10&{}", o2));
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for i in 0..7 {
        t.send_body("direct:log", format!("line-{}\n", i)).unwrap();
    }
    assert_eq!(mock.object("logs", "app.log").unwrap(), b"line-0\nline-1\nline-2\n");
    assert_eq!(mock.object("logs", "app-1.log").unwrap(), b"line-3\nline-4\nline-5\n");
    assert!(mock.object("logs", "app-2.log").is_none());
    // stopping the producer completes the pending object
    ctx.stop().unwrap();
    assert_eq!(mock.object("logs", "app-2.log").unwrap(), b"line-6\n");
    assert!(mock.multipart_stats().1 >= 2);

    // restartingPolicy=lastPart continues the numbering; the timeout completes idle objects
    let ctx = context();
    let o3 = opts.clone();
    ctx.add_routes(move |b| {
        b.from("direct:log").to(&format!(
            "aws2-s3://logs?streamingUploadMode=true&keyName=app.log&batchMessageNumber=100&restartingPolicy=lastPart&streamingUploadTimeout=100&{}",
            o3
        ));
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body("direct:log", "after restart").unwrap();
    wait_until("timeout completion", || mock.object("logs", "app-3.log").is_some());
    assert_eq!(mock.object("logs", "app-3.log").unwrap(), b"after restart");
    ctx.stop().unwrap();
    assert!(context().get_endpoint(&format!("aws2-s3://logs?streamingUploadMode=true&{}", opts)).is_err(), "keyName is required");
}

#[test]
fn sqs_extend_message_visibility_while_processing() {
    let mock = AwsMock::start().unwrap();
    mock.create_queue("slow");
    mock.send_sqs("slow", "takes a while");
    let ctx = context();
    let opts = mock.endpoint_options();
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-sqs://slow?visibilityTimeout=1&extendMessageVisibility=true&delay=20&initialDelay=0&{}", opts))
            .process(|_ex: &mut Exchange| {
                std::thread::sleep(Duration::from_millis(1300));
                Ok(())
            })
            .to("mock:slow");
    })
    .unwrap();
    let slow = ctx.mock_endpoint("mock:slow").unwrap();
    slow.expected_message_count(1);
    slow.set_result_wait_time(Duration::from_secs(10));
    ctx.start().unwrap();
    slow.assert_is_satisfied().unwrap();
    wait_until("message deleted", || mock.queue_messages("slow").is_empty());
    ctx.stop().unwrap();
    assert!(mock.visibility_changes() >= 1, "visibility extended {} times", mock.visibility_changes());
    assert_eq!(slow.received_counter(), 1, "not redelivered");
    // the option requires a visibility timeout
    assert!(context().get_endpoint(&format!("aws2-sqs://slow?extendMessageVisibility=true&{}", mock.endpoint_options())).is_err());
}
