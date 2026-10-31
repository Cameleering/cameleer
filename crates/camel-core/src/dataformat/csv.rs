//! The `csv` data format (RFC 4180).
//!
//! Unmarshal produces an array of records; each record is an array of strings, or an
//! object keyed by the header (`useMaps` / `useOrderedMaps`, header from the `header`
//! option or the first line). Marshal accepts an array of records (arrays, objects or
//! scalars), a single object or a single array of scalars.
//!
//! Options: `delimiter` (default `,`), `quote` (default `"`), `quoteDisabled`,
//! `recordSeparator` (default CRLF), `header` (comma separated names), `skipHeaderRecord`,
//! `useMaps`, `useOrderedMaps`, `ignoreEmptyLines` (default true),
//! `ignoreSurroundingSpaces`/`trim`, `quoteMode` (`MINIMAL`, `ALL`, `NON_NUMERIC`, `NONE`),
//! `nullString`, `commentMarker`, `escape`.

use std::collections::BTreeMap;

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::value::Value;

use super::DataFormat;

/// CSV configuration.
#[derive(Clone, Debug)]
pub struct CsvDataFormat {
    pub delimiter: char,
    pub quote: Option<char>,
    pub escape: Option<char>,
    pub record_separator: String,
    pub header: Vec<String>,
    pub skip_header_record: bool,
    pub use_maps: bool,
    pub ignore_empty_lines: bool,
    pub trim: bool,
    pub quote_mode: String,
    pub null_string: Option<String>,
    pub comment_marker: Option<char>,
}

impl Default for CsvDataFormat {
    fn default() -> Self {
        CsvDataFormat {
            delimiter: ',',
            quote: Some('"'),
            escape: None,
            record_separator: "\r\n".into(),
            header: vec![],
            skip_header_record: false,
            use_maps: false,
            ignore_empty_lines: true,
            trim: false,
            quote_mode: "MINIMAL".into(),
            null_string: None,
            comment_marker: None,
        }
    }
}

/// Parse a single char option (`;`, `\t`, `TAB`, ...).
pub fn parse_char_option(v: &str) -> Option<char> {
    match v {
        "\\t" | "TAB" | "tab" | "\t" => Some('\t'),
        "\\n" => Some('\n'),
        "SPACE" | "space" => Some(' '),
        "" => None,
        other => other.chars().next(),
    }
}

impl CsvDataFormat {
    fn err(msg: impl Into<String>) -> CamelError {
        CamelError::new("DataFormatException", msg.into())
    }

