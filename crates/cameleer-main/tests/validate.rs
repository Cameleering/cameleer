//! `cameleer validate` and `cameleer schema`: one test per finding code (fixtures in
//! `tests/validate_fixtures/`), the JSON and text output, exit codes, and every route file in
//! `examples/` and every `<!-- example: id -->` YAML block in `docs/` (all must validate
//! without errors).

// needs every component family (the default features)
#![cfg(feature = "full")]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cameleer_main::cli;
use cameleer_main::validate::{self, codes, FileReport, Severity};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("validate_fixtures")
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap().to_path_buf()
}

fn fixture(name: &str) -> String {
    fixtures().join(name).display().to_string()
}

/// Validate fixture files together; returns the report of the first.
fn check(names: &[&str]) -> FileReport {
    check_with(names, BTreeMap::new())
}

fn check_with(names: &[&str], props: BTreeMap<String, String>) -> FileReport {
    let paths: Vec<String> = names.iter().map(|n| fixture(n)).collect();
    let report = validate::validate_paths(&paths, props).unwrap();
    report.files.into_iter().next().unwrap()
}

fn messages(r: &FileReport, code: &str) -> Vec<String> {
    r.with_code(code).iter().map(|f| format!("{}:{} {} ({})", f.line.unwrap_or(0), f.column.unwrap_or(0), f.message, f.hint.clone().unwrap_or_default())).collect()
}

