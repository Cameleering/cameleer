//! `thrift` data format: Apache Thrift binary and compact protocols driven by a runtime IDL
//! (instead of generated classes).
//!
//! The minimal IDL parser understands `struct`, `union`, `exception` (fields `1: required
//! i32 id = 0,` with base types `bool byte i8 i16 i32 i64 double string binary uuid`,
//! `list<T>`, `set<T>`, `map<K,V>`, struct/enum references), `enum`, `typedef`, and ignores
//! `namespace`, `include`, `const`, `service`, annotations and comments.
//!
//! Options: `instanceClass` (the struct name, optionally qualified), `schema` (inline IDL)
//! or `idlFile`, `contentTypeFormat`: `binary` (TBinaryProtocol, default), `compact`
//! (TCompactProtocol) or `json` (plain JSON of the value, like TSimpleJSONProtocol),
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
}

/// A parsed IDL.
#[derive(Clone, Debug, Default)]
pub struct Idl {
    pub structs: HashMap<String, Vec<TField>>,
    pub enums: HashMap<String, Vec<(String, i32)>>,
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
    fn skip_block(&mut self) {
        // skip until matching '}' (for services) or to ';'/newline-ish token for consts
        let mut depth = 0;
        while let Some(t) = self.t.get(self.i).cloned() {
            self.i += 1;
            match t.as_str() {
                "{" | "[" | "(" => depth += 1,
                "}" | "]" | ")" => {
                    depth -= 1;
                    if depth <= 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
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
                    p.ty()?;
                    p.next()?;
                    p.expect("=")?;
                    if matches!(p.peek(), Some("{") | Some("[")) {
                        p.skip_block();
                    } else {
                        p.next()?;
                    }
                    if matches!(p.peek(), Some(";") | Some(",")) {
                        p.i += 1;
                    }
                }
                "service" => {
                    while p.peek().is_some() && p.peek() != Some("{") {
                        p.i += 1;
                    }
                    p.skip_block();
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
                    p.expect("{")?;
                    let mut fields = Vec::new();
                    let mut auto_id = -1i16;
                    while p.peek() != Some("}") {
                        let mut id = None;
                        if p.t.get(p.i + 1).map(|s| s == ":").unwrap_or(false) {
                            id = Some(p.next()?.parse::<i16>().map_err(|_| terr("Invalid field id"))?);
                            p.i += 1;
                        }
                        let mut required = false;
                        if matches!(p.peek(), Some("required") | Some("optional")) {
                            required = p.next()? == "required";
                        }
                        let ty = p.ty()?;
                        let fname = p.next()?;
                        if p.peek() == Some("=") {
                            p.i += 1;
                            if matches!(p.peek(), Some("{") | Some("[")) {
                                p.skip_block();
                            } else {
                                p.next()?;
                            }
                        }
                        p.skip_annotations();
                        if matches!(p.peek(), Some(",") | Some(";")) {
                            p.i += 1;
                        }
                        let id = id.unwrap_or_else(|| {
                            auto_id -= 1;
                            auto_id + 1
                        });
                        fields.push(TField { id, name: fname, ty, required });
                    }
                    p.i += 1;
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
        Ok(idl)
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
            let fv = match o.get(&f.name) {
                Some(Value::Null) | None => {
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
            Value::String(s) if self.format == "json" => camel_core::json::parse(s)?,
            other => other.clone(),
        };
        let (body, ct) = match self.format.as_str() {
            "json" => {
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
            "json" => camel_core::json::parse(&body_text(&ex.message.body)?)?,
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
        None if idl.structs.len() == 1 => idl.structs.keys().next().unwrap().clone(),
        None => return Err(CamelError::illegal_argument("The thrift data format requires instanceClass")),
    };
    idl.fields(&name)?;
    let format = o.string(&["contentTypeFormat"], "binary").to_ascii_lowercase();
    if !matches!(format.as_str(), "binary" | "compact" | "json") {
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
