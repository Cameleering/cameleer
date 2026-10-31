//! The newer AWS components end-to-end against the SigV4-verifying emulator: MSK (+ the
//! `AWS_MSK_IAM` SASL mechanism of the kafka component), Athena, Textract, Translate,
//! Comprehend, Polly, Rekognition, Transcribe, Bedrock (runtime + agent runtime), MQ, ECS, EKS,
//! KMS, Timestream, CloudTrail, Config and Redshift Data, with negative authentication tests.

use std::time::Duration;

use camel_cloud::testing::AwsMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

fn s(v: &Value) -> String {
    v.to_string()
}


/// Options with a wrong secret for the mock's access key.
fn bad_options(mock: &AwsMock) -> String {
    format!("accessKey=RAW({})&secretKey=RAW(wrong)&region=us-east-1&uriEndpointOverride={}&overrideEndpoint=true", AwsMock::ACCESS_KEY, mock.url())
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
fn msk_cluster_management_and_auth() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("aws2-msk://msk?{}", mock.endpoint_options());
    let ex = req(
        &t,
        &uri,
        "",
        &[
            ("CamelAwsMSKOperation", "createCluster".into()),
            ("CamelAwsMSKClusterName", "orders".into()),
            ("CamelAwsMSKClusterKafkaVersion", "3.6.0".into()),
            ("CamelAwsMSKBrokerNodesNumber", Value::Int(3)),
            ("CamelAwsMSKBrokerNodesGroupInfo", Value::object([("clientSubnets", Value::from(vec!["subnet-1"])), ("instanceType", Value::from("kafka.m5.large"))])),
        ],
    );
    let arn = s(ex.header("CamelAwsMSKClusterArn").unwrap());
    assert!(arn.starts_with("arn:aws:kafka:us-east-1:"), "{}", arn);
    assert_eq!(s(ex.header("CamelAwsMSKClusterState").unwrap()), "CREATING");
    for name in ["other-1", "other-2"] {
        req(&t, &uri, "", &[("CamelAwsMSKOperation", "createCluster".into()), ("CamelAwsMSKClusterName", name.into()), ("CamelAwsMSKClusterKafkaVersion", "3.6.0".into()), ("CamelAwsMSKBrokerNodesNumber", Value::Int(2))]);
    }
    // duplicate name → ConflictException
    let e = req_err(&t, &uri, "", &[("CamelAwsMSKOperation", "createCluster".into()), ("CamelAwsMSKClusterName", "orders".into()), ("CamelAwsMSKClusterKafkaVersion", "3.6.0".into()), ("CamelAwsMSKBrokerNodesNumber", Value::Int(1))]);
    assert!(e.is_a("MskException") && e.is_a("AwsServiceException"), "{:?}", e);
    assert!(e.message.contains("ConflictException"), "{}", e);
    // list follows the pages (the emulator pages by 2)
    let all = req(&t, &uri, "", &[("CamelAwsMSKOperation", "listClusters".into())]);
    assert_eq!(all.body().as_array().unwrap().len(), 3);
    let filtered = req(&t, &uri, "", &[("CamelAwsMSKOperation", "listClusters".into()), ("CamelAwsMSKClusterFilter", "other".into())]);
    assert_eq!(filtered.body().as_array().unwrap().len(), 2);
    let d = req(&t, &uri, "", &[("CamelAwsMSKOperation", "describeCluster".into()), ("CamelAwsMSKClusterArn", arn.clone().into())]);
    assert_eq!(s(d.body().get("clusterName").unwrap()), "orders");
    assert_eq!(s(d.header("CamelAwsMSKClusterState").unwrap()), "ACTIVE");
    let bb = req(&t, &uri, "", &[("CamelAwsMSKOperation", "getBootstrapBrokers".into()), ("CamelAwsMSKClusterArn", arn.clone().into())]);
    assert!(s(bb.body().get("bootstrapBrokerStringSaslIam").unwrap()).ends_with(":9098"));
    let del = req(&t, &uri, "", &[("CamelAwsMSKOperation", "deleteCluster".into()), ("CamelAwsMSKClusterArn", arn.clone().into())]);
    assert_eq!(s(del.header("CamelAwsMSKClusterState").unwrap()), "DELETING");
    let e = req_err(&t, &uri, "", &[("CamelAwsMSKOperation", "describeCluster".into()), ("CamelAwsMSKClusterArn", arn.into())]);
    assert!(e.message.contains("NotFoundException"), "{}", e);
    // a wrong secret is rejected by the signature check
    let bad = format!("aws2-msk://msk?{}", bad_options(&mock));
    let e = req_err(&t, &bad, "", &[("CamelAwsMSKOperation", "listClusters".into())]);
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
    assert!(mock.auth_failures() >= 1);
}

