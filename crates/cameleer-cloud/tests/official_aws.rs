//! Official-client cross-verification of the AWS emulator (AwsMock) and the AWS components:
//! boto3 / botocore sign every request with SigV4 and parse every response; botocore's signer
//! is the reference for Cameleer's SigV4 (header and presigned); data written by boto3 is
//! read by the components and the other way round.
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use cameleer_cloud::aws::sigv4::{presign_url, sign_request, Credentials};
use cameleer_cloud::http::{Request, Url};
use cameleer_cloud::testing::AwsMock;
use cameleer_core::prelude::*;
use cameleer_tls::testing::official::{self, python_script, Run};

const SERVICES: &[&str] = &["s3", "sqs", "sqs-json", "sns", "dynamodb", "kinesis", "lambda", "kms", "secretsmanager", "sts", "badsig"];

fn boto3(endpoint: &str, services: &[&str], extra_env: &[(&str, &str)]) -> Run {
    let mut args = vec![endpoint];
    args.extend_from_slice(services);
    let mut env = vec![("AWS_ACCESS_KEY_ID", AwsMock::ACCESS_KEY), ("AWS_SECRET_ACCESS_KEY", AwsMock::SECRET_KEY), ("AWS_CONFIG_FILE", "/dev/null"), ("AWS_SHARED_CREDENTIALS_FILE", "/dev/null")];
    env.extend_from_slice(extra_env);
    python_script("aws_boto3.py", &args, &env)
}

fn mock() -> AwsMock {
    let m = AwsMock::start().unwrap();
    m.register_function("echo", |b| Ok(b.to_vec()));
    m.register_function("fail", |_| Err("boom".into()));
    m
}

#[test]
fn boto3_against_aws_mock() {
    if !official::enabled() {
        return;
    }
    let m = mock();
    let r = boto3(&m.url(), SERVICES, &[]).ok();
    for s in SERVICES {
        assert!(r.values("ok:").iter().any(|x| x == s), "{} failed:\n{}\n{}", s, r.stdout, r.stderr);
    }
    let expect = [
        ("s3-get:", "hello world text/plain 11 [('mixed-case', 'v'), ('owner', 'boto3')]"),
        ("s3-put-etag:", "True"),
        ("s3-range:", "world"),
        ("s3-special-key:", "x"),
        ("s3-list:", "5 keys 3 pages"),
        ("s3-delimiter:", "dir/,list/,spécial dir/"),
        ("s3-missing:", "NoSuchKey 404"),
        ("s3-list-parts:", "3"),
        ("s3-multipart-etag:", "True"),
        ("s3-multipart:", "10486760 True"),
        ("s3-managed-transfer:", "True"),
        ("s3-presigned-get:", "hello world"),
        ("s3-presigned-put:", "via presigned put"),
        ("s3-presigned-tampered:", "403"),
        ("sqs-md5:", "True True"),
        ("sqs-receive:", "['batch-a', 'batch-b', 'hello sqs']"),
        ("sqs-attributes:", "greeting 42 0001 1"),
        ("sqs-fifo:", "f0,f1,f2"),
        ("sqs-missing:", "AWS.SimpleQueueService.NonExistentQueue"),
        ("sqsjson-protocol:", "json"),
        ("sqsjson-receive:", "['b1', 'b2', 'over json'] ['v']"),
        ("sqsjson-missing:", "AWS.SimpleQueueService.NonExistentQueue"),
        ("sns-delivered:", "['hello sns', 's']"),
        ("ddb-get:", "n3 ['a', 'b'] 4.5 [Decimal('1'), 'two', None, True] 00ff"),
        ("ddb-query:", "[4, 3, 2] 3 4"),
        ("ddb-update:", "renamed 1 False"),
        ("ddb-condition:", "ConditionalCheckFailedException"),
        ("ddb-scan:", "[0, 2, 5] ['name', 'sk']"),
        ("ddb-batch-get:", "2"),
        ("ddb-transact:", "True"),
        ("ddb-transact-cancel:", "TransactionCanceledException ['None', 'ConditionalCheckFailed'] False"),
        ("kinesis-get:", "['record-1', 'record-2', 'record-3', 'record-4'] True"),
        ("lambda-invoke:", "200 {'hello': 'lambda'}"),
        ("lambda-function-error:", "Unhandled True"),
        ("kms-roundtrip:", "secret data True"),
        ("kms-context:", "InvalidCiphertextException"),
        ("kms-data-key:", "32 True"),
        ("kms-disabled:", "DisabledException"),
        ("sm-version:", "v2 True"),
        ("sm-binary:", "000102"),
        ("sts-assume:", "ASIA True True"),
        ("sts-temp-creds:", "True"),
        ("sts-missing-token:", "InvalidClientTokenId"),
        ("badsig-sqs:", "SignatureDoesNotMatch"),
        ("badsig-s3:", "SignatureDoesNotMatch"),
        ("badsig-unknown-key:", "UnrecognizedClientException"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}", k, r.stdout);
    }
    assert!(m.auth_failures() >= 4, "bad signatures were refused by the emulator");
}

