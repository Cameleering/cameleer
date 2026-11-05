//! `thrift:` component: consumer + producer over every protocol/transport, declared
//! exceptions, oneway calls, inherited methods and TLS.

use std::sync::{Arc, Mutex};

use cameleer_core::json;
use cameleer_core::prelude::*;
use cameleer_dataformats::thrift_rpc::ThriftEndpoint;

const IDL: &str = r#"
    namespace java org.apache.camel.component.thrift.generated
    enum Operation { ADD = 1, SUBTRACT = 2, MULTIPLY = 3, DIVIDE = 4 }
    struct Work { 1: i32 num1 = 0, 2: i32 num2, 3: Operation op, 4: optional string comment }
    exception InvalidOperation { 1: i32 whatOp, 2: string why }
    const i32 MAX = 100
    service SharedService { string getStruct(1: i32 key) }
    service Calculator extends SharedService {
        void ping(),
        i32 add(1:i32 num1, 2:i32 num2),
        i32 calculate(1:i32 logid, 2:Work w) throws (1:InvalidOperation ouch),
        list<Work> echoAll(1: list<Work> works),
        oneway void zip()
    }
"#;

fn port(ctx: &CamelContext, uri: &str) -> u16 {
    let ep = ctx.get_endpoint(uri).unwrap();
    ep.as_any().downcast_ref::<ThriftEndpoint>().unwrap().local_port().unwrap()
}

