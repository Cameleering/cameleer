//! `yaml` (`snakeYaml`) data format: `Value` <-> YAML text using camel-yaml's parser.
//!
//! Options (SnakeYAML / Camel names):
//! * `prettyFlow` (default false): flow collections are written over several indented lines.
//! * `defaultFlowStyle` (`flowStyle`): `AUTO` (default, like SnakeYAML: collections that
//!   only contain scalars are written in flow style `{a: 1}`, others in block style),
//!   `BLOCK` or `FLOW`.
//! * `allowAnyType` (default false): safe constructor semantics. Without it, documents with
//!   application tags (`!!com.acme.Pojo`, `!foo`) are rejected (no arbitrary types are ever
//!   constructed: with `allowAnyType=true` the tag is ignored and the node read as plain data).
//! * `typeFilter`: comma separated tag names (or `prefix.*`) allowed without `allowAnyType`.
//! * `maxAliasesForCollections` (default 50): maximum number of aliases in a document.
//! * `unmarshalType` accepted and ignored (values map to `Value`).

use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::Result;
use camel_core::message::Exchange;
use camel_core::Value;
use camel_yaml::yaml::{format_scalar, parse_nodes, Node, NodeKind};

use crate::{body_text, df_err, Opts};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlowStyle {
    Auto,
    Block,
    Flow,
}

/// The YAML data format.
pub struct YamlDataFormat {
    pub pretty_flow: bool,
    pub flow_style: FlowStyle,
    pub allow_any_type: bool,
    pub type_filter: Vec<String>,
    pub max_aliases: usize,
}

impl Default for YamlDataFormat {
    fn default() -> Self {
        YamlDataFormat { pretty_flow: false, flow_style: FlowStyle::Auto, allow_any_type: false, type_filter: Vec::new(), max_aliases: 50 }
    }
}

fn is_core_tag(t: &str) -> bool {
    const CORE: [&str; 12] = ["str", "int", "float", "bool", "null", "binary", "map", "seq", "timestamp", "omap", "set", "merge"];
    if t == "!" {
        return true;
    }
    let name = t.strip_prefix("!!").or_else(|| t.strip_prefix("tag:yaml.org,2002:"));
    matches!(name, Some(n) if CORE.contains(&n))
}

impl YamlDataFormat {
    fn tag_allowed(&self, tag: &str) -> bool {
        if self.allow_any_type || is_core_tag(tag) {
            return true;
        }
        let name = tag.trim_start_matches('!');
        let name = name.strip_prefix("tag:yaml.org,2002:").unwrap_or(name);
        self.type_filter.iter().any(|f| match f.strip_suffix('*') {
            Some(p) => name.starts_with(p),
            None => f == name,
        })
    }

    fn check(&self, n: &Node, aliases: &mut usize) -> Result<()> {
        if let Some(t) = &n.tag {
            if !self.tag_allowed(t) {
                return Err(df_err(format!("Global tag is not allowed: {} (line {}, column {}); set allowAnyType=true or typeFilter", t, n.line, n.column)));
            }
        }
        match &n.kind {
            NodeKind::Scalar(..) => {}
            NodeKind::Sequence(items) => {
                for i in items {
                    self.check(i, aliases)?;
                }
            }
            NodeKind::Mapping(entries) => {
                for (k, v) in entries {
                    self.check(k, aliases)?;
                    self.check(v, aliases)?;
                }
            }
        }
        Ok(())
    }

    /// Parse YAML text to a value with the safe-constructor checks.
    pub fn parse(&self, text: &str) -> Result<Value> {
        let aliases = count_aliases(text);
        if aliases > self.max_aliases {
            return Err(df_err(format!("Number of aliases for non-scalar nodes exceeds the specified max={}", self.max_aliases)));
        }
        let nodes = parse_nodes(text).map_err(|e| df_err(e.to_string()))?;
        let first = match nodes.into_iter().next() {
            Some(n) => n,
            None => return Ok(Value::Null),
        };
        let mut a = 0;
        self.check(&first, &mut a)?;
        strip_tags_value(&first, self.allow_any_type)
    }

