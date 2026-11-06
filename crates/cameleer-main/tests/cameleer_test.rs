//! `cameleer test`: the example agent test passes, and a wrong expectation fails with the step.
#![cfg(feature = "full")]

use std::path::PathBuf;

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

#[test]
fn the_example_agent_test_passes() {
    let mut out = Vec::new();
    let file = repo().join("examples/agents/tests/support-agent.test.yaml");
    let (passed, failed) = cameleer_main::testing::run_files(&[file], &mut out).unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    assert_eq!((passed, failed), (1, 0), "{}", text);
    assert!(text.contains("PASS a refund waits for approval"), "{}", text);
}

#[test]
fn a_wrong_expectation_fails_with_its_step() {
    let dir = std::env::temp_dir().join(format!("cameleer-test-cmd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("routes.camel.yaml"),
        "- route:\n    from:\n      uri: direct:in\n      steps:\n        - setBody:\n            simple: \"hello ${body}\"\n        - to: mock:out\n",
    )
    .unwrap();
    let spec = dir.join("hello.test.yaml");
    std::fs::write(
        &spec,
        r#"
- test:
    name: greets
    routes: [routes.camel.yaml]
    steps:
      - send: {to: direct:in, body: Ann}
        expect: {body: hello Ann, mocks: {"mock:out": 1}}
- test:
    name: wrong greeting
    routes: [routes.camel.yaml]
    steps:
      - send: {to: direct:in, body: Bo}
        expect: {bodyContains: goodbye}
"#,
    )
    .unwrap();
    let mut out = Vec::new();
    let (passed, failed) = cameleer_main::testing::run_files(&[spec.clone()], &mut out).unwrap();
    let text = String::from_utf8_lossy(&out).to_string();
    assert_eq!((passed, failed), (1, 1), "{}", text);
    assert!(text.contains("FAIL wrong greeting\n     step 1: expected the body to contain \"goodbye\", got \"hello Bo\""), "{}", text);
    // the CLI exits 1
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_cameleer")).args(["test", spec.to_string_lossy().as_ref()]).env("CAMEL_LOCAL_STATUS", "false").output().unwrap();
    assert_eq!(status.status.code(), Some(1));
    let _ = std::fs::remove_dir_all(&dir);
}
