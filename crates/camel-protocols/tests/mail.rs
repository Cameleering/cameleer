use std::time::Duration;

use camel_core::prelude::*;
use camel_protocols::mail::mime::parse_message;
use camel_protocols::testing::{ImapServer, Mailbox, Pop3Server, SmtpServer};

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_protocols::register(&ctx);
    ctx
}

fn wait_until(f: impl Fn() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn smtp_to_pop3_round_trip() {
    let mailbox = Mailbox::new();
    mailbox.set_password("bob@localhost", "secret");
    let smtp = SmtpServer::start(&mailbox).unwrap();
    let pop3 = Pop3Server::start(&mailbox).unwrap();
    let ctx = context();
    let pop3_uri = format!("pop3://bob@127.0.0.1:{}?password=secret&delete=true&initialDelay=0&delay=50", pop3.port());
    ctx.add_routes(|b| {
        b.from(&pop3_uri).to("mock:inbox");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:inbox").unwrap();
    mock.expected_message_count(1);
    mock.expected_bodies_received(vec!["Hello Bob\n.a line starting with a dot\nBye"]);
    mock.expected_header_received("Subject", "Hello from Camel ✓");
    mock.expected_header_received("From", "camel@localhost");
    mock.expected_header_received("To", "bob@localhost");
    mock.expected_header_received("Cc", "carol@localhost");
    mock.expected_header_received("X-Order-Id", "4711");

    let t = ctx.create_producer_template();
    let smtp_uri = format!("smtp://camel@127.0.0.1:{}?password=pw&to=bob@localhost&subject=Default", smtp.port());
    t.send_body_and_headers(
        &smtp_uri,
        "Hello Bob\n.a line starting with a dot\nBye",
        vec![
            ("Subject", Value::from("Hello from Camel ✓")),
            ("Cc", Value::from("carol@localhost")),
            ("Bcc", Value::from("dave@localhost")),
            ("X-Order-Id", Value::from(4711)),
            ("CamelInternal", Value::from("not copied")),
        ],
    )
    .unwrap();
    assert_eq!(mailbox.count("carol@localhost"), 1);
    let dave = mailbox.messages("dave@localhost");
    assert_eq!(dave.len(), 1);
    let parsed = parse_message(dave[0].raw.as_bytes());
    assert!(parsed.header("Bcc").is_none(), "Bcc must not be in the headers");
    assert!(parsed.header("CamelInternal").is_none());
    assert_eq!(dave[0].envelope_from, "camel@localhost");
    assert!(mailbox.logins().contains(&("camel".to_string(), "pw".to_string())));

    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(wait_until(|| mailbox.count("bob@localhost") == 0), "message must be deleted");
    ctx.stop().unwrap();
    smtp.stop();
    pop3.stop();
}

#[test]
fn pop3_unseen_without_delete_and_options_from_uri() {
    let mailbox = Mailbox::new();
    let smtp = SmtpServer::start(&mailbox).unwrap();
    let pop3 = Pop3Server::start(&mailbox).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let smtp_uri = format!("smtp://127.0.0.1:{}?from=Shop <shop@localhost>&to=jane@localhost&replyTo=support@localhost", smtp.port());
    t.send_body_and_header(&smtp_uri, "first", "Subject", "one").unwrap();
    t.send_body_and_header(&smtp_uri, "second", "Subject", "two").unwrap();
    assert_eq!(mailbox.count("jane@localhost"), 2);

    let pop3_uri = format!("pop3://127.0.0.1:{}?username=jane&password=x&initialDelay=0&delay=20", pop3.port());
    ctx.add_routes(|b| {
        b.from(&pop3_uri).to("mock:jane");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:jane").unwrap();
    mock.expected_bodies_received(vec!["first", "second"]);
    mock.expected_header_received("Reply-To", "support@localhost");
    mock.expected_header_received("From", "Shop <shop@localhost>");
    mock.assert_is_satisfied().unwrap();
    // later polls must not deliver the same messages again
    let logins = mailbox.logins().len();
    assert!(wait_until(|| mailbox.logins().len() >= logins + 3));
    assert_eq!(mock.received_counter(), 2);
    assert_eq!(mailbox.count("jane@localhost"), 2);
    ctx.stop().unwrap();
    smtp.stop();
    pop3.stop();
}

#[test]
fn imap_consumer_with_attachments_and_move() {
    let mailbox = Mailbox::new();
    let smtp = SmtpServer::start(&mailbox).unwrap();
    let imap = ImapServer::start(&mailbox).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let smtp_uri = format!("smtp://127.0.0.1:{}?to=ann@localhost&subject=Report", smtp.port());
    let ex = t
        .request(&smtp_uri, |ex| {
            ex.set_body("See attached");
            let mut att = std::collections::BTreeMap::new();
            att.insert("data.bin".to_string(), Value::Bytes(vec![0, 1, 2, 3, 255]));
            att.insert("notes.txt".to_string(), Value::from("some notes"));
            ex.set_property("CamelAttachments", Value::Object(att));
        })
        .unwrap();
    assert!(ex.exception.is_none(), "{:?}", ex.exception);
    // a raw message delivered directly into the mailbox
    mailbox.deliver("ann@localhost", "Subject: old\r\n\r\nalready read\r\n");

    let imap_uri = format!("imap://ann@127.0.0.1:{}?password=pw&moveTo=Archive&initialDelay=0&delay=20", imap.port());
    ctx.add_routes(|b| {
        b.from(&imap_uri).to("mock:ann");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:ann").unwrap();
    mock.expected_bodies_received(vec!["See attached", "already read"]);
    mock.assert_is_satisfied().unwrap();
    let first = mock.exchange(0).unwrap();
    assert_eq!(first.header("Subject").unwrap().to_string(), "Report");
    let att = first.property("CamelAttachments").and_then(|v| v.as_object()).cloned().expect("attachments");
    assert_eq!(att.get("data.bin"), Some(&Value::Bytes(vec![0, 1, 2, 3, 255])));
    assert_eq!(att.get("notes.txt").map(|v| v.to_bytes()), Some(b"some notes".to_vec()));
    assert!(wait_until(|| mailbox.count("ann@localhost") == 0 && mailbox.count("ann@localhost/Archive") == 2));
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(mock.received_counter(), 2);
    ctx.stop().unwrap();
    smtp.stop();
    imap.stop();
}

#[test]
fn imap_marks_seen_and_failed_messages_stay_unseen() {
    let mailbox = Mailbox::new();
    mailbox.deliver("joe@localhost", "Subject: a\r\nFrom: x@y\r\n\r\nok\r\n");
    mailbox.deliver("joe@localhost", "Subject: b\r\nFrom: x@y\r\n\r\nboom\r\n");
    let imap = ImapServer::start(&mailbox).unwrap();
    let ctx = context();
    let uri = format!("imap://127.0.0.1:{}?username=joe@localhost&password=pw&initialDelay=0&delay=20", imap.port());
    ctx.add_routes(|b| {
        b.from(&uri).process(|ex: &mut Exchange| {
            if ex.body_as::<String>()? == "boom" {
                return Err(CamelError::illegal_state("failed"));
            }
            Ok(())
        }).to("mock:joe");
    })
    .unwrap();
    let mock = ctx.mock_endpoint("mock:joe").unwrap();
    mock.expected_bodies_received(vec!["ok"]);
    ctx.start().unwrap();
    mock.assert_is_satisfied().unwrap();
    assert!(wait_until(|| {
        let m = mailbox.messages("joe@localhost");
        m.len() == 2 && m[0].seen && !m[1].seen
    }));
    ctx.stop().unwrap();
    imap.stop();
}

#[test]
fn smtp_authentication_failure_and_unsupported_schemes() {
    let mailbox = Mailbox::new();
    mailbox.set_password("camel", "right");
    let smtp = SmtpServer::start(&mailbox).unwrap();
    let ctx = context();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let err = t.send_body(&format!("smtp://127.0.0.1:{}?username=camel&password=wrong&to=x@localhost", smtp.port()), "hi").unwrap_err();
    let cause = err.cause.as_deref().unwrap_or(&err);
    assert_eq!(cause.kind, "AuthenticationFailedException");
    assert_eq!(mailbox.count("x@localhost"), 0);
    // the TLS schemes resolve (end-to-end tests in tests/tls.rs)
    for uri in ["smtps://host", "pop3s://host", "imaps://host", "smtp://host?mail.smtp.starttls.enable=true"] {
        assert!(ctx.get_endpoint(uri).is_ok(), "{}", uri);
    }
    // pop3 cannot produce, smtp cannot consume
    assert!(ctx.get_endpoint("pop3://host").unwrap().create_producer().is_err());
    ctx.stop().unwrap();
    smtp.stop();
}