    /// Emit a value as YAML text.
    pub fn emit(&self, v: &Value) -> String {
        let mut out = String::new();
        if self.flow_style == FlowStyle::Flow || (self.flow_style == FlowStyle::Auto && !has_nested(v)) {
            flow(v, 0, self.pretty_flow, &mut out);
            out.push('\n');
            return out;
        }
        match v {
            Value::Object(m) if !m.is_empty() => block_map(self, m, 0, &mut out),
            Value::Array(a) if !a.is_empty() => block_seq(self, a, 0, &mut out),
            other => {
                out.push_str(&scalar(other, false));
                out.push('\n');
            }
        }
        out
    }
}

fn count_aliases(text: &str) -> usize {
    // aliases `*name` outside quotes (approximation sufficient for the limit)
    let mut n = 0;
    let mut quote: Option<char> = None;
    let mut prev = ' ';
    for c in text.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None => {
                if (c == '"' || c == '\'') && (prev.is_whitespace() || "[{,:-".contains(prev)) {
                    quote = Some(c);
                } else if c == '*' && (prev.is_whitespace() || "[{,:-".contains(prev)) {
                    n += 1;
                }
            }
        }
        prev = c;
    }
    n
}

/// Convert the node, ignoring application tags when they are allowed.
fn strip_tags_value(n: &Node, _allow: bool) -> Result<Value> {
    let mut n = n.clone();
    strip(&mut n);
    n.to_value().map_err(|e| df_err(e.to_string()))
}

fn strip(n: &mut Node) {
    if let Some(t) = &n.tag {
        if !is_core_tag(t) {
            n.tag = None;
        }
    }
    match &mut n.kind {
        NodeKind::Scalar(..) => {}
        NodeKind::Sequence(items) => items.iter_mut().for_each(strip),
        NodeKind::Mapping(entries) => {
            for (k, v) in entries {
                strip(k);
                strip(v);
            }
        }
    }
}

fn is_collection(v: &Value) -> bool {
    matches!(v, Value::Array(_) | Value::Object(_))
}

fn has_nested(v: &Value) -> bool {
    match v {
        Value::Array(a) => a.iter().any(is_collection),
        Value::Object(m) => m.iter().any(|(_, v)| is_collection(v)),
        _ => false,
    }
}

fn quote(s: &str) -> String {
    let mut out = String::new();
    camel_core::value::json_escape(s, &mut out);
    out
}

fn scalar(v: &Value, in_flow: bool) -> String {
    match v {
        Value::String(s) if in_flow && s.contains([',', '[', ']', '{', '}']) => quote(s),
        Value::String(s) if s.contains('\n') => quote(s),
        other => format_scalar(other),
    }
}

fn key_text(k: &str, in_flow: bool) -> String {
    scalar(&Value::String(k.to_string()), in_flow)
}

fn flow(v: &Value, indent: usize, pretty: bool, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Array(a) if !a.is_empty() => {
            out.push('[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&pad(indent + 1));
                }
                flow(item, indent + 1, pretty, out);
            }
            if pretty {
                out.push('\n');
                out.push_str(&pad(indent));
            }
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() => {
            out.push('{');
            for (i, (k, item)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    out.push('\n');
                    out.push_str(&pad(indent + 1));
                }
                out.push_str(&key_text(k, true));
                out.push_str(": ");
                flow(item, indent + 1, pretty, out);
            }
            if pretty {
                out.push('\n');
                out.push_str(&pad(indent));
            }
            out.push('}');
        }
        other => out.push_str(&scalar(other, true)),
    }
}

/// Write a nested value after `key:` or `-` in block context.
fn block_child(df: &YamlDataFormat, v: &Value, indent: usize, in_seq: bool, out: &mut String) {
    let use_flow = !is_collection(v) || is_empty_collection(v) || df.flow_style == FlowStyle::Flow || (df.flow_style == FlowStyle::Auto && !has_nested(v));
    if use_flow {
        out.push(' ');
        if is_collection(v) {
            flow(v, indent, df.pretty_flow, out);
        } else {
            out.push_str(&scalar(v, false));
        }
        out.push('\n');
        return;
    }
    match v {
        Value::Object(m) if in_seq => {
            // compact `- key: value`
            out.push(' ');
            let mut first = true;
            for (k, item) in m.iter() {
                if !first {
                    out.push_str(&"  ".repeat(indent + 1));
                }
                first = false;
                out.push_str(&key_text(k, false));
                out.push(':');
                block_child(df, item, indent + 1, false, out);
            }
        }
        Value::Object(m) => {
            out.push('\n');
            block_map(df, m, indent + 1, out);
        }
        Value::Array(a) => {
            out.push('\n');
            block_seq(df, a, if in_seq { indent + 1 } else { indent }, out);
        }
        _ => unreachable!(),
    }
}

