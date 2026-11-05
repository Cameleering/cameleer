//! Remaining Camel AWS capabilities end-to-end against the SigV4-verifying emulator:
//! Bedrock streaming (event stream framing) / guardrails / agents.

use std::sync::{Arc, Mutex};

use cameleer_cloud::endpoint::{CloudEndpoint, CloudService};
use cameleer_cloud::testing::AwsMock;
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
fn bedrock_streaming_operations_decode_event_streams() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    // Titan streaming, complete mode
    let ex = req(&t, &format!("aws-bedrock://b?modelId=amazon.titan-text-express-v1&operation=invokeTextModelStreaming&{}", opts), Value::object([("inputText", "tell me a story")]), &[]);
    assert_eq!(s(ex.body()), "Echo: tell me a story");
    assert_eq!(ex.header("CamelAwsBedrockStreamingChunkCount").unwrap().as_i64(), Some(5));
    assert_eq!(s(ex.header("CamelAwsBedrockStreamingCompletionReason").unwrap()), "FINISH");
    assert_eq!(ex.header("CamelAwsBedrockStreamingTokenCount").unwrap().as_i64(), Some(5));
    // Anthropic messages streaming, chunks mode, with a chunk processor observing each piece
    let seen: Arc<Mutex<Vec<(i64, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    ctx.registry().bind_processor("onChunk", move |ex: &mut Exchange| {
        seen2.lock().unwrap().push((ex.header("CamelAwsBedrockStreamingChunkIndex").and_then(|v| v.as_i64()).unwrap(), ex.body().to_string()));
        Ok(())
    });
    let body = Value::object([("anthropic_version", Value::from("bedrock-2023-05-31")), ("max_tokens", Value::Int(50)), ("messages", Value::Array(vec![Value::object([("role", "user"), ("content", "hi there")])]))]);
    let ex = req(&t, &format!("aws-bedrock://b?modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=invokeTextModelStreaming&streamOutputMode=chunks&streamingChunkProcessor=#onChunk&{}", opts), body, &[]);
    let chunks: Vec<String> = ex.body().as_array().unwrap().iter().map(s).collect();
    assert_eq!(chunks, vec!["Echo: ", "hi ", "there"]);
    assert_eq!(s(ex.header("CamelAwsBedrockStreamingCompletionReason").unwrap()), "end_turn");
    assert_eq!(seen.lock().unwrap().clone(), vec![(0, "Echo: ".to_string()), (1, "hi ".to_string()), (2, "there".to_string())]);
    // converseStream
    let ex = req(&t, &format!("aws-bedrock://b?modelId=anthropic.claude-3-haiku-20240307-v1:0&operation=converseStream&{}", opts), "stream this answer", &[]);
    assert_eq!(s(ex.body()), "Echo: stream this answer");
    assert_eq!(s(ex.header("CamelAwsBedrockConverseStopReason").unwrap()), "end_turn");
    assert_eq!(ex.header("CamelAwsBedrockConverseUsage").unwrap().get("outputTokens").unwrap().as_i64(), Some(4));
    assert_eq!(s(ex.header("CamelAwsBedrockConverseOutputMessage").unwrap().get("role").unwrap()), "assistant");
    // the output mode header overrides the option
    let ex = req(&t, &format!("aws-bedrock://b?modelId=m1&operation=converseStream&{}", opts), "a b", &[("CamelAwsBedrockStreamOutputMode", Value::from("chunks"))]);
    assert_eq!(ex.body().as_array().unwrap().len(), 3);
    // a mid-stream exception message fails the exchange; an unknown model fails before streaming
    let e = req_err(&t, &format!("aws-bedrock://b?modelId=m1&operation=converseStream&{}", opts), "throttle-me", &[]);
    assert!(e.message.contains("throttlingException"), "{}", e);
    let e = req_err(&t, &format!("aws-bedrock://b?modelId=missing&operation=invokeTextModelStreaming&{}", opts), Value::object([("inputText", "x")]), &[]);
    assert!(e.message.contains("ResourceNotFoundException"), "{}", e);
    assert!(req_err(&t, &format!("aws-bedrock://b?streamOutputMode=bogus&{}", opts), "x", &[]).message.contains("streamOutputMode"));
}

#[test]
fn bedrock_guardrails_and_agents() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let uri = format!("aws-bedrock://b?operation=applyGuardrail&guardrailIdentifier=gr-1&guardrailVersion=1&{}", opts);
    let ex = req(&t, &uri, "a harmless question", &[]);
    assert_eq!(s(ex.header("CamelAwsBedrockGuardrailAction").unwrap()), "NONE");
    let ex = req(&t, &uri, "something forbidden", &[("CamelAwsBedrockGuardrailSource", Value::from("OUTPUT"))]);
    assert_eq!(s(ex.header("CamelAwsBedrockGuardrailAction").unwrap()), "GUARDRAIL_INTERVENED");
    assert_eq!(s(ex.body().get("outputs").unwrap().as_array().unwrap()[0].get("text").unwrap()), "Sorry, I cannot help with that.");
    assert!(req_err(&t, &format!("aws-bedrock://b?operation=applyGuardrail&{}", opts), "x", &[]).message.contains("guardrailIdentifier"));
    // guardrail settings travel with converse requests
    req(&t, &format!("aws-bedrock://b?modelId=m1&operation=converse&guardrailIdentifier=gr-1&guardrailTrace=true&{}", opts), "hello", &[]);
    let (_, sent) = mock.bedrock_requests().pop().unwrap();
    assert_eq!(s(sent.get("guardrailConfig").unwrap().get("trace").unwrap()), "enabled");
    assert_eq!(s(sent.get("guardrailConfig").unwrap().get("guardrailVersion").unwrap()), "DRAFT");

    // invokeAgent
    let agent = format!("aws-bedrock-agent-runtime://a?operation=invokeAgent&agentId=AG1&agentAliasId=TSTALIAS&{}", opts);
    let ex = req(&t, &agent, "what is the weather?", &[("CamelAwsBedrockAgentRuntimeEnableTrace", Value::Bool(true))]);
    assert_eq!(s(ex.body()), "Agent AG1 answered: what is the weather?");
    let session = s(ex.header("CamelAwsBedrockAgentRuntimeSessionId").unwrap());
    assert!(!session.is_empty());
    assert_eq!(ex.header("CamelAwsBedrockAgentRuntimeTraces").unwrap().as_array().unwrap().len(), 1);
    assert_eq!(ex.header("CamelAwsBedrockAgentRuntimeCitations").unwrap().as_array().unwrap().len(), 1);
    // same session reused, with session attributes
    let ex = req(
        &t,
        &agent,
        "and tomorrow?",
        &[("CamelAwsBedrockAgentRuntimeSessionId", Value::from(session.clone())), ("CamelAwsBedrockAgentRuntimeSessionAttributes", Value::object([("city", "Paris")]))],
    );
    assert_eq!(s(ex.header("CamelAwsBedrockAgentRuntimeSessionId").unwrap()), session);
    assert!(s(ex.body()).contains("Paris"));
    assert!(ex.header("CamelAwsBedrockAgentRuntimeTraces").unwrap().as_array().unwrap().is_empty());
    let e = req_err(&t, &format!("aws-bedrock-agent-runtime://a?operation=invokeAgent&agentAliasId=X&{}", opts), "q", &[]);
    assert!(e.message.contains("agentId"), "{}", e);
    let e = req_err(&t, &format!("aws-bedrock-agent-runtime://a?operation=invokeAgent&agentId=missing&agentAliasId=X&{}", opts), "q", &[]);
    assert!(e.message.contains("ResourceNotFoundException"), "{}", e);
}

/// The service behind an endpoint (to drive consumer polls deterministically).
fn service(ctx: &CamelContext, uri: &str) -> std::sync::Arc<dyn CloudService> {
    let ep = ctx.get_endpoint(uri).unwrap();
    ep.as_any().downcast_ref::<CloudEndpoint>().unwrap().service().clone()
}

/// A processor recording bodies; fails bodies listed in `fail`.
fn recorder(seen: &Arc<Mutex<Vec<String>>>, fail: &Arc<Mutex<Vec<String>>>) -> impl Fn(&mut Exchange) -> Result<()> + Send + Sync + 'static {
    let (seen, fail) = (seen.clone(), fail.clone());
    move |ex: &mut Exchange| {
        let b = String::from_utf8_lossy(&ex.body().to_bytes()).into_owned();
        if fail.lock().unwrap().contains(&b) {
            return Err(CamelError::new("IOException", format!("cannot process {}", b)));
        }
        seen.lock().unwrap().push(b);
        Ok(())
    }
}

