//! Tool definitions: name, description, JSON Schemas and hints, built from Camel's `ai-tool`
//! endpoint options, plus the canonical form and hash used for pinning.

use std::collections::BTreeMap;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

/// The four MCP tool annotations (Camel's `*Hint` options). `None` means not declared.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hints {
    pub read_only: Option<bool>,
    pub destructive: Option<bool>,
    pub idempotent: Option<bool>,
    pub open_world: Option<bool>,
}

impl Hints {
    /// True when the tool changes something: neither `readOnlyHint` nor `idempotentHint` is
    /// true. Such tools are deduplicated by default.
    pub fn has_side_effects(&self) -> bool {
        self.read_only != Some(true) && self.idempotent != Some(true)
    }
    /// The annotations object as MCP lists it (only the declared hints).
    pub fn to_value(&self) -> Value {
        let mut m = ObjectMap::new();
        for (k, v) in [
            ("readOnlyHint", self.read_only),
            ("destructiveHint", self.destructive),
            ("idempotentHint", self.idempotent),
            ("openWorldHint", self.open_world),
        ] {
            if let Some(b) = v {
                m.insert(k.to_string(), Value::Bool(b));
            }
        }
        Value::Object(m)
    }
}

/// What a client sees of a tool: the definition is what gets listed, hashed and pinned.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolDefinition {
    pub name: String,
    pub title: Option<String>,
    pub description: String,
    pub tags: Vec<String>,
    /// JSON Schema (2020-12) of the arguments; always an object schema.
    pub input_schema: Value,
    /// JSON Schema of the structured result, if declared.
    pub output_schema: Option<Value>,
    pub hints: Hints,
    /// The definition's version (`version` option, default `1`). A changed definition needs a
    /// new version when pinning is on.
    pub version: String,
    /// Camel `searchable`: whether tool search may return the tool (metadata).
    pub searchable: bool,
    /// Camel `returnDirect`: the result goes straight to the user, not back to the model.
    pub return_direct: bool,
}

