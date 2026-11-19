//! The gRPC server (`GrpcServer`) on simoom: what a handler can rely on, checked with the crate's own
//! client.

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cameleer_grpc::grpc::server::{GrpcServer, ServerCall};
use cameleer_grpc::grpc::{Code, GrpcClient, Status};
use cameleer_grpc::h2::H2Config;

fn serve(f: impl Fn(&ServerCall) -> Status + Send + Sync + 'static) -> GrpcServer {
    GrpcServer::bind("127.0.0.1:0", H2Config::default(), None, 4 << 20, Arc::new(f)).unwrap()
}

fn client(s: &GrpcServer) -> GrpcClient {
    GrpcClient::plaintext(&s.local_addr().to_string())
}

#[test]
fn a_bidirectional_call_is_answered_while_the_request_is_still_open() {
    let s = serve(|call| {
        while let Ok(Some(m)) = call.recv() {
            if call.send(&m).is_err() {
                return Status::new(Code::Cancelled, "gone");
            }
        }
        Status::ok()
    });
    let c = client(&s);
    let mut call = c.start_call("/t.Echo/Say", &vec![], Some(Duration::from_secs(10))).unwrap();
    for word in ["alpha", "beta", "gamma"] {
        call.send(word.as_bytes()).unwrap();
        // the answer comes before the client has half-closed
        assert_eq!(call.recv().unwrap().unwrap(), word.as_bytes());
    }
    call.close_send().unwrap();
    assert!(call.recv().unwrap().is_none());
    assert!(call.status().unwrap().is_ok());
}

#[test]
fn a_client_cancel_reaches_a_handler_that_is_waiting_for_the_next_message() {
    let (tx, rx) = channel();
    let tx = Mutex::new(tx);
    let s = serve(move |call| {
        let first = call.recv();
        let second = call.recv();
        let _ = tx.lock().unwrap().send((first.map(|m| m.is_some()), second.map(|m| m.is_some()).unwrap_err().code, call.is_cancelled()));
        Status::ok()
    });
    let c = client(&s);
    let call = c.start_call("/t.Echo/Say", &vec![], None).unwrap();
    call.send(b"one").unwrap();
    std::thread::sleep(Duration::from_millis(200));
    call.cancel();
    let (first, code, cancelled) = rx.recv_timeout(Duration::from_secs(5)).expect("the handler noticed the cancel");
    assert_eq!(first.unwrap(), true);
    assert_eq!(code, Code::Cancelled);
    assert!(cancelled);
}

#[test]
fn a_call_that_ends_without_a_message_carries_its_status_trailers_only() {
    let s = serve(|call| {
        call.add_trailer("x-reason", "none such");
        Status::new(Code::NotFound, "no such thing")
    });
    let c = client(&s);
    let e = c.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::NotFound, "{e}");
    assert_eq!(e.message, "no such thing");
}

#[test]
fn a_panicking_handler_is_an_internal_error_and_the_server_goes_on() {
    let s = serve(|call| {
        if call.method() == "Boom" {
            panic!("handler bug");
        }
        let _ = call.send(b"fine");
        Status::ok()
    });
    let c = client(&s);
    let e = c.unary("/t.Echo/Boom", b"x", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::Internal, "{e}");
    let (r, _) = c.unary("/t.Echo/Fine", b"x", &vec![], Some(Duration::from_secs(5))).unwrap();
    assert_eq!(r, b"fine");
}

#[test]
fn response_headers_and_trailers_reach_the_client() {
    let s = serve(|call| {
        call.add_response_header("x-served-by", "test");
        call.add_trailer("x-count", "1");
        let _ = call.send(b"hello");
        Status::ok()
    });
    let c = client(&s);
    let (r, meta) = c.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap();
    assert_eq!(r, b"hello");
    assert!(meta.iter().any(|(n, v)| n == "x-served-by" && v == "test"), "{meta:?}");
    assert!(meta.iter().any(|(n, v)| n == "x-count" && v == "1"), "{meta:?}");
}

#[test]
fn request_metadata_deadline_and_peer_are_visible_to_the_handler() {
    let seen = Arc::new(Mutex::new(None));
    let seen2 = seen.clone();
    let s = serve(move |call| {
        *seen2.lock().unwrap() = Some((call.service().to_string(), call.method().to_string(), call.metadata().clone(), call.deadline().is_some(), call.peer_addr().is_some()));
        let _ = call.send(b"ok");
        Status::ok()
    });
    let c = client(&s);
    c.unary("/pkg.Svc/Method", b"x", &vec![("x-tenant".into(), "t1".into())], Some(Duration::from_secs(5))).unwrap();
    let (svc, method, meta, deadline, peer) = seen.lock().unwrap().take().unwrap();
    assert_eq!((svc.as_str(), method.as_str()), ("pkg.Svc", "Method"));
    assert!(meta.iter().any(|(n, v)| n == "x-tenant" && v == "t1"), "{meta:?}");
    assert!(!meta.iter().any(|(n, _)| n == "content-type" || n == "te"), "reserved headers stay out: {meta:?}");
    assert!(deadline && peer);
}

