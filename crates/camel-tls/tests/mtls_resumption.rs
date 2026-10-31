//! Mutual TLS (client certificates), Ed25519 certificates, CRL checks and session
//! resumption over loopback, in TLS 1.3 and TLS 1.2.

use camel_tls::certgen::{issue, issue_crl, CertParams};
use camel_tls::testing::{mtls, mtls_anonymous_client_params, mtls_client_params, mtls_server_params, mtls_untrusted_client_params};
use camel_tls::tls::alert;
use camel_tls::{
    CertifiedKey, ClientAuth, Crl, NamedGroup, PrivateKey, TlsClientConfig, TlsError, TlsServerConfig, TlsStream, TlsVersion,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[derive(Debug)]
struct ServerSide {
    resumed: bool,
    peer_cn: Option<String>,
    origin: Vec<u8>,
    version: TlsVersion,
}

#[derive(Debug)]
struct ClientSide {
    resumed: bool,
    version: TlsVersion,
    peer_certs: usize,
}

fn cn(der: &[u8]) -> String {
    camel_tls::Certificate::from_der(der).unwrap().subject.common_name().unwrap_or("").to_string()
}

/// A server accepting `n` connections with one configuration; each echoes a line upper-cased.
fn server(cfg: TlsServerConfig, n: usize) -> (u16, mpsc::Receiver<Result<ServerSide, TlsError>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let cfg = Arc::new(cfg);
    thread::spawn(move || {
        for _ in 0..n {
            let (s, _) = match l.accept() {
                Ok(x) => x,
                Err(_) => return,
            };
            s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let r = (|| {
                let mut t = TlsStream::accept(s, &cfg)?;
                let mut line = String::new();
                BufReader::new(&mut t).read_line(&mut line).map_err(TlsError::Io)?;
                t.write_all(line.to_uppercase().as_bytes()).map_err(TlsError::Io)?;
                let _ = t.close();
                Ok(ServerSide {
                    resumed: t.session_resumed(),
                    peer_cn: t.peer_certificates().first().map(|c| cn(c)),
                    origin: t.session_origin_id().to_vec(),
                    version: t.protocol_version(),
                })
            })();
            let _ = tx.send(r);
        }
    });
    (port, rx)
}

/// Connects, exchanges one line (reading the reply also picks up TLS 1.3 tickets).
fn client(port: u16, cfg: &TlsClientConfig) -> Result<ClientSide, TlsError> {
    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut t = TlsStream::connect_with_session_key(s, "localhost", cfg, Some(&format!("localhost:{port}")))?;
    t.write_all(b"ping\n").map_err(TlsError::Io)?;
    let mut line = String::new();
    BufReader::new(&mut t).read_line(&mut line).map_err(TlsError::Io)?;
    if line != "PING\n" {
        return Err(TlsError::Io(std::io::Error::other(format!("bad reply {line:?}"))));
    }
    let _ = t.close();
    Ok(ClientSide { resumed: t.session_resumed(), version: t.protocol_version(), peer_certs: t.peer_certificates().len() })
}

fn versions() -> [(TlsVersion, &'static str); 2] {
    [(TlsVersion::Tls13, "TLS 1.3"), (TlsVersion::Tls12, "TLS 1.2")]
}

fn server_cfg(mode: ClientAuth, v: TlsVersion) -> TlsServerConfig {
    let mut p = mtls_server_params(mode);
    p.min_version = Some(v);
    p.max_version = Some(v);
    p.server_config().unwrap()
}

fn client_cfg(p: camel_tls::SslContextParameters) -> TlsClientConfig {
    p.client_config().unwrap()
}

#[test]
fn mutual_tls_accepts_trusted_client_certificates() {
    let pki = mtls();
    // an Ed25519 client certificate from the same CA
    let ed_client = issue(&CertParams::client("ed-client"), PrivateKey::generate_ed25519(), Some(&pki.ca));
    let ed_cfg = TlsClientConfig::builder()
        .system_roots(false)
        .add_root_pem(pki.ca.cert_pem.as_bytes())
        .unwrap()
        .client_certificate(CertifiedKey::new(vec![ed_client.cert_der.clone()], ed_client.key.clone()).unwrap())
        .build();
    for (v, label) in versions() {
        for mode in [ClientAuth::Need, ClientAuth::Want] {
            let (port, rx) = server(server_cfg(mode, v), 2);
            let c = client(port, &client_cfg(mtls_client_params())).unwrap_or_else(|e| panic!("{label} {mode:?}: {e}"));
            assert_eq!(c.version, v);
            let s = rx.recv().unwrap().unwrap();
            assert_eq!(s.peer_cn.as_deref(), Some("camel-client"), "{label} {mode:?}");
            client(port, &ed_cfg).unwrap_or_else(|e| panic!("{label} Ed25519 client: {e}"));
            assert_eq!(rx.recv().unwrap().unwrap().peer_cn.as_deref(), Some("ed-client"), "{label}");
        }
    }
}

fn expect_rejected(port: u16, cfg: &TlsClientConfig, rx: &mpsc::Receiver<Result<ServerSide, TlsError>>, alert_code: u8, label: &str) {
    // in TLS 1.3 the client finishes its handshake before the server validates the
    // certificate: the alert then arrives on the first read
    let received = match client(port, cfg) {
        Err(TlsError::AlertReceived(a)) => Some(a),
        Err(TlsError::Io(e)) => e.get_ref().and_then(|i| i.downcast_ref::<TlsError>()).and_then(|t| match t {
            TlsError::AlertReceived(a) => Some(*a),
            _ => None,
        }),
        _ => None,
    };
    assert_eq!(received, Some(alert_code), "{label}: expected alert {}", alert::name(alert_code));
    let s = rx.recv().unwrap();
    assert!(s.is_err(), "{label}: server accepted");
}

#[test]
fn mutual_tls_rejects_missing_and_untrusted_certificates() {
    for (v, label) in versions() {
        let (port, rx) = server(server_cfg(ClientAuth::Need, v), 2);
        let missing = if v == TlsVersion::Tls13 { alert::CERTIFICATE_REQUIRED } else { alert::HANDSHAKE_FAILURE };
        expect_rejected(port, &client_cfg(mtls_anonymous_client_params()), &rx, missing, &format!("{label} without certificate"));
        expect_rejected(port, &client_cfg(mtls_untrusted_client_params()), &rx, alert::UNKNOWN_CA, &format!("{label} untrusted certificate"));
    }
}

#[test]
fn want_mode_accepts_anonymous_clients() {
    for (v, label) in versions() {
        let (port, rx) = server(server_cfg(ClientAuth::Want, v), 1);
        client(port, &client_cfg(mtls_anonymous_client_params())).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(rx.recv().unwrap().unwrap().peer_cn, None);
    }
}

#[test]
fn crl_revokes_client_certificate() {
    let pki = mtls();
    let crl = Crl::from_der(&issue_crl(&pki.ca, &[&pki.client.cert_der], 7)).unwrap();
    for (v, label) in versions() {
        let cfg = TlsServerConfig::builder()
            .cert_chain_der(vec![pki.server.cert_der.clone()])
            .private_key(pki.server.key.clone())
            .client_auth(ClientAuth::Need)
            .add_client_root_pem(pki.ca.cert_pem.as_bytes())
            .crls(vec![crl.clone()])
            .versions(v, v)
            .build()
            .unwrap();
        let (port, rx) = server(cfg, 1);
        expect_rejected(port, &client_cfg(mtls_client_params()), &rx, alert::CERTIFICATE_REVOKED, label);
    }
}

#[test]
fn ed25519_server_certificates() {
    let ca = issue(&CertParams::ca("Ed CA"), PrivateKey::generate_ed25519(), None);
    let srv = issue(&CertParams::server(&["localhost"]), PrivateKey::generate_ed25519(), Some(&ca));
    for (v, label) in versions() {
        let cfg = TlsServerConfig::builder().cert_chain_der(vec![srv.cert_der.clone()]).private_key(srv.key.clone()).versions(v, v).build().unwrap();
        let (port, rx) = server(cfg, 1);
        let ccfg = TlsClientConfig::builder().system_roots(false).add_root_der(&ca.cert_der).unwrap().build();
        let c = client(port, &ccfg).unwrap_or_else(|e| panic!("{label}: {e}"));
        assert_eq!(c.version, v);
        rx.recv().unwrap().unwrap();
    }
}

#[test]
fn session_resumption_tls13_and_tls12() {
    for (v, label) in versions() {
        for mode in [ClientAuth::None, ClientAuth::Need] {
            let (port, rx) = server(server_cfg(mode, v), 3);
            let cfg = client_cfg(mtls_client_params());
            let first = client(port, &cfg).unwrap();
            let s1 = rx.recv().unwrap().unwrap();
            assert!(!first.resumed && !s1.resumed, "{label}: first connection is a full handshake");
            let second = client(port, &cfg).unwrap();
            let s2 = rx.recv().unwrap().unwrap();
            assert!(second.resumed, "{label} {mode:?}: client did not resume");
            assert!(s2.resumed, "{label} {mode:?}: server did not resume");
            assert_eq!(s2.version, v);
            assert_eq!(s1.origin, s2.origin, "{label}: same session origin");
            assert_eq!(second.peer_certs, first.peer_certs, "{label}: server certificates restored");
            if mode == ClientAuth::Need {
                assert_eq!(s2.peer_cn.as_deref(), Some("camel-client"), "{label}: client certificate restored");
            }
            // a third connection still resumes (tickets are reusable / renewed)
            assert!(client(port, &cfg).unwrap().resumed, "{label}: third connection");
            assert!(rx.recv().unwrap().unwrap().resumed);
        }
    }
}

#[test]
fn tls12_session_ids_without_tickets() {
    let id = camel_tls::self_signed("localhost");
    let cfg = TlsServerConfig::builder()
        .cert_chain_pem(id.cert_pem.as_bytes())
        .private_key_pem(id.key_pem.as_bytes())
        .versions(TlsVersion::Tls12, TlsVersion::Tls12)
        .session_tickets(false)
        .build()
        .unwrap();
    let (port, rx) = server(cfg, 2);
    let ccfg = TlsClientConfig::builder().system_roots(false).add_root_pem(id.cert_pem.as_bytes()).unwrap().build();
    assert!(!client(port, &ccfg).unwrap().resumed);
    let s1 = rx.recv().unwrap().unwrap();
    assert!(client(port, &ccfg).unwrap().resumed);
    let s2 = rx.recv().unwrap().unwrap();
    assert!(s2.resumed && s1.origin == s2.origin);
}

#[test]
fn resumption_disabled_or_unknown_ticket_falls_back_to_full_handshake() {
    let id = camel_tls::self_signed("localhost");
    let mk = || TlsServerConfig::builder().cert_chain_pem(id.cert_pem.as_bytes()).private_key_pem(id.key_pem.as_bytes()).build().unwrap();
    // client without a session cache
    let (port, rx) = server(mk(), 2);
    let ccfg = TlsClientConfig::builder().system_roots(false).add_root_pem(id.cert_pem.as_bytes()).unwrap().session_resumption(false).build();
    client(port, &ccfg).unwrap();
    assert!(!client(port, &ccfg).unwrap().resumed);
    rx.recv().unwrap().unwrap();
    assert!(!rx.recv().unwrap().unwrap().resumed);
    // a ticket from another server instance (different ticket key) is ignored
    let ccfg = TlsClientConfig::builder().system_roots(false).add_root_pem(id.cert_pem.as_bytes()).unwrap().build();
    let (port1, rx1) = server(mk(), 1);
    client(port1, &ccfg).unwrap();
    rx1.recv().unwrap().unwrap();
    let (port2, rx2) = server(mk(), 1);
    let s = TcpStream::connect(("127.0.0.1", port2)).unwrap();
    // reuse the first server's session under the second server's key
    let mut t = TlsStream::connect_with_session_key(s, "localhost", &ccfg, Some(&format!("localhost:{port1}"))).unwrap();
    assert!(!t.session_resumed());
    t.write_all(b"x\n").unwrap();
    let mut b = [0u8; 2];
    t.read_exact(&mut b).unwrap();
    assert!(!rx2.recv().unwrap().unwrap().resumed);
}

#[test]
fn resumption_after_hello_retry_request() {
    let id = camel_tls::self_signed("localhost");
    let cfg = TlsServerConfig::builder()
        .cert_chain_pem(id.cert_pem.as_bytes())
        .private_key_pem(id.key_pem.as_bytes())
        .groups(&[NamedGroup::Secp384r1])
        .build()
        .unwrap();
    let (port, rx) = server(cfg, 2);
    let ccfg = TlsClientConfig::builder()
        .system_roots(false)
        .add_root_pem(id.cert_pem.as_bytes())
        .unwrap()
        .groups(&[NamedGroup::X25519, NamedGroup::Secp384r1])
        .build();
    client(port, &ccfg).unwrap();
    rx.recv().unwrap().unwrap();
    assert!(client(port, &ccfg).unwrap().resumed, "PSK offered again in the second ClientHello");
    assert!(rx.recv().unwrap().unwrap().resumed);
}

#[test]
fn net_stream_resumes_per_host_and_port() {
    let id = camel_tls::self_signed("localhost");
    let cfg = TlsServerConfig::builder().cert_chain_pem(id.cert_pem.as_bytes()).private_key_pem(id.key_pem.as_bytes()).build().unwrap();
    let (port, rx) = server(cfg, 2);
    let ccfg = TlsClientConfig::builder().system_roots(false).add_root_pem(id.cert_pem.as_bytes()).unwrap().build();
    for i in 0..2 {
        let s = camel_tls::NetStream::connect_tls("localhost", port, &ccfg, Duration::from_secs(5)).unwrap();
        assert_eq!(s.session_resumed(), i == 1);
        assert_eq!(s.session_key(), Some(format!("localhost:{port}").as_str()));
        (&s).write_all(b"hi\n").unwrap();
        let mut line = String::new();
        BufReader::new(&s).read_line(&mut line).unwrap();
        assert_eq!(line, "HI\n");
        rx.recv().unwrap().unwrap();
    }
}
