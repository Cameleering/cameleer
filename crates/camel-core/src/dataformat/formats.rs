//! Simple data formats: `json`, `string`, `hex`, `jacksonXml` and `urlencoded`.


use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::value::{ObjectMap, Value};

use super::DataFormat;

fn body_text(v: &Value) -> Result<String> {
    Ok(match v {
        Value::Bytes(b) => String::from_utf8(b.clone()).map_err(|e| CamelError::new("DataFormatException", format!("Body is not valid UTF-8: {}", e)))?,
        other => other.to_string(),
    })
}

/// The `json` data format (Jackson-like): marshal any value to JSON text, unmarshal JSON
/// text (string or bytes) to a structured value. Options: `prettyPrint`,
/// `contentTypeHeader` (default true, sets `Content-Type: application/json`),
/// `allowEmptyBody`/`unmarshalType` accepted (type ignored).
pub struct JsonDataFormat {
    pub pretty_print: bool,
    pub content_type_header: bool,
}

impl DataFormat for JsonDataFormat {
    fn name(&self) -> &str {
        "json"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let v = &ex.message.body;
        let text = if self.pretty_print { v.to_json_pretty() } else { v.to_json() };
        ex.message.body = Value::String(text);
        if self.content_type_header {
            ex.message.headers.set(crate::message::props::CONTENT_TYPE, "application/json");
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        match &ex.message.body {
            Value::String(_) | Value::Bytes(_) => {
                let t = body_text(&ex.message.body)?;
                ex.message.body = if t.trim().is_empty() { Value::Null } else { crate::json::parse(&t)? };
            }
            _ => {}
        }
        Ok(())
    }
}

/// The `string` data format: marshal to bytes / unmarshal bytes to a string using the
/// `charset` option (`UTF-8` (default), `ISO-8859-1`/`latin1`, `US-ASCII`).
pub struct StringDataFormat {
    pub charset: String,
}

fn is_latin1(cs: &str) -> bool {
    matches!(cs.to_ascii_uppercase().replace('_', "-").as_str(), "ISO-8859-1" | "LATIN1" | "ISO8859-1" | "LATIN-1")
}

impl DataFormat for StringDataFormat {
    fn name(&self) -> &str {
        "string"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let s = match &ex.message.body {
            Value::Bytes(b) => {
                ex.message.body = Value::Bytes(b.clone());
                return Ok(());
            }
            other => other.to_string(),
        };
        let bytes = if is_latin1(&self.charset) {
            s.chars().map(|c| if (c as u32) < 256 { c as u32 as u8 } else { b'?' }).collect()
        } else if self.charset.eq_ignore_ascii_case("US-ASCII") || self.charset.eq_ignore_ascii_case("ASCII") {
            s.chars().map(|c| if c.is_ascii() { c as u8 } else { b'?' }).collect()
        } else {
            s.into_bytes()
        };
        ex.message.body = Value::Bytes(bytes);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let s = match &ex.message.body {
            Value::Bytes(b) => {
                if is_latin1(&self.charset) {
                    b.iter().map(|&x| x as char).collect()
                } else {
                    String::from_utf8_lossy(b).to_string()
                }
            }
            other => other.to_string(),
        };
        ex.message.body = Value::String(s);
        Ok(())
    }
}

/// The `hex` data format: marshal bytes to a lowercase hex string, unmarshal hex to bytes.
pub struct HexDataFormat {
    pub upper_case: bool,
}

impl DataFormat for HexDataFormat {
    fn name(&self) -> &str {
        "hex"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let h = crate::language::support::to_hex(&ex.message.body.to_bytes());
        ex.message.body = Value::String(if self.upper_case { h.to_uppercase() } else { h });
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let t: String = ex.message.body.to_string().chars().filter(|c| !c.is_whitespace()).collect();
        if !t.len().is_multiple_of(2) {
            return Err(CamelError::new("DataFormatException", "Hex string has odd length"));
        }
        let mut out = Vec::with_capacity(t.len() / 2);
        for i in (0..t.len()).step_by(2) {
            out.push(u8::from_str_radix(&t[i..i + 2], 16).map_err(|_| CamelError::new("DataFormatException", format!("Invalid hex: {}", &t[i..i + 2])))?);
        }
        ex.message.body = Value::Bytes(out);
        Ok(())
    }
}

/// The `jacksonXml` data format: structured value <-> XML (see [`crate::language::xml`]).
/// Options: `rootName` (default `root`), `prettyPrint`, `contentTypeHeader`.
pub struct XmlDataFormat {
    pub root_name: String,
    pub pretty_print: bool,
    pub content_type_header: bool,
}

impl DataFormat for XmlDataFormat {
    fn name(&self) -> &str {
        "jacksonXml"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let v = match &ex.message.body {
            Value::String(_) | Value::Bytes(_) => {
                let t = body_text(&ex.message.body)?;
                match crate::json::parse(&t) {
                    Ok(j @ (Value::Object(_) | Value::Array(_))) => j,
                    _ => Value::String(t),
                }
            }
            other => other.clone(),
        };
        ex.message.body = Value::String(crate::language::xml::value_to_xml(&v, &self.root_name, self.pretty_print));
        if self.content_type_header {
            ex.message.headers.set(crate::message::props::CONTENT_TYPE, "application/xml");
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let t = body_text(&ex.message.body)?;
        ex.message.body = crate::language::xml::xml_to_value(&t)?;
        Ok(())
    }
}

/// `urlencoded` data format: object <-> `application/x-www-form-urlencoded` text.
pub struct UrlEncodedDataFormat;

impl DataFormat for UrlEncodedDataFormat {
    fn name(&self) -> &str {
        "urlencoded"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let o = match &ex.message.body {
            Value::Object(o) => o.clone(),
            Value::Null => ObjectMap::new(),
            other => return Err(CamelError::new("DataFormatException", format!("urlencoded marshal requires an object body, got {}", other.type_name()))),
        };
        let mut parts = Vec::new();
        for (k, v) in &o {
            match v {
                Value::Array(items) => {
                    for it in items {
                        parts.push(format!("{}={}", crate::util::url_encode(k), crate::util::url_encode(&it.to_string())));
                    }
                }
                other => parts.push(format!("{}={}", crate::util::url_encode(k), crate::util::url_encode(&other.to_string()))),
            }
        }
        ex.message.body = Value::String(parts.join("&"));
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let t = body_text(&ex.message.body)?;
        let mut m = ObjectMap::new();
        for pair in t.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let k = crate::util::url_decode(&k.replace('+', " "));
            let v = Value::String(crate::util::url_decode(&v.replace('+', " ")));
            match m.remove(&k) {
                None => {
                    m.insert(k, v);
                }
                Some(Value::Array(mut a)) => {
                    a.push(v);
                    m.insert(k, Value::Array(a));
                }
                Some(prev) => {
                    m.insert(k, Value::Array(vec![prev, v]));
                }
            }
        }
        ex.message.body = Value::Object(m);
        Ok(())
    }
}
