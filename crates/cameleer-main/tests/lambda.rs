//! The AWS Lambda custom runtime (`cameleer_main::lambda`) against an embedded Runtime API
//! that speaks the documented protocol (`/2018-06-01/runtime/...`). Hermetic: no AWS.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cameleer_core::prelude::*;
use cameleer_http::{HttpRequest, HttpResponse, HttpServer, ServerConfig};
use cameleer_main::Main;

#[derive(Default)]
struct FakeApi {
    events: Mutex<VecDeque<(String, String)>>,
    /// (path, Lambda-Runtime-Function-Error-Type header, body)
    posts: Mutex<Vec<(String, Option<String>, String)>>,
}

fn start_api(events: &[(&str, &str)]) -> (Arc<FakeApi>, HttpServer) {
    let api = Arc::new(FakeApi::default());
    for (id, ev) in events {
        api.events.lock().unwrap().push_back((id.to_string(), ev.to_string()));
    }
    let a = api.clone();
    let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), move |req: &mut HttpRequest| -> HttpResponse {
        if req.method == "GET" && req.path == "/2018-06-01/runtime/invocation/next" {
            return match a.events.lock().unwrap().pop_front() {
                Some((id, ev)) => {
                    let mut r = HttpResponse::new(200);
                    r.headers.set("Lambda-Runtime-Aws-Request-Id", id);
                    r.headers.set("Lambda-Runtime-Deadline-Ms", "1893456000000");
                    r.headers.set("Lambda-Runtime-Invoked-Function-Arn", "arn:aws:lambda:eu-west-1:123456789012:function:orders");
                    r.headers.set("Lambda-Runtime-Trace-Id", "Root=1-5f84c7a1-0123456789abcdef01234567");
                    r.headers.set("Content-Type", "application/json");
                    r.body = ev.into_bytes();
                    r
                }
                // the real API blocks; the tests stop through max messages before this
                None => HttpResponse::new(500),
            };
        }
        if req.method == "POST" && req.path.starts_with("/2018-06-01/runtime/") {
            let kind = req.headers.get("Lambda-Runtime-Function-Error-Type").map(str::to_string);
            a.posts.lock().unwrap().push((req.path.clone(), kind, req.body_text()));
            return HttpResponse::new(202);
        }
        HttpResponse::new(404)
    })
    .unwrap();
    (api, server)
}

fn main_for(server: &HttpServer, max: usize) -> Main {
    let mut main = Main::new();
    main.set_lambda_runtime_api(Some(format!("127.0.0.1:{}", server.port())));
    main.set_property("camel.main.duration-max-messages", max.to_string());
    main.set_property("camel.server.port", "0");
    cameleer_http::register(main.context());
    main
}

#[test]
fn events_go_to_the_handler_and_replies_to_the_response_endpoint() {
    let (api, server) = start_api(&[("req-1", r#"{"name":"Ada"}"#), ("req-2", r#"{"name":"Bob"}"#)]);
    let mut main = main_for(&server, 2);
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("direct:lambda").set_body(simple("Hello ${jsonpath($.name)} (${header.CamelAwsLambdaRequestId}, ${header.CamelAwsLambdaFunctionArn})"));
        })
    });
    main.run().unwrap();
    let posts = api.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 2, "{posts:?}");
    assert_eq!(posts[0].0, "/2018-06-01/runtime/invocation/req-1/response");
    assert_eq!(posts[0].2, "Hello Ada (req-1, arn:aws:lambda:eu-west-1:123456789012:function:orders)");
    assert_eq!(posts[1].0, "/2018-06-01/runtime/invocation/req-2/response");
    assert!(posts[1].2.starts_with("Hello Bob"));
    assert_eq!(main.context().status(), ServiceStatus::Stopped);
    server.stop();
}

#[test]
fn a_failed_exchange_is_reported_to_the_error_endpoint() {
    let (api, server) = start_api(&[("bad-1", r#"{}"#)]);
    let mut main = main_for(&server, 1);
    main.set_property("camel.main.lambda-handler", "direct:orders");
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("direct:orders").process(|_: &mut Exchange| Err(CamelError::illegal_argument("no order id")));
        })
    });
    main.run().unwrap();
    let posts = api.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/2018-06-01/runtime/invocation/bad-1/error");
    let kind = posts[0].1.clone().expect("error type header");
    assert!(posts[0].2.contains("no order id"), "{}", posts[0].2);
    assert!(posts[0].2.contains(&format!("\"errorType\":\"{kind}\"")), "{}", posts[0].2);
    server.stop();
}

