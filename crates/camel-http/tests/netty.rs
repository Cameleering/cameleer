use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, UdpSocket};
use std::time::Duration;

use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_http::register(&ctx);
    ctx
}

#[test]
fn tcp_textline_sync_echo() {
    let ctx = context();
    let uri = "netty:tcp://127.0.0.1:0?textline=true&sync=true";
    ctx.add_routes(|b| {
        b.from(uri).process(|ex: &mut Exchange| {
            let s: String = ex.body_as()?;
            ex.set_body(format!("echo: {}", s));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, uri).expect("bound port");

    // raw client: multiple lines on one connection
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut w = &s;
    w.write_all(b"hello\r\nworld\n").unwrap();
    let mut r = BufReader::new(&s);
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert_eq!(line, "echo: hello\n");
    line.clear();
    r.read_line(&mut line).unwrap();
    assert_eq!(line, "echo: world\n");
    drop(r);
    drop(s);

    // producer with sync request-reply
    let t = ctx.create_producer_template();
    let reply = t.request_body(&format!("netty:tcp://127.0.0.1:{}?textline=true&sync=true&requestTimeout=5000", port), "camel").unwrap();
    assert_eq!(reply, Value::from("echo: camel"));
    ctx.stop().unwrap();
}

#[test]
fn tcp_null_delimiter_raw_and_disconnect() {
    let ctx = context();
    let null_uri = "netty:tcp://127.0.0.1:0?textline=true&delimiter=NULL&disconnect=true";
    let raw_uri = "netty:tcp://127.0.0.1:0?sync=true&allowDefaultCodec=false";
    ctx.add_routes(|b| {
        b.from(null_uri).process(|ex: &mut Exchange| {
            let s: String = ex.body_as()?;
            ex.set_body(s.to_uppercase());
            Ok(())
        });
        b.from(raw_uri).process(|ex: &mut Exchange| {
            let mut bytes = ex.body().to_bytes();
            bytes.reverse();
            ex.set_body(Value::Bytes(bytes));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p_null = camel_http::local_port(&ctx, null_uri).unwrap();
    let p_raw = camel_http::local_port(&ctx, raw_uri).unwrap();

    let mut s = TcpStream::connect(("127.0.0.1", p_null)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"abc\0").unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).unwrap(); // server disconnects after the reply
    assert_eq!(out, b"ABC\0");

    let t = ctx.create_producer_template();
    let reply = t.request_body(&format!("netty:tcp://127.0.0.1:{}?requestTimeout=5000", p_raw), Value::Bytes(vec![1, 2, 3])).unwrap();
    assert_eq!(reply, Value::Bytes(vec![3, 2, 1]));
    ctx.stop().unwrap();
}

#[test]
fn tcp_inonly_producer_and_request_timeout() {
    let ctx = context();
    let uri = "netty:tcp://127.0.0.1:0?textline=true&sync=false";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:received");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:received").unwrap();
    mock.expected_bodies_received(vec!["one", "two"]);
    let t = ctx.create_producer_template();
    let target = format!("netty:tcp://127.0.0.1:{}?textline=true&sync=false", port);
    t.send_body(&target, "one").unwrap();
    t.send_body(&target, "two").unwrap();
    mock.assert_is_satisfied().unwrap();

    // a sync request against a server that never answers times out
    let err = t.request_body(&format!("netty:tcp://127.0.0.1:{}?textline=true&sync=true&requestTimeout=200", port), "three").unwrap_err();
    assert_eq!(err.root_cause().kind, kinds::EXCHANGE_TIMED_OUT);
    ctx.stop().unwrap();
}

#[test]
fn udp_send_and_sync_reply() {
    let ctx = context();
    let uri = "netty:udp://127.0.0.1:0?textline=true&sync=false";
    let sync_uri = "netty:udp://127.0.0.1:0?textline=true&sync=true";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:udp");
        b.from(sync_uri).process(|ex: &mut Exchange| {
            let s: String = ex.body_as()?;
            ex.set_body(format!("pong {}", s));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = camel_http::local_port(&ctx, uri).unwrap();
    let sync_port = camel_http::local_port(&ctx, sync_uri).unwrap();
    let mock = ctx.mock_endpoint("mock:udp").unwrap();
    mock.expected_bodies_received(vec!["datagram-1"]);
    let t = ctx.create_producer_template();
    t.send_body(&format!("netty:udp://127.0.0.1:{}?textline=true&sync=false", port), "datagram-1").unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(mock.received_exchanges()[0].header("CamelNettyRemoteAddress").is_some());

    let reply = t.request_body(&format!("netty:udp://127.0.0.1:{}?textline=true&sync=true&requestTimeout=5000", sync_port), "ping").unwrap();
    assert_eq!(reply, Value::from("pong ping"));

    // raw socket client
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    sock.send_to(b"raw\n", ("127.0.0.1", sync_port)).unwrap();
    let mut buf = [0u8; 64];
    let (n, _) = sock.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"pong raw\n");
    ctx.stop().unwrap();
}

#[test]
fn invalid_netty_uris() {
    let ctx = context();
    assert!(ctx.get_endpoint("netty:sctp://localhost:1").is_err());
    assert!(ctx.get_endpoint("netty:tcp://localhost:1?delimiter=TAB").is_err());
    assert!(ctx.get_endpoint("netty:tcp://localhost:1?bogus=1").is_err());
    assert!(ctx.get_endpoint("netty:tcp://localhost:1?textline=true&encoding=ISO-8859-1").is_ok());
}
