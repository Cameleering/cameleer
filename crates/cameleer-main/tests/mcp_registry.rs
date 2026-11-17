//! `server.json` (the official MCP Registry entry) must match what a release publishes: the
//! registry checks the crate version and the `mcp-name:` line in the crate's README on crates.io,
//! and, for the image, the `io.modelcontextprotocol.server.name` label. A version bump that
//! forgets `server.json` fails here.

use std::path::Path;

fn repo_file(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("reading {rel}: {e}"))
}

/// The string values of `"key": "value"` pairs in order (enough for this flat, hand-written file).
fn string_values<'a>(json: &'a str, key: &str) -> Vec<&'a str> {
    let needle = format!("\"{key}\":");
    let mut out = Vec::new();
    let mut rest = json;
    while let Some(i) = rest.find(&needle) {
        rest = rest[i + needle.len()..].trim_start();
        if let Some(body) = rest.strip_prefix('"') {
            let end = body.find('"').expect("closing quote");
            out.push(&body[..end]);
            rest = &body[end..];
        }
    }
    out
}

#[test]
fn server_json_matches_the_release() {
    let json = repo_file("server.json");
    // the namespace is the organization as GitHub spells it: the registry compares it case-sensitively
    let name = "io.github.Cameleering/cameleer";
    assert_eq!(string_values(&json, "name").first().copied(), Some(name));

    // the registry checks the published `cameleer` wrapper crate: its version is the entry's version
    let manifest = repo_file("publish/cameleer/Cargo.toml");
    let version = string_values(&manifest, "version").first().copied().map(str::to_string).or_else(|| {
        manifest.lines().find_map(|l| l.strip_prefix("version = \"").and_then(|r| r.strip_suffix('"')).map(str::to_string))
    });
    let version = version.expect("the wrapper crate has a version");
    let versions = string_values(&json, "version");
    assert_eq!(versions.len(), 2, "server version and package version");
    for v in versions {
        assert_eq!(v, version, "server.json version must follow the cameleer wrapper crate");
    }

    let description = string_values(&json, "description")[0];
    assert!(description.len() <= 100, "the registry allows 100 characters, got {}", description.len());

    // Ownership checks the registry makes against the published artifacts.
    let readme = repo_file("publish/cameleer/README.md");
    assert!(readme.contains(&format!("mcp-name: {name}")), "crate README needs a visible mcp-name line");
    assert!(!readme.contains("<!-- mcp-name"), "crates.io strips HTML comments; keep the line visible");
    let dockerfile = repo_file("Dockerfile");
    assert!(dockerfile.contains(&format!("io.modelcontextprotocol.server.name=\"{name}\"")));
}
