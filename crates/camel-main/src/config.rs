//! Application configuration: what Spring Boot / Quarkus auto-configuration provides for
//! Camel, done by camel-main.
//!
//! - **Files**: `application.properties`, `application.yaml`/`.yml` (nested maps are
//!   flattened to dotted keys, lists to `key[0]`; multi-document YAML is supported).
//! - **Profiles**: the active profile is `camel.main.profile`, `spring.profiles.active`,
//!   `quarkus.profile` (or the env vars `CAMEL_MAIN_PROFILE`, `SPRING_PROFILES_ACTIVE`,
//!   `QUARKUS_PROFILE`, or [`crate::Main::set_profile`]). It adds
//!   `application-<profile>.properties|yaml` from the same directory, Quarkus-style
//!   `%<profile>.key=value` entries and YAML documents with
//!   `spring.config.activate.on-profile: <profile>` (or `spring.profiles`).
//! - **Aliases**: `camel.springboot.*` and `quarkus.camel.*` = `camel.main.*`;
//!   `server.port`/`quarkus.http.port` = `camel.server.port` (when not set).
//! - **Relaxed binding**: keys match regardless of case, dashes and underscores
//!   (`camel.main.durationMaxSeconds` = `camel.main.duration-max-seconds` =
//!   `CAMEL_MAIN_DURATION_MAX_SECONDS`). Environment variables starting with `CAMEL_`
//!   override file properties: `CAMEL_MAIN_X_Y` -> `camel.main.xY`,
//!   `CAMEL_COMPONENT_<SCHEME>_<OPTION>` -> `camel.component.<scheme>.<option>` (a scheme with
//!   dashes such as `platform-http` is written `PLATFORM_HTTP`), others
//!   `CAMEL_A_B_C` -> `camel.a.bC`.
//! - **Component options**: `camel.component.<scheme>.<option>` (see
//!   `CamelContext::component_options`) are the defaults of endpoint options.
//! - **Beans**: `camel.beans.<name>=#class:<type>` (or `#type:<type>`) plus
//!   `camel.beans.<name>.<property>=value` create beans with the bean factory registered for
//!   the type (`camel_yaml::register_bean_factory`), like Camel Main.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use camel_core::prelude::*;
use camel_core::value::ObjectMap;

/// A key in relaxed form: lowercase without `-`, `_` and `.` (indexes `[n]` are kept).
pub fn relaxed(key: &str) -> String {
    key.chars().filter(|c| !matches!(c, '-' | '_' | '.')).flat_map(|c| c.to_lowercase()).collect()
}

/// Relaxed lookup in a property map.
pub fn relaxed_get<'a>(props: &'a BTreeMap<String, String>, key: &str) -> Option<&'a String> {
    if let Some(v) = props.get(key) {
        return Some(v);
    }
    let r = relaxed(key);
    props.iter().find(|(k, _)| relaxed(k) == r).map(|(_, v)| v)
}

/// Flatten a YAML/JSON value into dotted property keys.
pub fn flatten(v: &Value, prefix: &str, out: &mut BTreeMap<String, String>) {
    match v {
        Value::Object(m) => {
            for (k, x) in m {
                let key = if prefix.is_empty() { k.clone() } else { format!("{}.{}", prefix, k) };
                flatten(x, &key, out);
            }
        }
        Value::Array(a) => {
            // lists of scalars also get a comma separated value (Spring's binding of lists)
            if a.iter().all(|x| !matches!(x, Value::Object(_) | Value::Array(_))) && !prefix.is_empty() {
                out.insert(prefix.to_string(), a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(","));
            }
            for (i, x) in a.iter().enumerate() {
                flatten(x, &format!("{}[{}]", prefix, i), out);
            }
        }
        Value::Null => {
            out.insert(prefix.to_string(), String::new());
        }
        other => {
            out.insert(prefix.to_string(), other.to_string());
        }
    }
}

/// Parse `application.yaml` text: (properties of profile-less documents, per-profile documents).
pub fn parse_yaml_config(text: &str) -> Result<(BTreeMap<String, String>, Vec<(String, BTreeMap<String, String>)>)> {
    let docs = camel_yaml::parse_yaml_all(text).map_err(|e| CamelError::illegal_argument(format!("Invalid application YAML: {}", e)))?;
    let mut base = BTreeMap::new();
    let mut profiles = Vec::new();
    for d in docs {
        let mut flat = BTreeMap::new();
        flatten(&d, "", &mut flat);
        let profile = ["spring.config.activate.on-profile", "spring.profiles", "quarkus.config.profile"]
            .iter()
            .find_map(|k| flat.remove(*k));
        match profile {
            Some(p) => profiles.push((p, flat)),
            None => base.extend(flat),
        }
    }
    Ok((base, profiles))
}

