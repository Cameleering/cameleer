//! Official-client cross-verification of camel-tls with Go's crypto/tls (an independent,
//! widely deployed TLS implementation), in both directions: every TLS 1.3 and TLS 1.2 suite,
//! the x25519 / P-256 / P-384 groups (and Go's default hybrid X25519MLKEM768 key share),
//! ALPN, mutual TLS (accepted, untrusted and missing client certificates) and session
//! resumption (TLS 1.3 PSK tickets, TLS 1.2 tickets / session ids).
//! Runs only with `CAMEL_OFFICIAL_CLIENTS=1` (see `scripts/official-clients.sh`).

use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use camel_tls::testing::official::{self, run, Run};
use camel_tls::{CipherSuite, ClientAuth, ClientSessionCache, NamedGroup, TlsClientConfig, TlsServerConfig, TlsStream, TlsVersion};

const RSA_CERT: &str = include_str!("../testdata/rsa_selfsigned.pem");
const RSA_KEY: &str = include_str!("../testdata/rsa2048.pkcs8.pem");

fn go_tls() -> &'static PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| official::go_build("go-tls"))
}

fn rsa_files(dir: &Path) -> (String, String) {
    let (c, k) = (dir.join("rsa.pem"), dir.join("rsa.key"));
    std::fs::write(&c, RSA_CERT).unwrap();
    std::fs::write(&k, RSA_KEY).unwrap();
    (c.display().to_string(), k.display().to_string())
}

fn version_name(v: TlsVersion) -> &'static str {
    match v {
        TlsVersion::Tls12 => "TLS 1.2",
        TlsVersion::Tls13 => "TLS 1.3",
    }
}

fn peer_cn(chain: &[Vec<u8>]) -> String {
    chain.first().and_then(|d| camel_tls::Certificate::from_der(d).ok()).and_then(|c| c.subject.common_name().map(String::from)).unwrap_or_else(|| "-".into())
}

/// A camel-tls line-echo server: answers each line with its view of the session.
fn server(cfg: TlsServerConfig) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let cfg = Arc::new(cfg);
    std::thread::spawn(move || {
        for sock in listener.incoming().flatten() {
            let cfg = cfg.clone();
            std::thread::spawn(move || {
                sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
                let Ok(mut tls) = TlsStream::accept(sock, &cfg) else { return };
                let mut line = Vec::new();
                let mut b = [0u8; 1];
                while tls.read(&mut b).map(|n| n == 1).unwrap_or(false) && b[0] != b'\n' {
                    line.push(b[0]);
                }
                let view = format!(
                    "version={} suite={} group={} alpn={} resumed={} peer={}\n",
                    version_name(tls.protocol_version()),
                    tls.cipher_suite().name(),
                    tls.named_group().map(|g| g.name()).unwrap_or("-"),
                    tls.alpn_protocol().map(|a| String::from_utf8_lossy(a).into_owned()).unwrap_or_else(|| "-".into()),
                    tls.session_resumed(),
                    peer_cn(tls.peer_certificates())
                );
                let _ = tls.write_all(view.as_bytes());
                let _ = tls.close();
            });
        }
    });
    port
}

fn ec_server(client_auth: ClientAuth, suites: Option<&[CipherSuite]>) -> u16 {
    let p = camel_tls::testing::mtls();
    let mut b = TlsServerConfig::builder().cert_chain_pem(p.server.cert_pem.as_bytes()).private_key_pem(p.server.key_pem.as_bytes()).alpn_protocols(&["h2", "http/1.1"]).client_auth(client_auth).add_client_root_pem(p.ca.cert_pem.as_bytes());
    if let Some(s) = suites {
        b = b.cipher_suites(s);
    }
    server(b.build().unwrap())
}

fn probe(port: u16, ca: &Path, extra: &[&str]) -> Run {
    let addr = format!("127.0.0.1:{}", port);
    let mut cmd = Command::new(go_tls());
    cmd.arg("probe").args(["-addr", &addr, "-ca"]).arg(ca).args(extra);
    let r = run(&mut cmd, None, Duration::from_secs(60));
    eprintln!("probe {} {:?}:\n{}", port, extra, r.stdout);
    r
}

