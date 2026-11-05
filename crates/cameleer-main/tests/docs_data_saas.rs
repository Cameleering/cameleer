//! Tests for the Google Cloud, data store, SaaS and AI component pages under `docs/components/`
//! and `docs/guides/ai-components.md`. Every YAML example is extracted from the markdown (never
//! duplicated here), parsed and its endpoint URIs resolved with every component registered; the
//! examples that have an embedded server or mock run end to end below.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use cameleer_cloud::testing::GcpMock;
use cameleer_cloud::testing::gcp::PROJECT;
use cameleer_core::prelude::*;
use cameleer_saas::testing::{GithubMock, JiraMock, OpenAiMock, SlackMock, TelegramMock, TwilioMock};
use h::*;

mod h {
    //! Shared helpers: markdown example extraction, placeholder substitution, endpoint resolution.
    #![allow(dead_code)]

    use std::time::{Duration, Instant};

    /// The pages owned by this test file (paths under `docs/`).
    pub const PAGES: &[&str] = &[
        "components/google-bigquery.md",
        "components/google-bigquery-sql.md",
        "components/google-calendar.md",
        "components/google-calendar-stream.md",
        "components/google-drive.md",
        "components/google-firestore.md",
        "components/google-functions.md",
        "components/google-mail.md",
        "components/google-mail-stream.md",
        "components/google-pubsub.md",
        "components/google-pubsub-lite.md",
        "components/google-secret-manager.md",
        "components/google-sheets.md",
        "components/google-sheets-stream.md",
        "components/google-speech-to-text.md",
        "components/google-storage.md",
        "components/google-text-to-speech.md",
        "components/google-vertexai.md",
        "components/sql.md",
        "components/sql-stored.md",
        "components/jdbc.md",
        "components/cql.md",
        "components/mongodb.md",
        "components/mongodb-gridfs.md",
        "components/couchdb.md",
        "components/elasticsearch.md",
        "components/opensearch.md",
        "components/spring-redis.md",
        "components/github.md",
        "components/jira.md",
        "components/slack.md",
        "components/telegram.md",
        "components/twilio.md",
        "components/openai.md",
        "guides/ai-components.md",
    ];

    /// Text of a page under `docs/`.
    pub fn doc(page: &str) -> String {
        let path = format!("{}/../../docs/{}", env!("CARGO_MANIFEST_DIR"), page);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {}", path, e)).replace("\r\n", "\n")
    }

    /// All `(id, lang, code)` blocks of a page that are preceded by an `<!-- example: id -->` marker,
    /// plus every fenced block as `(marker-or-empty, lang, code)`.
    pub fn blocks(text: &str) -> Vec<(String, String, String)> {
        let lines: Vec<&str> = text.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let l = lines[i];
            if let Some(lang) = l.strip_prefix("```") {
                let lang = lang.trim().to_string();
                let marker = if i > 0 {
                    lines[i - 1].trim().strip_prefix("<!-- example:").and_then(|s| s.strip_suffix("-->")).map(|s| s.trim().to_string()).unwrap_or_default()
                } else {
                    String::new()
                };
                let mut code = String::new();
                i += 1;
                while i < lines.len() && !lines[i].starts_with("```") {
                    code.push_str(lines[i]);
                    code.push('\n');
                    i += 1;
                }
                out.push((marker, lang, code));
            }
            i += 1;
        }
        out
    }

    /// The fenced block directly after `<!-- example: id -->` in `page`.
    pub fn example(page: &str, id: &str) -> String {
        let text = doc(page);
        blocks(&text).into_iter().find(|(m, _, _)| m == id).map(|(_, _, c)| c).unwrap_or_else(|| panic!("{}: no example {}", page, id))
    }

    /// Replace `from` with `to` (each must occur).
    pub fn subst(text: &str, replace: &[(&str, &str)]) -> String {
        let mut text = text.to_string();
        for (from, to) in replace {
            assert!(text.contains(from), "example does not contain {}:\n{}", from, text);
            text = text.replace(from, to);
        }
        text
    }

    /// Replace every `{{env:NAME:default}}` with the value given in `env`, else the default, and
    /// every `{{env:NAME}}` with the value given in `env`, else `dummy-NAME`.
    pub fn env_subst(text: &str, env: &[(&str, &str)]) -> String {
        let mut out = String::new();
        let mut rest = text;
        while let Some(start) = rest.find("{{env:") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 6..];
            let end = after.find("}}").expect("unterminated {{env:");
            let inner = &after[..end];
            let (name, default) = match inner.split_once(':') {
                Some((n, d)) => (n, Some(d)),
                None => (inner, None),
            };
            let value = env.iter().find(|(k, _)| *k == name).map(|(_, v)| v.to_string()).or(default.map(|d| d.to_string())).unwrap_or_else(|| format!("dummy-{}", name));
            out.push_str(&value);
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        out
    }

    fn unquote(s: &str) -> String {
        let s = s.trim();
        let s = s.strip_prefix("- ").unwrap_or(s).trim();
        if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2) || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2) {
            s[1..s.len() - 1].to_string()
        } else {
            s.to_string()
        }
    }

    /// The static endpoint URIs of a YAML route (`uri:`, `to:`, `toD:`, `wireTap:` scalar values);
    /// dynamic ones (with `${`) are skipped.
    pub fn endpoint_uris(yaml: &str) -> Vec<String> {
        let mut out = Vec::new();
        for line in yaml.lines() {
            let t = line.trim().trim_start_matches("- ").trim();
            for key in ["uri:", "to:", "toD:", "wireTap:"] {
                if let Some(v) = t.strip_prefix(key) {
                    let v = unquote(v);
                    if v.is_empty() || v.contains("${") || !v.contains(':') {
                        continue;
                    }
                    out.push(v);
                }
            }
        }
        out
    }

    /// Parse the YAML example and resolve every static endpoint URI in a context with every
    /// component, after placeholder substitution. Returns the resolved URIs.
    pub fn resolve(yaml: &str, env: &[(&str, &str)]) -> Vec<String> {
        let yaml = env_subst(yaml, env);
        let main = cameleer_main::new_main();
        let ctx = main.context();
        // `jdbc:` endpoints look their data source up in the registry (as `camel.datasource.url`
        // binds it at runtime)
        cameleer_sql::bind_datasource(ctx, "dataSource", cameleer_sql::datasource_from_url("jdbc:rustsql:mem:docs_resolve").unwrap());
        cameleer_yaml::load_routes(ctx, &yaml).unwrap_or_else(|e| panic!("yaml does not parse: {}\n{}", e, yaml));
        let uris = endpoint_uris(&yaml);
        for uri in &uris {
            if let Err(e) = ctx.get_endpoint(uri) {
                panic!("endpoint {} does not resolve: {}", uri, e);
            }
        }
        uris
    }

    /// Start the YAML text with `new_main` (plus optional extra Rust routes).
    pub fn start_yaml(name: &str, yaml: String, configure: Option<Box<dyn FnOnce(&cameleer_core::prelude::CamelContext) -> cameleer_core::prelude::Result<()>>>) -> cameleer_main::Main {
        let mut main = cameleer_main::new_main();
        main.add_route_source(format!("{}.camel.yaml", name), yaml);
        if let Some(f) = configure {
            main.configure(f);
        }
        main.start().unwrap_or_else(|e| panic!("{} does not start: {}", name, e));
        main
    }

    pub fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }

    pub fn wait_for(what: &str, f: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < deadline, "timed out waiting for {}", what);
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// Every yaml block on every page is marked, uniquely named, parses, and its endpoints resolve.
#[test]
fn all_examples_parse_and_resolve() {
    let mut ids = std::collections::BTreeSet::new();
    let mut count = 0;
    for page in PAGES {
        let text = doc(page);
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        assert!(first.starts_with("# "), "{}: first line must be the H1", page);
        let answer = text.lines().skip_while(|l| !l.starts_with("# ")).skip(1).find(|l| !l.trim().is_empty()).unwrap_or("");
        assert!(answer.starts_with("> "), "{}: the line under the H1 must be the answer blockquote", page);
        let mut n = 0;
        for (marker, lang, code) in blocks(&text) {
            if lang != "yaml" {
                continue;
            }
            assert!(!marker.is_empty(), "{}: unmarked yaml block:\n{}", page, code);
            assert!(ids.insert(marker.clone()), "{}: duplicate example id {}", page, marker);
            let uris = resolve(&code, &[]);
            assert!(!uris.is_empty(), "{}: {} has no endpoint", page, marker);
            count += 1;
            n += 1;
        }
        assert!(n >= 1, "{}: no example", page);
    }
    assert!(count >= PAGES.len(), "only {} examples", count);
}

// ===================================================================
// Google Cloud
// ===================================================================

const GCP_SCHEMES: &[&str] = &[
    "google-storage",
    "google-pubsub",
    "google-pubsub-lite",
    "google-bigquery",
    "google-bigquery-sql",
    "google-secret-manager",
    "google-functions",
    "google-firestore",
    "google-vertexai",
    "google-speech-to-text",
    "google-text-to-speech",
];


/// The emulator plus a key file; the Google components of a context are pointed at it with
/// component-level options, so the examples run unchanged.
struct GcpEnv {
    mock: GcpMock,
    key: std::path::PathBuf,
    dir: std::path::PathBuf,
}

