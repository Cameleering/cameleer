//! Component families and other optional parts compiled into this binary (`cameleer-main`
//! cargo features).
//!
//! The default build has every family. A smaller build (`--no-default-features --features
//! cloud,sql`, for a FaaS function or an edge device) leaves the others out; a route that names
//! a component of a missing family then fails with a message naming the feature, instead of a
//! bare "No component found".

/// (feature, compiled in) for every optional family and part.
pub const FEATURES: &[(&str, bool)] = &[
    ("ai", cfg!(feature = "ai")),
    ("attest", cfg!(feature = "attest")),
    ("amqp", cfg!(feature = "amqp")),
    ("amqp10", cfg!(feature = "amqp10")),
    ("cloud", cfg!(feature = "cloud")),
    ("dataformats", cfg!(feature = "dataformats")),
    ("datastores", cfg!(feature = "datastores")),
    ("grpc", cfg!(feature = "grpc")),
    ("kafka-mqtt", cfg!(feature = "kafka-mqtt")),
    ("protocols", cfg!(feature = "protocols")),
    ("saas", cfg!(feature = "saas")),
    ("script", cfg!(feature = "script")),
    ("soap", cfg!(feature = "soap")),
    ("sql", cfg!(feature = "sql")),
    ("ssh", cfg!(feature = "ssh")),
    ("xml", cfg!(feature = "xml")),
    // parts of core, http and this crate
    ("templates", cfg!(feature = "templates")),
    ("jq", cfg!(feature = "jq")),
    ("xpath", cfg!(feature = "xpath")),
    ("http2", cfg!(feature = "http2")),
    ("validate", cfg!(feature = "validate")),
];

/// The families compiled in.
pub fn enabled() -> Vec<&'static str> {
    FEATURES.iter().filter(|(_, on)| *on).map(|(f, _)| *f).collect()
}

/// Whether every optional family is compiled in.
pub fn is_full() -> bool {
    FEATURES.iter().all(|(_, on)| *on)
}

/// Component schemes of each family (the cloud family is matched by prefix, see
/// [`feature_for`]). `tests/features.rs` checks this table against what each family registers.
pub const COMPONENTS: &[(&str, &[&str])] = &[
    ("ai", &["ai-prompt", "ai-resource", "ai-tool", "mcp"]),
    ("xml", &["xj", "xmlsecurity-sign", "xmlsecurity-verify", "xquery", "xslt", "xslt-saxon"]),
    ("ssh", &["sftp", "ssh"]),
    (
        "protocols",
        &["ftp", "ftps", "imap", "imaps", "pop3", "pop3s", "smtp", "smtps", "syslog", "vertx-websocket", "websocket", "ws", "wss"],
    ),
    ("kafka-mqtt", &["kafka", "paho", "paho-mqtt5"]),
    ("sql", &["jdbc", "sql", "sql-stored"]),
    ("amqp", &["activemq", "amqp-stomp", "jms", "rabbitmq", "spring-rabbitmq", "stomp"]),
    ("amqp10", &["amqp", "azure-eventhubs", "azure-servicebus"]),
    ("grpc", &["grpc"]),
    ("datastores", &["couchdb", "cql", "elasticsearch", "mongodb", "mongodb-gridfs", "opensearch", "spring-redis"]),
    ("script", &["mvel"]),
    ("saas", &["anthropic", "atom", "gemini", "github", "graphql", "jira", "llm", "openai", "rss", "slack", "telegram", "twilio", "webhook"]),
    ("soap", &["cxf", "cxfrs", "spring-ws"]),
    ("dataformats", &["thrift"]),
    ("templates", &["freemarker", "mustache", "string-template", "velocity"]),
];

/// Languages of each family.
pub const LANGUAGES: &[(&str, &[&str])] = &[
    ("script", &["js", "javascript", "javaScript", "groovy", "python", "ognl", "mvel", "spel", "datasonnet", "joor", "java", "csimple"]),
    ("xml", &["xquery"]),
    ("jq", &["jq"]),
    // the `xml` family registers the full XPath too, so this hint only shows when both are out
    ("xpath", &["xpath"]),
];

/// Packs and what they add: (pack, components, data formats, languages). Only the names, so a
/// build without a pack can still say which pack a route needs; a pack that is compiled in
/// describes itself ([`compiled_packs`]). `tests/features.rs` checks this table against
/// `cameleer_health::PACK`.
pub const PACKS: &[(&str, &[&str], &[&str], &[&str])] = &[(
    "health",
    &["mllp", "fhir", "dicom", "dicomweb"],
    &["hl7", "fhirJson", "fhirXml", "dicom", "dicomJson", "dicomDeidentify"],
    &["hl7terser", "dicom"],
)];

