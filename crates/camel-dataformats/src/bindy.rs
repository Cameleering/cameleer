//! Bindy data formats: `bindyCsv`, `bindyFixed` and `bindyKvp` (Camel's annotation driven
//! record binding). Rust has no annotations, so record layouts are *descriptors*: built with
//! [`BindyDescriptor`] / [`BindyField`] in Rust, or written in YAML/JSON.
//!
//! # Descriptor format
//!
//! ```yaml
//! className: com.acme.Order      # informational name (what classType refers to)
//! # --- CSV (@CsvRecord)
//! separator: ","                 # default ","  (escapes like "\t" and "\u0001" allowed)
//! skipFirstLine: true            # default false
//! generateHeaderColumns: true    # default false (marshal writes a header line)
//! quote: "\""                    # default "\""
//! quoting: false                 # quote every value on marshal (default false)
//! removeQuotes: true             # default true
//! crlf: WINDOWS                  # WINDOWS (default), UNIX, MAC or a literal like "\n"
//! endWithLineBreak: true         # default true
//! # --- fixed length (@FixedLengthRecord)
//! length: 30                     # expected record length (0 = no check)
//! paddingChar: " "
//! ignoreTrailingChars: false
//! ignoreMissingChars: false
//! skipHeader / skipFooter: false # ignore the first / last line
//! # --- key value pairs (@Message)
//! keyValuePairSeparator: "="
//! pairSeparator: "\u0001"
//! fields:                        # @DataField / @KeyValuePairField
//!   - name: orderNr
//!     pos: 1                     # csv: column (1 based); fixed: start position (1 based)
//!                                # (or the order when no field has an explicit position)
//!     type: int                  # string (default), int, long, double, float, decimal,
//!                                # boolean, char, date
//!     pattern: dd-MM-yyyy        # date pattern (yyyy yy MM dd HH mm ss SSS) or decimal pattern
//!     precision: 2               # decimals for double/decimal on marshal
//!     trim: true
//!     defaultValue: "0"
//!     required: true
//!     columnName: Order Nr       # header column (default: name)
//!     length: 10                 # fixed: width
//!     align: R                   # fixed: R (default) or L
//!     paddingChar: "0"           # fixed: field padding (default: record paddingChar)
//!     delimiter: ";"             # fixed: delimited field (no length)
//!     clip: false                # fixed: truncate too long values instead of failing
//!     tag: 8                     # kvp: key of the pair
//! ```
//!
//! # Options
//! * `classType` (`className`): a descriptor bound in the registry ([`bind_descriptor`]) or
//!   a registry `String` with descriptor text, or a `.yaml`/`.yml`/`.json` file path.
//! * `descriptor`: inline descriptor text; `descriptorFile`: file path.
//! * `type` (for the generic `bindy` name): `Csv` (default), `Fixed`, `KeyValue`.
//! * `unwrapSingleInstance` (default true): a single record unmarshals to an object instead
//!   of an array with one object.
//! * `allowEmptyStream` (default false): an empty body unmarshals to an empty array.
//! * `locale` accepted and ignored.
//!
//! Unmarshal returns an array of objects (field name -> typed value; dates as ISO-8601
//! strings); marshal accepts an array of objects or a single object.

use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::value::ObjectMap;
use camel_core::Value;

use crate::{body_text, df_err, unescape_opt, Opts};

fn bindy_err(msg: impl Into<String>) -> CamelError {
    CamelError::new("IllegalArgumentException", msg)
}

/// Record kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Csv,
    Fixed,
    Kvp,
}

/// Field type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    String,
    Int,
    Float,
    Decimal,
    Boolean,
    Char,
    Date,
}

/// A field (`@DataField` / `@KeyValuePairField`).
#[derive(Clone, Debug)]
pub struct BindyField {
    pub name: String,
    pub pos: usize,
    pub field_type: FieldType,
    pub pattern: Option<String>,
    pub precision: Option<usize>,
    pub trim: bool,
    pub default_value: Option<String>,
    pub required: bool,
    pub column_name: Option<String>,
    pub length: usize,
    pub align_left: bool,
    pub padding_char: Option<char>,
    pub delimiter: Option<String>,
    pub clip: bool,
    pub tag: Option<String>,
}