impl GcpEnv {
    fn new(name: &str) -> GcpEnv {
        let mock = GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        let dir = std::env::temp_dir().join(format!("cameleer-docs-gcp-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        GcpEnv { mock, key, dir }
    }
    fn configure(&self, ctx: &CamelContext) {
        let key = format!("file:{}", self.key.to_string_lossy().replace('\\', "/"));
        for s in GCP_SCHEMES {
            ctx.set_component_option(s, "serviceAccountKey", key.clone());
            ctx.set_component_option(s, "uriEndpointOverride", self.mock.url());
            ctx.set_component_option(s, "overrideEndpoint", "true");
            ctx.set_component_option(s, "pubsubEndpoint", self.mock.pubsub_grpc_address());
        }
    }
    /// A sub directory (forward slashes, for YAML).
    fn path(&self, sub: &str) -> String {
        let p = self.dir.join(sub);
        std::fs::create_dir_all(&p).unwrap();
        p.to_string_lossy().replace('\\', "/")
    }
    fn start(&self, name: &str, yaml: String) -> cameleer_main::Main {
        let mut main = cameleer_main::new_main();
        self.configure(main.context());
        main.add_route_source(format!("{}.camel.yaml", name), yaml);
        main.start().unwrap_or_else(|e| panic!("{} does not start: {}", name, e));
        main
    }
    fn files(&self, sub: &str) -> Vec<(String, Vec<u8>)> {
        let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(self.dir.join(sub))
            .map(|d| d.flatten().filter(|e| e.path().is_file()).map(|e| (e.file_name().to_string_lossy().into_owned(), std::fs::read(e.path()).unwrap())).collect())
            .unwrap_or_default();
        out.sort();
        out
    }
}

impl Drop for GcpEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ---------------------------------------------------------------- google-storage

#[test]
fn google_storage_upload_files() {
    let env = GcpEnv::new("gcs-up");
    let inbox = env.path("inbox");
    let yaml = env_subst(&example("components/google-storage.md", "google-storage-upload-files"), &[("INBOX_DIR", &inbox), ("GCS_BUCKET", "reports")]);
    let main = env.start("gcs-up", yaml);
    std::fs::write(env.dir.join("inbox").join("sales.csv"), "region,total\neu,42\n").unwrap();
    wait_for("the upload", || env.mock.object("reports", "uploads/sales.csv").is_some());
    assert_eq!(env.mock.object("reports", "uploads/sales.csv").unwrap(), b"region,total\neu,42\n");
    main.stop().unwrap();
}

#[test]
fn google_storage_consume_and_archive() {
    let env = GcpEnv::new("gcs-in");
    let out = env.path("out");
    env.mock.put_object("incoming", "orders/1.json", br#"{"id":1}"#);
    env.mock.put_object("incoming", "other/skip.txt", b"skip");
    env.mock.create_bucket("archive");
    let yaml = env_subst(
        &example("components/google-storage.md", "google-storage-consume-and-archive"),
        &[("GCS_BUCKET", "incoming"), ("GCS_ARCHIVE_BUCKET", "archive"), ("OUT_DIR", &out)],
    );
    let main = env.start("gcs-in", yaml);
    wait_for("the archived object", || env.mock.object("archive", "orders/1.json").is_some() && env.mock.object("incoming", "orders/1.json").is_none());
    wait_for("the local copy", || !env.files("out/orders").is_empty());
    assert_eq!(env.files("out/orders"), vec![("1.json".to_string(), br#"{"id":1}"#.to_vec())]);
    assert!(env.mock.object("incoming", "other/skip.txt").is_some(), "outside the prefix");
    main.stop().unwrap();
}

fn rust_dsl_google_storage(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("file:inbox?move=.done")
            .set_header("CamelGoogleCloudStorageObjectName", simple("uploads/${file:name}"))
            .to("google-storage://my-bucket")
            .log("uploaded ${file:name}");
    })?;
    Ok(())
}

/// POST `body` to `127.0.0.1:port/path`; returns (status, body).
fn gcp_http_post(port: u16, path: &str, body: &str) -> (u16, String) {
    use std::io::{Read, Write};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut s = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(e) => {
                assert!(std::time::Instant::now() < deadline, "connect {}: {}", port, e);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    };
    s.set_read_timeout(Some(std::time::Duration::from_secs(20))).unwrap();
    let req = format!(
        "POST {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        path,
        body.len(),
        body
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

impl GcpEnv {
    /// Start with an HTTP server port (for `platform-http` examples).
    fn start_http(&self, name: &str, yaml: String) -> (cameleer_main::Main, u16) {
        let port = free_port();
        let mut main = cameleer_main::new_main();
        main.set_property("camel.server.port", port.to_string());
        self.configure(main.context());
        main.add_route_source(format!("{}.camel.yaml", name), yaml);
        main.start().unwrap_or_else(|e| panic!("{} does not start: {}", name, e));
        (main, port)
    }
    /// A started context with the Google components pointed at the emulator (setup calls).
    fn admin(&self) -> CamelContext {
        let ctx = CamelContext::new();
        cameleer_cloud::register(&ctx);
        self.configure(&ctx);
        ctx.start().unwrap();
        ctx
    }
}

// ---------------------------------------------------------------- google-pubsub

#[test]
fn google_pubsub_publish_webhooks() {
    let env = GcpEnv::new("ps-pub");
    env.mock.create_topic(PROJECT, "events");
    env.mock.create_subscription(PROJECT, "events-sub", "events");
    let yaml = env_subst(&example("components/google-pubsub.md", "google-pubsub-publish-webhooks"), &[("GCP_PROJECT", PROJECT)]);
    let (main, port) = env.start_http("ps-pub", yaml);
    let (status, body) = gcp_http_post(port, "/events", r#"{"type":"signup","user":"ann"}"#);
    assert_eq!(status, 202, "{}", body);
    assert!(!body.trim().is_empty(), "message id in the response");
    assert_eq!(env.mock.backlog(PROJECT, "events-sub"), vec![r#"{"type":"signup","user":"ann"}"#.to_string()]);
    main.stop().unwrap();
}

#[test]
fn google_pubsub_consume_subscription() {
    let env = GcpEnv::new("ps-sub");
    let out = env.path("out");
    env.mock.create_topic(PROJECT, "events");
    env.mock.create_subscription(PROJECT, "events-sub", "events");
    let id = env.mock.publish(PROJECT, "events", br#"{"n":1}"#, &[]);
    let yaml = env_subst(&example("components/google-pubsub.md", "google-pubsub-consume-subscription"), &[("GCP_PROJECT", PROJECT), ("OUT_DIR", &out)]);
    let main = env.start("ps-sub", yaml);
    wait_for("the file", || !env.files("out").is_empty());
    assert_eq!(env.files("out"), vec![(format!("{}.json", id), br#"{"n":1}"#.to_vec())]);
    wait_for("the ack", || env.mock.backlog(PROJECT, "events-sub").is_empty());
    main.stop().unwrap();
}

// ---------------------------------------------------------------- google-pubsub-lite

#[test]
fn google_pubsub_lite_publish() {
    let env = GcpEnv::new("lite-pub");
    let admin = env.admin();
    let lite = format!("google-pubsub-lite:{}:us-central1-a:telemetry", PROJECT);
    admin.create_producer_template().request_body(&format!("{}?operation=createTopic", lite), "").unwrap();
    let yaml = env_subst(&example("components/google-pubsub-lite.md", "google-pubsub-lite-publish"), &[("GCP_PROJECT", PROJECT)]);
    let main = env.start("lite-pub", yaml);
    let topic = format!("projects/{}/locations/us-central1-a/topics/telemetry", PROJECT);
    wait_for("published messages", || env.mock.lite_messages(&topic, 0).len() >= 2);
    let msgs = env.mock.lite_messages(&topic, 0);
    assert_eq!(msgs[0], b"reading 1".to_vec());
    assert_eq!(msgs[1], b"reading 2".to_vec());
    main.stop().unwrap();
    admin.stop().unwrap();
}

#[test]
fn google_pubsub_lite_consume() {
    let env = GcpEnv::new("lite-sub");
    let out = env.path("out");
    let admin = env.admin();
    let t = admin.create_producer_template();
    let base = format!("google-pubsub-lite:{}:us-central1-a", PROJECT);
    t.request_body(&format!("{}:telemetry?operation=createTopic", base), "").unwrap();
    t.request_body(&format!("{}:telemetry-sub?operation=createSubscription&topic=telemetry", base), "").unwrap();
    t.request_body(&format!("{}:telemetry", base), Value::Array(vec![Value::from("a"), Value::from("b")])).unwrap();
    let yaml = env_subst(&example("components/google-pubsub-lite.md", "google-pubsub-lite-consume"), &[("GCP_PROJECT", PROJECT), ("OUT_DIR", &out)]);
    let main = env.start("lite-sub", yaml);
    wait_for("two files", || env.files("out").len() == 2);
    assert_eq!(env.files("out"), vec![("p0-0.txt".to_string(), b"a".to_vec()), ("p0-1.txt".to_string(), b"b".to_vec())]);
    main.stop().unwrap();
    admin.stop().unwrap();
}

// ---------------------------------------------------------------- google-bigquery

#[test]
fn google_bigquery_stream_from_pubsub() {
    let env = GcpEnv::new("bq-stream");
    env.mock.create_topic(PROJECT, "events");
    env.mock.create_subscription(PROJECT, "events-sub", "events");
    env.mock.create_bq_table(PROJECT, "analytics", "events", &[("id", "STRING"), ("kind", "STRING"), ("amount", "INTEGER")]);
    env.mock.publish(PROJECT, "events", br#"{"id":"e1","kind":"order","amount":42}"#, &[]);
    env.mock.publish(PROJECT, "events", br#"{"id":"e2","kind":"refund","amount":7}"#, &[]);
    let yaml = env_subst(&example("components/google-bigquery.md", "google-bigquery-stream-from-pubsub"), &[("GCP_PROJECT", PROJECT)]);
    let main = env.start("bq-stream", yaml);
    wait_for("two rows", || env.mock.bq_rows(PROJECT, "analytics", "events").len() == 2);
    let mut kinds: Vec<String> = env.mock.bq_rows(PROJECT, "analytics", "events").iter().map(|r| r.get("kind").map(|v| v.to_string()).unwrap_or_default()).collect();
    kinds.sort();
    assert_eq!(kinds, vec!["order", "refund"]);
    main.stop().unwrap();
}

#[test]
fn google_bigquery_load_from_gcs() {
    let env = GcpEnv::new("bq-load");
    env.mock.put_object("staging", "exports/events.json", b"{\"id\":\"a\",\"kind\":\"x\"}\n{\"id\":\"b\",\"kind\":\"y\"}\n");
    env.mock.create_bq_table(PROJECT, "analytics", "events", &[("id", "STRING"), ("kind", "STRING")]);
    let yaml = env_subst(&example("components/google-bigquery.md", "google-bigquery-load-from-gcs"), &[("GCP_PROJECT", PROJECT), ("GCS_BUCKET", "staging")]);
    let main = env.start("bq-load", yaml);
    wait_for("loaded rows", || env.mock.bq_rows(PROJECT, "analytics", "events").len() == 2);
    main.stop().unwrap();
}

// ---------------------------------------------------------------- google-bigquery-sql

#[test]
fn google_bigquery_sql_hourly_report() {
    let env = GcpEnv::new("bq-sql");
    let out = env.path("out");
    env.mock.create_bq_table(PROJECT, "shop", "orders", &[("id", "INTEGER"), ("customer", "STRING"), ("total", "FLOAT")]);
    let admin = env.admin();
    let insert = format!("google-bigquery-sql://{}:INSERT INTO shop.orders (id, customer, total) VALUES (1, 'acme', 10.5), (2, 'globex', 99.0), (3, 'initech', 75.25)", PROJECT);
    assert_eq!(admin.create_producer_template().request_body(&insert, "").unwrap(), Value::Int(3));
    let yaml = env_subst(&example("components/google-bigquery-sql.md", "google-bigquery-sql-hourly-report"), &[("GCP_PROJECT", PROJECT), ("OUT_DIR", &out)]);
    let main = env.start("bq-sql", yaml);
    wait_for("the report", || !env.files("out").is_empty());
    let (name, data) = env.files("out").remove(0);
    assert_eq!(name, "large-orders.json");
    let rows = cameleer_core::json::parse(&String::from_utf8(data).unwrap()).unwrap();
    let ids: Vec<i64> = rows.as_array().unwrap().iter().map(|r| r.get("id").unwrap().as_i64().unwrap()).collect();
    assert_eq!(ids, vec![2, 3], "orders over 50, largest first");
    main.stop().unwrap();
    admin.stop().unwrap();
}

#[test]
fn google_bigquery_sql_insert_from_http() {
    let env = GcpEnv::new("bq-http");
    env.mock.create_bq_table(PROJECT, "shop", "orders", &[("id", "INTEGER"), ("customer", "STRING"), ("total", "FLOAT")]);
    let yaml = env_subst(&example("components/google-bigquery-sql.md", "google-bigquery-sql-insert-from-http"), &[("GCP_PROJECT", PROJECT)]);
    let (main, port) = env.start_http("bq-http", yaml);
    let (status, body) = gcp_http_post(port, "/orders", r#"{"id":7,"customer":"acme","total":12.5}"#);
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, "inserted 1 row(s)");
    let rows = env.mock.bq_rows(PROJECT, "shop", "orders");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get("customer").map(|v| v.to_string()).as_deref(), Some("acme"));
    main.stop().unwrap();
}

// ---------------------------------------------------------------- google-firestore

#[test]
fn google_firestore_save_from_http() {
    let env = GcpEnv::new("fs-http");
    let yaml = env_subst(&example("components/google-firestore.md", "google-firestore-save-from-http"), &[("GCP_PROJECT", PROJECT)]);
    let (main, port) = env.start_http("fs-http", yaml);
    let (status, body) = gcp_http_post(port, "/devices", r#"{"id":"dev-1","temperature":21,"online":true}"#);
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, "saved dev-1");
    let doc = env.mock.firestore_doc(PROJECT, "devices/dev-1").expect("document");
    assert_eq!(doc.get("temperature"), Some(&Value::Int(21)));
    assert_eq!(doc.get("online"), Some(&Value::Bool(true)));
    main.stop().unwrap();
}

#[test]
fn google_firestore_listen_changes() {
    let env = GcpEnv::new("fs-listen");
    let out = env.path("out");
    env.mock.put_firestore_doc(PROJECT, "devices/dev-1", Value::object([("temperature", 20)]));
    let yaml = env_subst(&example("components/google-firestore.md", "google-firestore-listen-changes"), &[("GCP_PROJECT", PROJECT), ("OUT_DIR", &out)]);
    let yaml = subst(&yaml, &[("delay=2000", "delay=100")]);
    let main = env.start("fs-listen", yaml);
    let log = || env.files("out").first().map(|(_, d)| String::from_utf8_lossy(d).into_owned()).unwrap_or_default();
    wait_for("the initial snapshot", || log().contains("ADDED dev-1"));
    env.mock.put_firestore_doc(PROJECT, "devices/dev-2", Value::object([("temperature", 30)]));
    wait_for("the new document", || log().contains("ADDED dev-2"));
    main.stop().unwrap();
}

// ---------------------------------------------------------------- google-functions

#[test]
fn google_functions_call_from_http() {
    let env = GcpEnv::new("fn");
    env.mock.register_function(PROJECT, "europe-west1", "greet", |data| Ok(format!("Hello {}", data)));
    let yaml = env_subst(&example("components/google-functions.md", "google-functions-call-from-http"), &[("GCP_PROJECT", PROJECT)]);
    let (main, port) = env.start_http("fn", yaml);
    let (status, body) = gcp_http_post(port, "/greet", "Camel");
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, "Hello Camel");
    main.stop().unwrap();
}

// ---------------------------------------------------------------- google-secret-manager

#[test]
fn google_secret_manager_rotate() {
    let env = GcpEnv::new("sm-rotate");
    let admin = env.admin();
    admin
        .create_producer_template()
        .request_body_and_header(&format!("google-secret-manager://{}?operation=createSecret", PROJECT), "first", "CamelGoogleSecretManagerSecretId", "webhook-token")
        .unwrap();
    let yaml = env_subst(&example("components/google-secret-manager.md", "google-secret-manager-rotate"), &[("GCP_PROJECT", PROJECT)]);
    let main = env.start("sm-rotate", yaml);
    wait_for("a new version", || env.mock.secret_versions(PROJECT, "webhook-token").len() == 2);
    assert_eq!(env.mock.secret_versions(PROJECT, "webhook-token")[1].len(), 36, "a uuid");
    main.stop().unwrap();
    admin.stop().unwrap();
}

#[test]
fn google_secret_manager_read() {
    let env = GcpEnv::new("sm-read");
    let out = env.path("out");
    env.mock.put_secret(PROJECT, "api-key", b"k-123");
    let yaml = env_subst(&example("components/google-secret-manager.md", "google-secret-manager-read"), &[("GCP_PROJECT", PROJECT), ("OUT_DIR", &out)]);
    let yaml = subst(&yaml, &[("- to: \"file:", "- to: \"mock:secret\"\n        - to: \"file:")]);
    let main = env.start("sm-read", yaml);
    wait_for("the status file", || !env.files("out").is_empty());
    let m = main.context().mock_endpoint("mock:secret").unwrap();
    assert_eq!(m.received_exchanges()[0].header("X-Api-Key").map(|v| v.to_string()).as_deref(), Some("k-123"));
    main.stop().unwrap();
}

#[test]
fn google_secret_manager_placeholders() {
    let env = GcpEnv::new("sm-ph");
    env.mock.put_secret(PROJECT, "db", br#"{"user":"app","password":"pw-1"}"#);
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    let p = ctx.properties();
    // the properties shown on the page, pointed at the emulator
    p.set("camel.vault.gcp.projectId", PROJECT);
    p.set("camel.vault.gcp.serviceAccountKey", format!("file:{}", env.key.to_string_lossy().replace('\\', "/")));
    p.set("camel.vault.gcp.uriEndpointOverride", env.mock.url());
    assert_eq!(ctx.resolve_placeholders("{{gcp:db/password}}").unwrap(), "pw-1");
    let props = blocks(&doc("components/google-secret-manager.md")).into_iter().find(|(_, l, _)| l == "properties").unwrap().2;
    assert!(props.contains("camel.vault.gcp.projectId=") && props.contains("camel.vault.gcp.serviceAccountKey="));
}

// ---------------------------------------------------------------- speech

#[test]
fn google_speech_to_text_transcribe_folder() {
    let env = GcpEnv::new("stt");
    let inbox = env.path("in");
    let out = env.path("out");
    let yaml = env_subst(&example("components/google-speech-to-text.md", "google-speech-to-text-transcribe-folder"), &[("INBOX_DIR", &inbox), ("OUT_DIR", &out)]);
    let main = env.start("stt", yaml);
    std::fs::write(env.dir.join("in").join("note1.wav"), b"turn on the lights").unwrap();
    wait_for("the transcript", || !env.files("out").is_empty());
    assert_eq!(env.files("out"), vec![("note1.txt".to_string(), b"turn on the lights".to_vec())]);
    main.stop().unwrap();
}

#[test]
fn google_text_to_speech_folder_to_mp3() {
    let env = GcpEnv::new("tts");
    let inbox = env.path("in");
    let out = env.path("out");
    let yaml = env_subst(&example("components/google-text-to-speech.md", "google-text-to-speech-folder-to-mp3"), &[("INBOX_DIR", &inbox), ("OUT_DIR", &out)]);
    let main = env.start("tts", yaml);
    std::fs::write(env.dir.join("in").join("hello.txt"), "Good morning").unwrap();
    wait_for("the audio", || !env.files("out").is_empty());
    assert_eq!(env.files("out"), vec![("hello.mp3".to_string(), b"MOCKAUDIO:en-US-Neural2-C:MP3:text:Good morning".to_vec())]);
    main.stop().unwrap();
}

// ---------------------------------------------------------------- google-vertexai

#[test]
fn google_vertexai_summarize_tickets() {
    let env = GcpEnv::new("vx-sum");
    let inbox = env.path("in");
    let out = env.path("out");
    let yaml = env_subst(&example("components/google-vertexai.md", "google-vertexai-summarize-tickets"), &[("GCP_PROJECT", PROJECT), ("INBOX_DIR", &inbox), ("OUT_DIR", &out)]);
    let main = env.start("vx-sum", yaml);
    std::fs::write(env.dir.join("in").join("t1.txt"), "Checkout is down").unwrap();
    wait_for("the summary", || !env.files("out").is_empty());
    // the emulator echoes the prompt, prefixed with the system instruction
    let (name, data) = env.files("out").remove(0);
    assert_eq!(name, "t1.txt");
    assert_eq!(String::from_utf8(data).unwrap(), "[Summarize the support ticket in one sentence.] Echo: Checkout is down");
    let sent = env.mock.vertex_requests().pop().unwrap();
    assert_eq!(sent.get("generationConfig").unwrap().get("maxOutputTokens"), Some(&Value::Int(256)));
    main.stop().unwrap();
}

#[test]
fn google_vertexai_embeddings() {
    let env = GcpEnv::new("vx-emb");
    let inbox = env.path("in");
    let out = env.path("out");
    let yaml = env_subst(&example("components/google-vertexai.md", "google-vertexai-embeddings"), &[("GCP_PROJECT", PROJECT), ("INBOX_DIR", &inbox), ("OUT_DIR", &out)]);
    let main = env.start("vx-emb", yaml);
    std::fs::write(env.dir.join("in").join("doc1.txt"), "Cameleer routes messages").unwrap();
    wait_for("the vector", || !env.files("out").is_empty());
    let (name, data) = env.files("out").remove(0);
    assert_eq!(name, "doc1.json");
    let v = cameleer_core::json::parse(&String::from_utf8(data).unwrap()).unwrap();
    assert!(!v.as_array().unwrap().is_empty() && v.as_array().unwrap()[0].as_f64().is_some(), "{}", v);
    main.stop().unwrap();
}

// ---------------------------------------------------------------- Rust DSL snippets (verbatim)

fn rust_dsl_google_pubsub(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-pubsub://my-project:events-sub")
            .log("got ${body}")
            .to("google-pubsub://my-project:audit");
    })?;
    Ok(())
}

fn rust_dsl_google_pubsub_lite(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-pubsub-lite:my-project:us-central1-a:telemetry-sub")
            .log("partition ${header.CamelGooglePubsubLitePartition}: ${body}");
    })?;
    Ok(())
}

fn rust_dsl_google_bigquery(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-pubsub://my-project:events-sub")
            .unmarshal(DataFormatDefinition::json())
            .to("google-bigquery://my-project:analytics:events?useAsInsertId=id");
    })?;
    Ok(())
}

fn rust_dsl_google_bigquery_sql(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:report?period=3600000")
            .set_header("min", constant(50))
            .to("google-bigquery-sql://my-project:SELECT id, total FROM shop.orders WHERE total > @min")
            .log("${body}");
    })?;
    Ok(())
}

fn rust_dsl_google_firestore(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-firestore://my-project:devices")
            .log("${header.CamelGoogleFirestoreChangeType}: ${body}");
    })?;
    Ok(())
}

fn rust_dsl_google_functions(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("platform-http:/greet")
            .to("google-functions://greet?project=my-project&location=europe-west1");
    })?;
    Ok(())
}

fn rust_dsl_google_secret_manager(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:read-key?repeatCount=1")
            .set_header("CamelGoogleSecretManagerSecretId", constant("api-key"))
            .to("google-secret-manager://my-project")
            .log("secret length ${body.length()}");
    })?;
    Ok(())
}

