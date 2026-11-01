//! ASN.1 module compiler for the `asn1` data format: parses ASN.1 module definitions
//! (X.680 subset) at runtime and encodes / decodes values of a named type with BER/DER,
//! replacing the classes Camel users generate with jASN1 / ASN1bean.
//!
//! Supported notation: `Module DEFINITIONS [EXPLICIT|IMPLICIT|AUTOMATIC TAGS]
//! [EXTENSIBILITY IMPLIED] ::= BEGIN ... END` (several modules per text; `EXPORTS` /
//! `IMPORTS` are skipped — imported names resolve against every parsed module), type
//! assignments and value assignments (`maxSize INTEGER ::= 10`, `id OBJECT IDENTIFIER ::=
//! { 1 2 3 }`). Types: `BOOLEAN`, `INTEGER` (named numbers), `ENUMERATED`, `REAL`, `NULL`,
//! `OCTET STRING`, `BIT STRING` (named bits), `OBJECT IDENTIFIER`, `RELATIVE-OID`, the
//! character string types, `UTCTime`, `GeneralizedTime`, `SEQUENCE` / `SET` (components with
//! `OPTIONAL`, `DEFAULT`, `COMPONENTS OF`, extension markers and `[[ ]]` version groups),
//! `SEQUENCE OF` / `SET OF` (with `SIZE` constraints and element names), `CHOICE`,
//! `ANY [DEFINED BY ..]`, tagged types (`[APPLICATION 1] IMPLICIT T`, `[2] EXPLICIT T`,
//! module tag defaults incl. automatic tagging, implicit tags on `CHOICE`/`ANY` turned
//! explicit) and type references; constraints are parsed and ignored.
//!
//! Value mapping: `SEQUENCE`/`SET` → objects keyed by component name (absent `OPTIONAL`s
//! omitted, `DEFAULT`s filled on decode and left out of the DER encoding when equal),
//! `CHOICE` → `{"alternative": value}`, `SEQUENCE OF` → arrays, `INTEGER` → integers (or
//! decimal strings beyond 64 bits), `ENUMERATED` → item names, `BIT STRING` with named bits →
//! array of the names of set bits (bytes otherwise), `OBJECT IDENTIFIER` → dotted string,
//! `OCTET STRING` → bytes, strings / times → strings, `REAL` → floats, `ANY` → the generic
//! tree of [`crate::asn1`].

use std::collections::HashMap;

use camel_core::error::{CamelError, Result};
use camel_core::value::ObjectMap;
use camel_core::Value;

use crate::asn1::{decode_int, decode_oid, encode_int, encode_oid, read_tlv, tlv, Tlv};

fn aerr(msg: impl Into<String>) -> CamelError {
    CamelError::new("ASN1Exception", msg)
}

/// A tag.
#[derive(Clone, Debug, PartialEq)]
pub struct TagSpec {
    pub class: u8,
    pub num: u64,
    pub implicit: bool,
}

/// A component of a SEQUENCE / SET / CHOICE.
#[derive(Clone, Debug)]
pub struct Component {
    pub name: String,
    pub ty: Type,
    pub optional: bool,
    pub default: Option<Value>,
}

/// The base of a type.
#[derive(Clone, Debug)]
pub enum Base {
    Boolean,
    Integer(Vec<(String, i64)>),
    Enumerated(Vec<(String, i64)>),
    Real,
    Null,
    OctetString,
    BitString(Vec<(String, i64)>),
    Oid,
    RelativeOid,
    /// Character string / time type with its universal tag.
    Str(u64),
    Sequence(Vec<Component>, bool),
    Set(Vec<Component>, bool),
    SequenceOf(Box<Type>),
    SetOf(Box<Type>),
    Choice(Vec<Component>, bool),
    Any,
    Ref(String),
}

/// A type: tags (outermost first) and a base.
#[derive(Clone, Debug)]
pub struct Type {
    pub tags: Vec<TagSpec>,
    pub base: Base,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum TagDefault {
    Explicit,
    Implicit,
    Automatic,
}

/// Parsed ASN.1 modules: types and values by name.
#[derive(Clone, Debug, Default)]
pub struct Asn1Module {
    pub types: HashMap<String, Type>,
    pub values: HashMap<String, Value>,
    pub module_names: Vec<String>,
}

// ----------------------------------------------------------------------------- lexer

fn tokenize(src: &str) -> Vec<String> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '-' && c.get(i + 1) == Some(&'-') {
            // comment until end of line or the next "--"
            i += 2;
            while i < c.len() && c[i] != '\n' && !(c[i] == '-' && c.get(i + 1) == Some(&'-')) {
                i += 1;
            }
            if i < c.len() && c[i] == '-' {
                i += 2;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if ch == ':' && c.get(i + 1) == Some(&':') && c.get(i + 2) == Some(&'=') {
            out.push("::=".into());
            i += 3;
        } else if ch == '.' && c.get(i + 1) == Some(&'.') && c.get(i + 2) == Some(&'.') {
            out.push("...".into());
            i += 3;
        } else if ch == '.' && c.get(i + 1) == Some(&'.') {
            out.push("..".into());
            i += 2;
        } else if (ch == '[' || ch == ']') && c.get(i + 1) == Some(&ch) {
            out.push(format!("{}{}", ch, ch));
            i += 2;
        } else if ch == '"' {
            let s = i;
            i += 1;
            while i < c.len() {
                if c[i] == '"' && c.get(i + 1) == Some(&'"') {
                    i += 2;
                } else if c[i] == '"' {
                    break;
                } else {
                    i += 1;
                }
            }
            i += 1;
            out.push(c[s..i.min(c.len())].iter().collect());
        } else if ch == '\'' {
            let s = i;
            i += 1;
            while i < c.len() && c[i] != '\'' {
                i += 1;
            }
            i += 1;
            if i < c.len() && (c[i] == 'H' || c[i] == 'B') {
                i += 1;
            }
            out.push(c[s..i.min(c.len())].iter().collect());
        } else if ch.is_alphanumeric() || ch == '_' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || (c[i] == '-' && c.get(i + 1).map(|n| n.is_alphanumeric()).unwrap_or(false) && c[i - 1] != '-')) {
                i += 1;
            }
            // qualified references Module.Type
            if i < c.len() && c[i] == '.' && c.get(i + 1).map(|n| n.is_alphabetic()).unwrap_or(false) && c[s].is_uppercase() {
                i += 1;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '-') {
                    i += 1;
                }
            }
            out.push(c[s..i].iter().collect());
        } else {
            out.push(ch.to_string());
            i += 1;
        }
    }
    out
}