    /// Parse CSV text into records of optional strings (None = null).
    pub fn parse(&self, text: &str) -> Result<Vec<Vec<Option<String>>>> {
        let c: Vec<char> = text.chars().collect();
        let mut records = Vec::new();
        let mut rec: Vec<Option<String>> = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut i = 0;
        let mut line_start = true;
        let mut any = false; // whether current record has content
        let mut line = 1;
        while i < c.len() {
            let ch = c[i];
            if line_start {
                if let Some(cm) = self.comment_marker {
                    if ch == cm {
                        while i < c.len() && c[i] != '\n' {
                            i += 1;
                        }
                        i += 1;
                        line += 1;
                        continue;
                    }
                }
                line_start = false;
            }
            if Some(ch) == self.quote && field.trim().is_empty() && !quoted {
                // quoted field
                quoted = true;
                field.clear();
                i += 1;
                any = true;
                loop {
                    if i >= c.len() {
                        return Err(Self::err(format!("Unterminated quoted field starting at line {}", line)));
                    }
                    let q = c[i];
                    if Some(q) == self.escape && i + 1 < c.len() {
                        field.push(c[i + 1]);
                        i += 2;
                        continue;
                    }
                    if Some(q) == self.quote {
                        if i + 1 < c.len() && Some(c[i + 1]) == self.quote {
                            field.push(q);
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    if q == '\n' {
                        line += 1;
                    }
                    field.push(q);
                    i += 1;
                }
                // skip spaces until delimiter / eol
                while i < c.len() && c[i] != self.delimiter && c[i] != '\n' && c[i] != '\r' {
                    if !c[i].is_whitespace() {
                        return Err(Self::err(format!("Invalid char between encapsulated token and delimiter at line {}", line)));
                    }
                    i += 1;
                }
                continue;
            }
            if ch == self.delimiter {
                rec.push(self.finish(&field, quoted));
                field.clear();
                quoted = false;
                any = true;
                i += 1;
                continue;
            }
            if ch == '\r' || ch == '\n' {
                if ch == '\r' && i + 1 < c.len() && c[i + 1] == '\n' {
                    i += 1;
                }
                i += 1;
                line += 1;
                if any || !field.is_empty() {
                    rec.push(self.finish(&field, quoted));
                    records.push(std::mem::take(&mut rec));
                } else if !self.ignore_empty_lines {
                    records.push(vec![Some(String::new())]);
                }
                field.clear();
                quoted = false;
                any = false;
                line_start = true;
                continue;
            }
            if Some(ch) == self.escape && i + 1 < c.len() {
                field.push(c[i + 1]);
                i += 2;
                any = true;
                continue;
            }
            field.push(ch);
            any = true;
            i += 1;
        }
        if any || !field.is_empty() {
            rec.push(self.finish(&field, quoted));
            records.push(rec);
        }
        Ok(records)
    }

    fn finish(&self, f: &str, quoted: bool) -> Option<String> {
        let v = if self.trim && !quoted { f.trim().to_string() } else { f.to_string() };
        if !quoted {
            if let Some(ns) = &self.null_string {
                if &v == ns {
                    return None;
                }
            }
        }
        Some(v)
    }

    fn needs_quote(&self, s: &str, numeric: bool) -> bool {
        let q = match self.quote {
            Some(q) => q,
            None => return false,
        };
        match self.quote_mode.to_ascii_uppercase().as_str() {
            "ALL" | "ALL_NON_NULL" => true,
            "NONE" => false,
            "NON_NUMERIC" => !numeric,
            _ => {
                s.contains(self.delimiter)
                    || s.contains(q)
                    || s.contains('\n')
                    || s.contains('\r')
                    || s.starts_with(' ')
                    || s.ends_with(' ')
                    || self.comment_marker.map(|c| s.starts_with(c)).unwrap_or(false)
            }
        }
    }

    fn write_field(&self, v: &Value, out: &mut String) {
        if v.is_null() {
            if let Some(ns) = &self.null_string {
                out.push_str(ns);
            }
            return;
        }
        let s = v.to_string();
        let numeric = matches!(v, Value::Int(_) | Value::Float(_));
        if self.needs_quote(&s, numeric) {
            let q = self.quote.unwrap();
            out.push(q);
            for ch in s.chars() {
                if ch == q {
                    out.push(q);
                }
                out.push(ch);
            }
            out.push(q);
        } else if let (None, Some(e)) = (self.quote, self.escape) {
            for ch in s.chars() {
                if ch == self.delimiter || ch == e {
                    out.push(e);
                }
                out.push(ch);
            }
        } else {
            out.push_str(&s);
        }
    }

    fn write_record(&self, fields: &[Value], out: &mut String) {
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                out.push(self.delimiter);
            }
            self.write_field(f, out);
        }
        out.push_str(&self.record_separator);
    }

