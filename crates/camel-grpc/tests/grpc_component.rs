//! gRPC round trips through Camel routes: a consumer route implements
//! `helloworld.Greeter`, producers call it.

use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_grpc::grpc::{Code, GrpcClient};
use camel_grpc::DescriptorPool;

const PROTO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/greeter.proto");

fn greeter(ex: &mut Exchange) -> Result<()> {
    let method = ex.header("CamelGrpcMethodName").map(|v| v.to_string()).unwrap_or_default();
    match method.as_str() {
        "sayHello" => {
            let name = ex.body().get("name").map(|v| v.to_string()).unwrap_or_default();
            match name.as_str() {
                "bad" => return Err(CamelError::illegal_argument("name must not be 'bad'")),
                "boom" => return Err(CamelError::new(kinds::CAMEL_EXCHANGE, "boom happened")),
                "missing" => return Err(CamelError::new("StatusRuntimeException", "NOT_FOUND: no such person")),
                "slow" => std::thread::sleep(Duration::from_millis(600)),
                _ => {}
            }
            let tenant = ex.header("x-tenant").map(|v| v.to_string()).unwrap_or_default();
            let mood = ex.body().get("mood").map(|v| v.to_string()).unwrap_or_default();
            ex.set_body(Value::object([("message", Value::from(format!("Hello {}{}", name, if tenant.is_empty() { String::new() } else { format!(" from {}", tenant) }))), ("index", Value::Int(mood.len() as i64))]));
        }
        "sayHelloStream" => {
            let name = ex.body().get("name").map(|v| v.to_string()).unwrap_or_default();
            let times = ex.body().get("times").and_then(|v| v.as_i64()).unwrap_or(1);
            let replies: Vec<Value> = (0..times).map(|i| Value::object([("message", Value::from(format!("Hello {} #{}", name, i))), ("index", Value::Int(i))])).collect();
            ex.set_body(Value::Array(replies));
        }
        "sayHelloToAll" => {
            let names: Vec<String> = ex.body().as_array().map(|a| a.iter().map(|r| r.get("name").map(|v| v.to_string()).unwrap_or_default()).collect()).unwrap_or_default();
            ex.set_body(Value::object([("message", Value::from(format!("Hello {}", names.join(", ")))), ("index", Value::Int(names.len() as i64))]));
        }
        other => return Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("method {} not implemented", other))),
    }
    Ok(())
}

struct Fixture {
    ctx: CamelContext,
    port: u16,
}