fn rust_dsl_google_speech_to_text(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("file:voice-notes?include=.*\\.wav")
            .to("google-speech-to-text://notes?sampleRateHertz=16000")
            .log("${body}");
    })?;
    Ok(())
}

fn rust_dsl_google_text_to_speech(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("file:announcements?include=.*\\.txt")
            .to("google-text-to-speech://announcer?voiceName=en-US-Neural2-C")
            .set_header("CamelFileName", simple("${file:onlyname.noext}.mp3"))
            .to("file:audio");
    })?;
    Ok(())
}

fn rust_dsl_google_vertexai(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("file:tickets?include=.*\\.txt")
            .to("google-vertexai:my-project:us-central1:gemini-2.0-flash?systemInstruction=Summarize in one sentence.")
            .to("file:summaries");
    })?;
    Ok(())
}

/// Every Rust DSL snippet compiles (copied verbatim above) and its routes are accepted.
#[test]
fn gcp_rust_dsl_snippets() {
    let snippets: &[fn(&CamelContext) -> Result<()>] = &[
        rust_dsl_google_storage,
        rust_dsl_google_pubsub,
        rust_dsl_google_pubsub_lite,
        rust_dsl_google_bigquery,
        rust_dsl_google_bigquery_sql,
        rust_dsl_google_firestore,
        rust_dsl_google_functions,
        rust_dsl_google_secret_manager,
        rust_dsl_google_speech_to_text,
        rust_dsl_google_text_to_speech,
        rust_dsl_google_vertexai,
    ];
    for f in snippets {
        let main = cameleer_main::new_main();
        f(main.context()).unwrap();
    }
    // the snippets are the ones on the pages
    for (page, needle) in [
        ("components/google-storage.md", "b.from(\"file:inbox?move=.done\")"),
        ("components/google-pubsub.md", ".to(\"google-pubsub://my-project:audit\")"),
        ("components/google-pubsub-lite.md", "b.from(\"google-pubsub-lite:my-project:us-central1-a:telemetry-sub\")"),
        ("components/google-bigquery.md", ".unmarshal(DataFormatDefinition::json())"),
        ("components/google-bigquery-sql.md", ".set_header(\"min\", constant(50))"),
        ("components/google-firestore.md", "b.from(\"google-firestore://my-project:devices\")"),
        ("components/google-functions.md", ".to(\"google-functions://greet?project=my-project&location=europe-west1\")"),
        ("components/google-secret-manager.md", ".log(\"secret length ${body.length()}\")"),
        ("components/google-speech-to-text.md", ".to(\"google-speech-to-text://notes?sampleRateHertz=16000\")"),
        ("components/google-text-to-speech.md", ".to(\"file:audio\")"),
        ("components/google-vertexai.md", "systemInstruction=Summarize in one sentence."),
    ] {
        let rust = blocks(&doc(page)).into_iter().find(|(_, l, _)| l == "rust").map(|(_, _, c)| c).unwrap_or_default();
        assert!(rust.contains(needle), "{}: rust snippet changed", page);
    }
}

// ===================================================================
// Google Workspace
// ===================================================================

/// GcpMock plus a service account key file; `opts()` points an endpoint at the mock.
struct WsEnv {
    mock: GcpMock,
    key: std::path::PathBuf,
}

impl WsEnv {
    fn new() -> WsEnv {
        let mock = GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        WsEnv { mock, key }
    }
    /// Service account options (key path with forward slashes, safe in YAML strings).
    fn opts(&self) -> String {
        format!("serviceAccountKey=file:{}&uriEndpointOverride={}&overrideEndpoint=true", self.key.to_string_lossy().replace('\\', "/"), self.mock.url())
    }
    /// Extra options for the refresh-token examples: token server and endpoint override.
    fn user_extra(&self) -> String {
        format!("tokenServerUrl={}&uriEndpointOverride={}&overrideEndpoint=true", self.mock.token_uri(), self.mock.url())
    }
    fn alice(&self) {
        self.mock.add_user_credentials("camel-app", "app-secret", "1//refresh-alice", "alice@acme.com", &["https://mail.google.com/", "https://www.googleapis.com/auth/calendar", "https://www.googleapis.com/auth/drive", "https://www.googleapis.com/auth/spreadsheets"]);
    }
}

impl Drop for WsEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
    }
}

/// A minimal HTTP/1.1 request; returns (status, body).
fn ws_http(port: u16, method: &str, path: &str, json: Option<&str>) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("connect {}: {}", port, e),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let body = json.unwrap_or("");
    let req = format!("{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", method, path, body.len(), body);
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

/// Start YAML text with `new_main` on an HTTP port.
fn ws_start(name: &str, yaml: String, port: Option<u16>) -> cameleer_main::Main {
    let mut main = cameleer_main::new_main();
    if let Some(p) = port {
        main.set_property("camel.server.port", p.to_string());
    }
    main.add_route_source(format!("{}.camel.yaml", name), yaml);
    main.start().unwrap_or_else(|e| panic!("{} does not start: {}", name, e));
    main
}

fn ws_req(t: &ProducerTemplate, uri: &str, body: Value, headers: &[(&str, Value)]) -> Exchange {
    let hs: Vec<(String, Value)> = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
    t.request(uri, move |ex| {
        ex.set_body(body.clone());
        for (k, v) in &hs {
            ex.set_header(k.clone(), v.clone());
        }
    })
    .unwrap_or_else(|e| panic!("{}: {:?}", uri, e))
}


// ------------------------------------------------------------------------- google-sheets

