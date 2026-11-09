//! HTTP/2 client <-> server integration tests.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use cameleer_grpc::h2::frame::{flags, frame_type, Frame, PREFACE};
use cameleer_grpc::h2::{ErrorCode, H2Config, H2Connection, H2Error, H2Request, H2Server, H2Stream, ServerRequest};

const T: Option<Duration> = Some(Duration::from_secs(20));

fn echo_server(config: H2Config, tls: Option<cameleer_tls::TlsServerConfig>) -> H2Server {
    H2Server::bind("127.0.0.1:0", config, tls, |req: ServerRequest, stream: H2Stream| {
        let body = stream.read_body(T).unwrap_or_default();
        let mut out = format!("{} {} ", req.method, req.path).into_bytes();
        out.extend_from_slice(&body);
        let h = vec![(":status".to_string(), "200".to_string()), ("x-len".to_string(), body.len().to_string())];
        if stream.send_headers(&h, false).is_ok() && stream.send_data(&out, false).is_ok() {
            let _ = stream.send_trailers(&[("x-trailer".to_string(), "done".to_string())]);
        }
    })
    .unwrap()
}

#[test]
fn simple_request_response_with_trailers() {
    let server = echo_server(H2Config::default(), None);
    let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::default()).unwrap();
    let resp = conn.request(&H2Request::post("/echo", b"hello".to_vec()).header("X-Custom", "1"), T).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, b"POST /echo hello");
    assert_eq!(resp.header("x-len"), Some("5"));
    assert_eq!(resp.trailers, vec![("x-trailer".to_string(), "done".to_string())]);
    let resp = conn.request(&H2Request::get("/x"), T).unwrap();
    assert_eq!(resp.body, b"GET /x ");
    assert!(conn.ping(Duration::from_secs(5)).is_ok());
    conn.close();
    server.stop();
}

#[test]
fn concurrent_streams_with_large_bodies_and_flow_control() {
    // protocol default windows (64 KiB) on both sides: every body needs many WINDOW_UPDATEs
    let server = echo_server(H2Config::protocol_defaults(), None);
    let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::protocol_defaults()).unwrap();
    let mut handles = Vec::new();
    for i in 0..12usize {
        let conn = conn.clone();
        handles.push(std::thread::spawn(move || {
            let size = 300_000 + i * 7919;
            let body: Vec<u8> = (0..size).map(|j| ((i * 31 + j) % 251) as u8).collect();
            let resp = conn.request(&H2Request::post(&format!("/big/{}", i), body.clone()), T).unwrap();
            assert_eq!(resp.status, 200);
            let prefix = format!("POST /big/{} ", i);
            assert_eq!(&resp.body[..prefix.len()], prefix.as_bytes());
            assert!(resp.body[prefix.len()..] == body[..], "body {} mismatch", i);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(conn.active_streams(), 0);
    server.stop();
}

#[test]
fn max_concurrent_streams_is_respected() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (a, p) = (active.clone(), peak.clone());
    let config = H2Config { max_concurrent_streams: 2, ..H2Config::default() };
    let server = H2Server::bind("127.0.0.1:0", config, None, move |_req: ServerRequest, stream: H2Stream| {
        let now = a.fetch_add(1, Ordering::SeqCst) + 1;
        p.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        let _ = stream.read_body(T);
        a.fetch_sub(1, Ordering::SeqCst);
        let _ = ServerRequest::respond(&stream, 200, &[], b"ok");
    })
    .unwrap();
    let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::default()).unwrap();
    // wait for the server SETTINGS
    conn.ping(Duration::from_secs(5)).unwrap();
    assert_eq!(conn.peer_max_concurrent_streams(), 2);
    let handles: Vec<_> = (0..6)
        .map(|_| {
            let c = conn.clone();
            std::thread::spawn(move || c.request(&H2Request::get("/"), T).unwrap().status)
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 200);
    }
    assert!(peak.load(Ordering::SeqCst) <= 2, "peak {}", peak.load(Ordering::SeqCst));
    server.stop();
}

#[test]
fn stream_reset_and_streaming_responses() {
    let server = H2Server::bind("127.0.0.1:0", H2Config::default(), None, |req: ServerRequest, stream: H2Stream| {
        if req.path == "/reset" {
            stream.reset(ErrorCode::REFUSED_STREAM);
            return;
        }
        stream.send_headers(&[(":status".into(), "200".into())], false).unwrap();
        for i in 0..5 {
            stream.send_data(format!("chunk{};", i).as_bytes(), false).unwrap();
        }
        stream.send_data(b"", true).unwrap();
    })
    .unwrap();
    let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::default()).unwrap();
    let err = conn.request(&H2Request::get("/reset"), T).unwrap_err();
    assert_eq!(err, H2Error::StreamReset(ErrorCode::REFUSED_STREAM));
    let headers = conn.request_headers(&H2Request::get("/stream"));
    let s = conn.open_stream(&headers, true).unwrap();
    assert!(s.recv_headers(T).unwrap().unwrap().contains(&(":status".to_string(), "200".to_string())));
    let mut chunks = Vec::new();
    while let Some(c) = s.recv_data(T).unwrap() {
        chunks.push(String::from_utf8(c).unwrap());
    }
    assert_eq!(chunks.concat(), "chunk0;chunk1;chunk2;chunk3;chunk4;");
    // connection still usable after a reset
    assert!(conn.ping(Duration::from_secs(5)).is_ok());
    server.stop();
}