fn run_cli(args: &[&str]) -> (i32, String, String) {
    let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let mut out = Vec::new();
    let mut err = Vec::new();
    let code = cli::run_status(&args, &mut out, &mut err);
    (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
}

// ---------------------------------------------------------------------------
// Good files
// ---------------------------------------------------------------------------

#[test]
fn good_fixtures_are_valid() {
    let report = validate::validate_paths(&[fixture("good")], BTreeMap::new()).unwrap();
    assert_eq!(report.files.len(), 2, "{:?}", report.files.iter().map(|f| &f.path).collect::<Vec<_>>());
    for f in &report.files {
        assert!(f.findings.is_empty(), "{}: {:?}", f.path, f.findings);
    }
    assert!(report.valid(true));
    let yaml = report.files.iter().find(|f| f.path.ends_with("orders.camel.yaml")).unwrap();
    assert_eq!(yaml.routes, 3);
}

// ---------------------------------------------------------------------------
// One test per code
// ---------------------------------------------------------------------------

#[test]
fn yaml_syntax_reports_line_and_column() {
    let r = check(&["bad/yaml-syntax.yaml"]);
    let f = r.with_code(codes::YAML_SYNTAX);
    assert_eq!(f.len(), 1, "{:?}", r.findings);
    assert_eq!(f[0].severity, Severity::Error);
    assert_eq!(f[0].line, Some(5));
    assert!(f[0].column.is_some());
}

#[test]
fn xml_syntax_reports_line_and_column() {
    let r = check(&["bad/xml-syntax.xml"]);
    let f = r.with_code(codes::XML_SYNTAX);
    assert_eq!(f.len(), 1, "{:?}", r.findings);
    assert_eq!(f[0].line, Some(5));
}

#[test]
fn dsl_structure_errors() {
    let r = check(&["bad/dsl-structure.yaml"]);
    let m = messages(&r, codes::DSL_STRUCTURE);
    let has = |needle: &str| m.iter().any(|x| x.contains(needle));
    assert!(has("7:11 unknown step 'setHeadr' (did you mean `setHeader`?)"), "{:#?}", m);
    assert!(has("16:13 unknown key 'nam' in step 'setHeader' (did you mean `name`?)"), "{:#?}", m);
    assert!(!has("missing the required key 'name'"), "a misspelled key is reported once: {:#?}", m);
    assert!(has("20:5 'from' is missing the required key 'uri'"), "{:#?}", m);
    assert!(has("unknown step 'when' (`when` is only valid inside `choice`)"), "{:#?}", m);
    assert!(has("unknown key 'to' in element 'from' (`to` is a step: put it in the `steps:` list)"), "{:#?}", m);
    assert!(has("'filter' requires an expression"), "{:#?}", m);
    assert!(has("unknown top-level element 'rout'") && has("did you mean `route`?"), "{:#?}", m);
    assert_eq!(m.len(), 7, "{:#?}", m);
    assert!(r.findings.iter().all(|f| f.code == codes::DSL_STRUCTURE));
}

#[test]
fn dsl_structure_includes_loader_errors() {
    // passes the model checks but the loader rejects it (invalid loggingLevel)
    let r = validate::validate_source(
        "loader.yaml",
        "- route:\n    from:\n      uri: timer:x\n      steps:\n        - log:\n            message: hi\n            loggingLevel: LOUD\n",
        BTreeMap::new(),
    );
    let m = messages(&r, codes::DSL_STRUCTURE);
    assert_eq!(m.len(), 1, "{:?}", r.findings);
    assert!(m[0].contains("invalid loggingLevel 'LOUD'"), "{:?}", m);
    assert!(m[0].starts_with("5:"), "position from the loader message: {:?}", m);
}

#[test]
fn endpoint_errors() {
    let r = check(&["bad/endpoint.yaml"]);
    let m = messages(&r, codes::ENDPOINT);
    let has = |needle: &str| m.iter().any(|x| x.contains(needle));
    assert!(has("Unknown parameters=[perod]"), "{:#?}", m);
    assert!(has("No component found with scheme: tmer") && has("did you mean `timer`?"), "{:#?}", m);
    assert!(has("Unknown parameters=[levl]"), "{:#?}", m);
    assert!(has("No component found with scheme: nosuch"), "dynamic URIs: the scheme is checked: {:#?}", m);
    // toD mock:${...} and {{env:TARGET:mock:fallback}} are fine
    assert_eq!(m.len(), 4, "{:#?}", m);
    let timer = r.with_code(codes::ENDPOINT).into_iter().find(|f| f.message.contains("perod")).unwrap();
    assert_eq!((timer.line, timer.column), (Some(4), Some(7)));
}

#[test]
fn expression_errors() {
    let r = check(&["bad/expression.yaml"]);
    let m = messages(&r, codes::EXPRESSION);
    let has = |needle: &str| m.iter().any(|x| x.contains(needle));
    assert!(has("7:13 invalid simple expression"), "{:#?}", m);
    assert!(has("invalid jsonpath expression"), "{:#?}", m);
    assert!(has("invalid xpath expression"), "{:#?}", m);
    assert!(has("invalid jq expression"), "{:#?}", m);
    assert_eq!(m.len(), 4, "{:#?}", m);
    assert!(m.iter().all(|x| !x.contains('\n')));
}

#[test]
fn unresolved_placeholders_are_warnings() {
    let r = check(&["bad/placeholder.yaml"]);
    let m = messages(&r, codes::UNRESOLVED_PLACEHOLDER);
    let has = |needle: &str| m.iter().any(|x| x.contains(needle));
    assert!(has("{{timer.name}}"), "{:#?}", m);
    assert!(has("{{env:PERIOD}}") && has("--prop PERIOD=VALUE"), "{:#?}", m);
    assert!(has("{{target}}"), "{:#?}", m);
    assert!(!has("{{name:default}}"), "{:#?}", m);
    assert_eq!(m.len(), 3, "{:#?}", m);
    assert_eq!(r.errors(), 0, "{:?}", r.findings);
    assert!(r.valid(false) && !r.valid(true));
}

#[test]
fn properties_resolve_placeholders_and_endpoints_are_checked() {
    let mut props = BTreeMap::new();
    props.insert("timer.name".to_string(), "t".to_string());
    props.insert("PERIOD".to_string(), "100".to_string());
    props.insert("target".to_string(), "log:x?levl=1".to_string());
    let r = check_with(&["bad/placeholder.yaml"], props);
    assert!(r.with_code(codes::UNRESOLVED_PLACEHOLDER).is_empty(), "{:?}", r.findings);
    let m = messages(&r, codes::ENDPOINT);
    assert!(m.len() == 1 && m[0].contains("Unknown parameters=[levl]"), "{:#?}", m);
}

#[test]
fn dangling_endpoints_are_warnings() {
    let r = check(&["bad/dangling.yaml"]);
    let m = messages(&r, codes::DANGLING_ENDPOINT);
    assert_eq!(m.len(), 2, "{:#?}", m);
    assert!(m.iter().any(|x| x.contains("'direct:nowhere'")), "{:#?}", m);
    assert!(m.iter().any(|x| x.contains("'seda:queue'")), "{:#?}", m);
    assert!(r.with_code(codes::DANGLING_ENDPOINT).iter().all(|f| f.severity == Severity::Warning));
}

#[test]
fn dangling_check_spans_the_validated_files() {
    let dir = std::env::temp_dir().join(format!("cameleer-validate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.yaml"), "- route:\n    from:\n      uri: timer:a\n      steps:\n        - to: direct:b\n").unwrap();
    std::fs::write(dir.join("b.yaml"), "- route:\n    from:\n      uri: direct:b\n      steps:\n        - to: mock:b\n").unwrap();
    let report = validate::validate_paths(&[dir.display().to_string()], BTreeMap::new()).unwrap();
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.warnings(), 0, "{:?}", report.files);
    let only_a = validate::validate_paths(&[dir.join("a.yaml").display().to_string()], BTreeMap::new()).unwrap();
    assert_eq!(only_a.files[0].with_code(codes::DANGLING_ENDPOINT).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn duplicate_route_ids() {
    let r = check(&["bad/duplicate-ids.yaml"]);
    let f = r.with_code(codes::DUPLICATE_ROUTE_ID);
    assert_eq!(f.len(), 1, "{:?}", r.findings);
    assert_eq!(f[0].severity, Severity::Error);
    assert_eq!(f[0].line, Some(8));
    assert!(f[0].message.contains("'same'") && f[0].message.contains("duplicate-ids.yaml:2"), "{}", f[0].message);
}

#[test]
fn route_templates_and_xml_are_checked() {
    let yaml = "- routeTemplate:\n    id: t\n    parameters:\n      - name: target\n      - name: period\n        defaultValue: 1000\n    from:\n      uri: \"timer:{{name:t}}?period={{period}}\"\n      steps:\n        - to: \"{{target}}\"\n        - to: \"log:x?levl={{period}}\"\n- templatedRoute:\n    routeTemplateRef: t\n    routeId: from-template\n    parameters:\n      - name: target\n        value: mock:x\n";
    let r = validate::validate_source("templates.yaml", yaml, BTreeMap::new());
    // template parameters are not property placeholders
    assert!(r.with_code(codes::UNRESOLVED_PLACEHOLDER).is_empty(), "{:?}", r.findings);
    let m = messages(&r, codes::ENDPOINT);
    assert!(m.len() == 1 && m[0].contains("levl"), "{:#?}", m);
    assert_eq!(r.routes, 1);

    let xml = "<routes><route id=\"x\"><from uri=\"timer:x?perod=1\"/><setBody><simple>${body</simple></setBody><to uri=\"direct:gone\"/></route></routes>";
    let r = validate::validate_source("routes.xml", xml, BTreeMap::new());
    assert_eq!(r.with_code(codes::ENDPOINT).len(), 1, "{:?}", r.findings);
    assert_eq!(r.with_code(codes::EXPRESSION).len(), 1, "{:?}", r.findings);
    assert_eq!(r.with_code(codes::DANGLING_ENDPOINT).len(), 1, "{:?}", r.findings);
}

// ---------------------------------------------------------------------------
// Output and exit codes
// ---------------------------------------------------------------------------

#[test]
fn json_output_shape() {
    let (code, out, _) = run_cli(&["validate", &fixture("bad/duplicate-ids.yaml"), &fixture("bad/placeholder.yaml"), "--format", "json"]);
    assert_eq!(code, 1);
    let v = cameleer_core::json::parse(out.trim()).unwrap_or_else(|e| panic!("{}: {}", e, out));
    assert_eq!(v.get("valid").and_then(|x| x.as_bool()), Some(false));
    let files = v.get("files").and_then(|f| f.as_array()).unwrap();
    assert_eq!(files.len(), 2);
    let f0 = &files[0];
    assert!(f0.get("path").and_then(|p| p.as_str()).unwrap().ends_with("duplicate-ids.yaml"));
    assert_eq!(f0.get("valid").and_then(|x| x.as_bool()), Some(false));
    assert_eq!(f0.get("routes").and_then(|x| x.as_i64()), Some(2));
    let finding = &f0.get("findings").and_then(|x| x.as_array()).unwrap()[0];
    for k in ["severity", "code", "message", "line", "column", "hint"] {
        assert!(finding.get(k).is_some(), "finding key {}: {}", k, out);
    }
    assert_eq!(finding.get("severity").and_then(|x| x.as_str()), Some("error"));
    assert_eq!(finding.get("code").and_then(|x| x.as_str()), Some("duplicate-route-id"));
    assert_eq!(finding.get("line").and_then(|x| x.as_i64()), Some(8));
    // the placeholder file only has warnings: valid without --strict
    assert_eq!(files[1].get("valid").and_then(|x| x.as_bool()), Some(true));
    let summary = v.get("summary").unwrap();
    assert_eq!(summary.get("files").and_then(|x| x.as_i64()), Some(2));
    assert_eq!(summary.get("errors").and_then(|x| x.as_i64()), Some(1));
    assert_eq!(summary.get("warnings").and_then(|x| x.as_i64()), Some(3));
    // null line/column/hint for findings without them
    let r = validate::Report {
        files: vec![FileReport {
            path: "a \"b\".yaml".into(),
            routes: 0,
            findings: vec![validate::Finding { severity: Severity::Warning, code: "x", message: "m\n".into(), line: None, column: None, hint: None }],
        }],
    };
    let v = cameleer_core::json::parse(&r.to_json(false)).unwrap();
    let f = &v.get("files").unwrap().as_array().unwrap()[0];
    assert_eq!(f.get("path").and_then(|p| p.as_str()), Some("a \"b\".yaml"));
    let x = &f.get("findings").unwrap().as_array().unwrap()[0];
    assert!(x.get("line").unwrap().is_null() && x.get("column").unwrap().is_null() && x.get("hint").unwrap().is_null());
}

#[test]
fn text_output_format() {
    let (code, out, _) = run_cli(&["validate", &fixture("bad/duplicate-ids.yaml")]);
    assert_eq!(code, 1);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{}", out);
    assert!(lines[0].ends_with("duplicate-ids.yaml:8:5: error[duplicate-route-id]: duplicate route id 'same' (first defined at ".to_string().as_str()) || lines[0].contains("duplicate-ids.yaml:8:5: error[duplicate-route-id]: duplicate route id 'same'"), "{}", out);
    assert!(lines[0].ends_with("(route ids must be unique in a CamelContext)"), "{}", out);
    assert_eq!(lines[1], "invalid: 1 file, 2 routes, 1 error, 0 warnings");
}

#[test]
fn exit_codes() {
    // valid
    let (code, out, _) = run_cli(&["validate", &fixture("good")]);
    assert_eq!(code, 0, "{}", out);
    assert!(out.starts_with("valid: 2 files"), "{}", out);
    // errors
    assert_eq!(run_cli(&["validate", &fixture("bad/endpoint.yaml")]).0, 1);
    // warnings only: 0, or 1 with --strict
    assert_eq!(run_cli(&["validate", &fixture("bad/dangling.yaml")]).0, 0);
    assert_eq!(run_cli(&["validate", &fixture("bad/dangling.yaml"), "--strict"]).0, 1);
    // --prop resolves placeholders
    let (code, _, _) = run_cli(&["validate", &fixture("bad/placeholder.yaml"), "--strict", "--prop", "timer.name=t", "--prop", "PERIOD=5", "--prop", "target=mock:x"]);
    assert_eq!(code, 0);
    // usage errors
    let (code, _, err) = run_cli(&["validate"]);
    assert_eq!(code, 2);
    assert!(err.contains("requires at least one file"), "{}", err);
    assert_eq!(run_cli(&["validate", &fixture("good"), "--bogus", "1"]).0, 2);
    assert_eq!(run_cli(&["validate", &fixture("good"), "--format", "xml"]).0, 2);
    assert_eq!(run_cli(&["validate", &fixture("nope.yaml")]).0, 2);
    assert_eq!(run_cli(&["validate", &fixture("good"), "--prop", "novalue"]).0, 2);
    // cli::run: Ok for valid, Err for invalid
    let mut out = Vec::new();
    assert!(cli::run(&["validate".to_string(), fixture("good")], &mut out).is_ok());
    assert!(cli::run(&["validate".to_string(), fixture("bad/endpoint.yaml")], &mut out).is_err());
    // other commands keep 0 / 1
    assert_eq!(run_cli(&["version"]).0, 0);
    assert_eq!(run_cli(&["bogus-command"]).0, 1);
}

#[test]
fn properties_file_option() {
    let dir = std::env::temp_dir().join(format!("cameleer-validate-props-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let props = dir.join("app.properties");
    std::fs::write(&props, "timer.name=t\nPERIOD=10\ntarget=mock:x\n").unwrap();
    let (code, out, _) = run_cli(&["validate", &fixture("bad/placeholder.yaml"), "--strict", "--properties", &props.display().to_string()]);
    assert_eq!(code, 0, "{}", out);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn usage_lists_validate_and_schema() {
    let u = cli::usage();
    assert!(u.contains("cameleer validate <file-or-dir>... [--format text|json] [--strict] [--properties FILE] [--prop key=value]"), "{}", u);
    assert!(u.contains("cameleer schema"), "{}", u);
}

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

#[test]
fn schema_is_json_and_covers_the_model() {
    let (code, out, _) = run_cli(&["schema"]);
    assert_eq!(code, 0);
    let v = cameleer_core::json::parse(out.trim()).unwrap_or_else(|e| panic!("{}", e));
    assert_eq!(v.get("$schema").and_then(|s| s.as_str()), Some("https://json-schema.org/draft/2020-12/schema"));
    assert_eq!(v.get("type").and_then(|s| s.as_str()), Some("array"));
    let defs = v.get("$defs").and_then(|d| d.as_object()).unwrap();
    for m in cameleer_yaml::model::STEPS {
        assert!(defs.contains_key(&format!("step.{}", m.name)), "step {}", m.name);
    }
    for m in cameleer_yaml::model::MODELS {
        assert!(defs.contains_key(m.name), "model {}", m.name);
    }
    // every $ref points to a definition
    fn refs(v: &cameleer_core::value::Value, out: &mut Vec<String>) {
        match v {
            cameleer_core::value::Value::Object(m) => {
                for (k, x) in m {
                    if k == "$ref" {
                        out.push(x.as_str().unwrap().to_string());
                    }
                    refs(x, out);
                }
            }
            cameleer_core::value::Value::Array(a) => a.iter().for_each(|x| refs(x, out)),
            _ => {}
        }
    }
    let mut all = Vec::new();
    refs(&v, &mut all);
    assert!(all.len() > 100);
    for r in all {
        let name = r.strip_prefix("#/$defs/").unwrap_or_else(|| panic!("{}", r));
        assert!(defs.contains_key(name), "dangling $ref {}", r);
    }
    // setHeader: required name (or headerName), an expression, no other keys
    let sh = defs.get("step.setHeader").unwrap();
    assert_eq!(sh.get("additionalProperties").and_then(|x| x.as_bool()), Some(false));
    assert!(sh.get("properties").and_then(|p| p.get("simple")).is_some());
}

// ---------------------------------------------------------------------------
// examples/ and docs/
// ---------------------------------------------------------------------------

#[test]
fn every_example_route_file_is_valid() {
    // the health examples and pages need the health pack (a `--features health` binary has it)
    cameleer_core::pack::install(&cameleer_health::HEALTH);
    let dir = repo_root().join("examples");
    let report = validate::validate_paths(&[dir.display().to_string()], BTreeMap::new()).unwrap();
    assert!(report.files.len() >= 19, "{}", report.files.len());
    let errors: Vec<String> = report
        .files
        .iter()
        .flat_map(|f| f.findings.iter().filter(|x| x.severity == Severity::Error).map(move |x| format!("{}:{}: [{}] {}", f.path, x.line.unwrap_or(0), x.code, x.message)))
        .collect();
    assert!(errors.is_empty(), "{:#?}", errors);
}

/// `(file:line, id, content)` of every `<!-- example: id -->` yaml block in docs/.
fn doc_examples() -> Vec<(String, String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().map(|x| x == "md").unwrap_or(false) {
                out.push(p);
            }
        }
    }
    let root = repo_root();
    let mut files = Vec::new();
    walk(&root.join("docs"), &mut files);
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let mut i = 0;
        while i < lines.len() {
            let t = lines[i].trim_start();
            if t.starts_with("```") || t.starts_with("~~~") {
                let fence: String = t.chars().take_while(|c| *c == '`' || *c == '~').collect();
                let lang = t[fence.len()..].trim().split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                let start = i;
                let mut body = Vec::new();
                i += 1;
                while i < lines.len() && !(lines[i].trim_start().starts_with(&fence) && lines[i].trim_start()[fence.len()..].trim().is_empty()) {
                    body.push(lines[i]);
                    i += 1;
                }
                let marker = if start > 0 { lines[start - 1].trim() } else { "" };
                let id = marker.strip_prefix("<!--").and_then(|r| r.strip_suffix("-->")).map(|r| r.trim()).and_then(|r| r.strip_prefix("example:")).map(|r| r.trim().to_string());
                if let (Some(id), true) = (id, lang == "yaml" || lang == "yml") {
                    let at = format!("{}:{}", f.strip_prefix(&root).unwrap().display().to_string().replace('\\', "/"), start + 1);
                    out.push((at, id, body.join("\n") + "\n"));
                }
            }
            i += 1;
        }
    }
    out
}

/// Doc examples that reference a companion file the page does not ship (a `.proto`, `.thrift`
/// or `.wsdl` next to the route): the endpoint cannot be created from the snippet alone. They
/// must fail with exactly that error; remove an entry when its page is fixed.
const KNOWN_MISSING_FILES: &[&str] = &["cxf-json-to-soap", "grpc-inventory-service", "grpc-call-inventory", "thrift-device-registry"];

#[test]
fn every_doc_example_validates_without_errors() {
    // the health examples and pages need the health pack (a `--features health` binary has it)
    cameleer_core::pack::install(&cameleer_health::HEALTH);
    let examples = doc_examples();
    assert!(examples.len() > 300, "found only {} doc examples", examples.len());
    let mut errors = Vec::new();
    let mut known_seen = Vec::new();
    for (at, id, content) in &examples {
        let r = validate::validate_source(&format!("{}.yaml", id), content, BTreeMap::new());
        for f in r.findings.iter().filter(|f| f.severity == Severity::Error) {
            let missing_file = f.code == codes::ENDPOINT && (f.message.contains("cannot find the file") || f.message.contains("No such file"));
            if KNOWN_MISSING_FILES.contains(&id.as_str()) && missing_file {
                known_seen.push(id.clone());
                continue;
            }
            errors.push(format!("{} [{}] +{}: {}[{}]: {}{}", at, id, f.line.unwrap_or(0), f.severity.as_str(), f.code, f.message, f.hint.as_ref().map(|h| format!(" ({})", h)).unwrap_or_default()));
        }
    }
    assert!(errors.is_empty(), "{} errors in doc examples:\n{}", errors.len(), errors.join("\n"));
    for id in KNOWN_MISSING_FILES {
        assert!(known_seen.iter().any(|s| s == id), "doc example '{}' validates now: remove it from KNOWN_MISSING_FILES", id);
    }
}

// ---------------------------------------------------------------------------
// The schema agrees with the validator on examples/ and docs/
// ---------------------------------------------------------------------------

use cameleer_core::value::Value;

/// A small JSON Schema evaluator for the keywords `cameleer schema` emits.
fn schema_ok(root: &Value, s: &Value, v: &Value) -> bool {
    let Value::Object(m) = s else { return true };
    if let Some(Value::String(r)) = m.get("$ref") {
        let name = r.trim_start_matches("#/$defs/");
        return schema_ok(root, root.get("$defs").unwrap().get(name).unwrap(), v);
    }
    if let Some(t) = m.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(|x| x.as_str()).collect(),
            _ => vec![],
        };
        let ok = types.iter().any(|t| match (*t, v) {
            ("string", Value::String(_)) | ("boolean", Value::Bool(_)) | ("integer", Value::Int(_)) | ("null", Value::Null) => true,
            ("number", Value::Int(_) | Value::Float(_)) | ("object", Value::Object(_)) | ("array", Value::Array(_)) => true,
            _ => false,
        });
        if !ok {
            return false;
        }
    }
    if let Some(Value::Array(e)) = m.get("enum") {
        if !e.contains(v) {
            return false;
        }
    }
    if let Some(Value::Array(any)) = m.get("anyOf") {
        if !any.iter().any(|x| schema_ok(root, x, v)) {
            return false;
        }
    }
    if let Some(Value::Array(all)) = m.get("allOf") {
        if !all.iter().all(|x| schema_ok(root, x, v)) {
            return false;
        }
    }
    if let Value::Array(items) = v {
        if let Some(is) = m.get("items") {
            if !items.iter().all(|x| schema_ok(root, is, x)) {
                return false;
            }
        }
    }
    if let Value::Object(obj) = v {
        if let Some(Value::Array(req)) = m.get("required") {
            if !req.iter().all(|r| obj.contains_key(r.as_str().unwrap())) {
                return false;
            }
        }
        if let Some(n) = m.get("minProperties").and_then(|x| x.as_i64()) {
            if (obj.len() as i64) < n {
                return false;
            }
        }
        if let Some(n) = m.get("maxProperties").and_then(|x| x.as_i64()) {
            if (obj.len() as i64) > n {
                return false;
            }
        }
        let props = m.get("properties").and_then(|p| p.as_object());
        for (k, x) in obj {
            match props.and_then(|p| p.get(k)) {
                Some(ps) => {
                    if !schema_ok(root, ps, x) {
                        return false;
                    }
                }
                None => match m.get("additionalProperties") {
                    Some(Value::Bool(false)) => return false,
                    Some(ap @ Value::Object(_)) => {
                        if !schema_ok(root, ap, x) {
                            return false;
                        }
                    }
                    _ => {}
                },
            }
        }
    }
    true
}

