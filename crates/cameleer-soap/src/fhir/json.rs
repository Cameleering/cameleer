//! A JSON tree that keeps number lexemes (FHIR decimals are significant: `1.50` ≠ `1.5`)
//! and key order, with a parser and a writer.

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    /// The number as written.
    Num(String),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    /// Member of an object.
    pub fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(m) => m.iter().find(|(x, _)| x == k).map(|(_, v)| v),
            _ => None,
        }
    }

    /// String content.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            J::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Convert to a [`Value`] (numbers become `Int` or `Float`).
    pub fn to_value(&self) -> Value {
        match self {
            J::Null => Value::Null,
            J::Bool(b) => Value::Bool(*b),
            J::Num(n) => match n.parse::<i64>() {
                Ok(i) => Value::Int(i),
                // keep the lexeme of decimals an f64 would not reproduce (`1.50`, `1e3`)
                Err(_) => match n.parse::<f64>() {
                    Ok(f) if format!("{}", f) == *n || format!("{:?}", f) == *n => Value::Float(f),
                    _ => Value::String(n.clone()),
                },
            },
            J::Str(s) => Value::String(s.clone()),
            J::Arr(a) => Value::Array(a.iter().map(J::to_value).collect()),
            J::Obj(m) => {
                let mut o = ObjectMap::new();
                for (k, v) in m {
                    o.insert(k.clone(), v.to_value());
                }
                Value::Object(o)
            }
        }
    }

    /// Convert from a [`Value`] (bytes become base64 strings).
    pub fn from_value(v: &Value) -> J {
        match v {
            Value::Null => J::Null,
            Value::Bool(b) => J::Bool(*b),
            Value::Int(i) => J::Num(i.to_string()),
            Value::Float(f) if f.is_finite() => J::Num(format!("{}", f)),
            Value::Float(_) => J::Null,
            Value::String(s) => J::Str(s.clone()),
            Value::Bytes(b) => J::Str(cameleer_tls::pem::base64_encode(b)),
            Value::Array(a) => J::Arr(a.iter().map(J::from_value).collect()),
            Value::Object(m) => J::Obj(m.iter().map(|(k, v)| (k.clone(), J::from_value(v))).collect()),
        }
    }
}

/// Parse JSON text.
pub fn parse(text: &str) -> Result<J> {
    let mut p = P { s: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("trailing characters"));
    }
    Ok(v)
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn err(&self, m: &str) -> CamelError {
        CamelError::new(super::DATA_FORMAT_EXCEPTION, format!("Invalid JSON at offset {}: {}", self.i, m))
    }
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn value(&mut self, depth: usize) -> Result<J> {
        if depth > 200 {
            return Err(self.err("nesting too deep"));
        }
        match self.s.get(self.i) {
            None => Err(self.err("unexpected end")),
            Some(b'{') => {
                self.i += 1;
                let mut m = Vec::new();
                self.ws();
                if self.eat(b'}') {
                    return Ok(J::Obj(m));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if !self.eat(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.ws();
                    let v = self.value(depth + 1)?;
                    m.push((k, v));
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b'}') {
                        return Ok(J::Obj(m));
                    }
                    return Err(self.err("expected ',' or '}'"));
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut a = Vec::new();
                self.ws();
                if self.eat(b']') {
                    return Ok(J::Arr(a));
                }
                loop {
                    self.ws();
                    a.push(self.value(depth + 1)?);
                    self.ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Ok(J::Arr(a));
                    }
                    return Err(self.err("expected ',' or ']'"));
                }
            }
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(J::Bool(true))
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(J::Bool(false))
            }
            Some(b'n') if self.s[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(J::Null)
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                self.i += 1;
                while self.i < self.s.len() && matches!(self.s[self.i], b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-') {
                    self.i += 1;
                }
                let n = std::str::from_utf8(&self.s[st..self.i]).unwrap_or("0").to_string();
                if n.parse::<f64>().is_err() {
                    return Err(self.err("invalid number"));
                }
                Ok(J::Num(n))
            }
            Some(_) => Err(self.err("unexpected character")),
        }
    }
    fn hex4(&mut self) -> Result<u32> {
        let h = self.s.get(self.i..self.i + 4).ok_or_else(|| self.err("bad escape"))?;
        let v = u32::from_str_radix(std::str::from_utf8(h).unwrap_or("x"), 16).map_err(|_| self.err("bad escape"))?;
        self.i += 4;
        Ok(v)
    }
    fn string(&mut self) -> Result<String> {
        if !self.eat(b'"') {
            return Err(self.err("expected string"));
        }
        let mut out = Vec::new();
        loop {
            let c = *self.s.get(self.i).ok_or_else(|| self.err("unterminated string"))?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.s.get(self.i).ok_or_else(|| self.err("bad escape"))?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.s[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            char::from_u32(cp).unwrap_or('\u{fffd}')
                        }
                        _ => return Err(self.err("bad escape")),
                    };
                    let mut b = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                }
                _ => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| self.err("invalid UTF-8"))
    }
}

fn esc(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Write JSON (2-space indentation when `pretty`).
pub fn write(v: &J, pretty: bool) -> String {
    let mut out = String::new();
    w(v, pretty, 0, &mut out);
    out
}

fn w(v: &J, pretty: bool, ind: usize, out: &mut String) {
    let nl = |out: &mut String, n: usize| {
        if pretty {
            out.push('\n');
            out.push_str(&"  ".repeat(n));
        }
    };
    match v {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Num(n) => out.push_str(n),
        J::Str(s) => esc(s, out),
        J::Arr(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, ind + 1);
                w(x, pretty, ind + 1, out);
            }
            nl(out, ind);
            out.push(']');
        }
        J::Obj(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                nl(out, ind + 1);
                esc(k, out);
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                w(x, pretty, ind + 1, out);
            }
            nl(out, ind);
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_lexemes() {
        let t = r#"{"a":1.50,"b":[true,null,"xé\n"],"c":{}}"#;
        let j = parse(t).unwrap();
        assert_eq!(write(&j, false), r#"{"a":1.50,"b":[true,null,"xé\n"],"c":{}}"#);
        assert!(parse("{").is_err());
        assert_eq!(j.get("a").unwrap().to_value(), Value::from("1.50"));
    }
}
