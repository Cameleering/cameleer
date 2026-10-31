//! AWS components end-to-end against the embedded SigV4-verifying emulator.

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

#[test]
fn s3_put_get_list_copy_delete() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    let t = ctx.create_producer_template();
    // createBucket, then putObject (default operation) with metadata and content type
    t.request_body_and_header(&format!("aws2-s3://docs?{}", opts), "", "CamelAwsS3Operation", "createBucket").unwrap();
    assert!(mock.bucket_exists("docs"));
    let ex = t.request(&format!("aws2-s3://docs?{}", opts), |ex| {
        ex.set_body("hello s3");
        ex.set_header("CamelAwsS3Key", "dir/a file.txt");
        ex.set_header("CamelAwsS3ContentType", "text/plain");
        ex.set_header("CamelAwsS3Metadata", Value::object([("owner", "camel")]));
    })
    .unwrap();
    assert_eq!(s(ex.header("CamelAwsS3ETag").unwrap()), format!("\"{}\"", camel_cloud::util::md5_hex(b"hello s3")));
    assert_eq!(mock.object("docs", "dir/a file.txt").unwrap(), b"hello s3");
    let (ct, meta) = mock.object_info("docs", "dir/a file.txt").unwrap();
    assert_eq!(ct, "text/plain");
    assert_eq!(meta, vec![("owner".to_string(), "camel".to_string())]);
    // keyName option
    t.send_body(&format!("aws2-s3://docs?keyName=b.txt&{}", opts), "second").unwrap();
    // getObject
    let ex = t
        .request(&format!("aws2-s3://docs?operation=getObject&{}", opts), |ex| ex.set_header("CamelAwsS3Key", "dir/a file.txt"))
        .unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "hello s3");
    assert_eq!(s(ex.header("CamelAwsS3ContentType").unwrap()), "text/plain");
    assert_eq!(ex.header("CamelAwsS3ContentLength").unwrap().as_i64(), Some(8));
    assert!(ex.header("CamelAwsS3LastModified").is_some());
    assert_eq!(ex.header("CamelAwsS3Metadata").unwrap().get("owner").unwrap(), &Value::from("camel"));
    // getObjectRange
    let ex = t
        .request(&format!("aws2-s3://docs?operation=getObjectRange&{}", opts), |ex| {
            ex.set_header("CamelAwsS3Key", "dir/a file.txt");
            ex.set_header("CamelAwsS3RangeStart", 0);
            ex.set_header("CamelAwsS3RangeEnd", 4);
        })
        .unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "hello");
    // listObjects with prefix header
    let body = t.request_body_and_header(&format!("aws2-s3://docs?operation=listObjects&{}", opts), "", "CamelAwsS3Prefix", "dir/").unwrap();
    let list = body.as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].get("key").unwrap(), &Value::from("dir/a file.txt"));
    assert_eq!(list[0].get("size").unwrap(), &Value::Int(8));
    let all = t.request_body(&format!("aws2-s3://docs?operation=listObjects&{}", opts), "").unwrap();
    assert_eq!(all.as_array().unwrap().len(), 2);
    // copyObject to another bucket
    mock.create_bucket("archive");
    t.request(&format!("aws2-s3://docs?operation=copyObject&{}", opts), |ex| {
        ex.set_header("CamelAwsS3Key", "b.txt");
        ex.set_header("CamelAwsS3BucketDestinationName", "archive");
        ex.set_header("CamelAwsS3DestinationKey", "copied/b.txt");
    })
    .unwrap();
    assert_eq!(mock.object("archive", "copied/b.txt").unwrap(), b"second");
    // listBuckets
    let buckets = t.request_body(&format!("aws2-s3://docs?operation=listBuckets&{}", opts), "").unwrap();
    let names: Vec<String> = buckets.as_array().unwrap().iter().map(|b| b.get("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["archive", "docs"]);
    // deleteObject, getObject of a missing key -> S3Exception NoSuchKey
    t.request_body_and_header(&format!("aws2-s3://docs?operation=deleteObject&{}", opts), "", "CamelAwsS3Key", "b.txt").unwrap();
    assert_eq!(mock.object_keys("docs"), vec!["dir/a file.txt"]);
    let ex = t.request(&format!("aws2-s3://docs?operation=getObject&{}", opts), |ex| ex.set_header("CamelAwsS3Key", "b.txt")).unwrap_err().root_cause().clone();
    assert!(ex.is_a("S3Exception") && ex.is_a("AwsServiceException"), "{}", ex);
    assert!(ex.message.contains("NoSuchKey"));
    // deleteBucket on a non-empty bucket fails; operation via header
    let e = t.request_body_and_header(&format!("aws2-s3://docs?{}", opts), "", "CamelAwsS3Operation", "deleteBucket").unwrap_err().root_cause().clone();
    assert!(e.message.contains("BucketNotEmpty"));
    let e = t.request_body_and_header(&format!("aws2-s3://archive?{}", opts), "", "CamelAwsS3Operation", "deleteObject").unwrap_err();
    assert!(e.root_cause().message.contains("CamelAwsS3Key"), "{}", e); // no key
    assert_eq!(mock.auth_failures(), 0);
}

#[test]
fn s3_presigned_download_link_is_accepted_by_the_server() {
    let mock = AwsMock::start().unwrap();
    mock.put_object("pub", "file.txt", b"presigned!");
    let ctx = context();
    let t = ctx.create_producer_template();
    let link = t
        .request_body_and_header(&format!("aws2-s3://pub?operation=createDownloadLink&{}", mock.endpoint_options()), "", "CamelAwsS3Key", "file.txt")
        .unwrap()
        .to_string();
    assert!(link.contains("X-Amz-Signature="), "{}", link);
    // fetch the link without any credentials
    let resp = camel_cloud::http::HttpClient::new().send(&camel_cloud::http::Request::new("GET", camel_cloud::http::Url::parse(&link).unwrap())).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"presigned!");
    // tampering with the key invalidates the signature
    let bad = link.replace("file.txt", "other.txt");
    let resp = camel_cloud::http::HttpClient::new().send(&camel_cloud::http::Request::new("GET", camel_cloud::http::Url::parse(&bad).unwrap())).unwrap();
    assert_eq!(resp.status, 403);
}

