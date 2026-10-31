//! Client <-> server over loopback TCP for every suite / group combination.

use camel_tls::{
    self_signed, CertError, CipherSuite, NamedGroup, TlsClientConfig, TlsError, TlsServerConfig, TlsStream, TlsVersion,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::thread;
use std::time::Duration;

const RSA_CERT: &str = include_str!("../testdata/rsa_selfsigned.pem");
const RSA_KEY: &str = include_str!("../testdata/rsa2048.pkcs8.pem");
const RSA_KEY_PKCS1: &str = include_str!("../testdata/rsa2048.pkcs1.pem");
const CA: &str = include_str!("../testdata/ca.pem");
const INT: &str = include_str!("../testdata/int.pem");
const LEAF: &str = include_str!("../testdata/leaf.pem");
const LEAF_KEY: &str = include_str!("../testdata/leaf.key");

fn ec_identity() -> &'static camel_tls::SelfSigned {
    static ID: OnceLock<camel_tls::SelfSigned> = OnceLock::new();
    ID.get_or_init(|| self_signed("localhost"))
}

struct Outcome {
    version: TlsVersion,
    suite: CipherSuite,
    group: NamedGroup,
    alpn: Option<Vec<u8>>,
    server_sni: Option<String>,
}

/// Runs a server thread that echoes one line in upper case, then closes.
fn run_pair(server_cfg: TlsServerConfig, client_cfg: TlsClientConfig, name: &str) -> Result<Outcome, TlsError> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = thread::spawn(move || -> Result<Option<String>, TlsError> {
        let (sock, _) = listener.accept().unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        let mut tls = TlsStream::accept(sock, &server_cfg)?;
        let sni = tls.sni_hostname().map(str::to_string);
        let mut line = String::new();
        {
            let mut r = BufReader::new(&mut tls);
            r.read_line(&mut line).map_err(TlsError::Io)?;
        }
        tls.write_all(line.to_uppercase().as_bytes()).map_err(TlsError::Io)?;
        tls.close().map_err(TlsError::Io)?;
        let mut rest = Vec::new();
        tls.read_to_end(&mut rest).map_err(TlsError::Io)?;
        Ok(sni)
    });
    let sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let client_result = (|| -> Result<_, TlsError> {
        let mut tls = TlsStream::connect(sock, name, &client_cfg)?;
        tls.write_all(b"hello tls\n").map_err(TlsError::Io)?;
        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).map_err(TlsError::Io)?;
        assert_eq!(resp, b"HELLO TLS\n");
        assert!(tls.peer_closed());
        tls.close().map_err(TlsError::Io)?;
        Ok((tls.protocol_version(), tls.cipher_suite(), tls.named_group().unwrap(), tls.alpn_protocol().map(|a| a.to_vec())))
    })();
    let server_result = server.join().unwrap();
    let (version, suite, group, alpn) = client_result?;
    let server_sni = server_result?;
    Ok(Outcome { version, suite, group, alpn, server_sni })
}

fn ec_server() -> camel_tls::TlsServerConfigBuilder {
    let id = ec_identity();
    TlsServerConfig::builder().cert_chain_pem(id.cert_pem.as_bytes()).private_key_pem(id.key_pem.as_bytes())
}

fn rsa_server() -> camel_tls::TlsServerConfigBuilder {
    TlsServerConfig::builder().cert_chain_pem(RSA_CERT.as_bytes()).private_key_pem(RSA_KEY.as_bytes())
}

fn ec_client() -> camel_tls::TlsClientConfigBuilder {
    TlsClientConfig::builder().system_roots(false).add_root_pem(ec_identity().cert_pem.as_bytes()).unwrap()
}

fn rsa_client() -> camel_tls::TlsClientConfigBuilder {
    TlsClientConfig::builder().system_roots(false).add_root_pem(RSA_CERT.as_bytes()).unwrap()
}

const TLS13_SUITES: [CipherSuite; 3] =
    [CipherSuite::Tls13Aes128GcmSha256, CipherSuite::Tls13Aes256GcmSha384, CipherSuite::Tls13Chacha20Poly1305Sha256];

