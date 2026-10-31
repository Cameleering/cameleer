//! BigQuery (+ SQL), Secret Manager (+ `{{gcp:...}}`), Cloud Functions, Firestore and Sheets
//! end-to-end against the embedded emulator (RS256 JWT → OAuth2 bearer tokens verified).

use camel_cloud::testing::gcp::PROJECT;
use camel_cloud::testing::GcpMock;
use camel_core::prelude::*;

fn context() -> CamelContext {
    let ctx = CamelContext::new();
    camel_cloud::register(&ctx);
    ctx
}

struct Env {
    mock: GcpMock,
    key: std::path::PathBuf,
}

impl Env {
    fn new() -> Env {
        let mock = GcpMock::start().unwrap();
        let key = mock.write_service_account_file().unwrap();
        Env { mock, key }
    }
    fn opts(&self) -> String {
        self.mock.endpoint_options(&self.key)
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.key);
    }
}

fn s(v: &Value) -> String {
    v.to_string()
}

#[test]
fn bigquery_streaming_insert() {
    let env = Env::new();
    env.mock.create_bq_table(PROJECT, "analytics", "events", &[("id", "STRING"), ("count", "INTEGER"), ("ok", "BOOLEAN")]);
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("google-bigquery://{}:analytics:events?useAsInsertId=id&{}", PROJECT, env.opts());
    t.send_body(&uri, Value::object([("id", Value::from("e1")), ("count", Value::Int(3)), ("ok", Value::Bool(true))])).unwrap();
    t.send_body(&uri, Value::Array(vec![Value::object([("id", Value::from("e2")), ("count", Value::Int(5))]), Value::object([("id", Value::from("e1")), ("count", Value::Int(99))])])).unwrap();
    let rows = env.mock.bq_rows(PROJECT, "analytics", "events");
    assert_eq!(rows.len(), 2, "insertId e1 de-duplicated: {:?}", rows);
    assert_eq!(rows[1].get("count").unwrap(), &Value::Int(5));
    // template suffix creates a sibling table; table id and partition from headers
    t.request(&format!("google-bigquery://{}:analytics?{}", PROJECT, env.opts()), |ex| {
        ex.set_body(r#"{"id":"s1","count":1}"#);
        ex.set_header("CamelGoogleBigQueryTableId", "events");
        ex.set_header("CamelGoogleBigQueryTableSuffix", "_2026");
    })
    .unwrap();
    assert_eq!(env.mock.bq_rows(PROJECT, "analytics", "events_2026").len(), 1);
    t.request(&uri, |ex| {
        ex.set_body(Value::object([("id", "p1")]));
        ex.set_header("CamelGoogleBigQueryPartitionDecorator", "20260101");
    })
    .unwrap();
    assert_eq!(env.mock.bq_rows(PROJECT, "analytics", "events").len(), 3);
    // rejected rows
    let e = t.request_body(&uri, Value::object([("id", Value::from("bad")), ("nope", Value::Int(1))])).unwrap_err().root_cause().clone();
    assert!(e.is_a("BigQueryException") && e.message.contains("no such field: nope"), "{}", e);
    let e = t.request_body(&format!("google-bigquery://{}:analytics:missing?{}", PROJECT, env.opts()), Value::object([("id", "x")])).unwrap_err().root_cause().clone();
    assert!(e.message.contains("404"), "{}", e);
    assert_eq!(env.mock.auth_failures(), 0);
}

#[test]
fn bigquery_sql_queries_dml_parameters_and_paging() {
    let env = Env::new();
    env.mock.create_bq_table(PROJECT, "shop", "orders", &[("id", "INTEGER"), ("customer", "STRING"), ("total", "FLOAT")]);
    let ctx = context();
    let t = ctx.create_producer_template();
    let sql = |q: &str| format!("google-bigquery-sql://{}:{}?{}", PROJECT, camel_cloud::util::uri_encode(q, false), env.opts());
    let n = t.request_body(&sql("INSERT INTO shop.orders (id, customer, total) VALUES (1, 'acme', 10.5), (2, 'globex', 99.0), (3, 'acme', 7.25)"), "").unwrap();
    assert_eq!(n, Value::Int(3));
    // parameters from headers and body, typed results
    let rows = t
        .request(&sql("SELECT id, total FROM `test-project.shop.orders` WHERE customer = @customer AND total > @min ORDER BY total DESC"), |ex| {
            ex.set_header("customer", "acme");
            ex.set_body(Value::object([("min", 1)]));
        })
        .unwrap();
    assert_eq!(rows.body(), &Value::Array(vec![Value::object([("id", Value::Int(1)), ("total", Value::Float(10.5))]), Value::object([("id", Value::Int(3)), ("total", Value::Float(7.25))])]));
    assert!(rows.header("CamelGoogleBigQueryJobId").is_some_and(|j| s(j).starts_with("job_")));
    // ${header.x} substitution, async completion and pagination
    env.mock.set_bq_async_polls(2);
    env.mock.set_bq_page_size(1);
    let ex = t
        .request(&sql("SELECT * FROM shop.${header.table} ORDER BY id"), |ex| {
            ex.set_header("table", "orders");
        })
        .unwrap();
    assert_eq!(s(ex.header("CamelGoogleBigQueryTranslatedQuery").unwrap()), "SELECT * FROM shop.orders ORDER BY id");
    let ids: Vec<i64> = ex.body().as_array().unwrap().iter().map(|r| r.get("id").unwrap().as_i64().unwrap()).collect();
    assert_eq!(ids, vec![1, 2, 3]);
    env.mock.set_bq_async_polls(0);
    env.mock.set_bq_page_size(0);
    let updated = t.request_body(&sql("UPDATE shop.orders SET total = 0 WHERE customer = 'globex'"), "").unwrap();
    assert_eq!(updated, Value::Int(1));
    let deleted = t.request_body_and_header(&sql("DELETE FROM shop.orders WHERE customer = @c"), "", "c", "acme").unwrap();
    assert_eq!(deleted, Value::Int(2));
    let count = t.request_body(&sql("SELECT COUNT(*) FROM shop.orders"), "").unwrap();
    assert_eq!(count.as_array().unwrap()[0].get("f0_").unwrap(), &Value::Int(1));
    // missing parameter and invalid SQL
    let e = t.request_body(&sql("SELECT * FROM shop.orders WHERE id = @nope"), "").unwrap_err();
    assert!(e.root_cause().message.contains("@nope"), "{}", e);
    let e = t.request_body(&sql("DROP TABLE shop.orders"), "").unwrap_err().root_cause().clone();
    assert!(e.is_a("BigQueryException") && e.message.contains("invalidQuery"), "{}", e);
    // query from a file resource
    let qf = std::env::temp_dir().join(format!("camel-bq-{}.sql", camel_cloud::util::random_hex(6)));
    std::fs::write(&qf, "SELECT * FROM shop.orders").unwrap();
    let r = t.request_body(&format!("google-bigquery-sql://{}:file:{}?{}", PROJECT, qf.display(), env.opts()), "").unwrap();
    assert_eq!(r.as_array().unwrap().len(), 1);
    let _ = std::fs::remove_file(&qf);
}

#[test]
fn secret_manager_operations() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("google-secret-manager://{}?{}", PROJECT, env.opts());
    let name = t
        .request(&format!("{}&operation=createSecret", uri), |ex| {
            ex.set_body("v1-value");
            ex.set_header("CamelGoogleSecretManagerSecretId", "api-key");
        })
        .unwrap()
        .body()
        .to_string();
    assert_eq!(name, format!("projects/{}/secrets/api-key/versions/1", PROJECT));
    t.request(&format!("{}&operation=addSecretVersion", uri), |ex| {
        ex.set_body("v2-value");
        ex.set_header("CamelGoogleSecretManagerSecretId", "api-key");
    })
    .unwrap();
    assert_eq!(t.request_body_and_header(&uri, "", "CamelGoogleSecretManagerSecretId", "api-key").unwrap(), Value::from("v2-value"));
    let v1 = t
        .request(&uri, |ex| {
            ex.set_header("CamelGoogleSecretManagerSecretId", "api-key");
            ex.set_header("CamelGoogleSecretManagerVersionId", "1");
        })
        .unwrap();
    assert_eq!(s(v1.body()), "v1-value");
    let e = t.request(&format!("{}&operation=createSecret", uri), |ex| ex.set_header("CamelGoogleSecretManagerSecretId", "api-key")).unwrap_err().root_cause().clone();
    assert!(e.is_a("ApiException") && e.message.contains("ALREADY_EXISTS"), "{}", e);
    let list = t.request_body(&format!("{}&operation=listSecrets", uri), "").unwrap();
    assert_eq!(list, Value::Array(vec![Value::from("api-key")]));
    t.request_body_and_header(&format!("{}&operation=deleteSecret", uri), "", "CamelGoogleSecretManagerSecretId", "api-key").unwrap();
    assert!(env.mock.secret_versions(PROJECT, "api-key").is_empty());
    let e = t.request_body_and_header(&uri, "", "CamelGoogleSecretManagerSecretId", "api-key").unwrap_err().root_cause().clone();
    assert!(e.message.contains("NOT_FOUND"), "{}", e);
}

