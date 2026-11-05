//! Third-party simulator: Cameleer's AWS components against moto in server mode
//! (`scripts/simulators.sh`: HTTP on 15000, HTTPS with the interop certificate on 15001,
//! IAM authentication + SigV4 verification enforced on 15002). Runs only with
//! `CAMEL_SIMULATORS=1`.

use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_tls::testing::{live, sim};

const ACCOUNT: &str = "123456789012";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

fn opts() -> String {
    "accessKey=testing&secretKey=testing&region=us-east-1&uriEndpointOverride=http://127.0.0.1:15000&overrideEndpoint=true".into()
}

fn s(v: &Value) -> String {
    v.to_string()
}

fn req(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> Exchange {
    let body = body.into();
    t.request(uri, |ex| {
        ex.set_body(body.clone());
        for (k, v) in headers {
            ex.set_header(*k, v.clone());
        }
    })
    .unwrap_or_else(|e| panic!("{}: {}", uri, e))
}

fn req_err(t: &ProducerTemplate, uri: &str, body: impl Into<Value>, headers: &[(&str, Value)]) -> CamelError {
    let body = body.into();
    t.request(uri, |ex| {
        ex.set_body(body.clone());
        for (k, v) in headers {
            ex.set_header(*k, v.clone());
        }
    })
    .err()
    .unwrap_or_else(|| panic!("{} should fail", uri))
    .root_cause()
    .clone()
}

fn json(s: &str) -> Value {
    cameleer_core::json::parse(s).unwrap()
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < Duration::from_secs(30), "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn keys(t: &ProducerTemplate, bucket: &str, o: &str) -> Vec<String> {
    let mut k: Vec<String> = t.request_body(&format!("aws2-s3://{}?operation=listObjects&{}", bucket, o), "").unwrap().as_array().unwrap().iter().map(|o| s(o.get("key").unwrap())).collect();
    k.sort();
    k
}

// ----------------------------------------------------------------------------- S3

#[test]
fn sim_aws_s3_objects_listing_copy_ranges_and_errors() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = opts();
    let bucket = sim::unique("docs");
    let archive = sim::unique("archive");
    let u = |b: &str, extra: &str| format!("aws2-s3://{}?{}{}", b, extra, o);
    req(&t, &u(&bucket, ""), "", &[("CamelAwsS3Operation", Value::from("createBucket"))]);
    req(&t, &u(&archive, "operation=createBucket&"), "", &[]);
    let ex = req(
        &t,
        &u(&bucket, ""),
        "hello s3",
        &[("CamelAwsS3Key", Value::from("dir/a file+ü.txt")), ("CamelAwsS3ContentType", Value::from("text/plain")), ("CamelAwsS3Metadata", Value::object([("owner", "camel")]))],
    );
    assert_eq!(s(ex.header("CamelAwsS3ETag").unwrap()), format!("\"{}\"", cameleer_cloud::util::md5_hex(b"hello s3")));
    t.send_body(&u(&bucket, "keyName=b.txt&"), "second").unwrap();
    // getObject with metadata, content type and length as moto stored them
    let ex = req(&t, &u(&bucket, "operation=getObject&"), "", &[("CamelAwsS3Key", Value::from("dir/a file+ü.txt"))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "hello s3");
    assert_eq!(s(ex.header("CamelAwsS3ContentType").unwrap()), "text/plain");
    assert_eq!(ex.header("CamelAwsS3ContentLength").unwrap().as_i64(), Some(8));
    assert_eq!(ex.header("CamelAwsS3Metadata").unwrap().get("owner").unwrap(), &Value::from("camel"));
    let ex = req(&t, &u(&bucket, "operation=getObjectRange&"), "", &[("CamelAwsS3Key", Value::from("dir/a file+ü.txt")), ("CamelAwsS3RangeStart", Value::Int(6)), ("CamelAwsS3RangeEnd", Value::Int(7))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "s3");
    // listing with a prefix
    let list = t.request_body_and_header(&u(&bucket, "operation=listObjects&"), "", "CamelAwsS3Prefix", "dir/").unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list.as_array().unwrap()[0].get("size").unwrap(), &Value::Int(8));
    assert_eq!(keys(&t, &bucket, &o), vec!["b.txt", "dir/a file+ü.txt"]);
    // copyObject to another bucket, headObject, listBuckets
    req(&t, &u(&bucket, "operation=copyObject&"), "", &[("CamelAwsS3Key", Value::from("b.txt")), ("CamelAwsS3BucketDestinationName", Value::from(archive.clone())), ("CamelAwsS3DestinationKey", Value::from("copied/b.txt"))]);
    let ex = req(&t, &u(&archive, "operation=getObject&"), "", &[("CamelAwsS3Key", Value::from("copied/b.txt"))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "second");
    let buckets = t.request_body(&u(&bucket, "operation=listBuckets&"), "").unwrap();
    let names: Vec<String> = buckets.as_array().unwrap().iter().map(|b| s(b.get("name").unwrap())).collect();
    assert!(names.contains(&bucket) && names.contains(&archive), "{:?}", names);
    // errors as moto reports them
    let e = req_err(&t, &u(&bucket, "operation=getObject&"), "", &[("CamelAwsS3Key", Value::from("missing"))]);
    assert!(e.is_a("S3Exception") && e.message.contains("NoSuchKey"), "{}", e);
    let e = req_err(&t, &u(&bucket, ""), "", &[("CamelAwsS3Operation", Value::from("deleteBucket"))]);
    assert!(e.message.contains("BucketNotEmpty"), "{}", e);
    // deleteObject(s), then deleteBucket
    req(&t, &u(&bucket, "operation=deleteObject&"), "", &[("CamelAwsS3Key", Value::from("b.txt"))]);
    assert_eq!(keys(&t, &bucket, &o), vec!["dir/a file+ü.txt"]);
    req(&t, &u(&bucket, "operation=deleteObject&"), "", &[("CamelAwsS3Key", Value::from("dir/a file+ü.txt"))]);
    req(&t, &u(&bucket, "operation=deleteBucket&"), "", &[]);
    let e = req_err(&t, &u(&bucket, "operation=listObjects&"), "", &[]);
    assert!(e.message.contains("NoSuchBucket"), "{}", e);
}

#[test]
fn sim_aws_s3_presigned_links_multipart_and_streaming() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let t = ctx.create_producer_template();
    let o = opts();
    let bucket = sim::unique("big");
    t.send_body(&format!("aws2-s3://{}?operation=createBucket&{}", bucket, o), "").unwrap();
    // presigned download and upload links, fetched without credentials
    t.send_body_and_header(&format!("aws2-s3://{}?{}", bucket, o), "presigned!", "CamelAwsS3Key", "file.txt").unwrap();
    let link = s(&t.request_body_and_header(&format!("aws2-s3://{}?operation=createDownloadLink&{}", bucket, o), "", "CamelAwsS3Key", "file.txt").unwrap());
    assert!(link.contains("X-Amz-Signature="), "{}", link);
    let http = cameleer_cloud::http::HttpClient::new();
    let resp = http.send(&cameleer_cloud::http::Request::new("GET", cameleer_cloud::http::Url::parse(&link).unwrap())).unwrap();
    assert_eq!((resp.status, resp.body.as_slice()), (200, &b"presigned!"[..]));

    // multipart upload with 5 MiB parts (S3's minimum, enforced by moto)
    let data: Vec<u8> = (0..(11 * 1024 * 1024u32)).map(|i| (i % 251) as u8).collect();
    let ex = req(&t, &format!("aws2-s3://{}?multiPartUpload=true&partSize=5242880&{}", bucket, o), Value::Bytes(data.clone()), &[("CamelAwsS3Key", Value::from("parts.bin"))]);
    assert!(s(ex.header("CamelAwsS3ETag").unwrap()).ends_with("-3\""), "{:?}", ex.header("CamelAwsS3ETag"));
    let ex = req(&t, &format!("aws2-s3://{}?operation=getObject&{}", bucket, o), "", &[("CamelAwsS3Key", Value::from("parts.bin"))]);
    assert_eq!(ex.body_as::<Vec<u8>>().unwrap(), data);
    // parts below the minimum: moto refuses the completion, the upload is aborted
    let e = req_err(&t, &format!("aws2-s3://{}?multiPartUpload=true&partSize=1048576&{}", bucket, o), Value::Bytes(data[..3 * 1024 * 1024].to_vec()), &[("CamelAwsS3Key", Value::from("small.bin"))]);
    assert!(e.message.contains("EntityTooSmall"), "{}", e);
    assert!(!keys(&t, &bucket, &o).contains(&"small.bin".to_string()));

    // streamed upload from a file (multipart) and streamed download into a file
    let dir = std::env::temp_dir().join(sim::unique("sim-s3"));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("src.bin");
    std::fs::write(&src, &data).unwrap();
    let ex = req(&t, &format!("aws2-s3://{}?multiPartUpload=true&partSize=5242880&{}", bucket, o), "", &[("CamelAwsS3Key", Value::from("from-file.bin")), ("CamelAwsS3SourceFile", Value::from(src.to_string_lossy().to_string()))]);
    assert_eq!(ex.header("CamelAwsS3ContentLength").unwrap().as_i64(), Some(data.len() as i64));
    let dst = dir.join("dst.bin");
    req(&t, &format!("aws2-s3://{}?operation=getObject&{}", bucket, o), "", &[("CamelAwsS3Key", Value::from("from-file.bin")), ("CamelAwsS3DownloadFile", Value::from(dst.to_string_lossy().to_string()))]);
    assert_eq!(std::fs::read(&dst).unwrap(), data);
    let _ = std::fs::remove_dir_all(&dir);

    // streaming upload mode: batches of messages become objects
    let logs = sim::unique("logs");
    let ctx2 = context();
    let o2 = o.clone();
    let l2 = logs.clone();
    ctx2.add_routes(move |b| {
        b.from("direct:log").to(&format!("aws2-s3://{}?autoCreateBucket=true&streamingUploadMode=true&keyName=app.log&batchMessageNumber=2&{}", l2, o2));
    })
    .unwrap();
    ctx2.start().unwrap();
    let t2 = ctx2.create_producer_template();
    for i in 0..3 {
        t2.send_body("direct:log", format!("line-{}\n", i)).unwrap();
    }
    ctx2.stop().unwrap();
    assert_eq!(keys(&t, &logs, &o), vec!["app-1.log", "app.log"]);
    let first = req(&t, &format!("aws2-s3://{}?operation=getObject&{}", logs, o), "", &[("CamelAwsS3Key", Value::from("app.log"))]);
    assert_eq!(first.body_as::<String>().unwrap(), "line-0\nline-1\n");
}

#[test]
fn sim_aws_s3_consumer_and_https() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let inbox = sim::unique("inbox");
    let done = sim::unique("done");
    for b in [&inbox, &done] {
        t.send_body(&format!("aws2-s3://{}?operation=createBucket&{}", b, o), "").unwrap();
    }
    for (k, v) in [("orders/1.json", "{\"id\":1}"), ("orders/2.json", "{\"id\":2}"), ("other/x.txt", "ignored")] {
        t.send_body_and_header(&format!("aws2-s3://{}?{}", inbox, o), v, "CamelAwsS3Key", k).unwrap();
    }
    let (o2, i2, d2) = (o.clone(), inbox.clone(), done.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-s3://{}?prefix=orders/&moveAfterRead=true&destinationBucket={}&destinationBucketPrefix=done-&delay=50&initialDelay=0&{}", i2, d2, o2)).to("mock:orders");
    })
    .unwrap();
    let orders = ctx.mock_endpoint("mock:orders").unwrap();
    orders.expected_bodies_received_in_any_order(vec!["{\"id\":1}", "{\"id\":2}"]);
    orders.all_messages(|ex| ex.header("CamelAwsS3Key").is_some_and(|k| k.to_string().starts_with("orders/")) && ex.header("CamelAwsS3ETag").is_some() && ex.header("CamelAwsS3ContentLength").and_then(|v| v.as_i64()) == Some(8));
    ctx.start().unwrap();
    orders.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    wait_until("objects moved", || keys(&t, &inbox, &o) == vec!["other/x.txt"] && keys(&t, &done, &o).len() == 2);
    assert_eq!(keys(&t, &done, &o), vec!["done-orders/1.json", "done-orders/2.json"]);
    ctx.stop().unwrap();
}