fn read(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).map_err(|e| CamelError::io(format!("Cannot read {}: {}", path.display(), e)))
}

/// A configuration source file: `.properties` or `.yaml`/`.yml`.
pub fn load_file(path: &Path) -> Result<(BTreeMap<String, String>, Vec<(String, BTreeMap<String, String>)>)> {
    let text = read(path)?;
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "yaml" | "yml" => parse_yaml_config(&text),
        _ => Ok((crate::properties::parse_properties(&text), Vec::new())),
    }
}

/// The application configuration files found in `dir` (`application.properties`,
/// `application.yaml`, `application.yml`).
pub fn application_files(dir: &Path) -> Vec<PathBuf> {
    ["application.properties", "application.yaml", "application.yml"].iter().map(|f| dir.join(f)).filter(|p| p.is_file()).collect()
}

/// The active profile named by the properties or the environment.
pub fn active_profile(props: &BTreeMap<String, String>, env: &BTreeMap<String, String>) -> Option<String> {
    for k in ["CAMEL_MAIN_PROFILE", "SPRING_PROFILES_ACTIVE", "QUARKUS_PROFILE"] {
        if let Some(v) = env.get(k).filter(|v| !v.trim().is_empty()) {
            return Some(v.trim().to_string());
        }
    }
    ["camel.main.profile", "camel.springboot.profile", "spring.profiles.active", "quarkus.profile"]
        .iter()
        .find_map(|k| relaxed_get(props, k))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Apply a profile: per-profile documents, then `%profile.` entries win; other `%x.`
/// entries are dropped. Several profiles may be given comma separated.
pub fn apply_profile(
    props: &mut BTreeMap<String, String>,
    profile_docs: &[(String, BTreeMap<String, String>)],
    profile: Option<&str>,
) {
    let active: Vec<String> = profile.map(|p| p.split(',').map(|s| s.trim().to_string()).collect()).unwrap_or_default();
    for (p, doc) in profile_docs {
        if p.split(',').any(|x| active.iter().any(|a| a == x.trim())) {
            props.extend(doc.clone());
        }
    }
    let prefixed: Vec<String> = props.keys().filter(|k| k.starts_with('%')).cloned().collect();
    let mut overrides = BTreeMap::new();
    for k in prefixed {
        let v = props.remove(&k).unwrap_or_default();
        if let Some((p, rest)) = k[1..].split_once('.') {
            if active.iter().any(|a| a == p) {
                overrides.insert(rest.to_string(), v);
            }
        }
    }
    props.extend(overrides);
}

/// Map an environment variable to a property key (see the module documentation); None for
/// variables that are not Camel configuration.
pub fn env_to_property(name: &str, component_schemes: &[String]) -> Option<String> {
    let rest = name.strip_prefix("CAMEL_")?;
    let parts: Vec<String> = rest.split('_').filter(|s| !s.is_empty()).map(|s| s.to_ascii_lowercase()).collect();
    if parts.len() < 2 {
        return None;
    }
    let camel = |segs: &[String]| -> String {
        let mut out = String::new();
        for (i, s) in segs.iter().enumerate() {
            if i == 0 {
                out.push_str(s);
            } else {
                let mut c = s.chars();
                if let Some(f) = c.next() {
                    out.extend(f.to_uppercase());
                    out.push_str(c.as_str());
                }
            }
        }
        out
    };
    if parts[0] == "component" && parts.len() >= 3 {
        // longest known scheme first (platform_http -> platform-http)
        for n in (1..parts.len() - 1).rev() {
            let scheme = parts[1..=n].join("-");
            if n == 1 || component_schemes.contains(&scheme) {
                return Some(format!("camel.component.{}.{}", scheme, camel(&parts[n + 1..])));
            }
        }
    }
    Some(format!("camel.{}.{}", parts[0], camel(&parts[1..])))
}

/// Rename alias keys: `camel.springboot.*`/`quarkus.camel.*` -> `camel.main.*`,
/// `server.port`/`quarkus.http.port` -> `camel.server.port` when not set.
pub fn apply_aliases(props: &mut BTreeMap<String, String>) {
    let mut add = BTreeMap::new();
    for (k, v) in props.iter() {
        for prefix in ["camel.springboot.", "camel.spring-boot.", "quarkus.camel."] {
            if let Some(rest) = k.strip_prefix(prefix) {
                let target = format!("camel.main.{}", rest);
                if relaxed_get(props, &target).is_none() {
                    add.insert(target, v.clone());
                }
            }
        }
    }
    props.extend(add);
    if relaxed_get(props, "camel.server.port").is_none() {
        if let Some(p) = relaxed_get(props, "server.port").or_else(|| relaxed_get(props, "quarkus.http.port")).cloned() {
            props.insert("camel.server.port".into(), p);
        }
    }
}

/// The bean definitions of `camel.beans.*` properties: name -> (type, properties).
pub fn bean_definitions(props: &BTreeMap<String, String>) -> BTreeMap<String, (Option<String>, ObjectMap)> {
    let mut out: BTreeMap<String, (Option<String>, ObjectMap)> = BTreeMap::new();
    for (k, v) in props {
        let Some(rest) = k.strip_prefix("camel.beans.") else { continue };
        match rest.split_once('.') {
            None => {
                let t = v.trim();
                let t = t.strip_prefix("#class:").or_else(|| t.strip_prefix("#type:")).unwrap_or(t);
                out.entry(rest.to_string()).or_default().0 = Some(t.to_string());
            }
            Some((name, prop)) => {
                let e = out.entry(name.to_string()).or_default();
                insert_nested(&mut e.1, prop, Value::String(v.clone()));
            }
        }
    }
    out
}

fn insert_nested(m: &mut ObjectMap, path: &str, v: Value) {
    match path.split_once('.') {
        None => {
            m.insert(path.to_string(), v);
        }
        Some((head, tail)) => {
            let e = m.entry(head.to_string()).or_insert_with(|| Value::Object(ObjectMap::new()));
            if !matches!(e, Value::Object(_)) {
                *e = Value::Object(ObjectMap::new());
            }
            if let Value::Object(inner) = e {
                insert_nested(inner, tail, v);
            }
        }
    }
}

/// Create the `camel.beans.*` beans with the registered bean factories.
pub fn create_beans(ctx: &CamelContext, props: &BTreeMap<String, String>) -> Result<()> {
    for (name, (type_name, properties)) in bean_definitions(props) {
        let raw = Value::object([
            ("name", Value::from(name.as_str())),
            ("type", type_name.clone().map(Value::from).unwrap_or(Value::Null)),
            ("properties", Value::Object(properties.clone())),
        ]);
        let def = camel_yaml::BeanDefinition { name: name.clone(), type_name: type_name.clone(), properties, raw, resolved: false };
        if !camel_yaml::create_bean(ctx, &def)? {
            return Err(CamelError::illegal_argument(format!(
                "No bean factory registered for bean '{}' of type {:?} (camel.beans.{})",
                name, type_name, name
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn relaxed_binding_and_env() {
        let p = m(&[("camel.main.duration-max-seconds", "5")]);
        assert_eq!(relaxed_get(&p, "camel.main.durationMaxSeconds").map(String::as_str), Some("5"));
        assert_eq!(relaxed_get(&p, "CAMEL_MAIN_DURATION_MAX_SECONDS").map(String::as_str), Some("5"));
        let schemes = vec!["platform-http".to_string(), "seda".to_string()];
        assert_eq!(env_to_property("CAMEL_MAIN_DURATION_MAX_SECONDS", &schemes).unwrap(), "camel.main.durationMaxSeconds");
        assert_eq!(env_to_property("CAMEL_COMPONENT_SEDA_QUEUE_SIZE", &schemes).unwrap(), "camel.component.seda.queueSize");
        assert_eq!(env_to_property("CAMEL_COMPONENT_PLATFORM_HTTP_PORT", &schemes).unwrap(), "camel.component.platform-http.port");
        assert_eq!(env_to_property("HOME", &schemes), None);
    }

    #[test]
    fn yaml_profiles_and_aliases() {
        let (mut base, docs) = parse_yaml_config(
            "camel:\n  springboot:\n    name: app\n  component:\n    seda:\n      queue-size: 5\ngreeting: hi\n'%dev':\n  x: 1\nlist: [a, b]\n---\nspring:\n  config:\n    activate:\n      on-profile: dev\ngreeting: dev-hi\n",
        )
        .unwrap();
        assert_eq!(base["camel.component.seda.queue-size"], "5");
        assert_eq!(base["list"], "a,b");
        assert_eq!(base["list[1]"], "b");
        base.insert("%dev.other".into(), "o".into());
        base.insert("%prod.other".into(), "p".into());
        apply_profile(&mut base, &docs, Some("dev"));
        apply_aliases(&mut base);
        assert_eq!(base["greeting"], "dev-hi");
        assert_eq!(base["other"], "o");
        assert_eq!(base["camel.main.name"], "app");
        assert!(!base.keys().any(|k| k.starts_with('%')));
    }

    #[test]
    fn beans() {
        let p = m(&[("camel.beans.greeter", "#class:com.acme.Greeter"), ("camel.beans.greeter.prefix", "Hi"), ("camel.beans.greeter.nested.a", "1")]);
        let d = bean_definitions(&p);
        let (t, props) = &d["greeter"];
        assert_eq!(t.as_deref(), Some("com.acme.Greeter"));
        assert_eq!(props["prefix"], Value::from("Hi"));
        assert_eq!(props["nested"].get("a"), Some(&Value::from("1")));
    }
}
