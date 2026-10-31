//! Optional test against a real public HTTPS site using the system trust store.
//! Ignored by default (needs network): `cargo test -p camel-tls --test public_site -- --ignored`.
//! Honors `HTTPS_PROXY` / `https_proxy` (HTTP CONNECT tunnel). Override the target with
//! `CAMEL_TLS_TEST_HOST` (default `pypi.org`); `NO_PROXY` is honored.

use camel_tls::{TlsClientConfig, TlsStream};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

fn bypass_proxy(host: &str) -> bool {
    let list = std::env::var("NO_PROXY").or_else(|_| std::env::var("no_proxy")).unwrap_or_default();
    list.split(',').map(|e| e.trim().trim_start_matches('*').trim_start_matches('.')).any(|e| !e.is_empty() && (host == e || host.ends_with(&format!(".{e}"))))
}

fn connect_via_proxy(host: &str, port: u16) -> TcpStream {
    let proxy = std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy")).ok().filter(|_| !bypass_proxy(host));
    match proxy {
        Some(p) => {
            let hostport = p.trim_start_matches("http://").trim_end_matches('/').rsplit('@').next().unwrap().to_string();
            let mut s = TcpStream::connect(&hostport).expect("connect to proxy");
            s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            write!(s, "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n").unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut status = String::new();
            r.read_line(&mut status).unwrap();
            assert!(status.contains(" 200"), "proxy refused CONNECT: {status}");
            loop {
                let mut l = String::new();
                r.read_line(&mut l).unwrap();
                if l == "\r\n" || l.is_empty() {
                    break;
                }
            }
            assert!(r.buffer().is_empty());
            s
        }
        None => {
            let s = TcpStream::connect((host, port)).expect("connect");
            s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
            s
        }
    }
}

#[test]
#[ignore]
fn https_get_public_site() {
    let host = std::env::var("CAMEL_TLS_TEST_HOST").unwrap_or_else(|_| "pypi.org".into());
    let cfg = TlsClientConfig::builder().alpn_protocols(&["http/1.1"]).build();
    assert!(!cfg.roots().is_empty(), "system trust store is empty");
    let sock = connect_via_proxy(&host, 443);
    let mut tls = TlsStream::connect(sock, &host, &cfg).unwrap_or_else(|e| panic!("TLS handshake with {host} failed: {e}"));
    eprintln!("{host}: {:?} {} {}", tls.protocol_version(), tls.cipher_suite().name(), tls.named_group().name());
    write!(tls, "GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: camel-tls-test\r\n\r\n").unwrap();
    let mut resp = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match tls.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => resp.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !resp.is_empty() => break,
            Err(e) => panic!("read failed: {e}"),
        }
    }
    let text = String::from_utf8_lossy(&resp);
    assert!(text.starts_with("HTTP/1.1 "), "unexpected response: {text}");
}