impl BindyField {
    pub fn new(name: impl Into<String>, pos: usize) -> BindyField {
        BindyField {
            name: name.into(),
            pos,
            field_type: FieldType::String,
            pattern: None,
            precision: None,
            trim: false,
            default_value: None,
            required: false,
            column_name: None,
            length: 0,
            align_left: false,
            padding_char: None,
            delimiter: None,
            clip: false,
            tag: None,
        }
    }
    pub fn field_type(mut self, t: FieldType) -> Self {
        self.field_type = t;
        self
    }
    pub fn pattern(mut self, p: &str) -> Self {
        self.pattern = Some(p.into());
        self
    }
    pub fn precision(mut self, p: usize) -> Self {
        self.precision = Some(p);
        self
    }
    pub fn trim(mut self, b: bool) -> Self {
        self.trim = b;
        self
    }
    pub fn default_value(mut self, v: &str) -> Self {
        self.default_value = Some(v.into());
        self
    }
    pub fn required(mut self, b: bool) -> Self {
        self.required = b;
        self
    }
    pub fn column_name(mut self, c: &str) -> Self {
        self.column_name = Some(c.into());
        self
    }
    pub fn length(mut self, l: usize) -> Self {
        self.length = l;
        self
    }
    pub fn align(mut self, a: &str) -> Self {
        self.align_left = a.eq_ignore_ascii_case("L");
        self
    }
    pub fn padding_char(mut self, c: char) -> Self {
        self.padding_char = Some(c);
        self
    }
    pub fn delimiter(mut self, d: &str) -> Self {
        self.delimiter = Some(d.into());
        self
    }
    pub fn clip(mut self, b: bool) -> Self {
        self.clip = b;
        self
    }
    pub fn tag(mut self, t: impl ToString) -> Self {
        self.tag = Some(t.to_string());
        self
    }
}

/// A record layout.
#[derive(Clone, Debug)]
pub struct BindyDescriptor {
    pub kind: Kind,
    pub class_name: Option<String>,
    pub separator: String,
    pub skip_first_line: bool,
    pub generate_header_columns: bool,
    pub quote: Option<char>,
    pub quoting: bool,
    pub remove_quotes: bool,
    pub crlf: String,
    pub end_with_line_break: bool,
    pub length: usize,
    pub padding_char: char,
    pub ignore_trailing_chars: bool,
    pub ignore_missing_chars: bool,
    pub skip_header: bool,
    pub skip_footer: bool,
    pub key_value_pair_separator: String,
    pub pair_separator: String,
    pub fields: Vec<BindyField>,
}

impl BindyDescriptor {
    fn new(kind: Kind) -> BindyDescriptor {
        BindyDescriptor {
            kind,
            class_name: None,
            separator: ",".into(),
            skip_first_line: false,
            generate_header_columns: false,
            quote: Some('"'),
            quoting: false,
            remove_quotes: true,
            crlf: "\r\n".into(),
            end_with_line_break: true,
            length: 0,
            padding_char: ' ',
            ignore_trailing_chars: false,
            ignore_missing_chars: false,
            skip_header: false,
            skip_footer: false,
            key_value_pair_separator: "=".into(),
            pair_separator: "\u{1}".into(),
            fields: Vec::new(),
        }
    }
    pub fn csv() -> Self {
        Self::new(Kind::Csv)
    }
    pub fn fixed() -> Self {
        Self::new(Kind::Fixed)
    }
    pub fn kvp() -> Self {
        Self::new(Kind::Kvp)
    }
    pub fn class_name(mut self, n: &str) -> Self {
        self.class_name = Some(n.into());
        self
    }
    pub fn separator(mut self, s: &str) -> Self {
        self.separator = unescape_opt(s);
        self
    }
    pub fn skip_first_line(mut self, b: bool) -> Self {
        self.skip_first_line = b;
        self
    }
    pub fn generate_header_columns(mut self, b: bool) -> Self {
        self.generate_header_columns = b;
        self
    }
    pub fn quoting(mut self, b: bool) -> Self {
        self.quoting = b;
        self
    }
    pub fn crlf(mut self, c: &str) -> Self {
        self.crlf = crlf(c);
        self
    }
    pub fn length(mut self, l: usize) -> Self {
        self.length = l;
        self
    }
    pub fn padding_char(mut self, c: char) -> Self {
        self.padding_char = c;
        self
    }
    pub fn key_value_pair_separator(mut self, s: &str) -> Self {
        self.key_value_pair_separator = unescape_opt(s);
        self
    }
    pub fn pair_separator(mut self, s: &str) -> Self {
        self.pair_separator = unescape_opt(s);
        self
    }
    pub fn field(mut self, f: BindyField) -> Self {
        self.fields.push(f);
        self
    }

    /// Parse a YAML or JSON descriptor.
    pub fn parse(text: &str, kind: Kind) -> Result<BindyDescriptor> {
        let v = camel_yaml::parse_yaml(text).map_err(|e| bindy_err(format!("Invalid bindy descriptor: {}", e)))?;
        Self::from_value(&v, kind)
    }