#[test]
fn http_events_are_served_by_platform_http_routes() {
    let event = r#"{"version":"2.0","routeKey":"$default","rawPath":"/greet/Ada","rawQueryString":"lang=en",
        "headers":{"accept":"application/json"},
        "requestContext":{"http":{"method":"GET","path":"/greet/Ada"}},"isBase64Encoded":false}"#;
    let (api, server) = start_api(&[("http-1", event)]);
    let mut main = main_for(&server, 1);
    // on Lambda the routes must not bind: the port is taken, so binding would fail startup
    let taken = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    main.set_property("camel.server.port", taken.local_addr().unwrap().port().to_string());
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("platform-http:/greet/{name}")
                .set_header("Content-Type", constant("application/json"))
                .set_body(simple(r#"{"hello":"${header.name}","lang":"${header.lang}"}"#));
        })
    });
    main.run().unwrap();
    let posts = api.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/2018-06-01/runtime/invocation/http-1/response");
    let resp = cameleer_core::json::parse(&posts[0].2).unwrap();
    assert_eq!(resp.get("statusCode"), Some(&Value::Int(200)), "{}", posts[0].2);
    let body = resp.get("body").and_then(|b| b.as_str()).unwrap();
    assert_eq!(body, r#"{"hello":"Ada","lang":"en"}"#);
    assert_eq!(resp.get("isBase64Encoded"), Some(&Value::Bool(false)));
    server.stop();
}

/// An MCP server on Lambda: function URL events for `/mcp` reach the MCP endpoint in process
/// (docs/guides/mcp-server.md, "On AWS Lambda"). Stateless 2026-07-28 requests, as a client
/// sends them to a function that may run in several sandboxes.
#[cfg(feature = "ai")]
#[test]
fn mcp_requests_in_http_events_are_served_by_the_mcp_server() {
    fn event(method: &str, name: Option<&str>, params: &str) -> String {
        let body = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{}}}}}"#,
            method, params
        );
        let name_header = name.map(|n| format!(r#","mcp-name":"{}""#, n)).unwrap_or_default();
        format!(
            r#"{{"version":"2.0","routeKey":"$default","rawPath":"/mcp","rawQueryString":"",
            "headers":{{"content-type":"application/json","accept":"application/json, text/event-stream","mcp-protocol-version":"2026-07-28","mcp-method":"{}"{}}},
            "requestContext":{{"http":{{"method":"POST","path":"/mcp"}}}},"body":{},"isBase64Encoded":false}}"#,
            method,
            name_header,
            Value::String(body).to_json()
        )
    }
    let (api, server) = start_api(&[
        ("mcp-1", &event("tools/list", None, "")),
        ("mcp-2", &event("tools/call", Some("lookup_order"), r#","name":"lookup_order","arguments":{"orderId":"EU-4411"}"#)),
    ]);
    // the routes and properties a function zip holds (bootstrap runs `cameleer run`)
    let mut main = cameleer_main::new_main();
    main.set_lambda_runtime_api(Some(format!("127.0.0.1:{}", server.port())));
    main.set_property("camel.main.duration-max-messages", "2");
    let taken = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
    main.set_property("camel.server.port", taken.local_addr().unwrap().port().to_string());
    main.set_property("camel.server.mcp-enabled", "true");
    main.set_property("camel.server.mcp-tags", "support");
    let routes = std::fs::read_to_string(format!("{}/../../examples/agents/mcp-server.camel.yaml", env!("CARGO_MANIFEST_DIR"))).unwrap();
    main.add_route_source("mcp-server.camel.yaml", routes);
    main.run().unwrap();
    let posts = api.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 2, "{posts:?}");
    let body = |i: usize| {
        let resp = cameleer_core::json::parse(&posts[i].2).unwrap();
        assert_eq!(resp.get("statusCode"), Some(&Value::Int(200)), "{}", posts[i].2);
        resp.get("body").and_then(|b| b.as_str()).unwrap().to_string()
    };
    assert_eq!(posts[0].0, "/2018-06-01/runtime/invocation/mcp-1/response");
    assert!(body(0).contains("\"lookup_order\""), "{}", body(0));
    assert!(body(1).contains("EU-4411") && body(1).contains("shipped"), "{}", body(1));
    server.stop();
}

#[test]
fn a_startup_failure_is_reported_to_the_init_error_endpoint() {
    let (api, server) = start_api(&[]);
    let mut main = main_for(&server, 1);
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("direct:lambda").to("nosuchcomponent:x");
        })
    });
    assert!(main.run().is_err());
    let posts = api.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0].0, "/2018-06-01/runtime/init/error");
    assert!(posts[0].2.contains("nosuchcomponent"), "{}", posts[0].2);
    assert!(posts[0].1.is_some());
    server.stop();
}

#[test]
fn bootstrap_executable_name_is_recognised() {
    assert!(cameleer_main::cli::invoked_as_bootstrap(Some("/var/task/bootstrap")));
    assert!(cameleer_main::cli::invoked_as_bootstrap(Some("bootstrap")));
    assert!(!cameleer_main::cli::invoked_as_bootstrap(Some("/usr/local/bin/cameleer")));
    assert!(!cameleer_main::cli::invoked_as_bootstrap(None));
}