#[test]
fn s3_consumer_delete_after_read_and_move() {
    let mock = AwsMock::start().unwrap();
    mock.put_object("inbox", "orders/1.json", b"{\"id\":1}");
    mock.put_object("inbox", "orders/2.json", b"{\"id\":2}");
    mock.put_object("inbox", "other/x.txt", b"ignored");
    mock.put_object("moving", "m.txt", b"move me");
    mock.create_bucket("processed");
    let ctx = context();
    let opts = mock.endpoint_options();
    let o2 = opts.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-s3://inbox?prefix=orders/&delay=20&initialDelay=0&{}", o2)).to("mock:orders");
        b.from(&format!("aws2-s3://moving?moveAfterRead=true&destinationBucket=processed&destinationBucketPrefix=done-&delay=20&initialDelay=0&{}", o2))
            .to("mock:moved");
    })
    .unwrap();
    let orders = ctx.mock_endpoint("mock:orders").unwrap();
    orders.expected_bodies_received_in_any_order(vec!["{\"id\":1}", "{\"id\":2}"]);
    orders.all_messages(|ex| {
        ex.header("CamelAwsS3BucketName").map(|v| v.to_string()).as_deref() == Some("inbox")
            && ex.header("CamelAwsS3Key").is_some_and(|k| k.to_string().starts_with("orders/"))
            && ex.header("CamelAwsS3ETag").is_some()
            && ex.header("CamelAwsS3ContentLength").and_then(|v| v.as_i64()) == Some(8)
            && ex.header("CamelAwsS3LastModified").is_some()
            && ex.header("CamelAwsS3ContentType").is_some()
    });
    let moved = ctx.mock_endpoint("mock:moved").unwrap();
    moved.expected_bodies_received(vec!["move me"]);
    ctx.start().unwrap();
    MockEndpoint::assert_all_satisfied(&[&orders, &moved]).unwrap();
    // deleteAfterRead (default true) removed the consumed objects, the other prefix stays
    wait_until("objects deleted", || mock.object_keys("inbox") == vec!["other/x.txt"]);
    wait_until("object moved", || mock.object("processed", "done-m.txt").is_some() && mock.object_keys("moving").is_empty());
    ctx.stop().unwrap();
    assert_eq!(orders.received_counter(), 2);
}

