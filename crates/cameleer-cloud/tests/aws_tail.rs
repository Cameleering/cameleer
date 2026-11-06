//! Remaining AWS features end-to-end against the SigV4-verifying emulator: the SQS Query
//! protocol fallback, the complete `aws2-lambda` operation list, DynamoDB expressions /
//! BatchGetItems / UpdateTable, the `aws2-ddbstream` consumer and the Secrets Manager refresh
//! task (CloudTrail driven context reload).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_cloud::aws::secretsmanager::{refresh_task, SecretsRefreshTask};
use cameleer_cloud::testing::AwsMock;
use cameleer_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let body = body.into();
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    let ex = t
        .request(uri, move |ex| {
            ex.set_body(body.clone());
            for (k, v) in &hs {
                ex.set_header(k.clone(), v.clone());
            }
        })
        .unwrap_or_else(|e| panic!("{}: {:?}", uri, e));
    assert!(ex.exception.is_none(), "{}: {:?}", uri, ex.exception);
    ex
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

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

// ----------------------------------------------------------------------------- SQS

#[test]
fn sqs_falls_back_to_the_query_protocol() {
    let mock = AwsMock::start().unwrap();
    mock.set_sqs_query_only(true);
    mock.create_queue("legacy");
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    // auto: JSON rejected with InvalidAction -> Query
    let ex = req(&t, &format!("aws2-sqs://legacy?{}", opts), "hello <query> & co", &[("color", Value::from("blue")), ("n", Value::Int(3))]);
    assert!(!ex.header("CamelAwsSqsMessageId").unwrap().to_string().is_empty());
    assert_eq!(mock.queue_messages("legacy"), vec!["hello <query> & co".to_string()]);
    let attrs = &mock.queue_message_attributes("legacy")[0];
    assert_eq!(attrs.get("color").map(|s| s.as_str()), Some("blue"));
    let batch = req(&t, &format!("aws2-sqs://legacy?operation=sendBatchMessage&{}", opts), Value::from(vec!["a", "b"]), &[]);
    assert_eq!(batch.body().get("successful").unwrap().as_array().unwrap().len(), 2);
    let queues = req(&t, &format!("aws2-sqs://legacy?operation=listQueues&{}", opts), "", &[]);
    assert!(queues.body().to_json().contains("legacy"));
    // consumer over Query (receive + delete)
    ctx.add_routes({
        let opts = opts.clone();
        move |b| {
            b.from(&format!("aws2-sqs://legacy?protocol=query&maxMessagesPerPoll=10&delay=10&initialDelay=0&{}", opts)).to("mock:legacy");
        }
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:legacy").unwrap();
    m.expected_message_count(3);
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let first = &m.received_exchanges()[0];
    assert_eq!(first.body().to_string(), "hello <query> & co");
    assert_eq!(first.header("color").unwrap().to_string(), "blue");
    assert_eq!(first.header("n").unwrap(), &Value::Int(3));
    ctx.stop().unwrap();
    assert!(mock.sqs_query_requests() >= 5);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !mock.queue_messages("legacy").is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(mock.queue_messages("legacy").is_empty(), "processed messages are deleted");
    // protocol=json does not fall back
    let e = req_err(&t, &format!("aws2-sqs://legacy?protocol=json&{}", opts), "x", &[]);
    assert!(e.message.contains("InvalidAction"), "{}", e.message);
    // bad signature over Query
    let bad = format!("accessKey=RAW({})&secretKey=RAW(wrong)&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true&protocol=query", AwsMock::ACCESS_KEY, mock.url());
    assert!(req_err(&t, &format!("aws2-sqs://legacy?{}", bad), "x", &[]).message.contains("SignatureDoesNotMatch"));
    // errors are mapped from the XML error response
    let e = req_err(&t, &format!("aws2-sqs://missing?protocol=query&{}", opts), "x", &[]);
    assert!(e.message.contains("NonExistentQueue"), "{}", e.message);
}

// ----------------------------------------------------------------------------- Lambda

#[test]
fn lambda_complete_operation_list() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let u = |op: &str| format!("aws2-lambda://GetHelloWithName?operation={}&{}", op, mock.endpoint_options());
    let created = req(
        &t,
        &u("createFunction"),
        Value::Bytes(b"PK\x03\x04fake zip".to_vec()),
        &[
            ("CamelAwsLambdaRole", Value::from("arn:aws:iam::000000000000:role/admin")),
            ("CamelAwsLambdaRuntime", Value::from("nodejs20.x")),
            ("CamelAwsLambdaHandler", Value::from("index.handler")),
            ("CamelAwsLambdaDescription", Value::from("Hello with node.js on Lambda")),
            ("CamelAwsLambdaMemorySize", Value::Int(512)),
            ("CamelAwsLambdaEnvironmentVariables", Value::object([("GREETING", Value::from("hi"))])),
            ("CamelAwsLambdaTags", Value::object([("team", Value::from("camel"))])),
        ],
    );
    let cfg = created.body();
    assert_eq!(cfg.get("FunctionName").unwrap().to_string(), "GetHelloWithName");
    assert_eq!(cfg.get("MemorySize").unwrap(), &Value::Int(512));
    let sha1 = cfg.get("CodeSha256").unwrap().to_string();
    let arn = cfg.get("FunctionArn").unwrap().to_string();
    // missing role
    assert!(req_err(&t, &u("createFunction"), Value::Bytes(vec![1]), &[("CamelAwsLambdaRuntime", Value::from("java21"))]).message.contains("CamelAwsLambdaRole"));
    // invoke the created function (echo)
    assert_eq!(req(&t, &u("invokeFunction"), r#"{"name":"Camel"}"#, &[]).body().to_string(), r#"{"name":"Camel"}"#);
    // update code from S3 and publish
    let upd = req(&t, &u("updateFunction"), "", &[("CamelAwsLambdaS3Bucket", Value::from("code")), ("CamelAwsLambdaS3Key", Value::from("fn.zip")), ("CamelAwsLambdaPublish", Value::Bool(true))]);
    assert_eq!(upd.body().get("Version").unwrap().to_string(), "1");
    assert_ne!(upd.body().get("CodeSha256").unwrap().to_string(), sha1);
    let v2 = req(&t, &u("publishVersion"), "", &[("CamelAwsLambdaVersionDescription", Value::from("second"))]);
    assert_eq!(v2.body().get("Version").unwrap().to_string(), "2");
    let versions = req(&t, &u("listVersions"), "", &[]);
    assert_eq!(versions.body().as_array().unwrap().len(), 3);
    // aliases
    let alias = req(&t, &u("createAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("prod")), ("CamelAwsLambdaFunctionVersion", Value::from("1")), ("CamelAwsLambdaAliasFunctionDescription", Value::from("production"))]);
    assert!(alias.body().get("AliasArn").unwrap().to_string().ends_with(":prod"));
    assert!(req_err(&t, &u("createAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("bad")), ("CamelAwsLambdaFunctionVersion", Value::from("9"))]).message.contains("ResourceNotFoundException"));
    assert_eq!(req(&t, &u("getAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("prod"))]).body().get("FunctionVersion").unwrap().to_string(), "1");
    assert_eq!(req(&t, &u("listAliases"), "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &u("deleteAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("prod"))]);
    assert!(req(&t, &u("listAliases"), "", &[]).body().as_array().unwrap().is_empty());
    // event source mappings
    let m = req(&t, &u("createEventSourceMapping"), "", &[("CamelAwsLambdaEventSourceArn", Value::from("arn:aws:sqs:us-east-1:000000000000:orders")), ("CamelAwsLambdaEventSourceBatchSize", Value::Int(5))]);
    let uuid = m.body().get("UUID").unwrap().to_string();
    assert_eq!(m.body().get("BatchSize").unwrap(), &Value::Int(5));
    assert_eq!(req(&t, &u("listEventSourceMapping"), "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &u("deleteEventSourceMapping"), "", &[("CamelAwsLambdaEventSourceUuid", Value::from(uuid))]);
    assert!(mock.lambda_event_source_mappings().is_empty());
    // tags
    assert_eq!(req(&t, &u("listTags"), "", &[("CamelAwsLambdaResourceArn", Value::from(arn.clone()))]).body().get("team").unwrap().to_string(), "camel");
    req(&t, &u("tagResource"), "", &[("CamelAwsLambdaResourceArn", Value::from(arn.clone())), ("CamelAwsLambdaResourceTags", Value::object([("env", Value::from("dev"))]))]);
    req(&t, &u("untagResource"), "", &[("CamelAwsLambdaResourceArn", Value::from(arn.clone())), ("CamelAwsLambdaResourceTagKeys", Value::from(vec!["team"]))]);
    let tags = req(&t, &u("listTags"), "", &[("CamelAwsLambdaResourceArn", Value::from(arn))]);
    assert_eq!(tags.body().to_json(), r#"{"env":"dev"}"#);
    // get / list / delete
    let got = req(&t, &u("getFunction"), "", &[]);
    assert_eq!(got.body().get("Configuration").unwrap().get("Runtime").unwrap().to_string(), "nodejs20.x");
    assert!(req(&t, &u("listFunctions"), "", &[]).body().to_json().contains("GetHelloWithName"));
    req(&t, &u("deleteFunction"), "", &[]);
    assert!(req_err(&t, &u("getFunction"), "", &[]).message.contains("ResourceNotFoundException"));
    // signature verified
    let bad = format!("aws2-lambda://x?operation=listFunctions&accessKey=RAW({})&secretKey=RAW(nope)&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true", AwsMock::ACCESS_KEY, mock.url());
    assert!(req_err(&t, &bad, "", &[]).message.contains("SignatureDoesNotMatch"));
}

// ----------------------------------------------------------------------------- DynamoDB

#[test]
fn ddb_expressions_batch_get_and_update_table() {
    let mock = AwsMock::start().unwrap();
    mock.create_table("orders", "customer", Some("id"));
    mock.create_table("users", "id", None);
    let ctx = context();
    let t = ctx.create_producer_template();
    let u = |table: &str, op: &str| format!("aws2-ddb://{}?operation={}&{}", table, op, mock.endpoint_options());
    for (c, id, total, status) in [("ann", "o1", 10, "NEW"), ("ann", "o2", 25, "SHIPPED"), ("ann", "o3", 40, "NEW"), ("bob", "o1", 5, "NEW")] {
        req(&t, &u("orders", "PutItem"), "", &[("CamelAwsDdbItem", Value::object([("customer", Value::from(c)), ("id", Value::from(id)), ("total", Value::Int(total)), ("status", Value::from(status))]))]);
    }
    // KeyConditionExpression + FilterExpression + #names
    let q = req(
        &t,
        &u("orders", "Query"),
        "",
        &[
            ("CamelAwsDdbKeyConditionExpression", Value::from("customer = :c AND id BETWEEN :a AND :b")),
            ("CamelAwsDdbFilterExpression", Value::from("#s = :new")),
            ("CamelAwsDdbExpressionAttributeNames", Value::object([("#s", Value::from("status"))])),
            ("CamelAwsDdbExpressionAttributeValues", Value::object([(":c", Value::from("ann")), (":a", Value::from("o1")), (":b", Value::from("o3")), (":new", Value::from("NEW"))])),
            ("CamelAwsDdbScanIndexForward", Value::Bool(false)),
        ],
    );
    let ids: Vec<String> = q.header("CamelAwsDdbItems").unwrap().as_array().unwrap().iter().map(|i| i.get("id").unwrap().to_string()).collect();
    assert_eq!(ids, vec!["o3", "o1"]);
    assert_eq!(q.header("CamelAwsDdbCount").unwrap(), &Value::Int(2));
    assert_eq!(q.header("CamelAwsDdbScannedCount").unwrap(), &Value::Int(3));
    // UpdateExpression with ReturnValues
    let up = req(
        &t,
        &u("orders", "UpdateItem"),
        "",
        &[
            ("CamelAwsDdbKey", Value::object([("customer", Value::from("ann")), ("id", Value::from("o1"))])),
            ("CamelAwsDdbUpdateExpression", Value::from("SET total = total + :inc, #s = :st, notes = list_append(if_not_exists(notes, :empty), :n) REMOVE legacy")),
            ("CamelAwsDdbConditionExpression", Value::from("attribute_exists(customer) AND total < :max")),
            ("CamelAwsDdbExpressionAttributeNames", Value::object([("#s", Value::from("status"))])),
            ("CamelAwsDdbExpressionAttributeValues", json(r#"{":inc":5,":st":"PAID",":empty":[],":n":["paid online"],":max":100}"#)),
            ("CamelAwsDdbReturnValues", Value::from("ALL_NEW")),
        ],
    );
    let attrs = up.header("CamelAwsDdbAttributes").unwrap();
    assert_eq!(attrs.get("total").unwrap(), &Value::Int(15));
    assert_eq!(attrs.get("status").unwrap().to_string(), "PAID");
    assert_eq!(attrs.get("notes").unwrap(), &Value::from(vec!["paid online"]));
    // failing condition
    let e = req_err(
        &t,
        &u("orders", "UpdateItem"),
        "",
        &[
            ("CamelAwsDdbKey", Value::object([("customer", Value::from("ann")), ("id", Value::from("o1"))])),
            ("CamelAwsDdbUpdateExpression", Value::from("SET total = :z")),
            ("CamelAwsDdbConditionExpression", Value::from("total > :max")),
            ("CamelAwsDdbExpressionAttributeValues", json(r#"{":z":0,":max":100}"#)),
        ],
    );
    assert!(e.message.contains("ConditionalCheckFailedException"), "{}", e.message);
    // conditional put (attribute_not_exists)
    let e = req_err(
        &t,
        &u("orders", "PutItem"),
        "",
        &[("CamelAwsDdbItem", Value::object([("customer", Value::from("bob")), ("id", Value::from("o1"))])), ("CamelAwsDdbConditionExpression", Value::from("attribute_not_exists(id)"))],
    );
    assert!(e.message.contains("ConditionalCheckFailedException"));
    // Scan with filter + projection
    let sc = req(
        &t,
        &u("orders", "Scan"),
        "",
        &[("CamelAwsDdbFilterExpression", Value::from("total >= :min")), ("CamelAwsDdbProjectionExpression", Value::from("id, total")), ("CamelAwsDdbExpressionAttributeValues", json(r#"{":min":20}"#))],
    );
    let items = sc.header("CamelAwsDdbItems").unwrap().as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i.get("customer").is_none() && i.get("total").is_some()));
    // BatchGetItems across tables
    req(&t, &u("users", "PutItem"), "", &[("CamelAwsDdbItem", Value::object([("id", Value::from("ann")), ("name", Value::from("Ann"))]))]);
    let bg = req(
        &t,
        &u("orders", "BatchGetItems"),
        "",
        &[(
            "CamelAwsDdbBatchItems",
            Value::object([
                ("orders", Value::object([("Keys", Value::Array(vec![Value::object([("customer", Value::from("ann")), ("id", Value::from("o2"))]), Value::object([("customer", Value::from("zed")), ("id", Value::from("x"))])]))])),
                ("users", Value::object([("Keys", Value::Array(vec![Value::object([("id", Value::from("ann"))])]))])),
            ]),
        )],
    );
    let resp = bg.header("CamelAwsDdbBatchResponse").unwrap();
    assert_eq!(resp.get("orders").unwrap().as_array().unwrap()[0].get("total").unwrap(), &Value::Int(25));
    assert_eq!(resp.get("users").unwrap().as_array().unwrap()[0].get("name").unwrap().to_string(), "Ann");
    // UpdateTable throughput + stream
    let ut = req(&t, &u("orders", "UpdateTable"), "", &[("CamelAwsDdbReadCapacity", Value::Int(7)), ("CamelAwsDdbWriteCapacity", Value::Int(3)), ("CamelAwsDdbStreamViewType", Value::from("NEW_AND_OLD_IMAGES"))]);
    assert_eq!(ut.header("CamelAwsDdbTableStatus").unwrap().to_string(), "ACTIVE");
    assert_eq!(ut.header("CamelAwsDdbProvisionedThroughput").unwrap().get("ReadCapacityUnits").unwrap(), &Value::Int(7));
    assert!(ut.header("CamelAwsDdbStreamArn").unwrap().to_string().contains("table/orders/stream/"));
    let d = req(&t, &u("orders", "DescribeTable"), "", &[]);
    assert!(d.header("CamelAwsDdbStreamArn").is_some());
    // invalid expression
    let e = req_err(&t, &u("orders", "Query"), "", &[("CamelAwsDdbKeyConditionExpression", Value::from("customer = :undefined"))]);
    assert!(e.message.contains("ValidationException"));
}

#[test]
fn ddbstream_consumer_reads_item_changes() {
    let mock = AwsMock::start().unwrap();
    mock.create_table("events", "id", None);
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    // no stream yet
    let u = |op: &str| format!("aws2-ddb://events?operation={}&{}", op, opts);
    mock.enable_ddb_stream("events", "NEW_AND_OLD_IMAGES");
    req(&t, &u("PutItem"), "", &[("CamelAwsDdbItem", Value::object([("id", Value::from("a")), ("v", Value::Int(1))]))]);
    ctx.add_routes({
        let opts = opts.clone();
        move |b| {
            b.from(&format!("aws2-ddbstream://events?streamIteratorType=FROM_START&delay=10&initialDelay=0&{}", opts)).to("mock:stream");
            b.from(&format!("aws2-ddbstream://events?delay=10&initialDelay=0&{}", opts)).to("mock:latest");
        }
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:stream").unwrap();
    m.expected_message_count(4);
    let latest = ctx.mock_endpoint("mock:latest").unwrap();
    ctx.start().unwrap();
    // FROM_LATEST positions itself with its first GetShardIterator; write only after that, or a
    // slow start (a loaded machine) lets it miss the first changes.
    let deadline = Instant::now() + Duration::from_secs(10);
    while mock.ddb_latest_iterator_requests() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(mock.ddb_latest_iterator_requests() > 0, "the FROM_LATEST consumer never asked for a shard iterator");
    req(&t, &u("UpdateItem"), "", &[("CamelAwsDdbKey", Value::object([("id", Value::from("a"))])), ("CamelAwsDdbUpdateExpression", Value::from("SET v = v + :one")), ("CamelAwsDdbExpressionAttributeValues", json(r#"{":one":1}"#))]);
    req(&t, &u("PutItem"), "", &[("CamelAwsDdbItem", Value::object([("id", Value::from("b")), ("v", Value::Int(9))]))]);
    req(&t, &u("DeleteItem"), "", &[("CamelAwsDdbKey", Value::object([("id", Value::from("a"))]))]);
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    let rx = m.received_exchanges();
    let names: Vec<String> = rx.iter().map(|e| e.header("CamelAwsDdbStreamEventName").unwrap().to_string()).collect();
    assert_eq!(names, vec!["INSERT", "MODIFY", "INSERT", "REMOVE"]);
    let modify = rx[1].body().get("dynamodb").unwrap().clone();
    assert_eq!(modify.get("OldImage").unwrap().get("v").unwrap(), &Value::Int(1));
    assert_eq!(modify.get("NewImage").unwrap().get("v").unwrap(), &Value::Int(2));
    assert_eq!(modify.get("Keys").unwrap().get("id").unwrap().to_string(), "a");
    let deadline = Instant::now() + Duration::from_secs(5);
    while latest.received_exchanges().len() < 3 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    // FROM_LATEST skips the record written before the consumer started
    let ln: Vec<String> = latest.received_exchanges().iter().map(|e| e.header("CamelAwsDdbStreamEventName").unwrap().to_string()).collect();
    assert_eq!(ln, vec!["MODIFY", "INSERT", "REMOVE"]);
    ctx.stop().unwrap();
    assert_eq!(mock.ddb_stream_records("events").len(), 4);
    // a table without stream fails the consumer poll; FROM_SEQUENCE_NUMBER needs a provider
    let c2 = context();
    assert!(c2.get_endpoint(&format!("aws2-ddbstream://events?streamIteratorType=FROM_SEQUENCE_NUMBER&{}", opts)).is_err());
}

// ----------------------------------------------------------------------------- Secrets refresh

fn vault_props(ctx: &CamelContext, mock: &AwsMock) {
    let p = ctx.properties();
    p.set("camel.vault.aws.accessKey", AwsMock::ACCESS_KEY);
    p.set("camel.vault.aws.secretKey", AwsMock::SECRET_KEY);
    p.set("camel.vault.aws.region", "us-east-1");
    p.set("camel.vault.aws.uriEndpointOverride", mock.url());
    p.set("camel.vault.aws.overrideEndpoint", "true");
}

#[test]
fn secret_updates_trigger_a_context_reload() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    vault_props(&ctx, &mock);
    ctx.properties().set("camel.vault.aws.refreshEnabled", "true");
    ctx.properties().set("camel.vault.aws.refreshPeriod", "50");
    ctx.properties().set("camel.vault.aws.secrets", "db*");
    let t = ctx.create_producer_template();
    let sm = format!("aws-secrets-manager://s?{}", mock.endpoint_options());
    req(&t, &format!("{}&operation=createSecret", sm), r#"{"user":"scott"}"#, &[("CamelAwsSecretsManagerSecretName", Value::from("dbCreds"))]);
    req(&t, &format!("{}&operation=createSecret", sm), "x", &[("CamelAwsSecretsManagerSecretName", Value::from("other"))]);
    ctx.add_routes(|b| {
        b.from("timer:tick?period=100000").route_id("ticker").to("mock:tick");
    })
    .unwrap();
    let task = refresh_task(&ctx).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    task.add_reload_listener(move |names| s2.lock().unwrap().extend(names.iter().cloned()));
    let started = Arc::new(Mutex::new(0usize));
    let st2 = started.clone();
    struct N(Arc<Mutex<usize>>);
    impl cameleer_core::spi::EventNotifier for N {
        fn notify(&self, e: &cameleer_core::context::CamelEvent) {
            if matches!(e, cameleer_core::context::CamelEvent::RouteStarted(id) if id == "ticker") {
                *self.0.lock().unwrap() += 1;
            }
        }
    }
    ctx.add_event_notifier(N(st2));
    ctx.start().unwrap();
    assert!(task.is_enabled());
    assert_eq!(*started.lock().unwrap(), 1);
    // an unwatched secret does not reload
    req(&t, &format!("{}&operation=updateSecret", sm), "y", &[("CamelAwsSecretsManagerSecretId", Value::from("other"))]);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(task.reload_count(), 0);
    // the watched one does (CloudTrail event recorded by the emulator)
    req(&t, &format!("{}&operation=updateSecret", sm), r#"{"user":"tiger"}"#, &[("CamelAwsSecretsManagerSecretId", Value::from("dbCreds"))]);
    let deadline = Instant::now() + Duration::from_secs(10);
    while task.reload_count() == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(task.reload_count(), 1);
    assert_eq!(*seen.lock().unwrap(), vec!["dbCreds".to_string()]);
    assert_eq!(*started.lock().unwrap(), 2, "the route was restarted");
    assert_eq!(ctx.resolve_placeholders("{{aws:dbCreds/user}}").unwrap(), "tiger");
    ctx.stop().unwrap();
    assert!(!task.is_enabled());
}

#[test]
fn refresh_watches_resolved_secrets_by_default() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    vault_props(&ctx, &mock);
    let t = ctx.create_producer_template();
    let sm = format!("aws-secrets-manager://s?{}", mock.endpoint_options());
    for n in ["apiKey", "unused"] {
        req(&t, &format!("{}&operation=createSecret", sm), "v1", &[("CamelAwsSecretsManagerSecretName", Value::from(n))]);
    }
    assert_eq!(ctx.resolve_placeholders("{{aws:apiKey}}").unwrap(), "v1");
    let task = SecretsRefreshTask::install(&ctx);
    assert!(!task.is_enabled(), "refreshEnabled not set");
    assert!(task.check_now().unwrap().is_empty());
    req(&t, &format!("{}&operation=updateSecret", sm), "v2", &[("CamelAwsSecretsManagerSecretId", Value::from("unused"))]);
    assert!(task.check_now().unwrap().is_empty());
    req(&t, &format!("{}&operation=updateSecret", sm), "v2", &[("CamelAwsSecretsManagerSecretId", Value::from("apiKey"))]);
    assert_eq!(task.check_now().unwrap(), vec!["apiKey".to_string()]);
    // each event is handled once
    assert!(task.check_now().unwrap().is_empty());
    assert_eq!(task.reload_count(), 1);
    // wrong vault credentials make the check fail
    ctx.properties().set("camel.vault.aws.secretKey", "wrong");
    assert!(task.check_now().is_err());
}
