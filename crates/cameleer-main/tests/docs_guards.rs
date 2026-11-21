//! Guards on the written documentation that need no build features: every `cameleer-*` version
//! snippet names the workspace's minor version (cargo reads `"0.6"` as `^0.6`, so an old minor in
//! a snippet gives a new user an old release), and every `CAMEL_*` environment variable that the
//! library code reads is documented. Both found real mistakes in the 2026-11-20 documentation review.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files(dir: &Path, ext: &str, skip_dirs: &[&str], out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !skip_dirs.contains(&name.as_str()) {
                files(&p, ext, skip_dirs, out);
            }
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
}

fn workspace_minor() -> String {
    let t = std::fs::read_to_string(repo().join("Cargo.toml")).unwrap();
    let v = t.lines().find_map(|l| l.trim().strip_prefix("version = \"")).unwrap();
    let v = &v[..v.find('"').unwrap()];
    v.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// `cameleer-core = "0.7"` and `cameleer-core = { version = "0.7", ... }` in the README and in `docs/`
/// (not the design notes, the release notes or the history). `cameleer-health` has its own version.
#[test]
fn version_snippets_name_the_workspace_minor() {
    let want = workspace_minor();
    let mut md = vec![repo().join("README.md")];
    files(&repo().join("docs"), "md", &["design", "release-notes"], &mut md);
    let mut bad = Vec::new();
    for f in md {
        if f.file_name().is_some_and(|n| n == "STATUS.md") {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        for (n, line) in text.lines().enumerate() {
            let mut from = 0;
            while let Some(i) = line[from..].find("cameleer") {
                let start = from + i;
                from = start + "cameleer".len();
                let ident: String = line[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
                if ident == "cameleer-health" {
                    continue;
                }
                let rest = line[start + ident.len()..].trim_start();
                let Some(rest) = rest.strip_prefix('=') else { continue };
                let rest = rest.trim_start();
                let rest = rest.strip_prefix('{').map(|r| r.trim_start()).and_then(|r| r.strip_prefix("version")).map(|r| r.trim_start()).and_then(|r| r.strip_prefix('=')).map(|r| r.trim_start()).unwrap_or(rest);
                let Some(v) = rest.strip_prefix("\"0.") else { continue };
                let minor: String = v.chars().take_while(|c| c.is_ascii_digit()).collect();
                if format!("0.{}", minor) != want {
                    bad.push(format!("{}:{}: {} = \"0.{}\" (workspace is {})", f.display(), n + 1, ident, minor, want));
                }
            }
        }
    }
    assert!(bad.is_empty(), "version snippets that do not match the workspace minor:\n{}", bad.join("\n"));
}

/// Every `CAMEL_*` variable that `crates/*/src` reads with `std::env::var` is named somewhere in
/// `docs/` (outside the design notes). The test helpers (`testing*.rs`) are skipped.
#[test]
fn environment_variables_are_documented() {
    let mut docs = Vec::new();
    files(&repo().join("docs"), "md", &["design", "release-notes"], &mut docs);
    let all: String = docs.iter().map(|f| std::fs::read_to_string(f).unwrap()).collect();
    let mut src = Vec::new();
    for e in std::fs::read_dir(repo().join("crates")).unwrap().flatten() {
        let s = e.path().join("src");
        if s.is_dir() {
            files(&s, "rs", &[], &mut src);
        }
    }
    let mut missing = std::collections::BTreeSet::new();
    for f in src {
        if f.file_name().is_some_and(|n| n.to_string_lossy().starts_with("testing")) {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        for pat in ["var(\"CAMEL_", "var_os(\"CAMEL_"] {
            let mut from = 0;
            while let Some(i) = text[from..].find(pat) {
                let start = from + i + pat.len() - "CAMEL_".len();
                from = start + 1;
                let name: String = text[start..].chars().take_while(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_').collect();
                if !all.contains(&name) {
                    missing.insert(format!("{} (read in {})", name, f.display()));
                }
            }
        }
    }
    assert!(missing.is_empty(), "environment variables read by the code but not documented in docs/:\n{}", missing.into_iter().collect::<Vec<_>>().join("\n"));
}