#[test]
fn schema_accepts_every_example_and_doc_route_and_rejects_bad_ones() {
    // the health examples and pages need the health pack (a `--features health` binary has it)
    cameleer_core::pack::install(&cameleer_health::HEALTH);
    let root = cameleer_core::json::parse(&cameleer_main::schema::json_schema()).unwrap();
    let mut docs: Vec<(String, String)> = doc_examples().into_iter().map(|(at, id, c)| (format!("{} [{}]", at, id), c)).collect();
    for f in validate::route_files(&repo_root().join("examples")) {
        docs.push((f.display().to_string(), std::fs::read_to_string(&f).unwrap()));
    }
    docs.push(("good/orders".into(), std::fs::read_to_string(fixtures().join("good/orders.camel.yaml")).unwrap()));
    let mut failures = Vec::new();
    let mut checked = 0;
    for (at, text) in &docs {
        for doc in cameleer_yaml::parse_yaml_all(text).unwrap() {
            // Kamelet / Pipe / Integration resources are not covered by the schema
            if matches!(&doc, Value::Object(m) if m.contains_key("apiVersion")) || doc.is_null() {
                continue;
            }
            checked += 1;
            if !schema_ok(&root, &root, &doc) {
                failures.push(at.clone());
            }
        }
    }
    assert!(checked > 300, "{}", checked);
    assert!(failures.is_empty(), "{} documents fail the schema:\n{}", failures.len(), failures.join("\n"));
    // the structure errors of the fixture are schema errors too
    let bad = cameleer_yaml::parse_yaml_all(&std::fs::read_to_string(fixtures().join("bad/dsl-structure.yaml")).unwrap()).unwrap();
    let Value::Array(items) = &bad[0] else { panic!("not a list") };
    for (i, item) in items.iter().enumerate() {
        assert!(!schema_ok(&root, &root, &Value::Array(vec![item.clone()])), "bad route #{} passes the schema", i);
    }
}

