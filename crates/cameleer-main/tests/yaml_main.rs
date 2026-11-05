//! The `cameleer` runner (`new_main`) with YAML/XML route files using the REST DSL and
//! route configurations.

use std::path::PathBuf;
use std::time::Duration;

use cameleer_core::prelude::*;
use cameleer_http::HttpClient;

fn temp_file(name: &str, content: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cameleer-main-yaml-{}-{}", std::process::id(), name.replace('.', "-")));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

fn client() -> HttpClient {
    HttpClient { read_timeout: Duration::from_secs(10), ..HttpClient::new() }
}

const APP: &str = r#"
# rest first, restConfiguration afterwards (both orders are supported)
- rest:
    path: /api
    get:
      - path: /hello/{name}
        id: helloRoute
        produces: text/plain
        param:
          - name: name
            type: path
            dataType: string
        to: direct:hello
      - path: /fail
        to:
          uri: direct:fail
    post:
      - path: /echo
        consumes: application/json
        bindingMode: json
        to: direct:echo
- rest:
    path: /other
    get:
      - to: direct:hello
- restConfiguration:
    component: platform-http
    host: 127.0.0.1
    port: 0
    apiProperty:
      - key: api.title
        value: YAML API
- routeConfiguration:
    onException:
      - onException:
          exception:
            - java.lang.IllegalStateException
          handled:
            constant: true
          steps:
            - setHeader:
                name: CamelHttpResponseCode
                constant: 503
            - setBody:
                constant: recovered
- route:
    id: hello
    from:
      uri: direct:hello
      steps:
        - setBody:
            simple: "Hello ${header.name}"
- route:
    id: fail
    from:
      uri: direct:fail
      steps:
        - throwException:
            exceptionType: java.lang.IllegalStateException
            message: boom
- route:
    id: echo
    from:
      uri: direct:echo
      steps:
        - setBody:
            simple: "${body}"
"#;

#[test]
fn main_runs_yaml_with_rest_and_route_configuration() {
    let file = temp_file("app.camel.yaml", APP);
    let mut main = cameleer_main::new_main();
    main.add_route_file(&file);
    main.start().unwrap();
    let ctx = main.context().clone();
    assert!(ctx.route("helloRoute").is_some());
    let port = cameleer_http::platform_http_port(&ctx).expect("platform-http port");
    let base = format!("http://127.0.0.1:{}", port);
    let c = client();

    let r = c.get(&format!("{}/api/hello/world", base)).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (200, "Hello world"));

    // the routeConfiguration (no id) applies to the YAML routes
    let r = c.get(&format!("{}/api/fail", base)).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (503, "recovered"));

    let r = c.post(&format!("{}/api/echo", base), "application/json", br#"{"a":1}"#).unwrap();
    assert_eq!(r.status, 200);
    assert_eq!(cameleer_core::json::parse(&r.body_text()).unwrap().get("a"), Some(&Value::from(1)));

    let r = c.get(&format!("{}/other", base)).unwrap();
    assert_eq!(r.body_text(), "Hello ");

    // one OpenAPI document for all rest elements of the file
    let r = c.get(&format!("{}/api-doc", base)).unwrap();
    assert_eq!(r.status, 200);
    let doc = cameleer_core::json::parse(&r.body_text()).unwrap();
    assert_eq!(doc.get("info").unwrap().get("title"), Some(&Value::from("YAML API")));
    let paths = doc.get("paths").unwrap();
    assert!(paths.get("/api/hello/{name}").is_some(), "{}", doc);
    assert!(paths.get("/other").is_some(), "{}", doc);

    main.stop().unwrap();
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn main_runs_xml_rest_with_configuration_from_earlier_file() {
    let cfg = temp_file(
        "config.camel.yaml",
        r#"
- restConfiguration:
    host: 127.0.0.1
    port: "{{test.port}}"
    contextPath: /svc
    apiDocs: false
"#,
    );
    let xml = temp_file(
        "routes.xml",
        r#"<routes>
  <rest path="/items">
    <get path="/{id}">
      <to uri="direct:item"/>
    </get>
  </rest>
  <route id="item">
    <from uri="direct:item"/>
    <setBody><simple>item ${header.id}</simple></setBody>
  </route>
</routes>"#,
    );
    let mut main = cameleer_main::new_main();
    main.set_property("test.port", "0");
    main.add_route_file(&cfg);
    main.add_route_file(&xml);
    main.start().unwrap();
    let port = cameleer_http::platform_http_port(main.context()).expect("port");
    let c = client();
    let r = c.get(&format!("http://127.0.0.1:{}/svc/items/7", port)).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (200, "item 7"));
    assert_eq!(c.get(&format!("http://127.0.0.1:{}/svc/api-doc", port)).unwrap().status, 404);
    main.stop().unwrap();
    let _ = std::fs::remove_dir_all(cfg.parent().unwrap());
    let _ = std::fs::remove_dir_all(xml.parent().unwrap());
}

#[test]
fn invalid_rest_reports_location() {
    let file = temp_file("bad.camel.yaml", "- rest:\n    path: /x\n    get:\n      - path: /y\n        bindingMode: weird\n        to: direct:y\n");
    let mut main = cameleer_main::new_main();
    main.add_route_file(&file);
    let e = main.start().unwrap_err();
    assert!(e.message.contains("Unsupported rest binding mode: weird"), "{}", e);
    assert!(e.message.contains("bad.camel.yaml"), "{}", e);
    let _ = std::fs::remove_dir_all(file.parent().unwrap());
}

#[test]
fn rest_configuration_in_a_later_file_applies() {
    let routes = temp_file(
        "a-routes.camel.yaml",
        r#"
- rest:
    path: /late
    get:
      - to: direct:late
- route:
    from:
      uri: direct:late
      steps:
        - setBody:
            constant: configured later
"#,
    );
    let cfg = temp_file(
        "b-config.camel.yaml",
        r#"
- restConfiguration:
    host: 127.0.0.1
    port: 0
    contextPath: /ctx
    apiDocs: false
"#,
    );
    let mut main = cameleer_main::new_main();
    main.add_route_file(&routes);
    main.add_route_file(&cfg);
    main.start().unwrap();
    let port = cameleer_http::platform_http_port(main.context()).expect("port");
    let r = client().get(&format!("http://127.0.0.1:{}/ctx/late", port)).unwrap();
    assert_eq!((r.status, r.body_text().as_str()), (200, "configured later"));
    main.stop().unwrap();
    let _ = std::fs::remove_dir_all(routes.parent().unwrap());
    let _ = std::fs::remove_dir_all(cfg.parent().unwrap());
}