#[test]
fn kafka_with_aws_msk_iam_sasl() {
    use camel_kafka_mqtt::testing::{KafkaBroker, KafkaBrokerConfig};
    use std::sync::Arc;
    let verifier: camel_kafka_mqtt::testing::kafka_broker::SaslVerifier = Arc::new(|payload: &[u8]| {
        let secret = |k: &str| (k == "AKIAMSKTEST").then(|| "msk-secret".to_string());
        camel_cloud::aws::msk::verify_iam_payload(payload, Some("127.0.0.1"), &secret, camel_cloud::util::now_secs())
    });
    let broker = KafkaBroker::start_with(KafkaBrokerConfig { sasl_custom: vec![("AWS_MSK_IAM".into(), verifier)], ..Default::default() }).unwrap();
    broker.create_topic("iam", 1);
    let b = broker.bootstrap();
    let ctx = context();
    camel_kafka_mqtt::register(&ctx);
    let jaas = |secret: &str| camel_core::util::url_encode(&format!("software.amazon.msk.auth.iam.IAMLoginModule required awsAccessKeyId=\"AKIAMSKTEST\" awsSecretAccessKey=\"{}\" awsRegion=\"eu-west-1\";", secret));
    let uri = format!("kafka:iam?brokers={}&securityProtocol=SASL_PLAINTEXT&saslMechanism=AWS_MSK_IAM&saslJaasConfig={}", b, jaas("msk-secret"));
    let t = ctx.create_producer_template();
    t.send_body(&uri, "signed with SigV4").unwrap();
    let logins = broker.sasl_logins();
    assert!(logins.contains(&("AWS_MSK_IAM".to_string(), "AKIAMSKTEST".to_string())), "{:?}", logins);
    // a wrong secret fails the SASL handshake
    let bad = format!("kafka:iam?brokers={}&securityProtocol=SASL_PLAINTEXT&saslMechanism=AWS_MSK_IAM&saslJaasConfig={}&requestTimeoutMs=3000", b, jaas("nope"));
    let err = t.send_body(&bad, "x").unwrap_err();
    assert!(err.chain().any(|c| c.kind == "SaslAuthenticationException"), "{:?}", err);
    // consume with IAM authentication too
    let consumer = format!("{}&groupId=g&autoOffsetReset=earliest&pollTimeoutMs=100&heartbeatIntervalMs=50&fetchWaitMaxMs=100", uri);
    ctx.add_routes(|r| {
        r.from(&consumer).to("mock:iam");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:iam").unwrap();
    m.expected_bodies_received(vec!["signed with SigV4"]);
    ctx.start().unwrap();
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    ctx.stop().unwrap();
}

#[test]
fn athena_query_polling_retry_and_pagination() {
    let mock = AwsMock::start().unwrap();
    mock.set_athena_result("FROM orders", vec![vec!["id", "item"], vec!["1", "apple"], vec!["2", "pear"], vec!["3", "plum"]]);
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let uri = format!("aws2-athena://q?database=shop&outputLocation=s3://results/&waitTimeout=5000&initialDelay=1&delay=5&{}", opts);
    let ex = req(&t, &uri, "SELECT * FROM orders", &[]);
    let id = s(ex.header("CamelAwsAthenaQueryExecutionId").unwrap());
    assert_eq!(s(ex.header("CamelAwsAthenaQueryExecutionState").unwrap()), "SUCCEEDED");
    assert_eq!(ex.header("CamelAwsAthenaStartQueryExecutionAttempts").unwrap().as_i64(), Some(1));
    assert!(s(ex.header("CamelAwsAthenaOutputLocation").unwrap()).starts_with("s3://results/"));
    // StreamList (default): every page, header row included
    let rows = req(&t, &format!("aws2-athena://q?operation=getQueryResults&maxResults=2&{}", opts), "", &[("CamelAwsAthenaQueryExecutionId", id.clone().into())]);
    let rows = rows.body().as_array().unwrap().clone();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[2], Value::from(vec!["2", "pear"]));
    // SelectList: one page plus the next token
    let page = req(&t, &format!("aws2-athena://q?operation=getQueryResults&outputType=SelectList&maxResults=2&{}", opts), "", &[("CamelAwsAthenaQueryExecutionId", id.clone().into())]);
    assert_eq!(page.body().get("ResultSet").unwrap().get("Rows").unwrap().as_array().unwrap().len(), 2);
    let token = page.header("CamelAwsAthenaNextToken").unwrap().clone();
    let page2 = req(
        &t,
        &format!("aws2-athena://q?operation=getQueryResults&outputType=SelectList&maxResults=2&{}", opts),
        "",
        &[("CamelAwsAthenaQueryExecutionId", id.clone().into()), ("CamelAwsAthenaNextToken", token)],
    );
    assert!(page2.header("CamelAwsAthenaNextToken").is_none());
    let ptr = req(&t, &format!("aws2-athena://q?operation=getQueryResults&outputType=S3Pointer&{}", opts), "", &[("CamelAwsAthenaQueryExecutionId", id.clone().into())]);
    assert!(s(ptr.body()).ends_with(&format!("{}.csv", id)));
    // a failed query is retried with retry=generic
    mock.athena_fail_next(1);
    let ex = req(&t, &format!("{}&maxAttempts=3&retry=generic", uri), "SELECT * FROM orders", &[]);
    assert_eq!(s(ex.header("CamelAwsAthenaQueryExecutionState").unwrap()), "SUCCEEDED");
    assert_eq!(ex.header("CamelAwsAthenaStartQueryExecutionAttempts").unwrap().as_i64(), Some(2));
    // ... but not with the default retry=never
    mock.athena_fail_next(1);
    let ex = req(&t, &uri, "SELECT 1", &[]);
    assert_eq!(s(ex.header("CamelAwsAthenaQueryExecutionState").unwrap()), "FAILED");
    let exec = req(&t, &format!("aws2-athena://q?operation=getQueryExecution&{}", opts), "", &[("CamelAwsAthenaQueryExecutionId", ex.header("CamelAwsAthenaQueryExecutionId").unwrap().clone())]);
    assert!(s(exec.body().get("Status").unwrap().get("StateChangeReason").unwrap()).contains("GENERIC_INTERNAL_ERROR"));
    // without waitTimeout the id is returned immediately
    let ex = req(&t, &format!("aws2-athena://q?{}", opts), "SELECT 2", &[]);
    assert_eq!(s(ex.header("CamelAwsAthenaQueryExecutionState").unwrap()), "QUEUED");
    let list = req(&t, &format!("aws2-athena://q?operation=listQueryExecutions&{}", opts), "", &[]);
    assert_eq!(list.body().as_array().unwrap().len(), mock.athena_query_count());
    let e = req_err(&t, &format!("aws2-athena://q?{}", bad_options(&mock)), "SELECT 1", &[]);
    assert!(e.is_a("AthenaException") && e.message.contains("SignatureDoesNotMatch"), "{:?}", e);
}

#[test]
fn textract_translate_comprehend() {
    let mock = AwsMock::start().unwrap();
    mock.put_object("docs", "invoice.txt", b"ACME Corp\nTotal: 42 EUR\nDate: 2026-10-31");
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    // detectDocumentText on body bytes
    let ex = req(&t, &format!("aws2-textract://t?{}", opts), Value::Bytes(b"Hello Camel\nSecond line".to_vec()), &[]);
    let lines: Vec<String> = ex.body().get("Blocks").unwrap().as_array().unwrap().iter().filter(|b| s(b.get("BlockType").unwrap()) == "LINE").map(|b| s(b.get("Text").unwrap())).collect();
    assert_eq!(lines, vec!["Hello Camel", "Second line"]);
    // analyzeDocument on an S3 object: key/value pairs
    let ex = req(&t, &format!("aws2-textract://t?operation=analyzeDocument&s3Bucket=docs&s3Object=invoice.txt&{}", opts), "", &[("CamelAwsTextractFeatureTypes", Value::from(vec!["FORMS"]))]);
    assert!(ex.body().get("Blocks").unwrap().as_array().unwrap().iter().any(|b| b.get("Text").map(s).as_deref() == Some("42 EUR")));
    // asynchronous text detection
    let ex = req(&t, &format!("aws2-textract://t?operation=startDocumentTextDetection&s3Bucket=docs&s3Object=invoice.txt&{}", opts), "", &[]);
    let job = ex.header("CamelAwsTextractJobId").unwrap().clone();
    let ex = req(&t, &format!("aws2-textract://t?operation=getDocumentTextDetection&{}", opts), "", &[("CamelAwsTextractJobId", job)]);
    assert_eq!(s(ex.body().get("JobStatus").unwrap()), "SUCCEEDED");
    let e = req_err(&t, &format!("aws2-textract://t?s3Bucket=docs&s3Object=missing.txt&{}", opts), "", &[]);
    assert!(e.is_a("TextractException") && e.message.contains("InvalidS3ObjectException"), "{:?}", e);

    // translate
    mock.add_translation("Hello", "it", "Ciao");
    let ex = req(&t, &format!("aws2-translate://tr?sourceLanguage=en&targetLanguage=it&{}", opts), "Hello", &[]);
    assert_eq!(s(ex.body()), "Ciao");
    let ex = req(&t, &format!("aws2-translate://tr?autodetectSourceLanguage=true&targetLanguage=de&{}", opts), "Ciao a tutti", &[]);
    assert_eq!(s(ex.body()), "[de] Ciao a tutti");
    assert_eq!(s(ex.header("CamelAwsTranslateSourceLanguage").unwrap()), "it");
    let ex = req(&t, &format!("aws2-translate://tr?sourceLanguage=en&targetLanguage=it&{}", opts), "Bye", &[("CamelAwsTranslateTargetLanguage", "fr".into())]);
    assert_eq!(s(ex.body()), "[fr] Bye");
    let e = req_err(&t, &format!("aws2-translate://tr?sourceLanguage=en&targetLanguage=xx&{}", opts), "Bye", &[]);
    assert!(e.is_a("TranslateException") && e.message.contains("UnsupportedLanguagePairException"), "{:?}", e);
    assert!(req_err(&t, &format!("aws2-translate://tr?targetLanguage=it&{}", opts), "x", &[]).message.contains("source language"));

    // comprehend
    let c = |op: &str| format!("aws2-comprehend://c?operation={}&{}", op, opts);
    let ex = req(&t, &c("detectSentiment"), "I love Apache Camel, it is great", &[]);
    assert_eq!(s(ex.body()), "POSITIVE");
    assert!(ex.header("CamelAwsComprehendSentimentScore").unwrap().get("Positive").is_some());
    assert_eq!(s(req(&t, &c("detectSentiment"), "This is terrible", &[]).body()), "NEGATIVE");
    let ex = req(&t, &c("detectDominantLanguage"), "Ciao, grazie mille", &[]);
    assert_eq!(s(ex.header("CamelAwsComprehendDetectedLanguage").unwrap()), "it");
    let ents = req(&t, &c("detectEntities"), "Alice works at ACME in Rome", &[]);
    let names: Vec<String> = ents.body().as_array().unwrap().iter().map(|e| s(e.get("Text").unwrap())).collect();
    assert_eq!(names, vec!["Alice", "ACME", "Rome"]);
    let pii = req(&t, &c("detectPiiEntities"), "mail me at bob@example.com", &[]);
    assert_eq!(s(pii.body().as_array().unwrap()[0].get("Type").unwrap()), "EMAIL");
    assert_eq!(req(&t, &c("detectKeyPhrases"), "camel routes all messages", &[]).body().as_array().unwrap().len(), 3);
    let e = req_err(&t, &format!("aws2-comprehend://c?{}", bad_options(&mock)), "hi", &[]);
    assert!(e.is_a("ComprehendException"), "{:?}", e);
}

#[test]
fn polly_rekognition_transcribe() {
    let mock = AwsMock::start().unwrap();
    mock.put_object("img", "pets.jpg", b"cat dog face");
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let ex = req(&t, &format!("aws2-polly://p?voiceId=Matthew&outputFormat=ogg_vorbis&{}", opts), "Hello world", &[]);
    assert_eq!(ex.body().to_bytes(), b"MOCKAUDIO:Matthew:ogg_vorbis:Hello world".to_vec());
    assert_eq!(s(ex.header("CamelAwsPollyContentType").unwrap()), "audio/ogg");
    assert_eq!(ex.header("CamelAwsPollyRequestCharacters").unwrap().as_i64(), Some(11));
    let voices = req(&t, &format!("aws2-polly://p?operation=describeVoices&languageCode=it-IT&{}", opts), "", &[]);
    assert_eq!(voices.body().as_array().unwrap().len(), 1);
    req(&t, &format!("aws2-polly://p?operation=putLexicon&{}", opts), "<lexicon/>", &[("CamelAwsPollyLexiconName", "camel".into())]);
    let ex = req(&t, &format!("aws2-polly://p?lexiconNames=camel&{}", opts), "Camel", &[]);
    assert!(s(ex.body()).starts_with("MOCKAUDIO:Joanna:mp3"));
    let e = req_err(&t, &format!("aws2-polly://p?voiceId=Nobody&{}", opts), "x", &[]);
    assert!(e.is_a("PollyException") && e.message.contains("ValidationException"), "{:?}", e);
    let task = req(&t, &format!("aws2-polly://p?operation=startSpeechSynthesisTask&{}", opts), "long text", &[("CamelAwsPollyS3Bucket", "audio".into())]);
    let tid = task.header("CamelAwsPollyTaskId").unwrap().clone();
    let got = req(&t, &format!("aws2-polly://p?operation=getSpeechSynthesisTask&{}", opts), "", &[("CamelAwsPollyTaskId", tid)]);
    assert_eq!(s(got.body().get("TaskStatus").unwrap()), "completed");

    // rekognition: labels from the S3 image, faces from bytes
    let labels = req(&t, &format!("aws2-rekognition://r?s3Bucket=img&s3Object=pets.jpg&minConfidence=80&{}", opts), "", &[]);
    let names: Vec<String> = labels.body().as_array().unwrap().iter().map(|l| s(l.get("Name").unwrap())).collect();
    assert_eq!(names, vec!["Cat", "Dog"]);
    let faces = req(&t, &format!("aws2-rekognition://r?operation=detectFaces&{}", opts), Value::Bytes(b"face face".to_vec()), &[]);
    assert_eq!(faces.body().as_array().unwrap().len(), 2);
    assert!(req_err(&t, &format!("aws2-rekognition://r?{}", opts), "", &[]).message.contains("image is required"));

    // transcribe
    let tr = |op: &str| format!("aws2-transcribe://t?operation={}&{}", op, opts);
    let ex = req(&t, &tr("startTranscriptionJob"), "", &[("CamelAwsTranscribeTranscriptionJobName", "job1".into()), ("CamelAwsTranscribeMediaUri", "s3://audio/a.mp3".into())]);
    assert_eq!(s(ex.header("CamelAwsTranscribeTranscriptionJobStatus").unwrap()), "IN_PROGRESS");
    let e = req_err(&t, &tr("startTranscriptionJob"), "", &[("CamelAwsTranscribeTranscriptionJobName", "job1".into()), ("CamelAwsTranscribeMediaUri", "s3://audio/a.mp3".into())]);
    assert!(e.is_a("TranscribeException") && e.message.contains("ConflictException"), "{:?}", e);
    req(&t, &tr("startTranscriptionJob"), "s3://audio/b.mp3", &[("CamelAwsTranscribeTranscriptionJobName", "job2".into())]);
    req(&t, &tr("startTranscriptionJob"), "s3://audio/c.mp3", &[("CamelAwsTranscribeTranscriptionJobName", "job3".into())]);
    let mut status = String::new();
    for _ in 0..3 {
        let ex = req(&t, &tr("getTranscriptionJob"), "", &[("CamelAwsTranscribeTranscriptionJobName", "job1".into())]);
        status = s(ex.header("CamelAwsTranscribeTranscriptionJobStatus").unwrap());
        if status == "COMPLETED" {
            assert!(ex.body().get("Transcript").is_some());
            break;
        }
    }
    assert_eq!(status, "COMPLETED");
    let all = req(&t, &tr("listTranscriptionJobs"), "", &[]);
    assert_eq!(all.body().as_array().unwrap().len(), 3, "all pages");
    let done = req(&t, &tr("listTranscriptionJobs"), "", &[("CamelAwsTranscribeStatus", "COMPLETED".into())]);
    assert_eq!(done.body().as_array().unwrap().len(), 1);
}

#[test]
fn bedrock_invoke_converse_and_knowledge_bases() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let claude = "anthropic.claude-3-haiku-20240307-v1:0";
    let body = Value::object([("anthropic_version", Value::from("bedrock-2023-05-31")), ("max_tokens", Value::Int(100)), ("messages", Value::Array(vec![Value::object([("role", "user"), ("content", "Hi there")])]))]);
    let ex = req(&t, &format!("aws-bedrock://b?modelId={}&{}", claude, opts), body, &[]);
    assert_eq!(s(ex.body().get("content").unwrap().as_array().unwrap()[0].get("text").unwrap()), "Echo: Hi there");
    let (model, sent) = mock.bedrock_requests().pop().unwrap();
    assert_eq!(model, claude);
    assert_eq!(sent.get("max_tokens").unwrap().as_i64(), Some(100));
    // embeddings through the aws2-bedrock alias
    let ex = req(&t, &format!("aws2-bedrock://b?modelId=amazon.titan-embed-text-v2:0&operation=invokeEmbeddingsModel&{}", opts), Value::object([("inputText", "vectorize me")]), &[]);
    assert_eq!(ex.body().get("embedding").unwrap().as_array().unwrap().len(), 8);
    // converse: text body, system prompt, inference config
    let ex = req(
        &t,
        &format!("aws-bedrock://b?modelId={}&operation=converse&{}", claude, opts),
        "What is Camel?",
        &[("CamelAwsBedrockConverseSystem", "be brief".into()), ("CamelAwsBedrockConverseInferenceConfig", Value::object([("maxTokens", Value::Int(50))]))],
    );
    assert_eq!(s(ex.body()), "[be brief] Echo: What is Camel?");
    assert_eq!(s(ex.header("CamelAwsBedrockConverseStopReason").unwrap()), "end_turn");
    assert!(ex.header("CamelAwsBedrockConverseUsage").unwrap().get("totalTokens").is_some());
    // a message list with plain-text content and a token limit
    let msgs = Value::Array(vec![Value::object([("role", "user"), ("content", "one two three four five")])]);
    let ex = req(&t, &format!("aws-bedrock://b?modelId={}&operation=converse&{}", claude, opts), "", &[("CamelAwsBedrockConverseMessages", msgs), ("CamelAwsBedrockConverseInferenceConfig", Value::object([("maxTokens", Value::Int(3))]))]);
    assert_eq!(s(ex.body()), "Echo: one two");
    assert_eq!(s(ex.header("CamelAwsBedrockConverseStopReason").unwrap()), "max_tokens");
    let e = req_err(&t, &format!("aws-bedrock://b?modelId=missing-model&operation=converse&{}", opts), "x", &[]);
    assert!(e.is_a("BedrockRuntimeException") && e.message.contains("ResourceNotFoundException"), "{:?}", e);
    assert!(req_err(&t, &format!("aws-bedrock://b?{}", opts), "x", &[]).message.contains("modelId"));

    // knowledge base retrieval
    mock.add_knowledge_base("KB1", &["Camel routes messages between endpoints", "Rust is a systems language", "Kafka is a log"]);
    let ex = req(&t, &format!("aws-bedrock-agent-runtime://kb?knowledgeBaseId=KB1&{}", opts), "how does camel route messages", &[("CamelAwsBedrockAgentRuntimeNumberOfResults", Value::Int(1))]);
    let results = ex.body().as_array().unwrap().clone();
    assert_eq!(results.len(), 1);
    assert_eq!(s(results[0].get("content").unwrap().get("text").unwrap()), "Camel routes messages between endpoints");
    let ex = req(&t, &format!("aws-bedrock-agent-runtime://kb?knowledgeBaseId=KB1&operation=retrieveAndGenerate&modelId={}&{}", claude, opts), "what is rust", &[]);
    assert!(s(ex.body()).contains("Rust is a systems language"));
    assert!(ex.header("CamelAwsBedrockAgentRuntimeSessionId").is_some());
    let e = req_err(&t, &format!("aws-bedrock-agent-runtime://kb?knowledgeBaseId=NOPE&{}", opts), "q", &[]);
    assert!(e.is_a("BedrockAgentRuntimeException"), "{:?}", e);
    let e = req_err(&t, &format!("aws-bedrock://b?modelId={}&{}", claude, bad_options(&mock)), "x", &[]);
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
}

