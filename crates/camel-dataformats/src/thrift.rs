//! `thrift` data format: Apache Thrift binary and compact protocols driven by a runtime IDL
//! (instead of generated classes).
//!
//! The minimal IDL parser understands `struct`, `union`, `exception` (fields `1: required
//! i32 id = 0,` with base types `bool byte i8 i16 i32 i64 double string binary uuid`,
//! `list<T>`, `set<T>`, `map<K,V>`, struct/enum references, default values), `enum`,
//! `typedef`, `const` (evaluated: numbers, strings, booleans, lists, sets, maps, struct
//! literals, references to other constants and `Enum.VALUE`; see [`Idl::consts`]),
//! `service` (methods with arguments, return types, `throws`, `oneway`, `extends`; see
//! [`Idl::services`], used by the `thrift` component in [`crate::thrift_rpc`]), and ignores
//! `namespace`, `include`, annotations and comments. Missing fields with a default value get
//! it on marshal and unmarshal.
//!
//! Options: `instanceClass` (the struct name, optionally qualified), `schema` (inline IDL)
//! or `idlFile`, `contentTypeFormat`: `binary` (TBinaryProtocol, default), `compact`
//! (TCompactProtocol), `json` (TJSONProtocol: `{"1":{"i32":5},...}`) or `sjson`
//! (TSimpleJSONProtocol-style plain JSON of the value; unmarshal parses plain JSON),
//! `contentTypeHeader` (default true). Values are objects keyed by field name; enums are
//! names; binary fields bytes; sets arrays.

use std::collections::HashMap;
use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::props::CONTENT_TYPE;
use camel_core::message::Exchange;
use camel_core::value::ObjectMap;
use camel_core::Value;

use crate::{body_bytes, body_text, df_err, Opts};

fn terr(msg: impl Into<String>) -> CamelError {
    CamelError::new("TException", msg)
}

/// A Thrift type.
#[derive(Clone, Debug, PartialEq)]
pub enum TType {
    Bool,
    Byte,
    I16,
    I32,
    I64,
    Double,
    String,
    Binary,
    Uuid,
    List(Box<TType>),
    Set(Box<TType>),
    Map(Box<TType>, Box<TType>),
    Struct(String),
    Enum(String),
}

#[derive(Clone, Debug)]
pub struct TField {
    pub id: i16,
    pub name: String,
    pub ty: TType,
    pub required: bool,
    /// Default value from the IDL (`= ...`).
    pub default: Option<Value>,
}

/// A service method.
#[derive(Clone, Debug)]
pub struct TMethod {
    pub name: String,
    /// `None` for `void`.
    pub ret: Option<TType>,
    pub args: Vec<TField>,
    pub throws: Vec<TField>,
    pub oneway: bool,
}

/// A service.
#[derive(Clone, Debug, Default)]
pub struct TService {
    pub name: String,
    pub extends: Option<String>,
    pub methods: Vec<TMethod>,
}

/// A parsed IDL.
#[derive(Clone, Debug, Default)]
pub struct Idl {
    pub structs: HashMap<String, Vec<TField>>,
    pub enums: HashMap<String, Vec<(String, i32)>>,
    /// Evaluated constants.
    pub consts: HashMap<String, Value>,
    pub services: HashMap<String, TService>,
}