    /// Build a descriptor from a parsed YAML/JSON value.
    pub fn from_value(v: &Value, kind: Kind) -> Result<BindyDescriptor> {
        let o = v.as_object().ok_or_else(|| bindy_err("A bindy descriptor must be an object"))?;
        let mut d = BindyDescriptor::new(kind);
        let s = |k: &str| o.get(k).map(|v| v.to_string());
        let b = |k: &str, def: bool| o.get(k).map(|v| v.as_bool().unwrap_or(v.to_string() == "true")).unwrap_or(def);
        let n = |k: &str| o.get(k).and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok())).unwrap_or(0).max(0) as usize;
        d.class_name = s("className").or_else(|| s("classType"));
        if let Some(x) = s("separator") {
            d.separator = unescape_opt(&x);
        }
        d.skip_first_line = b("skipFirstLine", false);
        d.generate_header_columns = b("generateHeaderColumns", false);
        if let Some(q) = s("quote") {
            d.quote = unescape_opt(&q).chars().next();
        }
        d.quoting = b("quoting", false);
        d.remove_quotes = b("removeQuotes", true);
        if let Some(c) = s("crlf") {
            d.crlf = crlf(&c);
        }
        if let Some(c) = s("eol") {
            d.crlf = unescape_opt(&c);
        }
        d.end_with_line_break = b("endWithLineBreak", true);
        d.length = n("length");
        if let Some(p) = s("paddingChar") {
            d.padding_char = unescape_opt(&p).chars().next().unwrap_or(' ');
        }
        d.ignore_trailing_chars = b("ignoreTrailingChars", false);
        d.ignore_missing_chars = b("ignoreMissingChars", false);
        d.skip_header = b("skipHeader", false);
        d.skip_footer = b("skipFooter", false);
        if let Some(x) = s("keyValuePairSeparator") {
            d.key_value_pair_separator = unescape_opt(&x);
        }
        if let Some(x) = s("pairSeparator") {
            d.pair_separator = unescape_opt(&x);
        }
        let fields = o.get("fields").and_then(|f| f.as_array()).ok_or_else(|| bindy_err("A bindy descriptor needs fields"))?;
        for (i, f) in fields.iter().enumerate() {
            let fo = f.as_object().ok_or_else(|| bindy_err("A bindy field must be an object"))?;
            let fs = |k: &str| fo.get(k).filter(|v| !v.is_null()).map(|v| v.to_string());
            let fb = |k: &str, def: bool| fo.get(k).map(|v| v.as_bool().unwrap_or(v.to_string() == "true")).unwrap_or(def);
            let fnum = |k: &str| fo.get(k).and_then(|v| v.as_i64().or_else(|| v.to_string().parse().ok()));
            let name = fs("name").ok_or_else(|| bindy_err("A bindy field needs a name"))?;
            let mut bf = BindyField::new(name, fnum("pos").or_else(|| fnum("position")).map(|p| p.max(0) as usize).unwrap_or(i + 1));
            bf.field_type = match fs("type").map(|t| t.to_ascii_lowercase()).as_deref() {
                None | Some("string") | Some("text") => FieldType::String,
                Some("int") | Some("integer") | Some("long") | Some("short") | Some("byte") | Some("biginteger") => FieldType::Int,
                Some("double") | Some("float") => FieldType::Float,
                Some("decimal") | Some("bigdecimal") => FieldType::Decimal,
                Some("boolean") | Some("bool") => FieldType::Boolean,
                Some("char") | Some("character") => FieldType::Char,
                Some("date") | Some("datetime") | Some("localdate") | Some("localdatetime") | Some("timestamp") => FieldType::Date,
                Some(other) => return Err(bindy_err(format!("Unknown bindy field type: {}", other))),
            };
            bf.pattern = fs("pattern");
            bf.precision = fnum("precision").map(|p| p.max(0) as usize);
            bf.trim = fb("trim", false);
            bf.default_value = fs("defaultValue");
            bf.required = fb("required", false);
            bf.column_name = fs("columnName");
            bf.length = fnum("length").unwrap_or(0).max(0) as usize;
            bf.align_left = fs("align").map(|a| a.eq_ignore_ascii_case("L")).unwrap_or(false);
            bf.padding_char = fs("paddingChar").and_then(|p| unescape_opt(&p).chars().next());
            bf.delimiter = fs("delimiter").map(|x| unescape_opt(&x));
            bf.clip = fb("clip", false);
            bf.tag = fs("tag");
            d.fields.push(bf);
        }
        Ok(d)
    }

    fn sorted_fields(&self) -> Vec<&BindyField> {
        let mut f: Vec<&BindyField> = self.fields.iter().collect();
        f.sort_by_key(|x| x.pos);
        f
    }

    // ------------------------------------------------------------------ unmarshal

    /// Parse text into records.
    pub fn unmarshal_text(&self, text: &str) -> Result<Vec<Value>> {
        let mut lines: Vec<&str> = split_lines(text);
        while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            lines.pop();
        }
        let mut start = 0;
        if (self.kind == Kind::Csv && self.skip_first_line) || (self.kind == Kind::Fixed && self.skip_header) {
            start = 1;
        }
        if self.kind == Kind::Fixed && self.skip_footer && lines.len() > start {
            lines.pop();
        }
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate().skip(start) {
            if line.trim().is_empty() {
                continue;
            }
            let line_no = i + 1;
            let rec = match self.kind {
                Kind::Csv => self.parse_csv(line, line_no)?,
                Kind::Fixed => self.parse_fixed(line, line_no)?,
                Kind::Kvp => self.parse_kvp(line, line_no)?,
            };
            out.push(rec);
        }
        Ok(out)
    }

    fn convert(&self, f: &BindyField, raw: Option<&str>, line_no: usize) -> Result<Value> {
        let mut s = raw.map(|r| if f.trim { r.trim().to_string() } else { r.to_string() });
        if s.as_deref().map(|x| x.is_empty()).unwrap_or(true) {
            if let Some(d) = &f.default_value {
                s = Some(d.clone());
            } else if f.required {
                return Err(bindy_err(format!("The mandatory field defined at the position {} is empty for the line: {}", f.pos, line_no)));
            } else {
                return Ok(Value::Null);
            }
        }
        let s = s.unwrap();
        let t = s.trim();
        let bad = |what: &str| bindy_err(format!("{} for the field: {}, line: {} (value: {:?})", what, f.name, line_no, s));
        Ok(match f.field_type {
            FieldType::String => Value::String(s.clone()),
            FieldType::Char => Value::String(s.chars().next().map(|c| c.to_string()).unwrap_or_default()),
            FieldType::Int => Value::Int(t.trim_start_matches('+').parse().map_err(|_| bad("Parsing error detected"))?),
            FieldType::Float | FieldType::Decimal => {
                let n = t.replace(',', if f.pattern.as_deref().map(|p| p.contains(',') && !p.contains('.')).unwrap_or(false) { "." } else { "" });
                Value::Float(n.parse().map_err(|_| bad("Parsing error detected"))?)
            }
            FieldType::Boolean => Value::Bool(matches!(t.to_ascii_lowercase().as_str(), "true" | "1" | "y" | "yes")),
            FieldType::Date => Value::String(parse_date(t, f.pattern.as_deref().unwrap_or("yyyy-MM-dd")).ok_or_else(|| bad("Date provided does not fit the pattern defined"))?),
        })
    }

    fn record(&self, values: Vec<(&BindyField, Option<String>)>, line_no: usize) -> Result<Value> {
        let mut m = ObjectMap::new();
        // keep the declaration order of the descriptor
        for f in &self.fields {
            let raw = values.iter().find(|(x, _)| std::ptr::eq(*x, f)).and_then(|(_, v)| v.clone());
            m.insert(f.name.clone(), self.convert(f, raw.as_deref(), line_no)?);
        }
        Ok(Value::Object(m))
    }

    fn parse_csv(&self, line: &str, line_no: usize) -> Result<Value> {
        let tokens = split_csv(line, &self.separator, if self.remove_quotes { self.quote } else { None });
        let fields = self.sorted_fields();
        let mut vals = Vec::new();
        for f in fields {
            let v = tokens.get(f.pos.saturating_sub(1)).cloned();
            vals.push((f, v));
        }
        if tokens.len() < self.fields.iter().filter(|f| f.required).map(|f| f.pos).max().unwrap_or(0) {
            return Err(bindy_err(format!("Some mandatory fields are missing, line: {}", line_no)));
        }
        self.record(vals, line_no)
    }

    fn parse_fixed(&self, line: &str, line_no: usize) -> Result<Value> {
        let chars: Vec<char> = line.chars().collect();
        if self.length > 0 {
            if chars.len() > self.length && !self.ignore_trailing_chars {
                return Err(bindy_err(format!("Size of the record: {} is not equal to the value provided in the model: {}", chars.len(), self.length)));
            }
            if chars.len() < self.length && !self.ignore_missing_chars {
                return Err(bindy_err(format!("Size of the record: {} is not equal to the value provided in the model: {}", chars.len(), self.length)));
            }
        }
        let fields = self.sorted_fields();
        let explicit = self.fields.iter().any(|f| f.length > 0) && fields.windows(2).all(|w| w[0].pos + w[0].length <= w[1].pos || w[0].delimiter.is_some());
        let mut vals = Vec::new();
        let mut cursor = 0usize;
        for f in fields {
            let start = if explicit && f.delimiter.is_none() && f.pos > 0 && f.pos - 1 >= cursor { f.pos - 1 } else { cursor };
            let (raw, next) = if let Some(d) = &f.delimiter {
                let rest: String = chars.get(start..).map(|c| c.iter().collect()).unwrap_or_default();
                match rest.find(d.as_str()) {
                    Some(p) => (rest[..p].to_string(), start + rest[..p].chars().count() + d.chars().count()),
                    None => (rest.clone(), chars.len()),
                }
            } else {
                let end = (start + f.length).min(chars.len());
                let s: String = chars.get(start..end).map(|c| c.iter().collect()).unwrap_or_default();
                (s, start + f.length)
            };
            cursor = next;
            let pad = f.padding_char.unwrap_or(self.padding_char);
            let value = if f.trim || f.field_type != FieldType::String {
                let stripped = if f.align_left { raw.trim_end_matches(pad) } else { raw.trim_start_matches(pad) };
                let stripped = if f.trim { stripped.trim_matches(pad).trim() } else { stripped.trim() };
                // numbers padded with zeros: keep at least one digit
                if stripped.is_empty() && pad == '0' && !raw.is_empty() {
                    "0".to_string()
                } else {
                    stripped.to_string()
                }
            } else {
                raw
            };
            vals.push((f, Some(value)));
        }
        self.record(vals, line_no)
    }

    fn parse_kvp(&self, line: &str, line_no: usize) -> Result<Value> {
        let mut pairs: Vec<(String, String)> = Vec::new();
        for p in line.split(self.pair_separator.as_str()) {
            if p.is_empty() {
                continue;
            }
            match p.split_once(self.key_value_pair_separator.as_str()) {
                Some((k, v)) => pairs.push((k.to_string(), v.to_string())),
                None => return Err(bindy_err(format!("Key value pair separator {:?} not found in {:?}, line: {}", self.key_value_pair_separator, p, line_no))),
            }
        }
        let fields = self.sorted_fields();
        let mut vals = Vec::new();
        for f in fields {
            let tag = f.tag.clone().unwrap_or_else(|| f.name.clone());
            let v = pairs.iter().find(|(k, _)| *k == tag).map(|(_, v)| v.clone());
            if v.is_none() && f.required {
                return Err(bindy_err(format!("The mandatory key/tag : {} has not been defined !", tag)));
            }
            vals.push((f, v));
        }
        self.record(vals, line_no)
    }

    // ------------------------------------------------------------------ marshal

    fn format(&self, f: &BindyField, v: Option<&Value>) -> Result<String> {
        let v = match v {
            None | Some(Value::Null) => {
                if f.required && f.default_value.is_none() {
                    return Err(bindy_err(format!("The mandatory field {} is not set", f.name)));
                }
                return Ok(f.default_value.clone().unwrap_or_default());
            }
            Some(v) => v,
        };
        Ok(match f.field_type {
            FieldType::Float | FieldType::Decimal => match (v.as_f64(), f.precision) {
                (Some(x), Some(p)) => format!("{:.*}", p, x),
                (Some(x), None) if f.field_type == FieldType::Decimal => {
                    let s = v.to_string();
                    if s.parse::<f64>().is_ok() {
                        s
                    } else {
                        x.to_string()
                    }
                }
                _ => v.to_string(),
            },
            FieldType::Date => match v {
                Value::Int(ms) => format_millis(*ms, f.pattern.as_deref().unwrap_or("yyyy-MM-dd")),
                other => {
                    let s = other.to_string();
                    match parse_iso(&s) {
                        Some(ms) => format_millis(ms, f.pattern.as_deref().unwrap_or("yyyy-MM-dd")),
                        None => s,
                    }
                }
            },
            FieldType::Boolean => v.as_bool().map(|b| b.to_string()).unwrap_or_else(|| v.to_string()),
            _ => v.to_string(),
        })
    }

    fn marshal_record(&self, rec: &Value) -> Result<String> {
        let o = rec.as_object().ok_or_else(|| bindy_err(format!("Bindy expects objects to marshal, got {}", rec.type_name())))?;
        let fields = self.sorted_fields();
        match self.kind {
            Kind::Csv => {
                let mut cols = Vec::new();
                for f in fields {
                    let s = self.format(f, o.get(&f.name))?;
                    cols.push(self.quote_csv(&s));
                }
                Ok(cols.join(&self.separator))
            }
            Kind::Fixed => {
                let mut line = String::new();
                for f in fields {
                    let s = self.format(f, o.get(&f.name))?;
                    if let Some(d) = &f.delimiter {
                        line.push_str(&s);
                        line.push_str(d);
                        continue;
                    }
                    let start = f.pos.saturating_sub(1);
                    let cur = line.chars().count();
                    if f.length > 0 && start > cur && self.fields.iter().all(|x| x.length > 0 || x.delimiter.is_some()) {
                        line.extend(std::iter::repeat_n(self.padding_char, start - cur));
                    }
                    let len = s.chars().count();
                    if f.length == 0 {
                        line.push_str(&s);
                    } else if len > f.length {
                        if !f.clip {
                            return Err(bindy_err(format!("Length for the {} must not be larger than allowed, was: {}, allowed: {}", f.name, len, f.length)));
                        }
                        line.extend(s.chars().take(f.length));
                    } else {
                        let pad = f.padding_char.unwrap_or(self.padding_char);
                        let p: String = std::iter::repeat_n(pad, f.length - len).collect();
                        if f.align_left {
                            line.push_str(&s);
                            line.push_str(&p);
                        } else if pad == '0' && s.starts_with('-') {
                            line.push('-');
                            line.push_str(&p);
                            line.push_str(&s[1..]);
                        } else {
                            line.push_str(&p);
                            line.push_str(&s);
                        }
                    }
                }
                let n = line.chars().count();
                if self.length > n {
                    line.extend(std::iter::repeat_n(self.padding_char, self.length - n));
                }
                Ok(line)
            }
            Kind::Kvp => {
                let mut out = String::new();
                for f in fields {
                    let v = o.get(&f.name);
                    if v.map(|x| x.is_null()).unwrap_or(true) && f.default_value.is_none() && !f.required {
                        continue;
                    }
                    let s = self.format(f, v)?;
                    out.push_str(f.tag.as_deref().unwrap_or(&f.name));
                    out.push_str(&self.key_value_pair_separator);
                    out.push_str(&s);
                    out.push_str(&self.pair_separator);
                }
                Ok(out)
            }
        }
    }

    fn quote_csv(&self, s: &str) -> String {
        match self.quote {
            Some(q) if self.quoting || s.contains(self.separator.as_str()) || s.contains(q) || s.contains('\n') || s.contains('\r') => {
                let doubled = s.replace(q, &format!("{}{}", q, q));
                format!("{}{}{}", q, doubled, q)
            }
            _ => s.to_string(),
        }
    }

    /// Format records as text.
    pub fn marshal_records(&self, records: &[Value]) -> Result<String> {
        let mut lines = Vec::new();
        if self.kind == Kind::Csv && self.generate_header_columns {
            let h: Vec<String> = self.sorted_fields().iter().map(|f| self.quote_csv(f.column_name.as_deref().unwrap_or(&f.name))).collect();
            lines.push(h.join(&self.separator));
        }
        for r in records {
            lines.push(self.marshal_record(r)?);
        }
        let mut out = lines.join(&self.crlf);
        if self.end_with_line_break && !lines.is_empty() {
            out.push_str(&self.crlf);
        }
        Ok(out)
    }
}