#[test]
fn a_streaming_call_may_outlast_a_quiet_spell_and_a_large_total() {
    // 3 MiB over one call in 64 KiB messages: more than the engine's default body limit, with pauses
    let s = serve(|call| {
        let mut total = 0usize;
        while let Ok(Some(m)) = call.recv() {
            total += m.len();
        }
        let _ = call.send(total.to_string().as_bytes());
        Status::ok()
    });
    let c = client(&s);
    let mut call = c.start_call("/t.Echo/Sum", &vec![], Some(Duration::from_secs(60))).unwrap();
    let msg = vec![7u8; 64 * 1024];
    for i in 0..48 {
        call.send(&msg).unwrap();
        if i == 10 {
            std::thread::sleep(Duration::from_millis(1500));
        }
    }
    call.close_send().unwrap();
    assert_eq!(call.recv().unwrap().unwrap(), (48 * 64 * 1024).to_string().as_bytes());
}

#[test]
fn a_stopped_server_refuses_new_connections_and_returns_promptly() {
    let s = serve(|call| {
        let _ = call.send(b"x");
        Status::ok()
    });
    let c = client(&s);
    c.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap();
    let t = Instant::now();
    s.stop(Duration::from_secs(1));
    assert!(t.elapsed() < Duration::from_secs(4), "stop took {:?}", t.elapsed());
    c.close();
    let e = c.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(3))).unwrap_err();
    assert_eq!(e.code, Code::Unavailable, "{e}");
}

#[test]
fn many_calls_blocked_in_their_handlers_do_not_starve_a_new_call() {
    // every blocked call holds pool threads (the call and the reader of its request); a new call still gets one
    let s = serve(|call| {
        if call.method() == "Hold" {
            while let Ok(Some(_)) = call.recv() {}
            return Status::ok();
        }
        let _ = call.send(b"ok");
        Status::ok()
    });
    let c = Arc::new(client(&s));
    let holding: Vec<_> = (0..200)
        .map(|_| {
            let call = c.start_call("/t.Echo/Hold", &vec![], Some(Duration::from_secs(60))).unwrap();
            call.send(b"x").unwrap();
            call
        })
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let t = Instant::now();
    let (r, _) = c.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(10))).unwrap();
    assert_eq!(r, b"ok");
    assert!(t.elapsed() < Duration::from_secs(3), "the call waited {:?} behind blocked ones", t.elapsed());
    for call in holding {
        call.cancel();
    }
}

#[test]
fn the_server_ends_a_call_at_its_deadline_whatever_the_handler_is_doing() {
    // the handler sleeps through the deadline; a raw HTTP/2 client (no timer of its own) sends grpc-timeout
    let s = serve(|call| {
        std::thread::sleep(Duration::from_secs(2));
        let _ = call.send(b"late");
        Status::ok()
    });
    let mut cfg = simoom_client::Config::default();
    cfg.version = simoom_client::HttpVersion::Http2;
    let client = simoom_client::ClientBuilder::default().config(cfg).build();
    let mut h = simoom_client::Headers::new();
    h.append("content-type", "application/grpc");
    h.append("te", "trailers");
    h.append("grpc-timeout", "300m");
    let mut call = client.call("POST", &format!("http://{}/t.Echo/Slow", s.local_addr()), &h).unwrap();
    call.send(&[0, 0, 0, 0, 1, b'x']).unwrap();
    call.finish().unwrap();
    let started = Instant::now();
    let until = Some(Instant::now() + Duration::from_secs(5));
    let status_in_head = call.response_until(until).unwrap().1.get("grpc-status").map(str::to_string);
    let mut buf = [0u8; 64];
    while call.read_until(&mut buf, until).unwrap_or(0) > 0 {}
    let status = status_in_head.or_else(|| call.trailers().and_then(|t| t.get("grpc-status").map(str::to_string)));
    assert_eq!(status.as_deref(), Some("4"), "DEADLINE_EXCEEDED");
    assert!(started.elapsed() < Duration::from_millis(1500), "ended after {:?}, not when the handler returned", started.elapsed());
}

/// A service whose methods are all unary, running `f`.
struct Unary<F>(F);

impl<F: Fn(&ServerCall) -> Status + Send + Sync> cameleer_grpc::grpc::server::GrpcService for Unary<F> {
    fn call(&self, call: &ServerCall) -> Status {
        (self.0)(call)
    }
    fn unary(&self, _: &str, _: &str) -> bool {
        true
    }
}

fn serve_unary(f: impl Fn(&ServerCall) -> Status + Send + Sync + 'static) -> GrpcServer {
    GrpcServer::bind("127.0.0.1:0", H2Config::default(), None, 4 << 20, Arc::new(Unary(f))).unwrap()
}