#[test]
fn ws_sheets_agent_memory() {
    let env = WsEnv::new();
    let sheet = env.mock.create_spreadsheet("Leads");
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    let t = ctx.create_producer_template();
    ws_req(&t, &format!("google-sheets://spreadsheets/batchUpdate?spreadsheetId={}&{}", sheet, env.opts()), cameleer_core::json::parse(r#"[{"addSheet":{"properties":{"title":"Leads"}}}]"#).unwrap(), &[]);
    ws_req(&t, &format!("google-sheets://data/append?spreadsheetId={}&range=Leads!A1&{}", sheet, env.opts()), cameleer_core::json::parse(r#"[["name","email","score"]]"#).unwrap(), &[]);

    let yaml = env_subst(&example("components/google-sheets.md", "google-sheets-agent-memory"), &[("SHEET_ID", &format!("{}&{}", sheet, env.opts()))]);
    let port = free_port();
    let main = ws_start("sheets", yaml, Some(port));
    let (status, body) = ws_http(port, "POST", "/tools/save-lead", Some(r#"{"name":"Ada","email":"ada@example.com","score":92}"#));
    assert_eq!(status, 200, "{}", body);
    assert!(body.contains("updatedRows"), "{}", body);
    ws_http(port, "POST", "/tools/save-lead", Some(r#"{"name":"Linus","email":"linus@example.com","score":75}"#));
    assert_eq!(env.mock.sheet_values(&sheet, "Leads"), vec![vec!["name", "email", "score"], vec!["Ada", "ada@example.com", "92"], vec!["Linus", "linus@example.com", "75"]]);
    let (status, body) = ws_http(port, "GET", "/tools/list-leads", None);
    assert_eq!(status, 200, "{}", body);
    let rows = cameleer_core::json::parse(&body).unwrap();
    assert_eq!(rows.as_array().unwrap().len(), 2, "{}", body);
    assert!(body.contains("ada@example.com") && !body.contains("\"email\""), "{}", body);
    main.stop().unwrap();
}

fn rust_dsl_google_sheets(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("platform-http:/tools/save-lead?httpMethodRestrict=POST")
            .unmarshal("json")
            .set_body(simple(r#"[["${body[name]}", "${body[email]}", "${body[score]}"]]"#))
            .to("google-sheets://data/append?spreadsheetId={{env:SHEET_ID}}&range=Leads!A:C&valueInputOption=RAW")
            .marshal("json");
        b.from("platform-http:/tools/list-leads?httpMethodRestrict=GET")
            .to("google-sheets://data/get?spreadsheetId={{env:SHEET_ID}}&range=Leads!A2:C1000")
            .marshal("json");
    })?;
    Ok(())
}

// ------------------------------------------------------------------- google-sheets-stream

#[test]
fn ws_sheets_stream_new_rows() {
    let env = WsEnv::new();
    let sheet = env.mock.create_spreadsheet("Orders");
    let ctx = CamelContext::new();
    cameleer_cloud::register(&ctx);
    let t = ctx.create_producer_template();
    let base = |m: &str| format!("google-sheets://{}?spreadsheetId={}&{}", m, sheet, env.opts());
    ws_req(&t, &base("spreadsheets/batchUpdate"), cameleer_core::json::parse(r#"[{"addSheet":{"properties":{"title":"Orders"}}}]"#).unwrap(), &[]);
    ws_req(&t, &format!("{}&range=Orders!A1", base("data/append")), cameleer_core::json::parse(r#"[["order","customer","qty","status"],["1041","Ada","3","new"],["1042","Linus","1","new"]]"#).unwrap(), &[]);

    let yaml = example("components/google-sheets-stream.md", "google-sheets-stream-new-rows");
    let yaml = subst(&yaml, &[("delay=30000", &format!("delay=50&initialDelay=0&{}", env.opts())), ("log:new-orders", "mock:new-orders")]);
    let yaml = env_subst(&yaml, &[("SHEET_ID", &sheet)]);
    let main = ws_start("sheets-stream", yaml, None);
    let m = main.context().mock_endpoint("mock:new-orders").unwrap();
    wait_for("first rows", || m.received_exchanges().len() >= 2);
    ws_req(&t, &format!("{}&range=Orders!A1", base("data/append")), cameleer_core::json::parse(r#"[["1043","Grace","2","new"]]"#).unwrap(), &[]);
    wait_for("appended row", || m.received_exchanges().len() >= 3);
    std::thread::sleep(Duration::from_millis(150));
    let got = m.received_exchanges();
    assert_eq!(got.len(), 3, "rows are emitted once");
    assert!(got[0].body().to_string().contains("1041"), "{}", got[0].body());
    assert!(got[2].body().to_string().contains("Grace"), "{}", got[2].body());
    assert_eq!(got[2].header("CamelGoogleSheets.valueIndex"), Some(&Value::Int(3)));
    main.stop().unwrap();
}

fn rust_dsl_google_sheets_stream(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-sheets-stream://{{env:SHEET_ID}}?range=Orders!A2:D1000&splitResults=true&delay=30000")
            .log("row ${header[CamelGoogleSheets.valueIndex]}: ${body}")
            .marshal("json")
            .to("log:new-orders");
    })?;
    Ok(())
}

// --------------------------------------------------------------------------- google-mail

#[test]
fn ws_mail_daily_digest() {
    let env = WsEnv::new();
    let yaml = env_subst(&example("components/google-mail.md", "google-mail-daily-digest"), &[("GOOGLE_USER", &format!("alice@acme.com&{}", env.opts())), ("DIGEST_TO", "bob@acme.com")]);
    let main = ws_start("mail", yaml, None);
    wait_for("the digest", || !env.mock.gmail_messages("bob@acme.com").is_empty());
    let (raw, labels) = env.mock.gmail_messages("bob@acme.com")[0].clone();
    assert!(raw.contains("Subject: Daily digest") && raw.contains("To: bob@acme.com"), "{}", raw);
    assert!(labels.contains(&"INBOX".to_string()));
    main.stop().unwrap();
}

fn rust_dsl_google_mail(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:daily-digest?period=86400000")
            .set_body(simple("Good morning. This digest was generated at ${date:now:yyyy-MM-dd HH:mm}."))
            .to("google-mail://messages/send?delegate={{env:GOOGLE_USER}}&to={{env:DIGEST_TO}}&subject=Daily digest")
            .log("sent Gmail message ${header[CamelGoogleMail.id]}");
    })?;
    Ok(())
}

// -------------------------------------------------------------------- google-mail-stream

#[test]
fn ws_mail_stream_llm_triage() {
    let env = WsEnv::new();
    env.alice();
    let ai = cameleer_saas::testing::OpenAiMock::start();
    env.mock.gmail_deliver("alice@acme.com", "From: bob@acme.com\r\nTo: alice@acme.com\r\nSubject: Server down\r\n\r\nThe checkout service is down since 09:00.");
    let yaml = example("components/google-mail-stream.md", "google-mail-stream-llm-triage");
    let yaml = subst(
        &yaml,
        &[
            ("delay=60000", &format!("delay=50&initialDelay=0&{}", env.user_extra())),
            ("temperature=0", &format!("temperature=0&apiKey={}", cameleer_saas::testing::OpenAiMock::API_KEY)),
            ("log:inbox-triage", "mock:inbox-triage"),
        ],
    );
    let yaml = env_subst(&yaml, &[("GOOGLE_CLIENT_ID", "camel-app"), ("GOOGLE_CLIENT_SECRET", "app-secret"), ("GOOGLE_REFRESH_TOKEN", "1//refresh-alice"), ("OPENAI_BASE_URL", &ai.base_url())]);
    let main = ws_start("mail-stream", yaml, None);
    let m = main.context().mock_endpoint("mock:inbox-triage").unwrap();
    wait_for("the triaged mail", || !m.received_exchanges().is_empty());
    let ex = &m.received_exchanges()[0];
    let body = ex.body().to_string();
    assert!(body.contains("The checkout service is down"), "{}", body);
    assert_eq!(ex.header("subject").map(|v| v.to_string()).as_deref(), Some("Server down"));
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    let system = req.get("messages").and_then(|m| m.as_array()).and_then(|m| m.first()).and_then(|m| m.get("content")).map(|c| c.to_string()).unwrap_or_default();
    assert!(system.starts_with("Summarize this email"), "{}", system);
    wait_for("marked read", || env.mock.gmail_messages("alice@acme.com").iter().all(|(_, l)| !l.contains(&"UNREAD".to_string())));
    main.stop().unwrap();
}

fn rust_dsl_google_mail_stream(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-mail-stream://index?maxResults=20&clientId={{env:GOOGLE_CLIENT_ID}}&clientSecret=RAW({{env:GOOGLE_CLIENT_SECRET}})&refreshToken=RAW({{env:GOOGLE_REFRESH_TOKEN}})&delay=60000")
            .set_header("subject", simple("${header.CamelGoogleMailStreamSubject}"))
            .to("openai:chat-completion?model={{env:OPENAI_MODEL:gpt-4o-mini}}&baseUrl={{env:OPENAI_BASE_URL:https://api.openai.com/v1}}&temperature=0&systemMessage=Summarize this email in one sentence, then label it URGENT, ACTION or FYI.")
            .log("${header.subject}: ${body}")
            .to("log:inbox-triage");
    })?;
    Ok(())
}

// ----------------------------------------------------------------------- google-calendar

#[test]
fn ws_calendar_book_meeting() {
    let env = WsEnv::new();
    let yaml = env_subst(&example("components/google-calendar.md", "google-calendar-book-meeting"), &[("GOOGLE_USER", &format!("alice@acme.com&{}", env.opts()))]);
    let port = free_port();
    let main = ws_start("calendar", yaml, Some(port));
    let (status, body) = ws_http(port, "POST", "/tools/book-meeting", Some(r#"{"summary":"Design review","start":{"dateTime":"2026-11-12T10:00:00Z"},"end":{"dateTime":"2026-11-12T10:30:00Z"},"attendees":[{"email":"ada@example.com"}]}"#));
    assert_eq!(status, 200, "{}", body);
    let ev = cameleer_core::json::parse(&body).unwrap();
    assert!(ev.get("id").is_some(), "{}", body);
    assert_eq!(ev.get("summary").map(|s| s.to_string()).as_deref(), Some("Design review"));
    assert_eq!(env.mock.calendar_events("primary").len(), 1);
    main.stop().unwrap();
}

fn rust_dsl_google_calendar(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("platform-http:/tools/book-meeting?httpMethodRestrict=POST")
            .to("google-calendar://events/insert?calendarId={{env:CALENDAR_ID:primary}}&sendUpdates=all&delegate={{env:GOOGLE_USER}}")
            .log("created event ${body[id]}")
            .marshal("json");
    })?;
    Ok(())
}

// ---------------------------------------------------------------- google-calendar-stream

#[test]
fn ws_calendar_stream_new_events() {
    let env = WsEnv::new();
    let yaml = example("components/google-calendar-stream.md", "google-calendar-stream-new-events");
    let yaml = subst(&yaml, &[("delay=60000", "delay=50&initialDelay=0"), ("log:meetings", "mock:meetings")]);
    let yaml = env_subst(&yaml, &[("GOOGLE_USER", &format!("alice@acme.com&{}", env.opts()))]);
    let main = ws_start("calendar-stream", yaml, None);
    let m = main.context().mock_endpoint("mock:meetings").unwrap();
    let t = main.context().create_producer_template();
    let future = cameleer_cloud::util::iso8601_millis(cameleer_cloud::util::now_millis() + 86_400_000);
    let later = cameleer_cloud::util::iso8601_millis(cameleer_cloud::util::now_millis() + 88_200_000);
    let ev = Value::object([("summary", Value::from("Design review")), ("start", Value::object([("dateTime", Value::from(future))])), ("end", Value::object([("dateTime", Value::from(later))]))]);
    let created = ws_req(&t, &format!("google-calendar://events/insert?delegate=alice@acme.com&{}", env.opts()), ev, &[]);
    let id = created.body().get("id").unwrap().to_string();
    wait_for("the event", || !m.received_exchanges().is_empty());
    std::thread::sleep(Duration::from_millis(150));
    let got = m.received_exchanges();
    assert_eq!(got.len(), 1, "an unchanged event is emitted once");
    assert!(got[0].body().to_string().contains("Design review"), "{}", got[0].body());
    assert_eq!(got[0].header("CamelGoogleCalendarStreamEventId").map(|v| v.to_string()), Some(id));
    main.stop().unwrap();
}

fn rust_dsl_google_calendar_stream(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("google-calendar-stream://index?calendarId={{env:CALENDAR_ID:primary}}&considerLastUpdate=true&delay=60000&delegate={{env:GOOGLE_USER}}")
            .log("event ${header.CamelGoogleCalendarStreamEventId}: ${body[summary]}")
            .marshal("json")
            .to("log:meetings");
    })?;
    Ok(())
}

// -------------------------------------------------------------------------- google-drive

#[test]
fn ws_drive_upload_and_watch() {
    let env = WsEnv::new();
    let dir = std::env::temp_dir().join(format!("cameleer-docs-drive-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir_s = dir.to_string_lossy().replace('\\', "/");
    let vars = [("REPORTS_DIR", dir_s.as_str()), ("DRIVE_FOLDER_ID", "folder-reports")];
    let upload = example("components/google-drive.md", "google-drive-upload-reports");
    let upload = env_subst(&subst(&upload, &[("mimeType=application/pdf", &format!("mimeType=application/pdf&{}", env.opts()))]), &vars);
    let watch = example("components/google-drive.md", "google-drive-watch-folder");
    let watch = env_subst(&subst(&watch, &[("delay=60000", &format!("delay=50&initialDelay=0&{}", env.opts())), ("log:drive-changes", "mock:drive-changes")]), &vars);
    // a file in another folder is not reported
    env.mock.put_drive_file("other.txt", "text/plain", b"x");
    let mut main = cameleer_main::new_main();
    main.add_route_source("upload.camel.yaml", upload);
    main.add_route_source("watch.camel.yaml", watch);
    main.start().unwrap();
    let m = main.context().mock_endpoint("mock:drive-changes").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    std::fs::write(dir.join("q3.pdf"), b"%PDF-1.7 report").unwrap();
    std::fs::write(dir.join("notes.txt"), b"not a pdf").unwrap();
    wait_for("the upload", || env.mock.drive_files().iter().any(|(n, _)| n == "q3.pdf") && dir.join(".uploaded").join("q3.pdf").exists());
    assert_eq!(env.mock.drive_files().iter().find(|(n, _)| n == "q3.pdf").unwrap().1, b"%PDF-1.7 report");
    assert!(!env.mock.drive_files().iter().any(|(n, _)| n == "notes.txt"));
    wait_for("the change", || !m.received_exchanges().is_empty());
    std::thread::sleep(Duration::from_millis(150));
    let got = m.received_exchanges();
    assert_eq!(got.len(), 1, "only the new file in the folder is reported");
    assert_eq!(got[0].header("CamelGoogleDrive.changeType").map(|v| v.to_string()).as_deref(), Some("created"));
    assert!(got[0].body().to_string().contains("q3.pdf"), "{}", got[0].body());
    main.stop().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

fn rust_dsl_google_drive(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("file:{{env:REPORTS_DIR:reports}}?antInclude=*.pdf&move=.uploaded")
            .to("google-drive://drive-files/create?parents={{env:DRIVE_FOLDER_ID}}&mimeType=application/pdf")
            .log("uploaded ${header.CamelFileName} as Drive file ${body[id]}");
        b.from("google-drive://drive-files/list?q='{{env:DRIVE_FOLDER_ID}}' in parents and trashed = false&consumeExisting=false&delay=60000")
            .log("${header[CamelGoogleDrive.changeType]}: ${body[name]} (${header[CamelGoogleDrive.fileId]})")
            .to("log:drive-changes");
    })?;
    Ok(())
}

/// The Rust DSL snippets of the pages (copied verbatim) build routes.
#[test]
fn ws_rust_dsl_snippets() {
    let fns: &[(&str, fn(&CamelContext) -> Result<()>)] = &[
        ("google-sheets", rust_dsl_google_sheets),
        ("google-sheets-stream", rust_dsl_google_sheets_stream),
        ("google-mail", rust_dsl_google_mail),
        ("google-mail-stream", rust_dsl_google_mail_stream),
        ("google-calendar", rust_dsl_google_calendar),
        ("google-calendar-stream", rust_dsl_google_calendar_stream),
        ("google-drive", rust_dsl_google_drive),
    ];
    for (name, f) in fns {
        let main = cameleer_main::new_main();
        f(main.context()).unwrap_or_else(|e| panic!("{}: {}", name, e));
    }
}

// ===================================================================
// SQL and data stores
// ===================================================================

// ---- local helpers (st_ prefix) ----

static ST_N: AtomicUsize = AtomicUsize::new(0);

fn st_unique(prefix: &str) -> String {
    format!("{}_{}_{}", prefix, std::process::id(), ST_N.fetch_add(1, Ordering::SeqCst))
}

/// Start a YAML example with `new_main` and extra properties.
fn st_start(name: &str, yaml: String, props: &[(&str, &str)]) -> cameleer_main::Main {
    let mut main = cameleer_main::new_main();
    for (k, v) in props {
        main.set_property(*k, *v);
    }
    main.add_route_source(format!("{}.camel.yaml", name), yaml);
    main.start().unwrap_or_else(|e| panic!("{} does not start: {}", name, e));
    main
}

/// A minimal HTTP/1.1 request; returns (status, body).
fn st_http(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut s = loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("connect {}: {}", port, e),
        }
    };
    s.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    let req = format!(
        "{} {} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        method,
        path,
        body.len(),
        body
    );
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out.split(' ').nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    // chunked responses: keep it simple, strip a leading size line if present
    let body = if out.to_ascii_lowercase().contains("transfer-encoding: chunked") {
        let mut res = String::new();
        let mut rest = body.as_str();
        while let Some((size, tail)) = rest.split_once("\r\n") {
            let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
            if n == 0 || tail.len() < n {
                break;
            }
            res.push_str(&tail[..n]);
            rest = tail[n..].trim_start_matches("\r\n");
        }
        res
    } else {
        body
    };
    (status, body)
}

fn st_exec(ds: &Arc<dyn cameleer_sql::DataSource>, sql: &str) -> cameleer_sql::QueryResult {
    ds.get_connection().unwrap().execute(sql, &[]).unwrap_or_else(|e| panic!("{}: {}", sql, e))
}

fn st_count(ds: &Arc<dyn cameleer_sql::DataSource>, sql: &str) -> i64 {
    st_exec(ds, sql).scalar().and_then(|v| v.as_i64()).unwrap_or(-1)
}



// ---- sql ----

#[test]
fn sql_poll_new_orders() {
    let url = format!("jdbc:rustsql:mem:{}", st_unique("docs_sql_poll"));
    let ds = cameleer_sql::datasource_from_url(&url).unwrap();
    let yaml = env_subst(&example("components/sql.md", "sql-poll-new-orders"), &[("DB_URL", &url)]);
    let main = st_start("sql-poll", yaml, &[]);
    // the schema route creates the table; then new rows are picked up once
    wait_for("the orders table", || ds.get_connection().unwrap().execute("select count(*) from orders", &[]).is_ok());
    st_exec(&ds, "insert into orders (item, qty) values ('camel', 2), ('saddle', 1)");
    wait_for("both orders processed", || st_count(&ds, "select count(*) from orders where processed = true") == 2);
    main.context().stop().unwrap();
}

fn st_sql_save(url: &str, ds: &Arc<dyn cameleer_sql::DataSource>) {
    st_exec(ds, "create table if not exists orders (id serial primary key, item text, qty int, processed boolean default false)");
    let port = free_port();
    let yaml = env_subst(&example("components/sql.md", "sql-save-http-orders"), &[("DB_URL", url)]);
    let main = st_start("sql-save", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "POST", "/orders", r#"{"item":"camel","qty":3}"#);
    assert_eq!(status, 200, "{}", body);
    assert_eq!(body, "inserted 1");
    let r = st_exec(ds, "select item, qty from orders");
    assert_eq!(r.rows, vec![vec![Value::from("camel"), Value::Int(3)]]);
    main.context().stop().unwrap();
}

#[test]
fn sql_save_http_orders_embedded() {
    let url = format!("jdbc:rustsql:mem:{}", st_unique("docs_sql_save"));
    let ds = cameleer_sql::datasource_from_url(&url).unwrap();
    st_sql_save(&url, &ds);
}

#[test]
fn sql_save_http_orders_postgres_wire() {
    use cameleer_sql::testing::{PgAuth, PgTestServer};
    let db = st_unique("docs_pg");
    let server = PgTestServer::start(&db, PgAuth::ScramSha256, &[("camel", "secret")]).unwrap();
    let url = format!("jdbc:postgresql://camel:secret@127.0.0.1:{}/{}", server.port(), db);
    let ds = cameleer_sql::datasource_from_url(&url).unwrap();
    st_sql_save(&url, &ds);
}

fn rust_dsl_sql(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:save-order")
            .to("sql:insert into orders (item, qty) values (:#item, :#qty)?dataSource=jdbc:rustsql:mem:shop");
        b.from("direct:find-order")
            .to("sql:select item, qty from orders where id = :#${header.id}?dataSource=jdbc:rustsql:mem:shop&outputType=SelectOne");
    })?;
    Ok(())
}

// ---- sql-stored ----

#[test]
fn sql_stored_add_order() {
    let url = format!("jdbc:rustsql:mem:{}", st_unique("docs_stored"));
    let ds = cameleer_sql::datasource_from_url(&url).unwrap();
    // the setup SQL shown on the page
    let text = doc("components/sql-stored.md");
    let setup = blocks(&text).into_iter().find(|(_, lang, _)| lang == "sql").map(|(_, _, c)| c).unwrap();
    for stmt in setup.split(";\ncreate ") {
        let stmt = if stmt.starts_with("create ") { stmt.to_string() } else { format!("create {}", stmt) };
        st_exec(&ds, stmt.trim().trim_end_matches(';'));
    }
    let port = free_port();
    let yaml = env_subst(&example("components/sql-stored.md", "sql-stored-add-order"), &[("DB_URL", &url)]);
    let main = st_start("sql-stored", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "POST", "/orders", r#"{"item":"camel","qty":3}"#);
    assert_eq!((status, body.replace(' ', "").as_str()), (200, r#"{"total":3}"#));
    let (_, body) = st_http(port, "POST", "/orders", r#"{"item":"saddle","qty":2}"#);
    assert_eq!(body.replace(' ', ""), r#"{"total":5}"#);
    let (status, body) = st_http(port, "POST", "/orders", r#"{"item":"plum","qty":0}"#);
    assert_eq!(status, 500, "{}", body);
    assert_eq!(st_count(&ds, "select count(*) from orders"), 2);
    main.context().stop().unwrap();
}

fn rust_dsl_sql_stored(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:add-order")
            .to("sql-stored:add_order(VARCHAR :#item, INTEGER :#qty, OUT INTEGER total)?dataSource=jdbc:rustsql:mem:shop");
        b.from("direct:big-orders")
            .to("sql-stored:orders_over(INTEGER ${header.min})?dataSource=jdbc:rustsql:mem:shop&function=true");
    })?;
    Ok(())
}

// ---- jdbc ----

#[test]
fn jdbc_products_below_price() {
    let url = format!("jdbc:rustsql:mem:{}", st_unique("docs_jdbc"));
    let ds = cameleer_sql::datasource_from_url(&url).unwrap();
    st_exec(&ds, "create table products (name text, price double precision)");
    st_exec(&ds, "insert into products values ('apple', 1.5), ('pear', 2.5), ('fig', 0.5)");
    let port = free_port();
    let yaml = example("components/jdbc.md", "jdbc-products-below-price");
    // the data source comes from the property shown on the page
    let props = doc("components/jdbc.md");
    let line = props.lines().find(|l| l.starts_with("camel.datasource.url=")).unwrap();
    let value = env_subst(line.trim_start_matches("camel.datasource.url="), &[("DB_URL", &url)]);
    let main = st_start("jdbc", yaml, &[("camel.server.port", &port.to_string()), ("camel.datasource.url", &value)]);
    let (status, body) = st_http(port, "GET", "/products?maxPrice=2", "");
    assert_eq!(status, 200, "{}", body);
    let v = cameleer_core::json::parse(&body).unwrap();
    let names: Vec<String> = v.as_array().unwrap().iter().map(|r| r.get("name").unwrap().to_string()).collect();
    assert_eq!(names, vec!["apple", "fig"]);
    main.context().stop().unwrap();
}

fn rust_dsl_jdbc(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:report")
            .set_body(constant("select count(*) as orders from orders"))
            .to("jdbc:dataSource?outputType=SelectOne");
    })?;
    Ok(())
}

// ---- cql ----

fn st_cassandra() -> (cameleer_datastores::testing::CassandraServer, String) {
    let server = cameleer_datastores::testing::CassandraServer::start().unwrap();
    let text = doc("components/cql.md");
    let setup = blocks(&text).into_iter().find(|(_, lang, c)| lang == "sql" && c.contains("KEYSPACE")).map(|(_, _, c)| c).unwrap();
    for stmt in setup.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        server.execute(stmt).unwrap_or_else(|e| panic!("{}: {:?}", stmt, e));
    }
    let hosts = format!("127.0.0.1:{}", server.port());
    (server, hosts)
}

#[test]
fn cql_insert_and_poll_events() {
    let (server, hosts) = st_cassandra();
    let port = free_port();
    let yaml = env_subst(&example("components/cql.md", "cql-insert-events"), &[("CASSANDRA_HOSTS", &hosts)]);
    let main = st_start("cql-insert", yaml, &[("camel.server.port", &port.to_string())]);
    for (seq, msg) in [(1, "door opened"), (2, "door closed")] {
        let (status, body) = st_http(port, "POST", "/events", &format!(r#"{{"day":"2026-11-05","seq":{},"msg":"{}"}}"#, seq, msg));
        assert_eq!((status, body.as_str()), (200, "saved"));
    }
    main.context().stop().unwrap();

    let yaml = env_subst(&example("components/cql.md", "cql-poll-events"), &[("CASSANDRA_HOSTS", &hosts)]);
    let yaml = subst(&yaml, &[("log:events", "mock:events"), ("delay=10000", "delay=10000&initialDelay=0")]);
    let main = st_start("cql-poll", yaml, &[]);
    let mock = main.context().mock_endpoint("mock:events").unwrap();
    wait_for("two rows", || mock.received_exchanges().len() >= 2);
    let rows: Vec<Value> = mock.received_exchanges().iter().map(|e| e.body().clone()).collect();
    assert_eq!(rows[0].get("msg"), Some(&Value::from("door opened")));
    assert_eq!(rows[1].get("seq"), Some(&Value::Int(2)));
    main.context().stop().unwrap();
    drop(server);
}

fn rust_dsl_cql(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:event")
            .to("cql://localhost:9042/iot?cql=INSERT INTO events (day, seq, msg) VALUES (?, ?, ?)");
        b.from("direct:day")
            .to("cql://localhost:9042/iot?cql=SELECT seq, msg FROM events WHERE day = ?&resultSetConversionStrategy=LIMIT_10");
    })?;
    Ok(())
}

// ---- mongodb ----

#[test]
fn mongodb_save_and_read_notes() {
    let server = cameleer_datastores::testing::MongoServer::start().unwrap();
    let uri = server.connection_string();
    let port = free_port();
    let yaml = env_subst(&example("components/mongodb.md", "mongodb-save-notes"), &[("MONGODB_URI", &uri)]);
    let main = st_start("mongodb-save", yaml, &[("camel.server.port", &port.to_string())]);
    for (session, text) in [("s1", "likes tea"), ("s1", "lives in Kochi"), ("s2", "other user")] {
        let (status, body) = st_http(port, "POST", "/notes", &format!(r#"{{"session":"{}","text":"{}"}}"#, session, text));
        assert_eq!(status, 200, "{}", body);
        assert!(body.contains("$oid") && body.contains(text), "{}", body);
    }
    assert_eq!(server.documents("agent", "notes").len(), 3);
    main.context().stop().unwrap();

    let port = free_port();
    let yaml = env_subst(&example("components/mongodb.md", "mongodb-recent-notes"), &[("MONGODB_URI", &uri)]);
    let main = st_start("mongodb-recent", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "GET", "/notes?session=s1", "");
    assert_eq!(status, 200, "{}", body);
    let v = cameleer_core::json::parse(&body).unwrap();
    let texts: Vec<String> = v.as_array().unwrap().iter().map(|d| d.get("text").unwrap().to_string()).collect();
    assert_eq!(texts, vec!["lives in Kochi", "likes tea"], "newest first, one session only");
    let (status, body) = st_http(port, "GET", "/notes?session=x%22%7D", "");
    assert_eq!((status, body.as_str()), (400, "invalid session"));
    main.context().stop().unwrap();
}

fn rust_dsl_mongodb(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:remember")
            .to("mongodb:agentDb?connectionUriString=mongodb://localhost:27017&database=agent&collection=notes&operation=insert");
        b.from("mongodb:agentDb?connectionUriString=mongodb://localhost:27017&database=agent&collection=notes&consumerType=changeStreams")
            .log("${header.CamelMongoDbStreamOperationType}: ${body}");
    })?;
    Ok(())
}

// ---- mongodb-gridfs ----

#[test]
fn mongodb_gridfs_upload_and_download() {
    let server = cameleer_datastores::testing::MongoServer::start().unwrap();
    let uri = server.connection_string();
    let inbox = std::env::temp_dir().join(st_unique("docs-gridfs-inbox"));
    std::fs::create_dir_all(&inbox).unwrap();
    let inbox_s = inbox.to_string_lossy().replace('\\', "/");
    let yaml = env_subst(&example("components/mongodb-gridfs.md", "mongodb-gridfs-upload-folder"), &[("MONGODB_URI", &uri), ("INBOX_DIR", &inbox_s)]);
    let main = st_start("gridfs-upload", yaml, &[]);
    std::fs::write(inbox.join("q3.csv"), "region,total\neu,42\n").unwrap();
    wait_for("the upload", || inbox.join(".done").join("q3.csv").exists());
    let files = server.documents("docs", "reports.files");
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].get("filename").map(|v| v.to_value()), Some(Value::from("q3.csv")));
    main.context().stop().unwrap();

    let port = free_port();
    let yaml = env_subst(&example("components/mongodb-gridfs.md", "mongodb-gridfs-download"), &[("MONGODB_URI", &uri)]);
    let main = st_start("gridfs-download", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "GET", "/reports?name=q3.csv", "");
    assert_eq!((status, body.as_str()), (200, "region,total\neu,42\n"));
    main.context().stop().unwrap();
    let _ = std::fs::remove_dir_all(&inbox);
}

fn rust_dsl_mongodb_gridfs(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("file:inbox?move=.done")
            .to("mongodb-gridfs:archive?connectionUriString=mongodb://localhost:27017&database=docs&bucket=reports");
        b.from("mongodb-gridfs:archive?connectionUriString=mongodb://localhost:27017&database=docs&bucket=reports&queryStrategy=FileAttribute")
            .to("file:outbox");
    })?;
    Ok(())
}

// ---- couchdb ----

#[test]
fn couchdb_save_and_follow_changes() {
    use cameleer_datastores::testing::{CouchDbServer, CouchDbServerConfig};
    let server = CouchDbServer::start_with(CouchDbServerConfig { credentials: Some(("admin".into(), "secret".into())), ..Default::default() }).unwrap();
    let url = server.url();
    let port = free_port();
    let yaml = env_subst(&example("components/couchdb.md", "couchdb-save-documents"), &[("COUCHDB_URL", &url)]);
    let main = st_start("couchdb-save", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, id) = st_http(port, "POST", "/notes", r#"{"text":"first"}"#);
    assert_eq!(status, 200, "{}", id);
    assert!(!id.is_empty());
    assert_eq!(server.document("notes", &id).and_then(|d| d.get("text").cloned()), Some(Value::from("first")));

    // the changes consumer sees documents saved after it started
    let yaml = env_subst(&example("components/couchdb.md", "couchdb-follow-changes"), &[("COUCHDB_URL", &url)]);
    let yaml = subst(&yaml, &[("log:changes", "mock:changes")]);
    let consumer = st_start("couchdb-changes", yaml, &[]);
    let mock = consumer.context().mock_endpoint("mock:changes").unwrap();
    std::thread::sleep(Duration::from_millis(300));
    let (_, id2) = st_http(port, "POST", "/notes", r#"{"text":"second"}"#);
    wait_for("the change", || mock.received_exchanges().iter().any(|e| e.header("CouchDbId").map(|v| v.to_string()) == Some(id2.clone())));
    let ex = mock.received_exchanges().into_iter().find(|e| e.header("CouchDbId").map(|v| v.to_string()) == Some(id2.clone())).unwrap();
    assert_eq!(ex.body().get("text"), Some(&Value::from("second")));
    assert_eq!(ex.header("CouchDbMethod").map(|v| v.to_string()).as_deref(), Some("UPDATE"));
    consumer.context().stop().unwrap();
    main.context().stop().unwrap();
}

fn rust_dsl_couchdb(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:save")
            .to("couchdb:http://localhost:5984/notes?createDatabase=true&username=admin&password=secret");
        b.from("direct:get")
            .set_header("CouchDbMethod", constant("GET"))
            .to("couchdb:http://localhost:5984/notes?username=admin&password=secret");
    })?;
    Ok(())
}

// ---- elasticsearch / opensearch ----

#[test]
fn elasticsearch_index_and_search() {
    use cameleer_datastores::testing::{ElasticsearchServer, ElasticsearchServerConfig};
    let server = ElasticsearchServer::start_with(ElasticsearchServerConfig { basic_auth: Some(("elastic".into(), "changeme".into())), ..Default::default() }).unwrap();
    let hosts = server.address();
    let port = free_port();
    let yaml = env_subst(&example("components/elasticsearch.md", "elasticsearch-index-documents"), &[("ES_HOSTS", &hosts)]);
    let main = st_start("es-index", yaml, &[("camel.server.port", &port.to_string())]);
    for text in ["Rust makes systems programming safe", "Camel routes messages between systems", "Bread recipes for beginners", "Rust async runtimes compared"] {
        let (status, id) = st_http(port, "POST", "/articles", &format!(r#"{{"text":"{}"}}"#, text));
        assert_eq!(status, 200, "{}", id);
        assert!(server.document("articles", &id).is_some(), "{}", id);
    }
    main.context().stop().unwrap();

    let port = free_port();
    let yaml = env_subst(&example("components/elasticsearch.md", "elasticsearch-search-for-rag"), &[("ES_HOSTS", &hosts)]);
    let main = st_start("es-search", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "GET", "/search?q=rust", "");
    assert_eq!(status, 200, "{}", body);
    let v = cameleer_core::json::parse(&body).unwrap();
    let texts: Vec<String> = v.as_array().unwrap().iter().map(|d| d.get("text").unwrap().to_string()).collect();
    assert_eq!(texts.len(), 2, "{:?}", texts);
    assert!(texts.iter().all(|t| t.contains("Rust")), "{:?}", texts);
    let (status, body) = st_http(port, "GET", "/search?q=%22%7D", "");
    assert_eq!((status, body.as_str()), (400, "invalid query"));
    main.context().stop().unwrap();
}

#[test]
fn opensearch_bulk_logs() {
    use cameleer_datastores::testing::{ElasticsearchServer, ElasticsearchServerConfig};
    let server = ElasticsearchServer::start_with(ElasticsearchServerConfig {
        basic_auth: Some(("admin".into(), "admin".into())),
        distribution: Some("opensearch".into()),
        ..Default::default()
    })
    .unwrap();
    let port = free_port();
    let yaml = env_subst(&example("components/opensearch.md", "opensearch-bulk-logs"), &[("OPENSEARCH_HOSTS", &server.address())]);
    let main = st_start("os-bulk", yaml, &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "POST", "/logs", r#"[{"level":"INFO","msg":"started"},{"level":"WARN","msg":"slow request"}]"#);
    assert_eq!(status, 200, "{}", body);
    assert_eq!(server.count("app-logs"), 2);
    main.context().stop().unwrap();
}

fn rust_dsl_elasticsearch(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:index")
            .to("elasticsearch://search?hostAddresses=localhost:9200&operation=Index&indexName=articles");
        b.from("direct:search")
            .to("elasticsearch://search?hostAddresses=localhost:9200&operation=Search&indexName=articles&enableDocumentOnlyMode=true");
    })?;
    Ok(())
}

fn rust_dsl_opensearch(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:logs")
            .to("opensearch://logs?hostAddresses=localhost:9200&operation=Bulk&indexName=app-logs");
        b.from("direct:find")
            .to("opensearch://logs?hostAddresses=localhost:9200&operation=Search&indexName=app-logs&size=10&enableDocumentOnlyMode=true");
    })?;
    Ok(())
}

