//! OTLP protobuf encoding of spans (`opentelemetry/proto/collector/trace/v1/trace_service.proto`
//! and `opentelemetry/proto/trace/v1/trace.proto`), and a decoder used by the embedded receiver
//! of the tests.
//!
//! Field numbers, from opentelemetry-proto:
//!
//! | message | fields |
//! |---|---|
//! | `ExportTraceServiceRequest` | `resource_spans` 1 |
//! | `ResourceSpans` | `resource` 1, `scope_spans` 2, `schema_url` 3 |
//! | `Resource` | `attributes` 1, `dropped_attributes_count` 2 |
//! | `ScopeSpans` | `scope` 1, `spans` 2, `schema_url` 3 |
//! | `InstrumentationScope` | `name` 1, `version` 2, `attributes` 3 |
//! | `Span` | `trace_id` 1, `span_id` 2, `trace_state` 3, `parent_span_id` 4, `name` 5, `kind` 6, `start_time_unix_nano` 7 (fixed64), `end_time_unix_nano` 8 (fixed64), `attributes` 9, `dropped_attributes_count` 10, `events` 11, `dropped_events_count` 12, `status` 15, `flags` 16 (fixed32) |
//! | `Span.Event` | `time_unix_nano` 1 (fixed64), `name` 2, `attributes` 3 |
//! | `Status` | `message` 2, `code` 3 |
//! | `KeyValue` | `key` 1, `value` 2 |
//! | `AnyValue` | `string_value` 1, `bool_value` 2, `int_value` 3, `double_value` 4, `array_value` 5, `kvlist_value` 6, `bytes_value` 7 |
//! | `ArrayValue` | `values` 1 |
//! | `ExportTraceServiceResponse` | `partial_success` 1; `ExportTracePartialSuccess`: `rejected_spans` 1, `error_message` 2 |

use cameleer_core::telemetry::span::{AttrValue, Attributes, SpanData, SpanEvent, SpanKind, StatusCode};

/// `Span.flags` bit: the `is_remote` bit below is set (SPAN_FLAGS_CONTEXT_HAS_IS_REMOTE_MASK).
pub const FLAG_HAS_IS_REMOTE: u32 = 0x100;
/// `Span.flags` bit: the parent is remote (SPAN_FLAGS_CONTEXT_IS_REMOTE_MASK).
pub const FLAG_IS_REMOTE: u32 = 0x200;

const VARINT: u8 = 0;
const FIXED64: u8 = 1;
const LEN: u8 = 2;
const FIXED32: u8 = 5;