fn crlf(c: &str) -> String {
    match c.to_ascii_uppercase().as_str() {
        "WINDOWS" => "\r\n".into(),
        "UNIX" => "\n".into(),
        "MAC" => "\r".into(),
        _ => unescape_opt(c),
    }
}

fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for l in text.split('\n') {
        out.push(l.strip_suffix('\r').unwrap_or(l));
    }
    out
}

/// Split a CSV line on `sep` honoring `quote` (doubled quotes are unescaped).
fn split_csv(line: &str, sep: &str, quote: Option<char>) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut i = 0;
    let chars: Vec<char> = line.chars().collect();
    let sepc: Vec<char> = sep.chars().collect();
    while i < chars.len() {
        let c = chars[i];
        if Some(c) == quote {
            if in_q && chars.get(i + 1) == Some(&c) {
                cur.push(c);
                i += 2;
                continue;
            }
            if in_q || cur.trim().is_empty() {
                in_q = !in_q;
                if in_q {
                    cur.clear();
                }
                i += 1;
                continue;
            }
        }
        if !in_q && !sepc.is_empty() && chars[i..].starts_with(&sepc) {
            out.push(std::mem::take(&mut cur));
            i += sepc.len();
            continue;
        }
        cur.push(c);
        i += 1;
    }
    out.push(cur);
    out
}