// ---- spring-redis ----

#[test]
fn spring_redis_memory_and_subscribe() {
    let server = cameleer_datastores::testing::RedisServer::start().unwrap();
    let host_port = server.address();
    let (host, rport) = host_port.rsplit_once(':').unwrap();
    let redis = |id: &str| subst(&env_subst(&example("components/spring-redis.md", id), &[("REDIS_HOST", host)]), &[(":6379", &format!(":{}", rport))]);
    let port = free_port();
    let main = st_start("redis-remember", redis("spring-redis-remember"), &[("camel.server.port", &port.to_string())]);
    for (i, turn) in ["user: hi", "assistant: hello"].iter().enumerate() {
        let (status, body) = st_http(port, "POST", "/memory?session=s1", turn);
        assert_eq!((status, body.as_str()), (200, (i + 1).to_string().as_str()));
    }
    main.context().stop().unwrap();

    let port = free_port();
    let main = st_start("redis-recall", redis("spring-redis-recall"), &[("camel.server.port", &port.to_string())]);
    let (status, body) = st_http(port, "GET", "/memory?session=s1", "");
    assert_eq!(status, 200, "{}", body);
    assert_eq!(cameleer_core::json::parse(&body).unwrap(), Value::Array(vec!["user: hi".into(), "assistant: hello".into()]));
    main.context().stop().unwrap();

    let yaml = subst(&redis("spring-redis-subscribe"), &[("log:alerts", "mock:alerts")]);
    let main = st_start("redis-subscribe", yaml, &[]);
    let mock = main.context().mock_endpoint("mock:alerts").unwrap();
    wait_for("the subscription", || server.subscription_count() > 0);
    server.exec(&["PUBLISH", "alerts", "disk almost full"]);
    wait_for("the message", || mock.received_exchanges().len() == 1);
    let ex = &mock.received_exchanges()[0];
    assert_eq!(ex.body().to_string(), "disk almost full");
    assert_eq!(ex.header("CamelRedis.Channel").map(|v| v.to_string()).as_deref(), Some("alerts"));
    main.context().stop().unwrap();
}

