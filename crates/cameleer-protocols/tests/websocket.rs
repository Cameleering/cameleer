use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use cameleer_core::prelude::*;
use cameleer_protocols::websocket::protocol::{encode_frame, opcode, read_frame};
use cameleer_protocols::websocket::{WebSocketEndpoint, WsConnection, WsMessage};

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    ctx
}

fn wait_until(f: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    false
}

const ECHO: &str = "websocket://127.0.0.1:0/echo";

fn echo_server() -> (CamelContext, u16) {
    let ctx = context();
    ctx.add_routes(|b| {
        b.from(ECHO).to(ECHO);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, ECHO).expect("bound port");
    (ctx, port)
}

fn raw_handshake(port: u16, path: &str, key: Option<&str>) -> (TcpStream, BufReader<TcpStream>, String) {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut req = format!("GET {} HTTP/1.1\r\nHost: localhost\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\n", path);
    if let Some(k) = key {
        req.push_str(&format!("Sec-WebSocket-Key: {}\r\n", k));
    }
    req.push_str("\r\n");
    (&s).write_all(req.as_bytes()).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut head = String::new();
    loop {
        let mut l = String::new();
        if r.read_line(&mut l).unwrap() == 0 || l == "\r\n" {
            break;
        }
        head.push_str(&l);
    }
    (s, r, head)
}

#[test]
fn raw_handshake_and_masked_frames() {
    let (ctx, port) = echo_server();
    let (s, mut r, head) = raw_handshake(port, "/echo", Some("dGhlIHNhbXBsZSBub25jZQ=="));
    assert!(head.starts_with("HTTP/1.1 101"), "{}", head);
    assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"), "{}", head);
    assert!(head.to_ascii_lowercase().contains("upgrade: websocket"));
    // RFC 6455 5.7: a masked "Hello" from the client
    (&s).write_all(&[0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58]).unwrap();
    let mut reply = [0u8; 7];
    r.read_exact(&mut reply).unwrap();
    assert_eq!(reply, [0x81, 0x05, 0x48, 0x65, 0x6c, 0x6c, 0x6f]);
    // ping -> pong with the same payload
    (&s).write_all(&encode_frame(true, opcode::PING, b"hb", Some([1, 2, 3, 4]))).unwrap();
    let f = read_frame(&mut r, 1000).unwrap();
    assert_eq!((f.opcode, f.payload.as_slice(), f.masked), (opcode::PONG, &b"hb"[..], false));
    // fragmented message: "Hel" + "lo"
    (&s).write_all(&encode_frame(false, opcode::TEXT, b"Hel", Some([9, 8, 7, 6]))).unwrap();
    (&s).write_all(&encode_frame(true, opcode::CONTINUATION, b"lo", Some([5, 4, 3, 2]))).unwrap();
    let f = read_frame(&mut r, 1000).unwrap();
    assert_eq!((f.opcode, f.payload.as_slice()), (opcode::TEXT, &b"Hello"[..]));
    // an unmasked client frame is a protocol error: the server closes with 1002
    (&s).write_all(&encode_frame(true, opcode::TEXT, b"x", None)).unwrap();
    let f = read_frame(&mut r, 1000).unwrap();
    assert_eq!(f.opcode, opcode::CLOSE);
    assert_eq!(&f.payload[..2], &1002u16.to_be_bytes());
    drop(s);

    // unknown path and bad requests are rejected
    let (_s, _r, head) = raw_handshake(port, "/nope", Some("dGhlIHNhbXBsZSBub25jZQ=="));
    assert!(head.starts_with("HTTP/1.1 404"), "{}", head);
    let (_s, _r, head) = raw_handshake(port, "/echo", None);
    assert!(head.starts_with("HTTP/1.1 400"), "{}", head);
    ctx.stop().unwrap();
}

#[test]
fn echo_with_client_large_fragmented_and_binary() {
    let (ctx, port) = echo_server();
    let c = WsConnection::connect("127.0.0.1", port, "/echo", Duration::from_secs(5)).unwrap();
    c.send_text("hello websocket").unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("hello websocket".into())));
    c.send_binary(&[0, 1, 2, 254, 255]).unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Binary(vec![0, 1, 2, 254, 255])));
    // 1 MiB text sent in 16 KiB fragments, echoed back (the server fragments at 64 KiB)
    let big: String = (0..1024 * 1024).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    c.send_fragmented(&WsMessage::Text(big.clone()), 16 * 1024).unwrap();
    match c.read_message().unwrap() {
        Some(WsMessage::Text(t)) => assert!(t == big, "large message differs (len {})", t.len()),
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    c.ping(b"x").unwrap();
    c.send_text("after ping").unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("after ping".into())));
    // closing handshake
    c.close(1000, "bye");
    assert_eq!(c.read_message().unwrap(), None);
    ctx.stop().unwrap();
}

