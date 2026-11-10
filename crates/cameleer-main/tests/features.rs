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
        ("ai", added(cameleer_ai::register)),
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

/// `cameleer tool ...` and `/q/dev/tools` say they need the `ai` feature.
#[cfg(not(feature = "ai"))]
#[test]
fn tool_commands_name_the_ai_feature() {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    assert_eq!(cameleer_main::cli::run_status(&args(&["tool", "list"]), &mut out, &mut err), 1);
    let err = String::from_utf8(err).unwrap();
    assert!(err.contains("built without the `ai` feature"), "{err}");
    let ctx = cameleer_core::context::CamelContext::new();
    let cfg = cameleer_main::console::ConsoleConfig { enabled: true, dev_console: true, ..Default::default() };
    let r = cameleer_main::console::handle_request(&ctx, &cfg, &cameleer_http::http::HttpRequest::new("GET", "/q/dev/tools"));
    assert_eq!(r.status, 501);
    assert!(String::from_utf8_lossy(&r.body).contains("`ai` feature"));
}

/// Every component, language and data format page names the cargo feature that provides it
/// (`**Cargo feature:**` in its header line), and that name matches `features::feature_for`, so
/// people and coding agents can trust the page when they pick features for a smaller build.
#[test]
fn reference_pages_name_their_cargo_feature() {
    use cameleer_main::features::feature_for;
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    // features named in the header's `**Cargo feature:**` segment; empty when "none"
    fn named(text: &str) -> Option<Vec<String>> {
        let at = text.find("**Cargo feature:**")?;
        let seg = &text[at + 18..];
        let seg = &seg[..seg.find(" · ").or_else(|| seg.find('\n')).unwrap_or(seg.len())];
        Some(seg.split('`').skip(1).step_by(2).map(str::to_string).collect())
    }
    fn first_code(text: &str, label: &str) -> Option<String> {
        let at = text.find(label)? + label.len();
        let rest = &text[at..];
        let a = rest.find('`')? + 1;
        let b = a + rest[a..].find('`')?;
        Some(rest[a..b].trim_end_matches(':').to_string())
    }
    let family = |crate_name: &str| -> Option<&'static str> {
        ["ai", "amqp", "amqp10", "cloud", "dataformats", "datastores", "grpc", "kafka-mqtt", "protocols", "saas", "script", "soap", "sql", "ssh", "xml"]
            .into_iter()
            .find(|f| crate_name == format!("cameleer-{f}"))
    };
    let mut checked = 0;
    let mut wrong = Vec::new();
    for (dir, kind, label) in [("components", "component", "**Scheme:**"), ("languages", "language", "**Name:**"), ("dataformats", "dataformat", "**Name:**")] {
        for entry in std::fs::read_dir(docs.join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let page = path.file_name().unwrap().to_string_lossy().to_string();
            let Some(listed) = named(&text) else {
                wrong.push(format!("{dir}/{page}: no **Cargo feature:** in the header"));
                continue;
            };
            let expected = if kind == "dataformat" {
                first_code(&text, "**Crate:**").and_then(|c| family(&c)).map(str::to_string)
            } else {
                let name = first_code(&text, label).unwrap_or_default();
                feature_for(kind, &name).map(str::to_string)
            };
            let ok = match &expected {
                Some(f) => listed.contains(f),
                // a part of core may still mention an optional extra (HTTP/2 on `http`)
                None => text[text.find("**Cargo feature:**").unwrap()..].starts_with("**Cargo feature:** none"),
            };
            if !ok {
                wrong.push(format!("{dir}/{page}: page lists {listed:?}, the code says {expected:?}"));
            }
            checked += 1;
        }
    }
    // pack pages: the `health` feature, the pack crate and a **Pack:** segment
    for dir in ["components", "languages", "dataformats"] {
        for entry in std::fs::read_dir(docs.join("packs/health").join(dir)).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let page = format!("packs/health/{dir}/{}", path.file_name().unwrap().to_string_lossy());
            let listed = named(&text).unwrap_or_default();
            let crate_name = first_code(&text, "**Crate:**").unwrap_or_default();
            if !listed.contains(&"health".to_string()) || crate_name != "cameleer-health" || !text.contains("**Pack:** [health]") {
                wrong.push(format!("{page}: a health pack page must name crate cameleer-health, the health pack and feature `health` (lists {listed:?}, crate {crate_name})"));
            }
            checked += 1;
        }
    }
    assert!(checked > 200, "only {checked} pages checked");
    assert!(wrong.is_empty(), "reference pages with a wrong cargo feature:\n{}", wrong.join("\n"));
}

#[cfg(not(any(feature = "ai", feature = "saas", feature = "cloud")))]
#[test]
fn the_agent_step_names_its_features() {
    let ctx = cameleer_core::context::CamelContext::new();
    let yaml = "- route:\n    from:\n      uri: direct:a\n      steps:\n        - agent:\n            model: openai:chat-completion?model=m\n";
    let err = cameleer_yaml::add_routes_from_yaml(&ctx, yaml).err().expect("refused");
    assert!(err.message.contains("ai, saas or cloud feature"), "{}", err.message);
}

/// The health pack is not part of `full`, and the PACKS name table matches what the pack
/// describes and registers.
#[test]
fn the_health_pack_is_separate_and_its_table_matches() {
    let toml = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
    let full = toml.split("full = [").nth(1).and_then(|r| r.split(']').next()).unwrap();
    assert!(!full.contains("health"), "full must not enable the health pack: {full}");
    let (_, comps, formats, langs) = cameleer_main::features::PACKS.iter().find(|(p, ..)| *p == "health").unwrap();
    let info = &cameleer_health::PACK;
    assert_eq!(info.components, *comps);
    assert_eq!(info.data_formats, *formats);
    assert_eq!(info.languages, *langs);
    let ctx = cameleer_core::context::CamelContext::new();
    ctx.add_pack(&cameleer_health::HEALTH);
    for c in info.components {
        assert!(ctx.component_names().iter().any(|n| n == c), "{c}");
    }
    // no family claims a pack component
    for c in info.components {
        assert_eq!(cameleer_main::features::feature_for("component", c), None, "{c}");
    }
}
