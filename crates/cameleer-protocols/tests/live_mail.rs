//! Live interop: `smtp`/`smtps` into Postfix (local delivery to a Maildir) and `imap`/`imaps`/
//! `pop3`/`pop3s` out of Dovecot (`scripts/live-interop.sh`). Runs only with
//! `CAMEL_LIVE_INTEROP=1`.
//!
//! All scenarios share the one test mailbox (`camelmail@localhost`), so they run in sequence
//! inside a single test.

use std::path::PathBuf;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_tls::testing::live;

const SMTP: u16 = 2525;
const SMTPS: u16 = 4465;
const IMAP: u16 = 11143;
const IMAPS: u16 = 11993;
const POP3: u16 = 11110;
const POP3S: u16 = 11995;
const RCPT: &str = "camelmail@localhost";

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_protocols::register(&ctx);
    live::bind_beans(&ctx);
    ctx
}

fn maildir() -> PathBuf {
    PathBuf::from("/home/camelmail/Maildir")
}

/// Messages in INBOX (`new` + `cur`) or in a folder (`.Archive`).
fn count(folder: &str) -> usize {
    let base = if folder.is_empty() { maildir() } else { maildir().join(format!(".{}", folder)) };
    ["new", "cur"].iter().map(|d| std::fs::read_dir(base.join(d)).map(|r| r.count()).unwrap_or(0)).sum()
}