// ---------------------------------------------------------------- dates

/// Parse a date with a SimpleDateFormat-like pattern; returns ISO-8601 text.
fn parse_date(s: &str, pattern: &str) -> Option<String> {
    let (mut y, mut mo, mut d, mut h, mut mi, mut se, mut ms) = (1970i64, 1u32, 1u32, 0u32, 0u32, 0u32, 0u32);
    let mut has_time = false;
    let pc: Vec<char> = pattern.chars().collect();
    let sc: Vec<char> = s.chars().collect();
    let (mut i, mut j) = (0, 0);
    while i < pc.len() {
        let c = pc[i];
        if c == '\'' {
            i += 1;
            while i < pc.len() && pc[i] != '\'' {
                if sc.get(j) != Some(&pc[i]) {
                    return None;
                }
                i += 1;
                j += 1;
            }
            i += 1;
            continue;
        }
        if !c.is_ascii_alphabetic() {
            if sc.get(j) != Some(&c) {
                return None;
            }
            i += 1;
            j += 1;
            continue;
        }
        let mut n = 0;
        while i < pc.len() && pc[i] == c {
            n += 1;
            i += 1;
        }
        // numeric field: take n digits (or as many as available when the next pattern char is a literal)
        let followed_by_letter = i < pc.len() && pc[i].is_ascii_alphabetic();
        let max = if followed_by_letter || c == 'y' && n == 2 { n } else { 9 };
        let start = j;
        while j < sc.len() && sc[j].is_ascii_digit() && j - start < max {
            j += 1;
        }
        if j == start {
            return None;
        }
        let num: i64 = sc[start..j].iter().collect::<String>().parse().ok()?;
        match c {
            'y' => y = if n == 2 { 2000 + num } else { num },
            'M' => mo = num as u32,
            'd' => d = num as u32,
            'H' | 'k' => {
                h = num as u32;
                has_time = true
            }
            'm' => {
                mi = num as u32;
                has_time = true
            }
            's' => {
                se = num as u32;
                has_time = true
            }
            'S' => {
                ms = num as u32;
                has_time = true
            }
            _ => return None,
        }
    }
    if j != sc.len() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some(if !has_time {
        format!("{:04}-{:02}-{:02}", y, mo, d)
    } else if ms > 0 {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}", y, mo, d, h, mi, se, ms)
    } else {
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}", y, mo, d, h, mi, se)
    })
}

fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim_end_matches('Z');
    let (date, time) = match s.split_once('T') {
        Some((a, b)) => (a, Some(b)),
        None => (s, None),
    };
    let p: Vec<&str> = date.split('-').collect();
    if p.len() != 3 {
        return None;
    }
    let (y, m, d) = (p[0].parse().ok()?, p[1].parse().ok()?, p[2].parse().ok()?);
    let (mut h, mut mi, mut se, mut ms) = (0, 0, 0, 0i64);
    if let Some(t) = time {
        let (hms, frac) = t.split_once('.').unwrap_or((t, "0"));
        let q: Vec<&str> = hms.split(':').collect();
        h = q.first()?.parse().ok()?;
        mi = q.get(1).map(|x| x.parse().ok()).unwrap_or(Some(0))?;
        se = q.get(2).map(|x| x.parse().ok()).unwrap_or(Some(0))?;
        ms = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;
    }
    Some(camel_core::util_time::epoch_from_civil(y, m, d, h, mi, se) * 1000 + ms)
}

fn format_millis(ms: i64, pattern: &str) -> String {
    let (y, mo, d, h, mi, s) = camel_core::util_time::civil_from_epoch(ms.div_euclid(1000));
    let frac = ms.rem_euclid(1000);
    let mut out = String::new();
    let pc: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < pc.len() {
        let c = pc[i];
        if c == '\'' {
            i += 1;
            while i < pc.len() && pc[i] != '\'' {
                out.push(pc[i]);
                i += 1;
            }
            i += 1;
            continue;
        }
        let mut n = 0;
        while i < pc.len() && pc[i] == c {
            n += 1;
            i += 1;
        }
        let num = |v: i64, w: usize| format!("{:0w$}", v, w = w);
        match c {
            'y' if n == 2 => out.push_str(&num(y.rem_euclid(100), 2)),
            'y' => out.push_str(&num(y, n.max(4))),
            'M' => out.push_str(&num(mo as i64, n)),
            'd' => out.push_str(&num(d as i64, n)),
            'H' | 'k' => out.push_str(&num(h as i64, n)),
            'm' => out.push_str(&num(mi as i64, n)),
            's' => out.push_str(&num(s as i64, n)),
            'S' => out.push_str(&num(frac, n.max(3))[..n.max(1).min(3)]),
            other => out.extend(std::iter::repeat_n(other, n)),
        }
    }
    out
}

