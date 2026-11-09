//! Tests for the reference guides in `docs/guides/` (configuration, CLI): the files and
//! commands shown on the pages are taken from the markdown, run, and checked against what the
//! pages say they do.

use std::collections::BTreeMap;
use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn page(name: &str) -> String {
    std::fs::read_to_string(repo().join("docs/guides").join(name)).unwrap().replace("\r\n", "\n")
}

/// The fenced block right after `marker` (an HTML comment line) in `text`.
fn block_after(text: &str, marker: &str) -> String {
    let i = text.find(marker).unwrap_or_else(|| panic!("{} not found", marker));
    let rest = &text[i..];
    let start = rest.find("```").unwrap();
    let body_start = start + rest[start..].find('\n').unwrap() + 1;
    let end = body_start + rest[body_start..].find("\n```").unwrap();
    rest[body_start..end + 1].to_string()
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("cameleer-docs-ref-{}-{}", std::process::id(), name));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// docs/guides/configuration.md, "A worked example": every row of the table.
#[test]
fn configuration_guide_worked_example() {
    let text = page("configuration.md");
    let route = block_after(&text, "<!-- example: configuration-orders -->");
    let app = block_after(&text, "<!-- file: application.properties -->");
    let prod = block_after(&text, "<!-- file: application-prod.properties -->");

    struct Row {
        profile: Option<&'static str>,
        env: &'static [(&'static str, &'static str)],
        name_flag: Option<&'static str>,
        target: &'static str,
        name: &'static str,
        port: &'static str,
    }
    let rows = [
        Row { profile: None, env: &[], name_flag: None, target: "log:orders?level=INFO", name: "orders", port: "8080" },
        Row { profile: Some("dev"), env: &[], name_flag: None, target: "log:orders?level=DEBUG", name: "orders", port: "8080" },
        Row { profile: Some("prod"), env: &[("PORT", "7070")], name_flag: None, target: "log:orders?level=WARN", name: "orders", port: "7070" },
        Row { profile: None, env: &[("CAMEL_MAIN_NAME", "orders-eu")], name_flag: None, target: "log:orders?level=INFO", name: "orders-eu", port: "8080" },
        Row { profile: None, env: &[("CAMEL_MAIN_NAME", "orders-eu")], name_flag: Some("orders-test"), target: "log:orders?level=INFO", name: "orders-test", port: "8080" },
    ];
    for (i, row) in rows.iter().enumerate() {
        let dir = temp_dir(&format!("cfg{}", i));
        std::fs::write(dir.join("application.properties"), &app).unwrap();
        std::fs::write(dir.join("application-prod.properties"), &prod).unwrap();
        let mut main = cameleer_main::new_main();
        // the process environment of the row, nothing else
        let env: BTreeMap<String, String> = row.env.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        // {{env:PORT}} is resolved through the process environment by the placeholder resolver
        for (k, v) in row.env {
            if *k == "PORT" {
                std::env::set_var("PORT", v);
            }
        }
        main.set_environment(env);
        main.load_application_config(&dir).unwrap();
        if let Some(p) = row.profile {
            main.set_profile(p);
        }
        if let Some(n) = row.name_flag {
            main.set_property("camel.main.name", n);
        }
        main.add_route_source("orders.camel.yaml", route.clone());
        main.start().unwrap();
        let props = main.properties().clone();
        let get = |k: &str| cameleer_main::config::relaxed_get(&props, k).cloned();
        let target = main.context().properties().resolve("{{orders.target}}").unwrap();
        assert_eq!(target, row.target, "row {}", i);
        assert_eq!(get("camel.main.name").as_deref(), Some(row.name), "row {}", i);
        assert_eq!(get("camel.server.port").unwrap_or_else(|| "8080".into()), row.port, "row {}", i);
        main.stop().unwrap();
        std::env::remove_var("PORT");
    }
}

fn cameleer(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_cameleer"))
        .args(args)
        .current_dir(dir)
        .env("CAMEL_LOCAL_STATUS", "false")
        .output()
        .unwrap()
}

/// docs/guides/cli.md: every command and every flag on the page is one the binary knows, and
/// every command and `run` flag the binary knows is on the page.
#[test]
fn cli_guide_lists_every_command_and_flag() {
    let text = page("cli.md");
    let usage = String::from_utf8(cameleer(&std::env::temp_dir(), &["help"]).stdout).unwrap();
    // commands: the first word after `cameleer ` on each usage line
    let commands: Vec<&str> = usage.lines().filter_map(|l| l.trim().strip_prefix("cameleer ")).map(|l| l.split([' ', '|']).next().unwrap()).collect();
    assert!(commands.len() >= 18, "{:?}", commands);
    for c in &commands {
        assert!(text.contains(&format!("`{}`", c)) || text.contains(&format!("cameleer {}", c)), "command {} missing from cli.md", c);
    }
    // flags in the usage are on the page, and flags on the page are in the usage
    let flags = |s: &str| -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        let b = s.as_bytes();
        let mut i = 0;
        while i + 2 < b.len() {
            if &b[i..i + 2] == b"--" && (i == 0 || !b[i - 1].is_ascii_alphanumeric() && b[i - 1] != b'-') && b[i + 2].is_ascii_lowercase() {
                let mut j = i + 2;
                while j < b.len() && (b[j].is_ascii_lowercase() || b[j] == b'-') {
                    j += 1;
                }
                out.insert(s[i..j].trim_end_matches('-').to_string());
                i = j;
            } else {
                i += 1;
            }
        }
        out
    };
    let in_usage = flags(&usage);
    let on_page = flags(&text);
    for f in &in_usage {
        assert!(on_page.contains(f), "flag {} missing from cli.md", f);
    }
    // the page may also name --version and --help, docker flags and cargo test --workspace
    for f in &on_page {
        if ["--version", "--help", "--rm", "--read-only", "--workspace"].contains(&f.as_str()) {
            continue;
        }
        assert!(in_usage.contains(f), "cli.md names {} but the binary has no such flag", f);
    }
}

/// docs/guides/cli.md, "A worked example".
#[test]
fn cli_guide_worked_example() {
    let dir = temp_dir("cli");
    let init = cameleer(&dir, &["init", "hello.camel.yaml"]);
    assert!(init.status.success(), "{:?}", init);
    assert!(String::from_utf8_lossy(&init.stdout).contains("hello.camel.yaml"));
    let v = cameleer(&dir, &["validate", "hello.camel.yaml"]);
    assert_eq!(v.status.code(), Some(0), "{}", String::from_utf8_lossy(&v.stdout));
    let run = cameleer(&dir, &["run", "hello.camel.yaml", "--max-messages", "2"]);
    assert!(run.status.success(), "{:?}", run);
    let all = format!("{}{}", String::from_utf8_lossy(&run.stdout), String::from_utf8_lossy(&run.stderr));
    assert_eq!(all.matches("Hello Camel from route1").count(), 2, "{}", all);
}