fn poll(svc: &std::sync::Arc<dyn CloudService>, ctx: &CamelContext, p: &(impl Fn(&mut Exchange) -> Result<()> + Send + Sync + 'static)) -> usize {
    svc.poll(Some(ctx.clone()), p, "test").unwrap()
}

#[test]
fn kinesis_kcl_leases_checkpoints_failover_and_resharding() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    mock.create_stream("orders", 2);
    for i in 0..6 {
        mock.put_kinesis_record("orders", &format!("pk-{}", i), format!("r{}", i).as_bytes()).unwrap();
    }
    let base = format!("aws2-kinesis://orders?useKclConsumers=true&applicationName=orders-app&maxResultsPerRequest=2&failoverTimeMillis=300&{}", opts);
    let w1 = service(&ctx, &format!("{}&workerIdentifier=w1", base));
    let w2 = service(&ctx, &format!("{}&workerIdentifier=w2", base));
    let (seen, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let p = recorder(&seen, &fail);
    // w1 takes its fair share first (both leases: it is alone), w2 then finds no free lease
    poll(&w1, &ctx, &p);
    poll(&w2, &ctx, &p);
    let items = mock.table_items("orders-app");
    assert_eq!(items.len(), 2, "{:?}", items);
    assert!(items.iter().all(|i| i.get("leaseOwner").map(|o| o.to_string()).as_deref() == Some("w1")), "{:?}", items);
    // checkpoints advance with the processed records
    while poll(&w1, &ctx, &p) > 0 {}
    let mut got = seen.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, vec!["r0", "r1", "r2", "r3", "r4", "r5"]);
    let items = mock.table_items("orders-app");
    assert!(items.iter().all(|i| i.get("checkpoint").map(|c| c.to_string()).unwrap_or_default().starts_with("49")), "{:?}", items);
    // a failed exchange is redelivered from the last checkpoint
    seen.lock().unwrap().clear();
    fail.lock().unwrap().push("bad".into());
    let (shard, _) = mock.put_kinesis_record("orders", "pk-x", b"bad").unwrap();
    assert_eq!(poll(&w1, &ctx, &p), 1);
    assert_eq!(poll(&w1, &ctx, &p), 1, "redelivered");
    fail.lock().unwrap().clear();
    assert_eq!(poll(&w1, &ctx, &p), 1);
    assert_eq!(seen.lock().unwrap().clone(), vec!["bad"]);
    // w1 stops renewing (crash): after the failover time w2 takes over from the checkpoints
    std::thread::sleep(std::time::Duration::from_millis(50));
    poll(&w2, &ctx, &p); // observes the counters
    std::thread::sleep(std::time::Duration::from_millis(400));
    mock.put_kinesis_record("orders", "pk-y", b"after failover").unwrap();
    seen.lock().unwrap().clear();
    poll(&w2, &ctx, &p);
    poll(&w2, &ctx, &p);
    let items = mock.table_items("orders-app");
    assert!(items.iter().all(|i| i.get("leaseOwner").map(|o| o.to_string()).as_deref() == Some("w2")), "{:?}", items);
    assert_eq!(seen.lock().unwrap().clone(), vec!["after failover"], "only the record after the checkpoint");
    assert!(items.iter().any(|i| i.get("leaseKey").map(|k| k.to_string()) == Some(shard.clone())));
    // resharding: parents are finished (SHARD_END) before the child shard is read
    mock.reshard_stream("orders", 1);
    mock.put_kinesis_record("orders", "pk-z", b"in child").unwrap();
    seen.lock().unwrap().clear();
    for _ in 0..4 {
        poll(&w2, &ctx, &p);
    }
    assert_eq!(seen.lock().unwrap().clone(), vec!["in child"]);
    let items = mock.table_items("orders-app");
    let child = items.iter().find(|i| i.get("leaseKey").map(|k| k.to_string()).as_deref() == Some("shardId-000000000002")).expect("child lease");
    assert_eq!(child.get("parentShardIds").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(items.iter().filter(|i| i.get("checkpoint").map(|c| c.to_string()).as_deref() == Some("SHARD_END")).count(), 2);
    // graceful stop releases the leases
    w2.on_stop().unwrap();
    assert!(mock.table_items("orders-app").iter().all(|i| i.get("leaseOwner").is_none()));
}

#[test]
fn kinesis_kcl_two_live_workers_share_the_shards() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    mock.create_stream("split", 4);
    let base = format!("aws2-kinesis://split?useKclConsumers=true&{}", mock.endpoint_options());
    let w1 = service(&ctx, &format!("{}&workerIdentifier=a", base));
    let w2 = service(&ctx, &format!("{}&workerIdentifier=b", base));
    let (seen, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let p = recorder(&seen, &fail);
    // w2 registers first with a lease, then w1, w2 again: 2 + 2 once both are known
    poll(&w2, &ctx, &p);
    let owners = |m: &AwsMock| m.table_items("split-camel").iter().filter_map(|i| i.get("leaseOwner").map(|o| o.to_string())).collect::<Vec<_>>();
    assert_eq!(owners(&mock).len(), 4);
    // release two leases by hand (as a rebalancing worker would) and let w1 pick them up
    w2.on_stop().unwrap();
    poll(&w1, &ctx, &p);
    poll(&w2, &ctx, &p);
    let o = owners(&mock);
    assert_eq!(o.len(), 4, "{:?}", o);
    assert_eq!(o.iter().filter(|x| *x == "a").count(), 4, "{:?}", o);
}

#[test]
fn kinesis_enhanced_fan_out_over_http2() {
    let mock = AwsMock::start().unwrap();
    mock.set_kinesis_subscription_millis(300);
    let ctx = context();
    mock.create_stream("fan", 2);
    mock.put_kinesis_record("fan", "a", b"one").unwrap();
    mock.put_kinesis_record("fan", "b", b"two").unwrap();
    let svc = service(&ctx, &format!("aws2-kinesis://fan?enhancedFanOut=true&consumerName=app1&{}", mock.endpoint_options()));
    svc.on_start().unwrap();
    let (seen, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let p = recorder(&seen, &fail);
    let wait = |n: usize| {
        let start = std::time::Instant::now();
        while seen.lock().unwrap().len() < n {
            assert!(start.elapsed() < std::time::Duration::from_secs(10), "timed out: {:?}", seen.lock().unwrap());
            poll(&svc, &ctx, &p);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    };
    wait(2);
    assert_eq!(mock.kinesis_consumers(), vec!["app1"]);
    // records pushed while subscribed, and after the subscriptions expired and were renewed
    mock.put_kinesis_record("fan", "a", b"three").unwrap();
    wait(3);
    std::thread::sleep(std::time::Duration::from_millis(500));
    mock.put_kinesis_record("fan", "b", b"four").unwrap();
    wait(4);
    assert!(mock.kinesis_subscriptions() >= 4, "{}", mock.kinesis_subscriptions());
    let mut got = seen.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, vec!["four", "one", "three", "two"]);
    // resharding: subscriptions follow the child shard
    mock.reshard_stream("fan", 1);
    mock.put_kinesis_record("fan", "c", b"five").unwrap();
    wait(5);
    svc.on_stop().unwrap();
}

#[test]
fn kinesis_cbor_protocol_and_resume_strategy() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let strategy = Arc::new(cameleer_cloud::aws::kinesis::InMemoryResumeStrategy::default());
    let s2: Arc<dyn cameleer_cloud::aws::kinesis::KinesisResumeStrategy> = strategy.clone();
    ctx.registry().bind("resume", s2);
    let uri = format!("aws2-kinesis://cb?cborEnabled=true&{}", mock.endpoint_options());
    let t = ctx.create_producer_template();
    t.request_body_and_header(&uri, "", "CamelAwsKinesisOperation", "createStream").unwrap();
    let ex = t.request(&uri, |ex| {
        ex.set_body(Value::Bytes(vec![0, 159, 255, 1]));
        ex.set_header("CamelAwsKinesisPartitionKey", "k");
    })
    .unwrap();
    let first = ex.header("CamelAwsKinesisSequenceNumber").unwrap().to_string();
    mock.put_kinesis_record("cb", "k", b"second").unwrap();
    mock.put_kinesis_record("cb", "k", b"third").unwrap();
    assert!(mock.kinesis_cbor_requests() >= 2);
    assert_eq!(mock.kinesis_records("cb")[0].1, vec![0, 159, 255, 1]);
    // resume after the first record
    strategy.set("cb", "shardId-000000000000", &first);
    let svc = service(&ctx, &format!("{}&resumeStrategy=#resume&maxResultsPerRequest=10", uri));
    let (seen, fail) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let p = recorder(&seen, &fail);
    poll(&svc, &ctx, &p);
    assert_eq!(seen.lock().unwrap().clone(), vec!["second", "third"]);
    let last = strategy.get("cb", "shardId-000000000000").unwrap();
    assert_ne!(last, first);
    // CBOR errors are decoded too
    let e = t.request_body_and_header(&format!("aws2-kinesis://nope?cborEnabled=true&{}", mock.endpoint_options()), "", "CamelAwsKinesisOperation", "describeStream").unwrap_err();
    assert!(e.root_cause().message.contains("ResourceNotFoundException"), "{}", e);
}

fn s3req(t: &ProducerTemplate, uri: &str, op: &str, key: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let mut hs: Vec<(&str, Value)> = vec![("CamelAwsS3Operation", Value::from(op)), ("CamelAwsS3Key", Value::from(key))];
    hs.extend(headers.iter().cloned());
    req(t, uri, body, &hs)
}

#[test]
fn s3_remaining_operations() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("docs");
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-s3://docs?{}", mock.endpoint_options());
    for k in ["a.txt", "b.txt", "c.txt"] {
        s3req(&t, &uri, "putObject", k, k, &[("CamelAwsS3ObjectTags", Value::object([("team", "blue")]))]);
    }
    assert_eq!(mock.s3_object_tags("docs", "a.txt"), vec![("team".to_string(), "blue".to_string())]);
    // tagging
    s3req(&t, &uri, "putObjectTagging", "a.txt", Value::object([("env", "prod"), ("owner", "me")]), &[]);
    let ex = s3req(&t, &uri, "getObjectTagging", "a.txt", "", &[]);
    assert_eq!(s(ex.body().get("env").unwrap()), "prod");
    s3req(&t, &uri, "deleteObjectTagging", "a.txt", "", &[]);
    assert!(s3req(&t, &uri, "getObjectTagging", "a.txt", "", &[]).body().as_object().unwrap().is_empty());
    // ACLs
    s3req(&t, &uri, "putObjectAcl", "a.txt", "", &[("CamelAwsS3CannedAcl", Value::from("public-read"))]);
    let ex = s3req(&t, &uri, "getObjectAcl", "a.txt", "", &[]);
    let grants = ex.body().get("grants").unwrap().as_array().unwrap().clone();
    assert_eq!(grants.len(), 2);
    assert_eq!(s(grants[1].get("permission").unwrap()), "READ");
    // object lock: legal hold and retention prevent deletes
    s3req(&t, &uri, "putObjectLegalHold", "b.txt", "", &[("CamelAwsS3ObjectLockLegalHoldStatus", Value::from("ON"))]);
    assert_eq!(s(s3req(&t, &uri, "getObjectLegalHold", "b.txt", "", &[]).body()), "ON");
    let e = req_err(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("deleteObject")), ("CamelAwsS3Key", Value::from("b.txt"))]);
    assert!(e.message.contains("AccessDenied"), "{}", e);
    s3req(&t, &uri, "putObjectLegalHold", "b.txt", "", &[("CamelAwsS3ObjectLockLegalHoldStatus", Value::from("OFF"))]);
    let until = cameleer_cloud::util::iso8601(cameleer_cloud::util::now_secs() + 3600);
    s3req(&t, &uri, "putObjectRetention", "c.txt", "", &[("CamelAwsS3ObjectLockMode", Value::from("GOVERNANCE")), ("CamelAwsS3ObjectLockRetainUntilDate", Value::from(until.clone()))]);
    let ex = s3req(&t, &uri, "getObjectRetention", "c.txt", "", &[]);
    assert_eq!(s(ex.header("CamelAwsS3ObjectLockMode").unwrap()), "GOVERNANCE");
    // deleteObjects reports the locked object as an error
    let ex = req(&t, &uri, Value::Array(vec![Value::from("b.txt"), Value::from("c.txt")]), &[("CamelAwsS3Operation", Value::from("deleteObjects"))]);
    assert_eq!(ex.body().as_array().unwrap().iter().map(s).collect::<Vec<_>>(), vec!["b.txt"]);
    let errors = ex.header("CamelAwsS3DeleteErrors").unwrap().as_array().unwrap().clone();
    assert_eq!(s(errors[0].get("key").unwrap()), "c.txt");
    // governance retention can be bypassed
    s3req(&t, &uri, "deleteObject", "c.txt", "", &[("CamelAwsS3BypassGovernanceRetention", Value::from("true"))]);
    assert_eq!(mock.object_keys("docs"), vec!["a.txt"]);
    // presigned upload link
    let ex = s3req(&t, &uri, "createUploadLink", "uploaded.bin", "", &[("CamelAwsS3UploadLinkExpirationTime", Value::Int(60_000))]);
    let link = s(ex.body());
    assert!(link.contains("X-Amz-Signature"));
    let http = cameleer_cloud::http::HttpClient::new();
    let resp = http.send(&cameleer_cloud::http::Request::new("PUT", cameleer_cloud::http::Url::parse(&link).unwrap()).body(b"via link".to_vec())).unwrap();
    assert!(resp.is_success(), "{} {}", resp.status, resp.text());
    assert_eq!(mock.object("docs", "uploaded.bin").unwrap(), b"via link");
    // restore of an archived object
    mock.put_object_with_class("docs", "old.log", b"archived", "GLACIER");
    let e = req_err(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("getObject")), ("CamelAwsS3Key", Value::from("old.log"))]);
    assert!(e.message.contains("InvalidObjectState"), "{}", e);
    assert_eq!(s3req(&t, &uri, "restoreObject", "old.log", "", &[("CamelAwsS3RestoreDays", Value::Int(2))]).body().as_bool(), Some(true));
    assert_eq!(s3req(&t, &uri, "getObject", "old.log", "", &[]).body().to_bytes(), b"archived");
    assert!(req_err(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("restoreObject")), ("CamelAwsS3Key", Value::from("a.txt"))]).message.contains("InvalidObjectState"));
    // conditional writes
    let cw = format!("{}&conditionalWritesEnabled=true", uri);
    let e = req_err(&t, &cw, "again", &[("CamelAwsS3Operation", Value::from("putObject")), ("CamelAwsS3Key", Value::from("a.txt"))]);
    assert!(e.message.contains("PreconditionFailed"), "{}", e);
    s3req(&t, &cw, "putObject", "fresh.txt", "new", &[]);
}