impl ToolDefinition {
    /// The declared argument names (the input schema's `properties`).
    pub fn declared_arguments(&self) -> Vec<String> {
        self.input_schema
            .as_object()
            .and_then(|m| m.get("properties"))
            .and_then(|p| p.as_object())
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Whether the input schema accepts arguments it does not name (`additionalProperties`
    /// or `unevaluatedProperties` other than `false`, and declared explicitly).
    pub fn accepts_undeclared_arguments(&self) -> bool {
        let Some(m) = self.input_schema.as_object() else { return false };
        let open = |k: &str| matches!(m.get(k), Some(Value::Bool(true)) | Some(Value::Object(_)));
        open("additionalProperties") || open("unevaluatedProperties") || m.contains_key("patternProperties")
    }

    /// The definition as MCP lists it in `tools/list` (`name`, `title`, `description`,
    /// `inputSchema`, `outputSchema`, `annotations`).
    pub fn to_value(&self) -> Value {
        let mut m = ObjectMap::new();
        m.insert("name".into(), Value::from(self.name.as_str()));
        if let Some(t) = &self.title {
            m.insert("title".into(), Value::from(t.as_str()));
        }
        m.insert("description".into(), Value::from(self.description.as_str()));
        m.insert("inputSchema".into(), self.input_schema.clone());
        if let Some(o) = &self.output_schema {
            m.insert("outputSchema".into(), o.clone());
        }
        let ann = self.hints.to_value();
        if ann.as_object().map(|a| !a.is_empty()).unwrap_or(false) {
            m.insert("annotations".into(), ann);
        }
        Value::Object(m)
    }

    /// SHA-256 (hex) of the canonical JSON of [`ToolDefinition::to_value`]: what pinning
    /// compares. Tags, version, `searchable` and `returnDirect` are not part of it.
    pub fn definition_hash(&self) -> String {
        sha256_hex(canonical_json(&self.to_value()).as_bytes())
    }
}

/// Build a tool's input schema from Camel's flat options:
/// `parameter.NAME=type`, `parameter.NAME.description`, `parameter.NAME.required`,
/// `parameter.NAME.enum` (comma separated). Without a type, a parameter is a string.
pub fn schema_from_parameters(params: &BTreeMap<String, String>) -> Result<Value> {
    let mut names: Vec<String> = Vec::new();
    let mut fields: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for (key, value) in params {
        let (name, attr) = match key.split_once('.') {
            Some((n, a)) => (n.to_string(), a.to_string()),
            None => (key.clone(), "type".to_string()),
        };
        if name.is_empty() {
            return Err(CamelError::illegal_argument(format!("Invalid parameter option: parameter.{}", key)));
        }
        if !names.contains(&name) {
            names.push(name.clone());
        }
        fields.entry(name).or_default().insert(attr, value.clone());
    }
    let mut props = ObjectMap::new();
    let mut required = Vec::new();
    for name in names {
        let f = &fields[&name];
        let mut p = ObjectMap::new();
        let ty = f.get("type").map(|t| t.trim().to_ascii_lowercase()).unwrap_or_else(|| "string".into());
        let ty = match ty.as_str() {
            "" | "string" | "text" => "string",
            "integer" | "int" | "long" => "integer",
            "number" | "double" | "float" | "decimal" => "number",
            "boolean" | "bool" => "boolean",
            "object" | "map" => "object",
            "array" | "list" => "array",
            other => return Err(CamelError::illegal_argument(format!("Unknown type {} for parameter {}", other, name))),
        };
        p.insert("type".into(), Value::from(ty));
        if let Some(d) = f.get("description") {
            p.insert("description".into(), Value::from(d.as_str()));
        }
        if let Some(e) = f.get("enum") {
            let values: Vec<Value> = e.split(',').map(|s| typed(ty, s.trim())).collect();
            p.insert("enum".into(), Value::Array(values));
        }
        for (k, v) in f {
            if !matches!(k.as_str(), "type" | "description" | "enum" | "required") {
                return Err(CamelError::illegal_argument(format!("Unknown parameter attribute: parameter.{}.{}", name, k)));
            }
            let _ = v;
        }
        if f.get("required").map(|r| r.trim().eq_ignore_ascii_case("true")).unwrap_or(false) {
            required.push(Value::from(name.as_str()));
        }
        props.insert(name, Value::Object(p));
    }
    let mut schema = ObjectMap::new();
    schema.insert("type".into(), Value::from("object"));
    schema.insert("properties".into(), Value::Object(props));
    if !required.is_empty() {
        schema.insert("required".into(), Value::Array(required));
    }
    Ok(Value::Object(schema))
}

fn typed(ty: &str, s: &str) -> Value {
    match ty {
        "integer" => s.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::from(s)),
        "number" => s.parse::<f64>().map(Value::Float).unwrap_or_else(|_| Value::from(s)),
        "boolean" => match s {
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            _ => Value::from(s),
        },
        _ => Value::from(s),
    }
}

/// Parse a schema option: inline JSON, or `file:PATH`. Network references are never fetched.
pub fn parse_schema_option(option: &str, text: &str) -> Result<Value> {
    let t = text.trim();
    let json = if let Some(path) = t.strip_prefix("file:") {
        std::fs::read_to_string(path).map_err(|e| CamelError::illegal_argument(format!("Cannot read {} {}: {}", option, path, e)))?
    } else if t.starts_with("http:") || t.starts_with("https:") {
        return Err(CamelError::illegal_argument(format!("{} must be inline JSON or file:PATH; schemas are never fetched over the network", option)));
    } else {
        t.to_string()
    };
    let v = cameleer_core::json::parse(&json).map_err(|e| CamelError::illegal_argument(format!("Invalid JSON in {}: {}", option, e.message)))?;
    if !matches!(v, Value::Object(_) | Value::Bool(_)) {
        return Err(CamelError::illegal_argument(format!("{} must be a JSON Schema object", option)));
    }
    check_schema_bounds(option, &v)?;
    Ok(v)
}

/// Schemas are bounded: at most 64 levels deep and 10 000 nodes, and every `$ref` is local.
pub fn check_schema_bounds(option: &str, v: &Value) -> Result<()> {
    fn walk(v: &Value, depth: usize, nodes: &mut usize, option: &str) -> Result<()> {
        *nodes += 1;
        if depth > 64 || *nodes > 10_000 {
            return Err(CamelError::illegal_argument(format!("{} is too large (more than 64 levels or 10000 nodes)", option)));
        }
        match v {
            Value::Object(m) => {
                if let Some(Value::String(r)) = m.get("$ref") {
                    if !r.starts_with('#') {
                        return Err(CamelError::illegal_argument(format!("{}: only local $ref values (#/...) are supported, got {}", option, r)));
                    }
                }
                for (_, x) in m.iter() {
                    walk(x, depth + 1, nodes, option)?;
                }
            }
            Value::Array(a) => {
                for x in a {
                    walk(x, depth + 1, nodes, option)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk(v, 0, &mut 0, option)
}

/// JSON with object keys sorted, no whitespace: the form that hashes and digests use.
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::from(k.as_str()).to_json());
                out.push(':');
                write_canonical(m.get(k).unwrap_or(&Value::Null), out);
            }
            out.push('}');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(x, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_json()),
    }
}

/// Lower-case hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    cameleer_core::language::support::to_hex(&cameleer_core::language::support::sha256(data))
}