// ---------------------------------------------------------------- data format

/// Bind a descriptor in the registry under `name` (referenced with `classType`).
pub fn bind_descriptor(registry: &camel_core::context::Registry, name: impl Into<String>, d: BindyDescriptor) {
    registry.bind(name, d);
}

/// The bindy data formats.
pub struct BindyDataFormat {
    pub kind: Kind,
    pub descriptor: Option<BindyDescriptor>,
    pub class_type: Option<String>,
    pub unwrap_single_instance: bool,
    pub allow_empty_stream: bool,
}

impl BindyDataFormat {
    /// A bindy data format for a descriptor.
    pub fn new(descriptor: BindyDescriptor) -> BindyDataFormat {
        BindyDataFormat { kind: descriptor.kind, descriptor: Some(descriptor), class_type: None, unwrap_single_instance: true, allow_empty_stream: false }
    }

    fn descriptor(&self, ex: &Exchange) -> Result<BindyDescriptor> {
        if let Some(d) = &self.descriptor {
            return Ok(d.clone());
        }
        let name = self.class_type.as_deref().ok_or_else(|| bindy_err("Bindy requires a classType or descriptor"))?;
        let name = name.trim_start_matches('#');
        if let Ok(ctx) = ex.context() {
            if let Some(d) = ctx.registry().lookup::<BindyDescriptor>(name) {
                let mut d = (*d).clone();
                d.kind = self.kind;
                return Ok(d);
            }
            if let Some(t) = ctx.registry().lookup::<String>(name) {
                return BindyDescriptor::parse(&t, self.kind);
            }
        }
        if name.ends_with(".yaml") || name.ends_with(".yml") || name.ends_with(".json") {
            let t = String::from_utf8(crate::read_resource(name)?).map_err(|_| bindy_err("Descriptor file is not UTF-8"))?;
            return BindyDescriptor::parse(&t, self.kind);
        }
        Err(bindy_err(format!("No bindy descriptor found for classType: {}", name)))
    }
}