#[test]
fn a_complete_response_survives_rst_stream_no_error() {
    // RFC 9113 section 8.1: a server may answer before the request is complete and then send
    // RST_STREAM(NO_ERROR); the client must not discard the response. Our server does exactly
    // that when its handler returns before the client ended its side.
    let server = H2Server::bind("127.0.0.1:0", H2Config::default(), None, |_req: ServerRequest, stream: H2Stream| {
        stream.send_headers(&[(":status".into(), "200".into())], false).unwrap();
        stream.send_data(b"early answer", true).unwrap();
    })
    .unwrap();
    let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::default()).unwrap();
    let headers = conn.request_headers(&H2Request::post("/early", Vec::new()));
    let s = conn.open_stream(&headers, false).unwrap();
    // wait until the server's response and its RST_STREAM(NO_ERROR) have arrived
    let until = std::time::Instant::now() + Duration::from_secs(20);
    while s.reset_code() != Some(ErrorCode::NO_ERROR) {
        assert!(std::time::Instant::now() < until, "no RST_STREAM(NO_ERROR) from the server");
        std::thread::sleep(Duration::from_millis(5));
    }
    // the rest of the request is dropped without an error
    s.send_data(b"late request body", false).unwrap();
    s.send_data(b"", true).unwrap();
    // and the complete response is still there
    assert!(s.recv_headers(T).unwrap().unwrap().contains(&(":status".to_string(), "200".to_string())));
    assert_eq!(s.read_body(T).unwrap(), b"early answer");
    assert_eq!(s.recv_headers(T).unwrap(), None);
    // a real reset is still an error
    assert!(conn.ping(Duration::from_secs(5)).is_ok());
    server.stop();
}

#[test]
fn goaway_on_server_stop_and_timeout() {
    let server = H2Server::bind("127.0.0.1:0", H2Config::default(), None, |_req: ServerRequest, stream: H2Stream| {
        std::thread::sleep(Duration::from_millis(300));
        let _ = ServerRequest::respond(&stream, 204, &[], b"");
    })
    .unwrap();
    let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::default()).unwrap();
    let err = conn.request(&H2Request::get("/slow"), Some(Duration::from_millis(50))).unwrap_err();
    assert_eq!(err, H2Error::Timeout);
    // in-flight request completes during graceful stop
    let c2 = conn.clone();
    let h = std::thread::spawn(move || c2.request(&H2Request::get("/slow"), T));
    std::thread::sleep(Duration::from_millis(50));
    server.stop_graceful(Duration::from_secs(5));
    assert_eq!(h.join().unwrap().unwrap().status, 204);
    std::thread::sleep(Duration::from_millis(50));
    assert!(conn.is_closed());
    assert!(conn.request(&H2Request::get("/"), T).is_err());
}