#[test]
fn s3_versioning_encryption_and_consumer_options() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("vault");
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-s3://vault?{}", mock.endpoint_options());
    // versioning
    req(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("putBucketVersioning")), ("CamelAwsS3VersioningStatus", Value::from("Enabled"))]);
    assert_eq!(s(req(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("getBucketVersioning"))]).body()), "Enabled");
    let v1 = s(s3req(&t, &uri, "putObject", "doc", "v1", &[]).header("CamelAwsS3VersionId").unwrap());
    let v2 = s(s3req(&t, &uri, "putObject", "doc", "v2", &[]).header("CamelAwsS3VersionId").unwrap());
    assert_ne!(v1, v2);
    assert_eq!(s3req(&t, &uri, "getObject", "doc", "", &[("CamelAwsS3VersionId", Value::from(v1.clone()))]).body().to_bytes(), b"v1");
    let del = s3req(&t, &uri, "deleteObject", "doc", "", &[]);
    assert!(del.header("CamelAwsS3VersionId").is_some());
    let versions = req(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("listObjectVersions"))]);
    let list = versions.body().as_array().unwrap().clone();
    assert_eq!(list.len(), 3);
    assert_eq!(list.iter().filter(|v| v.get("deleteMarker").unwrap().as_bool() == Some(true)).count(), 1);
    // SSE-KMS / SSE-S3
    let kms = format!("{}&useAwsKMS=true&awsKMSKeyId=arn:aws:kms:us-east-1:000000000000:key/k1", uri);
    s3req(&t, &kms, "putObject", "secret.txt", "s", &[]);
    assert_eq!(mock.s3_object_encryption("vault", "secret.txt").unwrap(), (Some("aws:kms".into()), Some("arn:aws:kms:us-east-1:000000000000:key/k1".into()), None));
    s3req(&t, &format!("{}&useSSES3=true", uri), "putObject", "aes.txt", "s", &[]);
    assert_eq!(mock.s3_object_encryption("vault", "aes.txt").unwrap().0.as_deref(), Some("AES256"));
    assert!(context().get_endpoint(&format!("{}&useAwsKMS=true", uri)).is_err());
    // SSE-C: single put, multipart parts and reads need the key
    let key = cameleer_cloud::util::base64_encode(&[7u8; 32]);
    let ssec = format!("{}&useCustomerKey=true&customerKeyId=RAW({})", uri, key);
    s3req(&t, &ssec, "putObject", "c.bin", "customer", &[]);
    assert!(mock.s3_object_encryption("vault", "c.bin").unwrap().2.is_some());
    assert_eq!(s3req(&t, &ssec, "getObject", "c.bin", "", &[]).body().to_bytes(), b"customer");
    let e = req_err(&t, &uri, "", &[("CamelAwsS3Operation", Value::from("getObject")), ("CamelAwsS3Key", Value::from("c.bin"))]);
    assert!(e.message.contains("InvalidRequest"), "{}", e);
    mock.set_min_part_size(4);
    let big = format!("{}&multiPartUpload=true&partSize=4", ssec);
    s3req(&t, &big, "putObject", "big.bin", "0123456789", &[]);
    assert_eq!(s3req(&t, &ssec, "getObject", "big.bin", "", &[]).body().to_bytes(), b"0123456789");
    // consumer: ignoreBody and localDirectory streaming
    mock.create_bucket("in");
    mock.put_object("in", "dir/one.txt", b"first");
    let dir = std::env::temp_dir().join(format!("camel-s3-local-{}", cameleer_cloud::util::random_hex(6)));
    let svc = service(&ctx, &format!("aws2-s3://in?deleteAfterRead=false&localDirectory={}&{}", dir.display(), mock.endpoint_options()));
    let got: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let g2 = got.clone();
    svc.poll(Some(ctx.clone()), &move |ex: &mut Exchange| {
        g2.lock().unwrap().push(ex.body().to_string());
        Ok(())
    }, "t")
    .unwrap();
    let path = got.lock().unwrap()[0].clone();
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    let svc = service(&ctx, &format!("aws2-s3://in?deleteAfterRead=false&ignoreBody=true&{}", mock.endpoint_options()));
    let g3 = got.clone();
    svc.poll(Some(ctx.clone()), &move |ex: &mut Exchange| {
        assert!(ex.header("CamelAwsS3ETag").is_some());
        g3.lock().unwrap().push(format!("empty={}", ex.body().is_null()));
        Ok(())
    }, "t")
    .unwrap();
    assert_eq!(got.lock().unwrap()[1], "empty=true");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn aws_web_identity_credentials() {
    let mock = AwsMock::start().unwrap();
    mock.add_web_identity_token("eyJhbGciOiJSUzI1NiJ9.oidc.token");
    mock.create_bucket("irsa");
    let dir = std::env::temp_dir().join(format!("camel-irsa-{}", cameleer_cloud::util::random_hex(6)));
    std::fs::create_dir_all(&dir).unwrap();
    let token_file = dir.join("token");
    std::fs::write(&token_file, "eyJhbGciOiJSUzI1NiJ9.oidc.token\n").unwrap();
    let cfg = cameleer_cloud::aws::AwsConfig { region: "us-east-1".into(), use_default_credentials_provider: true, uri_endpoint_override: Some(mock.url()), override_endpoint: true, ..Default::default() };
    let tf = token_file.display().to_string();
    let env = |k: &str| match k {
        "AWS_WEB_IDENTITY_TOKEN_FILE" => Some(tf.clone()),
        "AWS_ROLE_ARN" => Some("arn:aws:iam::000000000000:role/camel-irsa".to_string()),
        "AWS_ROLE_SESSION_NAME" => Some("pod-1".to_string()),
        "HOME" => Some("/nonexistent".to_string()),
        "AWS_EC2_METADATA_DISABLED" => Some("true".to_string()),
        _ => None,
    };
    let creds = cameleer_cloud::aws::resolve_credentials(&cfg, &env).unwrap();
    assert!(creds.access_key.starts_with("ASIA"));
    assert!(creds.session_token.is_some());
    assert_eq!(mock.web_identity_requests(), 1);
    // the temporary credentials are accepted by the services
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!(
        "aws2-s3://irsa?accessKey=RAW({})&secretKey=RAW({})&sessionToken=RAW({})&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true",
        creds.access_key,
        creds.secret_key,
        creds.session_token.clone().unwrap(),
        mock.url()
    );
    s3req(&t, &uri, "putObject", "k", "irsa", &[]);
    assert_eq!(mock.object("irsa", "k").unwrap(), b"irsa");
    // cached until close to expiry
    cameleer_cloud::aws::resolve_credentials(&cfg, &env).unwrap();
    assert_eq!(mock.web_identity_requests(), 1);
    // an unknown token is rejected
    std::fs::write(&token_file, "forged").unwrap();
    let cfg2 = cameleer_cloud::aws::AwsConfig { region: "eu-west-1".into(), ..cfg.clone() };
    cameleer_cloud::aws::credentials::clear_cache();
    assert!(cameleer_cloud::aws::resolve_credentials(&cfg2, &env).is_err());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sqs_remaining_operations_and_options() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    mock.create_queue("dlq");
    let redrive = r#"{"maxReceiveCount":"2","deadLetterTargetArn":"arn:aws:sqs:us-east-1:000000000000:dlq"}"#;
    let uri = format!(
        "aws2-sqs://work?autoCreateQueue=true&maximumMessageSize=64&delayQueue=true&delaySeconds=0&serverSideEncryptionEnabled=true&kmsMasterKeyId=alias/k&redrivePolicy=RAW({})&receiveMessageWaitTimeSeconds=1&{}",
        redrive, opts
    );
    t.request_body(&uri, "hello").unwrap();
    let attrs = mock.queue_attributes("work");
    assert_eq!(attrs.get("MaximumMessageSize").map(|s| s.as_str()), Some("64"));
    assert_eq!(attrs.get("KmsMasterKeyId").map(|s| s.as_str()), Some("alias/k"));
    assert!(attrs.get("RedrivePolicy").unwrap().contains("dlq"));
    assert_eq!(attrs.get("ReceiveMessageWaitTimeSeconds").map(|s| s.as_str()), Some("1"));
    // the queue rejects messages above maximumMessageSize
    let e = t.request_body(&uri, "x".repeat(100)).unwrap_err();
    assert!(e.root_cause().message.contains("InvalidParameterValue"), "{}", e);
    // receiveMessage / changeMessageVisibility / getQueueAttributes
    let ex = req(&t, &uri, "", &[("CamelAwsSqsOperation", Value::from("receiveMessage")), ("CamelAwsSqsMaxNumberOfMessages", Value::Int(5))]);
    let msgs = ex.body().as_array().unwrap().clone();
    assert_eq!(msgs.len(), 1);
    assert_eq!(s(msgs[0].get("body").unwrap()), "hello");
    let handle = s(msgs[0].get("receiptHandle").unwrap());
    req(&t, &uri, "", &[("CamelAwsSqsOperation", Value::from("changeMessageVisibility")), ("CamelAwsSqsReceiptHandle", Value::from(handle.clone())), ("CamelAwsSqsVisibilityTimeout", Value::Int(0))]);
    let ex = req(&t, &uri, "", &[("CamelAwsSqsOperation", Value::from("getQueueAttributes"))]);
    assert_eq!(s(ex.body().get("ApproximateNumberOfMessages").unwrap()), "1");
    assert!(s(ex.body().get("QueueArn").unwrap()).ends_with(":work"));
    // received twice → moved to the dead-letter queue on the next receive
    let r = req(&t, &format!("aws2-sqs://work?visibilityTimeout=0&{}", opts), "", &[("CamelAwsSqsOperation", Value::from("receiveMessage"))]);
    assert_eq!(r.body().as_array().unwrap().len(), 1);
    let ex = req(&t, &uri, "", &[("CamelAwsSqsOperation", Value::from("receiveMessage"))]);
    assert!(ex.body().as_array().unwrap().is_empty());
    assert_eq!(mock.queue_messages("dlq"), vec!["hello"]);
    // setQueueAttributes, deleteMessageBatch, batchSeparator
    req(&t, &uri, Value::object([("VisibilityTimeout", "7")]), &[("CamelAwsSqsOperation", Value::from("setQueueAttributes"))]);
    assert_eq!(mock.queue_attributes("work").get("VisibilityTimeout").map(|s| s.as_str()), Some("7"));
    req(&t, &format!("aws2-sqs://work?batchSeparator=;&{}", opts), "a;b;c", &[("CamelAwsSqsOperation", Value::from("sendBatchMessage"))]);
    let got = req(&t, &uri, "", &[("CamelAwsSqsOperation", Value::from("receiveMessage")), ("CamelAwsSqsMaxNumberOfMessages", Value::Int(10))]);
    let handles: Vec<Value> = got.body().as_array().unwrap().iter().map(|m| m.get("receiptHandle").cloned().unwrap()).collect();
    assert_eq!(handles.len(), 3);
    let mut handles2 = handles.clone();
    handles2.push(Value::from("bogus"));
    let ex = req(&t, &uri, Value::Array(handles2), &[("CamelAwsSqsOperation", Value::from("deleteMessageBatch"))]);
    assert_eq!(ex.body().get("successful").unwrap().as_array().unwrap().len(), 3);
    assert_eq!(ex.body().get("failed").unwrap().as_array().unwrap().len(), 1);
    // existing queue: attributes applied on start with SetQueueAttributes
    mock.create_queue("plain");
    t.request_body(&format!("aws2-sqs://plain?policy=RAW({{\"Version\":\"2012-10-17\"}})&{}", opts), "x").unwrap();
    assert!(mock.queue_attributes("plain").get("Policy").unwrap().contains("2012-10-17"));
}