fn empty_mailbox() {
    for folder in ["", ".Archive", ".Trash"] {
        for d in ["new", "cur"] {
            if let Ok(r) = std::fs::read_dir(maildir().join(folder).join(d)) {
                for e in r.flatten() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(std::time::Instant::now() < deadline, "timed out waiting for {}", what);
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn live_postfix_dovecot_mail_round_trips() {
    if !live::enabled() {
        return;
    }
    assert!(maildir().exists(), "run scripts/live-interop.sh setup first");
    empty_mailbox();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let pw = live::PASSWORD;

    // 1. plain SMTP (no auth, from mynetworks) -> Postfix local delivery -> POP3 with delete
    let smtp = format!("smtp://127.0.0.1:{}?from=camel@localhost&to={}&subject=Default", SMTP, RCPT);
    t.send_body_and_headers(
        &smtp,
        "Hello Bob\n.a line starting with a dot\nBye",
        vec![("Subject", Value::from("Hello from Camel ✓")), ("Cc", Value::from(RCPT)), ("X-Order-Id", Value::from(4711)), ("CamelInternal", Value::from("not copied"))],
    )
    .unwrap();
    // To + Cc are the same mailbox: Postfix delivers one copy per envelope recipient (deduplicated)
    wait_until("postfix delivery", || count("") >= 1);
    let pop3 = format!("pop3://camelmail@127.0.0.1:{}?password={}&delete=true&initialDelay=0&delay=100", POP3, pw);
    ctx.add_routes(move |b| {
        b.from(&pop3).route_id("pop3").to("mock:pop3");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:pop3").unwrap();
    m.expected_minimum_message_count(1);
    m.assert_is_satisfied_within(Duration::from_secs(20)).unwrap();
    let ex = m.exchange(0).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap().trim_end(), "Hello Bob\n.a line starting with a dot\nBye");
    let s = |h: &str| ex.header(h).map(|v| v.to_string()).unwrap_or_default();
    assert_eq!(s("Subject"), "Hello from Camel ✓");
    assert_eq!(s("From"), "camel@localhost");
    assert_eq!(s("To"), RCPT);
    assert_eq!(s("Cc"), RCPT);
    assert_eq!(s("X-Order-Id"), "4711");
    assert!(ex.header("CamelInternal").is_none());
    assert!(s("Received").contains("Postfix") || s("Return-Path").contains("camel@localhost"), "{:?}", ex.message.headers);
    wait_until("pop3 delete", || count("") == 0);
    ctx.stop_route("pop3").unwrap();
    ctx.remove_route("pop3").unwrap();

    // 2. SMTP AUTH (Dovecot SASL) over STARTTLS with attachments -> IMAP moveTo=Archive
    let starttls = format!("smtp://camel@localhost:{}?password={}&from=camel@localhost&to={}&subject=Report&mail.smtp.starttls.required=true&sslContextParameters=#liveSsl", SMTP, pw, RCPT);
    let ex = t
        .request(&starttls, |ex| {
            ex.set_body("See attached");
            let mut att = cameleer_core::value::ObjectMap::new();
            att.insert("data.bin".to_string(), Value::Bytes(vec![0, 1, 2, 3, 255]));
            att.insert("notes.txt".to_string(), Value::from("some notes ✓"));
            ex.set_property("CamelAttachments", Value::Object(att));
        })
        .unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    // wrong SMTP password
    let err = t.send_body(&format!("smtp://camel@localhost:{}?password=wrong&to={}&mail.smtp.starttls.required=true&sslContextParameters=#liveSsl", SMTP, RCPT), "x").unwrap_err();
    assert_eq!(err.cause.as_deref().unwrap_or(&err).kind, "AuthenticationFailedException", "{:?}", err);
    wait_until("starttls delivery", || count("") == 1);
    let imap = format!("imap://camelmail@127.0.0.1:{}?password={}&moveTo=Archive&initialDelay=0&delay=100", IMAP, pw);
    ctx.add_routes(move |b| {
        b.from(&imap).route_id("imap").to("mock:imap");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:imap").unwrap();
    m.expected_message_count(1);
    m.assert_is_satisfied_within(Duration::from_secs(20)).unwrap();
    let ex = m.exchange(0).unwrap();
    assert_eq!(ex.body_as::<String>().unwrap().trim_end(), "See attached");
    assert_eq!(ex.header("Subject").unwrap().to_string(), "Report");
    let att = ex.property("CamelAttachments").and_then(|v| v.as_object()).cloned().expect("attachments");
    assert_eq!(att.get("data.bin"), Some(&Value::Bytes(vec![0, 1, 2, 3, 255])));
    assert_eq!(att.get("notes.txt").map(|v| String::from_utf8_lossy(&v.to_bytes()).to_string()), Some("some notes ✓".to_string()));
    wait_until("moved to Archive", || count("") == 0 && count("Archive") == 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(m.received_counter(), 1);
    ctx.stop_route("imap").unwrap();
    ctx.remove_route("imap").unwrap();

    // 3. SMTPS (implicit TLS) -> IMAPS: seen marking, failed messages stay unseen
    let smtps = format!("smtps://camel@localhost:{}?password={}&from=camel@localhost&to={}&sslContextParameters=#liveSsl", SMTPS, pw, RCPT);
    t.send_body_and_header(&smtps, "ok", "Subject", "a").unwrap();
    wait_until("smtps delivery a", || count("") == 1);
    t.send_body_and_header(&smtps, "boom", "Subject", "b").unwrap();
    wait_until("smtps delivery b", || count("") == 2);
    let imaps = format!("imaps://camelmail@localhost:{}?password={}&sslContextParameters=#liveSsl&initialDelay=0&delay=100", IMAPS, pw);
    ctx.add_routes(move |b| {
        b.from(&imaps)
            .route_id("imaps")
            .process(|ex: &mut Exchange| {
                if ex.body_as::<String>()?.trim_end() == "boom" {
                    return Err(CamelError::illegal_state("failed"));
                }
                Ok(())
            })
            .to("mock:imaps");
    })
    .unwrap();
    let m = ctx.mock_endpoint("mock:imaps").unwrap();
    m.expected_message_count(1);
    m.assert_is_satisfied_within(Duration::from_secs(20)).unwrap();
    assert_eq!(m.exchange(0).unwrap().body_as::<String>().unwrap().trim_end(), "ok");
    // Dovecot keeps flags in the Maildir file names (`:2,S` = seen)
    let seen = || -> (usize, usize) {
        let files: Vec<String> = std::fs::read_dir(maildir().join("cur")).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default();
        let s = files.iter().filter(|f| f.rsplit(":2,").next().is_some_and(|fl| fl.contains('S'))).count();
        (s, count("") - s)
    };
    wait_until("one seen, one unseen", || seen() == (1, 1));
    ctx.stop_route("imaps").unwrap();
    ctx.remove_route("imaps").unwrap();

    // 4. POP3 STLS and POP3S read the two remaining messages (no delete: still there afterwards)
    for (n, uri) in [
        ("pop3-stls", format!("pop3://camelmail@localhost:{}?password={}&mail.pop3.starttls.required=true&sslContextParameters=#liveSsl&initialDelay=0&delay=100", POP3, pw)),
        ("pop3s", format!("pop3s://camelmail@localhost:{}?password={}&sslContextParameters=#liveSsl&initialDelay=0&delay=100", POP3S, pw)),
    ] {
        let mock_uri = format!("mock:{}", n);
        let mu = mock_uri.clone();
        ctx.add_routes(move |b| {
            b.from(&uri).route_id(n).to(&mu);
        })
        .unwrap();
        let m = ctx.mock_endpoint(&mock_uri).unwrap();
        m.expected_minimum_message_count(2);
        m.assert_is_satisfied_within(Duration::from_secs(20)).unwrap_or_else(|e| panic!("{}: {}", n, e));
        ctx.stop_route(n).unwrap();
        ctx.remove_route(n).unwrap();
    }
    assert_eq!(count(""), 2);
    // a wrong POP3 password
    let bad = format!("pop3://camelmail@127.0.0.1:{}?password=wrong&initialDelay=0&delay=100", POP3);
    ctx.add_routes(move |b| {
        b.from(&bad).route_id("bad").to("mock:bad");
    })
    .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(ctx.mock_endpoint("mock:bad").unwrap().received_counter(), 0);
    ctx.stop().unwrap();
    empty_mailbox();
}