struct P {
    t: Vec<String>,
    i: usize,
    default: TagDefault,
    extensible: bool,
}

impl P {
    fn peek(&self) -> Option<&str> {
        self.t.get(self.i).map(|s| s.as_str())
    }
    fn peek_at(&self, k: usize) -> Option<&str> {
        self.t.get(self.i + k).map(|s| s.as_str())
    }
    fn next(&mut self) -> Result<String> {
        let t = self.t.get(self.i).cloned().ok_or_else(|| aerr("Unexpected end of ASN.1 module"))?;
        self.i += 1;
        Ok(t)
    }
    fn eat(&mut self, s: &str) -> bool {
        if self.peek() == Some(s) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        let t = self.next()?;
        if t != s {
            return Err(aerr(format!("Expected '{}' but found '{}' in ASN.1 module", s, t)));
        }
        Ok(())
    }
    /// Skips a balanced group starting at the current open token.
    fn skip_group(&mut self) -> Result<()> {
        let open = self.next()?;
        let close = match open.as_str() {
            "(" => ")",
            "{" => "}",
            "[" => "]",
            _ => return Ok(()),
        };
        let mut depth = 1;
        while depth > 0 {
            let t = self.next()?;
            if t == open {
                depth += 1;
            } else if t == close {
                depth -= 1;
            }
        }
        Ok(())
    }
    fn skip_constraints(&mut self) -> Result<()> {
        while self.peek() == Some("(") {
            self.skip_group()?;
        }
        Ok(())
    }

    fn named_numbers(&mut self) -> Result<Vec<(String, i64)>> {
        let mut out = Vec::new();
        self.expect("{")?;
        let mut next = 0i64;
        while !self.eat("}") {
            let t = self.next()?;
            if t == "," || t == "..." {
                continue;
            }
            let v = if self.eat("(") {
                let mut neg = false;
                let mut n = self.next()?;
                if n == "-" {
                    neg = true;
                    n = self.next()?;
                }
                self.expect(")")?;
                let v = n.parse::<i64>().map_err(|_| aerr(format!("Invalid number {} in ASN.1 module", n)))?;
                if neg {
                    -v
                } else {
                    v
                }
            } else {
                next
            };
            next = v + 1;
            out.push((t, v));
        }
        Ok(out)
    }

    fn components(&mut self, choice: bool) -> Result<(Vec<Component>, bool)> {
        self.expect("{")?;
        let mut comps = Vec::new();
        let mut ext = self.extensible;
        while !self.eat("}") {
            match self.peek() {
                Some(",") | Some("[[") | Some("]]") => {
                    self.i += 1;
                    if self.peek_at(0).map(|s| s.chars().all(|c| c.is_ascii_digit())).unwrap_or(false) && self.peek_at(1) == Some(":") {
                        self.i += 2; // version number of a version group
                    }
                }
                Some("...") => {
                    self.i += 1;
                    ext = true;
                    if self.eat("!") {
                        // exception spec
                        let _ = self.next()?;
                    }
                }
                Some("COMPONENTS") => {
                    self.i += 1;
                    self.expect("OF")?;
                    let t = self.ty()?;
                    comps.push(Component { name: "COMPONENTS OF".into(), ty: t, optional: false, default: None });
                }
                _ => {
                    let name = self.next()?;
                    let ty = self.ty()?;
                    let mut c = Component { name, ty, optional: false, default: None };
                    if !choice {
                        if self.eat("OPTIONAL") {
                            c.optional = true;
                        } else if self.eat("DEFAULT") {
                            c.default = Some(self.value()?);
                        }
                    }
                    comps.push(c);
                }
            }
        }
        Ok((comps, ext))
    }