impl DataFormat for BindyDataFormat {
    fn name(&self) -> &str {
        match self.kind {
            Kind::Csv => "bindyCsv",
            Kind::Fixed => "bindyFixed",
            Kind::Kvp => "bindyKvp",
        }
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let d = self.descriptor(ex)?;
        let records = match &ex.message.body {
            Value::Array(a) => a.clone(),
            Value::Object(_) => vec![ex.message.body.clone()],
            Value::String(_) | Value::Bytes(_) => {
                let t = body_text(&ex.message.body)?;
                match camel_core::json::parse(&t)? {
                    Value::Array(a) => a,
                    o => vec![o],
                }
            }
            Value::Null => Vec::new(),
            other => return Err(df_err(format!("Bindy cannot marshal a {}", other.type_name()))),
        };
        ex.message.body = Value::String(d.marshal_records(&records)?);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let d = self.descriptor(ex)?;
        let text = body_text(&ex.message.body)?;
        if text.trim().is_empty() {
            if self.allow_empty_stream {
                ex.message.body = Value::Array(Vec::new());
                return Ok(());
            }
            return Err(bindy_err("The stream is empty"));
        }
        let mut records = d.unmarshal_text(&text)?;
        ex.message.body = if self.unwrap_single_instance && records.len() == 1 { records.remove(0) } else { Value::Array(records) };
        Ok(())
    }
}

fn create(o: &Opts, kind: Kind) -> Result<Arc<dyn DataFormat>> {
    let descriptor = match (o.get(&["descriptor"]), o.get(&["descriptorFile"])) {
        (Some(t), _) => Some(BindyDescriptor::parse(t, kind)?),
        (None, Some(f)) => Some(BindyDescriptor::parse(&String::from_utf8(crate::read_resource(f)?).map_err(|_| bindy_err("Descriptor file is not UTF-8"))?, kind)?),
        _ => None,
    };
    let class_type = o.get(&["classType", "className", "class"]).map(String::from);
    if descriptor.is_none() && class_type.is_none() {
        return Err(bindy_err("Bindy requires a classType or descriptor option"));
    }
    Ok(Arc::new(BindyDataFormat {
        kind,
        descriptor,
        class_type,
        unwrap_single_instance: o.bool(&["unwrapSingleInstance"], true)?,
        allow_empty_stream: o.bool(&["allowEmptyStream"], false)?,
    }))
}

pub(crate) fn create_csv(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let kind = match o.get(&["type"]).map(|t| t.to_ascii_lowercase()) {
        Some(t) if t == "fixed" => Kind::Fixed,
        Some(t) if t == "keyvalue" || t == "kvp" => Kind::Kvp,
        _ => Kind::Csv,
    };
    create(o, kind)
}

pub(crate) fn create_fixed(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    create(o, Kind::Fixed)
}

pub(crate) fn create_kvp(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    create(o, Kind::Kvp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(parse_date("02-01-2009", "dd-MM-yyyy").unwrap(), "2009-01-02");
        assert_eq!(parse_date("20090102 10:11:12", "yyyyMMdd HH:mm:ss").unwrap(), "2009-01-02T10:11:12");
        assert!(parse_date("2009/01/02", "dd-MM-yyyy").is_none());
        assert_eq!(format_millis(parse_iso("2009-01-02T10:11:12").unwrap(), "dd/MM/yy HH:mm"), "02/01/09 10:11");
    }

    #[test]
    fn csv_split() {
        assert_eq!(split_csv(r#"a,"b,c",,"d""e""#, ",", Some('"')), vec!["a", "b,c", "", "d\"e"]);
        assert_eq!(split_csv("a||b", "||", None), vec!["a", "b"]);
    }
}
