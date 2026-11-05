//! uniVocity parsers data formats: `univocityCsv`, `univocityTsv`, `univocityFixed`.
//!
//! `univocityCsv` is an alias of cameleer-core's `csv` data format with the option names
//! translated (`delimiter`, `quote`, `quoteEscape`, `lineSeparator`, `headers`, `asMap`,
//! `headerExtractionEnabled`, `nullValue`, `comment`, `skipEmptyLines`,
//! `ignoreLeadingWhitespaces`/`ignoreTrailingWhitespaces`). `univocityTsv` (tab separated,
//! `\t`/`\n`/`\r`/`\\` escapes) and `univocityFixed` (`fieldLengths=5,10,3`, `padding`; characters after
//! the last field are ignored) are implemented here with the same row options:
//! unmarshal gives an array of rows (arrays of strings) or, with `asMap=true`, objects
//! keyed by `headers` (or the first line with `headerExtractionEnabled=true`); marshal
//! accepts arrays of arrays or objects (`headers` select and order the columns).
//!
//! Common options of all three: `numberOfRecordsToRead` (stop after n records), `emptyValue`
//! (value of empty fields; for CSV the empty strings of the parsed rows), `headersDisabled`
//! (no header names: `asMap` keys become `column1`...; the header line is still skipped
//! with `headerExtractionEnabled`), `lazyLoad` (accepted: the rows are returned as a list),
//! `normalizedLineSeparator`. TSV: `escapeChar` (default `\`). Fixed width parses the input
//! as a character stream like uniVocity: a record is `sum(fieldLengths)` characters
//! followed by an optional newline (a newline always ends the current field);
//! `skipTrailingCharsUntilNewline=true` drops the rest of
//! the line after the last field, `recordEndsOnNewline=true` ends a record early at a
//! newline (missing fields are null).

use std::collections::BTreeMap;
use std::sync::Arc;

use cameleer_core::dataformat::{create_data_format, DataFormat};
use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::ObjectMap;
use cameleer_core::Value;

use crate::{body_text, unescape_opt, Opts};

