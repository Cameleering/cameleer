//! Service Call, Script, Poll and removeVariables EIPs.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camel_core::prelude::*;
use camel_core::processor::service_call::{FilterConfig, ServiceCallDefinition, ServiceDefinition, ServiceLoadBalancer, SERVICE_CALL_SERVICE_HOST};

fn backends(b: &mut RouteBuilder) {
    for n in ["a", "b", "c"] {
        b.from(format!("direct:{}", n)).transform(simple(format!("{}:${{body}}", n)));
    }
}

#[test]
fn service_call_round_robin_across_static_servers() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        backends(b);
        b.from("direct:start").service_call("svc").static_service_discovery("svc@a:8001,svc@b:8002,other@c:1").service_call_uri("direct:{host}").to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    let r: Vec<Value> = (0..4).map(|i| t.request_body("direct:start", i).unwrap()).collect();
    assert_eq!(r, vec![Value::from("a:0"), Value::from("b:1"), Value::from("a:2"), Value::from("b:3")]);
    let out = ctx.mock_endpoint("out").unwrap().exchange(1).unwrap();
    assert_eq!(out.header(SERVICE_CALL_SERVICE_HOST), Some(&Value::from("b")));
    assert_eq!(out.header("CamelServiceCallServicePort"), Some(&Value::from(8002)));
}

#[test]
fn service_call_filters_expression_and_random() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        backends(b);
        b.from("direct:blacklist").service_call("svc").static_service_discovery("svc@a:1,svc@b:2").blacklist_service_filter("svc@a:1").service_call_uri("direct:{host}");
        b.from("direct:expr")
            .service_call("svc")
            .static_service_discovery("a:1,b:2")
            .service_call_expression(simple("direct:${header.CamelServiceCallServiceHost}"));
        b.from("direct:random")
            .service_call("svc")
            .static_service_discovery("a:1,b:2,c:3")
            .service_call_load_balancer(ServiceLoadBalancer::Random)
            .service_call_uri("direct:{host}");
        b.from("direct:healthy")
            .service_call("svc")
            .service_discovery(|_name: &str| {
                let mut down = ServiceDefinition::new(None, "a", 1);
                down.healthy = false;
                Ok(vec![down, ServiceDefinition::new(None, "c", 3)])
            })
            .service_filter(FilterConfig::Healthy)
            .service_call_uri("direct:{host}");
        b.from("direct:none").service_call("svc").static_service_discovery("other@a:1").service_call_uri("direct:{host}");
        let def = ServiceCallDefinition::new("svc").static_servers("b:2").unwrap().uri("direct:{host}");
        b.from("direct:with").service_call_with(def);
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    for _ in 0..3 {
        assert_eq!(t.request_body("direct:blacklist", "x").unwrap(), Value::from("b:x"));
        assert_eq!(t.request_body("direct:healthy", "x").unwrap(), Value::from("c:x"));
    }
    assert_eq!(t.request_body("direct:expr", "1").unwrap(), Value::from("a:1"));
    assert_eq!(t.request_body("direct:expr", "2").unwrap(), Value::from("b:2"));
    for _ in 0..10 {
        let r = t.request_body("direct:random", "r").unwrap().to_string();
        assert!(["a:r", "b:r", "c:r"].contains(&r.as_str()), "{}", r);
    }
    assert_eq!(t.request_body("direct:with", "w").unwrap(), Value::from("b:w"));
    let e = t.request_body("direct:none", "x").unwrap_err();
    assert!(format!("{}", e).contains("No active services with name svc"), "{}", e);
}

#[test]
fn service_call_requires_discovery() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:x").service_call("svc");
    })
    .unwrap();
    assert!(ctx.start().is_err());
}

#[test]
fn script_evaluates_in_any_language_and_keeps_the_message() {
    let ctx = CamelContext::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    ctx.add_routes(move |b| {
        b.from("direct:start")
            .script(simple("${body} ${header.foo}"))
            .script(constant("ignored"))
            .script(header("foo"))
            .script(language("jsonpath", "$.a"))
            .script(language("jq", ".a"))
            .script(expr(move |_ex| {
                c.fetch_add(1, Ordering::SeqCst);
                Ok(Value::Null)
            }))
            .to("mock:out");
    })
    .unwrap();
    ctx.start().unwrap();
    ctx.create_producer_template().send_body_and_header("direct:start", r#"{"a":1}"#, "foo", "bar").unwrap();
    assert_eq!(ctx.mock_endpoint("out").unwrap().received_bodies(), vec![Value::from(r#"{"a":1}"#)]);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // an unknown language fails the route creation
    let ctx2 = CamelContext::new();
    ctx2.add_routes(|b| {
        b.from("direct:x").script(language("groovy", "1+1"));
    })
    .unwrap();
    assert!(ctx2.start().is_err());
}

#[test]
fn poll_receives_one_message() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:poll").poll("seda:queue", 1000).to("mock:out");
        b.from("direct:empty").poll("seda:${header.q}", 0).to("mock:empty");
    })
    .unwrap();
    ctx.start().unwrap();
    let t = ctx.create_producer_template();
    t.send_body("seda:queue", "hello").unwrap();
    t.send_body("direct:poll", "ignored").unwrap();
    assert_eq!(ctx.mock_endpoint("out").unwrap().received_bodies(), vec![Value::from("hello")]);
    t.send_body_and_header("direct:empty", "x", "q", "nothing").unwrap();
    assert_eq!(ctx.mock_endpoint("empty").unwrap().received_bodies(), vec![Value::Null]);
}

#[test]
fn remove_variables_by_pattern() {
    let ctx = CamelContext::new();
    ctx.add_routes(|b| {
        b.from("direct:start")
            .set_variable("foo1", constant(1))
            .set_variable("foo2", constant(2))
            .set_variable("bar", constant(3))
            .remove_variables("foo*")
            .set_body(simple("${variable.foo1}-${variable.foo2}-${variable.bar}"));
    })
    .unwrap();
    ctx.start().unwrap();
    assert_eq!(ctx.create_producer_template().request_body("direct:start", "").unwrap(), Value::from("--3"));
}
