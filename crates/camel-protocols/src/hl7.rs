//! HL7 v2 "lite": parse ER7 (pipe delimited) messages into segments/fields, terser-like
//! path access (`PID-5-1`, `OBX(1)-5`, `PID-3(1)-1`), acknowledgement generation, the
//! `hl7` data format and the `hl7terser` language.
//!
//! The structured value produced by `unmarshal(hl7)` is an object:
//! `{"messageType": "ADT^A01", "triggerEvent": "A01", "messageControlId": "...",
//! "versionId": "2.5", "segments": [{"name": "MSH", "fields": ["|", "^~\\&", ...]}, ...]}`
//! where `fields[i]` is field `i + 1` of the segment (raw, still encoded).

use std::collections::BTreeMap;
use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::{CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Expression, Language};
use camel_core::util_time::{format_millis, now_millis};
use camel_core::value::Value;

/// Exception kind for HL7 parse failures.
pub const HL7_EXCEPTION: &str = "HL7Exception";

/// Headers set by the `hl7` data format when unmarshalling (Camel `HL7Constants`).
pub mod headers {
    pub const SENDING_APPLICATION: &str = "CamelHL7SendingApplication";
    pub const SENDING_FACILITY: &str = "CamelHL7SendingFacility";
    pub const RECEIVING_APPLICATION: &str = "CamelHL7ReceivingApplication";
    pub const RECEIVING_FACILITY: &str = "CamelHL7ReceivingFacility";
    pub const TIMESTAMP: &str = "CamelHL7Timestamp";
    pub const SECURITY: &str = "CamelHL7Security";
    pub const MESSAGE_TYPE: &str = "CamelHL7MessageType";
    pub const TRIGGER_EVENT: &str = "CamelHL7TriggerEvent";
    pub const MESSAGE_CONTROL: &str = "CamelHL7MessageControl";
    pub const PROCESSING_ID: &str = "CamelHL7ProcessingId";
    pub const VERSION_ID: &str = "CamelHL7VersionId";
    pub const CHARSET: &str = "CamelHL7Charset";
}

/// One segment: its name and fields (`fields[0]` is field 1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub name: String,
    pub fields: Vec<String>,
}

/// A parsed HL7 v2 message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hl7Message {
    pub segments: Vec<Segment>,
    pub field_separator: char,
    pub component_separator: char,
    pub repetition_separator: char,
    pub escape_character: char,
    pub subcomponent_separator: char,
}