#[test]
fn sqs_concurrent_consumers_sorting_and_filters() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    mock.create_queue("many");
    for i in 0..25 {
        mock.send_sqs("many", &format!("m{:02}", i));
    }
    let svc = service(&ctx, &format!("aws2-sqs://many?concurrentConsumers=3&maxMessagesPerPoll=25&{}", mock.endpoint_options()));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let n = svc
        .poll(Some(ctx.clone()), &move |ex: &mut Exchange| {
            s2.lock().unwrap().push(ex.body().to_string());
            Ok(())
        }, "t")
        .unwrap();
    assert_eq!(n, 25);
    assert_eq!(seen.lock().unwrap().len(), 25);
    assert!(mock.queue_messages("many").is_empty());
    // sortAttributeName + deleteIfFiltered=false
    for b in ["b", "a", "c"] {
        mock.send_sqs("many", b);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let svc = service(&ctx, &format!("aws2-sqs://many?maxMessagesPerPoll=10&sortAttributeName=SentTimestamp&deleteIfFiltered=false&visibilityTimeout=0&{}", mock.endpoint_options()));
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let o2 = order.clone();
    svc.poll(Some(ctx.clone()), &move |ex: &mut Exchange| {
        let b = ex.body().to_string();
        if b == "a" {
            ex.set_property("CamelFilterMatched", false);
        }
        o2.lock().unwrap().push(b);
        Ok(())
    }, "t")
    .unwrap();
    assert_eq!(order.lock().unwrap().clone(), vec!["b", "a", "c"]);
    assert_eq!(mock.queue_messages("many"), vec!["a"], "the filtered message is kept");
    // sendEmptyMessageWhenIdle
    mock.create_queue("idle");
    let svc = service(&ctx, &format!("aws2-sqs://idle?sendEmptyMessageWhenIdle=true&{}", mock.endpoint_options()));
    let empties = Arc::new(Mutex::new(0));
    let e2 = empties.clone();
    svc.poll(Some(ctx.clone()), &move |ex: &mut Exchange| {
        assert!(ex.body().is_null());
        *e2.lock().unwrap() += 1;
        Ok(())
    }, "t")
    .unwrap();
    assert_eq!(*empties.lock().unwrap(), 1);
}