pub(crate) fn create_csv(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let mut m = BTreeMap::new();
    let mut set = |k: &str, v: Option<&str>| {
        if let Some(v) = v {
            m.insert(k.to_string(), v.to_string());
        }
    };
    set("delimiter", o.get(&["delimiter"]));
    set("quote", o.get(&["quote"]));
    set("escape", o.get(&["quoteEscape"]));
    set("recordSeparator", o.get(&["lineSeparator", "normalizedLineSeparator"]));
    set("nullString", o.get(&["nullValue"]));
    set("commentMarker", o.get(&["comment"]));
    set("header", o.get(&["headers"]));
    set("ignoreEmptyLines", o.get(&["skipEmptyLines"]));
    if o.bool(&["ignoreLeadingWhitespaces"], false)? || o.bool(&["ignoreTrailingWhitespaces"], false)? {
        m.insert("trim".into(), "true".into());
    }
    let as_map = o.bool(&["asMap"], false)?;
    if as_map {
        m.insert("useMaps".into(), "true".into());
    }
    if o.bool(&["headerExtractionEnabled"], false)? && !as_map {
        m.insert("skipHeaderRecord".into(), "true".into());
    }
    if o.bool(&["quoteAllFields"], false)? {
        m.insert("quoteMode".into(), "ALL".into());
    }
    let inner = create_data_format("csv", &m)?;
    let limit = o.get(&["numberOfRecordsToRead"]).map(|n| n.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid numberOfRecordsToRead: {}", n)))).transpose()?.filter(|n| *n >= 0).map(|n| n as usize);
    let empty_value = o.get(&["emptyValue"]).map(String::from);
    let headers_disabled = o.bool(&["headersDisabled"], false)?;
    if limit.is_none() && empty_value.is_none() && !headers_disabled {
        return Ok(inner);
    }
    Ok(Arc::new(CsvWrapper { inner, limit, empty_value }))
}

/// `univocityCsv` options on top of the core `csv` data format.
struct CsvWrapper {
    inner: Arc<dyn DataFormat>,
    limit: Option<usize>,
    empty_value: Option<String>,
}

fn replace_empty(v: &mut Value, e: &str) {
    match v {
        Value::String(s) if s.is_empty() => *v = Value::String(e.to_string()),
        Value::Array(a) => a.iter_mut().for_each(|x| replace_empty(x, e)),
        Value::Object(o) => o.values_mut().for_each(|x| replace_empty(x, e)),
        _ => {}
    }
}

impl DataFormat for CsvWrapper {
    fn name(&self) -> &str {
        "univocityCsv"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        self.inner.marshal(ex)
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        self.inner.unmarshal(ex)?;
        if let Value::Array(rows) = &mut ex.message.body {
            if let Some(n) = self.limit {
                rows.truncate(n);
            }
            if let Some(e) = &self.empty_value {
                rows.iter_mut().for_each(|r| replace_empty(r, e));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Tsv,
    Fixed,
}

/// `univocityTsv` / `univocityFixed`.
pub struct RowsDataFormat {
    flavor: Flavor,
    headers: Vec<String>,
    header_extraction: bool,
    as_map: bool,
    line_separator: String,
    skip_empty_lines: bool,
    null_value: Option<String>,
    empty_value: Option<String>,
    comment: Option<char>,
    field_lengths: Vec<usize>,
    padding: char,
    trim: bool,
    limit: Option<usize>,
    headers_disabled: bool,
    escape: char,
    skip_trailing: bool,
    ends_on_newline: bool,
}

fn tsv_unescape(s: &str, esc: char) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == esc {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some(o) if o == esc => out.push(esc),
                Some(o) => {
                    out.push(esc);
                    out.push(o);
                }
                None => out.push(esc),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn tsv_escape(s: &str, esc: char) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\t' => {
                out.push(esc);
                out.push('t')
            }
            '\n' => {
                out.push(esc);
                out.push('n')
            }
            '\r' => {
                out.push(esc);
                out.push('r')
            }
            c if c == esc => {
                out.push(esc);
                out.push(esc)
            }
            c => out.push(c),
        }
    }
    out
}

impl RowsDataFormat {
    fn parse_row(&self, line: &str) -> Vec<String> {
        match self.flavor {
            Flavor::Tsv => line.split('\t').map(|c| tsv_unescape(c, self.escape)).collect(),
            Flavor::Fixed => {
                let chars: Vec<char> = line.chars().collect();
                let mut pos = 0;
                let mut out = Vec::new();
                for l in &self.field_lengths {
                    let end = (pos + l).min(chars.len());
                    let s: String = chars.get(pos..end).map(|c| c.iter().collect()).unwrap_or_default();
                    out.push(s.trim_matches(self.padding).to_string());
                    pos += l;
                }
                out
            }
        }
    }

    fn value(&self, s: String) -> Value {
        let s = if self.trim { s.trim().to_string() } else { s };
        if s.is_empty() {
            return match (&self.empty_value, &self.null_value) {
                (Some(e), _) => Value::String(e.clone()),
                (None, Some(n)) => Value::String(n.clone()),
                _ => Value::Null,
            };
        }
        Value::String(s)
    }

    fn format_row(&self, cells: Vec<String>) -> Result<String> {
        match self.flavor {
            Flavor::Tsv => Ok(cells.iter().map(|c| tsv_escape(c, self.escape)).collect::<Vec<_>>().join("\t")),
            Flavor::Fixed => {
                let mut out = String::new();
                for (i, l) in self.field_lengths.iter().enumerate() {
                    let c = cells.get(i).cloned().unwrap_or_default();
                    let n = c.chars().count();
                    if n >= *l {
                        out.extend(c.chars().take(*l));
                    } else {
                        out.push_str(&c);
                        out.extend(std::iter::repeat_n(self.padding, l - n));
                    }
                }
                Ok(out)
            }
        }
    }
}

impl RowsDataFormat {
    /// Fixed-width records from a character stream (see the module docs).
    fn fixed_records(&self, text: &str) -> Vec<Vec<String>> {
        let chars: Vec<char> = text.chars().collect();
        let mut i = 0;
        let mut out = Vec::new();
        let at_newline = |i: usize| chars.get(i) == Some(&'\n') || (chars.get(i) == Some(&'\r'));
        let skip_newline = |i: &mut usize| {
            if chars.get(*i) == Some(&'\r') {
                *i += 1;
            }
            if chars.get(*i) == Some(&'\n') {
                *i += 1;
            }
        };
        while i < chars.len() {
            // blank lines and comments
            if at_newline(i) {
                if !self.skip_empty_lines {
                    out.push(self.field_lengths.iter().map(|_| String::new()).collect());
                }
                skip_newline(&mut i);
                continue;
            }
            if self.comment.is_some_and(|c| chars[i] == c) {
                while i < chars.len() && !at_newline(i) {
                    i += 1;
                }
                skip_newline(&mut i);
                continue;
            }
            let mut cells = Vec::new();
            for l in &self.field_lengths {
                let mut cell = String::new();
                let mut n = 0;
                while n < *l && i < chars.len() && !at_newline(i) {
                    cell.push(chars[i]);
                    i += 1;
                    n += 1;
                }
                cells.push(cell.trim_matches(self.padding).to_string());
            }
            if self.skip_trailing || self.ends_on_newline {
                while i < chars.len() && !at_newline(i) {
                    i += 1;
                }
            }
            skip_newline(&mut i);
            out.push(cells);
        }
        out
    }
}

impl DataFormat for RowsDataFormat {
    fn name(&self) -> &str {
        match self.flavor {
            Flavor::Tsv => "univocityTsv",
            Flavor::Fixed => "univocityFixed",
        }
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let rows = match &ex.message.body {
            Value::Array(a) => a.clone(),
            Value::Object(_) => vec![ex.message.body.clone()],
            other => return Err(CamelError::new("DataFormatException", format!("Cannot marshal a {} as rows", other.type_name()))),
        };
        let mut lines = Vec::new();
        for r in &rows {
            let cells: Vec<String> = match r {
                Value::Array(a) => a.iter().map(|v| if v.is_null() { self.null_value.clone().unwrap_or_default() } else { v.to_string() }).collect(),
                Value::Object(o) => {
                    let keys: Vec<String> = if self.headers.is_empty() { o.keys().cloned().collect() } else { self.headers.clone() };
                    keys.iter().map(|k| o.get(k).filter(|v| !v.is_null()).map(|v| v.to_string()).unwrap_or_else(|| self.null_value.clone().unwrap_or_default())).collect()
                }
                other => vec![other.to_string()],
            };
            lines.push(self.format_row(cells)?);
        }
        let mut out = lines.join(&self.line_separator);
        if !lines.is_empty() {
            out.push_str(&self.line_separator);
        }
        ex.message.body = Value::String(out);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = body_text(&ex.message.body)?;
        let mut headers = if self.headers_disabled { Vec::new() } else { self.headers.clone() };
        let mut out = Vec::new();
        let mut first = true;
        let records: Vec<Vec<String>> = match self.flavor {
            Flavor::Tsv => text
                .split('\n')
                .map(|l| l.strip_suffix('\r').unwrap_or(l))
                .filter(|l| !(l.is_empty() && self.skip_empty_lines))
                .filter(|l| self.comment.is_none_or(|c| !l.starts_with(c)))
                .map(|l| self.parse_row(l))
                .collect(),
            Flavor::Fixed => self.fixed_records(&text),
        };
        for cells in records {
            if self.limit.is_some_and(|n| out.len() >= n) {
                break;
            }
            if first && self.header_extraction {
                first = false;
                if headers.is_empty() && !self.headers_disabled {
                    headers = cells;
                }
                continue;
            }
            first = false;
            if self.as_map {
                let mut m = ObjectMap::new();
                for (i, c) in cells.into_iter().enumerate() {
                    let k = headers.get(i).cloned().unwrap_or_else(|| format!("column{}", i + 1));
                    m.insert(k, self.value(c));
                }
                out.push(Value::Object(m));
            } else {
                out.push(Value::Array(cells.into_iter().map(|c| self.value(c)).collect()));
            }
        }
        ex.message.body = Value::Array(out);
        Ok(())
    }
}

fn rows(o: &Opts, flavor: Flavor) -> Result<Arc<dyn DataFormat>> {
    let list = |k: &str| -> Vec<String> { o.get(&[k]).map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default() };
    let field_lengths: Vec<usize> = list("fieldLengths").iter().map(|x| x.parse().map_err(|_| CamelError::illegal_argument(format!("Invalid field length: {}", x)))).collect::<Result<_>>()?;
    if flavor == Flavor::Fixed && field_lengths.is_empty() {
        return Err(CamelError::illegal_argument("The fieldLengths must have been defined in order to use the fixed-width format."));
    }
    Ok(Arc::new(RowsDataFormat {
        flavor,
        headers: list("headers"),
        header_extraction: o.bool(&["headerExtractionEnabled"], false)?,
        as_map: o.bool(&["asMap"], false)?,
        line_separator: o.get(&["lineSeparator"]).map(unescape_opt).unwrap_or_else(|| "\n".into()),
        skip_empty_lines: o.bool(&["skipEmptyLines"], true)?,
        null_value: o.get(&["nullValue"]).map(String::from),
        empty_value: o.get(&["emptyValue"]).map(String::from),
        comment: o.get(&["comment"]).and_then(|c| c.chars().next()),
        field_lengths,
        padding: o.get(&["padding"]).and_then(|c| unescape_opt(c).chars().next()).unwrap_or(' '),
        trim: o.bool(&["ignoreLeadingWhitespaces"], false)? || o.bool(&["ignoreTrailingWhitespaces"], false)?,
        limit: o.get(&["numberOfRecordsToRead"]).map(|n| n.trim().parse::<i64>().map_err(|_| CamelError::illegal_argument(format!("Invalid numberOfRecordsToRead: {}", n)))).transpose()?.filter(|n| *n >= 0).map(|n| n as usize),
        headers_disabled: o.bool(&["headersDisabled"], false)?,
        escape: o.get(&["escapeChar"]).and_then(|c| unescape_opt(c).chars().next()).unwrap_or('\\'),
        skip_trailing: o.bool(&["skipTrailingCharsUntilNewline"], false)?,
        ends_on_newline: o.bool(&["recordEndsOnNewline"], false)?,
    }))
}

pub(crate) fn create_tsv(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    rows(o, Flavor::Tsv)
}

pub(crate) fn create_fixed(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    rows(o, Flavor::Fixed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn df(f: fn(&Opts) -> Result<Arc<dyn DataFormat>>, opts: &[(&str, &str)]) -> Arc<dyn DataFormat> {
        f(&Opts(opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())).unwrap()
    }

    fn unmarshal(d: &Arc<dyn DataFormat>, text: &str) -> Value {
        let mut ex = Exchange::new(None);
        ex.message.body = Value::from(text);
        d.unmarshal(&mut ex).unwrap();
        ex.message.body
    }

    #[test]
    fn fixed_width_stream_options() {
        // records without newlines: a record is sum(fieldLengths) characters
        let d = df(create_fixed, &[("fieldLengths", "2,3")]);
        assert_eq!(unmarshal(&d, "abcdeFGHIJ").to_json(), r#"[["ab","cde"],["FG","HIJ"]]"#);
        // trailing characters: by default they start the next record, skipTrailingCharsUntilNewline drops them
        assert_eq!(unmarshal(&d, "abcdeXX\nfghij\n").to_json(), r#"[["ab","cde"],["XX",null],["fg","hij"]]"#);
        let d = df(create_fixed, &[("fieldLengths", "2,3"), ("skipTrailingCharsUntilNewline", "true")]);
        assert_eq!(unmarshal(&d, "abcdeXX\nfghij\n").to_json(), r#"[["ab","cde"],["fg","hij"]]"#);
        // short lines end the record with recordEndsOnNewline
        let d = df(create_fixed, &[("fieldLengths", "2,3"), ("recordEndsOnNewline", "true"), ("emptyValue", "-")]);
        assert_eq!(unmarshal(&d, "ab\ncdefg\n").to_json(), r#"[["ab","-"],["cd","efg"]]"#);
        let d = df(create_fixed, &[("fieldLengths", "2,3"), ("numberOfRecordsToRead", "1"), ("asMap", "true"), ("headers", "a,b"), ("headersDisabled", "true")]);
        assert_eq!(unmarshal(&d, "abcde\nfghij\n").to_json(), r#"[{"column1":"ab","column2":"cde"}]"#);
    }

    #[test]
    fn tsv_escape_char_and_csv_options() {
        let d = df(create_tsv, &[("escapeChar", "|")]);
        assert_eq!(unmarshal(&d, "a|tb\tc||d\n").to_json(), r#"[["a\tb","c|d"]]"#);
        let mut ex = Exchange::new(None);
        ex.message.body = Value::Array(vec![Value::Array(vec![Value::from("x\ty"), Value::from("z")])]);
        d.marshal(&mut ex).unwrap();
        assert_eq!(ex.message.body, Value::from("x|ty\tz\n"));
        let c = df(create_csv, &[("numberOfRecordsToRead", "2"), ("emptyValue", "EMPTY")]);
        assert_eq!(unmarshal(&c, "a,,c\nd,e,f\ng,h,i\n").to_json(), r#"[["a","EMPTY","c"],["d","e","f"]]"#);
    }
}
