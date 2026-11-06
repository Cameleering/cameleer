//! The component-family table (`cameleer_main::features`) against what each family registers,
//! and the error a route gets for a component left out of the build.

#[cfg(feature = "full")]
use cameleer_core::context::CamelContext;
#[cfg(feature = "full")]
use cameleer_main::features;

#[cfg(feature = "full")]
fn added(register: impl Fn(&CamelContext)) -> Vec<String> {
    let before = CamelContext::new().component_names();
    let ctx = CamelContext::new();
    register(&ctx);
    ctx.component_names().into_iter().filter(|n| !before.contains(n)).collect()
}

/// Every component a family registers maps back to that family, so a build without it can
/// name the feature to enable.
#[cfg(feature = "full")]
#[test]
fn table_covers_every_registered_component() {
    let families: Vec<(&str, Vec<String>)> = vec![
        ("xml", added(cameleer_xml::register)),
        ("ssh", added(cameleer_ssh::register)),
        ("kafka-mqtt", added(cameleer_kafka_mqtt::register)),
        ("sql", added(cameleer_sql::register)),
        ("amqp", added(cameleer_amqp::register)),
        ("amqp10", added(cameleer_amqp10::register)),
        ("grpc", added(cameleer_grpc::register)),
        ("cloud", added(cameleer_cloud::register)),
        ("datastores", added(cameleer_datastores::register)),
        ("script", added(cameleer_script::register)),
        ("saas", added(cameleer_saas::register)),
        ("soap", added(cameleer_soap::register)),
        ("dataformats", added(cameleer_dataformats::register)),
        // protocols registers a placeholder `sftp` that the ssh family replaces
        ("protocols", added(cameleer_protocols::register).into_iter().filter(|s| s != "sftp").collect()),
    ];
    for (family, schemes) in families {
        assert!(!schemes.is_empty(), "{family}");
        for s in schemes {
            assert_eq!(features::feature_for("component", &s), Some(family), "component {s}");
        }
    }
}

#[cfg(feature = "full")]
#[test]
fn table_covers_every_registered_language() {
    let core = CamelContext::new().language_names();
    for (family, register) in [
        ("script", cameleer_script::register as fn(&CamelContext)),
        ("protocols", cameleer_protocols::register),
    ] {
        let ctx = CamelContext::new();
        register(&ctx);
        for l in ctx.language_names().into_iter().filter(|l| !core.contains(l)) {
            assert_eq!(features::feature_for("language", &l), Some(family), "language {l}");
        }
    }
}

#[cfg(feature = "full")]
#[test]
fn the_full_build_has_every_family_and_no_hints() {
    assert!(features::is_full());
    assert_eq!(features::enabled().len(), features::FEATURES.len());
    let main = cameleer_main::new_main();
    let e = main.context().check_endpoint("nosuch:x").unwrap_err();
    assert!(!e.message.contains("feature"), "{}", e.message);
}

/// In a build without the cloud family a cloud route fails with the feature to enable.
#[cfg(not(feature = "cloud"))]
#[test]
fn a_missing_family_names_its_feature() {
    let main = cameleer_main::new_main();
    let e = main.context().check_endpoint("aws2-s3:bucket").unwrap_err();
    assert!(e.message.contains("No component found with scheme: aws2-s3"), "{}", e.message);
    assert!(e.message.contains("`cloud` feature"), "{}", e.message);
    // core components still work
    main.context().check_endpoint("timer:t?period=10").unwrap();
    main.context().check_endpoint("platform-http:/hello").unwrap();
}

#[cfg(not(feature = "script"))]
#[test]
fn a_missing_language_names_its_feature() {
    let main = cameleer_main::new_main();
    let e = main.context().resolve_language("groovy").err().expect("no groovy");
    assert!(e.message.contains("`script` feature"), "{}", e.message);
    assert!(main.context().resolve_language("simple").is_ok());
}