fn rust_dsl_spring_redis(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:cache-answer")
            .set_header("CamelRedis.Key", simple("answer:${header.questionId}"))
            .set_header("CamelRedis.Value", simple("${body}"))
            .set_header("CamelRedis.Timeout", constant(3600))
            .to("spring-redis://localhost:6379?command=SETEX");
        b.from("spring-redis://localhost:6379?command=PSUBSCRIBE&channels=orders.*")
            .log("${header[CamelRedis.Channel]}: ${body}");
    })?;
    Ok(())
}

#[test]
fn st_rust_dsl_snippets() {
    let fns: &[(&str, fn(&CamelContext) -> Result<()>)] = &[("sql", rust_dsl_sql), ("sql-stored", rust_dsl_sql_stored), ("jdbc", rust_dsl_jdbc), ("cql", rust_dsl_cql), ("mongodb", rust_dsl_mongodb), ("mongodb-gridfs", rust_dsl_mongodb_gridfs), ("couchdb", rust_dsl_couchdb), ("elasticsearch", rust_dsl_elasticsearch), ("opensearch", rust_dsl_opensearch), ("spring-redis", rust_dsl_spring_redis)];
    for (name, f) in fns {
        let main = cameleer_main::new_main();
        let ctx = main.context();
        cameleer_sql::bind_datasource(ctx, "dataSource", cameleer_sql::datasource_from_url("jdbc:rustsql:mem:st_dsl").unwrap());
        f(ctx).unwrap_or_else(|e| panic!("{}: {}", name, e));
    }
}

// ===================================================================
// SaaS and AI
// ===================================================================


fn saas_mock(ctx: &CamelContext, uri: &str) -> MockEndpoint {
    ctx.mock_endpoint(uri).unwrap()
}

