//! Hermetic regressions for the AwsMock gaps found with boto3 (`tests/official_aws.rs`):
//! S3 ListParts / ListMultipartUploads, SQS MD5OfMessageAttributes / FIFO deduplication /
//! Query-compatible error codes / Query-format auth errors, SNS per-protocol messages,
//! DynamoDB ListTables / BatchWriteItem / TransactWriteItems, KMS encryption context /
//! aliases / data keys, Secrets Manager UpdateSecret, single Content-Type headers and
//! UnrecognizedClientException for unknown keys.

use cameleer_cloud::aws::sigv4::{sign_request, Credentials};
use cameleer_cloud::http::{HttpClient, Request, Response, Url};
use cameleer_cloud::testing::AwsMock;
use cameleer_core::json::parse;
use cameleer_core::value::Value;

fn call(m: &AwsMock, service: &str, method: &str, path_query: &str, headers: &[(&str, &str)], body: &[u8]) -> Response {
    call_as(m, AwsMock::ACCESS_KEY, AwsMock::SECRET_KEY, service, method, path_query, headers, body)
}

#[allow(clippy::too_many_arguments)]
fn call_as(m: &AwsMock, ak: &str, sk: &str, service: &str, method: &str, path_query: &str, headers: &[(&str, &str)], body: &[u8]) -> Response {
    let mut req = Request::new(method, Url::parse(&format!("{}{}", m.url(), path_query)).unwrap());
    for (k, v) in headers {
        req = req.header(k, *v);
    }
    req.body = body.to_vec();
    sign_request(&mut req, &Credentials::new(ak, sk), "us-east-1", service, cameleer_cloud::util::now_secs());
    HttpClient::new().send(&req).unwrap()
}

fn json(m: &AwsMock, service: &str, target: &str, body: &str) -> (u16, Value) {
    let r = call(m, service, "POST", "/", &[("Content-Type", "application/x-amz-json-1.0"), ("X-Amz-Target", target)], body.as_bytes());
    (r.status, parse(&r.text()).unwrap_or(Value::Null))
}

#[test]
fn s3_list_parts_and_multipart_uploads() {
    let m = AwsMock::start().unwrap();
    m.create_bucket("b");
    let r = call(&m, "s3", "POST", "/b/big?uploads", &[], b"");
    let id = r.text().split("<UploadId>").nth(1).unwrap().split('<').next().unwrap().to_string();
    for n in 1..=3 {
        call(&m, "s3", "PUT", &format!("/b/big?partNumber={}&uploadId={}", n, id), &[], &vec![n as u8; 10]);
    }
    let r = call(&m, "s3", "GET", &format!("/b/big?uploadId={}&max-parts=2", id), &[], b"");
    assert_eq!(r.status, 200);
    let x = r.text();
    assert!(x.contains("<ListPartsResult") && x.contains("<IsTruncated>true</IsTruncated>") && x.contains("<NextPartNumberMarker>2</NextPartNumberMarker>"), "{}", x);
    assert_eq!(x.matches("<Part>").count(), 2);
    let x = call(&m, "s3", "GET", &format!("/b/big?uploadId={}&part-number-marker=2", id), &[], b"").text();
    assert!(x.contains("<PartNumber>3</PartNumber>") && x.contains("<Size>10</Size>"));
    let x = call(&m, "s3", "GET", "/b?uploads", &[], b"").text();
    assert!(x.contains("<ListMultipartUploadsResult") && x.contains(&format!("<UploadId>{}</UploadId>", id)), "{}", x);
    assert_eq!(call(&m, "s3", "GET", "/b/big?uploadId=nope", &[], b"").status, 404);
}

