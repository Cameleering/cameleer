use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_http::rest::{RestBindingMode, RestDslExt, RestParam};
use cameleer_http::HttpClient;

fn client() -> HttpClient {
    HttpClient { read_timeout: Duration::from_secs(10), ..HttpClient::new() }
}

#[test]
fn rest_dsl_with_json_binding_and_openapi() {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.add_routes(|b| {
        b.rest_configuration()
            .component("platform-http")
            .host("127.0.0.1")
            .port(0)
            .context_path("/api")
            .binding_mode(RestBindingMode::Json)
            .api_property("api.title", "User API")
            .api_property("api.version", "2.1");
        b.rest("/users")
            .description("User service")
            .tag("users")
            .get("/{id}")
            .route_id("getUser")
            .description("Find a user")
            .param(RestParam::path("id").data_type("integer").description("user id"))
            .param(RestParam::query("verbose").data_type("boolean"))
            .produces("application/json")
            .response_message(200, "The user")
            .response_message(404, "Not found")
            .to("direct:getUser")
            .post("")
            .route_id("createUser")
            .consumes("application/json")
            .param(RestParam::body("user").required(true))
            .to("direct:createUser")
            .delete("/{id}")
            .to("direct:deleteUser");
        b.rest("/health").get("").binding_mode(RestBindingMode::Off).to("direct:health");

        b.from("direct:getUser").process(|ex: &mut Exchange| {
            let id = ex.header("id").map(|v| v.to_string()).unwrap_or_default();
            if id == "0" {
                ex.set_header("CamelHttpResponseCode", 404);
                ex.set_body("no such user");
                return Ok(());
            }
            ex.set_body(Value::object([("id", Value::from(id.parse::<i64>().unwrap())), ("name", Value::from(format!("user{}", id)))]));
            Ok(())
        });
        b.from("direct:createUser").process(|ex: &mut Exchange| {
            // JSON binding gives us a structured body
            let name = ex.body().get("name").cloned().ok_or_else(|| CamelError::illegal_argument("missing name"))?;
            ex.set_header("CamelHttpResponseCode", 201);
            ex.set_body(Value::object([("created", name), ("ok", Value::Bool(true))]));
            Ok(())
        });
        b.from("direct:deleteUser").process(|ex: &mut Exchange| {
            ex.set_body(Value::Null);
            Ok(())
        });
        b.from("direct:health").set_body(constant("UP"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert!(ctx.route("getUser").is_some());
    assert!(ctx.route("createUser").is_some());
    let p = cameleer_http::platform_http_port(&ctx).expect("port");
    let base = format!("http://127.0.0.1:{}/api", p);
    let c = client();

    let r = c.get(&format!("{}/users/7", base)).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.get("Content-Type"), Some("application/json"));
    let v = cameleer_core::json::parse(&r.body_text()).unwrap();
    assert_eq!(v.get("id"), Some(&Value::from(7)));
    assert_eq!(v.get("name"), Some(&Value::from("user7")));

    let r = c.get(&format!("{}/users/0", base)).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (404, "no such user"));

    let r = c.post(&format!("{}/users", base), "application/json", br#"{"name":"alice"}"#).unwrap();
    assert_eq!(r.status, 201);
    let v = cameleer_core::json::parse(&r.body_text()).unwrap();
    assert_eq!(v.get("created"), Some(&Value::from("alice")));

    let r = c.post(&format!("{}/users", base), "application/json", b"{not json").unwrap();
    assert_eq!(r.status, 400);
    let r = c.post(&format!("{}/users", base), "text/plain", b"plain").unwrap();
    assert_eq!(r.status, 415);
    let r = c.request("DELETE", &format!("{}/users/3", base), &[], None, None).unwrap();
    assert_eq!(r.status, 204);
    let r = c.request("PUT", &format!("{}/users/3", base), &[], Some(b"{}"), None).unwrap();
    assert_eq!(r.status, 405);
    let r = c.get(&format!("{}/health", base)).unwrap();
    assert_eq!(r.body_text(), "UP");

    // through the http producer and the rest: producer
    let t = ctx.create_producer_template();
    let reply = t.request_body(&format!("http://127.0.0.1:{}/api/users/9", p), Value::Null).unwrap();
    assert!(reply.to_string().contains("user9"));
    let reply = t.request_body_and_header(&format!("rest:get:/api/users/{{id}}?host=127.0.0.1:{}", p), Value::Null, "id", "11").unwrap();
    assert!(reply.to_string().contains("user11"), "{}", reply);

    // OpenAPI document
    let r = c.get(&format!("{}/api-doc", base)).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(r.headers.get("Content-Type"), Some("application/json"));
    let doc = cameleer_core::json::parse(&r.body_text()).unwrap();
    assert_eq!(doc.get("openapi"), Some(&Value::from("3.0.3")));
    assert_eq!(doc.get("info").unwrap().get("title"), Some(&Value::from("User API")));
    assert_eq!(doc.get("info").unwrap().get("version"), Some(&Value::from("2.1")));
    let paths = doc.get("paths").unwrap();
    let get = paths.get("/users/{id}").unwrap().get("get").unwrap();
    assert_eq!(get.get("operationId"), Some(&Value::from("getUser")));
    assert_eq!(get.get("summary"), Some(&Value::from("Find a user")));
    assert!(get.get("responses").unwrap().get("404").is_some());
    let params = get.get("parameters").unwrap().as_array().unwrap();
    assert_eq!(params.len(), 2);
    assert_eq!(params[0].get("schema").unwrap().get("type"), Some(&Value::from("integer")));
    assert!(paths.get("/users/{id}").unwrap().get("delete").is_some());
    let post = paths.get("/users").unwrap().get("post").unwrap();
    assert!(post.get("requestBody").unwrap().get("content").unwrap().get("application/json").is_some());
    assert!(paths.get("/health").is_some());
    assert_eq!(doc.get("servers").unwrap().as_array().unwrap()[0].get("url"), Some(&Value::from("/api")));

    ctx.stop().unwrap();
}

#[test]
fn rest_auto_binding_cors_and_rest_consumer() {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    ctx.properties().set("camel.server.port", "0");
    ctx.add_routes(|b| {
        b.rest_configuration().binding_mode(RestBindingMode::Auto).enable_cors(true).api_docs(false);
        b.rest("/items").put("/{id}").to("direct:put");
        b.from("direct:put").process(|ex: &mut Exchange| {
            let structured = matches!(ex.body(), Value::Object(_));
            ex.set_body(Value::object([("structured", Value::Bool(structured))]));
            Ok(())
        });
        b.from("rest:get:/plain/{name}").process(|ex: &mut Exchange| {
            let n = ex.header("name").map(|v| v.to_string()).unwrap_or_default();
            ex.set_body(format!("hi {}", n));
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let p = cameleer_http::platform_http_port(&ctx).expect("port");
    let c = client();
    let r = c.request("PUT", &format!("http://127.0.0.1:{}/items/1", p), &[("Content-Type", "application/json")], Some(br#"{"a":1}"#), None).unwrap();
    assert_eq!(r.body_text(), r#"{"structured":true}"#);
    assert_eq!(r.headers.get("Access-Control-Allow-Origin"), Some("*"));
    let r = c.request("PUT", &format!("http://127.0.0.1:{}/items/1", p), &[("Content-Type", "text/plain")], Some(b"{\"a\":1}"), None).unwrap();
    assert_eq!(r.body_text(), r#"{"structured":false}"#);
    let r = c.request("OPTIONS", &format!("http://127.0.0.1:{}/items/1", p), &[], None, None).unwrap();
    assert_eq!(r.status, 200);
    assert!(r.headers.get("Access-Control-Allow-Methods").is_some());
    assert_eq!(c.get(&format!("http://127.0.0.1:{}/api-doc", p)).unwrap().status, 404);
    assert_eq!(c.get(&format!("http://127.0.0.1:{}/plain/bob", p)).unwrap().body_text(), "hi bob");
    ctx.stop().unwrap();
}

#[test]
fn rest_verb_without_to_fails() {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    let r = ctx.add_routes(|b| {
        b.rest("/x").get("");
    });
    assert!(r.is_err());
}