#[test]
fn s3_consumer_keeps_objects_when_processing_fails_and_auto_creates_bucket() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-s3://fresh?autoCreateBucket=true&delay=20&initialDelay=0&{}", opts))
            .process(|_ex: &mut Exchange| Err(CamelError::runtime("boom")))
            .to("mock:never");
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(mock.bucket_exists("fresh"), "autoCreateBucket");
    mock.put_object("fresh", "k", b"v");
    let start = mock.request_count();
    wait_until("several polls", || mock.request_count() > start + 6);
    ctx.stop().unwrap();
    assert_eq!(mock.object_keys("fresh"), vec!["k"], "failed exchanges are not deleted");
}

#[test]
fn wrong_secret_is_rejected_with_403() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("b");
    let ctx = context();
    let uri = format!("aws2-s3://b?accessKey={}&secretKey=wrong&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true", AwsMock::ACCESS_KEY, mock.url());
    let e = ctx.create_producer_template().request_body_and_header(&uri, "x", "CamelAwsS3Key", "k").unwrap_err().root_cause().clone();
    assert!(e.is_a("S3Exception"), "{}", e);
    assert!(e.message.contains("Status Code: 403") && e.message.contains("SignatureDoesNotMatch"), "{}", e);
    assert_eq!(mock.auth_failures(), 1);
    assert!(mock.object("b", "k").is_none());
    // unknown access key
    let uri = format!("aws2-sqs://q?accessKey=NOPE&secretKey=x&uriEndpointOverride={}&overrideEndpoint=true", mock.url());
    let e = ctx.create_producer_template().request_body(&uri, "x").unwrap_err().root_cause().clone();
    assert!(e.is_a("SqsException") && e.message.contains("InvalidAccessKeyId"), "{}", e);
}