#[test]
fn camel_ws_client_endpoints() {
    let (ctx, port) = echo_server();
    let client_uri = format!("ws://127.0.0.1:{}/echo", port);
    ctx.add_routes(|b| {
        b.from(&client_uri).to("mock:client");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:client").unwrap();
    mock.expected_bodies_received(vec![Value::from("via camel client"), Value::Bytes(vec![1, 2, 3])]);
    let t = ctx.create_producer_template();
    t.send_body(&client_uri, "via camel client").unwrap();
    t.send_body(&client_uri, Value::Bytes(vec![1, 2, 3])).unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(mock.exchange(0).unwrap().header("CamelWebsocketConnectionKey").is_some());
    ctx.stop().unwrap();
}

#[test]
fn send_to_all_and_connection_keys() {
    let ctx = context();
    let news = "websocket://127.0.0.1:0/news?sendToAll=true";
    let inbox = "websocket://127.0.0.1:0/inbox";
    ctx.add_routes(|b| {
        b.from("direct:news").to(news);
        b.from(inbox).to("mock:inbox");
        b.from("direct:reply").to("websocket://127.0.0.1:0/inbox");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, news).expect("producer started the server");
    // consumers on the same host:port share one listener
    assert_eq!(cameleer_protocols::local_port(&ctx, inbox), Some(port));
    let a = WsConnection::connect("127.0.0.1", port, "/news", Duration::from_secs(5)).unwrap();
    let b = WsConnection::connect("127.0.0.1", port, "/news", Duration::from_secs(5)).unwrap();
    let ep = ctx.get_endpoint(news).unwrap();
    let ep = ep.as_any().downcast_ref::<WebSocketEndpoint>().unwrap();
    assert!(wait_until(|| ep.connection_keys().len() == 2));
    let t = ctx.create_producer_template();
    t.send_body("direct:news", "breaking").unwrap();
    assert_eq!(a.read_message().unwrap(), Some(WsMessage::Text("breaking".into())));
    assert_eq!(b.read_message().unwrap(), Some(WsMessage::Text("breaking".into())));

    // reply to a single connection by key
    let mock = ctx.mock_endpoint("mock:inbox").unwrap();
    mock.expected_message_count(1);
    let c = WsConnection::connect("127.0.0.1", port, "/inbox", Duration::from_secs(5)).unwrap();
    c.send_text("hi server").unwrap();
    mock.assert_is_satisfied().unwrap();
    let key = mock.exchange(0).unwrap().header("CamelWebsocketConnectionKey").unwrap().to_string();
    t.send_body_and_header("direct:reply", "just for you", "CamelWebsocketConnectionKey", key.as_str()).unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("just for you".into())));
    // no key and no sendToAll is an error
    let err = t.send_body("direct:reply", "nobody").unwrap_err();
    assert!(err.to_string().contains("connection key not set"), "{}", err);
    ctx.stop().unwrap();
    // stopping closes the connections (close frame 1001)
    assert_eq!(a.read_message().unwrap_or(None), None);
}

#[test]
fn vertx_websocket_events_and_consume_as_client() {
    let ctx = context();
    let chat = "vertx-websocket:127.0.0.1:0/chat?fireWebSocketConnectionEvents=true";
    ctx.add_routes(|b| {
        b.from(chat).to("mock:chat");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, chat).unwrap();
    let mock = ctx.mock_endpoint("mock:chat").unwrap();
    mock.expected_header_values_received_in_any_order("CamelVertxWebsocket.event", vec!["OPEN", "MESSAGE"]);
    let c = WsConnection::connect("127.0.0.1", port, "/chat", Duration::from_secs(5)).unwrap();
    c.send_text("hello vertx").unwrap();
    mock.assert_is_satisfied().unwrap();
    let msg = mock.received_exchanges().into_iter().find(|e| e.header("CamelVertxWebsocket.event").map(|v| v.to_string()) == Some("MESSAGE".into())).unwrap();
    assert_eq!(msg.body(), &Value::from("hello vertx"));
    let key = msg.header("CamelVertxWebsocket.connectionKey").unwrap().to_string();
    // the producer on the same uri sends to server-side peers
    let t = ctx.create_producer_template();
    t.send_body_and_header("vertx-websocket:127.0.0.1:0/chat", "welcome", "CamelVertxWebsocket.connectionKey", key.as_str()).unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("welcome".into())));

    // consumeAsClient against an echo server in another context
    let (echo_ctx, echo_port) = echo_server();
    let client = format!("vertx-websocket:127.0.0.1:{}/echo?consumeAsClient=true", echo_port);
    ctx.add_routes(|b| {
        b.from(&client).to("mock:echoed");
    })
    .unwrap();
    let echoed = ctx.mock_endpoint("mock:echoed").unwrap();
    echoed.expected_bodies_received(vec!["ping from vertx client"]);
    // no local server for that uri: the producer uses the client connection
    t.send_body(&client, "ping from vertx client").unwrap();
    echoed.assert_is_satisfied().unwrap();
    c.close(1000, "");
    ctx.stop().unwrap();
    echo_ctx.stop().unwrap();
    // wss endpoints resolve (TLS end-to-end tests in tests/tls.rs); bad options still fail
    assert!(ctx.get_endpoint("wss://host/x").is_ok());
    assert!(ctx.get_endpoint("wss://host/x?sslContextParameters=#missing").is_err());
}