#[test]
fn sns_subscribe_to_sqs_fifo_attributes_and_batches() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    mock.create_queue("inbox");
    let uri = format!(
        "aws2-sns://news?autoCreateTopic=true&subscribeSNStoSQS=true&queueArn=arn:aws:sqs:us-east-1:000000000000:inbox&serverSideEncryptionEnabled=true&kmsMasterKeyId=alias/sns&policy=RAW({{\"Statement\":[]}})&subject=Hello&{}",
        opts
    );
    t.request_body_and_header(&uri, "breaking", "category", "world").unwrap();
    let arn = "arn:aws:sns:us-east-1:000000000000:news";
    assert_eq!(mock.sns_subscriptions(), vec![(arn.to_string(), "sqs".to_string(), "arn:aws:sqs:us-east-1:000000000000:inbox".to_string())]);
    let attrs = mock.topic_attributes(arn);
    assert_eq!(attrs.get("KmsMasterKeyId").map(|s| s.as_str()), Some("alias/sns"));
    assert!(attrs.get("Policy").unwrap().contains("Statement"));
    // delivered to the queue in the SNS envelope
    let msgs = mock.queue_messages("inbox");
    assert_eq!(msgs.len(), 1);
    let env = cameleer_core::json::parse(&msgs[0]).unwrap();
    assert_eq!(s(env.get("Message").unwrap()), "breaking");
    assert_eq!(s(env.get("Subject").unwrap()), "Hello");
    assert_eq!(s(env.get("MessageAttributes").unwrap().get("category").unwrap().get("Value").unwrap()), "world");
    // subscribing again is idempotent
    t.request_body(&uri, "second").unwrap();
    assert_eq!(mock.sns_subscriptions().len(), 1);
    assert!(context().get_endpoint(&format!("aws2-sns://news?subscribeSNStoSQS=true&{}", opts)).is_err());
    // FIFO topic: created with FifoTopic / ContentBasedDeduplication, fifo queue subscription
    mock.create_queue("orders.fifo");
    let fifo = format!(
        "aws2-sns://orders.fifo?autoCreateTopic=true&messageDeduplicationIdStrategy=useContentBasedDeduplication&messageGroupIdStrategy=useExchangeId&subscribeSNStoSQS=true&queueArn=arn:aws:sqs:us-east-1:000000000000:orders.fifo&{}",
        opts
    );
    t.request_body(&fifo, "o-1").unwrap();
    let fattrs = mock.topic_attributes("arn:aws:sns:us-east-1:000000000000:orders.fifo");
    assert_eq!(fattrs.get("FifoTopic").map(|s| s.as_str()), Some("true"));
    assert_eq!(fattrs.get("ContentBasedDeduplication").map(|s| s.as_str()), Some("true"));
    assert!(mock.published("orders.fifo")[0].group_id.is_some());
    assert_eq!(mock.queue_messages("orders.fifo").len(), 1);
    // a standard queue cannot subscribe to a FIFO topic
    let e = t.request_body(&format!("aws2-sns://orders.fifo?autoCreateTopic=true&subscribeSNStoSQS=true&queueArn=arn:aws:sqs:us-east-1:000000000000:inbox&{}", opts), "x").unwrap_err();
    assert!(e.root_cause().message.contains("FIFO"), "{}", e);
    // PublishBatch
    let ex = req(&t, &format!("aws2-sns://news?batchEnabled=true&{}", opts), Value::Array((0..12).map(|i| Value::from(format!("b{}", i))).collect()), &[]);
    assert_eq!(ex.body().get("successful").unwrap().as_array().unwrap().len(), 12);
    assert_eq!(mock.queue_messages("inbox").len(), 14);
}