/// An example value for a schema, used in validation messages so the model can correct its
/// call: the declared `examples`/`default`/`const`/first `enum` value, else a placeholder by
/// type.
pub fn example_for(schema: &Value) -> Value {
    let Some(m) = schema.as_object() else { return Value::Null };
    if let Some(Value::Array(ex)) = m.get("examples") {
        if let Some(first) = ex.first() {
            return first.clone();
        }
    }
    for k in ["default", "const"] {
        if let Some(v) = m.get(k) {
            return v.clone();
        }
    }
    if let Some(Value::Array(e)) = m.get("enum") {
        if let Some(first) = e.first() {
            return first.clone();
        }
    }
    let ty = match m.get("type") {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(ts)) => ts.iter().filter_map(|t| t.as_str()).find(|t| *t != "null").unwrap_or("null"),
        _ if m.contains_key("properties") => "object",
        _ => "string",
    };
    match ty {
        "integer" => Value::Int(m.get("minimum").and_then(|x| x.as_i64()).unwrap_or(1)),
        "number" => Value::Float(m.get("minimum").and_then(|x| x.as_f64()).unwrap_or(1.5)),
        "boolean" => Value::Bool(true),
        "array" => match m.get("items") {
            Some(items) => Value::Array(vec![example_for(items)]),
            None => Value::Array(Vec::new()),
        },
        "object" => {
            let mut o = ObjectMap::new();
            let required: Vec<&str> = m.get("required").and_then(|r| r.as_array()).map(|r| r.iter().filter_map(|x| x.as_str()).collect()).unwrap_or_default();
            if let Some(Value::Object(props)) = m.get("properties") {
                for (k, p) in props.iter() {
                    if required.is_empty() || required.contains(&k.as_str()) {
                        o.insert(k.clone(), example_for(p));
                    }
                }
            }
            Value::Object(o)
        }
        "null" => Value::Null,
        _ => Value::from("text"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn flat_parameters_build_a_schema() {
        let s = schema_from_parameters(&params(&[
            ("city", "string"),
            ("city.description", "The city"),
            ("city.required", "true"),
            ("days", "integer"),
            ("unit", "string"),
            ("unit.enum", "celsius,fahrenheit"),
        ]))
        .unwrap();
        assert_eq!(
            canonical_json(&s),
            r#"{"properties":{"city":{"description":"The city","type":"string"},"days":{"type":"integer"},"unit":{"enum":["celsius","fahrenheit"],"type":"string"}},"required":["city"],"type":"object"}"#
        );
        assert!(schema_from_parameters(&params(&[("x", "uuid")])).is_err());
        assert!(schema_from_parameters(&params(&[("x.colour", "red")])).is_err());
    }

    #[test]
    fn hash_ignores_key_order_but_not_content() {
        let a = cameleer_core::json::parse(r#"{"b":1,"a":[1,{"y":2,"x":1}]}"#).unwrap();
        let b = cameleer_core::json::parse(r#"{"a":[1,{"x":1,"y":2}],"b":1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn schema_options_are_bounded_and_local() {
        assert!(parse_schema_option("argSchema", r#"{"type":"object"}"#).is_ok());
        assert!(parse_schema_option("argSchema", "https://example.com/s.json").is_err());
        assert!(parse_schema_option("argSchema", r#"{"$ref":"https://example.com/s.json"}"#).is_err());
        assert!(parse_schema_option("argSchema", "[1]").is_err());
        let deep = format!("{}{}", r#"{"not":"#.repeat(70), format!("true{}", "}".repeat(70)));
        assert!(parse_schema_option("argSchema", &deep).is_err());
    }

    #[test]
    fn examples_follow_the_schema() {
        let s = cameleer_core::json::parse(r#"{"type":"object","required":["n"],"properties":{"n":{"type":"integer","minimum":3},"u":{"enum":["a","b"]}}}"#).unwrap();
        assert_eq!(example_for(&s).to_json(), r#"{"n":3}"#);
    }
}
