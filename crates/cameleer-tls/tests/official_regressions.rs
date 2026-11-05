//! Hermetic regressions for interop gaps found with Go's crypto/tls (`tests/official_tls.rs`).

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

use cameleer_tls::{TlsClientConfig, TlsStream, TlsVersion};

/// The extension types of the first ClientHello a client sends.
fn client_hello_extensions(cfg: TlsClientConfig) -> Vec<u16> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let tcp = TcpStream::connect(("127.0.0.1", port)).unwrap();
        tcp.set_read_timeout(Some(Duration::from_secs(2))).ok();
        let _ = TlsStream::connect(tcp, "localhost", &cfg);
    });
    let (mut sock, _) = listener.accept().unwrap();
    let mut head = [0u8; 5];
    sock.read_exact(&mut head).unwrap();
    let mut rec = vec![0u8; u16::from_be_bytes([head[3], head[4]]) as usize];
    sock.read_exact(&mut rec).unwrap();
    drop(sock);
    t.join().unwrap();
    assert_eq!((head[0], rec[0]), (22, 1), "a ClientHello record");
    let mut i = 4 + 2 + 32;
    i += 1 + rec[i] as usize;
    i += 2 + u16::from_be_bytes([rec[i], rec[i + 1]]) as usize;
    i += 1 + rec[i] as usize;
    let end = i + 2 + u16::from_be_bytes([rec[i], rec[i + 1]]) as usize;
    i += 2;
    let mut types = Vec::new();
    while i + 4 <= end {
        types.push(u16::from_be_bytes([rec[i], rec[i + 1]]));
        i += 4 + u16::from_be_bytes([rec[i + 2], rec[i + 3]]) as usize;
    }
    types
}

const PSK_KEY_EXCHANGE_MODES: u16 = 45;

#[test]
fn first_client_hello_lists_psk_modes_when_resumption_is_enabled() {
    // Go's crypto/tls (and BoringSSL) only send TLS 1.3 NewSessionTickets to clients that
    // list psk_dhe_ke, so a full handshake must advertise it or resumption never starts
    let with_cache = TlsClientConfig::builder().system_roots(false).insecure_skip_verify(true).build();
    assert!(client_hello_extensions(with_cache).contains(&PSK_KEY_EXCHANGE_MODES));
    let no_cache = TlsClientConfig::builder().system_roots(false).insecure_skip_verify(true).session_resumption(false).build();
    assert!(!client_hello_extensions(no_cache).contains(&PSK_KEY_EXCHANGE_MODES));
    let tls12_only = TlsClientConfig::builder().system_roots(false).insecure_skip_verify(true).versions(TlsVersion::Tls12, TlsVersion::Tls12).build();
    assert!(!client_hello_extensions(tls12_only).contains(&PSK_KEY_EXCHANGE_MODES));
}