fn is_empty_collection(v: &Value) -> bool {
    matches!(v, Value::Array(a) if a.is_empty()) || matches!(v, Value::Object(m) if m.is_empty())
}

fn block_map(df: &YamlDataFormat, m: &camel_core::value::ObjectMap, indent: usize, out: &mut String) {
    for (k, v) in m.iter() {
        out.push_str(&"  ".repeat(indent));
        out.push_str(&key_text(k, false));
        out.push(':');
        block_child(df, v, indent, false, out);
    }
}

fn block_seq(df: &YamlDataFormat, a: &[Value], indent: usize, out: &mut String) {
    for v in a {
        out.push_str(&"  ".repeat(indent));
        out.push('-');
        block_child(df, v, indent, true, out);
    }
}

impl DataFormat for YamlDataFormat {
    fn name(&self) -> &str {
        "yaml"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = self.emit(&ex.message.body);
        ex.message.body = Value::String(text);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        if let Value::String(_) | Value::Bytes(_) = &ex.message.body {
            let t = body_text(&ex.message.body)?;
            ex.message.body = self.parse(&t)?;
        }
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let flow_style = match o.get(&["defaultFlowStyle", "flowStyle"]).map(|s| s.to_ascii_uppercase()) {
        None => FlowStyle::Auto,
        Some(s) if s == "AUTO" => FlowStyle::Auto,
        Some(s) if s == "BLOCK" => FlowStyle::Block,
        Some(s) if s == "FLOW" => FlowStyle::Flow,
        Some(s) => return Err(camel_core::CamelError::illegal_argument(format!("Unknown flow style: {}", s))),
    };
    Ok(Arc::new(YamlDataFormat {
        pretty_flow: o.bool(&["prettyFlow"], false)?,
        flow_style,
        allow_any_type: o.bool(&["allowAnyType"], false)?,
        type_filter: o.get(&["typeFilter", "typeFilters"]).map(|s| s.split(',').map(|x| x.trim().trim_start_matches("type:").to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
        max_aliases: o.num(&["maxAliasesForCollections"], 50)?.max(0) as usize,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_core::json;

    fn df() -> YamlDataFormat {
        YamlDataFormat::default()
    }

    #[test]
    fn snakeyaml_like_output() {
        let v = json::parse(r#"{"name":"Camel"}"#).unwrap();
        assert_eq!(df().emit(&v), "{name: Camel}\n");
        let pretty = YamlDataFormat { pretty_flow: true, ..df() };
        assert_eq!(pretty.emit(&v), "{\n  name: Camel\n}\n");
        let block = YamlDataFormat { flow_style: FlowStyle::Block, ..df() };
        assert_eq!(block.emit(&v), "name: Camel\n");
        let v = json::parse(r#"{"a":1,"l":[1,"x, y"],"m":{"k":[{"z":true}]}}"#).unwrap();
        let text = df().emit(&v);
        assert_eq!(text, "a: 1\nl: [1, \"x, y\"]\nm:\n  k:\n  - {z: true}\n");
        assert_eq!(df().parse(&text).unwrap(), v);
        let text = block.emit(&v);
        assert_eq!(df().parse(&text).unwrap(), v, "{}", text);
        let text = YamlDataFormat { flow_style: FlowStyle::Flow, pretty_flow: true, ..df() }.emit(&v);
        assert_eq!(df().parse(&text).unwrap(), v, "{}", text);
    }

    #[test]
    fn safe_constructor() {
        let e = df().parse("!!com.acme.Pojo {name: x}").unwrap_err();
        assert!(e.to_string().contains("Global tag is not allowed"), "{}", e);
        let any = YamlDataFormat { allow_any_type: true, ..df() };
        assert_eq!(any.parse("!!com.acme.Pojo {name: x}").unwrap(), json::parse(r#"{"name":"x"}"#).unwrap());
        let filtered = YamlDataFormat { type_filter: vec!["com.acme.*".into()], ..df() };
        assert!(filtered.parse("!!com.acme.Pojo {name: x}").is_ok());
        assert_eq!(df().parse("a: !!str 1").unwrap(), json::parse(r#"{"a":"1"}"#).unwrap());
        let limited = YamlDataFormat { max_aliases: 1, ..df() };
        assert!(limited.parse("a: &x [1]\nb: *x\nc: *x\n").is_err());
    }
}