/// The optional parts of core (template components, jq, the core XPath) map to their features.
#[cfg(feature = "full")]
#[test]
fn core_parts_are_in_the_full_build_and_in_the_table() {
    let ctx = CamelContext::new();
    for c in ["freemarker", "mustache", "string-template", "velocity"] {
        assert!(ctx.component_names().contains(&c.to_string()), "{c}");
        assert_eq!(features::feature_for("component", c), Some("templates"), "{c}");
    }
    for l in ["jq", "xpath"] {
        assert!(ctx.resolve_language(l).is_ok(), "{l}");
        assert_eq!(features::feature_for("language", l), Some(l));
    }
}

#[cfg(not(feature = "templates"))]
#[test]
fn template_components_name_the_templates_feature() {
    let main = cameleer_main::new_main();
    for uri in ["velocity:t.vm", "freemarker:t.ftl", "mustache:t.mustache", "string-template:t.st"] {
        let e = main.context().check_endpoint(uri).unwrap_err();
        assert!(e.message.contains("No component found with scheme"), "{}", e.message);
        assert!(e.message.contains("`templates` feature"), "{}", e.message);
    }
}

/// The `jq` language and Simple's `${jq(...)}` both fail with the feature to enable.
#[cfg(not(feature = "jq"))]
#[test]
fn jq_names_the_jq_feature() {
    use std::collections::BTreeMap;
    let main = cameleer_main::new_main();
    let ctx = main.context();
    let e = ctx.resolve_language("jq").err().expect("no jq");
    assert!(e.message.contains("`jq` feature"), "{}", e.message);
    let simple = ctx.resolve_language("simple").unwrap().create_expression("${jq(.a)}", &BTreeMap::new());
    let e = match simple {
        Err(e) => e,
        Ok(x) => {
            let mut ex = cameleer_core::message::Exchange::new(Some(ctx.clone()));
            ex.message.body = "{\"a\":1}".into();
            x.evaluate(&ex).unwrap_err()
        }
    };
    assert!(e.message.contains("`jq` feature"), "{}", e.message);
}

/// Without `xpath` (and without the `xml` family, which has its own XPath) the `xpath`
/// language and Simple's `${xpath(...)}` fail with the feature to enable.
#[cfg(not(any(feature = "xpath", feature = "xml")))]
#[test]
fn xpath_names_the_xpath_feature() {
    use std::collections::BTreeMap;
    let main = cameleer_main::new_main();
    let ctx = main.context();
    let e = ctx.resolve_language("xpath").err().expect("no xpath");
    assert!(e.message.contains("`xpath` feature"), "{}", e.message);
    let simple = ctx.resolve_language("simple").unwrap().create_expression("${xpath(/a)}", &BTreeMap::new());
    let e = match simple {
        Err(e) => e,
        Ok(x) => {
            let mut ex = cameleer_core::message::Exchange::new(Some(ctx.clone()));
            ex.message.body = "<a>1</a>".into();
            x.evaluate(&ex).unwrap_err()
        }
    };
    assert!(e.message.contains("`xpath` feature"), "{}", e.message);
}

/// The http producer rejects `httpVersion=HTTP/2` in a build without `http2`; HTTP/1.1 works.
#[cfg(not(feature = "http2"))]
#[test]
fn http2_names_the_http2_feature() {
    let main = cameleer_main::new_main();
    let ctx = main.context();
    for v in ["HTTP/2", "2", "h2c"] {
        let e = ctx.check_endpoint(&format!("http://localhost:1/x?httpVersion={v}")).unwrap_err();
        assert!(e.message.contains("`http2` feature"), "{}", e.message);
    }
    ctx.check_endpoint("http://localhost:1/x?httpVersion=HTTP/1.1").unwrap();
}

/// `cameleer validate` and `cameleer schema` say which feature they need.
#[cfg(not(feature = "validate"))]
#[test]
fn validate_and_schema_name_the_validate_feature() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(cameleer_main::cli::run_status(&args(&["validate", "routes.yaml"]), &mut out, &mut err), 2);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("`cameleer validate` is not in this binary") && err.contains("--features validate"), "{err}");
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(cameleer_main::cli::run_status(&args(&["schema"]), &mut out, &mut err), 1);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("`cameleer schema` is not in this binary") && err.contains("`validate` feature"), "{err}");
}