#[test]
fn mq_ecs_eks() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let mq = |op: &str| format!("aws2-mq://mq?operation={}&{}", op, opts);
    let ex = req(&t, &mq("createBroker"), "", &[("CamelAwsMQBrokerName", "orders".into()), ("CamelAwsMQBrokerUsers", Value::Array(vec![Value::object([("username", "admin"), ("password", "secret-pass-123")])]))]);
    let id = ex.header("CamelAwsMQBrokerId").unwrap().clone();
    req(&t, &mq("createBroker"), "", &[("CamelAwsMQBrokerName", "audit".into())]);
    assert_eq!(req(&t, &mq("listBrokers"), "", &[("CamelAwsMQMaxResults", Value::Int(1))]).body().as_array().unwrap().len(), 2, "all pages");
    let d = req(&t, &mq("describeBroker"), "", &[("CamelAwsMQBrokerId", id.clone())]);
    assert_eq!(s(d.body().get("brokerState").unwrap()), "RUNNING");
    assert_eq!(s(d.body().get("engineType").unwrap()), "ACTIVEMQ");
    req(&t, &mq("rebootBroker"), "", &[("CamelAwsMQBrokerId", id.clone())]);
    req(&t, &mq("deleteBroker"), "", &[("CamelAwsMQBrokerId", id.clone())]);
    let e = req_err(&t, &mq("describeBroker"), "", &[("CamelAwsMQBrokerId", id)]);
    assert!(e.is_a("MqException") && e.message.contains("NotFoundException"), "{:?}", e);

    let ecs = |op: &str| format!("aws2-ecs://ecs?operation={}&{}", op, opts);
    for n in ["a", "b", "c"] {
        req(&t, &ecs("createCluster"), "", &[("CamelAwsECSClusterName", n.into())]);
    }
    let arns = req(&t, &ecs("listClusters"), "", &[]);
    assert_eq!(arns.body().as_array().unwrap().len(), 3);
    let c = req(&t, &ecs("describeCluster"), "", &[("CamelAwsECSClusterName", "b".into())]);
    assert_eq!(s(c.body().get("status").unwrap()), "ACTIVE");
    let del = req(&t, &ecs("deleteCluster"), "", &[("CamelAwsECSClusterName", "b".into())]);
    assert_eq!(s(del.body().get("status").unwrap()), "INACTIVE");
    let e = req_err(&t, &ecs("deleteCluster"), "", &[("CamelAwsECSClusterName", "b".into())]);
    assert!(e.is_a("EcsException") && e.message.contains("ClusterNotFoundException"), "{:?}", e);

    let eks = |op: &str| format!("aws2-eks://eks?operation={}&{}", op, opts);
    let ex = req(
        &t,
        &eks("createCluster"),
        "",
        &[("CamelAwsEKSClusterName", "k8s".into()), ("CamelAwsEKSRoleARN", "arn:aws:iam::000000000000:role/eks".into()), ("CamelAwsEKSVPCConfig", Value::object([("subnetIds", Value::from(vec!["subnet-1"]))]))],
    );
    assert_eq!(s(ex.body().get("status").unwrap()), "ACTIVE");
    assert_eq!(req(&t, &eks("listClusters"), "", &[]).body(), &Value::from(vec!["k8s"]));
    assert_eq!(s(req(&t, &eks("describeCluster"), "", &[("CamelAwsEKSClusterName", "k8s".into())]).body().get("roleArn").unwrap()), "arn:aws:iam::000000000000:role/eks");
    let e = req_err(&t, &eks("createCluster"), "", &[("CamelAwsEKSClusterName", "k8s".into()), ("CamelAwsEKSRoleARN", "r".into())]);
    assert!(e.is_a("EksException") && e.message.contains("ResourceInUseException"), "{:?}", e);
    req(&t, &eks("deleteCluster"), "", &[("CamelAwsEKSClusterName", "k8s".into())]);
    assert!(req(&t, &eks("listClusters"), "", &[]).body().as_array().unwrap().is_empty());
    let e = req_err(&t, &format!("aws2-eks://eks?operation=listClusters&{}", bad_options(&mock)), "", &[]);
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
}

