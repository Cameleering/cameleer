//! JSON parsing (serialization lives on [`Value::to_json`]).


use crate::error::{CamelError, Result};
use crate::value::{ObjectMap, Value};

/// Parse a JSON document into a [`Value`].
pub fn parse(text: &str) -> Result<Value> {
    let mut p = Parser { s: text.as_bytes(), i: 0, text };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i != p.s.len() {
        return Err(p.err("Unexpected trailing characters"));
    }
    Ok(v)
}

/// Serialize a value to compact JSON.
pub fn to_string(v: &Value) -> String {
    v.to_json()
}

/// Serialize a value to pretty JSON.
pub fn to_string_pretty(v: &Value) -> String {
    v.to_json_pretty()
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    text: &'a str,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> CamelError {
        let (mut line, mut col) = (1, 1);
        for &b in &self.s[..self.i.min(self.s.len())] {
            if b == b'\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        let _ = self.text;
        CamelError::new(crate::error::kinds::PARSE, format!("Invalid JSON: {} at line {} column {}", msg, line, col))
    }

    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn expect_lit(&mut self, lit: &str, v: Value) -> Result<Value> {
        if self.s[self.i..].starts_with(lit.as_bytes()) {
            self.i += lit.len();
            Ok(v)
        } else {
            Err(self.err("Unexpected token"))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Value> {
        if depth > 512 {
            return Err(self.err("Nesting too deep"));
        }
        match self.peek() {
            None => Err(self.err("Unexpected end of input")),
            Some(b'{') => {
                self.i += 1;
                // small objects are collected first and wrapped once (duplicate keys: the
                // last value wins at the first position, as with `ObjectMap::insert`)
                let mut small: Vec<(String, Value)> = Vec::new();
                let mut map: Option<ObjectMap> = None;
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                    return Ok(Value::Object(ObjectMap::new()));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("Expected string key"));
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(self.err("Expected ':'"));
                    }
                    self.i += 1;
                    self.ws();
                    let v = self.value(depth + 1)?;
                    match &mut map {
                        Some(m) => {
                            m.insert(k, v);
                        }
                        None => match small.iter().position(|(sk, _)| *sk == k) {
                            Some(i) => small[i].1 = v,
                            None if small.len() < 16 => small.push((k, v)),
                            None => {
                                let mut m = ObjectMap::from_distinct(std::mem::take(&mut small));
                                m.insert(k, v);
                                map = Some(m);
                            }
                        },
                    }
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Value::Object(map.unwrap_or_else(|| ObjectMap::from_distinct(small))));
                        }
                        _ => return Err(self.err("Expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut arr = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.i += 1;
                    return Ok(Value::Array(arr));
                }
                loop {
                    self.ws();
                    arr.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Value::Array(arr));
                        }
                        _ => return Err(self.err("Expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Value::String(self.string()?)),
            Some(b't') => self.expect_lit("true", Value::Bool(true)),
            Some(b'f') => self.expect_lit("false", Value::Bool(false)),
            Some(b'n') => self.expect_lit("null", Value::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => Err(self.err("Unexpected character")),
        }
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let mut is_float = false;
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' => self.i += 1,
                b'.' | b'e' | b'E' | b'+' | b'-' => {
                    is_float = true;
                    self.i += 1
                }
                _ => break,
            }
        }
        // the input is a `&str` and a number is ASCII
        let t = &self.text[start..self.i];
        if !is_float {
            if let Ok(i) = t.parse::<i64>() {
                return Ok(Value::Int(i));
            }
        }
        t.parse::<f64>().map(Value::Float).map_err(|_| self.err("Invalid number"))
    }

    fn string(&mut self) -> Result<String> {
        self.i += 1; // opening quote
        // usual case: no escapes, one exact allocation (the input is a `&str`, and quotes
        // and backslashes are ASCII, so the slice is valid UTF-8)
        let start = self.i;
        let mut end = start;
        while end < self.s.len() && self.s[end] != b'"' && self.s[end] != b'\\' {
            end += 1;
        }
        if end < self.s.len() && self.s[end] == b'"' {
            self.i = end + 1;
            return Ok(self.text[start..end].to_string());
        }
        let mut out = String::new();
        loop {
            let start = self.i;
            while self.i < self.s.len() && self.s[self.i] != b'"' && self.s[self.i] != b'\\' {
                self.i += 1;
            }
            out.push_str(std::str::from_utf8(&self.s[start..self.i]).map_err(|_| self.err("Invalid UTF-8"))?);
            match self.peek() {
                None => return Err(self.err("Unterminated string")),
                Some(b'"') => {
                    self.i += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.i += 1;
                    let c = self.peek().ok_or_else(|| self.err("Unterminated escape"))?;
                    self.i += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{08}'),
                        b'f' => out.push('\u{0c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.s[self.i..].starts_with(b"\\u") {
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    let c = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                                    out.push(char::from_u32(c).unwrap_or('\u{FFFD}'));
                                } else {
                                    out.push('\u{FFFD}');
                                }
                            } else {
                                out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return Err(self.err("Invalid escape")),
                    }
                }
                _ => unreachable!(),
            }
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        if self.i + 4 > self.s.len() {
            return Err(self.err("Invalid unicode escape"));
        }
        let h = std::str::from_utf8(&self.s[self.i..self.i + 4]).map_err(|_| self.err("Invalid unicode escape"))?;
        let v = u32::from_str_radix(h, 16).map_err(|_| self.err("Invalid unicode escape"))?;
        self.i += 4;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let src = r#"{"a": [1, 2.5, "x\"y", true, null], "b": {"c": "é"}}"#;
        let v = parse(src).unwrap();
        assert_eq!(v.get("a").unwrap().as_array().unwrap().len(), 5);
        assert_eq!(v.get("b").unwrap().get("c").unwrap().as_str().unwrap(), "é");
        let again = parse(&v.to_json()).unwrap();
        assert_eq!(v, again);
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("[1,2] x").is_err());
    }
}
