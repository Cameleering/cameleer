//! camel-main configuration (application.yaml, profiles, env, aliases, beans, component
//! options), Camel K Integrations, dev reload and the `camel` CLI commands.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use camel_core::prelude::*;
use camel_main::{cli, local};

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("camel-main-cli-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cli_out(args: &[&str]) -> std::result::Result<String, String> {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    cli::run(&args, &mut out)?;
    Ok(String::from_utf8(out).unwrap())
}

fn wait_until(mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn application_yaml_profiles_env_aliases_beans_and_component_options() {
    let dir = temp_dir("config");
    std::fs::write(
        dir.join("application.yaml"),
        "camel:\n  springboot:\n    name: yaml-app\n  component:\n    timer:\n      repeat-count: 1\n  beans:\n    greeter: \"#class:com.acme.Greeter\"\n    greeter.prefix: Hello\ngreeting: base\nserver:\n  port: 0\n",
    )
    .unwrap();
    std::fs::write(dir.join("application-test.properties"), "greeting=from-profile\n%test.other=quarkus-style\n").unwrap();
    camel_yaml::register_bean_factory("com.acme.Greeter", |ctx: &CamelContext, def: &camel_yaml::BeanDefinition| {
        let prefix = def.properties.get("prefix").map(|v| v.to_string()).unwrap_or_default();
        ctx.registry().bind(def.name.clone(), prefix);
        Ok(())
    });
    let mut main = camel_main::new_main();
    assert_eq!(main.load_application_config(&dir).unwrap(), 1);
    main.set_profile("test");
    let env: BTreeMap<String, String> = [("CAMEL_MAIN_DURATION_MAX_MESSAGES".to_string(), "1".to_string())].into_iter().collect();
    main.set_environment(env);
    main.configure(|ctx| {
        ctx.add_routes(|b| {
            b.from("timer:t").set_body(simple("{{greeting}} {{other}}")).to("mock:t");
        })
    });
    main.start().unwrap();
    let ctx = main.context().clone();
    assert!(ctx.mock_endpoint("mock:t").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(ctx.mock_endpoint("mock:t").unwrap().exchange(0).unwrap().body(), &Value::from("from-profile quarkus-style"));
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(ctx.mock_endpoint("mock:t").unwrap().received_counter(), 1, "camel.component.timer.repeat-count");
    assert_eq!(main.properties().get("camel.main.name").map(String::as_str), Some("yaml-app"));
    assert_eq!(main.properties().get("camel.main.durationMaxMessages").map(String::as_str), Some("1"));
    assert_eq!(main.properties().get("camel.server.port").map(String::as_str), Some("0"));
    assert_eq!(ctx.registry().lookup::<String>("greeter").map(|s| (*s).clone()), Some("Hello".into()));
    main.stop().unwrap();
}

#[test]
fn camel_k_integration_resource() {
    let dir = temp_dir("integration");
    let f = dir.join("hello.yaml");
    std::fs::write(
        &f,
        "apiVersion: camel.apache.org/v1\nkind: Integration\nmetadata:\n  name: hello-k\nspec:\n  traits:\n    camel:\n      properties:\n        - greeting=Hi from K\n    container:\n      image: ignored\n  configuration:\n    - type: property\n      value: target=mock:k\n  flows:\n    - from:\n        uri: direct:k\n        steps:\n          - setBody:\n              simple: \"{{greeting}}\"\n          - to: \"{{target}}\"\n  sources:\n    - name: more.yaml\n      content: |\n        - from:\n            uri: direct:more\n            steps:\n              - to: mock:more\n",
    )
    .unwrap();
    let mut main = camel_main::new_main();
    main.add_route_file(&f);
    main.start().unwrap();
    let t = main.context().create_producer_template();
    t.send_body("direct:k", "x").unwrap();
    t.send_body("direct:more", "y").unwrap();
    assert_eq!(main.context().mock_endpoint("mock:k").unwrap().exchange(0).unwrap().body(), &Value::from("Hi from K"));
    assert_eq!(main.context().mock_endpoint("mock:more").unwrap().received_counter(), 1);
    assert_eq!(main.properties().get("camel.main.name").map(String::as_str), Some("hello-k"));
    main.stop().unwrap();
}

#[test]
fn init_bind_and_export() {
    let dir = temp_dir("init");
    let d = dir.to_string_lossy().to_string();
    for f in ["route.yaml", "route.xml", "my-source.kamelet.yaml", "p.pipe.yaml", "it.integration.yaml"] {
        assert!(cli_out(&["init", f, "--directory", &d]).unwrap().contains("Created"));
        assert!(dir.join(f).exists());
    }
    assert!(cli_out(&["init", "route.yaml", "--directory", &d]).is_err(), "existing file");
    // the generated templates load
    let ctx = CamelContext::new();
    camel_yaml::register_kamelet_component(&ctx);
    camel_yaml::add_routes_from_yaml(&ctx, &std::fs::read_to_string(dir.join("route.yaml")).unwrap()).unwrap();
    camel_yaml::add_routes_from_xml(&ctx, &std::fs::read_to_string(dir.join("route.xml")).unwrap()).unwrap();
    camel_yaml::add_routes_from_yaml(&ctx, &std::fs::read_to_string(dir.join("my-source.kamelet.yaml")).unwrap()).unwrap();
    camel_yaml::add_routes_from_yaml(&ctx, &std::fs::read_to_string(dir.join("p.pipe.yaml")).unwrap()).unwrap();
    ctx.start().unwrap();
    ctx.stop().unwrap();

    let pipe = dir.join("bound.yaml");
    let out = cli_out(&[
        "bind", "bound", "--source", "timer-source", "--step", "log-action", "--sink", "mock:bound", "--property", "source.message=hi",
        "--property", "source.period=5", "--error-handler", "sink:mock:errors", "--output", &pipe.to_string_lossy(),
    ])
    .unwrap();
    assert!(out.contains("Created"));
    let ctx = CamelContext::new();
    camel_yaml::register_kamelet_component(&ctx);
    camel_yaml::add_routes_from_yaml(&ctx, &std::fs::read_to_string(&pipe).unwrap()).unwrap();
    ctx.start().unwrap();
    assert!(ctx.mock_endpoint("mock:bound").unwrap().wait_for_messages(1, Duration::from_secs(5)));
    assert_eq!(ctx.mock_endpoint("mock:bound").unwrap().exchange(0).unwrap().body(), &Value::from("hi"));
    ctx.stop().unwrap();

    let export_dir = dir.join("exported");
    std::fs::write(dir.join("application.properties"), "greeting=hi\n").unwrap();
    let files = vec![dir.join("route.yaml"), dir.join("application.properties")];
    let created = cli::export_project(&files, &export_dir, "my app", std::path::Path::new("/opt/rust-camel/crates")).unwrap();
    assert!(created.len() >= 4);
    let cargo = std::fs::read_to_string(export_dir.join("Cargo.toml")).unwrap();
    assert!(cargo.contains("name = \"my-app\"") && cargo.contains("/opt/rust-camel/crates/camel-main") && cargo.contains("[workspace]"));
    let main_rs = std::fs::read_to_string(export_dir.join("src/main.rs")).unwrap();
    assert!(main_rs.contains("include_str!(\"../resources/route.yaml\")"));
    assert!(main_rs.contains("load_properties_text(include_str!(\"../resources/application.properties\"), false)"));
    assert!(export_dir.join("resources/route.yaml").exists());
}

#[test]
fn local_status_commands_and_dev_reload() {
    let home = temp_dir("home");
    std::env::set_var("CAMEL_HOME", &home);
    let routes = home.join("routes.yaml");
    std::fs::write(&routes, "- route:\n    id: r1\n    from:\n      uri: direct:a\n      steps:\n        - setBody:\n            constant: v1\n        - to: mock:a\n").unwrap();
    let mut main = camel_main::new_main();
    main.add_route_file(&routes);
    main.set_property("camel.main.name", "status-app");
    main.set_property("camel.main.dev-reload", "true");
    main.set_property("camel.main.duration-max-seconds", "60");
    main.enable_local_status(&home);
    let ctx = main.context().clone();
    let home2 = home.clone();
    let checker = std::thread::spawn(move || {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| checks(&home2, &routes, &ctx)));
        if r.is_err() {
            let _ = cli_out(&["stop", "--all"]);
        }
        r
    });
    main.run().unwrap();
    if let Err(p) = checker.join().unwrap() {
        std::panic::resume_unwind(p);
    }
    assert!(wait_until(|| local::list_processes(&home).is_empty()));
    assert!(cli_out(&["get", "routes"]).is_err());
}