#[test]
fn kms_keys_and_envelope() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let kms = |op: &str| format!("aws2-kms://kms?operation={}&{}", op, opts);
    let ex = req(&t, &kms("createKey"), "", &[("CamelAwsKMSDescription", "orders key".into())]);
    let key = ex.header("CamelAwsKMSKeyId").unwrap().clone();
    assert_eq!(s(ex.body().get("KeyState").unwrap()), "Enabled");
    req(&t, &kms("createKey"), "", &[]);
    req(&t, &kms("createKey"), "", &[]);
    let keys = req(&t, &kms("listKeys"), "", &[("CamelAwsKMSLimit", Value::Int(2))]);
    assert_eq!(keys.body().as_array().unwrap().len(), 3, "follows NextMarker");
    let blob = req(&t, &kms("encrypt"), Value::Bytes(b"top secret".to_vec()), &[("CamelAwsKMSKeyId", key.clone())]).body().to_bytes();
    assert_ne!(blob, b"top secret".to_vec());
    let plain = req(&t, &kms("decrypt"), Value::Bytes(blob.clone()), &[]);
    assert_eq!(plain.body().to_bytes(), b"top secret".to_vec());
    assert!(s(plain.header("CamelAwsKMSKeyId").unwrap()).ends_with(&s(&key)));
    req(&t, &kms("disableKey"), "", &[("CamelAwsKMSKeyId", key.clone())]);
    assert_eq!(s(req(&t, &kms("describeKey"), "", &[("CamelAwsKMSKeyId", key.clone())]).body().get("KeyState").unwrap()), "Disabled");
    let e = req_err(&t, &kms("decrypt"), Value::Bytes(blob), &[]);
    assert!(e.is_a("KmsException") && e.message.contains("DisabledException"), "{:?}", e);
    req(&t, &kms("enableKey"), "", &[("CamelAwsKMSKeyId", key.clone())]);
    let del = req(&t, &kms("scheduleKeyDeletion"), "", &[("CamelAwsKMSKeyId", key.clone()), ("CamelAwsKMSPendingWindowInDays", Value::Int(7))]);
    assert_eq!(s(del.body().get("KeyState").unwrap()), "PendingDeletion");
    let e = req_err(&t, &kms("describeKey"), "", &[("CamelAwsKMSKeyId", "nope".into())]);
    assert!(e.message.contains("NotFoundException"), "{}", e);
    assert!(req_err(&t, &kms("describeKey"), "", &[]).message.contains("CamelAwsKMSKeyId"));
    let e = req_err(&t, &format!("aws2-kms://kms?operation=listKeys&{}", bad_options(&mock)), "", &[]);
    assert!(e.is_a("KmsException") && e.message.contains("SignatureDoesNotMatch"), "{:?}", e);
}

