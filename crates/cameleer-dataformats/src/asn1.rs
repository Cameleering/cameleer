//! `asn1` data format: ASN.1 BER/DER.
//!
//! * `usingIterator=true` (Camel's main use): unmarshal splits a stream of BER encoded
//!   records into an array with the bytes of each top-level element (for `split`).
//! * otherwise unmarshal decodes BER/DER into a `Value` tree: `SEQUENCE`/`SET` -> arrays,
//!   `INTEGER`/`ENUMERATED` -> integers (decimal strings when larger than 64 bits),
//!   `BOOLEAN`, `NULL`, `OCTET STRING` -> bytes, character strings -> strings, `OBJECT
//!   IDENTIFIER` -> dotted string, times -> strings, `BIT STRING` -> bytes (unused bits
//!   dropped); application/context/private tags and other universal types become
//!   `{"tag": "[0]" | "[APPLICATION 1]" | "[UNIVERSAL n]", "value": ...}` (constructed) or
//!   `{"tag": ..., "bytes": ...}` (primitive). Indefinite lengths are accepted.
//! * marshal encodes such a tree as DER: integers, booleans, null, strings (UTF8String),
//!   bytes (OCTET STRING), arrays (SEQUENCE), `{"oid": "1.2.3"}`, `{"set": [...]}`,
//!   `{"printable"|"ia5"|"utcTime"|"generalizedTime": "..."}`, tagged objects as above
//!   (implicit for `bytes`, explicit/constructed for `value`), and other objects as a
//!   SEQUENCE of their values. Bytes bodies are passed through unchanged.
//! * typed mode (instead of jASN1-generated classes): `schema` (inline ASN.1 module text) or
//!   `moduleFile` plus `clazzName` (the type name, optionally qualified) encode / decode
//!   values of that type with the module compiler in [`crate::asn1_module`]; with
//!   `usingIterator=true` unmarshal decodes a stream of records into an array.

use std::sync::Arc;

use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::value::ObjectMap;
use cameleer_core::Value;

use crate::{body_bytes, df_err, Opts};

pub(crate) struct Tlv<'a> {
    pub class: u8,
    pub constructed: bool,
    pub tag: u64,
    pub content: &'a [u8],
    /// Content of an indefinite length element includes nested elements up to end-of-contents.
    pub total: usize,
}

pub(crate) fn read_tlv(d: &[u8], depth: usize) -> Result<Tlv<'_>> {
    if depth > 64 {
        return Err(df_err("ASN.1 nesting too deep"));
    }
    let b0 = *d.first().ok_or_else(|| df_err("Truncated ASN.1 data"))?;
    let class = b0 >> 6;
    let constructed = b0 & 0x20 != 0;
    let mut i = 1;
    let mut tag = (b0 & 0x1f) as u64;
    if tag == 0x1f {
        tag = 0;
        loop {
            let b = *d.get(i).ok_or_else(|| df_err("Truncated ASN.1 tag"))?;
            i += 1;
            tag = (tag << 7) | (b & 0x7f) as u64;
            if b & 0x80 == 0 {
                break;
            }
            if i > 10 {
                return Err(df_err("ASN.1 tag too large"));
            }
        }
    }
    let l0 = *d.get(i).ok_or_else(|| df_err("Truncated ASN.1 length"))?;
    i += 1;
    if l0 == 0x80 {
        if !constructed {
            return Err(df_err("Indefinite length on a primitive ASN.1 element"));
        }
        // walk nested elements until 00 00
        let mut j = i;
        loop {
            if d.get(j..j + 2) == Some(&[0, 0]) {
                return Ok(Tlv { class, constructed, tag, content: &d[i..j], total: j + 2 });
            }
            if j >= d.len() {
                return Err(df_err("Missing ASN.1 end-of-contents"));
            }
            let t = read_tlv(&d[j..], depth + 1)?;
            j += t.total;
        }
    }
    let len = if l0 & 0x80 == 0 {
        l0 as usize
    } else {
        let n = (l0 & 0x7f) as usize;
        if n > 8 {
            return Err(df_err("ASN.1 length too large"));
        }
        let mut l = 0usize;
        for k in 0..n {
            l = (l << 8) | *d.get(i + k).ok_or_else(|| df_err("Truncated ASN.1 length"))? as usize;
        }
        i += n;
        l
    };
    let end = i.checked_add(len).filter(|e| *e <= d.len()).ok_or_else(|| df_err("Truncated ASN.1 content"))?;
    Ok(Tlv { class, constructed, tag, content: &d[i..end], total: end })
}

