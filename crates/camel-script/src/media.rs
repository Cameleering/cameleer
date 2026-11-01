//! DataSonnet media types: reading the payload (`bodyMediaType`) and writing the result
//! (`outputMediaType`).
//!
//! | media type | payload | output |
//! |---|---|---|
//! | `application/json` (default) | parsed JSON | JSON text (sorted keys) |
//! | `text/plain` | the body as a string | the result as text |
//! | `application/xml`, `text/xml` | BadgerFish object (`@attr`, `$` text, repeated elements as arrays) | XML text from a single-root BadgerFish object |
//! | `application/csv`, `text/csv` | array of objects (header row) or arrays (`header=false`) | CSV text |
//! | `application/x-www-form-urlencoded` | object of decoded fields | encoded form |
//! | `application/x-java-object` | the body value | the result value |
//!
//! Media type parameters configure CSV (`header`, `separator`, `quote`) and XML
//! (`indent=true`, `xmlDeclaration=true`), e.g. `application/csv; separator=;`.

use std::collections::BTreeMap;

use camel_core::error::{kinds, CamelError, Result};
use camel_core::language::xml::{escape_attr, escape_text, Document, NodeKind};
use camel_core::value::Value;

use crate::interp::Interp;
use crate::value::*;

struct MediaType {
    essence: String,
    params: BTreeMap<String, String>,
}

fn parse_media_type(s: &str) -> MediaType {
    let mut parts = s.split(';');
    let essence = parts.next().unwrap_or("").trim().to_ascii_lowercase();
    let mut params = BTreeMap::new();
    for p in parts {
        if let Some((k, v)) = p.split_once('=') {
            params.insert(k.trim().to_ascii_lowercase(), v.trim().trim_matches('"').to_string());
        }
    }
    MediaType { essence, params }
}

fn param<'a>(mt: &'a MediaType, opts: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    mt.params.get(&name.to_ascii_lowercase()).map(|s| s.as_str()).or_else(|| opts.get(name).map(|s| s.as_str()))
}

fn is_json(e: &str) -> bool {
    e.contains("json")
}
fn is_xml(e: &str) -> bool {
    e.ends_with("/xml") || e.ends_with("+xml")
}
fn is_csv(e: &str) -> bool {
    e.ends_with("/csv")
}