#[test]
fn timestream_write_and_query() {
    let mock = AwsMock::start().unwrap();
    mock.set_timestream_page_size(1);
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let w = |op: &str| format!("aws2-timestream://write:ts?operation={}&{}", op, opts);
    let db = ("CamelAwsTimestreamDatabaseName", Value::from("iot"));
    let table = ("CamelAwsTimestreamTableName", Value::from("readings"));
    req(&t, &w("createDatabase"), "", std::slice::from_ref(&db));
    let e = req_err(&t, &w("createDatabase"), "", std::slice::from_ref(&db));
    assert!(e.is_a("TimestreamWriteException") && e.message.contains("ConflictException"), "{:?}", e);
    req(&t, &w("createTable"), "", &[db.clone(), table.clone(), ("CamelAwsTimestreamMemoryStoreRetentionPeriodInHours", Value::Int(24))]);
    assert_eq!(req(&t, &w("listTables"), "", std::slice::from_ref(&db)).body().as_array().unwrap().len(), 1);
    let rec = |dev: &str, v: &str| Value::object([("Dimensions", Value::Array(vec![Value::object([("Name", "device"), ("Value", dev)])])), ("MeasureValue", Value::from(v)), ("Time", Value::from("1700000000000"))]);
    let common = Value::object([("MeasureName", "temperature"), ("MeasureValueType", "DOUBLE")]);
    let ex = req(&t, &w("writeRecords"), Value::Array(vec![rec("d1", "21.5"), rec("d2", "19.0")]), &[db.clone(), table.clone(), ("CamelAwsTimestreamCommonAttributes", common)]);
    assert_eq!(ex.body().get("Total").unwrap().as_i64(), Some(2));
    assert_eq!(mock.timestream_records("iot", "readings").len(), 2);
    // a record without a measure is rejected
    let e = req_err(&t, &w("writeRecords"), Value::object([("Dimensions", Value::Array(vec![]))]), &[db.clone(), table.clone()]);
    assert!(e.message.contains("RejectedRecordsException"), "{}", e);
    // query follows NextToken (one row per page here)
    let ex = req(&t, &format!("aws2-timestream://query:ts?{}", opts), "SELECT * FROM \"iot\".\"readings\"", &[]);
    let rows = ex.body().as_array().unwrap().clone();
    assert_eq!(rows.len(), 2);
    assert_eq!(s(rows[0].get("device").unwrap()), "d1");
    assert_eq!(s(rows[1].get("measure_value").unwrap()), "19.0");
    assert_eq!(s(rows[1].get("measure_name").unwrap()), "temperature");
    assert!(ex.header("CamelAwsTimestreamQueryId").is_some());
    let eps = req(&t, &format!("aws2-timestream://query:ts?operation=describeEndpoints&{}", opts), "", &[]);
    assert_eq!(eps.body().as_array().unwrap().len(), 1);
    let e = req_err(&t, &format!("aws2-timestream://query:ts?{}", opts), "SELECT * FROM nowhere", &[]);
    assert!(e.is_a("TimestreamQueryException"), "{:?}", e);
    assert!(ctx.get_endpoint("aws2-timestream://other:x").is_err());
    let e = req_err(&t, &format!("aws2-timestream://query:ts?{}", bad_options(&mock)), "SELECT 1", &[]);
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
}

