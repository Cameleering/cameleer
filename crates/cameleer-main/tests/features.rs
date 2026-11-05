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