// ----------------------------------------------------------------------------- SQS / SNS

#[test]
fn sim_aws_sqs_producer_consumer_batches_fifo_and_errors() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let q = sim::unique("orders");
    let ex = req(&t, &format!("aws2-sqs://{}?autoCreateQueue=true&{}", q, o), "order-1", &[("customer", Value::from("ACME")), ("quantity", Value::Int(3)), ("CamelInternal", Value::from("dropped"))]);
    assert!(!s(ex.header("CamelAwsSqsMessageId").unwrap()).is_empty());
    assert_eq!(s(ex.header("CamelAwsSqsMD5OfBody").unwrap()), cameleer_cloud::util::md5_hex(b"order-1"));
    let r = t.request_body(&format!("aws2-sqs://{}?operation=sendBatchMessage&{}", q, o), Value::from(vec!["b1", "b2"])).unwrap();
    assert_eq!(r.get("successful").unwrap().as_array().unwrap().len(), 2);
    let queues = t.request_body_and_header(&format!("aws2-sqs://{}?operation=listQueues&{}", q, o), "", "CamelAwsSqsPrefix", q.as_str()).unwrap();
    assert_eq!(queues.as_array().unwrap().len(), 1);
    // consumer: attributes and message attributes as moto returns them; processed messages deleted
    let (o2, q2) = (o.clone(), q.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-sqs://{}?maxMessagesPerPoll=10&waitTimeSeconds=1&delay=50&initialDelay=0&attributeNames=All&messageAttributeNames=All&{}", q2, o2)).route_id("sqs").to("mock:sqs");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:sqs").unwrap();
    m.expected_bodies_received_in_any_order(vec!["order-1", "b1", "b2"]);
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    let first = m.received_exchanges().into_iter().find(|e| e.body_as::<String>().unwrap() == "order-1").unwrap();
    assert_eq!(s(first.header("customer").unwrap()), "ACME");
    assert_eq!(first.header("quantity").unwrap(), &Value::Int(3));
    assert!(first.header("CamelAwsSqsAttributes").unwrap().get("SentTimestamp").is_some());
    assert!(first.header("CamelAwsSqsReceiptHandle").is_some());
    ctx.stop_route("sqs").unwrap();
    // failed exchanges are redelivered after the visibility timeout
    let work = sim::unique("work");
    t.send_body(&format!("aws2-sqs://{}?autoCreateQueue=true&{}", work, o), "poison").unwrap();
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let a2 = attempts.clone();
    let (o3, w3) = (o.clone(), work.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-sqs://{}?visibilityTimeout=1&waitTimeSeconds=1&delay=50&initialDelay=0&{}", w3, o3)).route_id("work").process(move |_ex: &mut Exchange| {
            a2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(CamelError::runtime("poison"))
        });
    })
    .unwrap();
    wait_until("redelivery", || attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    ctx.stop_route("work").unwrap();
    // FIFO queue: group id strategy and content-based deduplication
    let fifo = format!("{}.fifo", sim::unique("jobs"));
    let fo = format!("aws2-sqs://{}?autoCreateQueue=true&messageGroupIdStrategy=useConstant&messageDeduplicationIdStrategy=useContentBasedDeduplication&{}", fifo, o);
    let ex = req(&t, &fo, "same", &[]);
    assert!(ex.header("CamelAwsSqsMessageId").is_some());
    // purge and delete
    t.request_body(&format!("aws2-sqs://{}?operation=purgeQueue&{}", q, o), "").unwrap();
    t.request_body(&format!("aws2-sqs://{}?operation=deleteQueue&{}", q, o), "").unwrap();
    let e = req_err(&t, &format!("aws2-sqs://{}?{}", sim::unique("missing"), o), "x", &[]);
    assert!(e.is_a("SqsException") && (e.message.contains("QueueDoesNotExist") || e.message.contains("NonExistentQueue")), "{}", e);
    let e = req_err(&t, &format!("aws2-sqs://{}?operation=deleteMessage&{}", work, o), "", &[("CamelAwsSqsReceiptHandle", Value::from("bogus"))]);
    assert!(e.is_a("SqsException"), "{}", e);
    ctx.stop().unwrap();
}