#[test]
fn gcp_secrets_properties_function() {
    let env = Env::new();
    env.mock.put_secret(PROJECT, "db", br#"{"user":"postgres","password":"pg-secret"}"#);
    env.mock.put_secret(PROJECT, "token", b"t-1");
    env.mock.put_secret(PROJECT, "token", b"t-2");
    let ctx = context();
    let p = ctx.properties();
    p.set("camel.vault.gcp.serviceAccountKey", format!("file:{}", env.key.display()));
    p.set("camel.vault.gcp.projectId", PROJECT);
    p.set("camel.vault.gcp.uriEndpointOverride", env.mock.url());
    assert_eq!(ctx.resolve_placeholders("{{gcp:token}}").unwrap(), "t-2");
    assert_eq!(ctx.resolve_placeholders("{{gcp:token@1}}").unwrap(), "t-1");
    assert_eq!(ctx.resolve_placeholders("{{gcp:db/user}}/{{gcp:db/password}}").unwrap(), "postgres/pg-secret");
    assert_eq!(ctx.resolve_placeholders("{{gcp:absent:fallback}}").unwrap(), "fallback");
    assert!(ctx.resolve_placeholders("{{gcp:absent}}").is_err());
    ctx.add_routes(|b| {
        b.from("direct:g").set_body(simple("pw={{gcp:db/password}}"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:g", "").unwrap(), Value::from("pw=pg-secret"));
    ctx.stop().unwrap();
    // revoked tokens are refreshed transparently
    env.mock.revoke_tokens();
    assert_eq!(ctx.resolve_placeholders("{{gcp:token}}").unwrap(), "t-2");
    // without credentials the emulator refuses the call
    let ctx2 = context();
    ctx2.properties().set("camel.vault.gcp.projectId", PROJECT);
    ctx2.properties().set("camel.vault.gcp.uriEndpointOverride", env.mock.url());
    let e = ctx2.resolve_placeholders("{{gcp:token}}").unwrap_err();
    assert!(e.message.contains("401") || e.message.contains("UNAUTHENTICATED"), "{}", e);
}

#[test]
fn cloud_functions_call_list_get_delete() {
    let env = Env::new();
    env.mock.register_function(PROJECT, "europe-west1", "greet", |data| Ok(format!("Hello {}", data)));
    env.mock.register_function(PROJECT, "europe-west1", "fail", |_| Err("boom".to_string()));
    let ctx = context();
    let t = ctx.create_producer_template();
    let base = |f: &str| format!("google-functions://{}?location=europe-west1&{}", f, env.opts());
    let ex = t.request(&base("greet"), |ex| ex.set_body("Camel")).unwrap();
    assert_eq!(s(ex.body()), "Hello Camel");
    assert!(ex.header("CamelGoogleCloudFunctionsExecutionId").is_some());
    let e = t.request_body(&base("fail"), "x").unwrap_err().root_cause().clone();
    assert!(e.is_a("ApiException") && e.message.contains("boom"), "{}", e);
    let names = t.request_body(&format!("{}&operation=listFunctions", base("greet")), "").unwrap();
    assert_eq!(names.as_array().unwrap().len(), 2);
    let f = t.request_body(&format!("{}&operation=getFunction", base("greet")), "").unwrap();
    assert_eq!(s(f.get("status").unwrap()), "ACTIVE");
    t.request_body(&format!("{}&operation=deleteFunction", base("fail")), "").unwrap();
    assert_eq!(env.mock.function_names().len(), 1);
    let e = t.request_body(&base("fail"), "x").unwrap_err().root_cause().clone();
    assert!(e.message.contains("404"), "{}", e);
}

#[test]
fn firestore_documents_and_queries() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let uri = format!("google-firestore://{}:cities?{}", PROJECT, env.opts());
    let op = |o: &str| format!("{}&operation={}", uri, o);
    for (id, name, pop, capital) in [("ROM", "Rome", 2_800_000i64, true), ("MIL", "Milan", 1_400_000, false), ("PAR", "Paris", 2_100_000, true)] {
        t.request(&op("createDocument"), |ex| {
            ex.set_body(Value::object([("name", Value::from(name)), ("population", Value::Int(pop)), ("capital", Value::Bool(capital)), ("tags", Value::Array(vec![Value::from("eu")]))]));
            ex.set_header("CamelGoogleFirestoreDocumentId", id);
        })
        .unwrap();
    }
    // generated id
    let ex = t.request(&op("createDocument"), |ex| ex.set_body(r#"{"name":"Nowhere","population":0}"#)).unwrap();
    let generated = s(ex.header("CamelGoogleFirestoreDocumentId").unwrap());
    assert_eq!(generated.len(), 20);
    let rome = t.request_body_and_header(&op("getDocument"), "", "CamelGoogleFirestoreDocumentId", "ROM").unwrap();
    assert_eq!(rome, Value::object([("name", Value::from("Rome")), ("population", Value::Int(2_800_000)), ("capital", Value::Bool(true)), ("tags", Value::Array(vec![Value::from("eu")]))]));
    // updateDocument merges, setDocument replaces
    t.request(&op("updateDocument"), |ex| {
        ex.set_body(Value::object([("population", 2_900_000)]));
        ex.set_header("CamelGoogleFirestoreDocumentId", "ROM");
    })
    .unwrap();
    assert_eq!(env.mock.firestore_doc(PROJECT, "cities/ROM").unwrap().get("name").unwrap(), &Value::from("Rome"));
    assert_eq!(env.mock.firestore_doc(PROJECT, "cities/ROM").unwrap().get("population").unwrap(), &Value::Int(2_900_000));
    let e = t
        .request(&op("updateDocument"), |ex| {
            ex.set_body(Value::object([("x", 1)]));
            ex.set_header("CamelGoogleFirestoreDocumentId", "NOPE");
        })
        .unwrap_err()
        .root_cause()
        .clone();
    assert!(e.is_a("FirestoreException") && e.message.contains("NOT_FOUND"), "{}", e);
    t.request(&op("setDocument"), |ex| {
        ex.set_body(Value::object([("name", "Milano")]));
        ex.set_header("CamelGoogleFirestoreDocumentId", "MIL");
    })
    .unwrap();
    assert_eq!(env.mock.firestore_doc(PROJECT, "cities/MIL").unwrap(), Value::object([("name", "Milano")]));
    // queries: equality map, ordering and limit, structured query passthrough
    let capitals = t.request_body(&format!("{}&orderBy=population&descending=true", op("runQuery")), Value::object([("capital", true)])).unwrap();
    let names: Vec<String> = capitals.as_array().unwrap().iter().map(|d| s(d.get("name").unwrap())).collect();
    assert_eq!(names, vec!["Rome", "Paris"]);
    assert_eq!(s(capitals.as_array().unwrap()[0].get("_id").unwrap()), "ROM");
    let big = r#"{"from":[{"collectionId":"cities"}],"where":{"fieldFilter":{"field":{"fieldPath":"population"},"op":"GREATER_THAN","value":{"integerValue":"2500000"}}}}"#;
    let r = t.request_body(&op("runQuery"), big).unwrap();
    assert_eq!(r.as_array().unwrap().len(), 1);
    let none = t.request_body(&op("runQuery"), Value::object([("name", "Atlantis")])).unwrap();
    assert_eq!(none, Value::Array(vec![]));
    let all = t.request_body(&op("listDocuments"), "").unwrap();
    assert_eq!(all.as_array().unwrap().len(), 4);
    // sub-collections
    t.request(&format!("{}&operation=createDocument", uri), |ex| {
        ex.set_body(Value::object([("name", "Trastevere")]));
        ex.set_header("CamelGoogleFirestoreCollection", "cities/ROM/districts");
        ex.set_header("CamelGoogleFirestoreDocumentId", "TRA");
    })
    .unwrap();
    let d = t.request_body_and_header(&op("runQuery"), Value::object([("name", "Trastevere")]), "CamelGoogleFirestoreCollection", "cities/ROM/districts").unwrap();
    assert_eq!(d.as_array().unwrap().len(), 1);
    t.request_body_and_header(&op("deleteDocument"), "", "CamelGoogleFirestoreDocumentId", generated.as_str()).unwrap();
    assert!(env.mock.firestore_doc(PROJECT, &format!("cities/{}", generated)).is_none());
    assert_eq!(env.mock.auth_failures(), 0);
}

#[test]
fn sheets_values_get_update_append_clear() {
    let env = Env::new();
    let ctx = context();
    let t = ctx.create_producer_template();
    let created = t.request(&format!("google-sheets://spreadsheets/create?title=Budget&{}", env.opts()), |_| {}).unwrap();
    let id = s(created.header("CamelGoogleSheets.spreadsheetId").unwrap());
    let base = |m: &str| format!("google-sheets://data/{}?spreadsheetId={}&{}", m, id, env.opts());
    let r = t
        .request(&format!("{}&range=Sheet1!A1:B2", base("update")), |ex| {
            ex.set_body(Value::Array(vec![Value::Array(vec![Value::from("item"), Value::from("cost")]), Value::Array(vec![Value::from("coffee"), Value::from("3")])]));
        })
        .unwrap();
    assert_eq!(s(r.body().get("updatedRange").unwrap()), "Sheet1!A1:B2");
    assert_eq!(r.body().get("updatedCells").unwrap().as_i64(), Some(4));
    // append after the last row, values from the header
    let r = t
        .request(&format!("{}&range=Sheet1!A1", base("append")), |ex| {
            ex.set_header("CamelGoogleSheets.values", Value::Array(vec![Value::Array(vec![Value::from("tea"), Value::from("2")])]));
        })
        .unwrap();
    assert_eq!(s(r.body().get("updatedRange").unwrap()), "Sheet1!A3:B3");
    let values = t.request_body(&format!("{}&range=Sheet1!A1:B10", base("get")), "").unwrap();
    assert_eq!(
        values,
        Value::Array(vec![
            Value::Array(vec![Value::from("item"), Value::from("cost")]),
            Value::Array(vec![Value::from("coffee"), Value::from("3")]),
            Value::Array(vec![Value::from("tea"), Value::from("2")]),
        ])
    );
    let col = t
        .request(&base("get"), |ex| {
            ex.set_header("CamelGoogleSheets.range", "Sheet1!B2:B3");
        })
        .unwrap();
    assert_eq!(col.body(), &Value::Array(vec![Value::Array(vec![Value::from("3")]), Value::Array(vec![Value::from("2")])]));
    let batch = t.request_body(&format!("{}&ranges=Sheet1!A1:A1,Sheet1!B3", base("batchGet")), "").unwrap();
    assert_eq!(batch.as_array().unwrap().len(), 2);
    t.request_body(&format!("{}&range=Sheet1!A2:B2", base("clear")), "").unwrap();
    assert_eq!(env.mock.sheet_values(&id, "Sheet1")[1], vec!["", ""]);
    let meta = t.request_body(&format!("google-sheets://spreadsheets/get?spreadsheetId={}&{}", id, env.opts()), "").unwrap();
    assert_eq!(s(meta.get("properties").unwrap().get("title").unwrap()), "Budget");
    // a cloud-platform token does not grant the spreadsheets scope
    let token = camel_cloud::gcp::access_token(
        &camel_cloud::gcp::ServiceAccountKey::load(&format!("file:{}", env.key.display())).unwrap(),
        camel_cloud::gcp::CLOUD_PLATFORM_SCOPE,
        &camel_cloud::http::HttpClient::new(),
    )
    .unwrap();
    let req = camel_cloud::http::Request::new("GET", camel_cloud::http::Url::parse(&format!("{}/v4/spreadsheets/{}", env.mock.url(), id)).unwrap()).header("Authorization", format!("Bearer {}", token));
    assert_eq!(camel_cloud::http::HttpClient::new().send(&req).unwrap().status, 403);
    assert!(context().get_endpoint(&format!("google-sheets://data/explode?{}", env.opts())).is_err());
}