impl Hl7Message {
    /// Parse an ER7 encoded message (segments separated by CR, LF or CRLF).
    pub fn parse(text: &str) -> Result<Hl7Message> {
        let text = text.trim_start_matches(['\u{b}', '\r', '\n', ' ']).trim_end_matches(['\u{1c}', '\r', '\n']);
        if !text.starts_with("MSH") || text.len() < 8 {
            return Err(CamelError::new(HL7_EXCEPTION, "HL7 message must start with an MSH segment"));
        }
        let mut chars = text[3..].chars();
        let fs = chars.next().unwrap();
        let enc: Vec<char> = text[4..].chars().take_while(|&c| c != fs).collect();
        let get = |i: usize, d: char| enc.get(i).copied().unwrap_or(d);
        let mut msg = Hl7Message {
            segments: Vec::new(),
            field_separator: fs,
            component_separator: get(0, '^'),
            repetition_separator: get(1, '~'),
            escape_character: get(2, '\\'),
            subcomponent_separator: get(3, '&'),
        };
        for line in text.split(['\r', '\n']).filter(|l| !l.trim().is_empty()) {
            let mut parts: Vec<String> = line.split(fs).map(|s| s.to_string()).collect();
            let name = parts.remove(0);
            if name.len() != 3 || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(CamelError::new(HL7_EXCEPTION, format!("Invalid segment name: {}", name)));
            }
            if name == "MSH" {
                parts.insert(0, fs.to_string());
            }
            msg.segments.push(Segment { name, fields: parts });
        }
        Ok(msg)
    }

    /// Encode back to ER7 (segments terminated by CR).
    pub fn encode(&self) -> String {
        let mut out = String::new();
        for s in &self.segments {
            out.push_str(&s.name);
            let fields = if s.name == "MSH" { &s.fields[1.min(s.fields.len())..] } else { &s.fields[..] };
            for f in fields {
                out.push(self.field_separator);
                out.push_str(f);
            }
            out.push('\r');
        }
        out
    }

    /// The `index`-th (0 based) segment named `name`.
    pub fn segment(&self, name: &str, index: usize) -> Option<&Segment> {
        self.segments.iter().filter(|s| s.name == name).nth(index)
    }

    /// Raw (encoded) value of a field (1 based), e.g. `field("MSH", 9)`.
    pub fn field(&self, segment: &str, field: usize) -> Option<&str> {
        let s = self.segment(segment, 0)?;
        s.fields.get(field.checked_sub(1)?).map(|s| s.as_str())
    }

    /// Unescape HL7 escape sequences (`\F\`, `\S\`, `\T\`, `\R\`, `\E\`, `\.br\`).
    pub fn unescape(&self, s: &str) -> String {
        let e = self.escape_character;
        if !s.contains(e) {
            return s.to_string();
        }
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find(e) {
            out.push_str(&rest[..i]);
            let after = &rest[i + e.len_utf8()..];
            match after.find(e) {
                Some(j) => {
                    match &after[..j] {
                        "F" => out.push(self.field_separator),
                        "S" => out.push(self.component_separator),
                        "T" => out.push(self.subcomponent_separator),
                        "R" => out.push(self.repetition_separator),
                        "E" => out.push(e),
                        ".br" => out.push('\r'),
                        other => {
                            out.push(e);
                            out.push_str(other);
                            out.push(e);
                        }
                    }
                    rest = &after[j + e.len_utf8()..];
                }
                None => {
                    out.push_str(&rest[i..]);
                    rest = "";
                }
            }
        }
        out.push_str(rest);
        out
    }

    /// Terser-like access: `SEG[(rep)]-field[(rep)][-component[-subcomponent]]`.
    /// Segment and field repetitions are 0 based, components 1 based (default 1),
    /// like HAPI's `Terser.get`. `None` if the segment does not exist.
    pub fn get(&self, path: &str) -> Option<String> {
        let p = TerserPath::parse(path)?;
        let seg = self.segment(&p.segment, p.segment_rep)?;
        if seg.name == "MSH" && p.field <= 2 {
            return Some(seg.fields.get(p.field - 1).cloned().unwrap_or_default());
        }
        let Some(raw) = seg.fields.get(p.field - 1) else { return Some(String::new()) };
        let rep = raw.split(self.repetition_separator).nth(p.field_rep).unwrap_or("");
        let comp = rep.split(self.component_separator).nth(p.component - 1).unwrap_or("");
        let sub = comp.split(self.subcomponent_separator).nth(p.subcomponent - 1).unwrap_or("");
        Some(self.unescape(sub))
    }

    /// The message type (`MSH-9`, e.g. `ADT^A01`).
    pub fn message_type(&self) -> String {
        self.field("MSH", 9).unwrap_or("").to_string()
    }

    /// The message control id (`MSH-10`).
    pub fn control_id(&self) -> String {
        self.get("MSH-10").unwrap_or_default()
    }

    /// Structured representation (see the module docs).
    pub fn to_value(&self) -> Value {
        let segs: Vec<Value> = self
            .segments
            .iter()
            .map(|s| Value::object([("name", Value::String(s.name.clone())), ("fields", Value::Array(s.fields.iter().cloned().map(Value::String).collect()))]))
            .collect();
        Value::object([
            ("messageType", Value::String(self.message_type())),
            ("triggerEvent", Value::String(self.get("MSH-9-2").unwrap_or_default())),
            ("messageControlId", Value::String(self.control_id())),
            ("versionId", Value::String(self.get("MSH-12").unwrap_or_default())),
            ("segments", Value::Array(segs)),
        ])
    }

    /// Build from the structured representation.
    pub fn from_value(v: &Value) -> Result<Hl7Message> {
        let segs = v.get("segments").and_then(|s| s.as_array()).ok_or_else(|| CamelError::new(HL7_EXCEPTION, "Value has no segments array"))?;
        let mut text = String::new();
        let mut fs = '|';
        for s in segs {
            let name = s.get("name").map(|n| n.to_string()).unwrap_or_default();
            let fields: Vec<String> = s.get("fields").and_then(|f| f.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_default();
            if name == "MSH" {
                fs = fields.first().and_then(|f| f.chars().next()).unwrap_or('|');
                text.push_str("MSH");
                text.push_str(&fields[1.min(fields.len())..].iter().map(|f| format!("{}{}", fs, f)).collect::<String>());
            } else {
                text.push_str(&name);
                text.push_str(&fields.iter().map(|f| format!("{}{}", fs, f)).collect::<String>());
            }
            text.push('\r');
        }
        Hl7Message::parse(&text)
    }

    /// Generate an acknowledgement (`AA`, `AE`, `AR`, `CA`, `CE`, `CR`) for this message:
    /// sending/receiving application and facility are swapped, `MSA-2` echoes `MSH-10`.
    pub fn acknowledgement(&self, code: &str, text: Option<&str>) -> String {
        let f = |p: &str| self.field("MSH", p[4..].parse().unwrap_or(0)).unwrap_or("").to_string();
        let fs = self.field_separator;
        let enc = self.field("MSH", 2).unwrap_or("^~\\&");
        let trigger = self.get("MSH-9-2").unwrap_or_default();
        let ack_type = if trigger.is_empty() { "ACK".to_string() } else { format!("ACK{}{}", self.component_separator, trigger) };
        let ts = format_millis(now_millis(), "yyyyMMddHHmmss");
        let fields = [
            enc.to_string(),
            f("MSH-5"),
            f("MSH-6"),
            f("MSH-3"),
            f("MSH-4"),
            ts,
            String::new(),
            ack_type,
            f("MSH-10"),
            f("MSH-11"),
            f("MSH-12"),
        ];
        let mut out = format!("MSH{}{}\r", fs, fields.join(&fs.to_string()));
        out.push_str(&format!("MSA{}{}{}{}", fs, code, fs, f("MSH-10")));
        if let Some(t) = text {
            out.push_str(&format!("{}{}", fs, t.replace(['\r', '\n'], " ").replace(fs, " ")));
        }
        out.push('\r');
        out
    }
}

