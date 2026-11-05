//! spring-ws `beanname:` endpoints; cxfrs resource descriptors with entity providers.

use cameleer_core::prelude::*;
use cameleer_soap::cxfrs::{EntityProvider, ResourceDescriptor, ResourceMethod};
use cameleer_soap::springws::SpringWsMapping;

fn ctx() -> CamelContext {
    let ctx = CamelContext::new();
    cameleer_http::register(&ctx);
    cameleer_soap::register(&ctx);
    ctx
}

#[test]
fn spring_ws_beanname_endpoints() {
    let ctx = ctx();
    ctx.registry().bind("quoteMapping", SpringWsMapping::root_qname("{urn:q}GetQuote"));
    ctx.registry().bind("customMapping", SpringWsMapping::new(|r| r.payload.is_some_and(|p| p.contains("vip"))));
    ctx.registry().bind("specMapping", "soapaction:urn:cancel".to_string());
    let a = "spring-ws:beanname:quoteMapping?address=http://127.0.0.1:0/ws";
    ctx.add_routes(move |b| {
        b.from(a).set_body(constant("<q:Quote xmlns:q=\"urn:q\">42</q:Quote>"));
        b.from("spring-ws:beanname:customMapping?address=http://127.0.0.1:0/ws").set_body(constant("<vip/>"));
        b.from("spring-ws:beanname:specMapping?address=http://127.0.0.1:0/ws").set_body(constant("<cancelled/>"));
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, a).unwrap();
    let t = ctx.create_producer_template();
    let url = format!("spring-ws:http://127.0.0.1:{}/ws", port);
    let r = t.request_body(&url, "<q:GetQuote xmlns:q=\"urn:q\"/>").unwrap().to_string();
    assert!(r.contains(">42<"), "{}", r);
    let r = t.request_body(&url, "<order kind=\"vip\"/>").unwrap().to_string();
    assert!(r.contains("vip"), "{}", r);
    let r = t.request_body_and_header(&url, "<x/>", "CamelSpringWebserviceSoapAction", "urn:cancel").unwrap().to_string();
    assert!(r.contains("cancelled"), "{}", r);
    assert!(t.request_body(&url, "<nothing/>").is_err());
    assert!(ctx.get_endpoint("spring-ws:beanname:missing").is_err());
    ctx.stop().unwrap();
}

fn csv_provider() -> EntityProvider {
    EntityProvider::new(
        &["text/csv"],
        |_m, b| {
            let rows: Vec<Value> = String::from_utf8_lossy(b).lines().map(|l| Value::Array(l.split(',').map(|c| Value::from(c.trim())).collect())).collect();
            Ok(Value::Array(rows))
        },
        |_m, v| {
            let rows = v.as_array().cloned().unwrap_or_default();
            Ok(rows.iter().map(|r| r.as_array().map(|c| c.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")).unwrap_or_default()).collect::<Vec<_>>().join("\n").into_bytes())
        },
    )
}

#[test]
fn cxfrs_resource_descriptors_and_providers() {
    let ctx = ctx();
    let customers = ResourceDescriptor::new("/customers")
        .produces("application/json")
        .method(ResourceMethod::new("getCustomer", "GET", "/{id}").produces("application/json").produces("text/csv"))
        .method(ResourceMethod::new("importCustomers", "POST", "/").consumes("text/csv"));
    ctx.registry().bind("customers", customers);
    ctx.registry().bind("csv", csv_provider());
    let uri = "cxfrs:http://127.0.0.1:0/api?resourceClasses=#customers&providers=#csv";
    ctx.add_routes(move |b| {
        b.from(uri).wire_tap("mock:rs").process(|ex: &mut Exchange| {
            let op = ex.message.headers.get("operationName").map(|v| v.to_string()).unwrap_or_default();
            match op.as_str() {
                "getCustomer" => {
                    let id = ex.message.headers.get("id").map(|v| v.to_string()).unwrap_or_default();
                    ex.message.body = Value::Array(vec![Value::Array(vec![Value::from(id), Value::from("Ann")])]);
                }
                "importCustomers" => {
                    let n = ex.message.body.as_array().map(|a| a.len()).unwrap_or(0);
                    ex.message.body = Value::object([("imported", n as i64)]);
                }
                other => panic!("{}", other),
            }
            Ok(())
        });
    })
    .unwrap();
    ctx.start().unwrap();
    let port = cameleer_soap::local_port(&ctx, uri).unwrap();
    let t = ctx.create_producer_template();
    let p = format!("cxfrs:http://127.0.0.1:{}/api?providers=#csv&throwExceptionOnFailure=false", port);
    let get = |accept: &str| {
        let mut ex = Exchange::new(Some(ctx.clone()));
        ex.message.headers.set("CamelHttpMethod", "GET");
        ex.message.headers.set("CamelHttpPath", "/customers/7");
        ex.message.headers.set("Accept", accept);
        ctx.get_endpoint(&p).unwrap().create_producer().unwrap().process(&mut ex).unwrap();
        ex
    };
    // text/csv written by the provider, and read back by the producer's provider
    let ex = get("text/csv");
    assert_eq!(ex.message.headers.get("Content-Type"), Some(&Value::from("text/csv")));
    assert_eq!(ex.message.body, Value::Array(vec![Value::Array(vec![Value::from("7"), Value::from("Ann")])]));
    // JSON by default
    let ex = get("application/json");
    assert_eq!(ex.message.body.to_string(), "[[\"7\",\"Ann\"]]");
    // 406 for an unsupported Accept
    let ex = get("application/xml");
    assert_eq!(ex.message.headers.get("CamelHttpResponseCode"), Some(&Value::Int(406)));
    // a CSV request entity read by the provider
    let r = t
        .request_body_and_headers(&p, Value::Array(vec![Value::Array(vec![Value::from("1"), Value::from("A")]), Value::Array(vec![Value::from("2"), Value::from("B")])]), vec![("CamelHttpMethod", Value::from("POST")), ("CamelHttpPath", Value::from("/customers")), ("Content-Type", Value::from("text/csv"))])
        .unwrap();
    assert!(r.to_string().contains("\"imported\":2"), "{}", r);
    // 415 for an unsupported entity type
    let mut ex = Exchange::new(Some(ctx.clone()));
    ex.message.body = Value::from("x");
    ex.message.headers.set("CamelHttpMethod", "POST");
    ex.message.headers.set("CamelHttpPath", "/customers");
    ex.message.headers.set("Content-Type", "application/pdf");
    ctx.get_endpoint(&p).unwrap().create_producer().unwrap().process(&mut ex).unwrap();
    assert_eq!(ex.message.headers.get("CamelHttpResponseCode"), Some(&Value::Int(415)));
    ctx.stop().unwrap();
}