// A unary reply is held and sent whole: head, message, response metadata and trailers in one
// response; an error status sends no message (trailers only), and a second reply is refused.
#[test]
fn a_unary_reply_carries_its_message_metadata_and_status() {
    let s = serve_unary(|call| {
        let m = call.recv().unwrap().unwrap();
        match &m[..] {
            b"fail" => {
                call.send(b"never sent").unwrap();
                Status::new(Code::NotFound, "no such thing")
            }
            b"twice" => {
                call.send(b"one").unwrap();
                match call.send(b"two") {
                    Err(e) => e,
                    Ok(()) => Status::ok(),
                }
            }
            _ => {
                call.add_response_header("x-answer", "42");
                call.add_trailer("x-trailer", "t");
                call.send(&[b"re: ".as_slice(), &m].concat()).unwrap();
                Status::ok()
            }
        }
    });
    let c = client(&s);
    let (reply, md) = c.unary("/t.S/M", b"hello", &vec![], Some(Duration::from_secs(5))).unwrap();
    assert_eq!(reply, b"re: hello");
    assert!(md.iter().any(|(n, v)| n == "x-answer" && v == "42") && md.iter().any(|(n, v)| n == "x-trailer" && v == "t"), "{md:?}");
    let e = c.unary("/t.S/M", b"fail", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!((e.code, e.message.as_str()), (Code::NotFound, "no such thing"));
    let e = c.unary("/t.S/M", b"twice", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::Internal, "{e:?}");
}

/// A bare HTTP/2 client (simoom-h2) that writes each call's head, message and end of stream in one
/// write, the way h2load and most gRPC clients send a unary call.
struct OneWrite {
    s: std::net::TcpStream,
    conn: simoom_h2::Connection,
    buf: Vec<u8>,
}

impl OneWrite {
    fn connect(s: &GrpcServer) -> OneWrite {
        let s = std::net::TcpStream::connect(s.local_addr()).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        OneWrite { s, conn: simoom_h2::Connection::client(simoom_h2::Config::default()), buf: Vec::new() }
    }

    fn write(&mut self) {
        use std::io::Write;
        let out = self.conn.output().to_vec();
        self.conn.advance_output(out.len());
        self.s.write_all(&out).unwrap();
    }

    /// One unary call: the reply message.
    fn unary(&mut self, path: &str, msg: &[u8]) -> Vec<u8> {
        use std::io::Read;
        let fields: [(&[u8], &[u8]); 2] = [(b"content-type", b"application/grpc"), (b"te", b"trailers")];
        let id = self.conn.send_request(b"POST", b"http", b"localhost", path.as_bytes(), &fields, false).unwrap();
        let mut framed = vec![0u8];
        framed.extend_from_slice(&(msg.len() as u32).to_be_bytes());
        framed.extend_from_slice(msg);
        self.conn.send_data(id, &framed, true).unwrap();
        self.write();
        let mut body = Vec::new();
        let mut done = false;
        let mut chunk = [0u8; 16384];
        while !done {
            let n = self.s.read(&mut chunk).unwrap();
            assert!(n > 0, "connection closed");
            self.buf.extend_from_slice(&chunk[..n]);
            let mut got = Vec::new();
            let used = self.conn.recv(&self.buf, 0, &mut |ev| match ev {
                simoom_h2::Event::Data { stream, data, .. } if stream == id => got.extend_from_slice(data),
                simoom_h2::Event::Trailers { stream, .. } if stream == id => done = true,
                simoom_h2::Event::Headers { stream, end_stream: true, .. } if stream == id => done = true,
                _ => {}
            });
            self.buf.drain(..used);
            self.conn.release(id, got.len());
            body.extend_from_slice(&got);
            self.write();
        }
        body.get(5..).unwrap_or_default().to_vec()
    }
}

// A fast unary route moves onto the event loop (no thread hand-off per call) once its calls arrive
// whole; a route that blocks stays on the pool, so it never stalls the loop.
#[test]
fn fast_unary_calls_run_on_the_loop_and_blocking_ones_on_the_pool() {
    let threads = Arc::new(Mutex::new(Vec::new()));
    let t2 = threads.clone();
    let s = serve_unary(move |call| {
        let m = call.recv().unwrap().unwrap();
        if m == b"slow" {
            std::thread::sleep(Duration::from_millis(60));
        }
        t2.lock().unwrap().push((m.clone(), std::thread::current().name().unwrap_or("").to_string()));
        call.send(&m).unwrap();
        Status::ok()
    });
    let mut c = OneWrite::connect(&s);
    for _ in 0..300 {
        assert_eq!(c.unary("/t.S/Fast", b"fast"), b"fast");
    }
    for _ in 0..3 {
        assert_eq!(c.unary("/t.S/Slow", b"slow"), b"slow");
    }
    // the crate's own client too (its unary replies are held whatever thread runs the call)
    assert_eq!(client(&s).unary("/t.S/Fast", b"fast", &vec![], Some(Duration::from_secs(5))).unwrap().0, b"fast");
    let t = threads.lock().unwrap();
    let on_loop = t.iter().filter(|(m, n)| m == b"fast" && n.starts_with("simoom")).count();
    assert!(on_loop >= 100, "a fast route runs on a loop thread: {on_loop} of 300 calls did");
    assert!(t.iter().filter(|(m, _)| m == b"slow").all(|(_, n)| !n.starts_with("simoom")), "a blocking route stays on the pool: {:?}", t.iter().filter(|(m, _)| m == b"slow").collect::<Vec<_>>());
}