#[test]
fn tls13_all_suites_and_groups() {
    for suite in TLS13_SUITES {
        for group in NamedGroup::ALL {
            let client = ec_client().cipher_suites(&[suite]).groups(&[group]).build();
            let out = run_pair(ec_server().build().unwrap(), client, "localhost").unwrap_or_else(|e| panic!("{suite:?}/{group:?}: {e}"));
            assert_eq!(out.version, TlsVersion::Tls13);
            assert_eq!(out.suite, suite);
            assert_eq!(out.group, group);
            assert_eq!(out.server_sni.as_deref(), Some("localhost"));
        }
    }
}

#[test]
fn tls13_rsa_pss_certificate() {
    for key in [RSA_KEY, RSA_KEY_PKCS1] {
        let server = TlsServerConfig::builder().cert_chain_pem(RSA_CERT.as_bytes()).private_key_pem(key.as_bytes()).build().unwrap();
        let out = run_pair(server, rsa_client().build(), "rsa.test").unwrap();
        assert_eq!(out.version, TlsVersion::Tls13);
    }
}

#[test]
fn tls12_ecdsa_suites_and_groups() {
    let suites = [CipherSuite::EcdheEcdsaAes128GcmSha256, CipherSuite::EcdheEcdsaAes256GcmSha384, CipherSuite::EcdheEcdsaChacha20Poly1305];
    for suite in suites {
        for group in NamedGroup::ALL {
            // the certificate curve (P-256) must be listed too (RFC 8422); the server picks `group`
            let client = ec_client()
                .versions(TlsVersion::Tls12, TlsVersion::Tls12)
                .cipher_suites(&[suite])
                .groups(&[group, NamedGroup::Secp256r1])
                .build();
            let out = run_pair(ec_server().groups(&[group]).build().unwrap(), client, "localhost").unwrap_or_else(|e| panic!("{suite:?}/{group:?}: {e}"));
            assert_eq!(out.version, TlsVersion::Tls12);
            assert_eq!(out.suite, suite);
            assert_eq!(out.group, group);
        }
    }
}

#[test]
fn tls12_rsa_suites_and_groups() {
    let suites = [CipherSuite::EcdheRsaAes128GcmSha256, CipherSuite::EcdheRsaAes256GcmSha384, CipherSuite::EcdheRsaChacha20Poly1305];
    for suite in suites {
        for group in NamedGroup::ALL {
            let client = rsa_client().versions(TlsVersion::Tls12, TlsVersion::Tls12).cipher_suites(&[suite]).groups(&[group]).build();
            let out = run_pair(rsa_server().build().unwrap(), client, "rsa.test").unwrap_or_else(|e| panic!("{suite:?}/{group:?}: {e}"));
            assert_eq!(out.version, TlsVersion::Tls12);
            assert_eq!(out.suite, suite);
            assert_eq!(out.group, group);
        }
    }
}

#[test]
fn tls12_server_limited_client_falls_back() {
    // client offers 1.3+1.2, server only 1.2 (downgrade sentinel must not be set by a 1.2-only server)
    let server = ec_server().versions(TlsVersion::Tls12, TlsVersion::Tls12).build().unwrap();
    let out = run_pair(server, ec_client().build(), "localhost").unwrap();
    assert_eq!(out.version, TlsVersion::Tls12);
}

#[test]
fn hello_retry_request() {
    let server = ec_server().groups(&[NamedGroup::Secp256r1]).build().unwrap();
    let client = ec_client().groups(&[NamedGroup::X25519, NamedGroup::Secp256r1]).build();
    let out = run_pair(server, client, "localhost").unwrap();
    assert_eq!(out.version, TlsVersion::Tls13);
    assert_eq!(out.group, NamedGroup::Secp256r1);
    // and with a SHA-384 suite (message_hash uses the suite hash)
    let server = ec_server().groups(&[NamedGroup::Secp384r1]).cipher_suites(&[CipherSuite::Tls13Aes256GcmSha384]).build().unwrap();
    let out = run_pair(server, ec_client().build(), "localhost").unwrap();
    assert_eq!(out.group, NamedGroup::Secp384r1);
    assert_eq!(out.suite, CipherSuite::Tls13Aes256GcmSha384);
}