/// A protobuf writer.
#[derive(Default)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
    }
    fn tag(&mut self, field: u32, wire: u8) {
        self.varint(((field as u64) << 3) | wire as u64);
    }
    pub fn uint(&mut self, field: u32, v: u64) {
        if v != 0 {
            self.tag(field, VARINT);
            self.varint(v);
        }
    }
    pub fn int(&mut self, field: u32, v: i64) {
        if v != 0 {
            self.tag(field, VARINT);
            self.varint(v as u64);
        }
    }
    pub fn bool(&mut self, field: u32, v: bool) {
        self.tag(field, VARINT);
        self.varint(v as u64);
    }
    pub fn fixed64(&mut self, field: u32, v: u64) {
        if v != 0 {
            self.tag(field, FIXED64);
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    pub fn double(&mut self, field: u32, v: f64) {
        self.tag(field, FIXED64);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// A fixed64 (or sfixed64) field, also written when zero.
    pub fn fixed64_always(&mut self, field: u32, v: u64) {
        self.tag(field, FIXED64);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    /// A packed repeated fixed64 field (written even when empty only if `v` is non-empty).
    pub fn packed_fixed64(&mut self, field: u32, v: &[u64]) {
        if v.is_empty() {
            return;
        }
        self.tag(field, LEN);
        self.varint((v.len() * 8) as u64);
        for x in v {
            self.buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    /// A packed repeated double field.
    pub fn packed_double(&mut self, field: u32, v: &[f64]) {
        if v.is_empty() {
            return;
        }
        self.tag(field, LEN);
        self.varint((v.len() * 8) as u64);
        for x in v {
            self.buf.extend_from_slice(&x.to_le_bytes());
        }
    }
    pub fn fixed32(&mut self, field: u32, v: u32) {
        if v != 0 {
            self.tag(field, FIXED32);
            self.buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    pub fn bytes(&mut self, field: u32, v: &[u8]) {
        if !v.is_empty() {
            self.tag(field, LEN);
            self.varint(v.len() as u64);
            self.buf.extend_from_slice(v);
        }
    }
    pub fn string(&mut self, field: u32, v: &str) {
        self.bytes(field, v.as_bytes())
    }
    /// A nested message, also written when empty.
    pub fn message(&mut self, field: u32, f: impl FnOnce(&mut Writer)) {
        let mut w = Writer::default();
        f(&mut w);
        self.tag(field, LEN);
        self.varint(w.buf.len() as u64);
        self.buf.extend_from_slice(&w.buf);
    }
}

fn any_value(w: &mut Writer, v: &AttrValue) {
    match v {
        AttrValue::String(s) => {
            // an empty string is still a string value
            w.tag(1, LEN);
            w.varint(s.len() as u64);
            w.buf.extend_from_slice(s.as_bytes());
        }
        AttrValue::Bool(b) => w.bool(2, *b),
        AttrValue::Int(i) => {
            w.tag(3, VARINT);
            w.varint(*i as u64);
        }
        AttrValue::Double(d) => w.double(4, *d),
        AttrValue::StringArray(a) => w.message(5, |w| {
            for s in a {
                w.message(1, |w| any_value(w, &AttrValue::String(s.clone())));
            }
        }),
    }
}

/// Writes `KeyValue`s as repeated field `field`.
pub fn attributes(w: &mut Writer, field: u32, attrs: &Attributes) {
    for (k, v) in attrs {
        w.message(field, |w| {
            w.string(1, k);
            w.message(2, |w| any_value(w, v));
        });
    }
}

fn span(w: &mut Writer, s: &SpanData) {
    w.bytes(1, &s.trace_id);
    w.bytes(2, &s.span_id);
    w.string(3, &s.trace_state);
    if !s.is_root() {
        w.bytes(4, &s.parent_span_id);
    }
    w.string(5, &s.name);
    w.uint(6, s.kind as u64);
    w.fixed64(7, s.start_unix_nano);
    w.fixed64(8, s.end_unix_nano);
    attributes(w, 9, &s.shared_attributes);
    attributes(w, 9, &s.attributes);
    w.uint(10, s.dropped_attributes as u64);
    for e in &s.events {
        w.message(11, |w| {
            w.fixed64(1, e.time_unix_nano);
            w.string(2, &e.name);
            attributes(w, 3, &e.attributes);
        });
    }
    if s.status != StatusCode::Unset {
        w.message(15, |w| {
            w.string(2, &s.status_message);
            w.uint(3, s.status as u64);
        });
    }
    let mut flags = s.flags as u32;
    if !s.is_root() {
        flags |= FLAG_HAS_IS_REMOTE;
        if s.parent_remote {
            flags |= FLAG_IS_REMOTE;
        }
    }
    w.fixed32(16, flags);
}

/// Encodes an `ExportTraceServiceRequest` with one resource; spans are grouped by their
/// instrumentation scope, in order of first appearance.
pub fn encode_export_request(resource: &Attributes, scope_version: &str, spans: &[SpanData]) -> Vec<u8> {
    let mut scopes: Vec<&str> = Vec::new();
    for s in spans {
        if !scopes.contains(&&*s.scope_name) {
            scopes.push(&s.scope_name);
        }
    }
    let mut w = Writer::default();
    w.message(1, |w| {
        w.message(1, |w| attributes(w, 1, resource));
        for scope in scopes {
            w.message(2, |w| {
                w.message(1, |w| {
                    w.string(1, scope);
                    w.string(2, scope_version);
                });
                for s in spans.iter().filter(|s| &*s.scope_name == scope) {
                    w.message(2, |w| span(w, s));
                }
            });
        }
    });
    w.buf
}

// ---------------------------------------------------------------------------
// decoding
// ---------------------------------------------------------------------------

/// A decoded protobuf field value.
#[derive(Clone, Debug)]
pub enum Field {
    Varint(u64),
    Fixed64(u64),
    Fixed32(u32),
    Bytes(Vec<u8>),
}

/// Splits a message into `(field number, value)` pairs.
pub fn fields(mut b: &[u8]) -> Result<Vec<(u32, Field)>, String> {
    fn varint(b: &mut &[u8]) -> Result<u64, String> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let (&c, rest) = b.split_first().ok_or("truncated varint")?;
            *b = rest;
            v |= ((c & 0x7f) as u64) << shift;
            if c < 0x80 {
                return Ok(v);
            }
        }
        Err("varint too long".into())
    }
    let mut out = Vec::new();
    while !b.is_empty() {
        let key = varint(&mut b)?;
        let field = (key >> 3) as u32;
        let take = |b: &mut &[u8], n: usize| -> Result<Vec<u8>, String> {
            if b.len() < n {
                return Err("truncated field".into());
            }
            let (a, rest) = b.split_at(n);
            *b = rest;
            Ok(a.to_vec())
        };
        let v = match (key & 7) as u8 {
            VARINT => Field::Varint(varint(&mut b)?),
            FIXED64 => Field::Fixed64(u64::from_le_bytes(take(&mut b, 8)?.try_into().unwrap())),
            FIXED32 => Field::Fixed32(u32::from_le_bytes(take(&mut b, 4)?.try_into().unwrap())),
            LEN => {
                let n = varint(&mut b)? as usize;
                Field::Bytes(take(&mut b, n)?)
            }
            w => return Err(format!("unsupported wire type {w}")),
        };
        out.push((field, v));
    }
    Ok(out)
}

fn bytes_of(f: &Field) -> &[u8] {
    match f {
        Field::Bytes(b) => b,
        _ => &[],
    }
}

fn str_of(f: &Field) -> String {
    String::from_utf8_lossy(bytes_of(f)).into_owned()
}

fn num_of(f: &Field) -> u64 {
    match f {
        Field::Varint(v) | Field::Fixed64(v) => *v,
        Field::Fixed32(v) => *v as u64,
        Field::Bytes(_) => 0,
    }
}

fn decode_any(b: &[u8]) -> Result<AttrValue, String> {
    let mut v = AttrValue::String(String::new());
    for (n, f) in fields(b)? {
        v = match n {
            1 => AttrValue::String(str_of(&f)),
            2 => AttrValue::Bool(num_of(&f) != 0),
            3 => AttrValue::Int(num_of(&f) as i64),
            4 => AttrValue::Double(f64::from_bits(num_of(&f))),
            5 => {
                let mut a = Vec::new();
                for (n2, f2) in fields(bytes_of(&f))? {
                    if n2 == 1 {
                        a.push(decode_any(bytes_of(&f2))?.to_string());
                    }
                }
                AttrValue::StringArray(a)
            }
            _ => v,
        };
    }
    Ok(v)
}

fn decode_kv(b: &[u8]) -> Result<(String, AttrValue), String> {
    let mut k = String::new();
    let mut v = AttrValue::String(String::new());
    for (n, f) in fields(b)? {
        match n {
            1 => k = str_of(&f),
            2 => v = decode_any(bytes_of(&f))?,
            _ => {}
        }
    }
    Ok((k, v))
}

/// A span as received, with its resource and scope.
#[derive(Clone, Debug)]
pub struct ReceivedSpan {
    pub resource: Attributes,
    pub scope_name: String,
    pub scope_version: String,
    pub span: SpanData,
    /// The raw `Span.flags`.
    pub raw_flags: u32,
}

fn kind_of(v: u64) -> SpanKind {
    match v {
        2 => SpanKind::Server,
        3 => SpanKind::Client,
        4 => SpanKind::Producer,
        5 => SpanKind::Consumer,
        _ => SpanKind::Internal,
    }
}

fn decode_span(b: &[u8], scope: &str) -> Result<(SpanData, u32), String> {
    let mut s = SpanData {
        trace_id: [0; 16],
        span_id: [0; 8],
        parent_span_id: [0; 8],
        parent_remote: false,
        trace_state: String::new(),
        flags: 0,
        name: String::new(),
        kind: SpanKind::Internal,
        start_unix_nano: 0,
        end_unix_nano: 0,
        attributes: Vec::new(),
        shared_attributes: Default::default(),
        dropped_attributes: 0,
        events: Vec::new(),
        status: StatusCode::Unset,
        status_message: String::new(),
        scope_name: scope.into(),
    };
    let mut raw = 0;
    for (n, f) in fields(b)? {
        match n {
            1 => s.trace_id = bytes_of(&f).try_into().map_err(|_| "bad trace_id")?,
            2 => s.span_id = bytes_of(&f).try_into().map_err(|_| "bad span_id")?,
            3 => s.trace_state = str_of(&f),
            4 => s.parent_span_id = bytes_of(&f).try_into().map_err(|_| "bad parent_span_id")?,
            5 => s.name = str_of(&f),
            6 => s.kind = kind_of(num_of(&f)),
            7 => s.start_unix_nano = num_of(&f),
            8 => s.end_unix_nano = num_of(&f),
            9 => s.attributes.push(decode_kv(bytes_of(&f))?),
            10 => s.dropped_attributes = num_of(&f) as u32,
            11 => {
                let mut e = SpanEvent { name: String::new(), time_unix_nano: 0, attributes: Vec::new() };
                for (n2, f2) in fields(bytes_of(&f))? {
                    match n2 {
                        1 => e.time_unix_nano = num_of(&f2),
                        2 => e.name = str_of(&f2),
                        3 => e.attributes.push(decode_kv(bytes_of(&f2))?),
                        _ => {}
                    }
                }
                s.events.push(e);
            }
            15 => {
                for (n2, f2) in fields(bytes_of(&f))? {
                    match n2 {
                        2 => s.status_message = str_of(&f2),
                        3 => {
                            s.status = match num_of(&f2) {
                                1 => StatusCode::Ok,
                                2 => StatusCode::Error,
                                _ => StatusCode::Unset,
                            }
                        }
                        _ => {}
                    }
                }
            }
            16 => {
                raw = num_of(&f) as u32;
                s.flags = raw as u8;
                s.parent_remote = raw & FLAG_IS_REMOTE != 0;
            }
            _ => {}
        }
    }
    Ok((s, raw))
}

/// Decodes an `ExportTraceServiceRequest`.
pub fn decode_export_request(b: &[u8]) -> Result<Vec<ReceivedSpan>, String> {
    let mut out = Vec::new();
    for (n, rs) in fields(b)? {
        if n != 1 {
            continue;
        }
        let rs = fields(bytes_of(&rs))?;
        let mut resource = Vec::new();
        for (n, f) in &rs {
            if *n == 1 {
                for (n2, f2) in fields(bytes_of(f))? {
                    if n2 == 1 {
                        resource.push(decode_kv(bytes_of(&f2))?);
                    }
                }
            }
        }
        for (n, ss) in &rs {
            if *n != 2 {
                continue;
            }
            let ss = fields(bytes_of(ss))?;
            let (mut name, mut version) = (String::new(), String::new());
            for (n2, f2) in &ss {
                if *n2 == 1 {
                    for (n3, f3) in fields(bytes_of(f2))? {
                        match n3 {
                            1 => name = str_of(&f3),
                            2 => version = str_of(&f3),
                            _ => {}
                        }
                    }
                }
            }
            for (n2, f2) in &ss {
                if *n2 == 2 {
                    let (span, raw_flags) = decode_span(bytes_of(f2), &name)?;
                    out.push(ReceivedSpan { resource: resource.clone(), scope_name: name.clone(), scope_version: version.clone(), span, raw_flags });
                }
            }
        }
    }
    Ok(out)
}

/// Encodes an `ExportTraceServiceResponse` with a partial success (test receivers).
pub fn encode_partial_success(rejected: i64, message: &str) -> Vec<u8> {
    let mut w = Writer::default();
    if rejected != 0 || !message.is_empty() {
        w.message(1, |w| {
            w.int(1, rejected);
            w.string(2, message);
        });
    }
    w.buf
}

/// Decodes the partial success of an `ExportTraceServiceResponse`: (rejected spans, message).
pub fn decode_partial_success(b: &[u8]) -> (i64, String) {
    let mut out = (0, String::new());
    if let Ok(fs) = fields(b) {
        for (n, f) in fs {
            if n == 1 {
                if let Ok(ps) = fields(bytes_of(&f)) {
                    for (n2, f2) in ps {
                        match n2 {
                            1 => out.0 = num_of(&f2) as i64,
                            2 => out.1 = str_of(&f2),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SpanData {
        SpanData {
            trace_id: [1; 16],
            span_id: [2; 8],
            parent_span_id: [3; 8],
            parent_remote: true,
            trace_state: "a=b".into(),
            flags: 1,
            name: "GET".into(),
            kind: SpanKind::Server,
            start_unix_nano: 1_700_000_000_000_000_000,
            end_unix_nano: 1_700_000_000_500_000_000,
            attributes: vec![
                ("camel.uri".into(), "platform-http:///x".into()),
                ("http.status_code".into(), AttrValue::Int(200)),
                ("error".into(), AttrValue::Bool(false)),
                ("ratio".into(), AttrValue::Double(0.5)),
                ("empty".into(), AttrValue::String(String::new())),
                ("list".into(), AttrValue::StringArray(vec!["x".into(), "y".into()])),
            ],
            shared_attributes: Default::default(),
            dropped_attributes: 2,
            events: vec![SpanEvent { name: "exception".into(), time_unix_nano: 5, attributes: vec![("exception.message".into(), "boom".into())] }],
            status: StatusCode::Error,
            status_message: "boom".into(),
            scope_name: "camel".into(),
        }
    }

    #[test]
    fn export_request_round_trips() {
        let s = sample();
        let mut root = sample();
        root.parent_span_id = [0; 8];
        root.scope_name = "other".into();
        let res = vec![("service.name".to_string(), AttrValue::from("svc"))];
        let b = encode_export_request(&res, "0.8.0", &[s.clone(), root.clone()]);
        let got = decode_export_request(&b).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].span, s);
        assert_eq!(got[0].raw_flags, 1 | FLAG_HAS_IS_REMOTE | FLAG_IS_REMOTE);
        assert_eq!(got[0].scope_version, "0.8.0");
        assert_eq!(got[0].resource, res);
        assert_eq!(got[1].scope_name, "other");
        assert!(got[1].span.is_root());
        assert_eq!(got[1].raw_flags, 1);
    }

    #[test]
    fn known_bytes() {
        // KeyValue{key:"a", value:{int_value:150}}: 0a 01 61 12 03 18 96 01
        let mut w = Writer::default();
        attributes(&mut w, 1, &vec![("a".into(), AttrValue::Int(150))]);
        assert_eq!(w.buf, [0x0a, 0x08, 0x0a, 0x01, b'a', 0x12, 0x03, 0x18, 0x96, 0x01]);
        assert_eq!(decode_partial_success(&encode_partial_success(3, "bad")), (3, "bad".to_string()));
        assert_eq!(decode_partial_success(&[]), (0, String::new()));
    }
}