#[test]
fn sqs_send_receive_delete_batch_list_purge() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    let t = ctx.create_producer_template();
    // autoCreateQueue creates the queue on first use
    let ex = t
        .request(&format!("aws2-sqs://orders?autoCreateQueue=true&{}", opts), |ex| {
            ex.set_body("order-1");
            ex.set_header("customer", "ACME");
            ex.set_header("quantity", 3);
            ex.set_header("CamelInternal", "dropped");
        })
        .unwrap();
    assert!(mock.queue_exists("orders"));
    assert!(!s(ex.header("CamelAwsSqsMessageId").unwrap()).is_empty());
    assert_eq!(s(ex.header("CamelAwsSqsMD5OfBody").unwrap()), camel_cloud::util::md5_hex(b"order-1"));
    let attrs = &mock.queue_message_attributes("orders")[0];
    assert_eq!(attrs.get("customer").map(|s| s.as_str()), Some("ACME"));
    assert_eq!(attrs.get("quantity").map(|s| s.as_str()), Some("3"));
    assert!(!attrs.contains_key("CamelInternal"));
    // sendBatchMessage
    let r = t.request_body(&format!("aws2-sqs://orders?operation=sendBatchMessage&{}", opts), Value::from(vec!["b1", "b2"])).unwrap();
    assert_eq!(r.get("successful").unwrap().as_array().unwrap().len(), 2);
    assert_eq!(mock.queue_messages("orders"), vec!["order-1", "b1", "b2"]);
    // listQueues
    mock.create_queue("other");
    let queues = t.request_body(&format!("aws2-sqs://orders?operation=listQueues&{}", opts), "").unwrap();
    assert_eq!(queues.as_array().unwrap().len(), 2);
    let filtered = t.request_body_and_header(&format!("aws2-sqs://orders?operation=listQueues&{}", opts), "", "CamelAwsSqsPrefix", "oth").unwrap();
    assert_eq!(filtered.as_array().unwrap().len(), 1);
    // consume one message with a polling route that does not delete, then delete it via the producer
    ctx.add_routes({
        let opts = opts.clone();
        move |b| {
            b.from(&format!("aws2-sqs://orders?deleteAfterRead=false&maxMessagesPerPoll=1&visibilityTimeout=30&delay=10&initialDelay=0&{}", opts))
                .route_id("peek")
                .to("mock:peek");
        }
    })
    .unwrap();
    let peek = ctx.mock_endpoint("mock:peek").unwrap();
    peek.expected_minimum_message_count(1);
    ctx.start().unwrap();
    peek.assert_is_satisfied().unwrap();
    ctx.stop_route("peek").unwrap();
    let first = peek.exchange(0).unwrap();
    assert_eq!(first.body_as::<String>().unwrap(), "order-1");
    assert_eq!(s(first.header("customer").unwrap()), "ACME");
    assert_eq!(first.header("quantity").unwrap(), &Value::Int(3));
    assert!(first.header("CamelAwsSqsAttributes").unwrap().get("SentTimestamp").is_some());
    assert_eq!(first.header("CamelAwsSqsMessageAttributes").unwrap().get("customer").unwrap(), &Value::from("ACME"));
    let handle = s(first.header("CamelAwsSqsReceiptHandle").unwrap());
    t.request_body_and_header(&format!("aws2-sqs://orders?operation=deleteMessage&{}", opts), "", "CamelAwsSqsReceiptHandle", handle.as_str()).unwrap();
    assert_eq!(mock.queue_messages("orders"), vec!["b1", "b2"]);
    // invalid receipt handle -> SqsException
    let e = t.request_body_and_header(&format!("aws2-sqs://orders?operation=deleteMessage&{}", opts), "", "CamelAwsSqsReceiptHandle", "bogus").unwrap_err().root_cause().clone();
    assert!(e.is_a("SqsException") && e.message.contains("ReceiptHandleIsInvalid"), "{}", e);
    // purgeQueue
    t.request_body(&format!("aws2-sqs://orders?operation=purgeQueue&{}", opts), "").unwrap();
    assert!(mock.queue_messages("orders").is_empty());
    // a missing queue without autoCreateQueue
    let e = t.request_body(&format!("aws2-sqs://missing?{}", opts), "x").unwrap_err().root_cause().clone();
    assert!(e.message.contains("QueueDoesNotExist"), "{}", e);
    ctx.stop().unwrap();
}

#[test]
fn sqs_consumer_deletes_processed_messages_and_redelivers_failed() {
    let mock = AwsMock::start().unwrap();
    mock.create_queue("work");
    for i in 0..5 {
        mock.send_sqs("work", &format!("job-{}", i));
    }
    mock.send_sqs("work", "poison");
    let ctx = context();
    let opts = mock.endpoint_options();
    let poison_attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let attempts = poison_attempts.clone();
    ctx.add_routes(move |b| {
        b.from(&format!("aws2-sqs://work?maxMessagesPerPoll=10&waitTimeSeconds=1&visibilityTimeout=1&delay=10&initialDelay=0&{}", opts))
            .process(move |ex: &mut Exchange| {
                if ex.body_as::<String>()? == "poison" {
                    attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    return Err(CamelError::runtime("poison message"));
                }
                Ok(())
            })
            .to("mock:jobs");
    })
    .unwrap();
    let jobs = ctx.mock_endpoint("mock:jobs").unwrap();
    jobs.expected_bodies_received_in_any_order(vec!["job-0", "job-1", "job-2", "job-3", "job-4"]);
    jobs.all_messages(|ex| ex.header("CamelAwsSqsMessageId").is_some() && ex.header("CamelAwsSqsReceiptHandle").is_some());
    ctx.start().unwrap();
    jobs.assert_is_satisfied().unwrap();
    wait_until("processed messages deleted", || mock.queue_messages("work") == vec!["poison"]);
    // the poison message becomes visible again after the visibility timeout and is retried
    wait_until("poison message redelivered", || poison_attempts.load(std::sync::atomic::Ordering::SeqCst) >= 2);
    ctx.stop().unwrap();
    assert_eq!(mock.queue_messages("work"), vec!["poison"]);
}