#[test]
fn alpn_negotiation() {
    let server = ec_server().alpn_protocols(&["http/1.1"]).build().unwrap();
    let client = ec_client().alpn_protocols(&["h2", "http/1.1"]).build();
    assert_eq!(run_pair(server, client, "localhost").unwrap().alpn.as_deref(), Some(&b"http/1.1"[..]));
    let server = ec_server().alpn_protocols(&["http/1.1"]).versions(TlsVersion::Tls12, TlsVersion::Tls12).build().unwrap();
    let client = ec_client().alpn_protocols(&["http/1.1"]).build();
    assert_eq!(run_pair(server, client, "localhost").unwrap().alpn.as_deref(), Some(&b"http/1.1"[..]));
    // no overlap -> no_application_protocol
    let server = ec_server().alpn_protocols(&["h2"]).build().unwrap();
    let client = ec_client().alpn_protocols(&["http/1.1"]).build();
    match run_pair(server, client, "localhost") {
        Err(TlsError::AlertReceived(120)) | Err(TlsError::Protocol { alert: 120, .. }) => {}
        other => panic!("expected no_application_protocol, got {:?}", other.err()),
    }
}

#[test]
fn full_chain_with_intermediate() {
    let chain = format!("{LEAF}{INT}");
    for (min, max) in [(TlsVersion::Tls13, TlsVersion::Tls13), (TlsVersion::Tls12, TlsVersion::Tls12)] {
        let server = TlsServerConfig::builder().cert_chain_pem(chain.as_bytes()).private_key_pem(LEAF_KEY.as_bytes()).build().unwrap();
        let client = TlsClientConfig::builder().system_roots(false).add_root_pem(CA.as_bytes()).unwrap().versions(min, max).build();
        let out = run_pair(server, client, "leaf.camel.test").unwrap();
        assert_eq!(out.version, min);
        // wildcard SAN
        let server = TlsServerConfig::builder().cert_chain_pem(chain.as_bytes()).private_key_pem(LEAF_KEY.as_bytes()).build().unwrap();
        let client = TlsClientConfig::builder().system_roots(false).add_root_pem(CA.as_bytes()).unwrap().versions(min, max).build();
        run_pair(server, client, "svc.apps.camel.test").unwrap();
    }
}

#[test]
fn certificate_failures() {
    // hostname mismatch
    let r = run_pair(ec_server().build().unwrap(), ec_client().build(), "example.com");
    assert!(matches!(r, Err(TlsError::Certificate(CertError::HostnameMismatch { .. }))), "{:?}", r.err());
    // untrusted issuer
    let r = run_pair(ec_server().build().unwrap(), rsa_client().build(), "localhost");
    assert!(matches!(r, Err(TlsError::Certificate(CertError::UnknownIssuer { .. }))), "{:?}", r.err());
    // missing intermediate
    let server = TlsServerConfig::builder().cert_chain_pem(LEAF.as_bytes()).private_key_pem(LEAF_KEY.as_bytes()).build().unwrap();
    let client = TlsClientConfig::builder().system_roots(false).add_root_pem(CA.as_bytes()).unwrap().build();
    assert!(matches!(run_pair(server, client, "leaf.camel.test"), Err(TlsError::Certificate(_))));
    // insecure mode accepts anything (but still checks the handshake signature)
    let client = TlsClientConfig::builder().system_roots(false).insecure_skip_verify(true).build();
    run_pair(ec_server().build().unwrap(), client, "whatever.invalid").unwrap();
    // IP address as server name (no SNI, IP SAN check)
    let out = run_pair(ec_server().build().unwrap(), ec_client().build(), "127.0.0.1").unwrap();
    assert_eq!(out.server_sni, None);
}