fn tag_name(class: u8, tag: u64) -> String {
    match class {
        0 => format!("[UNIVERSAL {}]", tag),
        1 => format!("[APPLICATION {}]", tag),
        2 => format!("[{}]", tag),
        _ => format!("[PRIVATE {}]", tag),
    }
}

fn parse_tag_name(s: &str) -> Option<(u8, u64)> {
    let inner = s.trim().strip_prefix('[')?.strip_suffix(']')?;
    let mut parts = inner.split_whitespace();
    let a = parts.next()?;
    match parts.next() {
        None => Some((2, a.parse().ok()?)),
        Some(n) => {
            let c = match a.to_ascii_uppercase().as_str() {
                "UNIVERSAL" => 0,
                "APPLICATION" => 1,
                "CONTEXT" => 2,
                "PRIVATE" => 3,
                _ => return None,
            };
            Some((c, n.parse().ok()?))
        }
    }
}

pub(crate) fn decode_int(c: &[u8]) -> Value {
    if c.is_empty() {
        return Value::Int(0);
    }
    if c.len() <= 8 {
        let mut v: i64 = if c[0] & 0x80 != 0 { -1 } else { 0 };
        for b in c {
            v = (v << 8) | *b as i64;
        }
        return Value::Int(v);
    }
    if c.len() <= 16 {
        let mut v: i128 = if c[0] & 0x80 != 0 { -1 } else { 0 };
        for b in c {
            v = (v << 8) | *b as i128;
        }
        return Value::String(v.to_string());
    }
    Value::Bytes(c.to_vec())
}

pub(crate) fn decode_oid(c: &[u8]) -> String {
    let mut parts: Vec<u128> = Vec::new();
    let mut v: u128 = 0;
    for b in c {
        v = (v << 7) | (b & 0x7f) as u128;
        if b & 0x80 == 0 {
            if parts.is_empty() {
                let first = if v < 40 { 0 } else if v < 80 { 1 } else { 2 };
                parts.push(first);
                parts.push(v - first * 40);
            } else {
                parts.push(v);
            }
            v = 0;
        }
    }
    parts.iter().map(|p| p.to_string()).collect::<Vec<_>>().join(".")
}

fn decode(d: &[u8], depth: usize) -> Result<(Value, usize)> {
    let t = read_tlv(d, depth)?;
    let children = |c: &[u8]| -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < c.len() {
            let (v, n) = decode(&c[i..], depth + 1)?;
            out.push(v);
            i += n;
        }
        Ok(out)
    };
    let v = if t.class == 0 {
        match (t.tag, t.constructed) {
            (16, true) => Value::Array(children(t.content)?),
            (17, true) => Value::object([("set", Value::Array(children(t.content)?))]),
            (1, false) => Value::Bool(t.content.first().map(|b| *b != 0).unwrap_or(false)),
            (2, false) | (10, false) => decode_int(t.content),
            (3, false) => Value::Bytes(t.content.get(1..).unwrap_or(&[]).to_vec()),
            (4, false) => Value::Bytes(t.content.to_vec()),
            (5, false) => Value::Null,
            (6, false) => Value::object([("oid", Value::String(decode_oid(t.content)))]),
            (12, false) | (18, false) | (19, false) | (20, false) | (22, false) | (25, false) | (26, false) | (27, false) => Value::String(String::from_utf8_lossy(t.content).to_string()),
            (23, false) => Value::object([("utcTime", Value::String(String::from_utf8_lossy(t.content).to_string()))]),
            (24, false) => Value::object([("generalizedTime", Value::String(String::from_utf8_lossy(t.content).to_string()))]),
            (30, false) => Value::String(t.content.chunks(2).filter_map(|c| if c.len() == 2 { char::from_u32(u16::from_be_bytes([c[0], c[1]]) as u32) } else { None }).collect()),
            (4, true) | (3, true) | (12, true) => {
                // constructed strings (BER): concatenate segments
                let mut out = Vec::new();
                for c in children(t.content)? {
                    out.extend(c.to_bytes());
                }
                if t.tag == 12 {
                    Value::String(String::from_utf8_lossy(&out).to_string())
                } else {
                    Value::Bytes(out)
                }
            }
            _ => tagged(&t, &children)?,
        }
    } else {
        tagged(&t, &children)?
    };
    Ok((v, t.total))
}