fn tokenize(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < c.len() {
        let ch = c[i];
        if ch.is_whitespace() {
            i += 1;
        } else if ch == '#' || (ch == '/' && c.get(i + 1) == Some(&'/')) {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
        } else if ch == '/' && c.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < c.len() && !(c[i] == '*' && c[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else if ch == '"' || ch == '\'' {
            let q = ch;
            let s = i;
            i += 1;
            while i < c.len() && c[i] != q {
                i += 1;
            }
            i += 1;
            out.push(c[s..i.min(c.len())].iter().collect());
        } else if ch.is_alphanumeric() || ch == '_' || ch == '.' || ch == '-' {
            let s = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_' || c[i] == '.' || (c[i] == '-' && i == s)) {
                i += 1;
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
    typedefs: HashMap<String, TType>,
}

impl P {
    fn peek(&self) -> Option<&str> {
        self.t.get(self.i).map(|s| s.as_str())
    }
    fn next(&mut self) -> Result<String> {
        let s = self.t.get(self.i).cloned().ok_or_else(|| terr("Unexpected end of IDL"))?;
        self.i += 1;
        Ok(s)
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        let n = self.next()?;
        if n != s {
            return Err(terr(format!("Expected '{}' but found '{}' in IDL", s, n)));
        }
        Ok(())
    }
    fn skip_annotations(&mut self) {
        if self.peek() == Some("(") {
            let mut depth = 0;
            while let Some(t) = self.t.get(self.i) {
                self.i += 1;
                if t == "(" {
                    depth += 1;
                } else if t == ")" {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
            }
        }
    }
    /// Parses a constant value for type `t` (struct literals need `structs`).
    fn const_value(&mut self, t: &TType, idl: &Idl) -> Result<Value> {
        let tok = self.next()?;
        let bad = |tok: &str| terr(format!("Invalid constant {} for type {:?}", tok, t));
        Ok(match tok.as_str() {
            "[" | "{" => {
                let close = if tok == "[" { "]" } else { "}" };
                let mut items: Vec<(Value, Value)> = Vec::new();
                let (kt, vt) = match t {
                    TType::List(e) | TType::Set(e) => (TType::String, (**e).clone()),
                    TType::Map(k, v) => ((**k).clone(), (**v).clone()),
                    other => (TType::String, other.clone()),
                };
                while self.peek() != Some(close) {
                    if matches!(t, TType::Map(..) | TType::Struct(_)) {
                        let k = self.const_value(&kt, idl)?;
                        self.expect(":")?;
                        let vt = match t {
                            TType::Struct(sn) => idl.structs.get(sn).and_then(|fs| fs.iter().find(|f| f.name == k.to_string())).map(|f| f.ty.clone()).unwrap_or(TType::String),
                            _ => vt.clone(),
                        };
                        items.push((k, self.const_value(&vt, idl)?));
                    } else {
                        items.push((Value::Null, self.const_value(&vt, idl)?));
                    }
                    if matches!(self.peek(), Some(",") | Some(";")) {
                        self.i += 1;
                    }
                }
                self.i += 1;
                match t {
                    TType::Map(..) | TType::Struct(_) => Value::Object(items.into_iter().map(|(k, v)| (k.to_string(), v)).collect()),
                    _ => Value::Array(items.into_iter().map(|(_, v)| v).collect()),
                }
            }
            s if s.starts_with('"') || s.starts_with('\'') => Value::String(s[1..s.len().saturating_sub(1)].to_string()),
            "true" => Value::Bool(true),
            "false" => Value::Bool(false),
            num if num.starts_with(|c: char| c.is_ascii_digit() || c == '-' || c == '+') => {
                let n = num.trim_start_matches('+');
                let (neg, digits) = match n.strip_prefix('-') {
                    Some(d) => (true, d),
                    None => (false, n),
                };
                let int = if let Some(h) = digits.strip_prefix("0x").or_else(|| digits.strip_prefix("0X")) { i64::from_str_radix(h, 16).ok() } else { digits.parse::<i64>().ok() };
                match (int, t) {
                    (Some(i), TType::Double) => Value::Float(if neg { -(i as f64) } else { i as f64 }),
                    (Some(i), TType::Bool) => Value::Bool(i != 0),
                    (Some(i), TType::Enum(e)) | (Some(i), TType::Struct(e)) if idl.enums.contains_key(e) => {
                        let i = if neg { -i } else { i };
                        idl.enums[e].iter().find(|(_, v)| *v as i64 == i).map(|(n, _)| Value::String(n.clone())).unwrap_or(Value::Int(i))
                    }
                    (Some(i), _) => Value::Int(if neg { -i } else { i }),
                    (None, _) => Value::Float(n.parse::<f64>().map_err(|_| bad(num))?),
                }
            }
            ident => {
                // Enum.VALUE, an enum value of the expected type, or another constant
                let short = ident.rsplit('.').next().unwrap_or(ident);
                let qual = ident.rsplit_once('.').map(|(q, _)| q.rsplit('.').next().unwrap_or(q));
                if let Some(v) = idl.consts.get(ident).or_else(|| idl.consts.get(short)) {
                    return Ok(v.clone());
                }
                let enum_name = qual.filter(|q| idl.enums.contains_key(*q)).map(String::from).or_else(|| match t {
                    TType::Enum(e) | TType::Struct(e) if idl.enums.contains_key(e) => Some(e.clone()),
                    _ => None,
                });
                match enum_name.and_then(|e| idl.enums.get(&e).and_then(|vs| vs.iter().find(|(n, _)| n == short)).cloned()) {
                    Some((n, v)) => match t {
                        TType::Byte | TType::I16 | TType::I32 | TType::I64 => Value::Int(v as i64),
                        _ => Value::String(n),
                    },
                    None => return Err(terr(format!("Unknown constant {}", ident))),
                }
            }
        })
    }

    fn ty(&mut self) -> Result<TType> {
        let n = self.next()?;
        let t = match n.as_str() {
            "bool" => TType::Bool,
            "byte" | "i8" => TType::Byte,
            "i16" => TType::I16,
            "i32" => TType::I32,
            "i64" => TType::I64,
            "double" => TType::Double,
            "string" => TType::String,
            "binary" => TType::Binary,
            "uuid" => TType::Uuid,
            "list" | "set" => {
                self.expect("<")?;
                let e = self.ty()?;
                self.expect(">")?;
                if n == "list" {
                    TType::List(Box::new(e))
                } else {
                    TType::Set(Box::new(e))
                }
            }
            "map" => {
                self.expect("<")?;
                let k = self.ty()?;
                self.expect(",")?;
                let v = self.ty()?;
                self.expect(">")?;
                TType::Map(Box::new(k), Box::new(v))
            }
            other => {
                let short = other.rsplit('.').next().unwrap_or(other).to_string();
                match self.typedefs.get(&short) {
                    Some(t) => t.clone(),
                    None => TType::Struct(short),
                }
            }
        };
        self.skip_annotations();
        Ok(t)
    }
}

/// Parses `{ 1: required T name = default, ... }` (or `( ... )` argument lists).
fn parse_field_list(p: &mut P, open: &str, close: &str, idl: &Idl) -> Result<Vec<TField>> {
    p.expect(open)?;
    let mut fields = Vec::new();
    let mut auto_id = -1i16;
    while p.peek() != Some(close) {
        let mut id = None;
        if p.t.get(p.i + 1).map(|s| s == ":").unwrap_or(false) {
            id = Some(p.next()?.parse::<i16>().map_err(|_| terr("Invalid field id"))?);
            p.i += 1;
        }
        let mut required = false;
        if matches!(p.peek(), Some("required") | Some("optional")) {
            required = p.next()? == "required";
        }
        let mut ty = p.ty()?;
        if let TType::Struct(n) = &ty {
            if idl.enums.contains_key(n) {
                ty = TType::Enum(n.clone());
            }
        }
        let fname = p.next()?;
        let mut default = None;
        if p.peek() == Some("=") {
            p.i += 1;
            default = Some(p.const_value(&ty, idl)?);
        }
        p.skip_annotations();
        if matches!(p.peek(), Some(",") | Some(";")) {
            p.i += 1;
        }
        let id = id.unwrap_or_else(|| {
            auto_id -= 1;
            auto_id + 1
        });
        fields.push(TField { id, name: fname, ty, required, default });
    }
    p.i += 1;
    Ok(fields)
}

impl Idl {
    /// Parse IDL text.
    pub fn parse(src: &str) -> Result<Idl> {
        let mut p = P { t: tokenize(src), i: 0, typedefs: HashMap::new() };
        let mut idl = Idl::default();
        while let Some(tok) = p.peek().map(String::from) {
            match tok.as_str() {
                "namespace" | "include" | "cpp_include" => {
                    p.i += 3.min(p.t.len() - p.i);
                    if tok == "include" || tok == "cpp_include" {
                        p.i -= 1;
                    }
                }
                "typedef" => {
                    p.i += 1;
                    let t = p.ty()?;
                    let name = p.next()?;
                    p.typedefs.insert(name, t);
                    if matches!(p.peek(), Some(";") | Some(",")) {
                        p.i += 1;
                    }
                }
                "const" => {
                    p.i += 1;
                    let mut t = p.ty()?;
                    if let TType::Struct(n) = &t {
                        if idl.enums.contains_key(n) {
                            t = TType::Enum(n.clone());
                        }
                    }
                    let name = p.next()?;
                    p.expect("=")?;
                    let v = p.const_value(&t, &idl)?;
                    idl.consts.insert(name, v);
                    if matches!(p.peek(), Some(";") | Some(",")) {
                        p.i += 1;
                    }
                }
                "service" => {
                    p.i += 1;
                    let name = p.next()?;
                    let mut svc = TService { name: name.clone(), ..Default::default() };
                    if p.peek() == Some("extends") {
                        p.i += 1;
                        let base = p.next()?;
                        svc.extends = Some(base.rsplit('.').next().unwrap_or(&base).to_string());
                    }
                    p.skip_annotations();
                    p.expect("{")?;
                    while p.peek() != Some("}") {
                        let mut oneway = false;
                        if p.peek() == Some("oneway") {
                            p.i += 1;
                            oneway = true;
                        }
                        let ret = if p.peek() == Some("void") {
                            p.i += 1;
                            None
                        } else {
                            Some(p.ty()?)
                        };
                        let mname = p.next()?;
                        let args = parse_field_list(&mut p, "(", ")", &idl)?;
                        let throws = if p.peek() == Some("throws") {
                            p.i += 1;
                            parse_field_list(&mut p, "(", ")", &idl)?
                        } else {
                            Vec::new()
                        };
                        p.skip_annotations();
                        if matches!(p.peek(), Some(",") | Some(";")) {
                            p.i += 1;
                        }
                        svc.methods.push(TMethod { name: mname, ret, args, throws, oneway });
                    }
                    p.i += 1;
                    p.skip_annotations();
                    idl.services.insert(name, svc);
                }
                "enum" => {
                    p.i += 1;
                    let name = p.next()?;
                    p.expect("{")?;
                    let mut vals = Vec::new();
                    let mut next = 0i32;
                    while p.peek() != Some("}") {
                        let n = p.next()?;
                        if p.peek() == Some("=") {
                            p.i += 1;
                            let v = p.next()?;
                            next = if let Some(h) = v.strip_prefix("0x") { i32::from_str_radix(h, 16) } else { v.parse() }.map_err(|_| terr(format!("Invalid enum value {}", v)))?;
                        }
                        p.skip_annotations();
                        vals.push((n, next));
                        next += 1;
                        if matches!(p.peek(), Some(",") | Some(";")) {
                            p.i += 1;
                        }
                    }
                    p.i += 1;
                    p.skip_annotations();
                    idl.enums.insert(name, vals);
                }
                "struct" | "union" | "exception" => {
                    p.i += 1;
                    let name = p.next()?;
                    p.skip_annotations();
                    let fields = parse_field_list(&mut p, "{", "}", &idl)?;
                    p.skip_annotations();
                    idl.structs.insert(name, fields);
                }
                _ => p.i += 1,
            }
        }
        // resolve enum references
        let enums: Vec<String> = idl.enums.keys().cloned().collect();
        fn fix(t: &mut TType, enums: &[String]) {
            match t {
                TType::Struct(n) if enums.contains(n) => *t = TType::Enum(n.clone()),
                TType::List(e) | TType::Set(e) => fix(e, enums),
                TType::Map(k, v) => {
                    fix(k, enums);
                    fix(v, enums);
                }
                _ => {}
            }
        }
        for fs in idl.structs.values_mut() {
            for f in fs.iter_mut() {
                fix(&mut f.ty, &enums);
            }
        }
        for svc in idl.services.values_mut() {
            for m in svc.methods.iter_mut() {
                if let Some(r) = m.ret.as_mut() {
                    fix(r, &enums);
                }
                for f in m.args.iter_mut().chain(m.throws.iter_mut()) {
                    fix(&mut f.ty, &enums);
                }
            }
        }
        // synthesized RPC structs: `<Service>.<method>_args` / `_result`, TApplicationException
        let mut extra = Vec::new();
        for svc in idl.services.values() {
            for m in &svc.methods {
                extra.push((format!("{}.{}_args", svc.name, m.name), m.args.clone()));
                let mut res = Vec::new();
                if let Some(r) = &m.ret {
                    res.push(TField { id: 0, name: "success".into(), ty: r.clone(), required: false, default: None });
                }
                res.extend(m.throws.iter().cloned());
                extra.push((format!("{}.{}_result", svc.name, m.name), res));
            }
        }
        extra.push((
            "TApplicationException".to_string(),
            vec![TField { id: 1, name: "message".into(), ty: TType::String, required: false, default: None }, TField { id: 2, name: "type".into(), ty: TType::I32, required: false, default: None }],
        ));
        for (n, f) in extra {
            idl.structs.entry(n).or_insert(f);
        }
        Ok(idl)
    }

    /// A service and its inherited methods (`extends`).
    pub fn service_method(&self, service: &str, method: &str) -> Option<(&TService, &TMethod)> {
        let mut cur = self.services.get(service.rsplit('.').next().unwrap_or(service));
        let mut guard = 0;
        while let Some(s) = cur {
            if let Some(m) = s.methods.iter().find(|m| m.name == method) {
                return Some((s, m));
            }
            cur = s.extends.as_ref().and_then(|b| self.services.get(b));
            guard += 1;
            if guard > 32 {
                break;
            }
        }
        None
    }

    fn fields(&self, name: &str) -> Result<&Vec<TField>> {
        self.structs.get(name).ok_or_else(|| terr(format!("Unknown struct {}", name)))
    }
}

// ------------------------------------------------------------------------- protocols

const B_STOP: u8 = 0;
const B_BOOL: u8 = 2;
const B_BYTE: u8 = 3;
const B_DOUBLE: u8 = 4;
const B_I16: u8 = 6;
const B_I32: u8 = 8;
const B_I64: u8 = 10;
const B_STRING: u8 = 11;
const B_STRUCT: u8 = 12;
const B_MAP: u8 = 13;
const B_SET: u8 = 14;
const B_LIST: u8 = 15;
const B_UUID: u8 = 16;

fn btype(t: &TType) -> u8 {
    match t {
        TType::Bool => B_BOOL,
        TType::Byte => B_BYTE,
        TType::I16 => B_I16,
        TType::I32 | TType::Enum(_) => B_I32,
        TType::I64 => B_I64,
        TType::Double => B_DOUBLE,
        TType::String | TType::Binary => B_STRING,
        TType::Uuid => B_UUID,
        TType::List(_) => B_LIST,
        TType::Set(_) => B_SET,
        TType::Map(..) => B_MAP,
        TType::Struct(_) => B_STRUCT,
    }
}

/// Compact protocol type ids.
fn ctype(b: u8) -> u8 {
    match b {
        B_BOOL => 1,
        B_BYTE => 3,
        B_I16 => 4,
        B_I32 => 5,
        B_I64 => 6,
        B_DOUBLE => 7,
        B_STRING => 8,
        B_LIST => 9,
        B_SET => 10,
        B_MAP => 11,
        B_STRUCT => 12,
        B_UUID => 13,
        _ => 0,
    }
}

fn from_ctype(c: u8) -> Result<u8> {
    Ok(match c {
        1 | 2 => B_BOOL,
        3 => B_BYTE,
        4 => B_I16,
        5 => B_I32,
        6 => B_I64,
        7 => B_DOUBLE,
        8 => B_STRING,
        9 => B_LIST,
        10 => B_SET,
        11 => B_MAP,
        12 => B_STRUCT,
        13 => B_UUID,
        o => return Err(terr(format!("Invalid compact type {}", o))),
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Protocol {
    Binary,
    Compact,
}

struct W<'a> {
    idl: &'a Idl,
    proto: Protocol,
    out: Vec<u8>,
    last_id: Vec<i16>,
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

impl W<'_> {
    fn int(&mut self, b: u8, v: i64) {
        match self.proto {
            Protocol::Binary => match b {
                B_BYTE => self.out.push(v as u8),
                B_I16 => self.out.extend_from_slice(&(v as i16).to_be_bytes()),
                B_I32 => self.out.extend_from_slice(&(v as i32).to_be_bytes()),
                _ => self.out.extend_from_slice(&v.to_be_bytes()),
            },
            Protocol::Compact => match b {
                B_BYTE => self.out.push(v as u8),
                _ => varint(&mut self.out, zz(v)),
            },
        }
    }
    fn bin(&mut self, b: &[u8]) {
        match self.proto {
            Protocol::Binary => self.out.extend_from_slice(&(b.len() as i32).to_be_bytes()),
            Protocol::Compact => varint(&mut self.out, b.len() as u64),
        }
        self.out.extend_from_slice(b);
    }
    fn coll_header(&mut self, et: u8, n: usize) {
        match self.proto {
            Protocol::Binary => {
                self.out.push(et);
                self.out.extend_from_slice(&(n as i32).to_be_bytes());
            }
            Protocol::Compact => {
                if n < 15 {
                    self.out.push(((n as u8) << 4) | ctype(et));
                } else {
                    self.out.push(0xf0 | ctype(et));
                    varint(&mut self.out, n as u64);
                }
            }
        }
    }
    fn value(&mut self, t: &TType, v: &Value) -> Result<()> {
        let bad = || terr(format!("Value {} does not match thrift type {:?}", v.to_json(), t));
        let as_i = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
        match t {
            TType::Bool => {
                let b = v.as_bool().ok_or_else(bad)?;
                match self.proto {
                    Protocol::Binary => self.out.push(b as u8),
                    Protocol::Compact => self.out.push(if b { 1 } else { 2 }),
                }
            }
            TType::Byte | TType::I16 | TType::I32 | TType::I64 => {
                let n = as_i(v).ok_or_else(bad)?;
                self.int(btype(t), n)
            }
            TType::Enum(e) => {
                let n = match v {
                    Value::String(s) => self.idl.enums.get(e).and_then(|vals| vals.iter().find(|(n, _)| n == s)).map(|(_, x)| *x as i64).or_else(|| s.parse().ok()).ok_or_else(bad)?,
                    other => as_i(other).ok_or_else(bad)?,
                };
                self.int(B_I32, n)
            }
            TType::Double => {
                let f = v.as_f64().ok_or_else(bad)?;
                match self.proto {
                    Protocol::Binary => self.out.extend_from_slice(&f.to_be_bytes()),
                    Protocol::Compact => self.out.extend_from_slice(&f.to_le_bytes()),
                }
            }
            TType::String | TType::Binary => match v {
                Value::Bytes(b) => self.bin(&b.clone()),
                other => self.bin(other.to_string().as_bytes()),
            },
            TType::Uuid => {
                let h: String = v.to_string().chars().filter(|c| *c != '-').collect();
                let b = camel_tls::util::try_unhex(&h).filter(|b| b.len() == 16).ok_or_else(bad)?;
                self.out.extend_from_slice(&b);
            }
            TType::List(e) | TType::Set(e) => {
                let a = v.as_array().ok_or_else(bad)?;
                self.coll_header(btype(e), a.len());
                for x in a {
                    self.value(e, x)?;
                }
            }
            TType::Map(k, vt) => {
                let m = v.as_object().ok_or_else(bad)?;
                match self.proto {
                    Protocol::Binary => {
                        self.out.push(btype(k));
                        self.out.push(btype(vt));
                        self.out.extend_from_slice(&(m.len() as i32).to_be_bytes());
                    }
                    Protocol::Compact => {
                        varint(&mut self.out, m.len() as u64);
                        if !m.is_empty() {
                            self.out.push((ctype(btype(k)) << 4) | ctype(btype(vt)));
                        }
                    }
                }
                for (key, x) in m.iter() {
                    let kv = match **k {
                        TType::String | TType::Binary | TType::Enum(_) => Value::String(key.clone()),
                        TType::Double => Value::Float(key.parse().map_err(|_| bad())?),
                        TType::Bool => Value::Bool(key == "true"),
                        _ => Value::Int(key.parse().map_err(|_| bad())?),
                    };
                    self.value(k, &kv)?;
                    self.value(vt, x)?;
                }
            }
            TType::Struct(name) => self.structure(name, v)?,
        }
        Ok(())
    }
    fn structure(&mut self, name: &str, v: &Value) -> Result<()> {
        let o = v.as_object().ok_or_else(|| terr(format!("Struct {} expects an object, got {}", name, v.type_name())))?;
        let fields = self.idl.fields(name)?.clone();
        self.last_id.push(0);
        for f in &fields {
            let fv = match o.get(&f.name).filter(|v| !v.is_null()).or(f.default.as_ref()) {
                None => {
                    if f.required {
                        return Err(terr(format!("Required field '{}' was not present! Struct: {}", f.name, name)));
                    }
                    continue;
                }
                Some(x) => x,
            };
            let bt = btype(&f.ty);
            match self.proto {
                Protocol::Binary => {
                    self.out.push(bt);
                    self.out.extend_from_slice(&f.id.to_be_bytes());
                    self.value(&f.ty, fv)?;
                }
                Protocol::Compact => {
                    let last = *self.last_id.last().unwrap();
                    let ct = if bt == B_BOOL {
                        if fv.as_bool().ok_or_else(|| terr(format!("Field {} expects a bool", f.name)))? {
                            1
                        } else {
                            2
                        }
                    } else {
                        ctype(bt)
                    };
                    let delta = f.id as i32 - last as i32;
                    if delta > 0 && delta <= 15 {
                        self.out.push(((delta as u8) << 4) | ct);
                    } else {
                        self.out.push(ct);
                        varint(&mut self.out, zz(f.id as i64));
                    }
                    *self.last_id.last_mut().unwrap() = f.id;
                    if bt != B_BOOL {
                        self.value(&f.ty, fv)?;
                    }
                }
            }
        }
        self.out.push(B_STOP);
        self.last_id.pop();
        Ok(())
    }
}

/// Serialize a struct value.
pub fn serialize(idl: &Idl, name: &str, v: &Value, proto: Protocol) -> Result<Vec<u8>> {
    let mut w = W { idl, proto, out: Vec::new(), last_id: Vec::new() };
    w.structure(name, v)?;
    Ok(w.out)
}

struct R<'a> {
    idl: &'a Idl,
    proto: Protocol,
    d: &'a [u8],
    p: usize,
    depth: usize,
}

impl R<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        if self.d.len() - self.p < n {
            return Err(terr("Unexpected end of thrift data"));
        }
        let s = &self.d[self.p..self.p + n];
        self.p += n;
        Ok(s)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn varint(&mut self) -> Result<u64> {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(terr("Invalid varint"));
            }
        }
    }
    fn zz(&mut self) -> Result<i64> {
        let v = self.varint()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    fn int(&mut self, b: u8) -> Result<i64> {
        Ok(match (self.proto, b) {
            (_, B_BYTE) => self.byte()? as i8 as i64,
            (Protocol::Binary, B_I16) => i16::from_be_bytes(self.take(2)?.try_into().unwrap()) as i64,
            (Protocol::Binary, B_I32) => i32::from_be_bytes(self.take(4)?.try_into().unwrap()) as i64,
            (Protocol::Binary, _) => i64::from_be_bytes(self.take(8)?.try_into().unwrap()),
            (Protocol::Compact, _) => self.zz()?,
        })
    }
    fn len(&mut self) -> Result<usize> {
        let n = match self.proto {
            Protocol::Binary => i32::from_be_bytes(self.take(4)?.try_into().unwrap()) as i64,
            Protocol::Compact => self.varint()? as i64,
        };
        if n < 0 || n as usize > self.d.len() - self.p + 16 * 1024 {
            return Err(terr(format!("Invalid length {}", n)));
        }
        Ok(n as usize)
    }
    fn coll(&mut self) -> Result<(u8, usize)> {
        match self.proto {
            Protocol::Binary => {
                let et = self.byte()?;
                Ok((et, self.len()?))
            }
            Protocol::Compact => {
                let h = self.byte()?;
                let et = from_ctype(h & 0x0f)?;
                let n = if h >> 4 == 15 { self.len()? } else { (h >> 4) as usize };
                Ok((et, n))
            }
        }
    }
    /// Read a value of wire type `b` (with the declared type when known).
    fn value(&mut self, b: u8, t: Option<&TType>, compact_bool: Option<bool>) -> Result<Value> {
        self.depth += 1;
        if self.depth > 64 {
            return Err(terr("Thrift nesting too deep"));
        }
        let v = match b {
            B_BOOL => Value::Bool(match compact_bool {
                Some(x) => x,
                None => {
                    let x = self.byte()?;
                    if self.proto == Protocol::Compact {
                        x == 1
                    } else {
                        x != 0
                    }
                }
            }),
            B_BYTE | B_I16 | B_I32 | B_I64 => {
                let n = self.int(b)?;
                match t {
                    Some(TType::Enum(e)) => self.idl.enums.get(e).and_then(|vals| vals.iter().find(|(_, x)| *x as i64 == n)).map(|(name, _)| Value::String(name.clone())).unwrap_or(Value::Int(n)),
                    _ => Value::Int(n),
                }
            }
            B_DOUBLE => {
                let bin = self.proto == Protocol::Binary;
                let s: [u8; 8] = self.take(8)?.try_into().unwrap();
                Value::Float(if bin { f64::from_be_bytes(s) } else { f64::from_le_bytes(s) })
            }
            B_STRING => {
                let n = self.len()?;
                let s = self.take(n)?.to_vec();
                match t {
                    Some(TType::Binary) => Value::Bytes(s),
                    _ => match String::from_utf8(s) {
                        Ok(x) => Value::String(x),
                        Err(e) => Value::Bytes(e.into_bytes()),
                    },
                }
            }
            B_UUID => {
                let h: String = self.take(16)?.iter().map(|x| format!("{:02x}", x)).collect();
                Value::String(format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..]))
            }
            B_LIST | B_SET => {
                let (et, n) = self.coll()?;
                let e = match t {
                    Some(TType::List(e)) | Some(TType::Set(e)) => Some(&**e),
                    _ => None,
                };
                let mut a = Vec::new();
                for _ in 0..n {
                    a.push(self.value(et, e, None)?);
                }
                Value::Array(a)
            }
            B_MAP => {
                let (kt, vt, n) = match self.proto {
                    Protocol::Binary => {
                        let kt = self.byte()?;
                        let vt = self.byte()?;
                        (kt, vt, self.len()?)
                    }
                    Protocol::Compact => {
                        let n = self.len()?;
                        if n == 0 {
                            (0, 0, 0)
                        } else {
                            let h = self.byte()?;
                            (from_ctype(h >> 4)?, from_ctype(h & 0x0f)?, n)
                        }
                    }
                };
                let (kd, vd) = match t {
                    Some(TType::Map(k, v)) => (Some(&**k), Some(&**v)),
                    _ => (None, None),
                };
                let mut m = ObjectMap::new();
                for _ in 0..n {
                    let k = self.value(kt, kd, None)?;
                    let v = self.value(vt, vd, None)?;
                    m.insert(k.to_string(), v);
                }
                Value::Object(m)
            }
            B_STRUCT => {
                let name = match t {
                    Some(TType::Struct(n)) => Some(n.as_str()),
                    _ => None,
                };
                self.structure(name)?
            }
            o => return Err(terr(format!("Invalid thrift type {}", o))),
        };
        self.depth -= 1;
        Ok(v)
    }
    fn structure(&mut self, name: Option<&str>) -> Result<Value> {
        let fields = match name {
            Some(n) => Some(self.idl.fields(n)?.clone()),
            None => None,
        };
        let mut m = ObjectMap::new();
        let mut last: i16 = 0;
        loop {
            let h = self.byte()?;
            if h == B_STOP {
                break;
            }
            let (bt, id, cb) = match self.proto {
                Protocol::Binary => (h, i16::from_be_bytes(self.take(2)?.try_into().unwrap()), None),
                Protocol::Compact => {
                    let ct = h & 0x0f;
                    let delta = h >> 4;
                    let id = if delta == 0 { self.zz()? as i16 } else { last + delta as i16 };
                    last = id;
                    let bt = from_ctype(ct)?;
                    (bt, id, if bt == B_BOOL { Some(ct == 1) } else { None })
                }
            };
            let f = fields.as_ref().and_then(|fs| fs.iter().find(|f| f.id == id));
            let v = self.value(bt, f.map(|f| &f.ty), cb)?;
            match f {
                Some(f) => {
                    m.insert(f.name.clone(), v);
                }
                None if fields.is_none() => {
                    m.insert(id.to_string(), v);
                }
                None => {} // unknown field: skipped
            }
        }
        if let Some(fs) = &fields {
            for f in fs {
                if let (Some(d), false) = (&f.default, m.contains_key(&f.name)) {
                    m.insert(f.name.clone(), d.clone());
                }
                if f.required && !m.contains_key(&f.name) {
                    return Err(terr(format!("Required field '{}' was not found in serialized data! Struct: {}", f.name, name.unwrap_or(""))));
                }
            }
        }
        Ok(Value::Object(m))
    }
}

/// Deserialize a struct.
pub fn deserialize(idl: &Idl, name: &str, data: &[u8], proto: Protocol) -> Result<Value> {
    let mut r = R { idl, proto, d: data, p: 0, depth: 0 };
    r.structure(Some(name))
}

/// Deserialize a struct at the start of `data`, returning the value and the bytes consumed.
pub fn deserialize_prefix(idl: &Idl, name: &str, data: &[u8], proto: Protocol) -> Result<(Value, usize)> {
    let mut r = R { idl, proto, d: data, p: 0, depth: 0 };
    let v = r.structure(Some(name))?;
    Ok((v, r.p))
}

// ------------------------------------------------------------------------- messages

/// Message types (`TMessageType`).
pub mod message_type {
    pub const CALL: u8 = 1;
    pub const REPLY: u8 = 2;
    pub const EXCEPTION: u8 = 3;
    pub const ONEWAY: u8 = 4;
}

/// A message header: name, type and sequence id.
#[derive(Clone, Debug, PartialEq)]
pub struct MessageHeader {
    pub name: String,
    pub mtype: u8,
    pub seqid: i32,
}

/// Write a strict binary or compact message header.
pub fn write_message_header(out: &mut Vec<u8>, proto: Protocol, h: &MessageHeader) {
    match proto {
        Protocol::Binary => {
            out.extend_from_slice(&(0x8001_0000u32 | h.mtype as u32).to_be_bytes());
            out.extend_from_slice(&(h.name.len() as i32).to_be_bytes());
            out.extend_from_slice(h.name.as_bytes());
            out.extend_from_slice(&h.seqid.to_be_bytes());
        }
        Protocol::Compact => {
            out.push(0x82);
            out.push(1 | (h.mtype << 5));
            varint(out, h.seqid as u32 as u64);
            varint(out, h.name.len() as u64);
            out.extend_from_slice(h.name.as_bytes());
        }
    }
}

/// Read a message header (strict or old-style binary, compact). Returns the header and the
/// bytes consumed.
pub fn read_message_header(data: &[u8], proto: Protocol) -> Result<(MessageHeader, usize)> {
    let idl = Idl::default();
    let mut r = R { idl: &idl, proto, d: data, p: 0, depth: 0 };
    let h = match proto {
        Protocol::Binary => {
            let first = i32::from_be_bytes(r.take(4)?.try_into().unwrap());
            if first < 0 {
                if (first as u32) & 0xffff_0000 != 0x8001_0000 {
                    return Err(terr("Bad version in readMessageBegin"));
                }
                let n = r.len()?;
                let name = String::from_utf8_lossy(r.take(n)?).into_owned();
                let seqid = i32::from_be_bytes(r.take(4)?.try_into().unwrap());
                MessageHeader { name, mtype: (first & 0xff) as u8, seqid }
            } else {
                let name = String::from_utf8_lossy(r.take(first as usize)?).into_owned();
                let mtype = r.byte()?;
                let seqid = i32::from_be_bytes(r.take(4)?.try_into().unwrap());
                MessageHeader { name, mtype, seqid }
            }
        }
        Protocol::Compact => {
            if r.byte()? != 0x82 {
                return Err(terr("Expected protocol id 0x82 in compact message"));
            }
            let vt = r.byte()?;
            if vt & 0x1f != 1 {
                return Err(terr(format!("Expected compact version 1 but got {}", vt & 0x1f)));
            }
            let seqid = r.varint()? as u32 as i32;
            let n = r.len()?;
            let name = String::from_utf8_lossy(r.take(n)?).into_owned();
            MessageHeader { name, mtype: vt >> 5, seqid }
        }
    };
    Ok((h, r.p))
}

// ------------------------------------------------------------------------- TJSONProtocol

fn tjson_type(t: &TType) -> &'static str {
    match t {
        TType::Bool => "tf",
        TType::Byte => "i8",
        TType::I16 => "i16",
        TType::I32 | TType::Enum(_) => "i32",
        TType::I64 => "i64",
        TType::Double => "dbl",
        TType::String | TType::Binary => "str",
        TType::Uuid => "uid",
        TType::Struct(_) => "rec",
        TType::Map(..) => "map",
        TType::Set(_) => "set",
        TType::List(_) => "lst",
    }
}

fn tjson_value(idl: &Idl, t: &TType, v: &Value) -> Result<Value> {
    let bad = || terr(format!("Value {} does not match thrift type {:?}", v.to_json(), t));
    let as_i = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
    Ok(match t {
        TType::Bool => Value::Int(v.as_bool().ok_or_else(bad)? as i64),
        TType::Byte | TType::I16 | TType::I32 | TType::I64 => Value::Int(as_i(v).ok_or_else(bad)?),
        TType::Enum(e) => Value::Int(match v {
            Value::String(s) => idl.enums.get(e).and_then(|vals| vals.iter().find(|(n, _)| n == s)).map(|(_, x)| *x as i64).or_else(|| s.parse().ok()).ok_or_else(bad)?,
            other => as_i(other).ok_or_else(bad)?,
        }),
        TType::Double => {
            let f = v.as_f64().ok_or_else(bad)?;
            if f.is_nan() {
                Value::String("NaN".into())
            } else if f.is_infinite() {
                Value::String(if f > 0.0 { "Infinity" } else { "-Infinity" }.into())
            } else {
                Value::Float(f)
            }
        }
        TType::Binary => Value::String(camel_tls::pem::base64_encode(&v.to_bytes())),
        TType::String | TType::Uuid => Value::String(v.to_string()),
        TType::List(e) | TType::Set(e) => {
            let a = v.as_array().ok_or_else(bad)?;
            let mut out = vec![Value::String(tjson_type(e).into()), Value::Int(a.len() as i64)];
            for x in a {
                out.push(tjson_value(idl, e, x)?);
            }
            Value::Array(out)
        }
        TType::Map(k, vt) => {
            let m = v.as_object().ok_or_else(bad)?;
            let mut o = ObjectMap::new();
            for (key, x) in m.iter() {
                let key = match **k {
                    TType::Enum(ref e) => idl.enums.get(e).and_then(|vals| vals.iter().find(|(n, _)| n == key)).map(|(_, x)| x.to_string()).unwrap_or_else(|| key.clone()),
                    TType::Bool => (if key == "true" { "1" } else { "0" }).to_string(),
                    _ => key.clone(),
                };
                o.insert(key, tjson_value(idl, vt, x)?);
            }
            Value::Array(vec![Value::String(tjson_type(k).into()), Value::String(tjson_type(vt).into()), Value::Int(m.len() as i64), Value::Object(o)])
        }
        TType::Struct(name) => tjson_struct(idl, name, v)?,
    })
}

fn tjson_struct(idl: &Idl, name: &str, v: &Value) -> Result<Value> {
    let o = v.as_object().ok_or_else(|| terr(format!("Struct {} expects an object, got {}", name, v.type_name())))?;
    let mut out = ObjectMap::new();
    for f in idl.fields(name)? {
        match o.get(&f.name).filter(|v| !v.is_null()).or(f.default.as_ref()) {
            None if f.required => return Err(terr(format!("Required field '{}' was not present! Struct: {}", f.name, name))),
            None => {}
            Some(x) => {
                let mut t = ObjectMap::new();
                t.insert(tjson_type(&f.ty).to_string(), tjson_value(idl, &f.ty, x)?);
                out.insert(f.id.to_string(), Value::Object(t));
            }
        }
    }
    Ok(Value::Object(out))
}

fn from_tjson_value(idl: &Idl, t: &TType, v: &Value) -> Result<Value> {
    let bad = || terr(format!("Invalid TJSONProtocol value {} for {:?}", v.to_json(), t));
    let int = |v: &Value| v.as_i64().or_else(|| v.as_str().and_then(|s| s.parse().ok()));
    Ok(match t {
        TType::Bool => Value::Bool(int(v).ok_or_else(bad)? != 0),
        TType::Byte | TType::I16 | TType::I32 | TType::I64 => Value::Int(int(v).ok_or_else(bad)?),
        TType::Enum(e) => {
            let n = int(v).ok_or_else(bad)?;
            idl.enums.get(e).and_then(|vals| vals.iter().find(|(_, x)| *x as i64 == n)).map(|(name, _)| Value::String(name.clone())).unwrap_or(Value::Int(n))
        }
        TType::Double => match v {
            Value::String(s) => Value::Float(match s.as_str() {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                o => o.parse().map_err(|_| bad())?,
            }),
            o => Value::Float(o.as_f64().ok_or_else(bad)?),
        },
        TType::Binary => Value::Bytes(camel_tls::pem::base64_decode(v.as_str().ok_or_else(bad)?).ok_or_else(bad)?),
        TType::String | TType::Uuid => Value::String(v.as_str().ok_or_else(bad)?.to_string()),
        TType::List(e) | TType::Set(e) => {
            let a = v.as_array().ok_or_else(bad)?;
            Value::Array(a.iter().skip(2).map(|x| from_tjson_value(idl, e, x)).collect::<Result<_>>()?)
        }
        TType::Map(k, vt) => {
            let m = v.as_array().and_then(|a| a.get(3)).and_then(|m| m.as_object()).ok_or_else(bad)?;
            let mut o = ObjectMap::new();
            for (key, x) in m.iter() {
                let key = match **k {
                    TType::Enum(ref e) => key.parse::<i64>().ok().and_then(|n| idl.enums.get(e).and_then(|vals| vals.iter().find(|(_, x)| *x as i64 == n))).map(|(n, _)| n.clone()).unwrap_or_else(|| key.clone()),
                    TType::Bool => (key == "1" || key == "true").to_string(),
                    _ => key.clone(),
                };
                o.insert(key, from_tjson_value(idl, vt, x)?);
            }
            Value::Object(o)
        }
        TType::Struct(name) => from_tjson_struct(idl, name, v)?,
    })
}

fn from_tjson_struct(idl: &Idl, name: &str, v: &Value) -> Result<Value> {
    let o = v.as_object().ok_or_else(|| terr(format!("Expected a TJSONProtocol struct for {}", name)))?;
    let fields = idl.fields(name)?;
    let mut m = ObjectMap::new();
    for (id, tv) in o.iter() {
        let Some(f) = id.parse::<i16>().ok().and_then(|id| fields.iter().find(|f| f.id == id)) else { continue };
        let Some((_, x)) = tv.as_object().and_then(|t| t.iter().next()) else { return Err(terr("Invalid TJSONProtocol field")) };
        m.insert(f.name.clone(), from_tjson_value(idl, &f.ty, x)?);
    }
    for f in fields {
        if let (Some(d), false) = (&f.default, m.contains_key(&f.name)) {
            m.insert(f.name.clone(), d.clone());
        }
        if f.required && !m.contains_key(&f.name) {
            return Err(terr(format!("Required field '{}' was not found in serialized data! Struct: {}", f.name, name)));
        }
    }
    Ok(Value::Object(m))
}

/// Serialize a struct with TJSONProtocol (`{"1":{"i32":1},...}`).
pub fn to_tjson(idl: &Idl, name: &str, v: &Value) -> Result<String> {
    Ok(tjson_struct(idl, name, v)?.to_json())
}

/// Deserialize TJSONProtocol text.
pub fn from_tjson(idl: &Idl, name: &str, text: &str) -> Result<Value> {
    from_tjson_struct(idl, name, &camel_core::json::parse(text)?)
}

/// A TJSONProtocol message `[1,"name",type,seqid,{struct}]`.
pub fn tjson_message(idl: &Idl, h: &MessageHeader, struct_name: &str, v: &Value) -> Result<String> {
    Ok(Value::Array(vec![Value::Int(1), Value::String(h.name.clone()), Value::Int(h.mtype as i64), Value::Int(h.seqid as i64), tjson_struct(idl, struct_name, v)?]).to_json())
}

/// Parse a TJSONProtocol message: header and the raw struct value (decode it with
/// [`from_tjson_struct_value`] once the struct name is known).
pub fn parse_tjson_message(text: &str) -> Result<(MessageHeader, Value)> {
    let v = camel_core::json::parse(text)?;
    let a = v.as_array().filter(|a| a.len() >= 5 && a[0].as_i64() == Some(1)).ok_or_else(|| terr("Invalid TJSONProtocol message"))?;
    let h = MessageHeader { name: a[1].to_string(), mtype: a[2].as_i64().unwrap_or(0) as u8, seqid: a[3].as_i64().unwrap_or(0) as i32 };
    Ok((h, a[4].clone()))
}

/// Decode a TJSONProtocol struct value.
pub fn from_tjson_struct_value(idl: &Idl, name: &str, v: &Value) -> Result<Value> {
    from_tjson_struct(idl, name, v)
}

/// Length of the first complete JSON value in `data` (`None` if incomplete).
pub fn json_value_len(data: &[u8]) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut esc = false;
    let mut started = false;
    for (i, &c) in data.iter().enumerate() {
        if in_str {
            if esc {
                esc = false;
            } else if c == b'\\' {
                esc = true;
            } else if c == b'"' {
                in_str = false;
            }
            continue;
        }
        match c {
            b'"' => in_str = true,
            b'[' | b'{' => {
                depth += 1;
                started = true;
            }
            b']' | b'}' => {
                depth -= 1;
                if started && depth == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// The `thrift` data format.
pub struct ThriftDataFormat {
    pub idl: Idl,
    pub struct_name: String,
    pub format: String,
    pub content_type_header: bool,
}

impl DataFormat for ThriftDataFormat {
    fn name(&self) -> &str {
        "thrift"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let v = match &ex.message.body {
            Value::String(s) if self.format == "json" || self.format == "sjson" => camel_core::json::parse(s)?,
            other => other.clone(),
        };
        let (body, ct) = match self.format.as_str() {
            "json" => (Value::String(to_tjson(&self.idl, &self.struct_name, &v)?), "application/json"),
            "sjson" => {
                serialize(&self.idl, &self.struct_name, &v, Protocol::Binary)?; // validation
                (Value::String(v.to_json()), "application/json")
            }
            "compact" => (Value::Bytes(serialize(&self.idl, &self.struct_name, &v, Protocol::Compact)?), "application/octet-stream"),
            _ => (Value::Bytes(serialize(&self.idl, &self.struct_name, &v, Protocol::Binary)?), "application/octet-stream"),
        };
        ex.message.body = body;
        if self.content_type_header {
            ex.message.headers.set(CONTENT_TYPE, ct);
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        ex.message.body = match self.format.as_str() {
            "json" => from_tjson(&self.idl, &self.struct_name, &body_text(&ex.message.body)?)?,
            "sjson" => camel_core::json::parse(&body_text(&ex.message.body)?)?,
            "compact" => deserialize(&self.idl, &self.struct_name, &body_bytes(&ex.message.body), Protocol::Compact)?,
            _ => deserialize(&self.idl, &self.struct_name, &body_bytes(&ex.message.body), Protocol::Binary)?,
        };
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let src = match (o.get(&["schema", "idl"]), o.get(&["idlFile", "schemaFile"])) {
        (Some(s), _) => s.to_string(),
        (None, Some(f)) => String::from_utf8(crate::read_resource(f)?).map_err(|_| df_err("IDL file is not UTF-8"))?,
        _ => return Err(CamelError::illegal_argument("The thrift data format requires a schema (inline IDL) or idlFile")),
    };
    let idl = Idl::parse(&src)?;
    let name = o.get(&["instanceClass", "instanceClassName"]).map(|n| n.rsplit('.').next().unwrap_or(n).to_string());
    let name = match name {
        Some(n) => n,
        None if idl.structs.keys().filter(|k| !k.contains('.') && *k != "TApplicationException").count() == 1 => idl.structs.keys().find(|k| !k.contains('.') && *k != "TApplicationException").unwrap().clone(),
        None => return Err(CamelError::illegal_argument("The thrift data format requires instanceClass")),
    };
    idl.fields(&name)?;
    let format = o.string(&["contentTypeFormat"], "binary").to_ascii_lowercase();
    if !matches!(format.as_str(), "binary" | "compact" | "json" | "sjson") {
        return Err(CamelError::illegal_argument(format!("Unsupported thrift contentTypeFormat: {}", format)));
    }
    Ok(Arc::new(ThriftDataFormat { idl, struct_name: name, format, content_type_header: o.bool(&["contentTypeHeader"], true)? }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_core::json;

    const IDL: &str = r#"
        namespace java org.apache.camel.dataformat.thrift.generated
        // a comment
        enum Operation { ADD = 1, SUBTRACT = 2, MULTIPLY = 3 }
        typedef i32 MyInteger
        const i32 INT32CONSTANT = 9853
        struct Work {
          1: i32 num1 = 0,
          2: MyInteger num2,
          3: Operation op,
          4: optional string comment,
        }
        struct Holder { 1: required list<Work> works; 2: map<string, i64> counts; 3: bool flag; 4: binary data; 5: set<i16> s; 6: double d; 20: bool late }
        service Calculator { i32 add(1:i32 num1, 2:i32 num2), }
    "#;

    #[test]
    fn binary_known_bytes() {
        let idl = Idl::parse(IDL).unwrap();
        let v = json::parse(r#"{"num1":1,"num2":100,"op":"MULTIPLY","comment":"Hi"}"#).unwrap();
        let b = serialize(&idl, "Work", &v, Protocol::Binary).unwrap();
        assert_eq!(camel_tls::util::hex(&b), "080001000000010800020000006408000300000003" .to_string() + "0b0004000000024869" + "00");
        assert_eq!(deserialize(&idl, "Work", &b, Protocol::Binary).unwrap(), v);
        let c = serialize(&idl, "Work", &v, Protocol::Compact).unwrap();
        // compact: field 1 i32 (delta 1, type 5) zigzag(1)=2; field 2 zigzag(100)=200 (c8 01); op 6; string "Hi"
        assert_eq!(camel_tls::util::hex(&c), "150215c801150618024869" .to_string() + "00");
        assert_eq!(deserialize(&idl, "Work", &c, Protocol::Compact).unwrap(), v);
    }

    #[test]
    fn nested_round_trip() {
        let idl = Idl::parse(IDL).unwrap();
        let mut v = json::parse(r#"{"works":[{"num1":1,"num2":2,"op":"ADD"},{"num1":-5,"num2":70000,"op":"SUBTRACT","comment":"c"}],"counts":{"a":1,"b":-2},"flag":true,"s":[1,2,3],"d":1.5,"late":false}"#).unwrap();
        v.as_object_mut().unwrap().insert("data".into(), Value::Bytes(vec![0, 1, 2]));
        for p in [Protocol::Binary, Protocol::Compact] {
            let b = serialize(&idl, "Holder", &v, p).unwrap();
            let back = deserialize(&idl, "Holder", &b, p).unwrap();
            for k in ["works", "counts", "flag", "data", "s", "d", "late"] {
                assert_eq!(back.get(k), v.get(k), "{:?} {}", p, k);
            }
        }
        assert!(serialize(&idl, "Holder", &json::parse("{}").unwrap(), Protocol::Binary).is_err());
    }
}