#[test]
fn iam_access_key_status_roles_policies_and_profiles() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = |op: &str| format!("aws2-iam://iam?operation={}&{}", op, mock.endpoint_options());
    let u = |n: &str| ("CamelAwsIAMUsername", Value::from(n));
    req(&t, &uri("createUser"), "", &[u("carol")]);
    let key = req(&t, &uri("createAccessKey"), "", &[u("carol")]).body().clone();
    let (ak, sk) = (s(key.get("accessKeyId").unwrap()), s(key.get("secretAccessKey").unwrap()));
    let as_carol = format!("aws2-iam://iam?operation=listUsers&accessKey=RAW({})&secretKey=RAW({})&uriEndpointOverride={}&overrideEndpoint=true", ak, sk, mock.url());
    assert_eq!(req(&t, &as_carol, "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &uri("updateAccessKey"), "", &[u("carol"), ("CamelAwsIAMAccessKeyID", Value::from(ak.as_str())), ("CamelAwsIAMAccessKeyStatus", Value::from("Inactive"))]);
    let keys = req(&t, &uri("listAccessKeys"), "", &[u("carol")]).body().clone();
    assert_eq!(s(keys.as_array().unwrap()[0].get("status").unwrap()), "Inactive");
    assert!(t.request_body(&as_carol, "").is_err(), "inactive keys cannot sign");
    req(&t, &uri("updateAccessKey"), "", &[("CamelAwsIAMAccessKeyID", Value::from(ak.as_str())), ("CamelAwsIAMAccessKeyStatus", Value::from("Active"))]);
    assert!(t.request_body(&as_carol, "").is_ok());
    // groups with members
    req(&t, &uri("createGroup"), "", &[("CamelAwsIAMGroupName", Value::from("ops"))]);
    req(&t, &uri("addUserToGroup"), "", &[u("carol"), ("CamelAwsIAMGroupName", Value::from("ops"))]);
    let g = req(&t, &uri("getGroup"), "", &[("CamelAwsIAMGroupName", Value::from("ops"))]).body().clone();
    assert_eq!(s(g.get("users").unwrap().as_array().unwrap()[0].get("userName").unwrap()), "carol");
    // roles
    let trust = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;
    let role = req(&t, &uri("createRole"), "", &[("CamelAwsIAMRoleName", Value::from("app")), ("CamelAwsIAMAssumeRolePolicyDocument", Value::from(trust)), ("CamelAwsIAMDescription", Value::from("the app"))]).body().clone();
    assert_eq!(s(role.get("arn").unwrap()), "arn:aws:iam::000000000000:role/app");
    assert_eq!(s(req(&t, &uri("getRole"), "", &[("CamelAwsIAMRoleName", Value::from("app"))]).body().get("description").unwrap()), "the app");
    assert_eq!(mock.iam_roles(), vec!["app"]);
    // managed policies + attachments
    let doc = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:*","Resource":"*"}]}"#;
    let pol = req(&t, &uri("createPolicy"), doc, &[("CamelAwsIAMPolicyName", Value::from("s3-all"))]).body().clone();
    let arn = s(pol.get("arn").unwrap());
    assert_eq!(arn, "arn:aws:iam::000000000000:policy/s3-all");
    let pa = ("CamelAwsIAMPolicyArn", Value::from(arn.as_str()));
    req(&t, &uri("attachUserPolicy"), "", &[u("carol"), pa.clone()]);
    req(&t, &uri("attachRolePolicy"), "", &[("CamelAwsIAMRoleName", Value::from("app")), pa.clone()]);
    req(&t, &uri("attachGroupPolicy"), "", &[("CamelAwsIAMGroupName", Value::from("ops")), pa.clone()]);
    let att = req(&t, &uri("listAttachedUserPolicies"), "", &[u("carol")]).body().clone();
    assert_eq!(s(att.as_array().unwrap()[0].get("policyName").unwrap()), "s3-all");
    assert_eq!(req(&t, &uri("getPolicy"), "", &[pa.clone()]).body().get("attachmentCount").unwrap().as_i64(), Some(3));
    assert_eq!(req(&t, &uri("listPolicies"), "", &[]).body().as_array().unwrap().len(), 1);
    let e = req_err(&t, &uri("deletePolicy"), "", &[pa.clone()]);
    assert!(e.message.contains("DeleteConflict"), "{}", e);
    req(&t, &uri("detachUserPolicy"), "", &[u("carol"), pa.clone()]);
    req(&t, &uri("detachRolePolicy"), "", &[("CamelAwsIAMRoleName", Value::from("app")), pa.clone()]);
    req(&t, &uri("detachGroupPolicy"), "", &[("CamelAwsIAMGroupName", Value::from("ops")), pa.clone()]);
    assert!(req(&t, &uri("listAttachedRolePolicies"), "", &[("CamelAwsIAMRoleName", Value::from("app"))]).body().as_array().unwrap().is_empty());
    req(&t, &uri("deletePolicy"), "", &[pa]);
    // instance profiles
    let ip = ("CamelAwsIAMInstanceProfileName", Value::from("app-profile"));
    req(&t, &uri("createInstanceProfile"), "", &[ip.clone()]);
    req(&t, &uri("addRoleToInstanceProfile"), "", &[ip.clone(), ("CamelAwsIAMRoleName", Value::from("app"))]);
    let prof = req(&t, &uri("getInstanceProfile"), "", &[ip.clone()]).body().clone();
    assert_eq!(s(&prof.get("roles").unwrap().as_array().unwrap()[0]), "app");
    assert_eq!(req(&t, &uri("listInstanceProfiles"), "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &uri("removeRoleFromInstanceProfile"), "", &[ip.clone(), ("CamelAwsIAMRoleName", Value::from("app"))]);
    req(&t, &uri("deleteInstanceProfile"), "", &[ip]);
    assert_eq!(req(&t, &uri("listRoles"), "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &uri("deleteRole"), "", &[("CamelAwsIAMRoleName", Value::from("app"))]);
    let e = req_err(&t, &uri("getRole"), "", &[("CamelAwsIAMRoleName", Value::from("app"))]);
    assert!(e.message.contains("NoSuchEntity"), "{}", e);
}

#[test]
fn ec2_monitoring_tags_and_security_groups() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = |op: &str| format!("aws2-ec2://ec2?operation={}&{}", op, mock.endpoint_options());
    let started = req(
        &t,
        &uri("createAndRunInstances"),
        "",
        &[
            ("CamelAwsEC2ImageId", Value::from("ami-1")),
            ("CamelAwsEC2InstanceType", Value::from("t3.small")),
            ("CamelAwsEC2InstanceMonitoring", Value::Bool(true)),
            ("CamelAwsEC2InstanceEbsOptimized", Value::Bool(true)),
            ("CamelAwsEC2InstanceKernelId", Value::from("aki-1")),
            ("CamelAwsEC2InstanceSecurityGroups", Value::from(vec!["web", "sg-0123"])),
            ("CamelAwsEC2InstancesPlacement", Value::from("us-east-1b")),
            ("CamelAwsEC2SubnetId", Value::from("subnet-9")),
            ("CamelAwsEC2InstancesClientToken", Value::from("tok-1")),
            ("CamelAwsEC2InstancesTags", Value::object([("env", "prod")])),
        ],
    )
    .body()
    .clone();
    let inst = started.as_array().unwrap()[0].clone();
    let id = s(inst.get("instanceId").unwrap());
    assert_eq!(s(inst.get("monitoring").unwrap()), "enabled");
    assert_eq!(s(inst.get("availabilityZone").unwrap()), "us-east-1b");
    assert_eq!(s(inst.get("subnetId").unwrap()), "subnet-9");
    assert_eq!(inst.get("ebsOptimized"), Some(&Value::Bool(true)));
    assert_eq!(inst.get("securityGroups").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(s(inst.get("tags").unwrap().get("env").unwrap()), "prod");
    // the client token makes the call idempotent
    let again = req(&t, &uri("createAndRunInstances"), "", &[("CamelAwsEC2ImageId", Value::from("ami-1")), ("CamelAwsEC2InstanceType", Value::from("t3.small")), ("CamelAwsEC2InstancesClientToken", Value::from("tok-1"))]).body().clone();
    assert_eq!(s(again.as_array().unwrap()[0].get("instanceId").unwrap()), id);
    assert_eq!(mock.ec2_instances().len(), 1);
    let ids = ("CamelAwsEC2InstancesIds", Value::from(vec![id.clone()]));
    let m = req(&t, &uri("unmonitorInstances"), "", &[ids.clone()]).body().clone();
    assert_eq!(s(m.as_array().unwrap()[0].get("monitoring").unwrap()), "disabling");
    let m = req(&t, &uri("monitorInstances"), "", &[ids.clone()]).body().clone();
    assert_eq!(s(m.as_array().unwrap()[0].get("monitoring").unwrap()), "pending");
    req(&t, &uri("createTags"), "", &[ids.clone(), ("CamelAwsEC2InstancesTags", Value::object([("team", "a"), ("tier", "web")]))]);
    req(&t, &uri("deleteTags"), "", &[ids.clone(), ("CamelAwsEC2InstancesTags", Value::from(vec!["env"]))]);
    req(&t, &uri("deleteTags"), "", &[ids.clone(), ("CamelAwsEC2InstancesTags", Value::object([("tier", "other")]))]);
    let d = req(&t, &uri("describeInstances"), "", &[ids.clone()]).body().clone();
    let tags = d.as_array().unwrap()[0].get("tags").unwrap().clone();
    assert_eq!(tags.as_object().unwrap().len(), 2, "{:?}", tags);
    assert!(tags.get("env").is_none() && s(tags.get("tier").unwrap()) == "web");
    req(&t, &uri("rebootInstances"), "", &[ids]);
}

#[test]
fn step_functions_activities_and_update_state_machine() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = |op: &str| format!("aws2-step-functions://sfn?operation={}&{}", op, mock.endpoint_options());
    let act = req(&t, &uri("createActivity"), "", &[("CamelAwsStepFunctionsActivityName", Value::from("approve"))]);
    let act_arn = s(act.header("CamelAwsStepFunctionsActivityArn").unwrap());
    assert!(act_arn.ends_with(":activity:approve"));
    assert_eq!(s(req(&t, &uri("describeActivity"), "", &[("CamelAwsStepFunctionsActivityArn", Value::from(act_arn.as_str()))]).body().get("name").unwrap()), "approve");
    assert_eq!(req(&t, &uri("listActivities"), "", &[]).body().as_array().unwrap().len(), 1);
    let def = format!(r#"{{"StartAt":"Ask","States":{{"Ask":{{"Type":"Task","Resource":"{}","Next":"Done"}},"Done":{{"Type":"Succeed"}}}}}}"#, act_arn);
    let sm = s(req(&t, &uri("createStateMachine"), "", &[("CamelAwsStepFunctionsStateMachineName", Value::from("flow")), ("CamelAwsStepFunctionsStateMachineDefinition", Value::from(def.as_str())), ("CamelAwsStepFunctionsStateMachineRoleArn", Value::from("arn:aws:iam::000000000000:role/sfn"))]).body());
    let smh = ("CamelAwsStepFunctionsStateMachineArn", Value::from(sm.as_str()));
    let run = |name: &str| s(req(&t, &uri("startExecution"), Value::object([("amount", 42)]), &[smh.clone(), ("CamelAwsStepFunctionsExecutionName", Value::from(name))]).header("CamelAwsStepFunctionsExecutionArn").unwrap());
    let status = |exec: &str| s(req(&t, &uri("describeExecution"), "", &[("CamelAwsStepFunctionsExecutionArn", Value::from(exec))]).body().get("status").unwrap());
    let ah = ("CamelAwsStepFunctionsActivityArn", Value::from(act_arn.as_str()));
    // worker: succeed
    let e1 = run("one");
    assert_eq!(status(&e1), "RUNNING");
    let task = req(&t, &uri("getActivityTask"), "", &[ah.clone(), ("CamelAwsStepFunctionsActivityWorkerName", Value::from("w1"))]);
    let token = s(task.header("CamelAwsStepFunctionsTaskToken").unwrap());
    assert_eq!(task.body().get("amount").and_then(|a| a.as_i64()), Some(42));
    req(&t, &uri("sendTaskHeartbeat"), "", &[("CamelAwsStepFunctionsTaskToken", Value::from(token.as_str()))]);
    req(&t, &uri("sendTaskSuccess"), Value::object([("approved", true)]), &[("CamelAwsStepFunctionsTaskToken", Value::from(token.as_str()))]);
    assert_eq!(status(&e1), "SUCCEEDED");
    let e = req_err(&t, &uri("sendTaskSuccess"), "{}", &[("CamelAwsStepFunctionsTaskToken", Value::from(token.as_str()))]);
    assert!(e.message.contains("TaskDoesNotExist"), "{}", e);
    // worker: fail
    let e2 = run("two");
    let token = s(req(&t, &uri("getActivityTask"), "", &[ah.clone()]).header("CamelAwsStepFunctionsTaskToken").unwrap());
    req(&t, &uri("sendTaskFailure"), "", &[("CamelAwsStepFunctionsTaskToken", Value::from(token.as_str())), ("CamelAwsStepFunctionsTaskError", Value::from("Rejected")), ("CamelAwsStepFunctionsTaskCause", Value::from("too much"))]);
    let d = req(&t, &uri("describeExecution"), "", &[("CamelAwsStepFunctionsExecutionArn", Value::from(e2.as_str()))]).body().clone();
    assert_eq!((s(d.get("status").unwrap()), s(d.get("error").unwrap())), ("FAILED".to_string(), "Rejected".to_string()));
    // update the machine to a plain Pass machine
    let pass = r#"{"StartAt":"P","States":{"P":{"Type":"Pass","Result":"ok","End":true}}}"#;
    let upd = req(&t, &uri("updateStateMachine"), "", &[smh.clone(), ("CamelAwsStepFunctionsStateMachineDefinition", Value::from(pass))]);
    assert!(!upd.body().is_null());
    let e3 = run("three");
    assert_eq!(status(&e3), "SUCCEEDED");
    let e = req_err(&t, &uri("updateStateMachine"), "", &[smh, ("CamelAwsStepFunctionsStateMachineDefinition", Value::from("{}"))]);
    assert!(e.message.contains("InvalidDefinition"), "{}", e);
    req(&t, &uri("deleteActivity"), "", &[ah.clone()]);
    let e = req_err(&t, &uri("describeActivity"), "", &[ah]);
    assert!(e.message.contains("ActivityDoesNotExist"), "{}", e);
}

#[test]
fn rekognition_collections_face_search_and_video() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("media");
    mock.put_object("media", "team.jpg", b"face:alice face:bob");
    mock.put_object("media", "clip.mp4", b"face:alice walking dog\nsign STOP face:carol");
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = |op: &str| format!("aws2-rekognition://r?operation={}&collectionId=staff&{}", op, mock.endpoint_options());
    req(&t, &uri("createCollection"), "", &[]);
    let e = req_err(&t, &uri("createCollection"), "", &[]);
    assert!(e.message.contains("ResourceAlreadyExists"), "{}", e);
    let recs = req(&t, &uri("indexFaces"), "", &[("CamelAwsRekognitionS3Bucket", Value::from("media")), ("CamelAwsRekognitionS3Object", Value::from("team.jpg")), ("CamelAwsRekognitionExternalImageId", Value::from("team"))]).body().clone();
    assert_eq!(recs.as_array().unwrap().len(), 2);
    req(&t, &uri("indexFaces"), Value::Bytes(b"face:alice".to_vec()), &[]);
    assert_eq!(req(&t, &uri("describeCollection"), "", &[]).body().get("FaceCount").and_then(|c| c.as_i64()), Some(3));
    assert_eq!(req(&t, &uri("listCollections"), "", &[]).body().as_array().unwrap().len(), 1);
    let faces = req(&t, &uri("listFaces"), "", &[]).body().clone();
    assert_eq!(faces.as_array().unwrap().len(), 3);
    let alice = s(recs.as_array().unwrap()[0].get("Face").unwrap().get("FaceId").unwrap());
    let m = req(&t, &uri("searchFaces"), "", &[("CamelAwsRekognitionFaceId", Value::from(alice.as_str()))]).body().clone();
    assert_eq!(m.as_array().unwrap().len(), 1, "the other alice face");
    let m = req(&t, &uri("searchFacesByImage"), Value::Bytes(b"face:bob".to_vec()), &[("CamelAwsRekognitionFaceMatchThreshold", Value::from("90"))]).body().clone();
    assert_eq!(s(m.as_array().unwrap()[0].get("Face").unwrap().get("ExternalImageId").unwrap()), "team");
    let cmp = req(&t, &uri("compareFaces"), Value::Bytes(b"face:bob".to_vec()), &[("CamelAwsRekognitionTargetS3Bucket", Value::from("media")), ("CamelAwsRekognitionTargetS3Object", Value::from("team.jpg"))]);
    assert_eq!(cmp.body().as_array().unwrap().len(), 1);
    assert_eq!(cmp.header("CamelAwsRekognitionUnmatchedFaces").unwrap().as_array().unwrap().len(), 1);
    let del = req(&t, &uri("deleteFaces"), Value::from(vec![alice.clone()]), &[]).body().clone();
    assert_eq!(del.as_array().unwrap().len(), 1);
    // stored video
    let video = [("CamelAwsRekognitionS3Bucket", Value::from("media")), ("CamelAwsRekognitionS3Object", Value::from("clip.mp4"))];
    let job = s(req(&t, &uri("startLabelDetection"), "", &video).body());
    let r = req(&t, &uri("getLabelDetection"), "", &[("CamelAwsRekognitionJobId", Value::from(job.as_str()))]);
    assert_eq!(s(r.header("CamelAwsRekognitionJobStatus").unwrap()), "SUCCEEDED");
    assert!(r.body().as_array().unwrap().iter().any(|l| s(l.get("Label").unwrap().get("Name").unwrap()) == "Dog"));
    let job = s(req(&t, &uri("startTextDetection"), "", &video).body());
    assert_eq!(req(&t, &uri("getTextDetection"), "", &[("CamelAwsRekognitionJobId", Value::from(job.as_str()))]).body().as_array().unwrap().len(), 2);
    let job = s(req(&t, &uri("startFaceDetection"), "", &video).body());
    assert_eq!(req(&t, &uri("getFaceDetection"), "", &[("CamelAwsRekognitionJobId", Value::from(job.as_str()))]).body().as_array().unwrap().len(), 2);
    let job = s(req(&t, &uri("startFaceSearch"), "", &video).body());
    let persons = req(&t, &uri("getFaceSearch"), "", &[("CamelAwsRekognitionJobId", Value::from(job.as_str()))]).body().clone();
    let p = persons.as_array().unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(p[0].get("FaceMatches").unwrap().as_array().unwrap().len(), 1, "one alice face is left in the collection");
    assert!(p[1].get("FaceMatches").unwrap().as_array().unwrap().is_empty());
    req(&t, &uri("deleteCollection"), "", &[]);
    let e = req_err(&t, &uri("describeCollection"), "", &[]);
    assert!(e.message.contains("ResourceNotFound"), "{}", e);
}

#[test]
fn transcribe_medical_call_analytics_vocabularies_models_and_tags() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let tr = |op: &str| format!("aws2-transcribe://t?operation={}&{}", op, mock.endpoint_options());
    let status = |ex: &Exchange| s(ex.header("CamelAwsTranscribeTranscriptionJobStatus").unwrap());
    // medical
    let mj = ("CamelAwsTranscribeMedicalTranscriptionJobName", Value::from("visit-1"));
    let e = req_err(&t, &tr("startMedicalTranscriptionJob"), "s3://audio/visit.wav", &[mj.clone()]);
    assert!(e.message.contains("OutputBucketName"), "{}", e);
    let ex = req(&t, &tr("startMedicalTranscriptionJob"), "s3://audio/visit.wav", &[mj.clone(), ("CamelAwsTranscribeOutputBucketName", Value::from("out")), ("CamelAwsTranscribeType", Value::from("DICTATION"))]);
    assert_eq!(status(&ex), "IN_PROGRESS");
    assert_eq!(s(ex.body().get("Specialty").unwrap()), "PRIMARYCARE");
    req(&t, &tr("getMedicalTranscriptionJob"), "", &[mj.clone()]);
    let ex = req(&t, &tr("getMedicalTranscriptionJob"), "", &[mj.clone()]);
    assert_eq!(status(&ex), "COMPLETED");
    assert!(s(ex.body().get("Transcript").unwrap().get("TranscriptFileUri").unwrap()).contains("/out/medical/visit-1"));
    assert_eq!(req(&t, &tr("listMedicalTranscriptionJobs"), "", &[("CamelAwsTranscribeStatus", Value::from("COMPLETED"))]).body().as_array().unwrap().len(), 1);
    // call analytics: category + job
    let rules = Value::Array(vec![Value::object([("NonTalkTimeFilter", Value::object([("Threshold", Value::Int(10000))]))])]);
    let cat = req(&t, &tr("createCallAnalyticsCategory"), rules.clone(), &[("CamelAwsTranscribeCategoryName", Value::from("silence"))]).body().clone();
    assert_eq!(s(cat.get("CategoryName").unwrap()), "silence");
    let e = req_err(&t, &tr("createCallAnalyticsCategory"), rules.clone(), &[("CamelAwsTranscribeCategoryName", Value::from("silence"))]);
    assert!(e.message.contains("ConflictException"), "{}", e);
    req(&t, &tr("updateCallAnalyticsCategory"), "", &[("CamelAwsTranscribeCategoryName", Value::from("silence")), ("CamelAwsTranscribeRules", rules)]);
    assert_eq!(req(&t, &tr("listCallAnalyticsCategories"), "", &[]).body().as_array().unwrap().len(), 1);
    assert!(req(&t, &tr("getCallAnalyticsCategory"), "", &[("CamelAwsTranscribeCategoryName", Value::from("silence"))]).body().get("Rules").is_some());
    req(&t, &tr("deleteCallAnalyticsCategory"), "", &[("CamelAwsTranscribeCategoryName", Value::from("silence"))]);
    let cj = ("CamelAwsTranscribeCallAnalyticsJobName", Value::from("call-1"));
    let channels = Value::Array(vec![Value::object([("ChannelId", Value::Int(0)), ("ParticipantRole", Value::from("AGENT"))])]);
    let ex = req(&t, &tr("startCallAnalyticsJob"), "", &[cj.clone(), ("CamelAwsTranscribeMediaUri", Value::from("s3://audio/call.wav")), ("CamelAwsTranscribeDataAccessRoleArn", Value::from("arn:aws:iam::000000000000:role/tr")), ("CamelAwsTranscribeChannelDefinitions", channels)]);
    assert_eq!(status(&ex), "IN_PROGRESS");
    req(&t, &tr("getCallAnalyticsJob"), "", &[cj.clone()]);
    assert_eq!(status(&req(&t, &tr("getCallAnalyticsJob"), "", &[cj.clone()])), "COMPLETED");
    assert_eq!(req(&t, &tr("listCallAnalyticsJobs"), "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &tr("deleteCallAnalyticsJob"), "", &[cj.clone()]);
    assert!(req_err(&t, &tr("getCallAnalyticsJob"), "", &[cj]).message.contains("NotFoundException"));
    // vocabularies (three, read over two pages)
    for n in ["camel", "kafka", "pulsar"] {
        let ex = req(&t, &tr("createVocabulary"), Value::from(vec!["Apache-Camel", "EIP"]), &[("CamelAwsTranscribeVocabularyName", Value::from(n))]);
        assert_eq!(status(&ex), "PENDING");
    }
    let e = req_err(&t, &tr("createVocabulary"), "", &[("CamelAwsTranscribeVocabularyName", Value::from("bad")), ("CamelAwsTranscribePhrases", Value::from(vec!["two words"]))]);
    assert!(e.message.contains("spaces"), "{}", e);
    let v = ("CamelAwsTranscribeVocabularyName", Value::from("camel"));
    req(&t, &tr("getVocabulary"), "", &[v.clone()]);
    assert_eq!(status(&req(&t, &tr("getVocabulary"), "", &[v.clone()])), "READY");
    assert_eq!(req(&t, &tr("listVocabularies"), "", &[]).body().as_array().unwrap().len(), 3, "all pages");
    assert_eq!(req(&t, &tr("listVocabularies"), "", &[("CamelAwsTranscribeStateEquals", Value::from("READY"))]).body().as_array().unwrap().len(), 1);
    req(&t, &tr("updateVocabulary"), "", &[v.clone(), ("CamelAwsTranscribeVocabularyFileUri", Value::from("s3://vocab/camel.txt"))]);
    req(&t, &tr("deleteVocabulary"), "", &[v]);
    assert_eq!(mock.transcribe_resources("vocabulary"), vec!["kafka", "pulsar"]);
    let mv = ("CamelAwsTranscribeVocabularyName", Value::from("drugs"));
    assert!(req_err(&t, &tr("createMedicalVocabulary"), Value::from(vec!["Aspirin"]), &[mv.clone()]).message.contains("VocabularyFileUri"));
    req(&t, &tr("createMedicalVocabulary"), "", &[mv.clone(), ("CamelAwsTranscribeVocabularyFileUri", Value::from("s3://vocab/drugs.txt"))]);
    req(&t, &tr("updateMedicalVocabulary"), "", &[mv.clone(), ("CamelAwsTranscribeVocabularyFileUri", Value::from("s3://vocab/drugs2.txt"))]);
    assert_eq!(s(req(&t, &tr("getMedicalVocabulary"), "", &[mv.clone()]).body().get("VocabularyFileUri").unwrap()), "s3://vocab/drugs2.txt");
    assert_eq!(req(&t, &tr("listMedicalVocabularies"), "", &[]).body().as_array().unwrap().len(), 1);
    // language model + tags
    let lm = ("CamelAwsTranscribeLanguageModelName", Value::from("support"));
    let ex = req(&t, &tr("createLanguageModel"), "", &[lm.clone(), ("CamelAwsTranscribeBaseModelName", Value::from("NarrowBand")), ("CamelAwsTranscribeInputDataS3Uri", Value::from("s3://train/")), ("CamelAwsTranscribeDataAccessRoleArn", Value::from("arn:aws:iam::000000000000:role/tr"))]);
    assert_eq!(status(&ex), "IN_PROGRESS");
    req(&t, &tr("describeLanguageModel"), "", &[lm.clone()]);
    assert_eq!(status(&req(&t, &tr("describeLanguageModel"), "", &[lm.clone()])), "COMPLETED");
    assert_eq!(req(&t, &tr("listLanguageModels"), "", &[]).body().as_array().unwrap().len(), 1);
    let arn = ("CamelAwsTranscribeResourceArn", Value::from("arn:aws:transcribe:us-east-1:000000000000:language-model/support"));
    req(&t, &tr("tagResource"), "", &[arn.clone(), ("CamelAwsTranscribeTags", Value::object([("team", "cx"), ("env", "dev")]))]);
    req(&t, &tr("untagResource"), "", &[arn.clone(), ("CamelAwsTranscribeTagKeys", Value::from(vec!["env"]))]);
    let tags = req(&t, &tr("listTagsForResource"), "", &[arn]).body().clone();
    assert_eq!(tags.as_object().unwrap().len(), 1);
    assert_eq!(s(tags.get("team").unwrap()), "cx");
    req(&t, &tr("deleteLanguageModel"), "", &[lm]);
    let e = req_err(&t, &tr("listTagsForResource"), "", &[("CamelAwsTranscribeResourceArn", Value::from("arn:aws:transcribe:us-east-1:000000000000:language-model/support"))]);
    assert!(e.message.contains("NotFoundException"), "{}", e);
}