fn tagged(t: &Tlv<'_>, children: &dyn Fn(&[u8]) -> Result<Vec<Value>>) -> Result<Value> {
    let mut m = ObjectMap::new();
    m.insert("tag".into(), Value::String(tag_name(t.class, t.tag)));
    if t.constructed {
        let mut c = children(t.content)?;
        m.insert("value".into(), if c.len() == 1 { c.remove(0) } else { Value::Array(c) });
    } else {
        m.insert("bytes".into(), Value::Bytes(t.content.to_vec()));
    }
    Ok(Value::Object(m))
}

/// Decode one BER/DER element (trailing data is an error).
pub fn ber_decode(d: &[u8]) -> Result<Value> {
    let (v, n) = decode(d, 0)?;
    if n != d.len() {
        return Err(df_err("Trailing data after ASN.1 element"));
    }
    Ok(v)
}

/// Split a stream of top-level BER elements.
pub fn ber_split(d: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let t = read_tlv(&d[i..], 0)?;
        out.push(d[i..i + t.total].to_vec());
        i += t.total;
    }
    Ok(out)
}

pub(crate) fn tlv(out: &mut Vec<u8>, class: u8, constructed: bool, tag: u64, content: &[u8]) {
    let b0 = (class << 6) | if constructed { 0x20 } else { 0 };
    if tag < 31 {
        out.push(b0 | tag as u8);
    } else {
        out.push(b0 | 0x1f);
        let mut groups = Vec::new();
        let mut t = tag;
        loop {
            groups.push((t & 0x7f) as u8);
            t >>= 7;
            if t == 0 {
                break;
            }
        }
        for (i, g) in groups.iter().rev().enumerate() {
            out.push(if i + 1 < groups.len() { g | 0x80 } else { *g });
        }
    }
    let l = content.len();
    if l < 128 {
        out.push(l as u8);
    } else {
        let bytes: Vec<u8> = l.to_be_bytes().iter().copied().skip_while(|b| *b == 0).collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
    out.extend_from_slice(content);
}

pub(crate) fn encode_int(v: i128) -> Vec<u8> {
    let b = v.to_be_bytes();
    let mut i = 0;
    while i < 15 && ((b[i] == 0 && b[i + 1] & 0x80 == 0) || (b[i] == 0xff && b[i + 1] & 0x80 != 0)) {
        i += 1;
    }
    b[i..].to_vec()
}

pub(crate) fn encode_oid(s: &str) -> Result<Vec<u8>> {
    let parts: Vec<u128> = s.split('.').map(|p| p.trim().parse().map_err(|_| df_err(format!("Invalid OID: {}", s)))).collect::<Result<_>>()?;
    if parts.len() < 2 || parts[0] > 2 {
        return Err(df_err(format!("Invalid OID: {}", s)));
    }
    let mut out = Vec::new();
    let mut push = |mut v: u128| {
        let mut g = vec![(v & 0x7f) as u8];
        v >>= 7;
        while v > 0 {
            g.push((v & 0x7f) as u8 | 0x80);
            v >>= 7;
        }
        g.reverse();
        out.extend(g);
    };
    push(parts[0] * 40 + parts[1]);
    for p in &parts[2..] {
        push(*p);
    }
    Ok(out)
}

/// Encode a value tree as DER.
pub fn der_encode(v: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    enc(v, &mut out)?;
    Ok(out)
}

fn enc(v: &Value, out: &mut Vec<u8>) -> Result<()> {
    match v {
        Value::Null => tlv(out, 0, false, 5, &[]),
        Value::Bool(b) => tlv(out, 0, false, 1, &[if *b { 0xff } else { 0 }]),
        Value::Int(i) => tlv(out, 0, false, 2, &encode_int(*i as i128)),
        Value::Float(f) => tlv(out, 0, false, 12, f.to_string().as_bytes()),
        Value::String(s) => tlv(out, 0, false, 12, s.as_bytes()),
        Value::Bytes(b) => tlv(out, 0, false, 4, b),
        Value::Array(a) => {
            let mut c = Vec::new();
            for x in a {
                enc(x, &mut c)?;
            }
            tlv(out, 0, true, 16, &c);
        }
        Value::Object(m) => {
            if let Some(t) = m.get("tag").and_then(|t| t.as_str()) {
                let (class, tag) = parse_tag_name(t).ok_or_else(|| df_err(format!("Invalid ASN.1 tag: {}", t)))?;
                if let Some(b) = m.get("bytes") {
                    tlv(out, class, false, tag, &b.to_bytes());
                } else {
                    let mut c = Vec::new();
                    match m.get("value") {
                        Some(Value::Array(a)) => {
                            for x in a {
                                enc(x, &mut c)?;
                            }
                        }
                        Some(x) => enc(x, &mut c)?,
                        None => {}
                    }
                    tlv(out, class, true, tag, &c);
                }
                return Ok(());
            }
            if m.len() == 1 {
                let (k, x) = m.iter().next().unwrap();
                let s = x.to_string();
                match k.as_str() {
                    "oid" => return Ok(tlv(out, 0, false, 6, &encode_oid(&s)?)),
                    "printable" => return Ok(tlv(out, 0, false, 19, s.as_bytes())),
                    "ia5" => return Ok(tlv(out, 0, false, 22, s.as_bytes())),
                    "utcTime" => return Ok(tlv(out, 0, false, 23, s.as_bytes())),
                    "generalizedTime" => return Ok(tlv(out, 0, false, 24, s.as_bytes())),
                    "integer" => {
                        let n: i128 = s.parse().map_err(|_| df_err(format!("Invalid INTEGER: {}", s)))?;
                        return Ok(tlv(out, 0, false, 2, &encode_int(n)));
                    }
                    "set" => {
                        let mut items = Vec::new();
                        for e in x.as_array().cloned().unwrap_or_default() {
                            items.push(der_encode(&e)?);
                        }
                        items.sort(); // DER SET OF ordering
                        return Ok(tlv(out, 0, true, 17, &items.concat()));
                    }
                    _ => {}
                }
            }
            let mut c = Vec::new();
            for (_, x) in m.iter() {
                enc(x, &mut c)?;
            }
            tlv(out, 0, true, 16, &c);
        }
    }
    Ok(())
}

/// The `asn1` data format.
pub struct Asn1DataFormat {
    pub using_iterator: bool,
    /// Typed mode: a compiled ASN.1 module and the type (`clazzName`).
    pub typed: Option<(Arc<crate::asn1_module::Asn1Module>, String)>,
}

impl DataFormat for Asn1DataFormat {
    fn name(&self) -> &str {
        "asn1"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        if let Value::Bytes(_) = ex.message.body {
            return Ok(());
        }
        if let Some((m, name)) = &self.typed {
            let out = match &ex.message.body {
                Value::Array(a) if self.using_iterator => {
                    let mut out = Vec::new();
                    for x in a {
                        out.extend(m.encode(name, x)?);
                    }
                    out
                }
                Value::String(s) => m.encode(name, &cameleer_core::json::parse(s)?)?,
                other => m.encode(name, other)?,
            };
            ex.message.body = Value::Bytes(out);
            return Ok(());
        }
        let v = match &ex.message.body {
            Value::Array(a) if self.using_iterator => {
                let mut out = Vec::new();
                for x in a {
                    out.extend(match x {
                        Value::Bytes(b) => b.clone(),
                        other => der_encode(other)?,
                    });
                }
                out
            }
            other => der_encode(other)?,
        };
        ex.message.body = Value::Bytes(v);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let d = body_bytes(&ex.message.body);
        if let Some((m, name)) = &self.typed {
            ex.message.body = if self.using_iterator {
                let mut out = Vec::new();
                let mut i = 0;
                while i < d.len() {
                    let (v, n) = m.decode_prefix(name, &d[i..])?;
                    out.push(v);
                    i += n;
                }
                Value::Array(out)
            } else {
                m.decode(name, &d)?
            };
            return Ok(());
        }
        ex.message.body = if self.using_iterator { Value::Array(ber_split(&d)?.into_iter().map(Value::Bytes).collect()) } else { ber_decode(&d)? };
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let src = match (o.get(&["schema", "module", "asn1Module"]), o.get(&["moduleFile", "schemaFile", "asn1File"])) {
        (Some(s), _) => Some(s.to_string()),
        (None, Some(f)) => Some(String::from_utf8(crate::read_resource(f)?).map_err(|_| df_err("ASN.1 module file is not UTF-8"))?),
        _ => None,
    };
    let typed = match (src, o.get(&["clazzName", "className", "typeName"])) {
        (Some(src), Some(name)) => {
            let m = crate::asn1_module::Asn1Module::parse(&src)?;
            m.type_named(name)?;
            Some((Arc::new(m), name.to_string()))
        }
        (Some(_), None) => return Err(cameleer_core::error::CamelError::illegal_argument("The asn1 data format needs clazzName (the ASN.1 type) with a schema / moduleFile")),
        (None, _) => None,
    };
    Ok(Arc::new(Asn1DataFormat { using_iterator: o.bool(&["usingIterator"], false)?, typed }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cameleer_core::json;

    #[test]
    fn x509_like_structure() {
        // SEQUENCE { INTEGER 5, OID 1.2.840.113549.1.1.11, [0] { UTF8String "x" }, BOOLEAN TRUE, NULL }
        let v = json::parse(r#"[5, {"oid":"1.2.840.113549.1.1.11"}, {"tag":"[0]","value":"x"}, true, null, -129, {"set":[2,1]}]"#).unwrap();
        let d = der_encode(&v).unwrap();
        assert_eq!(
            cameleer_tls::util::hex(&d),
            "302402010506092a864886f70d01010ba0030c01780101ff05000202ff7f3106020101020102"
        );
        assert_eq!(ber_decode(&d).unwrap(), v.clone().as_array().map(|a| {
            let mut a = a.clone();
            a[6] = json::parse(r#"{"set":[1,2]}"#).unwrap();
            Value::Array(a)
        }).unwrap());
        // indefinite length BER
        let ber = [0x30, 0x80, 0x02, 0x01, 0x07, 0x04, 0x02, 0xab, 0xcd, 0x00, 0x00];
        assert_eq!(ber_decode(&ber).unwrap(), Value::Array(vec![Value::Int(7), Value::Bytes(vec![0xab, 0xcd])]));
        let stream = [ber.to_vec(), d.clone()].concat();
        assert_eq!(ber_split(&stream).unwrap(), vec![ber.to_vec(), d]);
    }
}
