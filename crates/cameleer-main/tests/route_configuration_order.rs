//! Route configurations apply to every route whatever the order of the files, as in Camel: a
//! top-level `interceptSendToEndpoint` or an anonymous `routeConfiguration` in one file applies
//! to the routes of files loaded before it, and a `routeConfigurationId` may name a
//! configuration defined in a later file.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use cameleer_core::value::Value;

const ORDERS: &str = r#"
- route:
    id: orders
    from:
      uri: direct:orders
      steps:
        - to: mock:kafka-orders
- route:
    id: billing
    routeConfigurationId: audit
    from:
      uri: direct:billing
      steps:
        - to: mock:billing
"#;

const INTERCEPT: &str = r#"
- interceptSendToEndpoint:
    uri: mock:kafka-*
    skipSendToOriginalEndpoint: true
    steps:
      - to: mock:intercepted
- routeConfiguration:
    id: audit
    intercept:
      - intercept:
          steps:
            - to: mock:audited
"#;

fn dir() -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let d = std::env::temp_dir().join(format!("cameleer-routecfg-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(files: &[(&str, &str)]) -> cameleer_main::Main {
    let d = dir();
    let mut main = cameleer_main::new_main();
    for (name, text) in files {
        let f = d.join(name);
        std::fs::write(&f, text).unwrap();
        main.add_route_file(f.to_string_lossy().to_string());
    }
    main.start().unwrap();
    main
}

fn received(main: &cameleer_main::Main, mock: &str) -> usize {
    main.context().mock_endpoint(mock).map(|m| m.received_counter()).unwrap_or(0)
}

#[test]
fn configurations_in_a_later_file_apply_to_earlier_routes() {
    for order in [[("a-orders.camel.yaml", ORDERS), ("b-intercept.camel.yaml", INTERCEPT)], [("a-intercept.camel.yaml", INTERCEPT), ("b-orders.camel.yaml", ORDERS)]] {
        let main = run(&order);
        let t = main.context().create_producer_template();
        t.send_body("direct:orders", Value::from("o1")).unwrap();
        t.send_body("direct:billing", Value::from("b1")).unwrap();
        assert_eq!(received(&main, "mock:intercepted"), 1, "the intercept applies whatever the file order ({:?})", order.map(|o| o.0));
        assert_eq!(received(&main, "mock:kafka-orders"), 0, "the original send is skipped");
        assert!(received(&main, "mock:audited") >= 1, "routeConfigurationId may name a configuration from a later file");
        assert_eq!(received(&main, "mock:billing"), 1);
        drop(main);
    }
}

#[test]
fn an_unknown_route_configuration_id_still_fails_at_start() {
    let d = dir();
    let f = d.join("bad.camel.yaml");
    std::fs::write(&f, ORDERS).unwrap();
    let mut main = cameleer_main::new_main();
    main.add_route_file(f.to_string_lossy().to_string());
    let err = main.start().unwrap_err();
    assert!(err.message.contains("Cannot find routeConfiguration with id 'audit'"), "{}", err.message);
}