/// (pack, compiled in) for every pack.
pub const PACK_FEATURES: &[(&str, bool)] = &[("health", cfg!(feature = "health"))];

/// The packs compiled into this binary.
pub fn compiled_packs() -> Vec<&'static cameleer_core::pack::PackInfo> {
    #[allow(unused_mut)]
    let mut v: Vec<&'static cameleer_core::pack::PackInfo> = Vec::new();
    #[cfg(feature = "health")]
    v.push(&cameleer_health::PACK);
    v
}

/// The pack that provides a component scheme (`kind` = `"component"`), a data format
/// (`"dataformat"`) or a language.
pub fn pack_for(kind: &str, name: &str) -> Option<&'static str> {
    PACKS
        .iter()
        .find(|(_, c, d, l)| match kind {
            "language" => l.contains(&name),
            "dataformat" => d.contains(&name),
            _ => c.contains(&name),
        })
        .map(|(p, ..)| *p)
}

/// The feature that provides a component scheme (`kind` = `"component"`) or a language.
pub fn feature_for(kind: &str, name: &str) -> Option<&'static str> {
    let table = if kind == "language" { LANGUAGES } else { COMPONENTS };
    if let Some((f, _)) = table.iter().find(|(_, names)| names.contains(&name)) {
        return Some(f);
    }
    // the cloud family is large; its schemes share prefixes
    if kind == "component" && (name.starts_with("aws") || name.starts_with("azure-") || name.starts_with("google-")) {
        return Some("cloud");
    }
    None
}

/// The hint installed in core (see `cameleer_core::component::set_unavailable_hint`): only
/// names of families left out of this build get one.
pub fn unavailable_hint(kind: &str, name: &str) -> Option<String> {
    if let Some(p) = pack_for(kind, name) {
        let on = PACK_FEATURES.iter().any(|(n, on)| *n == p && *on);
        return (!on).then(|| {
            format!("`{name}` is in the {p} pack, which this cameleer binary does not include; use the `cameleer-{p}` distribution (image pmsudhi/cameleer:{p}) or rebuild cameleer-main with `--features {p}`")
        });
    }
    let f = feature_for(kind, name)?;
    let on = FEATURES.iter().any(|(n, on)| *n == f && *on);
    (!on).then(|| format!("this cameleer binary was built without the `{f}` feature; rebuild cameleer-main with `--features {f}`"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_prefixes_and_tables() {
        assert_eq!(feature_for("component", "aws2-s3"), Some("cloud"));
        assert_eq!(feature_for("component", "azure-servicebus"), Some("amqp10"));
        assert_eq!(feature_for("component", "kafka"), Some("kafka-mqtt"));
        assert_eq!(feature_for("language", "groovy"), Some("script"));
        assert_eq!(feature_for("component", "timer"), None);
        assert_eq!(feature_for("component", "nosuch"), None);
        assert_eq!(pack_for("component", "mllp"), Some("health"));
        assert_eq!(pack_for("language", "hl7terser"), Some("health"));
        assert_eq!(pack_for("dataformat", "fhirJson"), Some("health"));
        assert_eq!(pack_for("component", "kafka"), None);
        let hint = unavailable_hint("component", "dicom");
        assert_eq!(hint.is_some(), !cfg!(feature = "health"));
        if let Some(h) = hint {
            assert!(h.contains("health pack") && h.contains("--features health"), "{h}");
        }
    }

    #[test]
    fn hints_only_for_missing_families() {
        for (f, on) in FEATURES {
            let hint = if let Some((_, s)) = COMPONENTS.iter().find(|(n, _)| n == f) {
                unavailable_hint("component", s[0])
            } else if let Some((_, l)) = LANGUAGES.iter().find(|(n, _)| n == f && !matches!(*f, "script" | "xml" | "protocols")) {
                unavailable_hint("language", l[0])
            } else if *f == "cloud" {
                unavailable_hint("component", "aws2-s3")
            } else {
                continue; // http2 and validate are not names of components or languages
            };
            assert_eq!(hint.is_some(), !on, "{f}");
        }
        assert_eq!(feature_for("component", "velocity"), Some("templates"));
        assert_eq!(feature_for("language", "jq"), Some("jq"));
    }
}