#[test]
fn protocol_errors_from_raw_client() {
    let server = echo_server(H2Config::default(), None);
    // bad preface: connection is dropped
    let mut s = TcpStream::connect(server.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    let _ = s.read_to_end(&mut buf);
    // an HTTP/1.1 request without Upgrade: h2c is refused (505) and the connection dropped
    assert!(buf.starts_with(b"HTTP/1.1 505"), "{}", String::from_utf8_lossy(&buf));

    // valid preface, then a DATA frame on stream 0 -> GOAWAY(PROTOCOL_ERROR)
    let mut s = TcpStream::connect(server.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = PREFACE.to_vec();
    Frame::settings(&[]).encode(&mut out);
    Frame::data(0, b"x", false).encode(&mut out);
    s.write_all(&out).unwrap();
    let mut goaway = None;
    while let Ok(Some(f)) = Frame::read_from(&mut s, 1 << 20) {
        if f.typ == frame_type::GOAWAY {
            goaway = Some(f);
            break;
        }
    }
    let g = goaway.expect("GOAWAY");
    assert_eq!(g.u32_at(4), Some(ErrorCode::PROTOCOL_ERROR.0));

    // WINDOW_UPDATE overflow on the connection -> FLOW_CONTROL_ERROR
    let mut s = TcpStream::connect(server.local_addr()).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut out = PREFACE.to_vec();
    Frame::settings(&[]).encode(&mut out);
    Frame::window_update(0, 0x7fff_ffff).encode(&mut out);
    s.write_all(&out).unwrap();
    let mut code = None;
    let mut acked = false;
    while let Ok(Some(f)) = Frame::read_from(&mut s, 1 << 20) {
        if f.typ == frame_type::SETTINGS && f.has(flags::ACK) {
            acked = true;
        }
        if f.typ == frame_type::GOAWAY {
            code = f.u32_at(4);
            break;
        }
    }
    assert!(acked, "server acknowledged our SETTINGS");
    assert_eq!(code, Some(ErrorCode::FLOW_CONTROL_ERROR.0));
    server.stop();
}

#[test]
fn tls_alpn_h2() {
    let ss = cameleer_tls::self_signed("localhost");
    let scfg = cameleer_tls::TlsServerConfig::builder()
        .cert_chain_pem(ss.cert_pem.as_bytes())
        .private_key_pem(ss.key_pem.as_bytes())
        .alpn_protocols(&["h2"])
        .build()
        .unwrap();
    let server = echo_server(H2Config::default(), Some(scfg));
    let ccfg = cameleer_tls::TlsClientConfig::builder().system_roots(false).add_root_pem(ss.cert_pem.as_bytes()).unwrap().alpn_protocols(&["h2"]).build();
    let conn = H2Connection::connect_tls(&server.local_addr().to_string(), "localhost", &ccfg, H2Config::default()).unwrap();
    assert!(conn.is_tls());
    assert_eq!(conn.alpn_protocol(), Some(&b"h2"[..]));
    let body = vec![7u8; 200_000];
    let resp = conn.request(&H2Request::post("/tls", body.clone()), T).unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(&resp.body[b"POST /tls ".len()..], &body[..]);
    // concurrent streams over TLS
    let hs: Vec<_> = (0..4)
        .map(|i| {
            let c = conn.clone();
            std::thread::spawn(move || c.request(&H2Request::post("/c", vec![i as u8; 50_000]), T).unwrap().body.len())
        })
        .collect();
    for h in hs {
        assert_eq!(h.join().unwrap(), "POST /c ".len() + 50_000);
    }
    server.stop();
}

fn curl_supports_http2() -> bool {
    std::process::Command::new("curl")
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("HTTP2"))
        .unwrap_or(false)
}

#[test]
fn curl_interop_h2c_prior_knowledge() {
    if !curl_supports_http2() {
        eprintln!("skipping: curl with HTTP/2 support not available");
        return;
    }
    let server = echo_server(H2Config::default(), None);
    let url = format!("http://{}/curl?x=1", server.local_addr());
    let body: String = "abcdefghij".repeat(20_000); // 200 KB, beyond curl's default windows
    let out = std::process::Command::new("curl")
        .args(["-sS", "--http2-prior-knowledge", "-H", "x-test: yes", "--data-binary", "@-", "-o", "-", "-w", "\n%{http_version} %{http_code}", &url])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            c.stdin.take().unwrap().write_all(body.as_bytes())?;
            c.wait_with_output()
        })
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "curl failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with("POST /curl?x=1 abcdefghij"), "{}", &stdout[..stdout.len().min(100)]);
    assert!(stdout.ends_with("\n2 200"), "{}", &stdout[stdout.len().saturating_sub(50)..]);
    assert_eq!(stdout.len(), "POST /curl?x=1 ".len() + body.len() + "\n2 200".len());
    server.stop();
}

#[test]
fn serve_externally_accepted_transport() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        // e.g. an HTTP/1.1 server that peeked the preface hands the socket over
        let mut pre = [0u8; 24];
        let mut got = 0;
        while got < 24 {
            got = sock.peek(&mut pre).unwrap();
        }
        assert_eq!(&pre, PREFACE);
        let t = cameleer_grpc::h2::Transport::tcp(sock).unwrap();
        cameleer_grpc::h2::serve_transport(t, H2Config::default(), |req: ServerRequest, stream: H2Stream| {
            let _ = ServerRequest::respond(&stream, 200, &[], format!("handed over {}", req.path).as_bytes());
        })
        .unwrap();
    });
    let conn = H2Connection::connect(&addr.to_string(), H2Config::default()).unwrap();
    let r = conn.request(&H2Request::get("/x"), T).unwrap();
    assert_eq!(r.body, b"handed over /x");
}

#[test]
fn the_server_closes_a_connection_the_peer_closed() {
    // A peer that closes its side gets the server's side closed too: the server's writer used
    // to keep the socket (and a thread) forever, which ran a busy server out of descriptors.
    let server = echo_server(H2Config::default(), None);
    for _ in 0..20 {
        let mut s = TcpStream::connect(server.local_addr()).unwrap();
        s.write_all(PREFACE).unwrap();
        s.write_all(&Frame::settings(&[]).to_bytes()).unwrap();
        s.shutdown(std::net::Shutdown::Write).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 4096];
        let closed = loop {
            match s.read(&mut buf) {
                Ok(0) => break true,
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break true,
                Err(_) => break false,
            }
        };
        assert!(closed, "the server kept a connection open after the peer closed it");
    }
    server.stop();
}