    /// Marshal a value to CSV text.
    pub fn to_csv(&self, body: &Value) -> Result<String> {
        let records: Vec<Value> = match body {
            Value::Array(items) if items.iter().any(|i| matches!(i, Value::Array(_) | Value::Object(_))) => items.clone(),
            Value::Array(items) => vec![Value::Array(items.clone())],
            Value::Object(_) => vec![body.clone()],
            Value::Null => vec![],
            Value::String(_) | Value::Bytes(_) => {
                // JSON text containing records? otherwise a single field
                match crate::json::parse(body.as_str().unwrap_or("")) {
                    Ok(v @ (Value::Array(_) | Value::Object(_))) => return self.to_csv(&v),
                    _ => vec![Value::Array(vec![body.clone()])],
                }
            }
            other => vec![Value::Array(vec![other.clone()])],
        };
        let mut out = String::new();
        let mut columns: Vec<String> = self.header.clone();
        let has_objects = records.iter().any(|r| matches!(r, Value::Object(_)));
        if columns.is_empty() && has_objects {
            for r in &records {
                if let Value::Object(o) = r {
                    for k in o.keys() {
                        if !columns.contains(k) {
                            columns.push(k.clone());
                        }
                    }
                }
            }
        }
        let write_header = !self.skip_header_record && !columns.is_empty() && (!self.header.is_empty() || (self.use_maps && has_objects));
        if write_header {
            let h: Vec<Value> = columns.iter().map(|c| Value::String(c.clone())).collect();
            self.write_record(&h, &mut out);
        }
        for r in &records {
            match r {
                Value::Object(o) => {
                    let fields: Vec<Value> = columns.iter().map(|c| o.get(c).cloned().unwrap_or(Value::Null)).collect();
                    self.write_record(&fields, &mut out);
                }
                Value::Array(a) => self.write_record(a, &mut out),
                other => self.write_record(std::slice::from_ref(other), &mut out),
            }
        }
        Ok(out)
    }

    /// Unmarshal CSV text to a value.
    pub fn from_csv(&self, text: &str) -> Result<Value> {
        let mut records = self.parse(text)?;
        let to_v = |f: Option<String>| f.map(Value::String).unwrap_or(Value::Null);
        if self.use_maps {
            let header: Vec<String> = if !self.header.is_empty() {
                if self.skip_header_record && !records.is_empty() {
                    records.remove(0);
                }
                self.header.clone()
            } else {
                if records.is_empty() {
                    return Ok(Value::Array(vec![]));
                }
                records.remove(0).into_iter().map(|f| f.unwrap_or_default()).collect()
            };
            let rows = records
                .into_iter()
                .map(|r| {
                    let mut m = BTreeMap::new();
                    for (i, f) in r.into_iter().enumerate() {
                        let key = header.get(i).cloned().unwrap_or_else(|| format!("{}", i));
                        m.insert(key, to_v(f));
                    }
                    Value::Object(m)
                })
                .collect();
            return Ok(Value::Array(rows));
        }
        if self.skip_header_record && !records.is_empty() {
            records.remove(0);
        }
        Ok(Value::Array(records.into_iter().map(|r| Value::Array(r.into_iter().map(to_v).collect())).collect()))
    }
}

impl DataFormat for CsvDataFormat {
    fn name(&self) -> &str {
        "csv"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        ex.message.body = Value::String(self.to_csv(&ex.message.body)?);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = match &ex.message.body {
            Value::Null => String::new(),
            Value::Bytes(b) => String::from_utf8_lossy(b).to_string(),
            other => other.to_string(),
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text).to_string();
        ex.message.body = self.from_csv(&text)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rfc4180() {
        let f = CsvDataFormat::default();
        let r = f.parse("a,b,c\r\n1,\"x,y\",\"he said \"\"hi\"\"\"\n\n2,,\"multi\nline\"\n").unwrap();
        assert_eq!(r.len(), 3);
        assert_eq!(r[1][1].as_deref(), Some("x,y"));
        assert_eq!(r[1][2].as_deref(), Some("he said \"hi\""));
        assert_eq!(r[2][1].as_deref(), Some(""));
        assert_eq!(r[2][2].as_deref(), Some("multi\nline"));
        assert!(f.parse("\"unterminated").is_err());
    }

    #[test]
    fn marshal_quotes() {
        let f = CsvDataFormat::default();
        let v = Value::from(vec![Value::from(vec!["a", "b,c", "d\"e"]), Value::from(vec![Value::Int(1), Value::Null, Value::from("x")])]);
        assert_eq!(f.to_csv(&v).unwrap(), "a,\"b,c\",\"d\"\"e\"\r\n1,,x\r\n");
    }
}