#[test]
fn sns_publish_with_subject_attributes_and_auto_create() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let opts = mock.endpoint_options();
    let t = ctx.create_producer_template();
    let ex = t
        .request(&format!("aws2-sns://alerts?autoCreateTopic=true&subject=Alert&{}", opts), |ex| {
            ex.set_body("disk full");
            ex.set_header("severity", "high");
        })
        .unwrap();
    assert!(!s(ex.header("CamelAwsSnsMessageId").unwrap()).is_empty());
    let published = mock.published("alerts");
    assert_eq!(published.len(), 1);
    assert_eq!(published[0].message, "disk full");
    assert_eq!(published[0].subject.as_deref(), Some("Alert"));
    assert_eq!(published[0].attributes.get("severity").map(|s| s.as_str()), Some("high"));
    assert_eq!(published[0].message_id, s(ex.header("CamelAwsSnsMessageId").unwrap()));
    // existing topic looked up by name (ListTopics), subject and structure from headers
    mock.create_topic("events");
    t.send_body_and_headers(
        &format!("aws2-sns://events?{}", opts),
        "{\"default\":\"d\",\"sqs\":\"s\"}",
        vec![("CamelAwsSnsSubject", Value::from("S")), ("CamelAwsSnsMessageStructure", Value::from("json"))],
    )
    .unwrap();
    let p = mock.published("events");
    assert_eq!((p[0].subject.as_deref(), p[0].message_structure.as_deref()), (Some("S"), Some("json")));
    // by ARN
    let arn = mock.create_topic("byarn");
    t.send_body(&format!("aws2-sns://{}?{}", arn, opts), "x").unwrap();
    assert_eq!(mock.published("byarn").len(), 1);
    // missing topic without autoCreateTopic
    let e = t.request_body(&format!("aws2-sns://nope?{}", opts), "x").unwrap_err().root_cause().clone();
    assert!(e.is_a("SnsException"), "{}", e);
    // FIFO topic gets a group id
    t.send_body(&format!("aws2-sns://orders.fifo?autoCreateTopic=true&messageGroupIdStrategy=useConstant&{}", opts), "f").unwrap();
    assert_eq!(mock.published("orders.fifo")[0].group_id.as_deref(), Some("CamelSingleMessageGroup"));
}

