//! The gRPC client (`GrpcClient`) on simoom-client: connecting, reconnecting, certificate checks, deadlines and
//! cancellation, against the crate's own gRPC server.

use std::sync::Arc;
use std::time::{Duration, Instant};

use cameleer_grpc::grpc::server::{GrpcServer, ServerCall};
use cameleer_grpc::grpc::{Code, GrpcClient, Status};
use cameleer_grpc::h2::H2Config;
use cameleer_tls::{TlsClientConfig, TlsServerConfig};

/// Echoes every request message; a message `slow` makes the call wait 3 seconds before answering.
fn echo(call: &ServerCall) -> Status {
    while let Ok(Some(m)) = call.recv() {
        if m == b"slow" {
            std::thread::sleep(Duration::from_secs(3));
        }
        if call.send(&m).is_err() {
            return Status::new(Code::Cancelled, "gone");
        }
    }
    Status::ok()
}

fn server(tls: Option<TlsServerConfig>) -> GrpcServer {
    GrpcServer::bind("127.0.0.1:0", H2Config::default(), tls, 4 << 20, Arc::new(echo)).unwrap()
}

fn tls_server() -> (GrpcServer, cameleer_tls::SelfSigned) {
    let ss = cameleer_tls::self_signed("localhost");
    let cfg = TlsServerConfig::builder().cert_chain_pem(ss.cert_pem.as_bytes()).private_key_pem(ss.key_pem.as_bytes()).alpn_protocols(&["h2"]).build().unwrap();
    (server(Some(cfg)), ss)
}

#[test]
fn a_call_after_close_connects_again() {
    let s = server(None);
    let client = GrpcClient::plaintext(&s.local_addr().to_string());
    let (r, _) = client.unary("/t.Echo/Say", b"one", &vec![], None).unwrap();
    assert_eq!(r, b"one");
    client.close();
    let (r, _) = client.unary("/t.Echo/Say", b"two", &vec![], None).unwrap();
    assert_eq!(r, b"two");
}

#[test]
fn calls_share_one_connection_and_run_concurrently() {
    let s = server(None);
    let client = Arc::new(GrpcClient::plaintext(&s.local_addr().to_string()));
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let c = client.clone();
            std::thread::spawn(move || {
                for j in 0..10 {
                    let msg = format!("{i}-{j}");
                    let (r, _) = c.unary("/t.Echo/Say", msg.as_bytes(), &vec![], None).unwrap();
                    assert_eq!(r, msg.as_bytes());
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(s.h2().connection_count(), 1, "all calls ride on one connection");
}

#[test]
fn an_unreachable_target_is_unavailable() {
    // a port nobody listens on
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let client = GrpcClient::plaintext(&format!("127.0.0.1:{port}"));
    let e = client.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::Unavailable, "{e}");
    assert!(e.message.contains("cannot connect"), "{e}");
}

#[test]
fn the_deadline_ends_a_call_the_server_is_still_working_on() {
    let s = server(None);
    let client = GrpcClient::plaintext(&s.local_addr().to_string());
    let t = Instant::now();
    let e = client.unary("/t.Echo/Say", b"slow", &vec![], Some(Duration::from_millis(300))).unwrap_err();
    assert_eq!(e.code, Code::DeadlineExceeded, "{e}");
    assert!(t.elapsed() < Duration::from_secs(2), "returned after {:?}", t.elapsed());
    // the channel is fine afterwards
    let (r, _) = client.unary("/t.Echo/Say", b"fast", &vec![], None).unwrap();
    assert_eq!(r, b"fast");
}

#[test]
fn cancel_ends_a_streaming_call_and_the_channel_survives() {
    let s = server(None);
    let client = GrpcClient::plaintext(&s.local_addr().to_string());
    let mut call = client.start_call("/t.Echo/Say", &vec![], None).unwrap();
    call.send(b"a").unwrap();
    assert_eq!(call.recv().unwrap().unwrap(), b"a");
    call.cancel();
    assert!(call.recv().is_err() || call.recv().unwrap().is_none() || call.status().is_some());
    let (r, _) = client.unary("/t.Echo/Say", b"b", &vec![], None).unwrap();
    assert_eq!(r, b"b");
}

#[test]
fn a_sender_on_another_thread_feeds_a_bidirectional_call() {
    let s = server(None);
    let client = GrpcClient::plaintext(&s.local_addr().to_string());
    let mut call = client.start_call("/t.Echo/Say", &vec![], None).unwrap();
    let sender = call.sender();
    let t = std::thread::spawn(move || {
        for i in 0..5 {
            sender.send(format!("m{i}").as_bytes()).unwrap();
        }
        sender.close_send().unwrap();
    });
    let mut got = Vec::new();
    while let Some(m) = call.recv().unwrap() {
        got.push(String::from_utf8(m).unwrap());
    }
    t.join().unwrap();
    assert_eq!(got, ["m0", "m1", "m2", "m3", "m4"]);
}

#[test]
fn a_server_certificate_the_client_does_not_trust_is_refused() {
    let (s, _ss) = tls_server();
    let addr = s.local_addr().to_string();
    // the system roots do not include the self-signed certificate
    let client = GrpcClient::tls(&addr, "localhost", TlsClientConfig::builder().alpn_protocols(&["h2"]).build());
    let e = client.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::Unavailable, "{e}");
}

#[test]
fn a_trusted_certificate_works_and_the_name_is_checked() {
    let (s, ss) = tls_server();
    let addr = s.local_addr().to_string();
    let trusting = || TlsClientConfig::builder().alpn_protocols(&["h2"]).system_roots(false).add_root_pem(ss.cert_pem.as_bytes()).unwrap().build();
    let client = GrpcClient::tls(&addr, "localhost", trusting());
    let (r, _) = client.unary("/t.Echo/Say", b"secure", &vec![], None).unwrap();
    assert_eq!(r, b"secure");
    // the certificate is for localhost, not for this name
    let wrong = GrpcClient::tls(&addr, "example.invalid", trusting());
    let e = wrong.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::Unavailable, "{e}");
}

#[test]
fn turning_certificate_verification_off_is_refused_with_a_message_that_says_what_to_do() {
    let (s, _ss) = tls_server();
    let cfg = TlsClientConfig::builder().alpn_protocols(&["h2"]).insecure_skip_verify(true).build();
    let client = GrpcClient::tls(&s.local_addr().to_string(), "localhost", cfg);
    let e = client.unary("/t.Echo/Say", b"x", &vec![], Some(Duration::from_secs(5))).unwrap_err();
    assert_eq!(e.code, Code::InvalidArgument, "{e}");
    assert!(e.message.contains("trust"), "{e}");
}

#[test]
fn a_sender_cancel_from_another_thread_wakes_a_blocked_recv() {
    let s = server(None);
    let client = GrpcClient::plaintext(&s.local_addr().to_string());
    let mut call = client.start_call("/t.Echo/Say", &vec![], None).unwrap();
    let sender = call.sender();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        sender.cancel();
    });
    let started = Instant::now();
    // the server sends nothing, so recv blocks until the cancel arrives
    let r = call.recv();
    t.join().unwrap();
    assert!(r.is_err(), "{r:?}");
    assert!(started.elapsed() < Duration::from_secs(3), "recv woke after {:?}", started.elapsed());
}
