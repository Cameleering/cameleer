//! `camel.metrics.*` in `Main`: Camel's built-in meters and the `micrometer` component on the
//! Prometheus endpoint (`/q/metrics` and `/observe/metrics`), and the metrics logged at shutdown.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_http::HttpClient;

const ROUTES: &str = r#"
- route:
    id: orders
    from:
      uri: timer:orders?repeatCount=3&delay=1&period=1
      steps:
        - to: micrometer:counter:orders.created?tags.source=timer
        - to: mock:orders
"#;

#[test]
fn camel_metrics_on_the_prometheus_endpoint() {
    let mut main = cameleer_main::new_main();
    for (k, v) in [
        ("camel.metrics.enabled", "true"),
        ("camel.metrics.logMetricsOnShutdown", "true"),
        ("camel.metrics.logMetricsOnShutdownFormat", "prometheus"),
        ("camel.metrics.logMetricsOnShutdownFilters", "orders*"),
        ("camel.server.enabled", "true"),
        ("camel.server.metricsEnabled", "true"),
        ("camel.server.host", "127.0.0.1"),
        ("camel.server.port", "0"),
    ] {
        main.set_property(k, v);
    }
    main.add_route_source("orders.camel.yaml", ROUTES);
    let logged = Arc::new(Mutex::new(Vec::new()));
    let l2 = logged.clone();
    cameleer_core::log::add_sink(move |r| {
        if r.logger == "MicrometerPrometheus" {
            l2.lock().unwrap().push(r.message.clone());
        }
    });
    main.start().unwrap();
    let ctx = main.context().clone();
    let mock = ctx.mock_endpoint("mock:orders").unwrap();
    for _ in 0..200 {
        if mock.received_counter() >= 3 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(mock.received_counter(), 3);
    let base = format!("http://127.0.0.1:{}", main.console_port().unwrap());
    let c = ctx.name().to_string();
    for path in ["/q/metrics", "/observe/metrics"] {
        let text = HttpClient::new().get(&format!("{base}{path}")).unwrap().body_text();
        assert!(text.contains(&format!("orders_created_total{{camelContext=\"{c}\",source=\"timer\"}} 3.0")), "{path}: {text}");
        assert!(text.contains(&format!("camel_exchanges_total{{camelContext=\"{c}\",eventType=\"route\",kind=\"CamelRoute\",routeId=\"orders\"}} 3.0")), "{text}");
        assert!(text.contains("camel_route_policy_seconds_count{"), "{text}");
        assert!(text.contains("# TYPE camel_exchange_event_notifier_seconds summary"), "{text}");
    }
    main.stop().unwrap();
    let logged = logged.lock().unwrap().join("\n");
    assert!(logged.contains("orders_created_total"), "{logged}");
    assert!(!logged.contains("camel_route_policy"), "{logged}");
}

#[test]
fn both_route_policies_are_refused_and_metrics_are_off_by_default() {
    let mut main = cameleer_main::new_main();
    main.set_property("camel.metrics.enabled", "true");
    main.set_property("camel.opentelemetry.metrics.enabled", "true");
    let e = main.start().unwrap_err();
    assert!(e.message.contains("choose one"), "{}", e.message);

    let mut main = cameleer_main::new_main();
    main.add_route_source("orders.camel.yaml", "- route:\n    from:\n      uri: direct:x\n      steps:\n        - to: mock:x\n");
    main.start().unwrap();
    // nothing measured, no registry created
    assert!(cameleer_core::metrics::MeterRegistry::existing(main.context()).is_none());
    main.stop().unwrap();
}
