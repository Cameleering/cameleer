//! PostgreSQL `sslmode` (SSLRequest negotiation + TLS) against the embedded server.

mod common;

use cameleer_sql::pg::{PgConfig, PgConnection, SslMode};
use cameleer_sql::testing::{PgAuth, PgTestServer};
use cameleer_sql::{datasource_from_url, Connection};
use cameleer_tls::testing;
use common::*;

fn tls_server(params: cameleer_tls::SslContextParameters) -> PgTestServer {
    PgTestServer::start_tls(&unique("pgtls"), PgAuth::ScramSha256, &[("camel", "s3cret")], params.server_config().unwrap()).unwrap()
}

fn url(s: &PgTestServer, host: &str, extra: &str) -> String {
    format!("jdbc:postgresql://{}:{}/{}?user=camel&password=s3cret&{}", host, s.port(), s.database(), extra)
}

#[test]
fn sslmode_require_and_verify_full() {
    let s = tls_server(testing::server_params());
    let (ca, _) = testing::write_localhost_pem_files("pg");

    // sslmode=disable: the server refuses plain sessions
    let err = PgConnection::connect(&PgConfig::from_url(&url(&s, "localhost", "sslmode=disable")).unwrap()).err().expect("plain refused");
    assert!(err.message.contains("SSL off"), "{}", err.message);

    // require (no verification), verify-ca and verify-full with the CA file
    for extra in ["sslmode=require".to_string(), format!("sslmode=verify-ca&sslrootcert={}", ca.display()), format!("sslmode=verify-full&sslrootcert={}", ca.display())] {
        let cfg = PgConfig::from_url(&url(&s, "localhost", &extra)).unwrap();
        let mut c = PgConnection::connect(&cfg).unwrap_or_else(|e| panic!("{}: {}", extra, e));
        let r = c.execute("SELECT 40 + 2 AS answer", &[]).unwrap();
        assert_eq!(r.scalar().and_then(|v| v.as_i64()), Some(42), "{}", extra);
    }

    // a pooled DataSource over TLS
    let ds = datasource_from_url(&url(&s, "127.0.0.1", &format!("ssl=true&sslrootcert={}", ca.display()))).unwrap();
    exec(&ds, "CREATE TABLE t (id INT, name VARCHAR(20))");
    exec(&ds, "INSERT INTO t VALUES (1, 'secure')");
    assert_eq!(count(&ds, "SELECT COUNT(*) FROM t"), 1);

    // programmatic SslContextParameters
    let mut cfg = PgConfig::from_url(&url(&s, "localhost", "")).unwrap();
    cfg.ssl_mode = SslMode::VerifyFull;
    cfg.ssl_params = Some(testing::client_params());
    PgConnection::connect(&cfg).unwrap();
}

#[test]
fn sslmode_failures() {
    let s = tls_server(testing::wrong_host_server_params());
    // verify-full: the certificate is for wrong.example
    let mut cfg = PgConfig::from_url(&url(&s, "localhost", "sslmode=verify-full")).unwrap();
    cfg.ssl_params = Some(testing::client_params());
    let err = PgConnection::connect(&cfg).err().expect("hostname mismatch");
    assert!(err.chain().any(|c| c.is_a("SSLException")), "{:?}", err);
    // verify-ca accepts it (chain only)
    cfg.ssl_mode = SslMode::VerifyCa;
    PgConnection::connect(&cfg).unwrap();
    // untrusted certificate
    cfg.ssl_mode = SslMode::VerifyFull;
    cfg.ssl_params = Some(testing::untrusting_client_params());
    let err = PgConnection::connect(&cfg).err().expect("untrusted");
    assert!(err.chain().any(|c| c.is_a("SSLException")), "{:?}", err);

    // a server without TLS: require fails, prefer falls back to plain
    let plain = PgTestServer::start(&unique("pgplain"), PgAuth::Md5, &[("camel", "s3cret")]).unwrap();
    let err = PgConnection::connect(&PgConfig::from_url(&url(&plain, "127.0.0.1", "sslmode=require")).unwrap()).err().expect("no ssl");
    assert!(err.message.contains("does not support SSL"), "{}", err.message);
    PgConnection::connect(&PgConfig::from_url(&url(&plain, "127.0.0.1", "sslmode=prefer")).unwrap()).unwrap();
    assert!(PgConfig::from_url(&url(&plain, "127.0.0.1", "sslmode=bogus")).is_err());
}

/// Client certificates (`sslcert` + `sslkey`, encrypted keys with `sslpassword`, PKCS#12
/// `sslkey`) against a server requiring client authentication.
#[test]
fn ssl_client_certificates() {
    use cameleer_tls::keystore::KeyStore;
    let pki = testing::mtls();
    let s = tls_server(testing::mtls_server_params(cameleer_tls::ClientAuth::Need));
    let dir = std::env::temp_dir().join(format!("cameleer-sql-mtls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = |name: &str, data: &[u8]| {
        let p = dir.join(name);
        std::fs::write(&p, data).unwrap();
        p.display().to_string()
    };
    let ca = file("ca.pem", pki.ca.cert_pem.as_bytes());
    let cert = file("client.crt", pki.client.cert_pem.as_bytes());
    let key = file("client.key", pki.client.key_pem.as_bytes());
    let enc = cameleer_tls::pem::encode_pem("ENCRYPTED PRIVATE KEY", &cameleer_tls::pbe::encrypt_private_key_info(&pki.client.key.to_pkcs8(), "kpw"));
    let enc_key = file("client-enc.key", enc.as_bytes());
    let p12 = file("client.p12", &KeyStore::from_key_entry("client", pki.client.key.clone(), vec![pki.client.cert_der.clone()]).to_pkcs12("p12pw"));
    let base = format!("sslmode=verify-full&sslrootcert={}", ca);
    for extra in [
        format!("{}&sslcert={}&sslkey={}", base, cert, key),
        format!("{}&sslcert={}&sslkey={}&sslpassword=kpw", base, cert, enc_key),
        format!("{}&sslkey={}&sslpassword=p12pw", base, p12),
    ] {
        let cfg = PgConfig::from_url(&url(&s, "localhost", &extra)).unwrap();
        let mut c = PgConnection::connect(&cfg).unwrap_or_else(|e| panic!("{}: {}", extra, e));
        assert_eq!(c.execute("SELECT 1 AS one", &[]).unwrap().scalar().and_then(|v| v.as_i64()), Some(1));
    }
    // without a client certificate (or with a wrong key password) the connection fails
    assert!(PgConnection::connect(&PgConfig::from_url(&url(&s, "localhost", &base)).unwrap()).is_err());
    let bad = format!("{}&sslcert={}&sslkey={}&sslpassword=nope", base, cert, enc_key);
    assert!(PgConnection::connect(&PgConfig::from_url(&url(&s, "localhost", &bad)).unwrap()).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
