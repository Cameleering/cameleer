//! `iCal` data format (RFC 5545): iCalendar text <-> `Value`.
//!
//! A component is an object `{"type": "VEVENT", "properties": {...}, "components": [...]}`.
//! Properties are keyed by name (in document order); a property without parameters is its
//! value string, with parameters `{"value": "...", "params": {"TZID": "Europe/Paris"}}`, and
//! a repeated property an array of those. Values are kept in their RFC 5545 text form
//! (TEXT escapes `\\n`, `\\,`... are decoded on unmarshal and encoded on marshal for the
//! TEXT properties `SUMMARY`, `DESCRIPTION`, `LOCATION`, `COMMENT`, `CONTACT`, `CATEGORIES`,
//! `RESOURCES`). Marshal folds lines at 75 octets with CRLF line endings. Unmarshal of a
//! stream with several `VCALENDAR` objects gives an array. Option `validating` (default
//! false) checks that a VCALENDAR has `PRODID` and `VERSION`.

use std::sync::Arc;

use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::Result;
use cameleer_core::message::props::CONTENT_TYPE;
use cameleer_core::message::Exchange;
use cameleer_core::value::ObjectMap;
use cameleer_core::Value;

use crate::{body_text, df_err, Opts};

const TEXT_PROPS: [&str; 7] = ["SUMMARY", "DESCRIPTION", "LOCATION", "COMMENT", "CONTACT", "CATEGORIES", "RESOURCES"];

fn unescape_text(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn escape_text(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace('\n', "\\n")
}

/// Unfold content lines.
fn unfold(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let l = raw.strip_suffix('\r').unwrap_or(raw);
        if (l.starts_with(' ') || l.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&l[1..]);
        } else if !l.is_empty() {
            lines.push(l.to_string());
        }
    }
    lines
}

/// Split `NAME;P=V;P2="a:b":value` into (name, params, value).
fn parse_line(l: &str) -> Result<(String, ObjectMap, String)> {
    let mut in_q = false;
    let mut colon = None;
    for (i, c) in l.char_indices() {
        match c {
            '"' => in_q = !in_q,
            ':' if !in_q => {
                colon = Some(i);
                break;
            }
            _ => {}
        }
    }
    let colon = colon.ok_or_else(|| df_err(format!("Invalid iCalendar content line: {}", l)))?;
    let (head, value) = (&l[..colon], &l[colon + 1..]);
    let mut parts = Vec::new();
    let mut cur = String::new();
    in_q = false;
    for c in head.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            ';' if !in_q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts[0].to_ascii_uppercase();
    let mut params = ObjectMap::new();
    for p in &parts[1..] {
        if let Some((k, v)) = p.split_once('=') {
            params.insert(k.to_ascii_uppercase(), Value::String(v.trim_matches('"').to_string()));
        }
    }
    Ok((name, params, value.to_string()))
}

/// Parse iCalendar text into components.
pub fn parse(text: &str) -> Result<Vec<Value>> {
    let mut stack: Vec<(String, ObjectMap, Vec<Value>)> = Vec::new();
    let mut out = Vec::new();
    for l in unfold(text) {
        let (name, params, value) = parse_line(&l)?;
        match name.as_str() {
            "BEGIN" => stack.push((value.to_ascii_uppercase(), ObjectMap::new(), Vec::new())),
            "END" => {
                let (t, props, comps) = stack.pop().ok_or_else(|| df_err(format!("Unexpected END:{}", value)))?;
                if !t.eq_ignore_ascii_case(&value) {
                    return Err(df_err(format!("Expected END:{} but found END:{}", t, value)));
                }
                let mut m = ObjectMap::new();
                m.insert("type".into(), Value::String(t));
                m.insert("properties".into(), Value::Object(props));
                if !comps.is_empty() {
                    m.insert("components".into(), Value::Array(comps));
                }
                match stack.last_mut() {
                    Some(parent) => parent.2.push(Value::Object(m)),
                    None => out.push(Value::Object(m)),
                }
            }
            _ => {
                let (_, props, _) = stack.last_mut().ok_or_else(|| df_err(format!("Property {} outside of a component", name)))?;
                let v = if TEXT_PROPS.contains(&name.as_str()) { unescape_text(&value) } else { value };
                let pv = if params.is_empty() {
                    Value::String(v)
                } else {
                    Value::object([("value", Value::String(v)), ("params", Value::Object(params))])
                };
                match props.get_mut(&name) {
                    Some(Value::Array(a)) => a.push(pv),
                    Some(existing) => {
                        let prev = std::mem::take(existing);
                        *existing = Value::Array(vec![prev, pv]);
                    }
                    None => {
                        props.insert(name, pv);
                    }
                }
            }
        }
    }
    if let Some((t, ..)) = stack.last() {
        return Err(df_err(format!("Missing END:{}", t)));
    }
    Ok(out)
}

fn fold(line: &str, out: &mut String) {
    let mut count = 0;
    for c in line.chars() {
        let n = c.len_utf8();
        if count + n > 75 {
            out.push_str("\r\n ");
            count = 1;
        }
        out.push(c);
        count += n;
    }
    out.push_str("\r\n");
}

