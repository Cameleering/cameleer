//! Camel K `Integration` resources run by cameleer-main (what the Camel K operator does on
//! Kubernetes, done in-process).
//!
//! ```yaml
//! apiVersion: camel.apache.org/v1
//! kind: Integration
//! metadata:
//!   name: hello
//! spec:
//!   flows:                       # YAML DSL routes
//!     - from: {uri: "timer:tick", steps: [{to: "log:info"}]}
//!   sources:                     # inline sources (yaml or xml)
//!     - name: more.yaml
//!       content: |
//!         - from: ...
//!   configuration:               # (v1 legacy) properties
//!     - type: property
//!       value: greeting=hi
//!   traits:
//!     camel:
//!       properties: ["k=v"]      # properties
//!     environment:
//!       vars: ["NAME=value"]     # exposed as properties (and CAMEL_* relaxed binding)
//!   dependencies: ["camel:kafka"] # informational: all components are built in
//! ```
//!
//! `metadata.name` becomes `camel.main.name`. Other traits (container, service, knative,
//! ...) only make sense on Kubernetes and are ignored with a log line.

use std::collections::BTreeMap;

use cameleer_core::prelude::*;

/// A parsed Integration resource.
#[derive(Clone, Debug, Default)]
pub struct Integration {
    pub name: Option<String>,
    pub properties: BTreeMap<String, String>,
    /// (language, content) of the flows and sources.
    pub sources: Vec<(String, String)>,
    pub dependencies: Vec<String>,
    pub ignored_traits: Vec<String>,
}

fn kv(s: &str) -> Option<(String, String)> {
    s.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
}

/// Whether a YAML text is a Camel K Integration resource.
pub fn is_integration(text: &str) -> bool {
    text.lines().any(|l| l.trim() == "kind: Integration") && text.contains("camel.apache.org")
}

/// Parse an Integration resource (None when the text is not one).
pub fn parse_integration(text: &str) -> Result<Option<Integration>> {
    if !is_integration(text) {
        return Ok(None);
    }
    let v = cameleer_yaml::parse_yaml(text).map_err(|e| CamelError::illegal_argument(format!("Invalid Integration: {}", e)))?;
    if v.get("kind").map(|k| k.to_string()) != Some("Integration".into()) {
        return Ok(None);
    }
    let mut it = Integration { name: v.get("metadata").and_then(|m| m.get("name")).map(|n| n.to_string()), ..Default::default() };
    if let Some(n) = &it.name {
        it.properties.insert("camel.main.name".into(), n.clone());
    }
    let spec = v.get("spec").cloned().unwrap_or(Value::Null);
    if let Some(Value::Array(flows)) = spec.get("flows") {
        if !flows.is_empty() {
            it.sources.push(("yaml".into(), cameleer_yaml::to_yaml(&Value::Array(flows.clone()))));
        }
    }
    if let Some(Value::Array(sources)) = spec.get("sources") {
        for s in sources {
            let name = s.get("name").map(|n| n.to_string()).unwrap_or_default();
            let lang = s
                .get("language")
                .map(|l| l.to_string())
                .unwrap_or_else(|| name.rsplit('.').next().unwrap_or("yaml").to_string());
            let content = s.get("content").map(|c| c.to_string()).ok_or_else(|| {
                CamelError::illegal_argument(format!("Integration source {} has no inline content (only inline sources are supported)", name))
            })?;
            it.sources.push((lang, content));
        }
    }
    if let Some(Value::Array(conf)) = spec.get("configuration") {
        for c in conf {
            let t = c.get("type").map(|t| t.to_string()).unwrap_or_default();
            let val = c.get("value").map(|t| t.to_string()).unwrap_or_default();
            match t.as_str() {
                "property" | "env" => {
                    if let Some((k, v)) = kv(&val) {
                        it.properties.insert(k, v);
                    }
                }
                other => it.ignored_traits.push(format!("configuration type {}", other)),
            }
        }
    }
    if let Some(Value::Object(traits)) = spec.get("traits") {
        for (name, t) in traits {
            let cfg = t.get("configuration").unwrap_or(t);
            match name.as_str() {
                "camel" => {
                    if let Some(Value::Array(props)) = cfg.get("properties") {
                        for p in props {
                            if let Some((k, v)) = kv(&p.to_string()) {
                                it.properties.insert(k, v);
                            }
                        }
                    }
                }
                "environment" => {
                    if let Some(Value::Array(vars)) = cfg.get("vars") {
                        for p in vars {
                            if let Some((k, v)) = kv(&p.to_string()) {
                                let schemes: Vec<String> = Vec::new();
                                if let Some(prop) = crate::config::env_to_property(&k, &schemes) {
                                    it.properties.insert(prop, v.clone());
                                }
                                it.properties.insert(k, v);
                            }
                        }
                    }
                }
                other => it.ignored_traits.push(other.to_string()),
            }
        }
    }
    if let Some(Value::Array(deps)) = spec.get("dependencies") {
        it.dependencies = deps.iter().map(|d| d.to_string()).collect();
    }
    Ok(Some(it))
}

/// Load the flows and sources of an Integration into a context.
pub fn load_integration(ctx: &CamelContext, it: &Integration) -> Result<()> {
    for t in &it.ignored_traits {
        cameleer_core::camel_info!("Integration", "Ignoring Kubernetes-only setting: {}", t);
    }
    for d in &it.dependencies {
        cameleer_core::camel_debug!("Integration", "Dependency {} is built in", d);
    }
    for (lang, content) in &it.sources {
        match lang.as_str() {
            "yaml" | "yml" => cameleer_yaml::add_routes_from_yaml(ctx, content)?,
            "xml" => cameleer_yaml::add_routes_from_xml(ctx, content)?,
            other => return Err(CamelError::illegal_argument(format!("Unsupported Integration source language: {}", other))),
        }
    }
    Ok(())
}