#[test]
fn sqs_attribute_md5_fifo_dedup_and_error_codes() {
    let m = AwsMock::start().unwrap();
    let (_, q) = json(&m, "sqs", "AmazonSQS.CreateQueue", r#"{"QueueName":"q"}"#);
    let url = q.get("QueueUrl").unwrap().to_string();
    let (_, r) = json(
        &m,
        "sqs",
        "AmazonSQS.SendMessage",
        &format!(r#"{{"QueueUrl":"{}","MessageBody":"x","MessageAttributes":{{"kind":{{"DataType":"String","StringValue":"greeting"}},"n":{{"DataType":"Number","StringValue":"42"}},"b":{{"DataType":"Binary","BinaryValue":"AAE="}}}}}}"#, url),
    );
    // reference value computed with the algorithm of the SQS developer guide
    assert_eq!(r.get("MD5OfMessageAttributes").unwrap().to_string(), "3119e6c9106ae4a57efa21ac21970823");
    // FIFO: content-based deduplication within 5 minutes, MessageGroupId required
    let (_, q) = json(&m, "sqs", "AmazonSQS.CreateQueue", r#"{"QueueName":"f.fifo","Attributes":{"FifoQueue":"true","ContentBasedDeduplication":"true"}}"#);
    let furl = q.get("QueueUrl").unwrap().to_string();
    let send = |body: &str, group: Option<&str>, dedup: Option<&str>| {
        let mut b = format!(r#"{{"QueueUrl":"{}","MessageBody":"{}""#, furl, body);
        if let Some(g) = group {
            b.push_str(&format!(r#","MessageGroupId":"{}""#, g));
        }
        if let Some(d) = dedup {
            b.push_str(&format!(r#","MessageDeduplicationId":"{}""#, d));
        }
        b.push('}');
        json(&m, "sqs", "AmazonSQS.SendMessage", &b)
    };
    let (_, a) = send("one", Some("g"), None);
    let (_, b) = send("one", Some("g"), None);
    assert_eq!(a.get("MessageId"), b.get("MessageId"), "duplicate returns the original message id");
    assert!(a.get("SequenceNumber").is_some());
    send("two", Some("g"), Some("explicit"));
    send("three", Some("g"), Some("explicit"));
    assert_eq!(m.queue_messages("f.fifo"), vec!["one", "two"]);
    let (status, e) = send("x", None, None);
    assert_eq!(status, 400);
    assert!(e.get("__type").unwrap().to_string().ends_with("MissingParameter"));
    // JSON errors carry the Query-compatible code SDKs expose (QueueDoesNotExist -> NonExistentQueue)
    let r = call(&m, "sqs", "POST", "/", &[("Content-Type", "application/x-amz-json-1.0"), ("X-Amz-Target", "AmazonSQS.GetQueueUrl")], br#"{"QueueName":"nope"}"#);
    assert_eq!(r.header("x-amzn-query-error"), Some("AWS.SimpleQueueService.NonExistentQueue;Sender"));
    assert_eq!(r.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("Content-Type")).count(), 1, "one Content-Type header");
    // Query-protocol requests get Query (XML) auth errors, JSON ones JSON errors
    let r = call_as(&m, AwsMock::ACCESS_KEY, "wrong", "sqs", "POST", "/", &[("Content-Type", "application/x-www-form-urlencoded")], b"Action=ListQueues&Version=2012-11-05");
    assert_eq!(r.status, 403);
    assert!(r.text().contains("<Code>SignatureDoesNotMatch</Code>"), "{}", r.text());
    let r = call_as(&m, AwsMock::ACCESS_KEY, "wrong", "sqs", "POST", "/", &[("Content-Type", "application/x-amz-json-1.0"), ("X-Amz-Target", "AmazonSQS.ListQueues")], b"{}");
    assert!(r.text().contains("\"__type\""));
    // unknown access keys: UnrecognizedClientException for JSON services, InvalidAccessKeyId for S3
    let r = call_as(&m, "AKIAUNKNOWN", "x", "dynamodb", "POST", "/", &[("Content-Type", "application/x-amz-json-1.0"), ("X-Amz-Target", "DynamoDB_20120810.ListTables")], b"{}");
    assert!(r.text().contains("UnrecognizedClientException"), "{}", r.text());
    let r = call_as(&m, "AKIAUNKNOWN", "x", "s3", "GET", "/", &[], b"");
    assert!(r.text().contains("InvalidAccessKeyId"));
}

#[test]
fn sns_message_structure_json_delivers_the_sqs_entry() {
    let m = AwsMock::start().unwrap();
    let form = |body: &str| call(&m, "sns", "POST", "/", &[("Content-Type", "application/x-www-form-urlencoded")], body.as_bytes());
    let arn = form("Action=CreateTopic&Name=t").text().split("<TopicArn>").nth(1).unwrap().split('<').next().unwrap().to_string();
    m.create_queue("target");
    let enc = |s: &str| cameleer_cloud::util::uri_encode(s, true);
    form(&format!("Action=Subscribe&TopicArn={}&Protocol=sqs&Endpoint={}&Attributes.entry.1.key=RawMessageDelivery&Attributes.entry.1.value=true", enc(&arn), enc("arn:aws:sqs:us-east-1:000000000000:target")));
    form(&format!("Action=Publish&TopicArn={}&MessageStructure=json&Message={}", enc(&arn), enc(r#"{"default":"d","sqs":"for sqs"}"#)));
    form(&format!("Action=Publish&TopicArn={}&MessageStructure=json&Message={}", enc(&arn), enc(r#"{"default":"only default"}"#)));
    assert_eq!(m.queue_messages("target"), vec!["for sqs", "only default"]);
}

#[test]
fn dynamodb_list_tables_batch_write_and_transactions() {
    let m = AwsMock::start().unwrap();
    for t in ["b", "a", "c"] {
        json(&m, "dynamodb", "DynamoDB_20120810.CreateTable", &format!(r#"{{"TableName":"{}","KeySchema":[{{"AttributeName":"id","KeyType":"HASH"}}],"AttributeDefinitions":[{{"AttributeName":"id","AttributeType":"S"}}]}}"#, t));
    }
    let (_, r) = json(&m, "dynamodb", "DynamoDB_20120810.ListTables", r#"{"Limit":2}"#);
    assert_eq!(r.get("TableNames").unwrap(), &Value::Array(vec!["a".into(), "b".into()]));
    assert_eq!(r.get("LastEvaluatedTableName").unwrap().to_string(), "b");
    let (_, r) = json(&m, "dynamodb", "DynamoDB_20120810.ListTables", r#"{"ExclusiveStartTableName":"b"}"#);
    assert_eq!(r.get("TableNames").unwrap(), &Value::Array(vec!["c".into()]));
    let (status, r) = json(
        &m,
        "dynamodb",
        "DynamoDB_20120810.BatchWriteItem",
        r#"{"RequestItems":{"a":[{"PutRequest":{"Item":{"id":{"S":"1"}}}},{"PutRequest":{"Item":{"id":{"S":"2"}}}}],"b":[{"PutRequest":{"Item":{"id":{"S":"x"}}}}]}}"#,
    );
    assert_eq!(status, 200, "{:?}", r);
    assert_eq!(m.table_items("a").len(), 2);
    json(&m, "dynamodb", "DynamoDB_20120810.BatchWriteItem", r#"{"RequestItems":{"a":[{"DeleteRequest":{"Key":{"id":{"S":"1"}}}}]}}"#);
    assert_eq!(m.table_items("a").len(), 1);
    assert_eq!(json(&m, "dynamodb", "DynamoDB_20120810.BatchWriteItem", r#"{"RequestItems":{"zz":[{"PutRequest":{"Item":{"id":{"S":"1"}}}}]}}"#).0, 400);
    // a failing condition cancels the whole transaction
    let (status, r) = json(
        &m,
        "dynamodb",
        "DynamoDB_20120810.TransactWriteItems",
        r#"{"TransactItems":[{"Put":{"TableName":"c","Item":{"id":{"S":"t1"}}}},{"ConditionCheck":{"TableName":"a","Key":{"id":{"S":"2"}},"ConditionExpression":"attribute_not_exists(id)"}}]}"#,
    );
    assert_eq!(status, 400);
    assert!(r.get("__type").unwrap().to_string().ends_with("TransactionCanceledException"));
    let reasons: Vec<String> = r.get("CancellationReasons").unwrap().as_array().unwrap().iter().map(|x| x.get("Code").unwrap().to_string()).collect();
    assert_eq!(reasons, vec!["None", "ConditionalCheckFailed"]);
    assert!(m.table_items("c").is_empty(), "rolled back");
    let (status, _) = json(
        &m,
        "dynamodb",
        "DynamoDB_20120810.TransactWriteItems",
        r#"{"TransactItems":[{"Put":{"TableName":"c","Item":{"id":{"S":"t1"}}}},{"Delete":{"TableName":"a","Key":{"id":{"S":"2"}}}},{"Update":{"TableName":"b","Key":{"id":{"S":"x"}},"UpdateExpression":"SET v = :v","ExpressionAttributeValues":{":v":{"N":"1"}}}}]}"#,
    );
    assert_eq!(status, 200);
    assert_eq!(m.table_items("c").len(), 1);
    assert!(m.table_items("a").is_empty());
}

#[test]
fn kms_context_aliases_and_data_keys() {
    let m = AwsMock::start().unwrap();
    let k = |target: &str, body: &str| json(&m, "kms", &format!("TrentService.{}", target), body);
    let (_, key) = k("CreateKey", r#"{"Description":"t"}"#);
    let id = key.get("KeyMetadata").unwrap().get("KeyId").unwrap().to_string();
    assert_eq!(k("CreateAlias", &format!(r#"{{"AliasName":"alias/app","TargetKeyId":"{}"}}"#, id)).0, 200);
    assert_eq!(k("CreateAlias", &format!(r#"{{"AliasName":"alias/app","TargetKeyId":"{}"}}"#, id)).0, 400);
    let (_, aliases) = k("ListAliases", "{}");
    assert_eq!(aliases.get("Aliases").unwrap().as_array().unwrap()[0].get("TargetKeyId").unwrap().to_string(), id);
    let (_, enc) = k("Encrypt", r#"{"KeyId":"alias/app","Plaintext":"c2VjcmV0","EncryptionContext":{"a":"1","b":"2"}}"#);
    let blob = enc.get("CiphertextBlob").unwrap().to_string();
    // the context is authenticated (order-independent), a different one fails
    let (s, dec) = k("Decrypt", &format!(r#"{{"CiphertextBlob":"{}","EncryptionContext":{{"b":"2","a":"1"}}}}"#, blob));
    assert_eq!(s, 200);
    assert_eq!(dec.get("Plaintext").unwrap().to_string(), "c2VjcmV0");
    let (s, e) = k("Decrypt", &format!(r#"{{"CiphertextBlob":"{}"}}"#, blob));
    assert_eq!(s, 400);
    assert!(e.get("__type").unwrap().to_string().ends_with("InvalidCiphertextException"));
    // tampering is detected
    let mut raw = cameleer_cloud::util::base64_decode(&blob).unwrap();
    let n = raw.len();
    raw[n - 1] ^= 1;
    assert_eq!(k("Decrypt", &format!(r#"{{"CiphertextBlob":"{}","EncryptionContext":{{"a":"1","b":"2"}}}}"#, cameleer_cloud::util::base64_encode(&raw))).0, 400);
    let (_, dk) = k("GenerateDataKey", &format!(r#"{{"KeyId":"{}","KeySpec":"AES_128"}}"#, id));
    let plain = dk.get("Plaintext").unwrap().to_string();
    assert_eq!(cameleer_cloud::util::base64_decode(&plain).unwrap().len(), 16);
    let (_, dec) = k("Decrypt", &format!(r#"{{"CiphertextBlob":"{}"}}"#, dk.get("CiphertextBlob").unwrap()));
    assert_eq!(dec.get("Plaintext").unwrap().to_string(), plain);
    let (_, nopt) = k("GenerateDataKeyWithoutPlaintext", &format!(r#"{{"KeyId":"{}","NumberOfBytes":64}}"#, id));
    assert!(nopt.get("Plaintext").is_none());
    assert_eq!(k("DeleteAlias", r#"{"AliasName":"alias/app"}"#).0, 200);
    assert_eq!(k("Encrypt", r#"{"KeyId":"alias/app","Plaintext":"eA=="}"#).0, 400);
}

#[test]
fn secrets_manager_update_secret() {
    let m = AwsMock::start().unwrap();
    let sm = |target: &str, body: &str| json(&m, "secretsmanager", &format!("secretsmanager.{}", target), body);
    sm("CreateSecret", r#"{"Name":"s","SecretString":"v1","Description":"old"}"#);
    let (s, r) = sm("UpdateSecret", r#"{"SecretId":"s","SecretString":"v2","Description":"new"}"#);
    assert_eq!(s, 200);
    assert!(r.get("VersionId").is_some());
    assert_eq!(m.secret("s").as_deref(), Some("v2"));
    let (_, d) = sm("DescribeSecret", r#"{"SecretId":"s"}"#);
    assert_eq!(d.get("Description").unwrap().to_string(), "new");
    let (_, r) = sm("UpdateSecret", r#"{"SecretId":"s","Description":"only"}"#);
    assert!(r.get("VersionId").is_none(), "no new version without a value");
    assert_eq!(sm("UpdateSecret", r#"{"SecretId":"nope","SecretString":"x"}"#).0, 400);
}