fn checks(home: &std::path::Path, routes: &std::path::Path, ctx: &CamelContext) {
    let home = home.to_path_buf();
    assert!(wait_until(|| local::list_processes(&home).len() == 1), "status file published");
    let ps = cli_out(&["ps"]).unwrap();
    assert!(ps.contains("status-app"), "{}", ps);
    let routes_out = cli_out(&["get", "routes"]).unwrap();
    assert!(routes_out.contains("r1"), "{}", routes_out);
    assert!(cli_out(&["get", "context", "status-app"]).unwrap().contains("Started"));
    assert!(cli_out(&["get", "health"]).is_ok());

    cli_out(&["cmd", "suspend-route", "--id", "r1"]).unwrap();
    assert_eq!(ctx.route_status("r1"), Some(ServiceStatus::Suspended));
    cli_out(&["cmd", "resume-route", "status-app", "--id", "r1"]).unwrap();
    assert_eq!(ctx.route_status("r1"), Some(ServiceStatus::Started));
    cli_out(&["cmd", "stop-route", "--id", "r1"]).unwrap();
    assert_eq!(ctx.route_status("r1"), Some(ServiceStatus::Stopped));
    cli_out(&["cmd", "start-route"]).unwrap();
    assert_eq!(ctx.route_status("r1"), Some(ServiceStatus::Started));
    assert!(cli_out(&["cmd", "bogus"]).is_err());

    // trace: the first call enables the backlog tracer, the next prints the events
    assert!(cli_out(&["trace"]).unwrap().contains("enabled"));
    ctx.create_producer_template().send_body("direct:a", "x").unwrap();
    assert!(wait_until(|| cli_out(&["trace"]).map(|t| t.contains("r1")).unwrap_or(false)));

    // log file
    local::log_to_file(&local::log_file(&home, std::process::id())).unwrap();
    camel_core::camel_info!("test", "hello log file");
    assert!(cli_out(&["log", "--tail", "50"]).unwrap().contains("hello log file"));

    // dev reload: changing the route file replaces the routes
    std::thread::sleep(Duration::from_millis(1100));
    std::fs::write(routes, "- route:\n    id: r2\n    from:\n      uri: direct:a\n      steps:\n        - setBody:\n            constant: v2\n        - to: mock:a\n").unwrap();
    assert!(wait_until(|| ctx.route_status("r2") == Some(ServiceStatus::Started) && ctx.route("r1").is_none()), "reloaded");
    assert_eq!(ctx.create_producer_template().request_body("direct:a", "x").unwrap(), Value::from("v2"));

    let out = cli_out(&["stop"]).unwrap();
    assert!(out.contains("Stopping status-app"));
}