#[test]
fn sim_aws_sns_publish_and_fan_out_to_sqs() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let topic = sim::unique("alerts");
    let queue = sim::unique("alerts-q");
    t.send_body(&format!("aws2-sqs://{}?autoCreateQueue=true&operation=listQueues&{}", queue, o), "").unwrap();
    let queue_arn = format!("arn:aws:sqs:us-east-1:{}:{}", ACCOUNT, queue);
    let queue_url = format!("http://127.0.0.1:15000/{}/{}", ACCOUNT, queue);
    // autoCreateTopic + subscribeSNStoSQS delivers to the queue
    let ex = req(
        &t,
        &format!("aws2-sns://{}?autoCreateTopic=true&subscribeSNStoSQS=true&queueArn={}&queueUrl={}&subject=Alert&{}", topic, queue_arn, queue_url, o),
        "disk full",
        &[("severity", Value::from("high"))],
    );
    assert!(!s(ex.header("CamelAwsSnsMessageId").unwrap()).is_empty());
    // by ARN, message structure json
    let arn = format!("arn:aws:sns:us-east-1:{}:{}", ACCOUNT, topic);
    t.send_body_and_headers(&format!("aws2-sns://{}?{}", arn, o), "{\"default\":\"d\",\"sqs\":\"for sqs\"}", vec![("CamelAwsSnsMessageStructure", Value::from("json"))]).unwrap();
    // the notifications arrive in SQS as SNS envelopes
    let (o2, q2) = (o.clone(), queue.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-sqs://{}?waitTimeSeconds=1&delay=50&initialDelay=0&{}", q2, o2)).to("mock:sns");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:sns").unwrap();
    m.expected_message_count(2);
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    let mut msgs: Vec<Value> = m.received_exchanges().iter().map(|e| json(&e.body_as::<String>().unwrap())).collect();
    msgs.sort_by_key(|v| s(v.get("Message").unwrap()));
    assert_eq!(s(msgs[0].get("Message").unwrap()), "disk full");
    assert_eq!(s(msgs[0].get("Subject").unwrap()), "Alert");
    assert_eq!(s(msgs[0].get("MessageAttributes").unwrap().get("severity").unwrap().get("Value").unwrap()), "high");
    assert_eq!(s(msgs[1].get("Message").unwrap()), "for sqs", "MessageStructure=json picks the sqs entry");
    let e = req_err(&t, &format!("aws2-sns://{}?{}", sim::unique("nope"), o), "x", &[]);
    assert!(e.is_a("SnsException"), "{}", e);
    ctx.stop().unwrap();
}

// ----------------------------------------------------------------------------- DynamoDB

