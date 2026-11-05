//! Official-client cross-verification of Cameleer's gRPC stack (HTTP/2, HPACK, protobuf,
//! TLS/ALPN) with grpcio, the gRPC C-core based Python implementation: grpcio calls a
//! Cameleer `grpc` consumer route (unary with metadata, server/client/bidi streaming, status
//! codes, deadlines, large messages; plaintext, TLS, mutual TLS, gzip), and the Cameleer
//! `grpc` producer calls a grpcio server (plaintext, gzip, TLS).
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_tls::testing::official::{self, python_script, Run};

const PROTO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources/greeter.proto");
const PROTO_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/resources");

fn greeter(ex: &mut Exchange) -> Result<()> {
    let method = ex.header("CamelGrpcMethodName").map(|v| v.to_string()).unwrap_or_default();
    let name_of = |v: &Value| v.get("name").map(|n| n.to_string()).unwrap_or_default();
    match method.as_str() {
        "sayHello" => {
            let name = name_of(ex.body());
            match name.as_str() {
                "missing" => return Err(CamelError::new("StatusRuntimeException", "NOT_FOUND: no such person")),
                "slow" => std::thread::sleep(Duration::from_millis(1500)),
                _ => {}
            }
            let tenant = ex.header("x-tenant").map(|v| v.to_string()).unwrap_or_default();
            let mood = ex.body().get("mood").map(|v| v.to_string()).unwrap_or_default();
            let msg = if name.len() > 1000 { name } else { format!("Hello {} from {} ({})", name, tenant, mood) };
            ex.set_body(Value::object([("message", Value::from(msg)), ("index", Value::Int(7))]));
        }
        "sayHelloStream" => {
            let name = name_of(ex.body());
            let times = ex.body().get("times").and_then(|v| v.as_i64()).unwrap_or(1);
            ex.set_body(Value::Array((0..times).map(|i| Value::object([("message", Value::from(format!("{}{}", name, i))), ("index", Value::Int(i))])).collect()));
        }
        "sayHelloToAll" | "chat" => {
            let names: Vec<String> = ex.body().as_array().map(|a| a.iter().map(name_of).collect()).unwrap_or_default();
            if method == "chat" {
                ex.set_body(Value::Array(names.iter().map(|n| Value::object([("message", Value::from(format!("chat {}", n)))])).collect()));
            } else {
                ex.set_body(Value::object([("message", Value::from(names.join("+"))), ("index", Value::Int(names.len() as i64))]));
            }
        }
        other => return Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("method {} not implemented", other))),
    }
    Ok(())
}