struct TerserPath {
    segment: String,
    segment_rep: usize,
    field: usize,
    field_rep: usize,
    component: usize,
    subcomponent: usize,
}

impl TerserPath {
    fn parse(path: &str) -> Option<TerserPath> {
        let path = path.trim().trim_start_matches('/').trim_start_matches('.');
        let path = path.rsplit('/').next()?;
        let parts: Vec<&str> = path.split('-').collect();
        if parts.len() < 2 {
            return None;
        }
        let rep = |s: &str| -> Option<(String, usize)> {
            match s.find('(') {
                Some(i) => Some((s[..i].to_string(), s[i + 1..].strip_suffix(')')?.parse().ok()?)),
                None => Some((s.to_string(), 0)),
            }
        };
        let (segment, segment_rep) = rep(parts[0])?;
        let (field, field_rep) = rep(parts[1])?;
        let field: usize = field.parse().ok().filter(|&f| f > 0)?;
        let component = parts.get(2).map(|c| c.parse().ok().filter(|&c| c > 0)).unwrap_or(Some(1))?;
        let subcomponent = parts.get(3).map(|c| c.parse().ok().filter(|&c| c > 0)).unwrap_or(Some(1))?;
        Some(TerserPath { segment, segment_rep, field, field_rep, component, subcomponent })
    }
}

/// Parse a message from a body value (ER7 string/bytes or the structured object).
pub fn message_from_value(v: &Value) -> Result<Hl7Message> {
    match v {
        Value::Object(_) => Hl7Message::from_value(v),
        Value::Bytes(b) => Hl7Message::parse(&String::from_utf8_lossy(b)),
        other => Hl7Message::parse(&other.to_string()),
    }
}

/// Terser access on a raw HL7 text (convenience): `hl7_terser(msg, "PID-5-1")`.
pub fn hl7_terser(message: &str, path: &str) -> Option<String> {
    Hl7Message::parse(message).ok()?.get(path)
}

/// Set the `CamelHL7*` headers from `MSH`.
pub fn set_hl7_headers(ex: &mut Exchange, m: &Hl7Message) {
    let pairs = [
        (headers::SENDING_APPLICATION, "MSH-3"),
        (headers::SENDING_FACILITY, "MSH-4"),
        (headers::RECEIVING_APPLICATION, "MSH-5"),
        (headers::RECEIVING_FACILITY, "MSH-6"),
        (headers::TIMESTAMP, "MSH-7"),
        (headers::SECURITY, "MSH-8"),
        (headers::MESSAGE_TYPE, "MSH-9-1"),
        (headers::TRIGGER_EVENT, "MSH-9-2"),
        (headers::MESSAGE_CONTROL, "MSH-10"),
        (headers::PROCESSING_ID, "MSH-11"),
        (headers::VERSION_ID, "MSH-12"),
        (headers::CHARSET, "MSH-18"),
    ];
    for (h, p) in pairs {
        ex.set_header(h, m.get(p).unwrap_or_default());
    }
}

/// The `hl7` data format: `unmarshal` parses ER7 text into the structured value and
/// sets the `CamelHL7*` headers; `marshal` encodes a structured value (or validates an
/// ER7 string) to ER7 text with CR segment separators.
#[derive(Clone, Debug, Default)]
pub struct Hl7DataFormat {
    /// Validate (parse) on marshal/unmarshal (default true).
    pub validate: bool,
}