#[test]
fn ddb_put_get_update_query_scan_delete() {
    let mock = AwsMock::start().unwrap();
    mock.create_table("orders", "customer", Some("orderId"));
    let ctx = context();
    let base = format!("aws2-ddb://orders?{}", mock.endpoint_options());
    let t = ctx.create_producer_template();
    for (c, o, total) in [("acme", "o1", 10), ("acme", "o2", 25), ("acme", "o3", 40), ("globex", "o1", 5)] {
        t.send_body_and_header(
            &format!("{}&operation=PutItem", base),
            "",
            "CamelAwsDdbItem",
            Value::object([("customer", Value::from(c)), ("orderId", Value::from(o)), ("total", Value::Int(total)), ("paid", Value::Bool(total > 20))]),
        )
        .unwrap();
    }
    assert_eq!(mock.table_items("orders").len(), 4);
    // GetItem
    let ex = t
        .request(&format!("{}&operation=GetItem", base), |ex| {
            ex.set_header("CamelAwsDdbKey", Value::object([("customer", "acme"), ("orderId", "o2")]));
        })
        .unwrap();
    let item = ex.header("CamelAwsDdbAttributes").unwrap();
    assert_eq!(item.get("total").unwrap(), &Value::Int(25));
    assert_eq!(item.get("paid").unwrap(), &Value::Bool(true));
    // UpdateItem with ALL_NEW
    let ex = t
        .request(&base, |ex| {
            ex.set_header("CamelAwsDdbOperation", "UpdateItem");
            ex.set_header("CamelAwsDdbKey", Value::object([("customer", "acme"), ("orderId", "o1")]));
            ex.set_header("CamelAwsDdbUpdateValues", Value::object([("total", Value::Int(11)), ("paid", Value::Null), ("note", Value::from("rush"))]));
            ex.set_header("CamelAwsDdbReturnValues", "ALL_NEW");
        })
        .unwrap();
    let updated = ex.header("CamelAwsDdbAttributes").unwrap();
    assert_eq!(updated.get("total").unwrap(), &Value::Int(11));
    assert_eq!(updated.get("note").unwrap(), &Value::from("rush"));
    assert!(updated.get("paid").is_none());
    // Query: hash EQ + range condition, descending
    let ex = t
        .request(&format!("{}&operation=Query", base), |ex| {
            ex.set_header(
                "CamelAwsDdbKeyConditions",
                Value::object([
                    ("customer", Value::from("acme")),
                    ("orderId", Value::object([("ComparisonOperator", Value::from("GE")), ("AttributeValueList", Value::Array(vec![Value::from("o2")]))])),
                ]),
            );
            ex.set_header("CamelAwsDdbScanIndexForward", false);
        })
        .unwrap();
    let items = ex.header("CamelAwsDdbItems").unwrap().as_array().unwrap().clone();
    assert_eq!(items.iter().map(|i| i.get("orderId").unwrap().to_string()).collect::<Vec<_>>(), vec!["o3", "o2"]);
    assert_eq!(ex.header("CamelAwsDdbCount").unwrap(), &Value::Int(2));
    // Scan with a filter and limit
    let ex = t
        .request(&format!("{}&operation=Scan", base), |ex| {
            ex.set_header(
                "CamelAwsDdbScanFilter",
                Value::object([("total", Value::object([("ComparisonOperator", Value::from("GT")), ("AttributeValueList", Value::Array(vec![Value::Int(10)]))]))]),
            );
        })
        .unwrap();
    assert_eq!(ex.header("CamelAwsDdbCount").unwrap(), &Value::Int(3));
    assert_eq!(ex.header("CamelAwsDdbScannedCount").unwrap(), &Value::Int(4));
    // DeleteItem returning the old item
    let ex = t
        .request(&format!("{}&operation=DeleteItem", base), |ex| {
            ex.set_header("CamelAwsDdbKey", Value::object([("customer", "globex"), ("orderId", "o1")]));
            ex.set_header("CamelAwsDdbReturnValues", "ALL_OLD");
        })
        .unwrap();
    assert_eq!(ex.header("CamelAwsDdbAttributes").unwrap().get("total").unwrap(), &Value::Int(5));
    assert_eq!(mock.table_items("orders").len(), 3);
    // DescribeTable
    let ex = t.request(&format!("{}&operation=DescribeTable", base), |_| {}).unwrap();
    assert_eq!(ex.header("CamelAwsDdbItemCount").unwrap(), &Value::Int(3));
    assert_eq!(s(ex.header("CamelAwsDdbTableStatus").unwrap()), "ACTIVE");
}

#[test]
fn ddb_creates_missing_table_on_start() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let uri = format!("aws2-ddb://users?keyAttributeName=id&keyAttributeType=S&{}", mock.endpoint_options());
    ctx.create_producer_template().send_body_and_header(&uri, "", "CamelAwsDdbItem", Value::object([("id", Value::from("u1")), ("name", Value::from("Ann"))])).unwrap();
    assert!(mock.table_exists("users"));
    assert_eq!(mock.table_items("users")[0].get("name").unwrap(), &Value::from("Ann"));
    // missing table without keyAttributeName fails on start
    let uri = format!("aws2-ddb://ghost?{}", mock.endpoint_options());
    let e = ctx.create_producer_template().request_body(&uri, "").unwrap_err().root_cause().clone();
    assert!(e.is_a("DynamoDbException") && e.message.contains("ResourceNotFoundException"), "{}", e);
}