fn calculator(ex: &mut Exchange, zips: &Arc<Mutex<u32>>) -> Result<()> {
    let m = ex.header("CamelThriftMethodName").unwrap().to_string();
    let args = ex.body().as_array().unwrap().clone();
    let out = match m.as_str() {
        "ping" => Value::Null,
        "zip" => {
            *zips.lock().unwrap() += 1;
            Value::Null
        }
        "getStruct" => Value::String(format!("key-{}", args[0])),
        "add" => Value::Int(args[0].as_i64().unwrap() + args[1].as_i64().unwrap()),
        "echoAll" => args[0].clone(),
        "calculate" => {
            let w = &args[1];
            let (a, b) = (w.get("num1").unwrap().as_i64().unwrap(), w.get("num2").unwrap().as_i64().unwrap());
            match w.get("op").unwrap().as_str().unwrap() {
                "ADD" => Value::Int(a + b),
                "SUBTRACT" => Value::Int(a - b),
                "MULTIPLY" => Value::Int(a * b),
                _ if b == 0 => return Err(CamelError::new("InvalidOperation", "Cannot divide by 0")),
                _ => json::parse(r#"{"@exception":"InvalidOperation","whatOp":4,"why":"division unsupported"}"#).unwrap(),
            }
        }
        other => return Err(CamelError::illegal_state(format!("unexpected {}", other))),
    };
    ex.set_body(out);
    Ok(())
}

fn run(protocol: &str, transport: &str) {
    run_with(protocol, transport, "")
}

fn run_with(protocol: &str, transport: &str, extra: &str) {
    let ctx = CamelContext::new();
    cameleer_dataformats::register(&ctx);
    let dir = std::env::temp_dir().join(format!("thrift-rpc-{}-{}-{}-{}", protocol, transport, extra.len(), std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let idl = dir.join("calc.thrift");
    std::fs::write(&idl, IDL).unwrap();
    let server = format!("thrift://127.0.0.1:0/org.apache.camel.component.thrift.generated.Calculator?idlFile={}&exchangeProtocol={}{}", idl.display(), protocol, extra);
    let zips = Arc::new(Mutex::new(0u32));
    let z = zips.clone();
    let s2 = server.clone();
    ctx.add_routes(move |b| {
        let z = z.clone();
        b.from(&s2).process(move |ex: &mut Exchange| calculator(ex, &z));
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx, &server);
    let client = |method: &str| format!("thrift://localhost:{}/Calculator?method={}&idlFile={}&exchangeProtocol={}&transport={}{}", p, method, idl.display(), protocol, transport, extra);
    let t = ctx.create_producer_template();

    let out = t.request_body(&client("add"), json::parse("[2, 40]").unwrap()).unwrap();
    assert_eq!(out, Value::Int(42), "{} {}", protocol, transport);
    // named arguments, header override, inherited method
    let out = t.request_body_and_header(&client("add"), json::parse(r#"{"num1":5,"num2":6}"#).unwrap(), "CamelThriftMethodName", "add").unwrap();
    assert_eq!(out, Value::Int(11));
    assert_eq!(t.request_body(&client("getStruct"), 7).unwrap().to_string(), "key-7");
    assert_eq!(t.request_body(&client("ping"), Value::Null).unwrap(), Value::Null);
    let w = json::parse(r#"[1, {"num1":6,"num2":7,"op":"MULTIPLY"}]"#).unwrap();
    assert_eq!(t.request_body(&client("calculate"), w).unwrap(), Value::Int(42));
    let works = json::parse(r#"[[{"num1":1,"num2":2,"op":"ADD","comment":"c"},{"num1":3,"num2":4,"op":"DIVIDE"}]]"#).unwrap();
    let out = t.request_body(&client("echoAll"), works.clone()).unwrap();
    assert_eq!(out, works.as_array().unwrap()[0]);

    // declared exception (from an exception kind and from an @exception body)
    let ex = t.send_with(&client("calculate"), ExchangePattern::InOut, |ex| ex.set_body(json::parse(r#"[1, {"num1":1,"num2":0,"op":"DIVIDE"}]"#).unwrap()));
    let e = ex.exception.clone().expect("exception");
    assert_eq!(e.kind, "InvalidOperation");
    assert_eq!(e.message, "Cannot divide by 0");
    let ex = t.send_with(&client("calculate"), ExchangePattern::InOut, |ex| ex.set_body(json::parse(r#"[1, {"num1":1,"num2":3,"op":"DIVIDE"}]"#).unwrap()));
    assert_eq!(ex.exception.as_ref().unwrap().message, "division unsupported");
    assert_eq!(ex.header("CamelThriftException").unwrap().get("whatOp"), Some(&Value::Int(4)));

    // oneway
    t.request_body(&client("zip"), Value::Null).unwrap();
    let out = t.request_body(&client("add"), json::parse("[1, 1]").unwrap()).unwrap();
    assert_eq!(out, Value::Int(2));
    // oneway calls are not acknowledged: wait for the server to process it
    for _ in 0..200 {
        if *zips.lock().unwrap() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(*zips.lock().unwrap(), 1);
    ctx.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn binary_framed_and_buffered() {
    run("BINARY", "FRAMED");
    run("BINARY", "BUFFERED");
}

#[test]
fn compact_and_json() {
    run("COMPACT", "FRAMED");
    run("COMPACT", "BUFFERED");
    run("JSON", "FRAMED");
    run("JSON", "BUFFERED");
}

#[test]
fn zlib_transport() {
    run_with("BINARY", "BUFFERED", "&compressionType=ZLIB");
    run_with("COMPACT", "FRAMED", "&compressionType=ZLIB");
}

#[test]
fn internal_errors_and_unknown_methods() {
    let ctx = CamelContext::new();
    cameleer_dataformats::register(&ctx);
    let server = format!("thrift://127.0.0.1:0/Calculator?schema={}", cameleer_core::util::url_encode(IDL));
    ctx.add_routes(|b| {
        b.from(&server).process(|_ex: &mut Exchange| Err(CamelError::illegal_state("boom")));
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx, &server);
    let t = ctx.create_producer_template();
    let client = format!("thrift://127.0.0.1:{}/Calculator?method=add&schema={}", p, cameleer_core::util::url_encode(IDL));
    let ex = t.send_with(&client, ExchangePattern::InOut, |ex| ex.set_body(json::parse("[1,2]").unwrap()));
    let e = ex.exception.unwrap();
    assert_eq!(e.kind, "TApplicationException");
    assert!(e.message.contains("boom"), "{}", e.message);
    // a client with a newer IDL calling a method the server does not know
    let newer = IDL.replace("oneway void zip()", "oneway void zip(), i32 sub(1:i32 a, 2:i32 b)");
    let other = CamelContext::new();
    cameleer_dataformats::register(&other);
    let t2 = other.create_producer_template();
    let ex = t2.send_with(&format!("thrift://127.0.0.1:{}/Calculator?method=sub&schema={}", p, cameleer_core::util::url_encode(&newer)), ExchangePattern::InOut, |ex| ex.set_body(json::parse("[1,2]").unwrap()));
    assert!(ex.exception.unwrap().message.contains("Invalid method name: 'sub'"));
    // unknown method / service at endpoint creation
    assert!(ctx.get_endpoint(&format!("thrift://127.0.0.1:1/Calculator?method=nope&schema={}", cameleer_core::util::url_encode(IDL))).is_err());
    assert!(ctx.get_endpoint(&format!("thrift://127.0.0.1:1/Nope?schema={}", cameleer_core::util::url_encode(IDL))).is_err());
    ctx.stop().unwrap();
}

#[test]
fn tls() {
    let ctx = CamelContext::new();
    cameleer_dataformats::register(&ctx);
    let id = cameleer_tls::certgen::self_signed("localhost");
    ctx.registry().bind("ssl", cameleer_tls::SslContextParameters::new().key_pair_pem(&id.cert_pem, &id.key_pem).trust_pem(&id.cert_pem));
    let enc = cameleer_core::util::url_encode(IDL);
    let server = format!("thrift://localhost:0/Calculator?schema={}&negotiationType=SSL&sslContextParameters=#ssl", enc);
    ctx.add_routes(|b| {
        b.from(&server).process(|ex: &mut Exchange| {
            let a = ex.body().as_array().unwrap().clone();
            ex.set_body(Value::Int(a[0].as_i64().unwrap() + a[1].as_i64().unwrap()));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = port(&ctx, &server);
    let t = ctx.create_producer_template();
    let out = t.request_body(&format!("thrift://localhost:{}/Calculator?method=add&schema={}&negotiationType=SSL&sslContextParameters=#ssl", p, enc), json::parse("[20,22]").unwrap()).unwrap();
    assert_eq!(out, Value::Int(42));
    ctx.stop().unwrap();
}