impl Hl7DataFormat {
    pub fn new() -> Self {
        Hl7DataFormat { validate: true }
    }
}

impl DataFormat for Hl7DataFormat {
    fn name(&self) -> &str {
        "hl7"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let text = match ex.body() {
            Value::Object(_) => Hl7Message::from_value(ex.body())?.encode(),
            other if self.validate => message_from_value(other)?.encode(),
            other => other.to_string(),
        };
        ex.set_body(text);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let m = message_from_value(ex.body())?;
        set_hl7_headers(ex, &m);
        ex.set_body(m.to_value());
        Ok(())
    }
}

/// The `hl7terser` language: evaluates a terser path against the HL7 body.
#[derive(Default)]
pub struct Hl7TerserLanguage;

impl Language for Hl7TerserLanguage {
    fn name(&self) -> &str {
        "hl7terser"
    }
    fn create_expression(&self, text: &str, _options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let path = text.trim().to_string();
        if TerserPath::parse(&path).is_none() {
            return Err(CamelError::new(camel_core::error::kinds::EXPRESSION_ILLEGAL_SYNTAX, format!("Invalid HL7 terser path: {}", path)));
        }
        Ok(Arc::new(move |ex: &Exchange| -> Result<Value> {
            let m = message_from_value(ex.body())?;
            Ok(m.get(&path).map(Value::String).unwrap_or(Value::Null))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const ADT: &str = "MSH|^~\\&|SENDAPP|SENDFAC|RECVAPP|RECVFAC|20240102120000||ADT^A01^ADT_A01|MSG00001|P|2.5\rEVN|A01|20240102120000\rPID|1||12345^^^MRN~67890^^^SSN||Doe^John^Q||19800101|M|||1 Main St\\F\\Apt 2^^Springfield^IL\rOBX|1|ST|A||first\rOBX|2|ST|B||second\r";

    #[test]
    fn parse_and_terser() {
        let m = Hl7Message::parse(ADT).unwrap();
        assert_eq!(m.segments.len(), 5);
        assert_eq!(m.get("MSH-1").as_deref(), Some("|"));
        assert_eq!(m.get("MSH-2").as_deref(), Some("^~\\&"));
        assert_eq!(m.get("MSH-3").as_deref(), Some("SENDAPP"));
        assert_eq!(m.get("MSH-9").as_deref(), Some("ADT"));
        assert_eq!(m.get("MSH-9-2").as_deref(), Some("A01"));
        assert_eq!(m.get("/.MSH-10").as_deref(), Some("MSG00001"));
        assert_eq!(m.get("PID-5-1").as_deref(), Some("Doe"));
        assert_eq!(m.get("PID-5-2").as_deref(), Some("John"));
        assert_eq!(m.get("PID-3(1)-1").as_deref(), Some("67890"));
        assert_eq!(m.get("PID-11-1").as_deref(), Some("1 Main St|Apt 2"));
        assert_eq!(m.get("OBX(1)-5").as_deref(), Some("second"));
        assert_eq!(m.get("PID-99").as_deref(), Some(""));
        assert_eq!(m.get("ZZZ-1"), None);
        assert_eq!(hl7_terser(ADT, "EVN-1").as_deref(), Some("A01"));
        assert_eq!(m.encode(), ADT);
    }

    #[test]
    fn value_round_trip_and_ack() {
        let m = Hl7Message::parse(&ADT.replace('\r', "\n")).unwrap();
        let v = m.to_value();
        assert_eq!(v.get("messageType").unwrap().to_string(), "ADT^A01^ADT_A01");
        assert_eq!(v.get("messageControlId").unwrap().to_string(), "MSG00001");
        assert_eq!(Hl7Message::from_value(&v).unwrap(), m);
        let ack = Hl7Message::parse(&m.acknowledgement("AA", None)).unwrap();
        assert_eq!(ack.get("MSH-3").as_deref(), Some("RECVAPP"));
        assert_eq!(ack.get("MSH-5").as_deref(), Some("SENDAPP"));
        assert_eq!(ack.get("MSH-9").as_deref(), Some("ACK"));
        assert_eq!(ack.get("MSH-9-2").as_deref(), Some("A01"));
        assert_eq!(ack.get("MSA-1").as_deref(), Some("AA"));
        assert_eq!(ack.get("MSA-2").as_deref(), Some("MSG00001"));
        assert!(Hl7Message::parse("PID|1").is_err());
    }
}