fn saas_s(v: Option<&Value>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

// ---------------------------------------------------------------- slack

#[test]
fn slack_webhook_post_example() {
    let slack = SlackMock::start();
    let yaml = env_subst(&example("components/slack.md", "slack-webhook-post"), &[("SLACK_WEBHOOK_URL", &slack.webhook_url())]);
    let main = start_yaml("slack-webhook-post", yaml, None);
    wait_for("the webhook post", || slack.webhook_posts().len() == 1);
    let p = &slack.webhook_posts()[0];
    assert_eq!(saas_s(p.get("text")), "Deployment of checkout v42 finished");
    assert_eq!(saas_s(p.get("username")), "cameleer");
    assert_eq!(saas_s(p.get("icon_emoji")), ":rocket:");
    assert_eq!(saas_s(p.get("channel")), "#ops");
    drop(main);
}

#[test]
fn slack_agent_summary_example() {
    let slack = SlackMock::start();
    let ai = OpenAiMock::start();
    let yaml = env_subst(
        &example("components/slack.md", "slack-agent-summary"),
        &[("OPENAI_BASE_URL", &ai.base_url()), ("OPENAI_API_KEY", OpenAiMock::API_KEY), ("SLACK_TOKEN", SlackMock::TOKEN), ("SLACK_API_URL", &slack.url())],
    );
    let main = start_yaml("slack-agent-summary", yaml, None);
    let t = main.context().create_producer_template();
    t.send_body("direct:incident-report", "Checkout returned 500s for 12 minutes after the v42 deploy").unwrap();
    let posted = slack.posted();
    assert_eq!(posted.len(), 1);
    let text = saas_s(posted[0].get("text"));
    assert!(text.starts_with(":memo: Echo: Checkout returned 500s"), "{}", text);
    assert!(text.contains("[Summarise this incident report"), "{}", text);
    assert_eq!(saas_s(posted[0].get("channel")), "#general");
}

#[test]
fn slack_read_channel_example() {
    let slack = SlackMock::start();
    slack.add_message("general", "U1", "hello team");
    slack.add_message("general", "U2", "please deploy checkout");
    let yaml = subst(
        &env_subst(&example("components/slack.md", "slack-read-channel"), &[("SLACK_TOKEN", SlackMock::TOKEN), ("SLACK_API_URL", &slack.url())]),
        &[("log:slack-deploy-requests", "mock:slack-deploy-requests"), ("delay=5000", "delay=200")],
    );
    let main = start_yaml("slack-read-channel", yaml, None);
    let m = saas_mock(main.context(), "mock:slack-deploy-requests");
    wait_for("the deploy request", || m.received_exchanges().len() == 1);
    assert_eq!(m.received_exchanges()[0].body().to_string(), "U2 asked: please deploy checkout");
    slack.add_message("general", "U3", "deploy search too");
    wait_for("the newer message", || m.received_exchanges().len() == 2);
    assert_eq!(m.received_exchanges()[1].body().to_string(), "U3 asked: deploy search too");
}

// ---------------------------------------------------------------- telegram

#[test]
fn telegram_ai_bot_example() {
    let tg = TelegramMock::start();
    let ai = OpenAiMock::start();
    tg.push_message(42, "What is Apache Camel?");
    let yaml = env_subst(
        &example("components/telegram.md", "telegram-ai-bot"),
        &[("TELEGRAM_TOKEN", TelegramMock::TOKEN), ("TELEGRAM_API_URL", &tg.url()), ("OPENAI_BASE_URL", &ai.base_url()), ("OPENAI_API_KEY", OpenAiMock::API_KEY)],
    );
    let yaml = subst(&yaml, &[("timeout=30", "timeout=1")]);
    let main = start_yaml("telegram-ai-bot", yaml, None);
    wait_for("the reply", || tg.sent().iter().any(|(m, _)| m == "sendMessage"));
    let (_, p) = tg.sent().into_iter().find(|(m, _)| m == "sendMessage").unwrap();
    assert_eq!(saas_s(p.get("chat_id")), "42");
    let text = saas_s(p.get("text"));
    assert!(text.starts_with("Echo: What is Apache Camel? [You are a helpful assistant."), "{}", text);
    main.stop().unwrap();
}

#[test]
fn telegram_send_alert_example() {
    let tg = TelegramMock::start();
    let yaml = env_subst(&example("components/telegram.md", "telegram-send-alert"), &[("TELEGRAM_TOKEN", TelegramMock::TOKEN), ("TELEGRAM_CHAT_ID", "-100777"), ("TELEGRAM_API_URL", &tg.url())]);
    let main = start_yaml("telegram-send-alert", yaml, None);
    wait_for("the alert", || tg.sent().len() == 1);
    let (m, p) = tg.sent().remove(0);
    assert_eq!(m, "sendMessage");
    assert_eq!(saas_s(p.get("chat_id")), "-100777");
    assert_eq!(saas_s(p.get("parse_mode")), "HTML");
    assert_eq!(saas_s(p.get("text")), "<b>edge-gw-07</b>: disk 91% full");
    main.stop().unwrap();
}

// ---------------------------------------------------------------- github

#[test]
fn github_pr_welcome_example() {
    let gh = GithubMock::start();
    gh.add_pull("existing work", "aaa");
    let yaml = env_subst(&example("components/github.md", "github-pr-welcome"), &[("GITHUB_TOKEN", GithubMock::TOKEN), ("GITHUB_API_URL", &gh.url())]);
    let yaml = subst(&yaml, &[("delay=30000", "delay=200")]);
    let main = start_yaml("github-pr-welcome", yaml, None);
    wait_for("the first poll", || !gh.server.requests().is_empty());
    let n = gh.add_pull("Add Rust DSL docs", "bbb");
    wait_for("the comment", || !gh.comments().is_empty());
    std::thread::sleep(std::time::Duration::from_millis(500));
    let comments = gh.comments();
    assert_eq!(comments.len(), 1, "only the new pull request is commented");
    assert_eq!(comments[0].0, n);
    assert_eq!(saas_s(comments[0].1.get("body")), "Thanks for the pull request 'Add Rust DSL docs'. A maintainer will review it soon.");
    main.stop().unwrap();
}

#[test]
fn github_agent_issue_example() {
    let gh = GithubMock::start();
    let yaml = env_subst(&example("components/github.md", "github-agent-issue"), &[("GITHUB_TOKEN", GithubMock::TOKEN), ("GITHUB_API_URL", &gh.url())]);
    let main = start_yaml("github-agent-issue", yaml, None);
    let t = main.context().create_producer_template();
    let ex = t.request_body_and_header("direct:file-bug", "Step 'load' timed out after 30 min", "job", "etl-orders").unwrap();
    assert!(ex.get("number").is_some(), "{}", ex);
    let issues = gh.issues();
    assert_eq!(issues.len(), 1);
    assert_eq!(saas_s(issues[0].get("title")), "Nightly job failed: etl-orders");
    assert_eq!(saas_s(issues[0].get("body")), "Step 'load' timed out after 30 min");
    main.stop().unwrap();
}

// ---------------------------------------------------------------- jira

#[test]
fn jira_agent_ticket_example() {
    let jira = JiraMock::start();
    let yaml = env_subst(&example("components/jira.md", "jira-agent-ticket"), &[("JIRA_URL", &jira.url()), ("JIRA_USER", JiraMock::USER), ("JIRA_API_TOKEN", JiraMock::PASSWORD)]);
    let main = start_yaml("jira-agent-ticket", yaml, None);
    let t = main.context().create_producer_template();
    let ex = t.request_body_and_header("direct:incident", "Checkout 500s since 09:12", "service", "checkout").unwrap();
    assert_eq!(saas_s(ex.get("key")), "OPS-1");
    let issue = &jira.issues()[0];
    let f = issue.get("fields").unwrap();
    assert_eq!(saas_s(f.get("summary")), "Incident on checkout");
    assert_eq!(saas_s(f.get("description")), "Checkout 500s since 09:12");
    assert_eq!(saas_s(f.get("issuetype").and_then(|t| t.get("name"))), "Bug");
    assert_eq!(saas_s(f.get("priority").and_then(|t| t.get("name"))), "High");
    main.stop().unwrap();
}

#[test]
fn jira_new_issues_to_slack_example() {
    let jira = JiraMock::start();
    let slack = SlackMock::start();
    jira.add_issue("OPS", "old issue");
    let yaml = env_subst(
        &example("components/jira.md", "jira-new-issues-to-slack"),
        &[("JIRA_URL", &jira.url()), ("JIRA_USER", JiraMock::USER), ("JIRA_API_TOKEN", JiraMock::PASSWORD), ("SLACK_WEBHOOK_URL", &slack.webhook_url())],
    );
    let yaml = subst(&yaml, &[("delay=60000", "delay=200")]);
    let main = start_yaml("jira-new-issues-to-slack", yaml, None);
    wait_for("the first poll", || !jira.server.requests_to("/rest/api/2/search").is_empty());
    let key = jira.add_issue("OPS", "Search is slow");
    wait_for("the slack post", || !slack.webhook_posts().is_empty());
    std::thread::sleep(std::time::Duration::from_millis(500));
    let posts = slack.webhook_posts();
    assert_eq!(posts.len(), 1, "only the new issue");
    assert_eq!(saas_s(posts[0].get("text")), format!("New Jira issue {}: Search is slow", key));
    main.stop().unwrap();
}

// ---------------------------------------------------------------- twilio

fn saas_twilio_env(tw: &TwilioMock) -> Vec<(&'static str, String)> {
    vec![("TWILIO_ACCOUNT_SID", TwilioMock::ACCOUNT_SID.to_string()), ("TWILIO_AUTH_TOKEN", TwilioMock::AUTH_TOKEN.to_string()), ("TWILIO_API_URL", tw.url())]
}

fn saas_env<'a>(v: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    v.iter().map(|(k, x)| (*k, x.as_str())).collect()
}

#[test]
fn twilio_send_sms_example() {
    let tw = TwilioMock::start();
    let env = saas_twilio_env(&tw);
    let yaml = env_subst(&example("components/twilio.md", "twilio-send-sms"), &saas_env(&env));
    let main = start_yaml("twilio-send-sms", yaml, None);
    let t = main.context().create_producer_template();
    let out = t.request_body_and_headers("direct:sms-alert", "", vec![("device", Value::from("edge-gw-07")), ("celsius", Value::from(81))]).unwrap();
    assert!(saas_s(out.get("sid")).starts_with("SM"), "{}", out);
    let msgs = tw.messages();
    assert_eq!(msgs.len(), 1);
    assert_eq!(saas_s(msgs[0].get("body")), "ALERT edge-gw-07: temperature 81 C");
    assert_eq!(saas_s(msgs[0].get("to")), "+15005550001");
    assert_eq!(saas_s(msgs[0].get("from")), "+15005550006");
    main.stop().unwrap();
}

#[test]
fn twilio_read_messages_example() {
    let tw = TwilioMock::start();
    let env = saas_twilio_env(&tw);
    let m0 = cameleer_main::new_main();
    let t0 = m0.context().create_producer_template();
    let send = format!("twilio:message/create?from=%2B15005550006&to=%2B15005550001&username={}&password={}&serverUrl={}", TwilioMock::ACCOUNT_SID, TwilioMock::AUTH_TOKEN, tw.url());
    t0.request_body(&send, "first").unwrap();
    t0.request_body(&send, "second").unwrap();
    let yaml = subst(&env_subst(&example("components/twilio.md", "twilio-read-messages"), &saas_env(&env)), &[("log:sms-log", "mock:sms-log")]);
    let main = start_yaml("twilio-read-messages", yaml, None);
    let m = saas_mock(main.context(), "mock:sms-log");
    wait_for("both messages", || m.received_exchanges().len() == 2);
    let bodies: Vec<String> = m.received_exchanges().iter().map(|e| saas_s(e.body().get("body"))).collect();
    assert_eq!(bodies, vec!["first", "second"]);
    let read = tw.server.requests().into_iter().find(|r| r.method == "GET").unwrap();
    assert_eq!(read.query_param("To"), Some("+15005550001"));
    main.stop().unwrap();
}

// ---------------------------------------------------------------- openai

fn saas_openai_env(ai: &OpenAiMock) -> Vec<(&'static str, String)> {
    vec![("OPENAI_BASE_URL", ai.base_url()), ("OPENAI_API_KEY", OpenAiMock::API_KEY.to_string())]
}

#[test]
fn openai_classify_example() {
    let ai = OpenAiMock::start();
    let env = saas_openai_env(&ai);
    let yaml = env_subst(&example("components/openai.md", "openai-classify"), &saas_env(&env));
    let yaml = yaml.replace("        - log: \"category=", "        - to: \"mock:classified\"\n        - log: \"category=");
    let main = start_yaml("openai-classify", yaml, None);
    let t = main.context().create_producer_template();
    t.send_body("direct:classify", "I was charged twice").unwrap();
    let m = saas_mock(main.context(), "mock:classified");
    let ex = &m.received_exchanges()[0];
    let cat = saas_s(ex.header("category"));
    assert!(cat.starts_with("Echo: I was charged twice [Classify the ticket"), "{}", cat);
    assert!(ex.header("CamelOpenAITotalTokens").is_some());
    assert_eq!(saas_s(ex.header("CamelOpenAIFinishReason")), "stop");
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    assert_eq!(saas_s(req.get("model")), "gpt-4o-mini");
    assert_eq!(req.get("max_tokens").and_then(|v| v.as_i64()), Some(5));
    assert_eq!(req.get("temperature").and_then(|v| v.as_f64()), Some(0.0));
    main.stop().unwrap();
}