#[test]
fn version_mismatch_fails() {
    let server = ec_server().versions(TlsVersion::Tls12, TlsVersion::Tls12).build().unwrap();
    let client = ec_client().versions(TlsVersion::Tls13, TlsVersion::Tls13).build();
    match run_pair(server, client, "localhost") {
        Err(TlsError::AlertReceived(70)) => {}
        other => panic!("expected protocol_version alert, got {:?}", other.err()),
    }
}

#[test]
fn tls12_ecdsa_requires_certificate_curve() {
    let client = ec_client().versions(TlsVersion::Tls12, TlsVersion::Tls12).groups(&[NamedGroup::X25519]).build();
    assert!(matches!(run_pair(ec_server().build().unwrap(), client, "localhost"), Err(TlsError::AlertReceived(40))));
}

#[test]
fn key_mismatch_rejected_by_builder() {
    let r = TlsServerConfig::builder().cert_chain_pem(RSA_CERT.as_bytes()).private_key_pem(ec_identity().key_pem.as_bytes()).build();
    assert!(matches!(r, Err(TlsError::Config(_))));
}

fn bulk_transfer(server_cfg: TlsServerConfig, client_cfg: TlsClientConfig, key_update: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let data: Vec<u8> = (0..1_500_000u32).map(|i| (i * 7 + i / 251) as u8).collect();
    let expected = data.clone();
    let server = thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let mut tls = TlsStream::accept(sock, &server_cfg).unwrap();
        let mut buf = vec![0u8; expected.len()];
        tls.read_exact(&mut buf).unwrap();
        assert!(buf == expected, "server received corrupted data");
        if key_update {
            tls.send_key_update(true).unwrap();
        }
        tls.write_all(&buf).unwrap();
        tls.close().unwrap();
        // drain until the client's close_notify so no unread data triggers a TCP reset
        let mut rest = Vec::new();
        tls.read_to_end(&mut rest).unwrap();
    });
    let sock = TcpStream::connect(addr).unwrap();
    let mut tls = TlsStream::connect(sock, "localhost", &client_cfg).unwrap();
    if key_update {
        tls.send_key_update(true).unwrap();
    }
    // write in odd-sized chunks
    for chunk in data.chunks(70_001) {
        tls.write_all(chunk).unwrap();
    }
    let mut back = Vec::new();
    tls.read_to_end(&mut back).unwrap();
    assert!(back == data, "client received corrupted data");
    tls.close().unwrap();
    server.join().unwrap();
}

#[test]
fn bulk_transfer_tls13_with_key_update() {
    bulk_transfer(ec_server().build().unwrap(), ec_client().build(), true);
}

#[test]
fn bulk_transfer_tls12() {
    bulk_transfer(ec_server().build().unwrap(), ec_client().versions(TlsVersion::Tls12, TlsVersion::Tls12).build(), false);
}

#[test]
fn non_tls_peer_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let t = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        let mut b = [0u8; 512];
        let _ = s.read(&mut b);
        let _ = s.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n");
    });
    let sock = TcpStream::connect(addr).unwrap();
    let r = TlsStream::connect(sock, "localhost", &ec_client().build());
    assert!(matches!(r, Err(TlsError::Protocol { .. })), "{:?}", r.err());
    t.join().unwrap();
}

#[test]
fn truncation_is_reported() {
    // peer closes TCP without close_notify -> UnexpectedEof, not a clean EOF
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let cfg = ec_server().build().unwrap();
    let t = thread::spawn(move || {
        let (sock, _) = listener.accept().unwrap();
        let mut tls = TlsStream::accept(sock, &cfg).unwrap();
        tls.write_all(b"partial").unwrap();
        drop(tls);
    });
    let sock = TcpStream::connect(addr).unwrap();
    let mut tls = TlsStream::connect(sock, "localhost", &ec_client().build()).unwrap();
    let mut v = Vec::new();
    let e = tls.read_to_end(&mut v).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
    assert_eq!(v, b"partial");
    t.join().unwrap();
}
