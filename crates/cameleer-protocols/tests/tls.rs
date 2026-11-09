//! End-to-end TLS tests for cameleer-protocols: ftps (explicit + implicit, PROT P),
//! smtps/pop3s/imaps, STARTTLS for smtp/pop3/imap, wss and syslog over TLS,
//! each against an embedded TLS server with a self-signed certificate trusted via
//! `SslContextParameters`, plus negative tests (untrusted / wrong host name).

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_protocols::ftp::{FtpClient, FtpSecurity};
use cameleer_protocols::testing::{FtpServer, FtpsMode, ImapServer, Mailbox, Pop3Server, SmtpServer};
use cameleer_protocols::websocket::{WsConnection, WsMessage};
use cameleer_tls::camel_support::EndpointTls;
use cameleer_tls::testing;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    testing::bind_test_beans(&ctx);
    ctx
}

fn server_cfg() -> cameleer_tls::TlsServerConfig {
    testing::server_params().server_config().unwrap()
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-tls-proto-{}-{}-{}", name, std::process::id(), cameleer_core::util_time::now_millis()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn ssl_kind(e: &CamelError) -> bool {
    e.chain().any(|c| c.is_a("SSLException"))
}

// ------------------------------------------------------------------ ftps

#[test]
fn ftps_client_explicit_and_implicit_with_protected_data() {
    let root = temp_dir("ftps-client");
    let tls = Arc::new(testing::client_params().client_config().unwrap());
    for mode in [FtpsMode::Explicit, FtpsMode::Implicit] {
        let server = FtpServer::start_tls(&root, &[("admin", "secret")], server_cfg(), mode).unwrap();
        let sec = match mode {
            FtpsMode::Explicit => FtpSecurity::Explicit { tls: tls.clone(), server_name: "localhost".into() },
            FtpsMode::Implicit => FtpSecurity::Implicit { tls: tls.clone(), server_name: "localhost".into() },
        };
        for passive in [true, false] {
            let mut c = FtpClient::connect_secure("127.0.0.1", server.port(), Duration::from_secs(5), Duration::from_secs(10), &sec).unwrap();
            assert!(c.is_secure());
            c.login("admin", "secret").unwrap();
            c.protect_data(0, "P").unwrap();
            c.set_passive(passive);
            c.set_binary(true).unwrap();
            c.stor("secret.bin", &[1, 2, 3, 0, 255], false).unwrap();
            assert_eq!(c.retr("secret.bin").unwrap(), vec![1, 2, 3, 0, 255]);
            assert_eq!(c.list("").unwrap().iter().filter(|f| f.name == "secret.bin").count(), 1);
            c.dele("secret.bin").unwrap();
            c.quit();
        }
        let cmds = server.commands();
        assert!(cmds.iter().any(|c| c == "PROT P"), "{:?}", cmds);
        if mode == FtpsMode::Explicit {
            assert!(cmds.iter().any(|c| c == "AUTH TLS"), "{:?}", cmds);
        }
        server.stop();
    }
    // a plain client cannot log in to the explicit FTPS server
    let server = FtpServer::start_tls(&root, &[("admin", "secret")], server_cfg(), FtpsMode::Explicit).unwrap();
    let mut plain = FtpClient::connect("127.0.0.1", server.port(), Duration::from_secs(5), Duration::from_secs(5)).unwrap();
    assert!(plain.login("admin", "secret").is_err());
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

/// vsftpd `require_ssl_reuse=YES` style server: data connections must resume the
/// control connection's TLS session (TLS 1.3 tickets and TLS 1.2 tickets / session IDs).
#[test]
fn ftps_data_connections_reuse_the_control_session() {
    let root = temp_dir("ftps-reuse");
    for v in [cameleer_tls::TlsVersion::Tls13, cameleer_tls::TlsVersion::Tls12] {
        for tickets in [true, false] {
            let mut p = testing::server_params();
            p.min_version = Some(v);
            p.max_version = Some(v);
            let cfg = cameleer_tls::TlsServerConfig::builder()
                .certified_key(p.key_material().unwrap().unwrap())
                .versions(v, v)
                .session_tickets(tickets)
                .build()
                .unwrap();
            for mode in [FtpsMode::Explicit, FtpsMode::Implicit] {
                let server = FtpServer::start_tls(&root, &[("admin", "secret")], cfg.clone(), mode).unwrap();
                server.set_require_ssl_reuse(true);
                let label = format!("{v:?} tickets={tickets} {mode:?}");
                for (resumption, ok) in [(true, true), (false, false)] {
                    let mut cp = testing::client_params();
                    cp.session_resumption = Some(resumption);
                    let tls = Arc::new(cp.client_config().unwrap());
                    let sec = match mode {
                        FtpsMode::Explicit => FtpSecurity::Explicit { tls, server_name: "localhost".into() },
                        FtpsMode::Implicit => FtpSecurity::Implicit { tls, server_name: "localhost".into() },
                    };
                    let mut c = FtpClient::connect_secure("127.0.0.1", server.port(), Duration::from_secs(5), Duration::from_secs(10), &sec).unwrap();
                    c.login("admin", "secret").unwrap();
                    c.protect_data(0, "P").unwrap();
                    c.set_binary(true).unwrap();
                    let r = c.stor("reuse.bin", b"reused", false);
                    if !ok {
                        let e = r.expect_err(&format!("{label}: transfer without session reuse must fail"));
                        assert!(e.to_string().contains("522") || e.to_string().contains("reuse"), "{label}: {e}");
                        c.quit();
                        continue;
                    }
                    r.unwrap_or_else(|e| panic!("{label}: {e}"));
                    // several data connections in a row, uploads and downloads
                    for _ in 0..2 {
                        assert_eq!(c.retr("reuse.bin").unwrap_or_else(|e| panic!("{label}: {e}")), b"reused");
                        assert!(c.list("").unwrap().iter().any(|f| f.name == "reuse.bin"), "{label}");
                        c.stor("reuse.bin", b"reused", false).unwrap();
                    }
                    c.dele("reuse.bin").unwrap();
                    c.quit();
                }
                server.stop();
            }
        }
    }
    // the ftps component (cached endpoint TLS configuration) against a reuse-enforcing server
    let server = FtpServer::start_tls(&root, &[("admin", "secret")], server_cfg(), FtpsMode::Explicit).unwrap();
    server.set_require_ssl_reuse(true);
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("ftps://admin@localhost:{}/reuse?password=secret&passiveMode=true&sslContextParameters=#ssl", server.port());
    t.send_body_and_header(&uri, "one", "CamelFileName", "1.txt").unwrap();
    t.send_body_and_header(&uri, "two", "CamelFileName", "2.txt").unwrap();
    assert_eq!(fs::read_to_string(root.join("reuse/2.txt")).unwrap(), "two");
    server.stop();
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ftps_routes_explicit_and_implicit() {
    let root = temp_dir("ftps-routes");
    let explicit = FtpServer::start_tls(&root, &[("admin", "secret")], server_cfg(), FtpsMode::Explicit).unwrap();
    let implicit = FtpServer::start_tls(&root, &[("admin", "secret")], server_cfg(), FtpsMode::Implicit).unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    // producer over explicit FTPS, consumer over implicit FTPS (same directory)
    let producer = format!("ftps://admin@localhost:{}/out?password=secret&passiveMode=true&sslContextParameters=#ssl", explicit.port());
    t.send_body_and_header(&producer, "over tls", "CamelFileName", "a.txt").unwrap();
    assert_eq!(fs::read_to_string(root.join("out/a.txt")).unwrap(), "over tls");
    let consumer = format!(
        "ftps://admin@localhost:{}/out?password=secret&passiveMode=true&isImplicit=true&sslContextParameters=#ssl&delete=true&initialDelay=0&delay=50",
        implicit.port()
    );
    ctx.add_routes(|b| {
        b.from(&consumer).convert_body_to("String").to("mock:ftps");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:ftps").unwrap();
    mock.expected_bodies_received(vec!["over tls"]);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(implicit.commands().iter().any(|c| c == "PBSZ 0"));
    assert!(implicit.commands().iter().any(|c| c == "PROT P"));
    ctx.stop().unwrap();

    // untrusted certificate and host name mismatch are rejected
    let bad = format!("ftps://admin@localhost:{}/out?password=secret&passiveMode=true&sslContextParameters=#sslUntrusted&maximumReconnectAttempts=0", explicit.port());
    let err = t.send_body_and_header(&bad, "x", "CamelFileName", "b.txt").unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    let wrong = FtpServer::start_tls(&root, &[], testing::wrong_host_server_params().server_config().unwrap(), FtpsMode::Implicit).unwrap();
    let mismatch = format!("ftps://u@localhost:{}/out?password=p&isImplicit=true&sslContextParameters=#ssl&maximumReconnectAttempts=0", wrong.port());
    let err = t.send_body_and_header(&mismatch, "x", "CamelFileName", "c.txt").unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    assert!(!root.join("out/b.txt").exists() && !root.join("out/c.txt").exists());
    explicit.stop();
    implicit.stop();
    wrong.stop();
    let _ = fs::remove_dir_all(&root);
}

// ------------------------------------------------------------------ mail

#[test]
fn smtps_to_imaps_and_pop3s() {
    let mailbox = Mailbox::new();
    mailbox.set_password("bob@localhost", "secret");
    let smtps = SmtpServer::start_tls(&mailbox, server_cfg(), true).unwrap();
    let imaps = ImapServer::start_tls(&mailbox, server_cfg(), true).unwrap();
    let pop3s = Pop3Server::start_tls(&mailbox, server_cfg(), true).unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();
    let smtp_uri = format!("smtps://camel@localhost:{}?password=pw&to=bob@localhost&subject=TLS&sslContextParameters=#ssl", smtps.port());
    t.send_body(&smtp_uri, "secret mail 1").unwrap();
    t.send_body(&smtp_uri, "secret mail 2").unwrap();
    assert_eq!(mailbox.count("bob@localhost"), 2);

    let imap_uri = format!("imaps://bob@localhost:{}?password=secret&sslContextParameters=#ssl&initialDelay=0&delay=50&maxMessagesPerPoll=1", imaps.port());
    let pop3_uri = format!("pop3s://bob@localhost:{}?password=secret&sslContextParameters=#ssl&initialDelay=0&delay=50", pop3s.port());
    ctx.add_routes(|b| {
        b.from(&imap_uri).to("mock:imaps");
        b.from(&pop3_uri).to("mock:pop3s");
    })
    .unwrap();
    let imap_mock = ctx.mock_endpoint("mock:imaps").unwrap();
    imap_mock.expected_minimum_message_count(1);
    let pop3_mock = ctx.mock_endpoint("mock:pop3s").unwrap();
    pop3_mock.expected_minimum_message_count(2);
    ctx.start().unwrap();
    imap_mock.assert_is_satisfied().unwrap();
    pop3_mock.assert_is_satisfied().unwrap();
    assert_eq!(imap_mock.exchange(0).unwrap().header("Subject").unwrap().to_string(), "TLS");
    ctx.stop().unwrap();

    // untrusted / plain clients are rejected
    let err = t.send_body(&format!("smtps://camel@localhost:{}?to=bob@localhost&sslContextParameters=#sslUntrusted", smtps.port()), "x").unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    assert!(t.send_body(&format!("smtp://camel@localhost:{}?to=bob@localhost&connectionTimeout=3s", smtps.port()), "x").is_err());
    smtps.stop();
    imaps.stop();
    pop3s.stop();
}

#[test]
fn starttls_for_smtp_pop3_and_imap() {
    let mailbox = Mailbox::new();
    let smtp = SmtpServer::start_tls(&mailbox, server_cfg(), false).unwrap();
    let pop3 = Pop3Server::start_tls(&mailbox, server_cfg(), false).unwrap();
    let imap = ImapServer::start_tls(&mailbox, server_cfg(), false).unwrap();
    let ctx = context();
    let t = ctx.create_producer_template();

    // without STARTTLS the server refuses to accept mail
    let plain = format!("smtp://camel@localhost:{}?password=pw&to=alice@localhost", smtp.port());
    assert!(t.send_body(&plain, "plain").is_err());
    assert_eq!(mailbox.count("alice@localhost"), 0);

    let starttls = format!("smtp://camel@localhost:{}?password=pw&to=alice@localhost&mail.smtp.starttls.enable=true&sslContextParameters=#ssl", smtp.port());
    t.send_body(&starttls, "upgraded 1").unwrap();
    let required = format!("smtp://camel@localhost:{}?password=pw&to=alice@localhost&mail.smtp.starttls.required=true&sslContextParameters=#ssl", smtp.port());
    t.send_body(&required, "upgraded 2").unwrap();
    assert_eq!(mailbox.count("alice@localhost"), 2);

    let pop3_uri = format!("pop3://alice@localhost:{}?password=x&mail.pop3.starttls.enable=true&sslContextParameters=#ssl&initialDelay=0&delay=50", pop3.port());
    let imap_uri = format!("imap://alice@localhost:{}?password=x&mail.imap.starttls.required=true&sslContextParameters=#ssl&initialDelay=0&delay=50", imap.port());
    ctx.add_routes(|b| {
        b.from(&pop3_uri).to("mock:pop3");
        b.from(&imap_uri).to("mock:imap");
    })
    .unwrap();
    let pop3_mock = ctx.mock_endpoint("mock:pop3").unwrap();
    pop3_mock.expected_minimum_message_count(2);
    let imap_mock = ctx.mock_endpoint("mock:imap").unwrap();
    imap_mock.expected_minimum_message_count(2);
    ctx.start().unwrap();
    pop3_mock.assert_is_satisfied().unwrap();
    imap_mock.assert_is_satisfied().unwrap();
    ctx.stop().unwrap();

    // STARTTLS required against a plain server without STARTTLS fails
    let plain_server = SmtpServer::start(&mailbox).unwrap();
    let err = t
        .send_body(&format!("smtp://camel@localhost:{}?to=alice@localhost&mail.smtp.starttls.required=true&sslContextParameters=#ssl", plain_server.port()), "x")
        .unwrap_err();
    assert!(err.root_cause().message.contains("STARTTLS"), "{:?}", err);
    // ... while starttls.enable falls back to plain
    t.send_body(&format!("smtp://camel@localhost:{}?to=alice@localhost&mail.smtp.starttls.enable=true&sslContextParameters=#ssl", plain_server.port()), "fallback").unwrap();
    // hostname mismatch during STARTTLS is rejected
    let wrong = SmtpServer::start_tls(&mailbox, testing::wrong_host_server_params().server_config().unwrap(), false).unwrap();
    let err = t
        .send_body(&format!("smtp://camel@localhost:{}?to=alice@localhost&mail.smtp.starttls.required=true&sslContextParameters=#ssl", wrong.port()), "x")
        .unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    plain_server.stop();
    wrong.stop();
    smtp.stop();
    pop3.stop();
    imap.stop();
}

// ------------------------------------------------------------------ websocket

#[test]
fn wss_server_and_client() {
    let ctx = context();
    let server_uri = "websocket://127.0.0.1:0/secure?sslContextParameters=#ssl";
    ctx.add_routes(|b| {
        b.from(server_uri).transform(simple("wss echo: ${body}")).to(server_uri);
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, server_uri).unwrap();

    // raw client API
    let tls = EndpointTls::new(Arc::new(testing::client_params()));
    let c = WsConnection::connect_secure("localhost", port, "/secure", Duration::from_secs(5), &[], Some(&tls)).unwrap();
    assert!(c.is_secure());
    c.send_text("hi").unwrap();
    assert_eq!(c.read_message().unwrap(), Some(WsMessage::Text("wss echo: hi".into())));
    c.close(1000, "");

    // wss:// endpoint (consumer + producer share the client connection)
    let client_uri = format!("wss://localhost:{}/secure?sslContextParameters=#ssl", port);
    let ctx2 = context();
    ctx2.add_routes(|b| {
        b.from(&client_uri).to("mock:replies");
    })
    .unwrap();
    let mock = ctx2.mock_endpoint("mock:replies").unwrap();
    mock.expected_bodies_received(vec!["wss echo: from endpoint"]);
    ctx2.start().unwrap();
    ctx2.create_producer_template().send_body(&client_uri, "from endpoint").unwrap();
    mock.assert_is_satisfied().unwrap();
    ctx2.stop().unwrap();

    // a client that does not trust the server certificate fails
    let err = ctx2.create_producer_template().send_body(&format!("wss://localhost:{}/secure?sslContextParameters=#sslUntrusted", port), "x").unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    // plain ws against the TLS port fails the handshake
    assert!(WsConnection::connect("127.0.0.1", port, "/secure", Duration::from_secs(3)).is_err());
    ctx.stop().unwrap();
}

// ------------------------------------------------------------------ syslog

#[test]
fn syslog_over_tls() {
    let ctx = context();
    let uri = "syslog:tls://127.0.0.1:0?sslContextParameters=#ssl";
    ctx.add_routes(|b| {
        b.from(uri).to("mock:syslog");
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_protocols::local_port(&ctx, uri).unwrap();
    let mock = ctx.mock_endpoint("mock:syslog").unwrap();
    mock.expected_message_count(2);
    mock.expected_header_values_received_in_any_order("CamelSyslogSeverity", vec!["CRIT", "CRIT"]);
    let t = ctx.create_producer_template();
    let msg = "<34>Oct 11 22:14:15 mymachine su: 'su root' failed for lonvick on /dev/pts/8";
    t.send_body(&format!("syslog:tls://localhost:{}?sslContextParameters=#ssl", port), msg).unwrap();
    t.send_body(&format!("syslog:tcp://localhost:{}?ssl=true&sslContextParameters=#ssl", port), msg).unwrap();
    mock.assert_is_satisfied().unwrap();
    let err = t.send_body(&format!("syslog:tls://localhost:{}?sslContextParameters=#sslUntrusted", port), msg).unwrap_err();
    assert!(ssl_kind(&err), "{:?}", err);
    ctx.stop().unwrap();
}