    /// A value (numbers, booleans, strings, identifiers, `{...}` OIDs / lists).
    fn value(&mut self) -> Result<Value> {
        let t = self.next()?;
        Ok(match t.as_str() {
            "TRUE" => Value::Bool(true),
            "FALSE" => Value::Bool(false),
            "NULL" => Value::Null,
            "-" => {
                let n = self.next()?;
                n.parse::<i64>().map(|v| Value::Int(-v)).or_else(|_| n.parse::<f64>().map(|v| Value::Float(-v))).map_err(|_| aerr(format!("Invalid value -{}", n)))?
            }
            "{" => {
                // OID components (numbers, name(number), names) or a list
                let mut parts = Vec::new();
                let mut depth = 1;
                while depth > 0 {
                    let x = self.next()?;
                    match x.as_str() {
                        "{" => depth += 1,
                        "}" => depth -= 1,
                        "(" => {
                            let n = self.next()?;
                            self.expect(")")?;
                            parts.pop();
                            parts.push(n);
                        }
                        "," => {}
                        other => parts.push(other.to_string()),
                    }
                }
                Value::Array(parts.into_iter().map(Value::String).collect())
            }
            s if s.starts_with('"') => Value::String(s[1..s.len() - 1].replace("\"\"", "\"")),
            s if s.starts_with('\'') => {
                let (body, kind) = s[1..].split_once('\'').unwrap_or((s, "H"));
                if kind == "B" {
                    let bits: Vec<u8> = body.bytes().filter(|b| *b == b'0' || *b == b'1').map(|b| b - b'0').collect();
                    Value::Bytes(bits.chunks(8).map(|c| c.iter().enumerate().fold(0u8, |a, (i, b)| a | (b << (7 - i)))).collect())
                } else {
                    Value::Bytes(camel_tls::util::try_unhex(&body.replace(' ', "")).unwrap_or_default())
                }
            }
            s if s.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) => {
                if self.peek() == Some(".") && self.peek_at(1).map(|x| x.chars().all(|c| c.is_ascii_digit())).unwrap_or(false) {
                    self.i += 1;
                    let f = self.next()?;
                    Value::Float(format!("{}.{}", s, f).parse().unwrap_or(0.0))
                } else {
                    s.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::String(s.to_string()))
                }
            }
            other => Value::String(format!("\u{1}{}", other)), // identifier, resolved later
        })
    }

    fn tag(&mut self) -> Result<TagSpec> {
        self.expect("[")?;
        let mut class = 2;
        match self.peek() {
            Some("UNIVERSAL") => {
                class = 0;
                self.i += 1
            }
            Some("APPLICATION") => {
                class = 1;
                self.i += 1
            }
            Some("PRIVATE") => {
                class = 3;
                self.i += 1
            }
            _ => {}
        }
        let n = self.next()?;
        let num = n.parse::<u64>().map_err(|_| aerr(format!("Invalid tag number {}", n)))?;
        self.expect("]")?;
        let implicit = if self.eat("IMPLICIT") {
            true
        } else if self.eat("EXPLICIT") {
            false
        } else {
            self.default != TagDefault::Explicit
        };
        Ok(TagSpec { class, num, implicit })
    }

    fn ty(&mut self) -> Result<Type> {
        let mut tags = Vec::new();
        while self.peek() == Some("[") {
            tags.push(self.tag()?);
        }
        let t = self.next()?;
        let base = match t.as_str() {
            "BOOLEAN" => Base::Boolean,
            "INTEGER" => {
                if self.peek() == Some("{") {
                    Base::Integer(self.named_numbers()?)
                } else {
                    Base::Integer(Vec::new())
                }
            }
            "ENUMERATED" => Base::Enumerated(self.named_numbers()?),
            "REAL" => Base::Real,
            "NULL" => Base::Null,
            "OCTET" => {
                self.expect("STRING")?;
                Base::OctetString
            }
            "BIT" => {
                self.expect("STRING")?;
                if self.peek() == Some("{") {
                    Base::BitString(self.named_numbers()?)
                } else {
                    Base::BitString(Vec::new())
                }
            }
            "OBJECT" => {
                self.expect("IDENTIFIER")?;
                Base::Oid
            }
            "RELATIVE-OID" => Base::RelativeOid,
            "UTF8String" => Base::Str(12),
            "NumericString" => Base::Str(18),
            "PrintableString" => Base::Str(19),
            "TeletexString" | "T61String" => Base::Str(20),
            "VideotexString" => Base::Str(21),
            "IA5String" => Base::Str(22),
            "UTCTime" => Base::Str(23),
            "GeneralizedTime" => Base::Str(24),
            "GraphicString" => Base::Str(25),
            "VisibleString" | "ISO646String" => Base::Str(26),
            "GeneralString" => Base::Str(27),
            "UniversalString" => Base::Str(28),
            "BMPString" => Base::Str(30),
            "ObjectDescriptor" => Base::Str(7),
            "SEQUENCE" | "SET" => {
                let set = t == "SET";
                if self.peek() == Some("SIZE") {
                    self.i += 1;
                    self.skip_constraints()?;
                }
                self.skip_constraints()?;
                if self.eat("OF") {
                    // optional element name: `SEQUENCE OF item Type`
                    if self.peek().map(|s| s.chars().next().map(|c| c.is_lowercase()).unwrap_or(false)).unwrap_or(false) && self.peek_at(1).is_some() && self.peek_at(1) != Some(",") && self.peek_at(1) != Some("}") {
                        self.i += 1;
                    }
                    let e = Box::new(self.ty()?);
                    if set {
                        Base::SetOf(e)
                    } else {
                        Base::SequenceOf(e)
                    }
                } else {
                    let (c, ext) = self.components(false)?;
                    if set {
                        Base::Set(c, ext)
                    } else {
                        Base::Sequence(c, ext)
                    }
                }
            }
            "CHOICE" => {
                let (c, ext) = self.components(true)?;
                Base::Choice(c, ext)
            }
            "ANY" => {
                if self.eat("DEFINED") {
                    self.expect("BY")?;
                    self.next()?;
                }
                Base::Any
            }
            "EXTERNAL" | "EMBEDDED" | "CHARACTER" => Base::Any,
            other if other.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) => Base::Ref(other.rsplit('.').next().unwrap_or(other).to_string()),
            other => return Err(aerr(format!("Unexpected '{}' in ASN.1 type", other))),
        };
        self.skip_constraints()?;
        let mut ty = Type { tags, base };
        if self.default == TagDefault::Automatic {
            apply_automatic(&mut ty);
        }
        Ok(ty)
    }
}

/// Automatic tagging: components of SEQUENCE / SET / CHOICE without any tag get `[0]`, `[1]`...
fn apply_automatic(t: &mut Type) {
    let comps = match &mut t.base {
        Base::Sequence(c, _) | Base::Set(c, _) | Base::Choice(c, _) => c,
        _ => return,
    };
    if comps.iter().any(|c| !c.ty.tags.is_empty()) {
        return;
    }
    for (i, c) in comps.iter_mut().filter(|c| c.name != "COMPONENTS OF").enumerate() {
        c.ty.tags.push(TagSpec { class: 2, num: i as u64, implicit: true });
    }
}

impl Asn1Module {
    /// Parse one or more ASN.1 modules.
    pub fn parse(src: &str) -> Result<Asn1Module> {
        let mut m = Asn1Module::default();
        let mut p = P { t: tokenize(src), i: 0, default: TagDefault::Explicit, extensible: false };
        let mut raw_values: Vec<(String, Type, Value)> = Vec::new();
        while p.peek().is_some() {
            // module header
            let name = p.next()?;
            if p.peek() == Some("{") {
                p.skip_group()?;
            }
            p.expect("DEFINITIONS")?;
            p.default = TagDefault::Explicit;
            p.extensible = false;
            loop {
                match p.next()?.as_str() {
                    "EXPLICIT" => p.default = TagDefault::Explicit,
                    "IMPLICIT" => p.default = TagDefault::Implicit,
                    "AUTOMATIC" => p.default = TagDefault::Automatic,
                    "TAGS" | "INSTRUCTIONS" | "XER" | "EXTENSIBILITY" => {}
                    "IMPLIED" => p.extensible = true,
                    "::=" => break,
                    other => return Err(aerr(format!("Unexpected '{}' in ASN.1 module header", other))),
                }
            }
            p.expect("BEGIN")?;
            m.module_names.push(name);
            loop {
                match p.peek() {
                    None => return Err(aerr("Missing END in ASN.1 module")),
                    Some("END") => {
                        p.i += 1;
                        break;
                    }
                    Some("EXPORTS") | Some("IMPORTS") => {
                        while p.next()? != ";" {}
                    }
                    Some(_) => {
                        let name = p.next()?;
                        if p.peek() == Some("{") {
                            // parameterized assignment: skip
                            p.skip_group()?;
                            p.expect("::=")?;
                            p.ty()?;
                            continue;
                        }
                        if p.eat("::=") {
                            let t = p.ty()?;
                            m.types.insert(name, t);
                        } else {
                            let t = p.ty()?;
                            p.expect("::=")?;
                            let v = p.value()?;
                            raw_values.push((name, t, v));
                        }
                    }
                }
            }
        }
        for (name, t, v) in raw_values {
            let v = m.resolve_value(&t, v, 0)?;
            m.values.insert(name, v);
        }
        // resolve identifiers in DEFAULT values
        let names: Vec<String> = m.types.keys().cloned().collect();
        for n in names {
            let mut t = m.types[&n].clone();
            m.resolve_defaults(&mut t, 0)?;
            m.types.insert(n, t);
        }
        Ok(m)
    }