fn start(extra_routes: impl FnOnce(&mut RouteBuilder)) -> Fixture {
    let ctx = CamelContext::new();
    camel_grpc::register(&ctx);
    let consumer = format!("grpc://127.0.0.1:0/helloworld.Greeter?protoFile={}", PROTO);
    let c2 = consumer.clone();
    ctx.add_routes(move |b| {
        b.from(&c2).process(greeter);
        extra_routes(b);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_grpc::local_port(&ctx, &consumer).expect("consumer port");
    Fixture { ctx, port }
}

impl Fixture {
    fn uri(&self, method: &str, extra: &str) -> String {
        format!("grpc://127.0.0.1:{}/helloworld.Greeter?method={}&protoFile={}&synchronous=true{}", self.port, method, PROTO, extra)
    }
}

#[test]
fn unary_round_trip_through_routes() {
    let f = start(|_| {});
    let t = f.ctx.create_producer_template();
    let reply = t.request_body(&f.uri("sayHello", ""), Value::object([("name", Value::from("Camel")), ("mood", Value::from("HAPPY"))])).unwrap();
    assert_eq!(reply, Value::object([("message", Value::from("Hello Camel")), ("index", Value::Int(5))]));
    // proto method name works too, JSON text bodies are accepted, metadata -> headers
    let reply = t.request_body_and_header(&f.uri("SayHello", ""), r#"{"name": "Json"}"#, "x-tenant", "acme").unwrap();
    assert_eq!(reply.get("message"), Some(&Value::from("Hello Json from acme")));
    // many concurrent calls over the shared connection
    let hs: Vec<_> = (0..16)
        .map(|i| {
            let t = f.ctx.create_producer_template();
            let uri = f.uri("sayHello", "");
            std::thread::spawn(move || t.request_body(&uri, Value::object([("name", Value::from(format!("n{}", i)))])).unwrap())
        })
        .collect();
    for (i, h) in hs.into_iter().enumerate() {
        assert_eq!(h.join().unwrap().get("message"), Some(&Value::from(format!("Hello n{}", i))));
    }
    f.ctx.stop().unwrap();
}

#[test]
fn server_streaming_array_and_stream_replies_to() {
    let f = start(|_| {});
    let t = f.ctx.create_producer_template();
    let reply = t.request_body(&f.uri("sayHelloStream", ""), Value::object([("name", Value::from("S")), ("times", Value::Int(3))])).unwrap();
    let arr = reply.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[2], Value::object([("message", Value::from("Hello S #2")), ("index", Value::Int(2))]));
    // per-message streaming to another endpoint
    let mock = f.ctx.mock_endpoint("mock:replies").unwrap();
    mock.expected_message_count(4);
    mock.expected_header_received("CamelGrpcEventType", "onNext");
    let reply = t.request_body(&f.uri("sayHelloStream", "&streamRepliesTo=mock:replies"), Value::object([("name", Value::from("M")), ("times", Value::Int(4))])).unwrap();
    assert_eq!(reply.as_array().unwrap().len(), 4);
    mock.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    assert_eq!(mock.received_bodies()[0].get("message"), Some(&Value::from("Hello M #0")));
    // zero replies
    assert_eq!(t.request_body(&f.uri("sayHelloStream", ""), Value::object([("name", Value::from("Z")), ("times", Value::Int(0))])).unwrap(), Value::Array(vec![]));
    f.ctx.stop().unwrap();
}

#[test]
fn client_streaming_aggregation_and_bidi_propagation() {
    let ctx = CamelContext::new();
    camel_grpc::register(&ctx);
    let agg = format!("grpc://127.0.0.1:0/helloworld.Greeter?protoFile={}", PROTO);
    let prop = format!("grpc://127.0.0.1:0/helloworld.Greeter?protoFile={}&consumerStrategy=PROPAGATION&forwardOnCompleted=true", PROTO);
    let (a2, p2) = (agg.clone(), prop.clone());
    ctx.add_routes(move |b| {
        b.from(&a2).process(greeter);
        b.from(&p2).process(|ex: &mut Exchange| {
            let event = ex.header("CamelGrpcEventType").map(|v| v.to_string()).unwrap_or_default();
            if event == "onCompleted" {
                ex.set_body(Value::object([("message", Value::from("bye"))]));
            } else {
                let name = ex.body().get("name").map(|v| v.to_string()).unwrap_or_default();
                ex.set_body(Value::object([("message", Value::from(format!("echo {}", name)))]));
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let (pa, pp) = (camel_grpc::local_port(&ctx, &agg).unwrap(), camel_grpc::local_port(&ctx, &prop).unwrap());
    let t = ctx.create_producer_template();
    let names = Value::Array(vec![Value::object([("name", "a")]), Value::object([("name", "b")]), Value::object([("name", "c")])]);
    let uri = format!("grpc://127.0.0.1:{}/helloworld.Greeter?method=sayHelloToAll&protoFile={}", pa, PROTO);
    let reply = t.request_body(&uri, names.clone()).unwrap();
    assert_eq!(reply, Value::object([("message", Value::from("Hello a, b, c")), ("index", Value::Int(3))]));
    let uri = format!("grpc://127.0.0.1:{}/helloworld.Greeter?method=chat&protoFile={}", pp, PROTO);
    let reply = t.request_body(&uri, names).unwrap();
    let msgs: Vec<String> = reply.as_array().unwrap().iter().map(|r| r.get("message").unwrap().to_string()).collect();
    assert_eq!(msgs, vec!["echo a", "echo b", "echo c", "bye"]);

    // STREAMING producer: one long-lived bidi call across exchanges, replies routed
    let mock = ctx.mock_endpoint("mock:chat").unwrap();
    mock.expected_minimum_message_count(4);
    let uri = format!("grpc://127.0.0.1:{}/helloworld.Greeter?method=chat&protoFile={}&producerStrategy=STREAMING&streamRepliesTo=mock:chat", pp, PROTO);
    t.send_body(&uri, Value::object([("name", "x")])).unwrap();
    t.send_body(&uri, Value::object([("name", "y")])).unwrap();
    t.send_body_and_header(&uri, Value::object([("name", "z")]), "CamelGrpcEventType", "onCompleted").unwrap();
    mock.assert_is_satisfied_within(Duration::from_secs(5)).unwrap();
    let bodies: Vec<Value> = mock.received_bodies();
    assert_eq!(bodies[0].get("message"), Some(&Value::from("echo x")));
    assert_eq!(bodies[2].get("message"), Some(&Value::from("echo z")));
    assert_eq!(bodies[3].get("message"), Some(&Value::from("bye")));
    ctx.stop().unwrap();
}

fn status_of(e: &CamelError) -> String {
    e.chain().find(|c| c.kind == "StatusRuntimeException").map(|c| c.message.clone()).unwrap_or_else(|| format!("{}: {}", e.kind, e.message))
}

#[test]
fn error_status_mapping() {
    let f = start(|_| {});
    let t = f.ctx.create_producer_template();
    let err = t.request_body(&f.uri("sayHello", ""), Value::object([("name", "bad")])).unwrap_err();
    assert_eq!(status_of(&err), "INVALID_ARGUMENT: name must not be 'bad'");
    assert!(err.chain().any(|c| c.is_a("RuntimeException")));
    let err = t.request_body(&f.uri("sayHello", ""), Value::object([("name", "boom")])).unwrap_err();
    assert_eq!(status_of(&err), "INTERNAL: boom happened");
    let err = t.request_body(&f.uri("sayHello", ""), Value::object([("name", "missing")])).unwrap_err();
    assert_eq!(status_of(&err), "NOT_FOUND: no such person");

    // byte-level client: unknown method / service, malformed request
    let client = GrpcClient::plaintext(&format!("127.0.0.1:{}", f.port));
    let e = client.unary("/helloworld.Greeter/Nope", b"", &vec![], None).unwrap_err();
    assert_eq!(e.code, Code::Unimplemented);
    let e = client.unary("/other.Service/SayHello", b"", &vec![], None).unwrap_err();
    assert_eq!(e.code, Code::Unimplemented);
    let e = client.unary("/helloworld.Greeter/SayHello", &[0x0a, 0x05, b'x'], &vec![], None).unwrap_err();
    assert_eq!(e.code, Code::Internal, "malformed request: got {:?}: {}", e.code, e.message);
    // a valid raw call
    let pool = DescriptorPool::from_file(PROTO).unwrap();
    let req = pool.encode("helloworld.HelloRequest", &Value::object([("name", "raw")])).unwrap();
    let (resp, _) = client.unary("/helloworld.Greeter/SayHello", &req, &vec![("x-tenant".into(), "t1".into())], None).unwrap();
    assert_eq!(pool.decode("helloworld.HelloReply", &resp).unwrap().get("message"), Some(&Value::from("Hello raw from t1")));
    // producer to a closed port -> UNAVAILABLE
    let err = t.request_body(&format!("grpc://127.0.0.1:1/helloworld.Greeter?method=sayHello&protoFile={}", PROTO), Value::object([("name", "x")])).unwrap_err();
    assert!(status_of(&err).starts_with("UNAVAILABLE"), "{}", status_of(&err));
    // unknown method in the uri fails at endpoint creation
    assert!(f.ctx.get_endpoint(&f.uri("nope", "")).is_err());
    f.ctx.stop().unwrap();
}

#[test]
fn deadline_exceeded() {
    let f = start(|_| {});
    let t = f.ctx.create_producer_template();
    let start = Instant::now();
    let err = t.request_body(&f.uri("sayHello", "&deadline=150"), Value::object([("name", "slow")])).unwrap_err();
    assert!(status_of(&err).starts_with("DEADLINE_EXCEEDED"), "{}", status_of(&err));
    assert!(start.elapsed() < Duration::from_millis(550), "{:?}", start.elapsed());
    // the server side enforces grpc-timeout as well (client without local timer check)
    let client = GrpcClient::plaintext(&format!("127.0.0.1:{}", f.port));
    let pool = DescriptorPool::from_file(PROTO).unwrap();
    let req = pool.encode("helloworld.HelloRequest", &Value::object([("name", "slow")])).unwrap();
    let mut call = client.start_call("/helloworld.Greeter/SayHello", &vec![("grpc-timeout".into(), "1S".into())], Some(Duration::from_millis(100))).unwrap();
    call.send(&req).unwrap();
    call.close_send().unwrap();
    assert_eq!(call.recv().unwrap_err().code, Code::DeadlineExceeded);
    // a fast call with a deadline succeeds
    let reply = t.request_body(&f.uri("sayHello", "&deadline=5s"), Value::object([("name", "quick")])).unwrap();
    assert_eq!(reply.get("message"), Some(&Value::from("Hello quick")));
    f.ctx.stop().unwrap();
}

#[test]
fn tls_with_alpn_h2() {
    let dir = std::env::temp_dir().join(format!("camel-grpc-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ss = camel_tls::self_signed("localhost");
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    std::fs::write(&cert, &ss.cert_pem).unwrap();
    std::fs::write(&key, &ss.key_pem).unwrap();
    let ctx = CamelContext::new();
    camel_grpc::register(&ctx);
    let consumer = format!(
        "grpc://localhost:0/helloworld.Greeter?protoFile={}&usePlainText=false&keyCertChainResource={}&keyResource={}",
        PROTO,
        cert.display(),
        key.display()
    );
    let c2 = consumer.clone();
    ctx.add_routes(move |b| {
        b.from(&c2).process(greeter);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_grpc::local_port(&ctx, &consumer).unwrap();
    let t = ctx.create_producer_template();
    let uri = format!(
        "grpc://localhost:{}/helloworld.Greeter?method=sayHello&protoFile={}&usePlainText=false&trustCertCollectionResource={}",
        port,
        PROTO,
        cert.display()
    );
    let reply = t.request_body(&uri, Value::object([("name", "TLS")])).unwrap();
    assert_eq!(reply.get("message"), Some(&Value::from("Hello TLS")));
    let uri = uri.replace("method=sayHello", "method=sayHelloStream");
    let reply = t.request_body(&uri, Value::object([("name", Value::from("TLS")), ("times", Value::Int(2))])).unwrap();
    assert_eq!(reply.as_array().unwrap().len(), 2);
    // a plaintext client cannot talk to the TLS server
    let err = t.request_body(&format!("grpc://localhost:{}/helloworld.Greeter?method=sayHello&protoFile={}&deadline=2s", port, PROTO), Value::object([("name", "x")])).unwrap_err();
    assert!(!status_of(&err).starts_with("OK"));
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn raw_bytes_mode_without_proto_file() {
    let ctx = CamelContext::new();
    camel_grpc::register(&ctx);
    let consumer = "grpc://127.0.0.1:0/raw.Echo";
    ctx.add_routes(|b| {
        b.from(consumer).process(|ex: &mut Exchange| {
            let mut b = ex.body().to_bytes();
            b.reverse();
            ex.set_body(Value::Bytes(b));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_grpc::local_port(&ctx, consumer).unwrap();
    let t = ctx.create_producer_template();
    let reply = t.request_body(&format!("grpc://127.0.0.1:{}/raw.Echo?method=echo", port), Value::Bytes(vec![1, 2, 3])).unwrap();
    assert_eq!(reply, Value::Bytes(vec![3, 2, 1]));
    ctx.stop().unwrap();
}

#[test]
fn protobuf_data_format() {
    let ctx = CamelContext::new();
    camel_grpc::register(&ctx);
    let native = camel_grpc::protobuf(PROTO, "helloworld.HelloRequest").unwrap();
    let json = camel_grpc::protobuf_json(PROTO, "HelloRequest").unwrap();
    let (n2, j2) = (native.clone(), json.clone());
    ctx.add_routes(move |b| {
        b.from("direct:marshal").marshal(n2.clone());
        b.from("direct:unmarshal").unmarshal(n2);
        b.from("direct:toJson").marshal(j2.clone());
        b.from("direct:fromJson").unmarshal(j2);
        b.from("direct:named").marshal("protobuf");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let req = Value::object([("name", Value::from("testing")), ("times", Value::Int(150))]);
    let bytes = t.request_body("direct:marshal", req.clone()).unwrap();
    assert_eq!(bytes, Value::Bytes(vec![0x0a, 0x07, b't', b'e', b's', b't', b'i', b'n', b'g', 0x10, 0x96, 0x01]));
    let back = t.request_body("direct:unmarshal", bytes).unwrap();
    assert_eq!(back, Value::object([("name", Value::from("testing")), ("times", Value::Int(150)), ("mood", Value::from("NEUTRAL"))]));
    let text = t.request_body("direct:toJson", Value::object([("name", Value::from("j")), ("mood", Value::from("HAPPY"))])).unwrap();
    let parsed = camel_core::json::parse(&text.to_string()).unwrap();
    assert_eq!(parsed, Value::object([("name", Value::from("j")), ("mood", Value::from("HAPPY"))]));
    let obj = t.request_body("direct:fromJson", r#"{"name":"k","times":2}"#).unwrap();
    assert_eq!(obj.get("times"), Some(&Value::Int(2)));
    // named registry bean configured through properties
    ctx.properties().set("camel.dataformat.protobuf.instanceClass", "helloworld.HelloRequest");
    ctx.properties().set("camel.dataformat.protobuf.protoFile", PROTO);
    assert_eq!(t.request_body("direct:named", Value::object([("times", Value::Int(1))])).unwrap(), Value::Bytes(vec![0x10, 0x01]));
    // invalid input is reported
    assert!(t.request_body("direct:marshal", Value::object([("nope", Value::Int(1))])).is_err());
    ctx.stop().unwrap();
}

#[test]
fn curl_grpc_call_to_camel_consumer() {
    let has_h2 = std::process::Command::new("curl").arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).contains("HTTP2")).unwrap_or(false);
    if !has_h2 {
        eprintln!("skipping: curl with HTTP/2 support not available");
        return;
    }
    use std::io::Write;
    let f = start(|_| {});
    // length-prefixed HelloRequest{name: "Bob"}
    let frame = [0u8, 0, 0, 0, 5, 0x0a, 0x03, b'B', b'o', b'b'];
    let out = std::process::Command::new("curl")
        .args(["-sS", "--http2-prior-knowledge", "-H", "content-type: application/grpc", "-H", "te: trailers", "-H", "x-tenant: curl", "--data-binary", "@-", "-o", "-"])
        .arg(format!("http://127.0.0.1:{}/helloworld.Greeter/SayHello", f.port))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin.take().unwrap().write_all(&frame)?;
            c.wait_with_output()
        })
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let body = out.stdout;
    assert_eq!(body[0], 0, "uncompressed message");
    let len = u32::from_be_bytes([body[1], body[2], body[3], body[4]]) as usize;
    assert_eq!(body.len(), 5 + len);
    let pool = DescriptorPool::from_file(PROTO).unwrap();
    let reply = pool.decode("helloworld.HelloReply", &body[5..]).unwrap();
    assert_eq!(reply.get("message"), Some(&Value::from("Hello Bob from curl")));
    f.ctx.stop().unwrap();
}