#[test]
fn lambda_invoke_list_get() {
    let mock = AwsMock::start().unwrap();
    mock.register_function("greeter", |payload| {
        let v = camel_core::json::parse(&String::from_utf8_lossy(payload)).map_err(|e| e.to_string())?;
        Ok(format!("{{\"greeting\":\"Hello {}\"}}", v.get("name").map(|n| n.to_string()).unwrap_or_default()).into_bytes())
    });
    mock.register_function("failing", |_| Err("kaboom".into()));
    let ctx = context();
    let opts = mock.endpoint_options();
    let t = ctx.create_producer_template();
    let out = t.request_body(&format!("aws2-lambda://greeter?operation=invokeFunction&{}", opts), "{\"name\":\"Camel\"}").unwrap();
    assert_eq!(out.to_string(), "{\"greeting\":\"Hello Camel\"}");
    // by ARN, in a route
    let arn = format!("arn:aws:lambda:us-east-1:{}:function:greeter", camel_cloud::testing::aws::ACCOUNT);
    ctx.add_routes({
        let opts = opts.clone();
        move |b| {
            b.from("direct:greet").to(&format!("aws2-lambda://{}?{}", arn, opts)).to("mock:greeted");
        }
    })
    .unwrap();
    ctx.start().unwrap();
    let greeted = ctx.mock_endpoint("mock:greeted").unwrap();
    greeted.expected_bodies_received(vec!["{\"greeting\":\"Hello Route\"}"]);
    t.send_body("direct:greet", "{\"name\":\"Route\"}").unwrap();
    greeted.assert_is_satisfied().unwrap();
    // function error
    let ex = t.request(&format!("aws2-lambda://failing?{}", opts), |ex| ex.set_body("{}")).unwrap();
    assert_eq!(s(ex.header("CamelAwsLambdaFunctionError").unwrap()), "Unhandled");
    assert!(ex.body_as::<String>().unwrap().contains("kaboom"));
    // listFunctions / getFunction
    let fs = t.request_body(&format!("aws2-lambda://greeter?operation=listFunctions&{}", opts), "").unwrap();
    assert_eq!(fs.as_array().unwrap().len(), 2);
    let f = t.request_body(&format!("aws2-lambda://greeter?operation=getFunction&{}", opts), "").unwrap();
    assert_eq!(f.get("Configuration").unwrap().get("FunctionName").unwrap(), &Value::from("greeter"));
    // unknown function
    let e = t.request_body(&format!("aws2-lambda://nope?{}", opts), "{}").unwrap_err().root_cause().clone();
    assert!(e.is_a("LambdaException") && e.message.contains("ResourceNotFoundException"), "{}", e);
    ctx.stop().unwrap();
}

#[test]
fn component_level_properties_and_unknown_options() {
    let mock = AwsMock::start().unwrap();
    mock.create_bucket("cfg");
    let ctx = context();
    for (k, v) in [
        ("accessKey", AwsMock::ACCESS_KEY.to_string()),
        ("secretKey", AwsMock::SECRET_KEY.to_string()),
        ("uriEndpointOverride", mock.url()),
        ("overrideEndpoint", "true".to_string()),
    ] {
        ctx.properties().set(format!("camel.component.aws2-s3.{}", k), v);
    }
    ctx.create_producer_template().send_body_and_header("aws2-s3://cfg", "via properties", "CamelAwsS3Key", "p.txt").unwrap();
    assert_eq!(mock.object("cfg", "p.txt").unwrap(), b"via properties");
    let e = ctx.create_producer_template().request_body("aws2-s3://cfg?noSuchOption=1", "x").unwrap_err().root_cause().clone();
    assert!(e.message.contains("noSuchOption"), "{}", e);
}