    fn resolve_defaults(&self, t: &mut Type, depth: usize) -> Result<()> {
        if depth > 64 {
            return Ok(());
        }
        match &mut t.base {
            Base::Sequence(c, _) | Base::Set(c, _) | Base::Choice(c, _) => {
                for comp in c.iter_mut() {
                    if let Some(d) = comp.default.take() {
                        comp.default = Some(self.resolve_value(&comp.ty, d, 0)?);
                    }
                    self.resolve_defaults(&mut comp.ty, depth + 1)?;
                }
            }
            Base::SequenceOf(e) | Base::SetOf(e) => self.resolve_defaults(e, depth + 1)?,
            _ => {}
        }
        Ok(())
    }

    /// Resolves identifiers / OID component lists in a value for type `t`.
    fn resolve_value(&self, t: &Type, v: Value, depth: usize) -> Result<Value> {
        if depth > 32 {
            return Err(aerr("Recursive ASN.1 value"));
        }
        let (_, base) = self.resolve(t)?;
        Ok(match (base, v) {
            (Base::Oid | Base::RelativeOid, Value::Array(parts)) => {
                let mut out: Vec<String> = Vec::new();
                for p in parts {
                    let s = p.to_string();
                    if s.chars().all(|c| c.is_ascii_digit()) {
                        out.push(s);
                    } else if let Some(Value::String(prefix)) = self.values.get(&s) {
                        out.push(prefix.clone());
                    } else {
                        let well_known = match s.as_str() {
                            "itu-t" | "ccitt" => "0",
                            "iso" => "1",
                            "joint-iso-itu-t" | "joint-iso-ccitt" => "2",
                            "member-body" => "2",
                            "identified-organization" => "3",
                            _ => return Err(aerr(format!("Unknown OID component {}", s))),
                        };
                        out.push(well_known.into());
                    }
                }
                Value::String(out.join("."))
            }
            (base, Value::String(s)) if s.starts_with('\u{1}') => {
                let id = &s[1..];
                match base {
                    Base::Enumerated(items) => {
                        if !items.iter().any(|(n, _)| n == id) {
                            return Err(aerr(format!("Unknown ENUMERATED item {}", id)));
                        }
                        Value::String(id.to_string())
                    }
                    Base::Integer(named) if named.iter().any(|(n, _)| n == id) => Value::Int(named.iter().find(|(n, _)| n == id).unwrap().1),
                    _ => self.values.get(id).cloned().ok_or_else(|| aerr(format!("Unknown ASN.1 value {}", id)))?,
                }
            }
            (Base::BitString(named), Value::Array(parts)) if !named.is_empty() => Value::Array(parts.into_iter().map(|p| Value::String(p.to_string())).collect()),
            (_, v) => v,
        })
    }