#[test]
fn boto3_over_https() {
    if !official::enabled() {
        return;
    }
    let m = AwsMock::start_tls().unwrap();
    let dir = official::temp_dir("aws-tls");
    let ca = dir.join("aws.pem");
    std::fs::write(&ca, m.cert_pem().unwrap()).unwrap();
    let url = format!("https://localhost:{}", m.port());
    let r = boto3(&url, &["s3", "sqs", "dynamodb"], &[("AWS_CA_BUNDLE", ca.to_str().unwrap())]).ok();
    assert_eq!(r.values("ok:"), vec!["s3", "sqs", "dynamodb"]);
    // the presigned GET through urllib also verified the certificate
    assert_eq!(r.value("s3-presigned-get:"), "hello world");
    let _ = std::fs::remove_dir_all(dir);
}

/// Cameleer's SigV4 signer produces exactly botocore's signatures for the same requests
/// (S3 and non-S3 canonical URIs, query canonicalization, session tokens, presigned URLs).
#[test]
fn sigv4_signatures_match_botocore() {
    if !official::enabled() {
        return;
    }
    let creds = Credentials::new("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY");
    let mut session = Credentials::new("ASIAEXAMPLE", "session/secret");
    session.session_token = Some("FQoGZXIvYXdzE+token/with=chars".into());
    let time = 1_700_000_000i64;
    // (method, url, headers, body, service, region, credentials)
    let cases: Vec<(&str, &str, Vec<(&str, &str)>, &[u8], &str, &str, &Credentials)> = vec![
        ("GET", "https://s3.us-east-1.amazonaws.com/bucket/dir/a%20b%2Bc/%C3%BC.txt?list-type=2&prefix=a%20b", vec![], b"", "s3", "us-east-1", &creds),
        ("PUT", "https://examplebucket.s3.amazonaws.com/test%24file.text", vec![("Content-Type", "text/plain"), ("x-amz-meta-owner", "  camel   rider ")], b"Welcome to Amazon S3.", "s3", "us-east-1", &creds),
        ("POST", "https://sqs.eu-west-1.amazonaws.com/", vec![("Content-Type", "application/x-amz-json-1.0"), ("X-Amz-Target", "AmazonSQS.SendMessage")], br#"{"QueueUrl":"q","MessageBody":"hi"}"#, "sqs", "eu-west-1", &creds),
        ("POST", "https://dynamodb.us-west-2.amazonaws.com/", vec![("Content-Type", "application/x-amz-json-1.0"), ("X-Amz-Target", "DynamoDB_20120810.GetItem")], br#"{"TableName":"t"}"#, "dynamodb", "us-west-2", &session),
        ("GET", "https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/my%20fn/configuration?Qualifier=%24LATEST", vec![], b"", "lambda", "us-east-1", &creds),
        ("POST", "https://sns.us-east-1.amazonaws.com/", vec![("Content-Type", "application/x-www-form-urlencoded; charset=utf-8")], b"Action=Publish&Message=hello%20world&TopicArn=arn%3Aaws%3Asns", "sns", "us-east-1", &session),
    ];
    let b64 = |b: &[u8]| cameleer_cloud::util::base64_encode(b);
    let mut vectors = Vec::new();
    let mut ours = Vec::new();
    for (method, url, headers, body, service, region, c) in &cases {
        let mut req = Request::new(method, Url::parse(url).unwrap());
        for (k, v) in headers {
            req = req.header(k, *v);
        }
        req.body = body.to_vec();
        sign_request(&mut req, c, region, service, time);
        ours.push(req.get_header("Authorization").unwrap().to_string());
        let hs: Vec<Value> = headers.iter().map(|(k, v)| Value::Array(vec![Value::from(*k), Value::from(*v)])).collect();
        let mut v = Value::object([
            ("method", Value::from(*method)),
            ("url", Value::from(*url)),
            ("headers", Value::Array(hs)),
            ("body_b64", Value::from(b64(body))),
            ("service", Value::from(*service)),
            ("region", Value::from(*region)),
            ("time", Value::Int(time)),
            ("access_key", Value::from(c.access_key.as_str())),
            ("secret_key", Value::from(c.secret_key.as_str())),
        ]);
        if let (Some(t), Some(o)) = (&c.session_token, v.as_object_mut()) {
            o.insert("token".into(), Value::from(t.as_str()));
        }
        vectors.push(v);
    }
    // presigned S3 GET / PUT
    let presign_cases = [("GET", "https://s3.amazonaws.com/bucket/key%20with%20space.txt", &creds), ("PUT", "https://bucket.s3.eu-central-1.amazonaws.com/upload.bin", &session)];
    let mut presigned_ours = Vec::new();
    for (method, url, c) in presign_cases {
        let region = if url.contains("eu-central-1") { "eu-central-1" } else { "us-east-1" };
        presigned_ours.push(presign_url(method, &Url::parse(url).unwrap(), c, region, "s3", time, 900));
        let mut v = Value::object([
            ("method", Value::from(method)),
            ("url", Value::from(url)),
            ("service", Value::from("s3")),
            ("region", Value::from(region)),
            ("time", Value::Int(time)),
            ("access_key", Value::from(c.access_key.as_str())),
            ("secret_key", Value::from(c.secret_key.as_str())),
            ("presign", Value::Int(900)),
        ]);
        if let (Some(t), Some(o)) = (&c.session_token, v.as_object_mut()) {
            o.insert("token".into(), Value::from(t.as_str()));
        }
        vectors.push(v);
    }
    let mut cmd = std::process::Command::new(official::python());
    cmd.arg("-I").arg("-W").arg("ignore").arg(official::script("aws_sigv4.py"));
    let input = Value::Array(vectors).to_json();
    let r = official::run(&mut cmd, Some(input.as_bytes()), std::time::Duration::from_secs(60)).ok();
    let lines: Vec<Value> = r.stdout.lines().skip(1).map(|l| cameleer_core::json::parse(l).unwrap()).collect();
    assert_eq!(lines.len(), cases.len() + presign_cases.len(), "{}", r.stdout);
    for (i, ours) in ours.iter().enumerate() {
        let theirs = lines[i].get("authorization").unwrap().to_string();
        assert_eq!(ours, &theirs, "case {} ({} {})", i, cases[i].0, cases[i].1);
    }
    let params = |q: &str| {
        let mut v: Vec<String> = q.split('&').map(String::from).collect();
        v.sort();
        v
    };
    for (i, ours) in presigned_ours.iter().enumerate() {
        let theirs = Url::parse(&lines[cases.len() + i].get("url").unwrap().to_string()).unwrap();
        assert_eq!(ours.path, theirs.path);
        assert_eq!(params(ours.query.as_deref().unwrap()), params(theirs.query.as_deref().unwrap()), "presigned case {}", i);
    }
}

/// Data written with boto3 is read by the aws2-s3 / aws2-sqs / aws2-ddb / aws-secrets-manager
/// components, and data the components write is read back with boto3.
#[test]
fn boto3_and_components_exchange_data() {
    if !official::enabled() {
        return;
    }
    let m = mock();
    boto3(&m.url(), &["interop-seed"], &[]).ok();
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let opts = m.endpoint_options();
    // S3
    let ex = t.request(&format!("aws2-s3://interop?operation=getObject&{}", opts), |ex| ex.set_header("CamelAwsS3Key", "from boto3/ä+b.txt")).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap(), "written by boto3");
    assert_eq!(ex.header("CamelAwsS3ContentType").unwrap().to_string(), "text/plain; charset=utf-8");
    assert_eq!(ex.header("CamelAwsS3Metadata").unwrap().get("origin"), Some(&Value::from("boto3")));
    t.request(&format!("aws2-s3://interop?{}", opts), |ex| {
        ex.set_body("written by camel");
        ex.set_header("CamelAwsS3Key", "from camel/ü b.txt");
        ex.set_header("CamelAwsS3ContentType", "text/csv");
        ex.set_header("CamelAwsS3Metadata", Value::object([("origin", "camel")]));
    })
    .unwrap();
    // SQS
    ctx.add_routes({
        let opts = opts.clone();
        move |b| {
            b.from(&format!("aws2-sqs://interop-in?maxMessagesPerPoll=10&delay=10&initialDelay=0&messageAttributeNames=All&{}", opts)).to("mock:sqs");
        }
    })
    .unwrap();
    let mock_ep = ctx.mock_endpoint("mock:sqs").unwrap();
    mock_ep.expected_message_count(1);
    mock_ep.assert_is_satisfied_within(std::time::Duration::from_secs(20)).unwrap();
    let got = mock_ep.exchange(0).unwrap();
    assert_eq!(got.body_as::<String>().unwrap(), "sqs from boto3");
    assert_eq!(got.header("origin").unwrap().to_string(), "boto3");
    assert_eq!(got.header("count").unwrap().as_i64(), Some(7));
    t.request(&format!("aws2-sqs://interop-out?autoCreateQueue=true&{}", opts), |ex| {
        ex.set_body("sqs from camel");
        ex.set_header("origin", "camel");
        ex.set_header("priority", 5);
    })
    .unwrap();
    // DynamoDB
    let ex = t.request(&format!("aws2-ddb://interop?operation=GetItem&{}", opts), |ex| ex.set_header("CamelAwsDdbKey", Value::object([("id", "boto3")]))).unwrap();
    let item = ex.header("CamelAwsDdbAttributes").unwrap().clone();
    assert_eq!(item.get("n"), Some(&Value::Int(42)));
    assert_eq!(item.get("f").and_then(|v| v.as_f64()), Some(1.25));
    assert_eq!(item.get("b"), Some(&Value::Bool(true)));
    assert_eq!(item.get("m").unwrap().get("k"), Some(&Value::from("v")));
    t.send_body_and_header(
        &format!("aws2-ddb://interop?operation=PutItem&{}", opts),
        "",
        "CamelAwsDdbItem",
        Value::object([("id", Value::from("camel")), ("name", Value::from("Cameleer")), ("n", Value::Int(7)), ("flag", Value::Bool(false)), ("nested", Value::object([("k", "nv")]))]),
    )
    .unwrap();
    // Secrets Manager
    let v = t.request_body_and_header(&format!("aws-secrets-manager://s?{}", opts), "", "CamelAwsSecretsManagerSecretId", "interop/secret").unwrap();
    assert_eq!(v.to_string(), "from-boto3");
    t.request(&format!("aws-secrets-manager://s?operation=createSecret&{}", opts), |ex| {
        ex.set_body("from-camel");
        ex.set_header("CamelAwsSecretsManagerSecretName", "interop/camel");
    })
    .unwrap();
    ctx.stop().unwrap();
    let r = boto3(&m.url(), &["interop-check"], &[]).ok();
    assert_eq!(r.value("check-s3:"), "written by camel text/csv camel");
    assert_eq!(r.value("check-sqs:"), "sqs from camel [('origin', ('String', 'camel')), ('priority', ('Number', '5'))] True");
    assert_eq!(r.value("check-ddb:"), "Cameleer 7 False nv");
    assert_eq!(r.value("check-secret:"), "from-camel");
}