#[test]
fn openai_embeddings_example() {
    let ai = OpenAiMock::start();
    let env = saas_openai_env(&ai);
    let main = start_yaml("openai-embeddings", env_subst(&example("components/openai.md", "openai-embeddings"), &saas_env(&env)), None);
    let t = main.context().create_producer_template();
    let v = t.request_body("direct:embed", "abcd").unwrap();
    let v = v.as_array().expect("a vector").clone();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].as_f64(), Some(4.0));
    let vs = t.request_body("direct:embed", Value::from(vec!["a", "bb"])).unwrap();
    assert_eq!(vs.as_array().unwrap().len(), 2);
    assert_eq!(saas_s(ai.server.requests_to("/v1/embeddings")[0].json().get("model")), "text-embedding-3-small");
    main.stop().unwrap();
}

#[test]
fn openai_ollama_local_example() {
    let ai = OpenAiMock::start();
    // the mock checks an API key, Ollama does not: the key is added for the test only
    let yaml = env_subst(&example("components/openai.md", "openai-ollama-local"), &[("OLLAMA_URL", &ai.base_url())]);
    let yaml = subst(&yaml, &[("model=llama3.2", &format!("model=llama3.2&apiKey={}", OpenAiMock::API_KEY))]);
    let main = start_yaml("openai-ollama-local", yaml, None);
    let t = main.context().create_producer_template();
    let convo = Value::Array(vec![Value::object([("role", "user"), ("content", "hi")]), Value::object([("role", "assistant"), ("content", "hello")]), Value::from("what is 2+2?")]);
    let answer = t.request_body("direct:ask-local", convo).unwrap();
    assert_eq!(answer.to_string(), "Echo: what is 2+2? [Answer briefly.]");
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    let roles: Vec<String> = req.get("messages").unwrap().as_array().unwrap().iter().map(|m| saas_s(m.get("role"))).collect();
    assert_eq!(roles, vec!["system", "user", "assistant", "user"]);
    assert_eq!(saas_s(req.get("model")), "llama3.2");
    main.stop().unwrap();
}

// ---------------------------------------------------------------- AI guide

const GUIDE: &str = "guides/ai-components.md";

#[test]
fn ai_triage_to_jira_example() {
    let ai = OpenAiMock::start();
    let jira = JiraMock::start();
    let mut env = saas_openai_env(&ai);
    env.extend([("JIRA_URL", jira.url()), ("JIRA_USER", JiraMock::USER.to_string()), ("JIRA_API_TOKEN", JiraMock::PASSWORD.to_string())]);
    let main = start_yaml("ai-triage-to-jira", env_subst(&example(GUIDE, "ai-triage-to-jira"), &saas_env(&env)), None);
    let t = main.context().create_producer_template();
    t.send_body("direct:incident", "Payments API latency above 2s since the 09:00 deploy").unwrap();
    let issue = &jira.issues()[0];
    let f = issue.get("fields").unwrap();
    let summary = saas_s(f.get("summary"));
    assert!(summary.starts_with("Echo: Payments API latency") && summary.contains("[Write a one-line Jira summary"), "{}", summary);
    assert_eq!(saas_s(f.get("description")), "Payments API latency above 2s since the 09:00 deploy");
    main.stop().unwrap();
}

#[test]
fn ai_embeddings_to_files_example() {
    let ai = OpenAiMock::start();
    let base = std::env::temp_dir().join(format!("cameleer-docs-d-embed-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (input, output) = (base.join("in"), base.join("out"));
    std::fs::create_dir_all(&input).unwrap();
    let (i, o) = (input.to_string_lossy().replace('\\', "/"), output.to_string_lossy().replace('\\', "/"));
    let mut env = saas_openai_env(&ai);
    env.extend([("DOCS_DIR", i), ("VECTOR_DIR", o)]);
    let yaml = subst(&env_subst(&example(GUIDE, "ai-embeddings-to-files"), &saas_env(&env)), &[("delay=2000", "delay=100")]);
    let main = start_yaml("ai-embeddings-to-files", yaml, None);
    std::fs::write(input.join("faq.txt"), "hello world!").unwrap();
    std::fs::write(input.join("image.png"), "not text").unwrap();
    let out = output.join("faq.json");
    wait_for("the vector file", || std::fs::read_to_string(&out).map(|s| s.contains(']')).unwrap_or(false));
    let v = cameleer_core::json::parse(&std::fs::read_to_string(&out).unwrap()).unwrap();
    let v = v.as_array().unwrap().clone();
    assert_eq!(v.len(), 3);
    assert_eq!(v[0].as_f64(), Some(12.0));
    assert!(!output.join("image.json").exists());
    main.stop().unwrap();
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn ai_azure_tool_calls_example() {
    let az = cameleer_cloud::testing::AzureMock::start().unwrap();
    az.add_openai_deployment("gpt-4o");
    let yaml = env_subst(&example(GUIDE, "ai-azure-tool-calls"), &[]);
    let yaml = subst(&yaml, &[("my-resource/gpt-4o?apiKey=RAW(dummy-AZURE_OPENAI_KEY)", &format!("my-resource/gpt-4o?{}", az.openai_options()))]);
    let yaml = yaml.replace("        - log: \"finish=", "        - to: \"mock:tool-calls\"\n        - log: \"finish=");
    let main = start_yaml("ai-azure-tool-calls", yaml, None);
    let t = main.context().create_producer_template();
    t.send_body("direct:ask-weather", "").unwrap();
    let m = saas_mock(main.context(), "mock:tool-calls");
    let ex = &m.received_exchanges()[0];
    assert_eq!(saas_s(ex.header("CamelAzureOpenAIFinishReason")), "tool_calls");
    let calls = ex.header("CamelAzureOpenAIToolCalls").and_then(|c| c.as_array()).cloned().unwrap();
    assert_eq!(saas_s(calls[0].get("function").and_then(|f| f.get("name"))), "get_weather");
    let sent = az.openai_requests().pop().unwrap();
    assert!(sent.get("tools").is_some());
    main.stop().unwrap();
}

#[test]
fn ai_openai_tools_passthrough_example() {
    let ai = OpenAiMock::start();
    let env = saas_openai_env(&ai);
    let main = start_yaml("ai-openai-tools-passthrough", env_subst(&example(GUIDE, "ai-openai-tools-passthrough"), &saas_env(&env)), None);
    let t = main.context().create_producer_template();
    let full = t.request_body("direct:ask-with-tools", "").unwrap();
    assert!(full.get("choices").and_then(|c| c.as_array()).is_some(), "the whole response: {}", full);
    let req = ai.server.requests_to("/v1/chat/completions")[0].json();
    assert_eq!(saas_s(req.get("tools").and_then(|t| t.as_array()).and_then(|t| t.first()).and_then(|t| t.get("function")).and_then(|f| f.get("name"))), "get_weather");
    assert_eq!(saas_s(req.get("model")), "gpt-4o-mini");
    main.stop().unwrap();
}

struct SaasGcp {
    mock: cameleer_cloud::testing::GcpMock,
    key: std::path::PathBuf,
}

impl SaasGcp {
    fn new() -> SaasGcp {
        let mock = cameleer_cloud::testing::GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        SaasGcp { mock, key }
    }
    fn opts(&self) -> String {
        let k = self.key.to_string_lossy().replace('\\', "/");
        self.mock.endpoint_options(std::path::Path::new(&k))
    }
}

impl Drop for SaasGcp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
    }
}

#[test]
fn ai_vertex_gemini_example() {
    let g = SaasGcp::new();
    let project = cameleer_cloud::testing::gcp::PROJECT;
    let yaml = env_subst(&example(GUIDE, "ai-vertex-gemini"), &[("GCP_PROJECT", project)]);
    let yaml = subst(&yaml, &[("temperature=0.2", &format!("temperature=0.2&{}", g.opts()))]);
    let main = start_yaml("ai-vertex-gemini", yaml, None);
    let t = main.context().create_producer_template();
    let answer = t.request_body("direct:ask-gemini", "What is Apache Camel?").unwrap();
    assert_eq!(answer.to_string(), "[Answer in one paragraph.] Echo: What is Apache Camel?");
    let sent = g.mock.vertex_requests().pop().unwrap();
    assert_eq!(sent.get("generationConfig").and_then(|c| c.get("maxOutputTokens")).and_then(|v| v.as_i64()), Some(256));
    main.stop().unwrap();
}

#[test]
fn ai_vertex_embeddings_example() {
    let g = SaasGcp::new();
    let project = cameleer_cloud::testing::gcp::PROJECT;
    let yaml = env_subst(&example(GUIDE, "ai-vertex-embeddings"), &[("GCP_PROJECT", project)]);
    let yaml = subst(&yaml, &[("taskType=RETRIEVAL_DOCUMENT", &format!("taskType=RETRIEVAL_DOCUMENT&{}", g.opts()))]);
    let main = start_yaml("ai-vertex-embeddings", yaml, None);
    let t = main.context().create_producer_template();
    let v = t.request_body("direct:embed-gemini", "camel routes").unwrap();
    assert!(v.as_array().map(|a| !a.is_empty() && a[0].as_f64().is_some()).unwrap_or(false), "{}", v);
    let vs = t.request_body("direct:embed-gemini", Value::from(vec!["a", "b"])).unwrap();
    assert_eq!(vs.as_array().unwrap().len(), 2);
    main.stop().unwrap();
}

#[test]
fn ai_bedrock_converse_example() {
    let aws = cameleer_cloud::testing::AwsMock::start().unwrap();
    let yaml = env_subst(&example(GUIDE, "ai-bedrock-converse"), &[]);
    let yaml = subst(&yaml, &[("region=us-east-1&useDefaultCredentialsProvider=true", &aws.endpoint_options())]);
    let main = start_yaml("ai-bedrock-converse", yaml, None);
    let t = main.context().create_producer_template();
    let answer = t.request_body("direct:ask-bedrock", "What is Camel?").unwrap();
    assert_eq!(answer.to_string(), "[Answer in one sentence.] Echo: What is Camel?");
    let (model, _) = aws.bedrock_requests().pop().unwrap();
    assert_eq!(model, "anthropic.claude-3-haiku-20240307-v1:0");
    main.stop().unwrap();
}

// ---------------------------------------------------------------- Rust DSL snippets (verbatim)

fn rust_dsl_slack(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:deploy-notice?repeatCount=1")
            .set_body(constant("Deployment of checkout v42 finished"))
            .to("slack:#ops?webhookUrl={{env:SLACK_WEBHOOK_URL}}&username=cameleer&iconEmoji=:rocket:");
    })?;
    Ok(())
}

fn rust_dsl_telegram(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("timer:disk-check?repeatCount=1")
            .set_body(constant("<b>edge-gw-07</b>: disk 91% full"))
            .to("telegram:bots?authorizationToken={{env:TELEGRAM_TOKEN}}&chatId={{env:TELEGRAM_CHAT_ID}}&parseMode=HTML");
    })?;
    Ok(())
}

fn rust_dsl_github(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("github:pullRequest?repoOwner=apache&repoName=camel&oauthToken={{env:GITHUB_TOKEN}}&delay=30000")
            .set_body(simple("Thanks for the pull request '${body[title]}'. A maintainer will review it soon."))
            .to("github:pullRequestComment?repoOwner=apache&repoName=camel&oauthToken={{env:GITHUB_TOKEN}}");
    })?;
    Ok(())
}

fn rust_dsl_jira(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:incident")
            .set_header("ProjectKey", constant("OPS"))
            .set_header("IssueTypeName", constant("Bug"))
            .set_header("IssueSummary", simple("Incident on ${header.service}"))
            .to("jira:addIssue?jiraUrl={{env:JIRA_URL:https://your-site.atlassian.net}}&username={{env:JIRA_USER}}&password={{env:JIRA_API_TOKEN}}");
    })?;
    Ok(())
}

fn rust_dsl_twilio(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:sms-alert")
            .set_body(simple("ALERT ${header.device}: temperature ${header.celsius} C"))
            .to("twilio:message/create?from=RAW(+15005550006)&to=RAW(+15005550001)&username={{env:TWILIO_ACCOUNT_SID}}&password={{env:TWILIO_AUTH_TOKEN}}");
    })?;
    Ok(())
}

fn rust_dsl_openai(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:classify")
            .to("openai:chat-completion?model=gpt-4o-mini&temperature=0&systemMessage=Classify the ticket as billing, bug or question. Reply with one word.")
            .set_header("category", simple("${body}"));
    })?;
    Ok(())
}

fn rust_dsl_ai_guide(ctx: &CamelContext) -> Result<()> {
    ctx.add_routes(|b| {
        b.from("direct:summarise")
            .to("openai:chat-completion?model=gpt-4o-mini&systemMessage=Summarise in one sentence.")
            .to("slack:#ops?webhookUrl={{env:SLACK_WEBHOOK_URL}}");
    })?;
    Ok(())
}

#[test]
fn saas_rust_dsl_snippets_build() {
    let snippets: &[(&str, fn(&CamelContext) -> Result<()>)] = &[
        ("slack", rust_dsl_slack),
        ("telegram", rust_dsl_telegram),
        ("github", rust_dsl_github),
        ("jira", rust_dsl_jira),
        ("twilio", rust_dsl_twilio),
        ("openai", rust_dsl_openai),
        ("ai guide", rust_dsl_ai_guide),
    ];
    for (name, f) in snippets {
        let main = cameleer_main::new_main();
        f(main.context()).unwrap_or_else(|e| panic!("{}: {}", name, e));
    }
}