    /// Follows references: the concatenated tags and the final base.
    pub fn resolve<'a>(&'a self, t: &'a Type) -> Result<(Vec<TagSpec>, &'a Base)> {
        let mut tags = t.tags.clone();
        let mut cur = t;
        let mut guard = 0;
        while let Base::Ref(n) = &cur.base {
            cur = self.types.get(n).ok_or_else(|| aerr(format!("Unknown ASN.1 type {}", n)))?;
            tags.extend(cur.tags.iter().cloned());
            guard += 1;
            if guard > 64 {
                return Err(aerr(format!("Circular ASN.1 type reference {}", n)));
            }
        }
        Ok((tags, &cur.base))
    }

    /// The TLV layers of a type (outermost first) after applying implicit tagging.
    fn layers(&self, t: &Type) -> Result<(Vec<(u8, u64)>, Base)> {
        let (mut tags, base) = self.resolve(t)?;
        if let Some(u) = universal_tag(base) {
            tags.push(TagSpec { class: 0, num: u, implicit: false });
        }
        let mut k = 0;
        while k < tags.len() {
            if tags[k].implicit && k + 1 < tags.len() {
                let removed = tags.remove(k + 1);
                tags[k].implicit = removed.implicit;
                continue;
            }
            k += 1;
        }
        Ok((tags.into_iter().map(|t| (t.class, t.num)).collect(), base.clone()))
    }

    /// The possible first (class, number) of an encoding; `None` = anything (ANY).
    fn first_tags(&self, t: &Type, depth: usize) -> Result<Option<Vec<(u8, u64)>>> {
        if depth > 32 {
            return Ok(None);
        }
        let (layers, base) = self.layers(t)?;
        if let Some(f) = layers.first() {
            return Ok(Some(vec![*f]));
        }
        match base {
            Base::Choice(alts, _) => {
                let mut out = Vec::new();
                for a in &alts {
                    match self.first_tags(&a.ty, depth + 1)? {
                        Some(v) => out.extend(v),
                        None => return Ok(None),
                    }
                }
                Ok(Some(out))
            }
            _ => Ok(None),
        }
    }

    fn matches(&self, t: &Type, class: u8, num: u64) -> Result<bool> {
        Ok(match self.first_tags(t, 0)? {
            None => true,
            Some(v) => v.contains(&(class, num)),
        })
    }

    /// Looks up a type by name (also `Module.Type` / `package.Type`).
    pub fn type_named(&self, name: &str) -> Result<Type> {
        let short = name.rsplit('.').next().unwrap_or(name);
        let short = short.rsplit('$').next().unwrap_or(short);
        if self.types.contains_key(short) {
            return Ok(Type { tags: vec![], base: Base::Ref(short.to_string()) });
        }
        // generated Java class names are capitalized type names with '-' removed
        for k in self.types.keys() {
            if k.replace('-', "").eq_ignore_ascii_case(short) {
                return Ok(Type { tags: vec![], base: Base::Ref(k.clone()) });
            }
        }
        Err(aerr(format!("Unknown ASN.1 type {}", name)))
    }

    // ------------------------------------------------------------------------- encode

    /// Encode a value of type `name` as DER.
    pub fn encode(&self, name: &str, v: &Value) -> Result<Vec<u8>> {
        let t = self.type_named(name)?;
        let mut out = Vec::new();
        self.enc(&t, v, &mut out, 0)?;
        Ok(out)
    }

    fn enc(&self, t: &Type, v: &Value, out: &mut Vec<u8>, depth: usize) -> Result<()> {
        if depth > 64 {
            return Err(aerr("ASN.1 value nested too deeply"));
        }
        let (layers, base) = self.layers(t)?;
        let (content, constructed) = self.enc_base(&base, v, depth)?;
        if layers.is_empty() {
            // CHOICE / ANY: the content is a complete TLV
            out.extend(content);
            return Ok(());
        }
        let mut cur = content;
        let n = layers.len();
        for (i, (class, num)) in layers.iter().enumerate().rev() {
            let mut w = Vec::new();
            let is_inner = i == n - 1;
            tlv(&mut w, *class, if is_inner { constructed } else { true }, *num, &cur);
            cur = w;
        }
        out.extend(cur);
        Ok(())
    }

    /// Content octets of a base and whether they are constructed (CHOICE / ANY give a TLV).
    fn enc_base(&self, base: &Base, v: &Value, depth: usize) -> Result<(Vec<u8>, bool)> {
        let bad = |what: &str| aerr(format!("Value {} does not match ASN.1 {}", v.to_json(), what));
        Ok(match base {
            Base::Boolean => (vec![if v.as_bool().ok_or_else(|| bad("BOOLEAN"))? { 0xff } else { 0 }], false),
            Base::Integer(named) => {
                let n: i128 = match v {
                    Value::Int(i) => *i as i128,
                    Value::String(s) => match named.iter().find(|(n, _)| n == s) {
                        Some((_, x)) => *x as i128,
                        None => s.parse().map_err(|_| bad("INTEGER"))?,
                    },
                    Value::Float(f) if f.fract() == 0.0 => *f as i128,
                    _ => return Err(bad("INTEGER")),
                };
                (encode_int(n), false)
            }
            Base::Enumerated(items) => {
                let n = match v {
                    Value::String(s) => items.iter().find(|(n, _)| n == s).map(|(_, x)| *x).ok_or_else(|| bad("ENUMERATED"))?,
                    Value::Int(i) => *i,
                    _ => return Err(bad("ENUMERATED")),
                };
                (encode_int(n as i128), false)
            }
            Base::Real => (encode_real(v.as_f64().ok_or_else(|| bad("REAL"))?), false),
            Base::Null => (Vec::new(), false),
            Base::OctetString => (
                match v {
                    Value::Bytes(b) => b.clone(),
                    other => other.to_string().into_bytes(),
                },
                false,
            ),
            Base::BitString(named) => {
                let mut c = Vec::new();
                match v {
                    Value::Array(names) if !named.is_empty() || names.iter().all(|n| n.as_i64().is_some()) => {
                        let mut bits: Vec<usize> = Vec::new();
                        for n in names {
                            let idx = match n {
                                Value::Int(i) => *i as usize,
                                other => named.iter().find(|(nn, _)| *nn == other.to_string()).map(|(_, x)| *x as usize).ok_or_else(|| bad("BIT STRING"))?,
                            };
                            bits.push(idx);
                        }
                        // DER: trailing zero bits removed
                        let nbits = bits.iter().max().map(|m| m + 1).unwrap_or(0);
                        let nbytes = nbits.div_ceil(8);
                        let mut data = vec![0u8; nbytes];
                        for b in bits {
                            data[b / 8] |= 0x80 >> (b % 8);
                        }
                        c.push(((8 - nbits % 8) % 8) as u8);
                        c.extend(data);
                    }
                    Value::Bytes(b) => {
                        c.push(0);
                        c.extend(b);
                    }
                    _ => return Err(bad("BIT STRING")),
                }
                (c, false)
            }
            Base::Oid => (encode_oid(&v.to_string())?, false),
            Base::RelativeOid => {
                let mut c = Vec::new();
                for part in v.to_string().split('.') {
                    let mut n: u128 = part.parse().map_err(|_| bad("RELATIVE-OID"))?;
                    let mut g = vec![(n & 0x7f) as u8];
                    n >>= 7;
                    while n > 0 {
                        g.push((n & 0x7f) as u8 | 0x80);
                        n >>= 7;
                    }
                    g.reverse();
                    c.extend(g);
                }
                (c, false)
            }
            Base::Str(u) => {
                let s = match v {
                    Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                    other => other.to_string(),
                };
                let c = match u {
                    30 => s.encode_utf16().flat_map(|x| x.to_be_bytes()).collect(),
                    28 => s.chars().flat_map(|x| (x as u32).to_be_bytes()).collect(),
                    _ => s.into_bytes(),
                };
                (c, false)
            }
            Base::Sequence(comps, _) | Base::Set(comps, _) => {
                let o = v.as_object().ok_or_else(|| bad("SEQUENCE"))?;
                let mut parts: Vec<Vec<u8>> = Vec::new();
                self.enc_components(comps, o, &mut parts, depth)?;
                if matches!(base, Base::Set(..)) {
                    parts.sort_by_key(|p| tag_sort_key(p));
                }
                (parts.concat(), true)
            }
            Base::SequenceOf(e) | Base::SetOf(e) => {
                let a = v.as_array().ok_or_else(|| bad("SEQUENCE OF"))?;
                let mut parts = Vec::new();
                for x in a {
                    let mut b = Vec::new();
                    self.enc(e, x, &mut b, depth + 1)?;
                    parts.push(b);
                }
                if matches!(base, Base::SetOf(_)) {
                    parts.sort();
                }
                (parts.concat(), true)
            }
            Base::Choice(alts, _) => {
                let o = v.as_object().filter(|o| o.len() == 1).ok_or_else(|| bad("CHOICE (an object with one alternative)"))?;
                let (k, x) = o.iter().next().unwrap();
                let a = alts.iter().find(|a| a.name == *k).ok_or_else(|| aerr(format!("Unknown CHOICE alternative {}", k)))?;
                let mut b = Vec::new();
                self.enc(&a.ty, x, &mut b, depth + 1)?;
                (b, true)
            }
            Base::Any => (
                match v {
                    Value::Bytes(b) => b.clone(),
                    other => crate::asn1::der_encode(other)?,
                },
                true,
            ),
            Base::Ref(_) => unreachable!("resolved"),
        })
    }

    fn enc_components(&self, comps: &[Component], o: &ObjectMap, parts: &mut Vec<Vec<u8>>, depth: usize) -> Result<()> {
        for c in comps {
            if c.name == "COMPONENTS OF" {
                let (_, b) = self.resolve(&c.ty)?;
                if let Base::Sequence(inner, _) | Base::Set(inner, _) = b {
                    self.enc_components(inner, o, parts, depth)?;
                }
                continue;
            }
            match o.get(&c.name).filter(|v| !v.is_null() || matches!(self.resolve(&c.ty).map(|r| r.1), Ok(Base::Null))) {
                None => {
                    if !c.optional && c.default.is_none() {
                        return Err(aerr(format!("Missing mandatory ASN.1 component {}", c.name)));
                    }
                }
                Some(x) => {
                    if c.default.as_ref() == Some(x) {
                        continue; // DER: defaults are not encoded
                    }
                    let mut b = Vec::new();
                    self.enc(&c.ty, x, &mut b, depth + 1)?;
                    parts.push(b);
                }
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------------- decode

    /// Decode one BER/DER value of type `name` (trailing data is an error).
    pub fn decode(&self, name: &str, d: &[u8]) -> Result<Value> {
        let t = self.type_named(name)?;
        let (v, n) = self.dec(&t, d, 0)?;
        if n != d.len() {
            return Err(aerr("Trailing data after ASN.1 value"));
        }
        Ok(v)
    }

    /// Decode a value of type `name` at the start of `d`, returning the bytes consumed.
    pub fn decode_prefix(&self, name: &str, d: &[u8]) -> Result<(Value, usize)> {
        let t = self.type_named(name)?;
        self.dec(&t, d, 0)
    }

    fn dec(&self, t: &Type, d: &[u8], depth: usize) -> Result<(Value, usize)> {
        if depth > 64 {
            return Err(aerr("ASN.1 data nested too deeply"));
        }
        let (layers, base) = self.layers(t)?;
        if layers.is_empty() {
            return self.dec_open(&base, d, depth);
        }
        let first = read_tlv(d, depth)?;
        let total = first.total;
        let mut cur = first;
        for (i, (class, num)) in layers.iter().enumerate() {
            if cur.class != *class || cur.tag != *num {
                return Err(aerr(format!("Unexpected ASN.1 tag {} (expected {})", tag_text(cur.class, cur.tag), tag_text(*class, *num))));
            }
            if i + 1 < layers.len() {
                cur = read_tlv(cur.content, depth + 1)?;
            }
        }
        let v = self.dec_content(&base, &cur, depth)?;
        Ok((v, total))
    }

    /// CHOICE / ANY (no own tag): the element itself.
    fn dec_open(&self, base: &Base, d: &[u8], depth: usize) -> Result<(Value, usize)> {
        match base {
            Base::Choice(alts, _) => {
                let t = read_tlv(d, depth)?;
                for a in alts {
                    if self.matches(&a.ty, t.class, t.tag)? {
                        let (v, n) = self.dec(&a.ty, d, depth + 1)?;
                        let mut o = ObjectMap::new();
                        o.insert(a.name.clone(), v);
                        return Ok((Value::Object(o), n));
                    }
                }
                Err(aerr(format!("No CHOICE alternative for tag {}", tag_text(t.class, t.tag))))
            }
            _ => {
                let t = read_tlv(d, depth)?;
                Ok((crate::asn1::ber_decode(&d[..t.total])?, t.total))
            }
        }
    }

    fn primitive_bytes(&self, t: &Tlv<'_>, depth: usize) -> Result<Vec<u8>> {
        if !t.constructed {
            return Ok(t.content.to_vec());
        }
        // BER constructed string: concatenate the segments
        let mut out = Vec::new();
        let mut i = 0;
        while i < t.content.len() {
            let s = read_tlv(&t.content[i..], depth + 1)?;
            out.extend(self.primitive_bytes(&s, depth + 1)?);
            i += s.total;
        }
        Ok(out)
    }

    fn dec_content(&self, base: &Base, t: &Tlv<'_>, depth: usize) -> Result<Value> {
        Ok(match base {
            Base::Boolean => Value::Bool(t.content.first().map(|b| *b != 0).unwrap_or(false)),
            Base::Integer(_) => decode_int(t.content),
            Base::Enumerated(items) => {
                let n = decode_int(t.content).as_i64().unwrap_or(i64::MIN);
                items.iter().find(|(_, x)| *x == n).map(|(name, _)| Value::String(name.clone())).unwrap_or(Value::Int(n))
            }
            Base::Real => Value::Float(decode_real(t.content)?),
            Base::Null => Value::Null,
            Base::OctetString => Value::Bytes(self.primitive_bytes(t, depth)?),
            Base::BitString(named) => {
                let b = self.primitive_bytes(t, depth)?;
                let unused = *b.first().unwrap_or(&0) as usize;
                let data = b.get(1..).unwrap_or(&[]);
                if named.is_empty() {
                    Value::Bytes(data.to_vec())
                } else {
                    let nbits = (data.len() * 8).saturating_sub(unused);
                    let mut out = Vec::new();
                    for i in 0..nbits {
                        if data[i / 8] & (0x80 >> (i % 8)) != 0 {
                            out.push(named.iter().find(|(_, x)| *x as usize == i).map(|(n, _)| Value::String(n.clone())).unwrap_or(Value::Int(i as i64)));
                        }
                    }
                    Value::Array(out)
                }
            }
            Base::Oid => Value::String(decode_oid(t.content)),
            Base::RelativeOid => {
                let mut parts = Vec::new();
                let mut v: u128 = 0;
                for b in t.content {
                    v = (v << 7) | (b & 0x7f) as u128;
                    if b & 0x80 == 0 {
                        parts.push(v.to_string());
                        v = 0;
                    }
                }
                Value::String(parts.join("."))
            }
            Base::Str(u) => {
                let b = self.primitive_bytes(t, depth)?;
                Value::String(match u {
                    30 => String::from_utf16_lossy(&b.chunks(2).filter(|c| c.len() == 2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>()),
                    28 => b.chunks(4).filter(|c| c.len() == 4).filter_map(|c| char::from_u32(u32::from_be_bytes([c[0], c[1], c[2], c[3]]))).collect(),
                    20 | 25 | 27 | 7 => b.iter().map(|x| *x as char).collect(),
                    _ => String::from_utf8_lossy(&b).into_owned(),
                })
            }
            Base::Sequence(comps, ext) => {
                let mut o = ObjectMap::new();
                let mut pos = 0;
                self.dec_sequence(comps, t.content, &mut pos, &mut o, depth)?;
                if pos < t.content.len() && !*ext {
                    return Err(aerr("Unexpected trailing components in ASN.1 SEQUENCE"));
                }
                Value::Object(o)
            }
            Base::Set(comps, ext) => {
                let mut flat = Vec::new();
                self.flatten(comps, &mut flat)?;
                let mut o = ObjectMap::new();
                let mut i = 0;
                while i < t.content.len() {
                    let e = read_tlv(&t.content[i..], depth + 1)?;
                    let mut found = false;
                    for c in &flat {
                        if !o.contains_key(&c.name) && self.matches(&c.ty, e.class, e.tag)? {
                            let (v, _) = self.dec(&c.ty, &t.content[i..i + e.total], depth + 1)?;
                            o.insert(c.name.clone(), v);
                            found = true;
                            break;
                        }
                    }
                    if !found && !*ext {
                        return Err(aerr(format!("Unexpected ASN.1 SET member {}", tag_text(e.class, e.tag))));
                    }
                    i += e.total;
                }
                for c in &flat {
                    if !o.contains_key(&c.name) {
                        match &c.default {
                            Some(d) => {
                                o.insert(c.name.clone(), d.clone());
                            }
                            None if !c.optional => return Err(aerr(format!("Missing mandatory ASN.1 component {}", c.name))),
                            None => {}
                        }
                    }
                }
                Value::Object(o)
            }
            Base::SequenceOf(e) | Base::SetOf(e) => {
                let mut out = Vec::new();
                let mut i = 0;
                while i < t.content.len() {
                    let (v, n) = self.dec(e, &t.content[i..], depth + 1)?;
                    out.push(v);
                    i += n;
                }
                Value::Array(out)
            }
            Base::Choice(..) | Base::Any => {
                // explicitly tagged CHOICE / ANY: the content is the element
                let (v, _) = self.dec_open(base, t.content, depth + 1)?;
                v
            }
            Base::Ref(_) => unreachable!("resolved"),
        })
    }

    fn flatten(&self, comps: &[Component], out: &mut Vec<Component>) -> Result<()> {
        for c in comps {
            if c.name == "COMPONENTS OF" {
                let (_, b) = self.resolve(&c.ty)?;
                if let Base::Sequence(inner, _) | Base::Set(inner, _) = b {
                    let inner = inner.clone();
                    self.flatten(&inner, out)?;
                }
            } else {
                out.push(c.clone());
            }
        }
        Ok(())
    }

    fn dec_sequence(&self, comps: &[Component], d: &[u8], pos: &mut usize, o: &mut ObjectMap, depth: usize) -> Result<()> {
        let mut flat = Vec::new();
        self.flatten(comps, &mut flat)?;
        for c in &flat {
            let present = if *pos < d.len() {
                let e = read_tlv(&d[*pos..], depth + 1)?;
                self.matches(&c.ty, e.class, e.tag)?
            } else {
                false
            };
            if present {
                let (v, n) = self.dec(&c.ty, &d[*pos..], depth + 1)?;
                o.insert(c.name.clone(), v);
                *pos += n;
            } else if let Some(def) = &c.default {
                o.insert(c.name.clone(), def.clone());
            } else if !c.optional {
                return Err(aerr(format!("Missing mandatory ASN.1 component {}", c.name)));
            }
        }
        Ok(())
    }
}

fn universal_tag(b: &Base) -> Option<u64> {
    Some(match b {
        Base::Boolean => 1,
        Base::Integer(_) => 2,
        Base::BitString(_) => 3,
        Base::OctetString => 4,
        Base::Null => 5,
        Base::Oid => 6,
        Base::Real => 9,
        Base::Enumerated(_) => 10,
        Base::RelativeOid => 13,
        Base::Sequence(..) | Base::SequenceOf(_) => 16,
        Base::Set(..) | Base::SetOf(_) => 17,
        Base::Str(u) => *u,
        Base::Choice(..) | Base::Any | Base::Ref(_) => return None,
    })
}

fn tag_text(class: u8, num: u64) -> String {
    match class {
        0 => format!("[UNIVERSAL {}]", num),
        1 => format!("[APPLICATION {}]", num),
        2 => format!("[{}]", num),
        _ => format!("[PRIVATE {}]", num),
    }
}

/// DER SET ordering key: class then tag number.
fn tag_sort_key(enc: &[u8]) -> (u8, u64) {
    match read_tlv(enc, 0) {
        Ok(t) => (t.class, t.tag),
        Err(_) => (255, u64::MAX),
    }
}

/// REAL: binary encoding (base 2) for finite values, special values per X.690 8.5.9.
pub fn encode_real(f: f64) -> Vec<u8> {
    if f == 0.0 {
        return if f.is_sign_negative() { vec![0x43] } else { vec![] };
    }
    if f.is_nan() {
        return vec![0x42];
    }
    if f.is_infinite() {
        return vec![if f > 0.0 { 0x40 } else { 0x41 }];
    }
    let bits = f.to_bits();
    let neg = bits >> 63 == 1;
    let exp_bits = ((bits >> 52) & 0x7ff) as i64;
    let frac = bits & ((1u64 << 52) - 1);
    let (mut m, mut e) = if exp_bits == 0 { (frac, -1074i64) } else { (frac | (1u64 << 52), exp_bits - 1075) };
    // DER: mantissa odd (or zero)
    while m & 1 == 0 {
        m >>= 1;
        e += 1;
    }
    let eb = encode_int(e as i128);
    let mut out = Vec::new();
    let mut first = 0x80u8 | if neg { 0x40 } else { 0 };
    match eb.len() {
        1 => {}
        2 => first |= 1,
        3 => first |= 2,
        _ => first |= 3,
    }
    out.push(first);
    if eb.len() > 3 {
        out.push(eb.len() as u8);
    }
    out.extend(&eb);
    let mb: Vec<u8> = m.to_be_bytes().iter().copied().skip_while(|b| *b == 0).collect();
    out.extend(mb);
    out
}

/// REAL decoding (binary, decimal NR1-3 and special values).
pub fn decode_real(c: &[u8]) -> Result<f64> {
    if c.is_empty() {
        return Ok(0.0);
    }
    let first = c[0];
    if first & 0x80 != 0 {
        let neg = first & 0x40 != 0;
        let base: f64 = match (first >> 4) & 3 {
            0 => 2.0,
            1 => 8.0,
            2 => 16.0,
            _ => return Err(aerr("Invalid REAL base")),
        };
        let scale = ((first >> 2) & 3) as i32;
        let (elen, start) = match first & 3 {
            0 => (1, 1),
            1 => (2, 1),
            2 => (3, 1),
            _ => (*c.get(1).ok_or_else(|| aerr("Truncated REAL"))? as usize, 2),
        };
        let eb = c.get(start..start + elen).ok_or_else(|| aerr("Truncated REAL"))?;
        let e = decode_int(eb).as_i64().ok_or_else(|| aerr("REAL exponent too large"))?;
        let mut m = 0f64;
        for b in &c[start + elen..] {
            m = m * 256.0 + *b as f64;
        }
        let v = m * 2f64.powi(scale) * base.powf(e as f64);
        return Ok(if neg { -v } else { v });
    }
    if first & 0x40 != 0 {
        return Ok(match first {
            0x40 => f64::INFINITY,
            0x41 => f64::NEG_INFINITY,
            0x42 => f64::NAN,
            0x43 => -0.0,
            _ => return Err(aerr("Invalid REAL special value")),
        });
    }
    let s = String::from_utf8_lossy(&c[1..]).trim().replace(',', ".");
    s.parse::<f64>().map_err(|_| aerr(format!("Invalid decimal REAL {}", s)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_core::json;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{:02x}", x)).collect()
    }

    // X.690 Annex A example: PersonnelRecord
    const PERSONNEL: &str = r#"
        PersonnelRecordModule DEFINITIONS ::= BEGIN
        PersonnelRecord ::= [APPLICATION 0] IMPLICIT SET {
            name Name,
            title [0] VisibleString,
            number EmployeeNumber,
            dateOfHire [1] Date,
            nameOfSpouse [2] Name,
            children [3] IMPLICIT SEQUENCE OF ChildInformation DEFAULT {} }
        ChildInformation ::= SET { name Name, dateOfBirth [0] Date }
        Name ::= [APPLICATION 1] IMPLICIT SEQUENCE {
            givenName VisibleString,
            initial VisibleString,
            familyName VisibleString }
        EmployeeNumber ::= [APPLICATION 2] IMPLICIT INTEGER
        Date ::= [APPLICATION 3] IMPLICIT VisibleString -- YYYYMMDD
        END
    "#;

    #[test]
    fn x690_annex_a_personnel_record() {
        let m = Asn1Module::parse(PERSONNEL).unwrap();
        let v = json::parse(
            r#"{"name":{"givenName":"John","initial":"P","familyName":"Smith"},"title":"Director","number":51,
            "dateOfHire":"19710917","nameOfSpouse":{"givenName":"Mary","initial":"T","familyName":"Smith"},
            "children":[{"name":{"givenName":"Ralph","initial":"T","familyName":"Smith"},"dateOfBirth":"19571111"},
                        {"name":{"givenName":"Susan","initial":"B","familyName":"Jones"},"dateOfBirth":"19590717"}]}"#,
        )
        .unwrap();
        let d = m.encode("PersonnelRecord", &v).unwrap();
        // X.690 Annex A.3 (DER: SET members ordered by tag: [APPLICATION 1] name,
        // [APPLICATION 2] number, [0] title, [1] dateOfHire, [2] spouse, [3] children)
        let expect = "60818561101a044a6f686e1a01501a05536d697468420133a00a1a084469726563746f72a10a43083139373130393137a21261101a044d6172791a01541a05536d697468a342311f61111a0552616c70681a01541a05536d697468a00a43083139353731313131311f61111a05537573616e1a01421a054a6f6e6573a00a43083139353930373137";
        assert_eq!(hex(&d), expect);
        let back = m.decode("PersonnelRecord", &d).unwrap();
        for k in ["name", "title", "number", "dateOfHire", "nameOfSpouse", "children"] {
            assert_eq!(back.get(k), v.get(k), "{}", k);
        }
    }

    #[test]
    fn automatic_tags_choice_optional_default_enum_bits() {
        let src = r#"
            Demo { iso(1) org(3) 42 } DEFINITIONS AUTOMATIC TAGS ::= BEGIN
            IMPORTS Foo FROM Other;
            maxItems INTEGER ::= 3
            demoOid OBJECT IDENTIFIER ::= { iso identified-organization 42 1 }
            Color ::= ENUMERATED { red(0), green(1), blue(2), ... }
            Flags ::= BIT STRING { read(0), write(1), exec(2) }
            Shape ::= CHOICE { circle INTEGER, square SEQUENCE { side INTEGER }, label UTF8String }
            Msg ::= SEQUENCE {
                id INTEGER (0..65535),
                color Color DEFAULT green,
                flags Flags OPTIONAL,
                shape Shape,
                tags SEQUENCE (SIZE(0..maxItems)) OF UTF8String,
                kind OBJECT IDENTIFIER OPTIONAL,
                ratio REAL OPTIONAL,
                ...,
                extra BOOLEAN OPTIONAL
            }
            END
        "#;
        let m = Asn1Module::parse(src).unwrap();
        assert_eq!(m.values["maxItems"], Value::Int(3));
        assert_eq!(m.values["demoOid"], Value::from("1.3.42.1"));
        let v = json::parse(r#"{"id":7,"color":"blue","flags":["read","exec"],"shape":{"square":{"side":4}},"tags":["a","b"],"kind":"1.3.42.1","ratio":0.75}"#).unwrap();
        let d = m.encode("Msg", &v).unwrap();
        // [0] id, [1] color=2, [2] flags (5 unused bits, 0xa0), [3] EXPLICIT choice -> [1] square {[0] 4}
        assert!(hex(&d).starts_with("30"), "{}", hex(&d));
        assert!(hex(&d).contains("800107810102820205a0a305a1038001"), "{}", hex(&d));
        let back = m.decode("Msg", &d).unwrap();
        assert_eq!(back, v);
        // default omitted in DER and filled on decode
        let v2 = json::parse(r#"{"id":1,"color":"green","shape":{"label":"x"},"tags":[]}"#).unwrap();
        let d2 = m.encode("Msg", &v2).unwrap();
        assert!(!hex(&d2).contains("8101"));
        assert_eq!(m.decode("Msg", &d2).unwrap(), v2);
        // missing mandatory component
        assert!(m.encode("Msg", &json::parse(r#"{"id":1}"#).unwrap()).is_err());
        // unknown extension additions are skipped
        let mut ext = d2.clone();
        ext.extend([0x9f, 0x30, 0x01, 0x00]);
        ext[1] += 4;
        assert!(m.decode("Msg", &ext).is_ok());
    }

    #[test]
    fn reals() {
        for f in [0.0, 1.0, -1.0, 0.5, 3.14159, 1e300, -2.5e-300, 123456789.0] {
            assert_eq!(decode_real(&encode_real(f)).unwrap(), f);
        }
        assert_eq!(hex(&encode_real(1.0)), "800001");
        assert_eq!(decode_real(b"\x03123.5E0").unwrap(), 123.5);
        assert!(decode_real(&encode_real(f64::NAN)).unwrap().is_nan());
    }
}