#[test]
fn go_crypto_tls_client_against_camel_tls_server() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("go-tls-client");
    let ca = pem.ca.clone();
    // TLS 1.3: each suite (the server offers only that one; Go cannot restrict 1.3 suites)
    for s in [CipherSuite::Tls13Aes128GcmSha256, CipherSuite::Tls13Aes256GcmSha384, CipherSuite::Tls13Chacha20Poly1305Sha256] {
        let port = ec_server(ClientAuth::None, Some(&[s]));
        let r = probe(port, &ca, &["-min", "1.3"]).ok();
        assert_eq!(r.value("conn1:"), format!("version=TLS 1.3 suite={} alpn=- resumed=false peer=localhost", s.name()), "{}", r.stdout);
        assert!(r.value("conn1-server:").starts_with(&format!("version=TLS 1.3 suite={} ", s.name())), "{}", r.stdout);
    }
    let port = ec_server(ClientAuth::Want, None);
    // groups, in both versions; Go's default key shares (X25519MLKEM768 + x25519)
    for (curve, group) in [("X25519", "x25519"), ("P256", "secp256r1"), ("P384", "secp384r1")] {
        let r = probe(port, &ca, &["-max", "1.3", "-curve", curve]).ok();
        assert!(r.value("conn1-server:").contains(&format!(" group={} ", group)), "1.3 {}: {}", curve, r.stdout);
    }
    // TLS 1.2 (RFC 8422): the client's supported groups also constrain the ECDSA
    // certificate's curve, so a P-256 certificate needs P-256 in the list
    let r = probe(port, &ca, &["-max", "1.2", "-curve", "P256"]).ok();
    assert!(r.value("conn1-server:").contains(" group=secp256r1 "), "{}", r.stdout);
    let r = probe(port, &ca, &["-max", "1.2", "-curve", "X25519"]).ok();
    assert!(r.value("conn1-error:").contains("handshake failure"), "{}", r.stdout);
    let r = probe(port, &ca, &[]).ok();
    assert!(r.value("conn1-server:").starts_with("version=TLS 1.3 ") && r.value("conn1-server:").contains(" group=x25519 "), "{}", r.stdout);
    let r = probe(port, &ca, &["-curve", "X25519MLKEM768"]).ok();
    assert!(!r.value("conn1-error:").is_empty(), "no common group must fail: {}", r.stdout);
    // TLS 1.2 ECDSA suites
    for s in ["TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256", "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384", "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256"] {
        let r = probe(port, &ca, &["-max", "1.2", "-suite", s]).ok();
        assert_eq!(r.value("conn1:"), format!("version=TLS 1.2 suite={} alpn=- resumed=false peer=localhost", s), "{}", r.stdout);
    }
    // TLS 1.2 RSA suites against an RSA server certificate
    let (rsa_cert, _) = rsa_files(&pem.dir);
    let rsa_port = server(TlsServerConfig::from_pem(RSA_CERT.as_bytes(), RSA_KEY.as_bytes()).unwrap());
    for s in ["TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256", "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384", "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256"] {
        let r = probe(rsa_port, Path::new(&rsa_cert), &["-servername", "rsa.test", "-max", "1.2", "-suite", s]).ok();
        assert_eq!(r.value("conn1:"), format!("version=TLS 1.2 suite={} alpn=- resumed=false peer=rsa.test", s), "{}", r.stdout);
    }
    for (curve, group) in [("X25519", "x25519"), ("P256", "secp256r1"), ("P384", "secp384r1")] {
        let r = probe(rsa_port, Path::new(&rsa_cert), &["-servername", "rsa.test", "-max", "1.2", "-curve", curve]).ok();
        assert!(r.value("conn1-server:").contains(&format!(" group={} ", group)), "1.2 {}: {}", curve, r.stdout);
    }
    // TLS 1.3 with an RSA certificate (rsa_pss_rsae signatures)
    let r = probe(rsa_port, Path::new(&rsa_cert), &["-servername", "rsa.test"]).ok();
    assert!(r.value("conn1:").starts_with("version=TLS 1.3 "), "{}", r.stdout);
    // ALPN
    let r = probe(port, &ca, &["-alpn", "h2,http/1.1"]).ok();
    assert!(r.value("conn1:").contains(" alpn=h2 ") && r.value("conn1-server:").contains(" alpn=h2 "), "{}", r.stdout);
    let r = probe(port, &ca, &["-alpn", "http/1.1"]).ok();
    assert!(r.value("conn1:").contains(" alpn=http/1.1 "), "{}", r.stdout);
    // session resumption: Go's session cache, second connection resumes (both sides agree)
    for max in ["1.3", "1.2"] {
        let r = probe(port, &ca, &["-max", max, "-conns", "3"]).ok();
        assert!(r.value("conn1:").contains("resumed=false"), "{}", r.stdout);
        for c in ["conn2", "conn3"] {
            assert!(r.value(&format!("{}:", c)).contains("resumed=true"), "{} {}: {}", max, c, r.stdout);
            assert!(r.value(&format!("{}-server:", c)).contains("resumed=true"), "{} {}: {}", max, c, r.stdout);
        }
    }
    // mutual TLS
    let (cc, ck) = (pem.client_cert.display().to_string(), pem.client_key.display().to_string());
    let (uc, uk) = (pem.untrusted_cert.display().to_string(), pem.untrusted_key.display().to_string());
    for max in ["1.3", "1.2"] {
        let r = probe(port, &ca, &["-max", max, "-cert", &cc, "-key", &ck]).ok();
        assert!(r.value("conn1-server:").ends_with("peer=camel-client"), "{}", r.stdout);
        let r = probe(port, &ca, &["-max", max]).ok();
        assert!(r.value("conn1-server:").ends_with("peer=-"), "Want: anonymous clients allowed: {}", r.stdout);
    }
    let need = ec_server(ClientAuth::Need, None);
    for max in ["1.3", "1.2"] {
        let r = probe(need, &ca, &["-max", max, "-cert", &cc, "-key", &ck, "-conns", "2"]).ok();
        assert!(r.value("conn2-server:").contains("resumed=true") && r.value("conn2-server:").ends_with("peer=camel-client"), "resumed sessions keep the client identity: {}", r.stdout);
        let r = probe(need, &ca, &["-max", max]).ok();
        assert!(!r.value("conn1-error:").is_empty(), "Need without certificate: {}", r.stdout);
        let r = probe(need, &ca, &["-max", max, "-cert", &uc, "-key", &uk]).ok();
        assert!(!r.value("conn1-error:").is_empty(), "untrusted certificate: {}", r.stdout);
    }
    // a server certificate Go does not trust
    let other = camel_tls::self_signed("localhost");
    let wrong = pem.dir.join("other.pem");
    std::fs::write(&wrong, &other.cert_pem).unwrap();
    let r = probe(port, &wrong, &[]).ok();
    assert!(r.value("conn1-error:").contains("certificate"), "{}", r.stdout);
}