#[test]
fn sim_aws_dynamodb_items_expressions_batches_and_streams() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let table = sim::unique("orders");
    let u = |op: &str| format!("aws2-ddb://{}?operation={}&keyAttributeName=customer&keyAttributeType=S&{}", table, op, o);
    // the table is created on first use
    for (c, total, status) in [("ann", 10, "NEW"), ("bob", 25, "SHIPPED"), ("cid", 40, "NEW")] {
        req(&t, &u("PutItem"), "", &[("CamelAwsDdbItem", Value::object([("customer", Value::from(c)), ("total", Value::Int(total)), ("status", Value::from(status)), ("tags", Value::from(vec!["a", "b"]))]))]);
    }
    let ex = req(&t, &u("GetItem"), "", &[("CamelAwsDdbKey", Value::object([("customer", "bob")]))]);
    let item = ex.header("CamelAwsDdbAttributes").unwrap();
    assert_eq!(item.get("total").unwrap(), &Value::Int(25));
    assert_eq!(item.get("tags").unwrap(), &Value::from(vec!["a", "b"]));
    // UpdateExpression with condition and ALL_NEW
    let up = req(
        &t,
        &u("UpdateItem"),
        "",
        &[
            ("CamelAwsDdbKey", Value::object([("customer", Value::from("ann"))])),
            // `total` and `status` are DynamoDB reserved words (moto enforces the list, as AWS does)
            ("CamelAwsDdbUpdateExpression", Value::from("SET #t = #t + :inc, #s = :st, notes = list_append(if_not_exists(notes, :empty), :n)")),
            ("CamelAwsDdbConditionExpression", Value::from("attribute_exists(customer) AND #t < :max")),
            ("CamelAwsDdbExpressionAttributeNames", Value::object([("#s", Value::from("status")), ("#t", Value::from("total"))])),
            ("CamelAwsDdbExpressionAttributeValues", json(r#"{":inc":5,":st":"PAID",":empty":[],":n":["paid online"],":max":100}"#)),
            ("CamelAwsDdbReturnValues", Value::from("ALL_NEW")),
        ],
    );
    let attrs = up.header("CamelAwsDdbAttributes").unwrap();
    assert_eq!(attrs.get("total").unwrap(), &Value::Int(15));
    assert_eq!(s(attrs.get("status").unwrap()), "PAID");
    assert_eq!(attrs.get("notes").unwrap(), &Value::from(vec!["paid online"]));
    let e = req_err(
        &t,
        &u("UpdateItem"),
        "",
        &[("CamelAwsDdbKey", Value::object([("customer", Value::from("ann"))])), ("CamelAwsDdbUpdateExpression", Value::from("SET #t = :z")), ("CamelAwsDdbConditionExpression", Value::from("#t > :max")), ("CamelAwsDdbExpressionAttributeNames", Value::object([("#t", Value::from("total"))])), ("CamelAwsDdbExpressionAttributeValues", json(r#"{":z":0,":max":100}"#))],
    );
    assert!(e.message.contains("ConditionalCheckFailedException"), "{}", e.message);
    // Query by key, Scan with filter + projection
    let q = req(&t, &u("Query"), "", &[("CamelAwsDdbKeyConditionExpression", Value::from("customer = :c")), ("CamelAwsDdbExpressionAttributeValues", json(r#"{":c":"cid"}"#))]);
    assert_eq!(q.header("CamelAwsDdbCount").unwrap(), &Value::Int(1));
    let sc = req(&t, &u("Scan"), "", &[("CamelAwsDdbFilterExpression", Value::from("#t >= :min")), ("CamelAwsDdbProjectionExpression", Value::from("customer, #t")), ("CamelAwsDdbExpressionAttributeNames", Value::object([("#t", Value::from("total"))])), ("CamelAwsDdbExpressionAttributeValues", json(r#"{":min":20}"#))]);
    let items = sc.header("CamelAwsDdbItems").unwrap().as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|i| i.get("status").is_none() && i.get("total").is_some()));
    assert_eq!(sc.header("CamelAwsDdbScannedCount").unwrap(), &Value::Int(3));
    // legacy ScanFilter maps
    let sc = req(&t, &u("Scan"), "", &[("CamelAwsDdbScanFilter", Value::object([("total", Value::object([("ComparisonOperator", Value::from("GT")), ("AttributeValueList", Value::Array(vec![Value::Int(20)]))]))]))]);
    assert_eq!(sc.header("CamelAwsDdbCount").unwrap(), &Value::Int(2));
    // BatchGetItems
    let bg = req(&t, &u("BatchGetItems"), "", &[("CamelAwsDdbBatchItems", Value::object([(table.as_str(), Value::object([("Keys", Value::Array(vec![Value::object([("customer", Value::from("bob"))]), Value::object([("customer", Value::from("zed"))])]))]))]))]);
    assert_eq!(bg.header("CamelAwsDdbBatchResponse").unwrap().get(&table).unwrap().as_array().unwrap().len(), 1);
    // UpdateTable enabling a stream, then the ddbstream consumer sees the changes
    let ut = req(&t, &u("UpdateTable"), "", &[("CamelAwsDdbReadCapacity", Value::Int(7)), ("CamelAwsDdbWriteCapacity", Value::Int(3)), ("CamelAwsDdbStreamViewType", Value::from("NEW_AND_OLD_IMAGES"))]);
    assert!(ut.header("CamelAwsDdbStreamArn").is_some_and(|a| s(a).contains(&format!("table/{}/stream/", table))), "{:?}", ut.header("CamelAwsDdbStreamArn"));
    let (o2, t2) = (o.clone(), table.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-ddbstream://{}?streamIteratorType=FROM_START&delay=50&initialDelay=0&{}", t2, o2)).to("mock:stream");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:stream").unwrap();
    m.expected_message_count(3);
    ctx.start().unwrap();
    req(&t, &u("PutItem"), "", &[("CamelAwsDdbItem", Value::object([("customer", Value::from("dan")), ("total", Value::Int(1))]))]);
    req(&t, &u("UpdateItem"), "", &[("CamelAwsDdbKey", Value::object([("customer", Value::from("dan"))])), ("CamelAwsDdbUpdateExpression", Value::from("SET #t = #t + :one")), ("CamelAwsDdbExpressionAttributeNames", Value::object([("#t", Value::from("total"))])), ("CamelAwsDdbExpressionAttributeValues", json(r#"{":one":1}"#))]);
    req(&t, &u("DeleteItem"), "", &[("CamelAwsDdbKey", Value::object([("customer", Value::from("dan"))]))]);
    m.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    let rx = m.received_exchanges();
    let names: Vec<String> = rx.iter().map(|e| s(e.header("CamelAwsDdbStreamEventName").unwrap())).collect();
    assert_eq!(names, vec!["INSERT", "MODIFY", "REMOVE"]);
    let modify = rx[1].body().get("dynamodb").unwrap().clone();
    assert_eq!(modify.get("OldImage").unwrap().get("total").unwrap(), &Value::Int(1));
    assert_eq!(modify.get("NewImage").unwrap().get("total").unwrap(), &Value::Int(2));
    ctx.stop().unwrap();
    // DescribeTable, DeleteTable
    let d = req(&t, &u("DescribeTable"), "", &[]);
    assert_eq!(d.header("CamelAwsDdbItemCount").unwrap(), &Value::Int(3));
    req(&t, &u("DeleteTable"), "", &[]);
    let e = req_err(&t, &format!("aws2-ddb://{}?operation=GetItem&{}", table, o), "", &[("CamelAwsDdbKey", Value::object([("customer", "ann")]))]);
    assert!(e.is_a("DynamoDbException") && e.message.contains("ResourceNotFoundException"), "{}", e);
}

// ----------------------------------------------------------------------------- Kinesis / Firehose

#[test]
fn sim_aws_kinesis_put_and_consume_every_shard_and_firehose() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let stream = sim::unique("events");
    let uri = format!("aws2-kinesis://{}?shardCount=2&{}", stream, o);
    req(&t, &uri, "", &[("CamelAwsKinesisOperation", Value::from("createStream"))]);
    let ex = req(&t, &uri, "first", &[("CamelAwsKinesisPartitionKey", Value::from("user-1"))]);
    assert!(ex.header("CamelAwsKinesisSequenceNumber").is_some_and(|v| !s(v).is_empty()));
    assert!(s(ex.header("CamelAwsKinesisShardId").unwrap()).starts_with("shardId-"));
    let items = Value::Array(vec![Value::from("second"), Value::object([("data", "third"), ("partitionKey", "user-3")]), Value::from("fourth")]);
    let ex = req(&t, &uri, items, &[("CamelAwsKinesisPartitionKey", Value::from("batch"))]);
    assert_eq!(ex.header("CamelAwsKinesisFailedRecordCount").unwrap().as_i64(), Some(0));
    let shards = t.request_body_and_header(&uri, "", "CamelAwsKinesisOperation", "listShards").unwrap();
    assert_eq!(shards.as_array().unwrap().len(), 2, "{}", shards.to_json());
    let (o2, s2) = (o.clone(), stream.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-kinesis://{}?maxResultsPerRequest=10&delay=50&initialDelay=0&{}", s2, o2)).to("mock:records");
    })
    .unwrap();
    let records = ctx.mock_endpoint("mock:records").unwrap();
    records.expected_bodies_received_in_any_order(vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec(), b"fourth".to_vec()]);
    records.all_messages(|ex| ex.header("CamelAwsKinesisPartitionKey").is_some() && ex.header("CamelAwsKinesisShardId").is_some() && ex.header("CamelAwsKinesisApproximateArrivalTimestamp").is_some());
    ctx.start().unwrap();
    records.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    ctx.stop().unwrap();
    req(&t, &uri, "", &[("CamelAwsKinesisOperation", Value::from("deleteStream"))]);

    // Firehose (moto delivers to an S3 destination)
    let bucket = sim::unique("firehose");
    t.send_body(&format!("aws2-s3://{}?operation=createBucket&{}", bucket, o), "").unwrap();
    let fh = sim::unique("logs");
    let furi = format!("aws2-kinesis-firehose://{}?{}", fh, o);
    // the request body is merged into CreateDeliveryStream (a destination is mandatory)
    let dest = format!(r#"{{"ExtendedS3DestinationConfiguration":{{"RoleARN":"arn:aws:iam::{}:role/firehose","BucketARN":"arn:aws:s3:::{}","Prefix":"out/","BufferingHints":{{"SizeInMBs":1,"IntervalInSeconds":60}}}}}}"#, ACCOUNT, bucket);
    let created = req(&t, &furi, dest, &[("CamelAwsKinesisFirehoseOperation", Value::from("createDeliveryStream"))]);
    assert!(s(created.body()).ends_with(&format!("deliverystream/{}", fh)), "{}", s(created.body()));
    let ex = req(&t, &furi, "line 1\n", &[]);
    assert!(ex.header("CamelAwsKinesisFirehoseRecordId").is_some_and(|v| !s(v).is_empty()));
    let body = t.request_body(&furi, Value::Array(vec![Value::from("line 2\n"), Value::from("line 3\n")])).unwrap();
    assert_eq!(body.as_array().unwrap().len(), 2);
    let desc = t.request_body_and_header(&furi, "", "CamelAwsKinesisFirehoseOperation", "describeDeliveryStream").unwrap();
    assert_eq!(s(desc.get("DeliveryStreamStatus").unwrap()), "ACTIVE");
    // moto wrote the records to the bucket
    wait_until("firehose objects", || !keys(&t, &bucket, &o).is_empty());
    t.request_body_and_header(&furi, "", "CamelAwsKinesisFirehoseOperation", "deleteDeliveryStream").unwrap();
}

// ----------------------------------------------------------------------------- EventBridge

#[test]
fn sim_aws_eventbridge_rules_targets_and_delivery_to_sqs() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let queue = sim::unique("orders-events");
    t.send_body(&format!("aws2-sqs://{}?autoCreateQueue=true&operation=listQueues&{}", queue, o), "").unwrap();
    let queue_arn = format!("arn:aws:sqs:us-east-1:{}:{}", ACCOUNT, queue);
    let rule = sim::unique("orders");
    let uri = format!("aws2-eventbridge://default?{}", o);
    let arn = req(&t, &format!("{}&operation=putRule", uri), "", &[("CamelAwsEventbridgeRuleName", Value::from(rule.clone())), ("CamelAwsEventbridgeEventPattern", Value::from(r#"{"source":["shop.orders"],"detail-type":["OrderPlaced"]}"#))]);
    assert!(s(arn.body()).ends_with(&format!("rule/{}", rule)), "{}", s(arn.body()));
    req(&t, &format!("{}&operation=putTargets", uri), "", &[("CamelAwsEventbridgeRuleName", Value::from(rule.clone())), ("CamelAwsEventbridgeTargets", Value::Array(vec![Value::object([("id", Value::from("q")), ("arn", Value::from(queue_arn.clone()))])]))]);
    for (src, dt, id) in [("shop.orders", "OrderPlaced", 42), ("shop.billing", "InvoiceSent", 7)] {
        let ex = req(&t, &format!("{}&operation=putEvent", uri), Value::object([("id", id)]), &[("CamelAwsEventbridgeEventSource", Value::from(src)), ("CamelAwsEventbridgeEventDetailType", Value::from(dt))]);
        assert_eq!(ex.header("CamelAwsEventbridgeFailedEntryCount").unwrap().as_i64(), Some(0));
    }
    let (o2, q2) = (o.clone(), queue.clone());
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-sqs://{}?waitTimeSeconds=1&delay=50&initialDelay=0&{}", q2, o2)).to("mock:events");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:events").unwrap();
    m.expected_message_count(1);
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(30)).unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(m.received_counter(), 1, "only the matching event is routed");
    let ev = json(&m.received_exchanges()[0].body_as::<String>().unwrap());
    assert_eq!(ev.get("detail").unwrap().get("id").unwrap(), &Value::Int(42));
    ctx.stop().unwrap();
    let r = t.request_body_and_header(&format!("{}&operation=describeRule", uri), "", "CamelAwsEventbridgeRuleName", rule.as_str()).unwrap();
    assert_eq!(s(r.get("State").unwrap()), "ENABLED");
    let targets = t.request_body_and_header(&format!("{}&operation=listTargetsByRule", uri), "", "CamelAwsEventbridgeRuleName", rule.as_str()).unwrap();
    assert_eq!(s(targets.as_array().unwrap()[0].get("Arn").unwrap()), queue_arn);
    let names = t.request_body_and_header(&format!("{}&operation=listRuleNamesByTarget", uri), "", "CamelAwsEventbridgeTargetArn", queue_arn.as_str()).unwrap();
    assert_eq!(names, Value::Array(vec![Value::from(rule.clone())]));
    t.request_body_and_header(&format!("{}&operation=disableRule", uri), "", "CamelAwsEventbridgeRuleName", rule.as_str()).unwrap();
    req(&t, &format!("{}&operation=removeTargets", uri), "", &[("CamelAwsEventbridgeRuleName", Value::from(rule.clone())), ("CamelAwsEventbridgeTargetsIds", Value::from("q"))]);
    t.request_body_and_header(&format!("{}&operation=deleteRule", uri), "", "CamelAwsEventbridgeRuleName", rule.as_str()).unwrap();
}

// ----------------------------------------------------------------------------- Secrets / KMS / STS / IAM / EC2

#[test]
fn sim_aws_secrets_manager_and_properties_function() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    let uri = format!("aws-secrets-manager://vault?{}", o);
    let name = sim::unique("db");
    let ex = req(&t, &format!("{}&operation=createSecret", uri), r#"{"user":"scott","port":5432}"#, &[("CamelAwsSecretsManagerSecretName", Value::from(name.clone())), ("CamelAwsSecretsManagerSecretDescription", Value::from("database"))]);
    let arn = s(ex.body());
    assert!(arn.starts_with(&format!("arn:aws:secretsmanager:us-east-1:{}:secret:{}", ACCOUNT, name)), "{}", arn);
    let v = t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", name.as_str()).unwrap();
    assert_eq!(s(&v), r#"{"user":"scott","port":5432}"#);
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", arn.as_str()).unwrap(), v);
    req(&t, &format!("{}&operation=updateSecret", uri), "rotated-value", &[("CamelAwsSecretsManagerSecretId", Value::from(name.clone()))]);
    assert_eq!(s(&t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", name.as_str()).unwrap()), "rotated-value");
    let d = t.request_body_and_header(&format!("{}&operation=describeSecret", uri), "", "CamelAwsSecretsManagerSecretId", name.as_str()).unwrap();
    assert_eq!(s(d.get("Description").unwrap()), "database");
    let list = t.request_body(&format!("{}&operation=listSecrets", uri), "").unwrap();
    assert!(list.to_json().contains(&name));
    let blob = sim::unique("blob");
    req(&t, &format!("{}&operation=createSecret&binaryPayload=true", uri), Value::Bytes(vec![0, 1, 2, 255]), &[("CamelAwsSecretsManagerSecretName", Value::from(blob.clone()))]);
    assert_eq!(t.request_body_and_header(&uri, "", "CamelAwsSecretsManagerSecretId", blob.as_str()).unwrap(), Value::Bytes(vec![0, 1, 2, 255]));
    req(&t, &format!("{}&operation=deleteSecret", uri), "", &[("CamelAwsSecretsManagerSecretId", Value::from(name.clone()))]);
    let e = req_err(&t, &uri, "", &[("CamelAwsSecretsManagerSecretId", Value::from(name.clone()))]);
    assert!(e.is_a("SecretsManagerException"), "{}", e);
    req(&t, &format!("{}&operation=restoreSecret", uri), "", &[("CamelAwsSecretsManagerSecretId", Value::from(name.clone()))]);
    // {{aws:...}} properties function
    let cfg = sim::unique("cfg");
    req(&t, &format!("{}&operation=createSecret", uri), r#"{"user":"scott","port":5432}"#, &[("CamelAwsSecretsManagerSecretName", Value::from(cfg.clone()))]);
    let p = ctx.properties();
    p.set("camel.vault.aws.accessKey", "testing");
    p.set("camel.vault.aws.secretKey", "testing");
    p.set("camel.vault.aws.region", "us-east-1");
    p.set("camel.vault.aws.uriEndpointOverride", "http://127.0.0.1:15000");
    p.set("camel.vault.aws.overrideEndpoint", "true");
    assert_eq!(ctx.resolve_placeholders(&format!("jdbc://{{{{aws:{0}/user}}}}@host:{{{{aws:{0}/port}}}}", cfg)).unwrap(), "jdbc://scott@host:5432");
    assert_eq!(ctx.resolve_placeholders(&format!("{{{{aws:{}:fallback}}}}", sim::unique("missing"))).unwrap(), "fallback");
}

#[test]
fn sim_aws_kms_sts_iam_ec2() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    // KMS
    let kms = format!("aws2-kms://kms?{}", o);
    let key = req(&t, &format!("{}&operation=createKey", kms), "", &[("CamelAwsKMSDescription", Value::from("sim key"))]);
    let key_id = s(key.body().get("KeyId").unwrap_or(key.body()));
    assert!(!key_id.is_empty());
    let keys = t.request_body(&format!("{}&operation=listKeys", kms), "").unwrap();
    assert!(keys.to_json().contains(&key_id), "{}", keys.to_json());
    let d = req(&t, &format!("{}&operation=describeKey", kms), "", &[("CamelAwsKMSKeyId", Value::from(key_id.clone()))]);
    assert!(d.body().to_json().contains("sim key"), "{}", d.body().to_json());
    req(&t, &format!("{}&operation=disableKey", kms), "", &[("CamelAwsKMSKeyId", Value::from(key_id.clone()))]);
    req(&t, &format!("{}&operation=enableKey", kms), "", &[("CamelAwsKMSKeyId", Value::from(key_id.clone()))]);
    let enc = req(&t, &format!("{}&operation=encrypt", kms), "top secret", &[("CamelAwsKMSKeyId", Value::from(key_id.clone()))]);
    let dec = req(&t, &format!("{}&operation=decrypt", kms), enc.body().clone(), &[("CamelAwsKMSKeyId", Value::from(key_id.clone()))]);
    assert_eq!(dec.body_as::<String>().unwrap(), "top secret");
    req(&t, &format!("{}&operation=scheduleKeyDeletion", kms), "", &[("CamelAwsKMSKeyId", Value::from(key_id.clone())), ("CamelAwsKMSPendingWindowInDays", Value::Int(7))]);

    // STS
    let sts = format!("aws2-sts://default?{}", o);
    let creds = req(&t, &sts, "", &[("CamelAwsStsRoleArn", Value::from(format!("arn:aws:iam::{}:role/reader", ACCOUNT))), ("CamelAwsStsRoleSessionName", Value::from("camel"))]).body().clone();
    assert!(s(creds.get("accessKeyId").unwrap()).starts_with("ASIA"), "{}", creds.to_json());
    assert!(s(creds.get("assumedRoleArn").unwrap()).ends_with("assumed-role/reader/camel"), "{}", creds.to_json());
    let st = t.request_body_and_header(&sts, "", "CamelAwsStsOperation", "getSessionToken").unwrap();
    assert!(s(st.get("sessionToken").unwrap()).len() > 10);
    let ft = req(&t, &sts, "", &[("CamelAwsStsOperation", Value::from("getFederationToken")), ("CamelAwsStsFederatedName", Value::from("bob"))]);
    assert!(s(ft.body().get("federatedUserArn").unwrap()).ends_with("federated-user/bob"), "{}", ft.body().to_json());

    // IAM
    let iam = format!("aws2-iam://iam?{}", o);
    let user = sim::unique("alice");
    let group = sim::unique("admins");
    let op = |o: &str, u: Option<&str>, g: Option<&str>| {
        t.request(&format!("{}&operation={}", iam, o), |ex| {
            if let Some(u) = u {
                ex.set_header("CamelAwsIAMUsername", u);
            }
            if let Some(g) = g {
                ex.set_header("CamelAwsIAMGroupName", g);
            }
        })
        .map(|e| e.body().clone())
    };
    let u = op("createUser", Some(&user), None).unwrap();
    assert_eq!(s(u.get("arn").unwrap()), format!("arn:aws:iam::{}:user/{}", ACCOUNT, user));
    let e = op("createUser", Some(&user), None).unwrap_err().root_cause().clone();
    assert!(e.is_a("IamException") && e.message.contains("EntityAlreadyExists"), "{}", e);
    assert_eq!(s(op("getUser", Some(&user), None).unwrap().get("userName").unwrap()), user);
    assert!(op("listUsers", None, None).unwrap().to_json().contains(&user));
    op("createGroup", None, Some(&group)).unwrap();
    op("addUserToGroup", Some(&user), Some(&group)).unwrap();
    assert!(op("listGroups", None, None).unwrap().to_json().contains(&group));
    let k = op("createAccessKey", Some(&user), None).unwrap();
    assert!(s(k.get("accessKeyId").unwrap()).starts_with("AKIA"));
    assert_eq!(op("listAccessKeys", Some(&user), None).unwrap().as_array().unwrap().len(), 1);
    op("removeUserFromGroup", Some(&user), Some(&group)).unwrap();
    req(&t, &format!("{}&operation=deleteAccessKey", iam), "", &[("CamelAwsIAMUsername", Value::from(user.clone())), ("CamelAwsIAMAccessKeyID", Value::from(s(k.get("accessKeyId").unwrap())))]);
    op("deleteUser", Some(&user), None).unwrap();
    let e = op("getUser", Some(&user), None).unwrap_err().root_cause().clone();
    assert!(e.message.contains("NoSuchEntity"), "{}", e);

    // EC2
    let ec2 = format!("aws2-ec2://ec2?{}", o);
    let started = req(&t, &format!("{}&operation=createAndRunInstances", ec2), "", &[("CamelAwsEC2ImageId", Value::from("ami-12c6146b")), ("CamelAwsEC2InstanceType", Value::from("t3.micro")), ("CamelAwsEC2InstanceMinCount", Value::Int(2)), ("CamelAwsEC2InstanceMaxCount", Value::Int(2))]).body().clone();
    let ids: Vec<String> = started.as_array().unwrap().iter().map(|i| s(i.get("instanceId").unwrap())).collect();
    assert_eq!(ids.len(), 2);
    let one = t.request_body_and_header(&format!("{}&operation=describeInstances", ec2), "", "CamelAwsEC2InstancesIds", Value::from(vec![ids[0].clone()])).unwrap();
    let inst = &one.as_array().unwrap()[0];
    assert_eq!((s(inst.get("instanceType").unwrap()), s(inst.get("state").unwrap())), ("t3.micro".to_string(), "running".to_string()));
    let changes = t.request_body_and_header(&format!("{}&operation=stopInstances", ec2), "", "CamelAwsEC2InstancesIds", ids[0].as_str()).unwrap();
    assert!(matches!(s(changes.as_array().unwrap()[0].get("currentState").unwrap()).as_str(), "stopping" | "stopped"));
    t.request_body_and_header(&format!("{}&operation=startInstances", ec2), "", "CamelAwsEC2InstancesIds", ids[0].as_str()).unwrap();
    req(&t, &format!("{}&operation=createTags", ec2), "", &[("CamelAwsEC2InstancesIds", Value::from(ids.clone())), ("CamelAwsEC2InstancesTags", Value::object([("team", "camel")]))]);
    let status = t.request_body(&format!("{}&operation=describeInstancesStatus", ec2), "").unwrap();
    assert!(status.as_array().is_some());
    t.request_body_and_header(&format!("{}&operation=terminateInstances", ec2), "", "CamelAwsEC2InstancesIds", Value::from(ids.clone())).unwrap();
    let e = req_err(&t, &format!("{}&operation=startInstances", ec2), "", &[("CamelAwsEC2InstancesIds", Value::from("i-1234567890abcdef0"))]);
    assert!(e.is_a("Ec2Exception") && e.message.contains("InvalidInstanceID.NotFound"), "{}", e);
}

// ----------------------------------------------------------------------------- CloudWatch / SES / Step Functions / Lambda / Athena

#[test]
fn sim_aws_cloudwatch_ses_step_functions_lambda_athena() {
    if !sim::enabled() {
        return;
    }
    let ctx = context();
    let o = opts();
    let t = ctx.create_producer_template();
    // CloudWatch metrics
    let ns = sim::unique("shop");
    let cw = format!("aws2-cw://{}?name=orders&unit=Count&{}", ns, o);
    req(&t, &cw, "", &[("CamelAwsCwMetricValue", Value::from(3.5)), ("CamelAwsCwMetricDimensions", Value::object([("region", "eu"), ("tier", "gold")]))]);
    req(&t, &cw, "", &[("CamelAwsCwMetricName", Value::from("latency")), ("CamelAwsCwMetricUnit", Value::from("Milliseconds")), ("CamelAwsCwMetricValue", Value::Int(120)), ("CamelAwsCwMetricTimestamp", Value::Int(1_700_000_000_000))]);
    t.request_body(&cw, Value::Array(vec![Value::object([("name", Value::from("a")), ("value", Value::from(1))]), Value::object([("name", Value::from("b")), ("value", Value::from(2))])])).unwrap();
    let names = list_metric_names(&ns);
    assert_eq!(names, vec!["a", "b", "latency", "orders"], "metrics stored by moto");

    // SES: moto only sends from verified identities
    let sender = format!("{}@example.com", sim::unique("noreply"));
    let ses_api = format!("aws2-ses://{}?to=a@example.com,b@example.com&subject=Hello&{}", sender, o);
    let e = req_err(&t, &ses_api, "before verification", &[]);
    assert!(e.message.contains("MessageRejected"), "{}", e);
    aws_query("ses", &format!("Action=VerifyEmailIdentity&EmailAddress={}&Version=2010-12-01", sender.replace('@', "%40")));
    let ex = req(&t, &ses_api, "plain text body", &[]);
    assert!(ex.header("CamelAwsSesMessageId").is_some_and(|v| !s(v).is_empty()));
    let ex2 = req(&t, &ses_api, "<b>hi</b>", &[("CamelAwsSesTo", Value::Array(vec![Value::from("c@example.com")])), ("CamelAwsSesCc", Value::from("d@example.com")), ("CamelAwsSesSubject", Value::from("Override")), ("CamelAwsSesHtmlEmail", Value::Bool(true)), ("CamelAwsSesReplyToAddresses", Value::from("reply@example.com"))]);
    assert_ne!(ex2.header("CamelAwsSesMessageId"), ex.header("CamelAwsSesMessageId"));
    let raw = format!("From: {}\r\nTo: a@example.com\r\nSubject: raw\r\n\r\nraw body", sender).into_bytes();
    t.request_body(&ses_api, Value::Bytes(raw)).unwrap();

    // Step Functions
    let sfn = format!("aws2-step-functions://sfn?{}", o);
    let name = sim::unique("hello");
    let def = r#"{"StartAt":"Hello","States":{"Hello":{"Type":"Pass","Result":{"greeting":"hi"},"Next":"Done"},"Done":{"Type":"Succeed"}}}"#;
    let arn = s(req(&t, &format!("{}&operation=createStateMachine", sfn), "", &[("CamelAwsStepFunctionsStateMachineName", Value::from(name.clone())), ("CamelAwsStepFunctionsStateMachineDefinition", Value::from(def)), ("CamelAwsStepFunctionsStateMachineRoleArn", Value::from(format!("arn:aws:iam::{}:role/sfn", ACCOUNT)))]).body());
    assert!(arn.ends_with(&format!(":stateMachine:{}", name)), "{}", arn);
    let ex = req(&t, &format!("{}&operation=startExecution", sfn), Value::object([("x", 1)]), &[("CamelAwsStepFunctionsStateMachineArn", Value::from(arn.clone())), ("CamelAwsStepFunctionsExecutionName", Value::from("run-1"))]);
    let exec = s(ex.header("CamelAwsStepFunctionsExecutionArn").unwrap());
    assert!(exec.ends_with(&format!(":execution:{}:run-1", name)), "{}", exec);
    let d = t.request_body_and_header(&format!("{}&operation=describeExecution", sfn), "", "CamelAwsStepFunctionsExecutionArn", exec.as_str()).unwrap();
    assert!(matches!(s(d.get("status").unwrap()).as_str(), "RUNNING" | "SUCCEEDED"), "{}", d.to_json());
    let machines = t.request_body(&format!("{}&operation=listStateMachines", sfn), "").unwrap();
    assert!(machines.to_json().contains(&name));
    let execs = t.request_body_and_header(&format!("{}&operation=listExecutions", sfn), "", "CamelAwsStepFunctionsStateMachineArn", arn.as_str()).unwrap();
    assert_eq!(execs.as_array().unwrap().len(), 1);
    let e = req_err(&t, &format!("{}&operation=startExecution", sfn), "{}", &[("CamelAwsStepFunctionsStateMachineArn", Value::from(arn.clone())), ("CamelAwsStepFunctionsExecutionName", Value::from("run-1"))]);
    assert!(e.is_a("SfnException") && e.message.contains("ExecutionAlreadyExists"), "{}", e);
    t.request_body_and_header(&format!("{}&operation=deleteStateMachine", sfn), "", "CamelAwsStepFunctionsStateMachineArn", arn.as_str()).unwrap();

    // Lambda management API (moto needs Docker to invoke functions)
    let role_name = sim::unique("lambda-role");
    aws_query(
        "iam",
        &format!(
            "Action=CreateRole&RoleName={}&AssumeRolePolicyDocument={}&Version=2010-05-08",
            role_name,
            cameleer_cloud::util::uri_encode(r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#, false)
        ),
    );
    let role = format!("arn:aws:iam::{}:role/{}", ACCOUNT, role_name);
    let fname = sim::unique("greeter");
    let lu = |op: &str| format!("aws2-lambda://{}?operation={}&{}", fname, op, o);
    let created = req(&t, &lu("createFunction"), Value::Bytes(zip_with_index_js()), &[("CamelAwsLambdaRole", Value::from(role)), ("CamelAwsLambdaRuntime", Value::from("nodejs20.x")), ("CamelAwsLambdaHandler", Value::from("index.handler")), ("CamelAwsLambdaMemorySize", Value::Int(512)), ("CamelAwsLambdaEnvironmentVariables", Value::object([("GREETING", Value::from("hi"))])), ("CamelAwsLambdaTags", Value::object([("team", Value::from("camel"))]))]);
    let cfg = created.body();
    assert_eq!(s(cfg.get("FunctionName").unwrap()), fname);
    assert_eq!(cfg.get("MemorySize").unwrap(), &Value::Int(512));
    let farn = s(cfg.get("FunctionArn").unwrap());
    let v1 = req(&t, &lu("publishVersion"), "", &[("CamelAwsLambdaVersionDescription", Value::from("first"))]);
    assert_eq!(s(v1.body().get("Version").unwrap()), "1");
    assert_eq!(req(&t, &lu("listVersions"), "", &[]).body().as_array().unwrap().len(), 2);
    let alias = req(&t, &lu("createAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("prod")), ("CamelAwsLambdaFunctionVersion", Value::from("1"))]);
    assert!(s(alias.body().get("AliasArn").unwrap()).ends_with(":prod"));
    assert_eq!(s(req(&t, &lu("getAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("prod"))]).body().get("FunctionVersion").unwrap()), "1");
    assert_eq!(req(&t, &lu("listAliases"), "", &[]).body().as_array().unwrap().len(), 1);
    req(&t, &lu("deleteAlias"), "", &[("CamelAwsLambdaAliasFunctionName", Value::from("prod"))]);
    assert_eq!(s(req(&t, &lu("listTags"), "", &[("CamelAwsLambdaResourceArn", Value::from(farn.clone()))]).body().get("team").unwrap()), "camel");
    req(&t, &lu("tagResource"), "", &[("CamelAwsLambdaResourceArn", Value::from(farn.clone())), ("CamelAwsLambdaResourceTags", Value::object([("env", Value::from("dev"))]))]);
    req(&t, &lu("untagResource"), "", &[("CamelAwsLambdaResourceArn", Value::from(farn.clone())), ("CamelAwsLambdaResourceTagKeys", Value::from(vec!["team"]))]);
    assert_eq!(req(&t, &lu("listTags"), "", &[("CamelAwsLambdaResourceArn", Value::from(farn))]).body().to_json(), r#"{"env":"dev"}"#);
    let got = req(&t, &lu("getFunction"), "", &[]);
    assert_eq!(s(got.body().get("Configuration").unwrap().get("Runtime").unwrap()), "nodejs20.x");
    assert!(req(&t, &lu("listFunctions"), "", &[]).body().to_json().contains(&fname));
    req(&t, &lu("deleteFunction"), "", &[]);
    assert!(req_err(&t, &lu("getFunction"), "", &[]).message.contains("ResourceNotFoundException"));

    // Athena (moto records queries and reports them SUCCEEDED)
    let athena = format!("aws2-athena://label?outputLocation=s3://{}/results/&{}", sim::unique("athena"), o);
    let ex = req(&t, &format!("{}&operation=startQueryExecution", athena), "SELECT 1", &[]);
    let qid = ex.header("CamelAwsAthenaQueryExecutionId").map(s).unwrap_or_else(|| s(ex.body()));
    assert!(!qid.is_empty());
    let st = req(&t, &format!("{}&operation=getQueryExecution", athena), "", &[("CamelAwsAthenaQueryExecutionId", Value::from(qid.clone()))]);
    assert!(st.body().to_json().contains("SUCCEEDED"), "{}", st.body().to_json());
    let list = t.request_body(&format!("{}&operation=listQueryExecutions", athena), "").unwrap();
    assert!(list.to_json().contains(&qid), "{}", list.to_json());
}

/// A signed AWS Query-protocol call to moto (for setup steps the components do not cover).
fn aws_query(service: &str, form: &str) -> String {
    let aws = cameleer_cloud::aws::AwsConfig {
        access_key: Some("testing".into()),
        secret_key: Some("testing".into()),
        session_token: None,
        region: "us-east-1".into(),
        use_default_credentials_provider: false,
        use_profile_credentials_provider: false,
        profile_credentials_name: None,
        uri_endpoint_override: Some("http://127.0.0.1:15000".into()),
        override_endpoint: true,
        proxy: cameleer_cloud::http::Proxy::Environment,
        trust_all_certificates: false,
    };
    let mut r = cameleer_cloud::http::Request::new("POST", cameleer_cloud::http::Url::parse("http://127.0.0.1:15000/").unwrap());
    r.set_header("Content-Type", "application/x-www-form-urlencoded; charset=utf-8");
    r.body = form.as_bytes().to_vec();
    String::from_utf8_lossy(&aws.send(r, service, "AwsServiceException").unwrap().body).to_string()
}

fn list_metric_names(namespace: &str) -> Vec<String> {
    let xml = aws_query("monitoring", &format!("Action=ListMetrics&Namespace={}&Version=2010-08-01", namespace));
    let mut names: Vec<String> = xml.split("<MetricName>").skip(1).map(|p| p.split('<').next().unwrap().to_string()).collect();
    names.sort();
    names.dedup();
    names
}

/// A deployment package with `index.js` (stored, uncompressed).
fn zip_with_index_js() -> Vec<u8> {
    let name = b"index.js";
    let data = b"exports.handler = async (e) => e;\n";
    let crc = cameleer_core::dataformat::compress::crc32(data);
    let mut z = Vec::new();
    z.extend_from_slice(&0x04034b50u32.to_le_bytes());
    z.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&crc.to_le_bytes());
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&(name.len() as u16).to_le_bytes());
    z.extend_from_slice(&0u16.to_le_bytes());
    z.extend_from_slice(name);
    z.extend_from_slice(data);
    let cd_start = z.len();
    z.extend_from_slice(&0x02014b50u32.to_le_bytes());
    z.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    z.extend_from_slice(&crc.to_le_bytes());
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&(name.len() as u16).to_le_bytes());
    z.extend_from_slice(&[0; 16]);
    z.extend_from_slice(name);
    let cd_len = z.len() - cd_start;
    z.extend_from_slice(&0x06054b50u32.to_le_bytes());
    z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend_from_slice(&(cd_len as u32).to_le_bytes());
    z.extend_from_slice(&(cd_start as u32).to_le_bytes());
    z.extend_from_slice(&0u16.to_le_bytes());
    z
}

// ----------------------------------------------------------------------------- IAM-enforcing server

#[test]
fn sim_aws_iam_authentication_and_sigv4() {
    if !sim::enabled() {
        return;
    }
    let ak = sim::state_value("moto-iam.env", "access_key").expect("state/moto-iam.env (scripts/simulators.sh start)");
    let sk = sim::state_value("moto-iam.env", "secret_key").unwrap();
    let with = |ak: &str, sk: &str| format!("accessKey=RAW({})&secretKey=RAW({})&region=us-east-1&uriEndpointOverride=http://127.0.0.1:15002&overrideEndpoint=true", ak, sk);
    let good = with(&ak, &sk);
    let ctx = context();
    let t = ctx.create_producer_template();
    // Cameleer's signatures are accepted by moto's verifier, for several protocols and for
    // presigned URLs
    let bucket = sim::unique("secure");
    t.send_body(&format!("aws2-s3://{}?operation=createBucket&{}", bucket, good), "").unwrap();
    t.send_body_and_header(&format!("aws2-s3://{}?{}", bucket, good), "payload with spaces & symbols", "CamelAwsS3Key", "dir/k ey+1.txt").unwrap();
    let ex = req(&t, &format!("aws2-s3://{}?operation=getObject&{}", bucket, good), "", &[("CamelAwsS3Key", Value::from("dir/k ey+1.txt"))]);
    assert_eq!(ex.body_as::<String>().unwrap(), "payload with spaces & symbols");
    let q = sim::unique("secure-q");
    t.send_body(&format!("aws2-sqs://{}?autoCreateQueue=true&{}", q, good), "signed").unwrap();
    let table = sim::unique("secure-t");
    req(&t, &format!("aws2-ddb://{}?operation=PutItem&keyAttributeName=id&keyAttributeType=S&{}", table, good), "", &[("CamelAwsDdbItem", Value::object([("id", "1")]))]);
    let users = t.request_body(&format!("aws2-iam://iam?operation=listUsers&{}", good), "").unwrap();
    assert!(users.to_json().contains("camel"));
    // (presigned URLs are not checked here: moto's IAM mode needs an Authorization header and
    // fails with a 500 on query-string signatures)
    // a wrong secret and an unknown key are refused with the services' error codes
    let e = req_err(&t, &format!("aws2-s3://{}?operation=listObjects&{}", bucket, with(&ak, "wrong")), "", &[]);
    assert!(e.is_a("S3Exception") && e.message.contains("SignatureDoesNotMatch"), "{}", e);
    let e = req_err(&t, &format!("aws2-sqs://{}?{}", q, with(&ak, "wrong")), "x", &[]);
    assert!(e.is_a("SqsException") && e.message.contains("SignatureDoesNotMatch"), "{}", e);
    let e = req_err(&t, &format!("aws2-s3://{}?operation=listObjects&{}", bucket, with("AKIAUNKNOWNKEY000000", "x")), "", &[]);
    assert!(e.message.contains("InvalidAccessKeyId"), "{}", e);
    let e = req_err(&t, &format!("aws2-ddb://{}?operation=DescribeTable&{}", table, with("AKIAUNKNOWNKEY000000", "x")), "", &[]);
    assert!(e.message.contains("UnrecognizedClientException") || e.message.contains("InvalidClientTokenId"), "{}", e);
}

#[test]
fn sim_aws_https_untrusted_certificate_refused_unless_trust_all() {
    if !sim::enabled() {
        return;
    }
    // this binary keeps the system trust store, which does not hold the interop CA
    let ctx = context();
    let t = ctx.create_producer_template();
    let https = "accessKey=testing&secretKey=testing&region=us-east-1&uriEndpointOverride=https://localhost:15001&overrideEndpoint=true";
    let e = t.request_body(&format!("aws2-s3://x?operation=listBuckets&{}", https), "").unwrap_err();
    let text = format!("{} / {}", e, e.root_cause());
    assert!(text.to_ascii_lowercase().contains("certificate") || text.contains("TLS"), "{}", text);
    assert!(t.request_body(&format!("aws2-s3://x?operation=listBuckets&trustAllCertificates=true&{}", https), "").is_ok());
}