fn param_value(v: &str) -> String {
    if v.contains([':', ';', ',']) {
        format!("\"{}\"", v)
    } else {
        v.to_string()
    }
}

fn emit_prop(name: &str, v: &Value, out: &mut String) {
    if let Value::Array(a) = v {
        for x in a {
            emit_prop(name, x, out);
        }
        return;
    }
    let (value, params) = match v {
        Value::Object(o) => (o.get("value").map(|x| x.to_string()).unwrap_or_default(), o.get("params").and_then(|p| p.as_object()).cloned()),
        other => (other.to_string(), None),
    };
    let mut line = name.to_ascii_uppercase();
    if let Some(p) = params {
        for (k, pv) in p.iter() {
            line.push(';');
            line.push_str(&k.to_ascii_uppercase());
            line.push('=');
            line.push_str(&param_value(&pv.to_string()));
        }
    }
    line.push(':');
    if TEXT_PROPS.contains(&name.to_ascii_uppercase().as_str()) {
        line.push_str(&escape_text(&value));
    } else {
        line.push_str(&value);
    }
    fold(&line, out);
}

/// Emit a component as iCalendar text.
pub fn emit(c: &Value, out: &mut String) -> Result<()> {
    let o = c.as_object().ok_or_else(|| df_err("An iCalendar component must be an object"))?;
    let t = o.get("type").map(|x| x.to_string()).unwrap_or_else(|| "VCALENDAR".into()).to_ascii_uppercase();
    fold(&format!("BEGIN:{}", t), out);
    if let Some(p) = o.get("properties").and_then(|p| p.as_object()) {
        for (k, v) in p.iter() {
            emit_prop(k, v, out);
        }
    }
    if let Some(cs) = o.get("components").and_then(|c| c.as_array()) {
        for c in cs {
            emit(c, out)?;
        }
    }
    fold(&format!("END:{}", t), out);
    Ok(())
}

/// The `iCal` data format.
pub struct ICalDataFormat {
    pub validating: bool,
}

impl ICalDataFormat {
    fn validate(&self, c: &Value) -> Result<()> {
        if self.validating && c.get("type").and_then(|t| t.as_str()) == Some("VCALENDAR") {
            for p in ["PRODID", "VERSION"] {
                if c.get("properties").and_then(|x| x.get(p)).is_none() {
                    return Err(df_err(format!("Property [{}] must be specified once", p)));
                }
            }
        }
        Ok(())
    }
}

impl DataFormat for ICalDataFormat {
    fn name(&self) -> &str {
        "iCal"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let mut out = String::new();
        match &ex.message.body {
            Value::Array(a) => {
                for c in a {
                    self.validate(c)?;
                    emit(c, &mut out)?;
                }
            }
            c => {
                self.validate(c)?;
                emit(c, &mut out)?
            }
        }
        ex.message.body = Value::String(out);
        ex.message.headers.set(CONTENT_TYPE, "text/calendar");
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let mut cals = parse(&body_text(&ex.message.body)?)?;
        for c in &cals {
            self.validate(c)?;
        }
        ex.message.body = if cals.len() == 1 { cals.remove(0) } else { Value::Array(cals) };
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(ICalDataFormat { validating: o.bool(&["validating"], false)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAL: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//hacksw/handcal//NONSGML v1.0//EN\r\nBEGIN:VEVENT\r\nUID:19970610T172345Z-AF23B2@example.com\r\nDTSTAMP:19970610T172345Z\r\nDTSTART;TZID=America/New_York:19970714T170000\r\nDTEND:19970715T040000Z\r\nSUMMARY:Bastille Day Party\\, with friends\r\nDESCRIPTION:A very long description that certainly needs to be folded becaus\r\n e it is longer than seventy-five octets.\r\nATTENDEE;CN=\"Doe, John\":mailto:john@example.com\r\nATTENDEE:mailto:jane@example.com\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn parse_and_emit() {
        let v = parse(CAL).unwrap();
        assert_eq!(v.len(), 1);
        let ev = &v[0].get("components").unwrap().as_array().unwrap()[0];
        let p = ev.get("properties").unwrap();
        assert_eq!(p.get("SUMMARY").unwrap().as_str(), Some("Bastille Day Party, with friends"));
        assert!(p.get("DESCRIPTION").unwrap().as_str().unwrap().contains("because it is"));
        assert_eq!(p.get("DTSTART").unwrap().get("params").unwrap().get("TZID").unwrap().as_str(), Some("America/New_York"));
        assert_eq!(p.get("ATTENDEE").unwrap().as_array().unwrap().len(), 2);
        let mut out = String::new();
        emit(&v[0], &mut out).unwrap();
        assert!(out.lines().all(|l| l.len() <= 76));
        assert_eq!(parse(&out).unwrap(), v);
        assert!(out.contains("ATTENDEE;CN=\"Doe, John\":mailto:john@example.com"));
        assert!(parse("BEGIN:VCALENDAR\r\nEND:VEVENT\r\n").is_err());
    }
}