fn consumer(options: &str) -> (CamelContext, u16) {
    let ctx = CamelContext::new();
    cameleer_grpc::register(&ctx);
    let uri = format!("grpc://localhost:0/helloworld.Greeter?protoFile={}{}", PROTO, options);
    let u2 = uri.clone();
    ctx.add_routes(move |b| {
        b.from(&u2).process(greeter);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_grpc::local_port(&ctx, &uri).expect("consumer port");
    (ctx, port)
}

fn grpcio_client(port: u16, flags: &[&str]) -> Run {
    let target = format!("localhost:{}", port);
    let mut args = vec!["client", target.as_str(), PROTO_DIR];
    args.extend_from_slice(flags);
    python_script("grpc_python.py", &args, &[])
}

fn assert_client_calls(r: &Run) {
    let expect = [
        ("unary:", "Hello grpcio from python (HAPPY)|7"),
        ("server-stream:", "S0/0,S1/1,S2/2"),
        ("client-stream:", "a+b+c|3"),
        ("bidi:", "chat x,chat y"),
        ("error:", "NOT_FOUND|no such person"),
        ("deadline:", "DEADLINE_EXCEEDED"),
        ("large:", "300000"),
    ];
    for (k, v) in expect {
        assert_eq!(r.value(k), v, "{}\n{}\n{}", k, r.stdout, r.stderr);
    }
}

#[test]
fn grpcio_client_against_camel_consumer() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("grpc-consumer");
    let (ca, cert, key) = (pem.ca.display().to_string(), pem.server_cert.display().to_string(), pem.server_key.display().to_string());
    // plaintext (h2c with prior knowledge)
    let (ctx, port) = consumer("");
    assert_client_calls(&grpcio_client(port, &[]).ok());
    // gzip-compressed requests; the consumer answers compressed too
    assert_client_calls(&grpcio_client(port, &["--gzip"]).ok());
    ctx.stop().unwrap();
    // TLS (ALPN h2)
    let (ctx, port) = consumer(&format!("&usePlainText=false&keyCertChainResource={}&keyResource={}&compression=gzip", cert, key));
    assert_client_calls(&grpcio_client(port, &["--ca", &ca, "--gzip"]).ok());
    ctx.stop().unwrap();
    // mutual TLS: the trusted client certificate is accepted, an untrusted one is not
    let (ctx, port) = consumer(&format!("&usePlainText=false&keyCertChainResource={}&keyResource={}&trustCertCollectionResource={}", cert, key, ca));
    let client = (pem.client_cert.display().to_string(), pem.client_key.display().to_string());
    assert_client_calls(&grpcio_client(port, &["--ca", &ca, "--cert", &client.0, "--key", &client.1]).ok());
    let bad = (pem.untrusted_cert.display().to_string(), pem.untrusted_key.display().to_string());
    let r = grpcio_client(port, &["--ca", &ca, "--cert", &bad.0, "--key", &bad.1]).ok();
    assert_eq!(r.value("connect-error:"), "UNAVAILABLE", "{}", r.stdout);
    let r = grpcio_client(port, &["--ca", &ca]).ok();
    assert_eq!(r.value("connect-error:"), "UNAVAILABLE", "{}", r.stdout);
    ctx.stop().unwrap();
}

/// A grpcio Greeter server; stopped when dropped (stdin closed).
struct PyServer {
    child: std::process::Child,
    port: u16,
}

impl PyServer {
    fn start(tls: Option<(&str, &str)>) -> PyServer {
        let dir = official::temp_dir("grpcio-server");
        let portfile = dir.join("port");
        let mut cmd = Command::new(official::python());
        cmd.args(["-u", "-I", "-W", "ignore"]).arg(official::script("grpc_python.py")).arg("server").arg(PROTO_DIR).arg(&portfile);
        if let Some((cert, key)) = tls {
            cmd.args([cert, key]);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("python");
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Ok(p) = std::fs::read_to_string(&portfile) {
                return PyServer { child, port: p.trim().parse().unwrap() };
            }
            if let Ok(Some(st)) = child.try_wait() {
                let mut err = String::new();
                child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
                panic!("grpcio server exited ({}): {}", st, err);
            }
            assert!(Instant::now() < deadline, "grpcio server did not start");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

impl Drop for PyServer {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn producer_calls(ctx: &CamelContext, base: &str) {
    let t = ctx.create_producer_template();
    let uri = |m: &str| format!("{}&method={}", base, m);
    let reply = t.request_body_and_header(&uri("sayHello"), Value::object([("name", Value::from("camel")), ("mood", Value::from("GRUMPY"))]), "x-tenant", "rust").unwrap();
    assert_eq!(reply, Value::object([("message", Value::from("Hi camel from grpcio (rust)")), ("index", Value::Int(2))]));
    let reply = t.request_body(&uri("sayHelloStream"), Value::object([("name", Value::from("s")), ("times", Value::Int(3))])).unwrap();
    let msgs: Vec<String> = reply.as_array().unwrap().iter().map(|r| r.get("message").unwrap().to_string()).collect();
    assert_eq!(msgs, vec!["s #0", "s #1", "s #2"]);
    let reqs = Value::Array(["a", "b"].iter().map(|n| Value::object([("name", Value::from(*n))])).collect());
    let reply = t.request_body(&uri("sayHelloToAll"), reqs.clone()).unwrap();
    assert_eq!(reply.get("message"), Some(&Value::from("all: a,b")));
    let reply = t.request_body(&uri("chat"), reqs).unwrap();
    let msgs: Vec<String> = reply.as_array().unwrap().iter().map(|r| r.get("message").unwrap().to_string()).collect();
    assert_eq!(msgs, vec!["chat a", "chat b"]);
    let err = t.request_body(&uri("sayHello"), Value::object([("name", "fail")])).unwrap_err();
    let status = err.chain().find(|c| c.kind == "StatusRuntimeException").map(|c| c.message.clone()).unwrap_or_default();
    assert_eq!(status, "PERMISSION_DENIED: no access for fail");
    let long = "y".repeat(200_000);
    let reply = t.request_body(&uri("sayHello"), Value::object([("name", Value::from(long.clone()))])).unwrap();
    assert!(reply.get("message").unwrap().to_string().contains(&long));
}

#[test]
fn camel_producer_against_grpcio_server() {
    if !official::enabled() {
        return;
    }
    let ctx = CamelContext::new();
    cameleer_grpc::register(&ctx);
    ctx.start().unwrap();
    let plain = PyServer::start(None);
    let base = format!("grpc://localhost:{}/helloworld.Greeter?protoFile={}", plain.port, PROTO);
    producer_calls(&ctx, &base);
    producer_calls(&ctx, &format!("{}&compression=gzip", base));
    drop(plain);
    let pem = official::pem_files("grpcio-server-tls");
    let (cert, key) = (pem.server_cert.display().to_string(), pem.server_key.display().to_string());
    let tls = PyServer::start(Some((&cert, &key)));
    let base = format!("grpc://localhost:{}/helloworld.Greeter?protoFile={}&usePlainText=false&trustCertCollectionResource={}", tls.port, PROTO, pem.ca.display());
    producer_calls(&ctx, &base);
    // an untrusted server certificate is refused
    let other = cameleer_tls::self_signed("localhost");
    let wrong = pem.dir.join("other.pem");
    std::fs::write(&wrong, &other.cert_pem).unwrap();
    let err = ctx
        .create_producer_template()
        .request_body(&format!("grpc://localhost:{}/helloworld.Greeter?protoFile={}&usePlainText=false&trustCertCollectionResource={}&method=sayHello&deadline=5s", tls.port, PROTO, wrong.display()), Value::object([("name", "x")]))
        .unwrap_err();
    assert!(err.chain().any(|c| c.message.contains("UNAVAILABLE") || c.message.to_lowercase().contains("certificate")), "{}", err);
    ctx.stop().unwrap();
}