#[test]
fn cloudtrail_consumer_delivers_each_event_once() {
    let mock = AwsMock::start().unwrap();
    let first = mock.add_cloudtrail_event("s3.amazonaws.com", "PutObject", "alice");
    mock.add_cloudtrail_event("ec2.amazonaws.com", "RunInstances", "bob");
    std::thread::sleep(Duration::from_millis(5));
    let second = mock.add_cloudtrail_event("s3.amazonaws.com", "DeleteObject", "carol");
    let ctx = context();
    let opts = mock.endpoint_options();
    ctx.add_routes(move |b| {
        b.from(&format!("aws-cloudtrail://trail?eventSource=s3.amazonaws.com&maxResults=10&delay=20&initialDelay=0&{}", opts)).to("mock:trail");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:trail").unwrap();
    m.expected_message_count(3);
    ctx.start().unwrap();
    // a later event is picked up by a following poll
    std::thread::sleep(Duration::from_millis(100));
    let third = mock.add_cloudtrail_event("s3.amazonaws.com", "GetObject", "dave");
    m.assert_is_satisfied_within(Duration::from_secs(10)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let ids: Vec<String> = m.received_exchanges().iter().map(|e| s(e.header("CamelAwsCloudTrailEventId").unwrap())).collect();
    assert_eq!(ids, vec![first, second, third], "oldest first, no duplicates");
    let ex = &m.received_exchanges()[0];
    assert_eq!(s(ex.header("CamelAwsCloudTrailEventUsername").unwrap()), "alice");
    assert_eq!(s(ex.header("CamelAwsCloudTrailEventName").unwrap()), "PutObject");
    assert!(String::from_utf8(ex.body().to_bytes()).unwrap().contains("\"eventName\":\"PutObject\""));
    ctx.stop().unwrap();
    assert!(context().create_producer_template().request_body(&format!("aws-cloudtrail://trail?{}", mock.endpoint_options()), "x").is_err());
}

#[test]
fn config_rules_and_conformance_packs() {
    let mock = AwsMock::start().unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = mock.endpoint_options();
    let c = |op: &str| format!("aws-config://cfg?operation={}&{}", op, opts);
    req(
        &t,
        &c("putConfigRule"),
        "",
        &[("CamelAwsConfigRuleName", "s3-encryption".into()), ("CamelAwsConfigRuleSourceIdentifier", "S3_BUCKET_SERVER_SIDE_ENCRYPTION_ENABLED".into()), ("CamelAwsConfigRuleInputParameters", Value::object([("x", "1")]))],
    );
    req(&t, &c("putConfigRule"), "", &[("CamelAwsConfigRuleName", "tags".into()), ("CamelAwsConfigRuleSourceIdentifier", "REQUIRED_TAGS".into())]);
    assert_eq!(mock.config_rule_names(), vec!["s3-encryption", "tags"]);
    let rules = req(&t, &c("describeConfigRules"), "", &[("CamelAwsConfigRuleName", "s3-encryption".into())]);
    let r = &rules.body().as_array().unwrap()[0];
    assert_eq!(s(r.get("InputParameters").unwrap()), "{\"x\":\"1\"}");
    assert_eq!(s(r.get("Source").unwrap().get("Owner").unwrap()), "AWS");
    mock.set_config_compliance("tags", "NON_COMPLIANT");
    let comp = req(&t, &c("describeRuleCompliance"), "", &[]);
    let types: Vec<String> = comp.body().as_array().unwrap().iter().map(|x| s(x.get("Compliance").unwrap().get("ComplianceType").unwrap())).collect();
    assert_eq!(types, vec!["COMPLIANT", "NON_COMPLIANT"]);
    req(&t, &c("removeConfigRule"), "", &[("CamelAwsConfigRuleName", "tags".into())]);
    let e = req_err(&t, &c("removeConfigRule"), "", &[("CamelAwsConfigRuleName", "tags".into())]);
    assert!(e.is_a("ConfigException") && e.message.contains("NoSuchConfigRuleException"), "{:?}", e);
    let arn = req(&t, &c("putConformancePack"), "Resources: {}", &[("CamelAwsConformancePackName", "baseline".into())]);
    assert!(s(arn.body()).contains("conformance-pack/baseline"));
    let pc = req(&t, &c("describeConformancePackCompliance"), "", &[("CamelAwsConformancePackName", "baseline".into())]);
    assert_eq!(pc.body().as_array().unwrap().len(), 1);
    req(&t, &c("removeConformancePack"), "", &[("CamelAwsConformancePackName", "baseline".into())]);
    let e = req_err(&t, &format!("aws-config://cfg?operation=describeConfigRules&{}", bad_options(&mock)), "", &[]);
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
}

#[test]
fn redshift_data_statements() {
    let mock = AwsMock::start().unwrap();
    mock.add_redshift_table("users", &["id", "name", "active"], vec![
        vec![Value::Int(1), Value::from("ann"), Value::Bool(true)],
        vec![Value::Int(2), Value::from("bob"), Value::Bool(false)],
        vec![Value::Int(3), Value::Null, Value::Bool(true)],
    ]);
    let ctx = context();
    let t = ctx.create_producer_template();
    let opts = format!("database=dev&clusterIdentifier=redshift-1&{}", mock.endpoint_options());
    let r = |op: &str| format!("aws2-redshift-data://rs?operation={}&{}", op, opts);
    // execute and wait (rust-camel waitTimeout), then fetch every page (2 rows per page)
    let ex = req(&t, &format!("{}&waitTimeout=5000&delay=5", r("executeStatement")), "SELECT * FROM users", &[]);
    assert_eq!(s(ex.header("CamelAwsRedshiftDataStatus").unwrap()), "FINISHED");
    let id = ex.header("CamelAwsRedshiftDataStatementId").unwrap().clone();
    let rows = req(&t, &r("getStatementResult"), "", &[("CamelAwsRedshiftDataStatementId", id.clone())]);
    assert_eq!(rows.header("CamelAwsRedshiftDataTotalNumRows").unwrap().as_i64(), Some(3));
    let rows = rows.body().as_array().unwrap().clone();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get("name").unwrap(), &Value::from("ann"));
    assert_eq!(rows[1].get("active").unwrap(), &Value::Bool(false));
    assert!(rows[2].get("name").unwrap().is_null());
    // without waiting: describe until finished
    let ex = req(&t, &r("executeStatement"), "", &[("CamelAwsRedshiftDataSql", "SELECT 1".into())]);
    let id2 = ex.header("CamelAwsRedshiftDataStatementId").unwrap().clone();
    let e = req_err(&t, &r("getStatementResult"), "", &[("CamelAwsRedshiftDataStatementId", id2.clone())]);
    assert!(e.is_a("RedshiftDataException") && e.message.contains("ValidationException"), "{:?}", e);
    let mut status = String::new();
    for _ in 0..3 {
        status = s(req(&t, &r("describeStatement"), "", &[("CamelAwsRedshiftDataStatementId", id2.clone())]).header("CamelAwsRedshiftDataStatus").unwrap());
    }
    assert_eq!(status, "FINISHED");
    // a failing statement raises when waiting
    let e = req_err(&t, &format!("{}&waitTimeout=5000&delay=5", r("executeStatement")), "SELECT * FROM missing", &[]);
    assert!(e.message.contains("does not exist"), "{}", e);
    req(&t, &r("batchExecuteStatement"), Value::from(vec!["CREATE TABLE t (a int)", "INSERT INTO t VALUES (1)"]), &[]);
    assert_eq!(req(&t, &r("listStatements"), "", &[]).body().as_array().unwrap().len(), 4);
    assert_eq!(req(&t, &r("listDatabases"), "", &[]).body(), &Value::from(vec!["dev", "sample_data_dev"]));
    assert_eq!(req(&t, &r("describeTable"), "", &[("CamelAwsRedshiftDataTable", "users".into())]).body().as_array().unwrap().len(), 3);
    assert!(req(&t, &r("cancelStatement"), "", &[("CamelAwsRedshiftDataStatementId", id)]).body().is_truthy());
    // the cluster / workgroup is required by the API
    let e = req_err(&t, &format!("aws2-redshift-data://rs?operation=executeStatement&database=dev&{}", mock.endpoint_options()), "SELECT 1", &[]);
    assert!(e.message.contains("ValidationException"), "{}", e);
    let e = req_err(&t, &format!("aws2-redshift-data://rs?operation=listStatements&{}", bad_options(&mock)), "", &[]);
    assert!(e.message.contains("SignatureDoesNotMatch"), "{}", e);
}