fn body_text(body: &Value) -> String {
    match body {
        Value::String(s) => s.clone(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Convert the message body to the DataSonnet `payload`.
pub fn read_payload(body: &Value, media: &str, opts: &BTreeMap<String, String>) -> Result<V> {
    let mt = parse_media_type(media);
    let e = mt.essence.as_str();
    if e == "application/x-java-object" || e.starts_with("application/x-java-object") {
        return Ok(from_value(body));
    }
    let textual = matches!(body, Value::String(_) | Value::Bytes(_) | Value::Null);
    if !textual {
        // already structured: use as is
        return Ok(from_value(body));
    }
    let text = body_text(body);
    if is_json(e) {
        if text.trim().is_empty() {
            return Ok(V::Null);
        }
        return Ok(crate::json::parse(&text).unwrap_or_else(|_| V::string(text)));
    }
    if e.starts_with("text/plain") || e == "text/*" {
        return Ok(V::string(text));
    }
    if is_xml(e) {
        if text.trim().is_empty() {
            return Ok(V::Null);
        }
        let doc = Document::parse(&text)?;
        let root = doc.root_element().ok_or_else(|| CamelError::new(kinds::PARSE, "XML payload has no root element"))?;
        let xo = XmlOpts::new(&mt, opts);
        let mut p = PropMap::default();
        let v = badgerfish(&doc, root, &xo);
        let name = xo.element_name(doc.name(root), &scopes_for(&doc, root));
        p.set(&name, v);
        return Ok(V::obj(p));
    }
    if is_csv(e) {
        let sep = param(&mt, opts, "separator").and_then(|s| s.chars().next()).unwrap_or(',');
        let quote = param(&mt, opts, "quote").and_then(|s| s.chars().next()).unwrap_or('"');
        let header = param(&mt, opts, "header").map(|h| h != "false").unwrap_or(true);
        let rows = parse_csv(&text, sep, quote);
        if !header {
            return Ok(V::arr(rows.into_iter().map(|r| V::arr(r.into_iter().map(V::string).collect())).collect()));
        }
        let mut it = rows.into_iter();
        let head = it.next().unwrap_or_default();
        let mut out = Vec::new();
        for r in it {
            let mut p = PropMap::default();
            for (i, h) in head.iter().enumerate() {
                p.set(h, V::string(r.get(i).cloned().unwrap_or_default()));
            }
            out.push(V::obj(p));
        }
        return Ok(V::arr(out));
    }
    if e == "application/x-www-form-urlencoded" {
        let mut p = PropMap::default();
        for pair in text.split('&').filter(|s| !s.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            p.set(&form_decode(k), V::string(form_decode(v)));
        }
        return Ok(V::obj(p));
    }
    Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("DataSonnet: unsupported body media type '{}' (supported: application/json, text/plain, application/xml, application/csv, application/x-www-form-urlencoded, application/x-java-object)", media)))
}

/// Options of the DataSonnet XML plugin (media type parameters, case-insensitive):
/// `TextValueKey` (`$`), `AttributeCharacterKey` (`@`), `CDataValueKey` (`#`, output only),
/// `NamespaceSeparator` (`:`), `NamespaceDeclarations.<prefix>=<uri>` (reading: elements and
/// attributes of that namespace use the prefix; writing: declared on the root element), and
/// for output `RootElement`, `OmitXmlDeclaration`, `XmlVersion`, `Encoding`,
/// `AutoEmptyElements`, `NullAsEmptyElement`.
struct XmlOpts {
    text: String,
    attr: String,
    cdata: String,
    sep: String,
    /// uri -> preferred prefix
    decls: Vec<(String, String)>,
}

impl XmlOpts {
    fn new(mt: &MediaType, opts: &BTreeMap<String, String>) -> XmlOpts {
        let get = |n: &str, d: &str| param(mt, opts, n).map(|s| s.to_string()).unwrap_or_else(|| d.to_string());
        let mut decls = Vec::new();
        for (k, v) in mt.params.iter() {
            if let Some(pfx) = k.strip_prefix("namespacedeclarations.") {
                decls.push((v.clone(), pfx.to_string()));
            }
        }
        for (k, v) in opts.iter() {
            if let Some(pfx) = k.strip_prefix("NamespaceDeclarations.").or_else(|| k.strip_prefix("namespaceDeclarations.")) {
                decls.push((v.clone(), pfx.to_string()));
            }
        }
        XmlOpts { text: get("TextValueKey", "$"), attr: get("AttributeCharacterKey", "@"), cdata: get("CDataValueKey", "#"), sep: get("NamespaceSeparator", ":"), decls }
    }

    /// The output name of a (possibly prefixed) element / attribute name.
    fn element_name(&self, name: &str, scopes: &[(String, String)]) -> String {
        let (pfx, local) = match name.split_once(':') {
            Some((p, l)) => (p, l),
            None => ("", name),
        };
        let uri = scopes.iter().rev().find(|(p, _)| p == pfx).map(|(_, u)| u.clone());
        if let Some(uri) = uri {
            if let Some((_, np)) = self.decls.iter().find(|(u, _)| *u == uri) {
                return if np.is_empty() || np == "$" { local.to_string() } else { format!("{}{}{}", np, self.sep, local) };
            }
        }
        if pfx.is_empty() {
            local.to_string()
        } else {
            format!("{}{}{}", pfx, self.sep, local)
        }
    }
}

/// The namespace declarations in scope at element `i` (prefix "" for the default namespace).
fn scopes_for(doc: &Document, i: usize) -> Vec<(String, String)> {
    let mut chain = vec![];
    let mut cur = Some(i);
    while let Some(n) = cur {
        if matches!(doc.nodes[n].kind, NodeKind::Element { .. }) {
            chain.push(n);
        }
        cur = doc.nodes[n].parent;
    }
    let mut out = vec![];
    for n in chain.into_iter().rev() {
        for (k, v) in doc.attrs(n) {
            if k == "xmlns" {
                out.push((String::new(), v.clone()));
            } else if let Some(p) = k.strip_prefix("xmlns:") {
                out.push((p.to_string(), v.clone()));
            }
        }
    }
    out
}

fn badgerfish(doc: &Document, i: usize, xo: &XmlOpts) -> V {
    let mut p = PropMap::default();
    let in_scope = scopes_for(doc, i);
    let mut ns = PropMap::default();
    for (k, v) in doc.attrs(i) {
        if k == "xmlns" {
            ns.set("$", V::str(v));
        } else if let Some(pfx) = k.strip_prefix("xmlns:") {
            ns.set(pfx, V::str(v));
        } else {
            let name = if k.contains(':') { xo.element_name(k, &in_scope) } else { k.clone() };
            p.set(&format!("{}{}", xo.attr, name), V::str(v));
        }
    }
    if !ns.is_empty() {
        p.set(&format!("{}xmlns", xo.attr), V::obj(ns));
    }
    let mut text = String::new();
    let mut groups: Vec<(String, Vec<V>)> = Vec::new();
    for &c in &doc.nodes[i].children {
        match &doc.nodes[c].kind {
            NodeKind::Text(t) => text.push_str(t),
            NodeKind::Element { name, .. } => {
                let v = badgerfish(doc, c, xo);
                let name = xo.element_name(name, &scopes_for(doc, c));
                match groups.iter_mut().find(|(n, _)| *n == name) {
                    Some((_, l)) => l.push(v),
                    None => groups.push((name, vec![v])),
                }
            }
            _ => {}
        }
    }
    for (n, mut l) in groups {
        let v = if l.len() == 1 { l.pop().unwrap() } else { V::arr(l) };
        p.set(&n, v);
    }
    if !text.trim().is_empty() {
        p.set(&xo.text, V::string(text.trim().to_string()));
    }
    V::obj(p)
}

fn form_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(x) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(x);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'*' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// RFC 4180 CSV parsing.
pub fn parse_csv(text: &str, sep: char, quote: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == quote {
                if chars.peek() == Some(&quote) {
                    field.push(quote);
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            _ if c == quote => {
                quoted = true;
                any = true;
            }
            _ if c == sep => {
                row.push(std::mem::take(&mut field));
                any = true;
            }
            '\r' => {}
            '\n' => {
                if any || !field.is_empty() {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                any = false;
            }
            _ => {
                field.push(c);
                any = true;
            }
        }
    }
    if any || !field.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows
}

fn csv_field(s: &str, sep: char, quote: char) -> String {
    if s.contains(sep) || s.contains(quote) || s.contains('\n') || s.contains('\r') {
        let q = quote.to_string();
        format!("{}{}{}", quote, s.replace(&q, &format!("{}{}", quote, quote)), quote)
    } else {
        s.to_string()
    }
}

/// Convert the DataSonnet result for the output media type.
pub fn write_output(it: &mut Interp, v: &V, out: Value, media: &str, opts: &BTreeMap<String, String>) -> Result<Value> {
    let mt = parse_media_type(media);
    let e = mt.essence.as_str();
    if e.starts_with("application/x-java-object") {
        return Ok(out);
    }
    if is_json(e) {
        return Ok(Value::String(crate::json::stringify(v, &crate::json::JsonOpts { indent: None, item_sep: "", key_sep: "", sort_keys: true, js: false, newline: "\n" }).unwrap_or_else(|| "null".into())));
    }
    if e.starts_with("text/plain") || e == "text/*" {
        return Ok(Value::String(it.display(v)));
    }
    if is_xml(e) {
        let indent = param(&mt, opts, "indent").is_some_and(|x| x == "true");
        let decl = param(&mt, opts, "xmlDeclaration").is_some_and(|x| x == "true") || param(&mt, opts, "OmitXmlDeclaration").is_some_and(|x| x == "false");
        let xo = XmlOpts::new(&mt, opts);
        let wo = WriteOpts {
            auto_empty: param(&mt, opts, "AutoEmptyElements").map(|x| x != "false").unwrap_or(true),
            null_as_empty: param(&mt, opts, "NullAsEmptyElement").map(|x| x != "false").unwrap_or(true),
        };
        let out = match param(&mt, opts, "RootElement") {
            Some(root) if !root.is_empty() => {
                let mut m = camel_core::value::ObjectMap::new();
                m.insert(root.to_string(), out);
                Value::Object(m)
            }
            _ => out,
        };
        let Value::Object(o) = &out else {
            return Err(CamelError::new(kinds::EXPRESSION_EVALUATION, "DataSonnet: XML output requires an object with a single root element key"));
        };
        if o.len() != 1 {
            return Err(CamelError::new(kinds::EXPRESSION_EVALUATION, format!("DataSonnet: XML output requires exactly one root element, got {} keys", o.len())));
        }
        let (name, val) = o.iter().next().unwrap();
        let mut s = String::new();
        if decl {
            let version = param(&mt, opts, "XmlVersion").unwrap_or("1.0");
            let enc = param(&mt, opts, "Encoding").unwrap_or("UTF-8");
            s.push_str(&format!("<?xml version=\"{}\" encoding=\"{}\"?>", version, enc));
            if indent {
                s.push('\n');
            }
        }
        // namespace declarations of the options go on the root element
        let val = if xo.decls.is_empty() {
            val.clone()
        } else {
            let mut v = match val {
                Value::Object(m) => m.clone(),
                other => {
                    let mut m = camel_core::value::ObjectMap::new();
                    m.insert(xo.text.clone(), other.clone());
                    m
                }
            };
            let mut ns = match v.get(&format!("{}xmlns", xo.attr)) {
                Some(Value::Object(m)) => m.clone(),
                _ => camel_core::value::ObjectMap::new(),
            };
            for (uri, pfx) in &xo.decls {
                ns.insert(if pfx.is_empty() { "$".to_string() } else { pfx.clone() }, Value::String(uri.clone()));
            }
            v.insert(format!("{}xmlns", xo.attr), Value::Object(ns));
            Value::Object(v)
        };
        write_xml(&xml_name(name, &xo), &val, &mut s, if indent { Some(0) } else { None }, &xo, &wo);
        return Ok(Value::String(s));
    }
    if is_csv(e) {
        let sep = param(&mt, opts, "separator").and_then(|s| s.chars().next()).unwrap_or(',');
        let quote = param(&mt, opts, "quote").and_then(|s| s.chars().next()).unwrap_or('"');
        let header = param(&mt, opts, "header").map(|h| h != "false").unwrap_or(true);
        let Value::Array(rows) = &out else {
            return Err(CamelError::new(kinds::EXPRESSION_EVALUATION, "DataSonnet: CSV output requires an array"));
        };
        let mut s = String::new();
        let cell = |v: &Value| match v {
            Value::String(x) => x.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        let mut cols: Vec<String> = Vec::new();
        for r in rows {
            if let Value::Object(o) = r {
                for k in o.keys() {
                    if !cols.contains(k) {
                        cols.push(k.clone());
                    }
                }
            }
        }
        if header && !cols.is_empty() {
            s.push_str(&cols.iter().map(|c| csv_field(c, sep, quote)).collect::<Vec<_>>().join(&sep.to_string()));
            s.push('\n');
        }
        for r in rows {
            let fields: Vec<String> = match r {
                Value::Object(o) => cols.iter().map(|c| o.get(c).map(cell).unwrap_or_default()).collect(),
                Value::Array(a) => a.iter().map(cell).collect(),
                other => vec![cell(other)],
            };
            s.push_str(&fields.iter().map(|f| csv_field(f, sep, quote)).collect::<Vec<_>>().join(&sep.to_string()));
            s.push('\n');
        }
        return Ok(Value::String(s));
    }
    if e == "application/x-www-form-urlencoded" {
        let Value::Object(o) = &out else {
            return Err(CamelError::new(kinds::EXPRESSION_EVALUATION, "DataSonnet: form output requires an object"));
        };
        let parts: Vec<String> = o
            .iter()
            .map(|(k, v)| {
                let s = match v {
                    Value::String(x) => x.clone(),
                    other => other.to_string(),
                };
                format!("{}={}", form_encode(k), form_encode(&s))
            })
            .collect();
        return Ok(Value::String(parts.join("&")));
    }
    Err(CamelError::new(kinds::UNSUPPORTED_OPERATION, format!("DataSonnet: unsupported output media type '{}'", media)))
}

struct WriteOpts {
    auto_empty: bool,
    null_as_empty: bool,
}

/// An XML name from a key using the namespace separator.
fn xml_name(k: &str, xo: &XmlOpts) -> String {
    if xo.sep != ":" {
        if let Some((p, l)) = k.split_once(xo.sep.as_str()) {
            return format!("{}:{}", p, l);
        }
    }
    k.to_string()
}

fn write_xml(name: &str, v: &Value, out: &mut String, indent: Option<usize>, xo: &XmlOpts, wo: &WriteOpts) {
    let pad = |out: &mut String, lvl: usize| {
        if indent.is_some() {
            out.push('\n');
            out.push_str(&"  ".repeat(lvl));
        }
    };
    let lvl = indent.unwrap_or(0);
    match v {
        Value::Array(items) => {
            for (k, it) in items.iter().enumerate() {
                if k > 0 {
                    pad(out, lvl);
                }
                write_xml(name, it, out, indent, xo, wo);
            }
        }
        Value::Object(o) => {
            out.push('<');
            out.push_str(name);
            let xmlns_key = format!("{}xmlns", xo.attr);
            for (k, a) in o.iter() {
                if *k == xmlns_key {
                    if let Value::Object(ns) = a {
                        for (p, u) in ns.iter() {
                            let uri = match u {
                                Value::String(x) => x.clone(),
                                other => other.to_string(),
                            };
                            if p == "$" || p.is_empty() {
                                out.push_str(&format!(" xmlns=\"{}\"", escape_attr(&uri)));
                            } else {
                                out.push_str(&format!(" xmlns:{}=\"{}\"", p, escape_attr(&uri)));
                            }
                        }
                    }
                    continue;
                }
                if let Some(an) = k.strip_prefix(xo.attr.as_str()) {
                    let s = match a {
                        Value::String(x) => x.clone(),
                        other => other.to_string(),
                    };
                    out.push_str(&format!(" {}=\"{}\"", xml_name(an, xo), escape_attr(&s)));
                }
            }
            let children: Vec<(&String, &Value)> = o.iter().filter(|(k, v)| !k.starts_with(xo.attr.as_str()) && !(matches!(v, Value::Null) && !wo.null_as_empty)).collect();
            if children.is_empty() {
                if wo.auto_empty {
                    out.push_str("/>");
                } else {
                    out.push_str(&format!("></{}>", name));
                }
                return;
            }
            out.push('>');
            let mut nested = false;
            for (k, c) in &children {
                if **k == xo.text {
                    let s = match c {
                        Value::String(x) => x.clone(),
                        other => other.to_string(),
                    };
                    out.push_str(&escape_text(&s));
                } else if **k == xo.cdata {
                    let s = match c {
                        Value::String(x) => x.clone(),
                        other => other.to_string(),
                    };
                    out.push_str(&format!("<![CDATA[{}]]>", s.replace("]]>", "]]]]><![CDATA[>")));
                } else {
                    nested = true;
                    pad(out, lvl + 1);
                    write_xml(&xml_name(k, xo), c, out, indent.map(|l| l + 1), xo, wo);
                }
            }
            if nested {
                pad(out, lvl);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
        Value::Null => {
            if wo.auto_empty {
                out.push_str(&format!("<{}/>", name));
            } else {
                out.push_str(&format!("<{}></{}>", name, name));
            }
        }
        other => {
            let s = match other {
                Value::String(x) => x.clone(),
                o => o.to_string(),
            };
            out.push_str(&format!("<{}>{}</{}>", name, escape_text(&s), name));
        }
    }
}