#[test]
fn route_configuration_ids_are_checked_across_files() {
    let dir = std::env::temp_dir().join(format!("cameleer-validate-rc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let route = "- route:\n    routeConfigurationId: audit\n    from:\n      uri: direct:a\n      steps:\n        - to: mock:a\n";
    let config = "- routeConfiguration:\n    id: audit\n    intercept:\n      - intercept:\n          steps:\n            - to: mock:audited\n";
    let a = dir.join("a-route.camel.yaml");
    let b = dir.join("b-config.camel.yaml");
    std::fs::write(&a, route).unwrap();
    std::fs::write(&b, config).unwrap();
    // the configuration is in a later file: fine, as at start-up
    let report = validate::validate_paths(&[a.display().to_string(), b.display().to_string()], BTreeMap::new()).unwrap();
    let errors: Vec<String> = report.files.iter().flat_map(|f| f.findings.iter().filter(|x| x.severity == Severity::Error).map(|x| x.message.clone())).collect();
    assert!(errors.is_empty(), "{:?}", errors);
    // without it, the unknown id is an error on the file that names it
    let report = validate::validate_paths(&[a.display().to_string()], BTreeMap::new()).unwrap();
    let msgs: Vec<String> = report.files[0].findings.iter().map(|x| x.message.clone()).collect();
    assert!(msgs.iter().any(|m| m.contains("Cannot find routeConfiguration with id 'audit'")), "{:?}", msgs);
}