/// The Go net/http + crypto/tls server; stopped when dropped (stdin closed).
pub struct GoServer {
    child: std::process::Child,
    pub port: u16,
}

impl GoServer {
    pub fn start(args: &[&str]) -> GoServer {
        let dir = official::temp_dir("go-serve");
        let portfile = dir.join("port");
        let mut child = Command::new(go_tls()).arg("serve").arg("-portfile").arg(&portfile).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().expect("go-tls");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(p) = std::fs::read_to_string(&portfile) {
                return GoServer { child, port: p.trim().parse().unwrap() };
            }
            if let Ok(Some(st)) = child.try_wait() {
                let mut err = String::new();
                child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
                panic!("go server exited ({}): {}", st, err);
            }
            assert!(Instant::now() < deadline, "go server did not start");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for GoServer {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        std::thread::sleep(Duration::from_millis(50));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// GET /info over HTTP/1.1 with a camel-tls client; returns (Go's view, client's view).
fn info(port: u16, server_name: &str, cfg: &TlsClientConfig, session_key: Option<&str>) -> Result<(String, String), String> {
    let tcp = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut tls = TlsStream::connect_with_session_key(tcp, server_name, cfg, session_key).map_err(|e| e.to_string())?;
    tls.write_all(format!("GET /info HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", server_name).as_bytes()).map_err(|e| e.to_string())?;
    let mut resp = String::new();
    BufReader::new(&mut tls).read_to_string(&mut resp).map_err(|e| e.to_string())?;
    let body = resp.split("\r\n\r\n").nth(1).unwrap_or("").trim().to_string();
    let mine = format!("version={} suite={} group={} resumed={}", version_name(tls.protocol_version()), tls.cipher_suite().name(), tls.named_group().map(|g| g.name()).unwrap_or("-"), tls.session_resumed());
    Ok((body, mine))
}

#[test]
fn camel_tls_client_against_go_crypto_tls_server() {
    if !official::enabled() {
        return;
    }
    let pem = official::pem_files("go-tls-server");
    let p = camel_tls::testing::mtls();
    let (rsa_cert, rsa_key) = rsa_files(&pem.dir);
    let go = GoServer::start(&[
        "-cert",
        pem.server_cert.to_str().unwrap(),
        "-key",
        pem.server_key.to_str().unwrap(),
        "-rsacert",
        &rsa_cert,
        "-rsakey",
        &rsa_key,
        "-ca",
        pem.ca.to_str().unwrap(),
    ]);
    let base = || TlsClientConfig::builder().system_roots(false).add_root_pem(p.ca.cert_pem.as_bytes()).unwrap().alpn_protocols(&["http/1.1"]).session_resumption(false);
    // every suite
    let suites = [
        (TlsVersion::Tls13, CipherSuite::Tls13Aes128GcmSha256),
        (TlsVersion::Tls13, CipherSuite::Tls13Aes256GcmSha384),
        (TlsVersion::Tls13, CipherSuite::Tls13Chacha20Poly1305Sha256),
        (TlsVersion::Tls12, CipherSuite::EcdheEcdsaAes128GcmSha256),
        (TlsVersion::Tls12, CipherSuite::EcdheEcdsaAes256GcmSha384),
        (TlsVersion::Tls12, CipherSuite::EcdheEcdsaChacha20Poly1305),
    ];
    for (v, s) in suites {
        let cfg = base().versions(v, v).cipher_suites(&[s]).build();
        let (theirs, mine) = info(go.port, "localhost", &cfg, None).unwrap();
        assert!(theirs.starts_with(&format!("proto=HTTP/1.1 version={} suite={} alpn=http/1.1 resumed=false peer=-", version_name(v), s.name())), "{:?}: {}", s, theirs);
        assert!(mine.starts_with(&format!("version={} suite={}", version_name(v), s.name())), "{}", mine);
    }
    // RSA suites (Go picks its RSA certificate for an RSA-only TLS 1.2 client)
    for s in [CipherSuite::EcdheRsaAes128GcmSha256, CipherSuite::EcdheRsaAes256GcmSha384, CipherSuite::EcdheRsaChacha20Poly1305] {
        let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(RSA_CERT.as_bytes()).unwrap().versions(TlsVersion::Tls12, TlsVersion::Tls12).cipher_suites(&[s]).session_resumption(false).build();
        let (theirs, _) = info(go.port, "rsa.test", &cfg, None).unwrap();
        assert!(theirs.contains(&format!("version=TLS 1.2 suite={} ", s.name())), "{:?}: {}", s, theirs);
    }
    // every group, both versions
    for v in [TlsVersion::Tls13, TlsVersion::Tls12] {
        for g in NamedGroup::ALL {
            let cfg = base().versions(v, v).groups(&[g]).build();
            let (_, mine) = info(go.port, "localhost", &cfg, None).unwrap();
            assert!(mine.contains(&format!("group={}", g.name())), "{:?} {:?}: {}", v, g, mine);
        }
    }
    // resumption against Go's ticket keys (TLS 1.3 PSK, TLS 1.2 tickets)
    for v in [TlsVersion::Tls13, TlsVersion::Tls12] {
        let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(p.ca.cert_pem.as_bytes()).unwrap().alpn_protocols(&["http/1.1"]).versions(v, v).session_cache(Arc::new(ClientSessionCache::new(8))).build();
        let key = format!("go-{:?}", v);
        let (first, _) = info(go.port, "localhost", &cfg, Some(&key)).unwrap();
        assert!(first.contains("resumed=false"), "{}", first);
        for _ in 0..2 {
            let (theirs, mine) = info(go.port, "localhost", &cfg, Some(&key)).unwrap();
            assert!(theirs.contains("resumed=true") && mine.ends_with("resumed=true"), "{:?}: {} / {}", v, theirs, mine);
        }
    }
    // mutual TLS: Go verifies the client certificate
    for v in [TlsVersion::Tls13, TlsVersion::Tls12] {
        let cfg = base().versions(v, v).client_cert_pem(p.client.cert_pem.as_bytes(), p.client.key_pem.as_bytes()).unwrap().build();
        let (theirs, _) = info(go.port, "localhost", &cfg, None).unwrap();
        assert!(theirs.contains("peer=camel-client"), "{}", theirs);
        let cfg = base().versions(v, v).client_cert_pem(p.untrusted_client.cert_pem.as_bytes(), p.untrusted_client.key_pem.as_bytes()).unwrap().build();
        assert!(info(go.port, "localhost", &cfg, None).is_err(), "Go must reject an untrusted client certificate ({:?})", v);
    }
    // hostname verification and an untrusted server
    let cfg = base().build();
    let e = info(go.port, "wrong.example", &cfg, None).unwrap_err();
    assert!(e.to_lowercase().contains("name") || e.to_lowercase().contains("host"), "{}", e);
    let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(camel_tls::self_signed("localhost").cert_pem.as_bytes()).unwrap().build();
    assert!(info(go.port, "localhost", &cfg, None).is_err());
    // ALPN h2 offered: Go selects h2 (the stream then speaks HTTP/2; just check the handshake)
    let cfg = base().alpn_protocols(&["h2", "http/1.1"]).build();
    let tls = TlsStream::connect(TcpStream::connect(("127.0.0.1", go.port)).unwrap(), "localhost", &cfg).unwrap();
    assert_eq!(tls.alpn_protocol(), Some(&b"h2"[..]));
}
