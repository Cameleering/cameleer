//! The Simple Log Service `LogGroup` protobuf message (the `PutLogs` request body), written and
//! read by hand (proto2, from SLS's `log.proto`):
//!
//! ```text
//! message Log {
//!     required uint32 Time = 1;
//!     message Content { required string Key = 1; required string Value = 2; }
//!     repeated Content Contents = 2;
//!     optional fixed32 Time_ns = 4;
//! }
//! message LogTag { required string Key = 1; required string Value = 2; }
//! message LogGroup {
//!     repeated Log Logs = 1;
//!     optional string Reserved = 2;
//!     optional string Topic = 3;
//!     optional string Source = 4;
//!     repeated LogTag LogTags = 6;
//! }
//! ```
//!
//! [`LogGroup::from_value`] maps a message body to a log group, [`LogGroup::to_value`] back.

use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::{ObjectMap, Value};
use cameleer_grpc::proto::codec::{get_varint, put_varint};

/// One log: a time (Unix seconds, optional nanoseconds) and its key/value contents.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LogItem {
    pub time: u32,
    pub time_ns: Option<u32>,
    pub contents: Vec<(String, String)>,
}

/// A log group: logs with a shared topic, source and tags.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LogGroup {
    pub logs: Vec<LogItem>,
    pub topic: Option<String>,
    pub source: Option<String>,
    pub tags: Vec<(String, String)>,
}

fn put_tag(num: u32, wire: u8, out: &mut Vec<u8>) {
    put_varint(((num as u64) << 3) | wire as u64, out);
}

fn put_bytes(num: u32, data: &[u8], out: &mut Vec<u8>) {
    put_tag(num, 2, out);
    put_varint(data.len() as u64, out);
    out.extend_from_slice(data);
}

fn put_pair(num: u32, k: &str, v: &str, out: &mut Vec<u8>) {
    let mut m = Vec::with_capacity(k.len() + v.len() + 4);
    put_bytes(1, k.as_bytes(), &mut m);
    put_bytes(2, v.as_bytes(), &mut m);
    put_bytes(num, &m, out);
}

fn corrupt(what: &str) -> CamelError {
    CamelError::illegal_argument(format!("Invalid SLS LogGroup protobuf: {}", what))
}

/// The fields of a message: (number, wire type, payload). Varints are returned as their value
/// in 8 little-endian bytes, fixed32 as 4 bytes.
fn fields(buf: &[u8]) -> Result<Vec<(u32, u8, Vec<u8>)>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        let key = get_varint(buf, &mut pos).map_err(|_| corrupt("bad field key"))?;
        let (num, wire) = ((key >> 3) as u32, (key & 7) as u8);
        let payload = match wire {
            0 => get_varint(buf, &mut pos).map_err(|_| corrupt("bad varint"))?.to_le_bytes().to_vec(),
            1 => {
                let b = buf.get(pos..pos + 8).ok_or_else(|| corrupt("truncated fixed64"))?.to_vec();
                pos += 8;
                b
            }
            2 => {
                let n = get_varint(buf, &mut pos).map_err(|_| corrupt("bad length"))? as usize;
                let b = buf.get(pos..pos.checked_add(n).ok_or_else(|| corrupt("bad length"))?).ok_or_else(|| corrupt("truncated field"))?.to_vec();
                pos += n;
                b
            }
            5 => {
                let b = buf.get(pos..pos + 4).ok_or_else(|| corrupt("truncated fixed32"))?.to_vec();
                pos += 4;
                b
            }
            _ => return Err(corrupt("unsupported wire type")),
        };
        out.push((num, wire, payload));
    }
    Ok(out)
}

fn text(b: Vec<u8>) -> Result<String> {
    String::from_utf8(b).map_err(|_| corrupt("a string is not UTF-8"))
}

fn pair(buf: &[u8]) -> Result<(String, String)> {
    let (mut k, mut v) = (None, None);
    for (num, wire, p) in fields(buf)? {
        match (num, wire) {
            (1, 2) => k = Some(text(p)?),
            (2, 2) => v = Some(text(p)?),
            _ => {}
        }
    }
    Ok((k.ok_or_else(|| corrupt("missing Key"))?, v.ok_or_else(|| corrupt("missing Value"))?))
}

impl LogItem {
    fn encode(&self, out: &mut Vec<u8>) {
        put_tag(1, 0, out);
        put_varint(self.time as u64, out);
        for (k, v) in &self.contents {
            put_pair(2, k, v, out);
        }
        if let Some(ns) = self.time_ns {
            put_tag(4, 5, out);
            out.extend_from_slice(&ns.to_le_bytes());
        }
    }

    fn decode(buf: &[u8]) -> Result<LogItem> {
        let mut item = LogItem::default();
        let mut has_time = false;
        for (num, wire, p) in fields(buf)? {
            match (num, wire) {
                (1, 0) => {
                    item.time = u64::from_le_bytes(p.try_into().unwrap_or([0; 8])) as u32;
                    has_time = true;
                }
                (2, 2) => item.contents.push(pair(&p)?),
                (4, 5) => item.time_ns = Some(u32::from_le_bytes(p.try_into().unwrap_or([0; 4]))),
                _ => {}
            }
        }
        if !has_time {
            return Err(corrupt("a Log without Time"));
        }
        Ok(item)
    }
}

impl LogGroup {
    /// The protobuf encoding.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for log in &self.logs {
            let mut m = Vec::new();
            log.encode(&mut m);
            put_bytes(1, &m, &mut out);
        }
        if let Some(t) = &self.topic {
            put_bytes(3, t.as_bytes(), &mut out);
        }
        if let Some(s) = &self.source {
            put_bytes(4, s.as_bytes(), &mut out);
        }
        for (k, v) in &self.tags {
            put_pair(6, k, v, &mut out);
        }
        out
    }

    /// Decode the protobuf encoding.
    pub fn decode(buf: &[u8]) -> Result<LogGroup> {
        let mut g = LogGroup::default();
        for (num, wire, p) in fields(buf)? {
            match (num, wire) {
                (1, 2) => g.logs.push(LogItem::decode(&p)?),
                (3, 2) => g.topic = Some(text(p)?),
                (4, 2) => g.source = Some(text(p)?),
                (6, 2) => g.tags.push(pair(&p)?),
                _ => {}
            }
        }
        Ok(g)
    }

    /// A log group from a message body:
    /// * an object with `logItems` (the `LogGroup` model of the SLS SDK) or `logs`: `{"topic": "...",
    ///   "source": "...", "logTags": {..} or [{"key","value"}], "logItems": [log, ...]}`;
    /// * an array: the logs;
    /// * any other object: one log;
    /// * a string: a JSON document of one of the above, else one log with the text as `message`.
    ///
    /// A log is `{"time": secs, "timeNs": n, "contents": {..} or [{"key","value"}]}`, or a flat
    /// object whose entries are the contents (`time` / `__time__` taken as the time). A log
    /// without a time gets `now` (Unix seconds).
    pub fn from_value(v: &Value, now: u32) -> Result<LogGroup> {
        match v {
            Value::Null => Err(CamelError::illegal_argument("putLogs needs a body: a LogGroup, a list of logs, a map or a text")),
            Value::String(s) => {
                let t = s.trim();
                if t.starts_with('{') || t.starts_with('[') {
                    if let Ok(parsed) = cameleer_core::json::parse(t) {
                        return LogGroup::from_value(&parsed, now);
                    }
                }
                Ok(LogGroup { logs: vec![LogItem { time: now, time_ns: None, contents: vec![("message".into(), s.to_string())] }], ..Default::default() })
            }
            Value::Bytes(b) => LogGroup::from_value(&Value::String(String::from_utf8_lossy(b).into_owned()), now),
            Value::Array(items) => Ok(LogGroup { logs: items.iter().map(|i| log_from_value(i, now)).collect::<Result<_>>()?, ..Default::default() }),
            Value::Object(m) => {
                let Some(logs) = m.get("logs").or_else(|| m.get("logItems")).or_else(|| m.get("Logs")).or_else(|| m.get("LogItems")) else {
                    return Ok(LogGroup { logs: vec![log_from_value(v, now)?], ..Default::default() });
                };
                let logs = match logs {
                    Value::Array(a) => a.iter().map(|i| log_from_value(i, now)).collect::<Result<Vec<_>>>()?,
                    other => vec![log_from_value(other, now)?],
                };
                let s = |k: &str, k2: &str| m.get(k).or_else(|| m.get(k2)).filter(|x| !x.is_null()).map(|x| x.to_string());
                let tags = m.get("logTags").or_else(|| m.get("LogTags")).map(pairs).transpose()?.unwrap_or_default();
                Ok(LogGroup { logs, topic: s("topic", "Topic"), source: s("source", "Source"), tags })
            }
            other => Ok(LogGroup { logs: vec![LogItem { time: now, time_ns: None, contents: vec![("message".into(), other.to_string())] }], ..Default::default() }),
        }
    }

    /// The log group as a value in the shape [`LogGroup::from_value`] reads.
    pub fn to_value(&self) -> Value {
        let mut m = ObjectMap::new();
        if let Some(t) = &self.topic {
            m.insert("topic".into(), Value::from(t.clone()));
        }
        if let Some(s) = &self.source {
            m.insert("source".into(), Value::from(s.clone()));
        }
        m.insert("logTags".into(), Value::Object(self.tags.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()));
        let logs = self
            .logs
            .iter()
            .map(|l| {
                let mut o = ObjectMap::new();
                o.insert("time".into(), Value::Int(l.time as i64));
                if let Some(ns) = l.time_ns {
                    o.insert("timeNs".into(), Value::Int(ns as i64));
                }
                o.insert("contents".into(), Value::Object(l.contents.iter().map(|(k, v)| (k.clone(), Value::from(v.clone()))).collect()));
                Value::Object(o)
            })
            .collect();
        m.insert("logs".into(), Value::Array(logs));
        Value::Object(m)
    }
}

fn scalar(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Object(_) | Value::Array(_) => v.to_json(),
        other => other.to_string(),
    }
}

/// Key/value pairs from an object or a list of `{"key","value"}` objects.
fn pairs(v: &Value) -> Result<Vec<(String, String)>> {
    match v {
        Value::Object(m) => Ok(m.iter().map(|(k, v)| (k.to_string(), scalar(v))).collect()),
        Value::Array(a) => a
            .iter()
            .map(|e| {
                let k = e.get("key").or_else(|| e.get("Key")).ok_or_else(|| CamelError::illegal_argument("A log content or tag needs a key"))?;
                Ok((k.to_string(), e.get("value").or_else(|| e.get("Value")).map(scalar).unwrap_or_default()))
            })
            .collect(),
        Value::Null => Ok(Vec::new()),
        _ => Err(CamelError::illegal_argument("Log contents and tags are a map or a list of {key, value}")),
    }
}

fn int(v: &Value, what: &str) -> Result<i64> {
    v.as_i64().or_else(|| v.to_string().trim().parse().ok()).ok_or_else(|| CamelError::illegal_argument(format!("The log {} must be a number: {}", what, v)))
}

fn log_from_value(v: &Value, now: u32) -> Result<LogItem> {
    let Value::Object(m) = v else {
        return Ok(LogItem { time: now, time_ns: None, contents: vec![("message".into(), scalar(v))] });
    };
    let time = m.get("time").or_else(|| m.get("Time")).or_else(|| m.get("__time__")).filter(|t| !t.is_null()).map(|t| int(t, "time")).transpose()?;
    let time = match time {
        Some(t) if !(0..=u32::MAX as i64).contains(&t) => return Err(CamelError::illegal_argument(format!("The log time is out of range (Unix seconds): {}", t))),
        Some(t) => t as u32,
        None => now,
    };
    let time_ns = m.get("timeNs").or_else(|| m.get("Time_ns")).filter(|t| !t.is_null()).map(|t| int(t, "timeNs").map(|n| n as u32)).transpose()?;
    let contents = match m.get("contents").or_else(|| m.get("Contents")) {
        Some(c) => pairs(c)?,
        None => m.iter().filter(|(k, _)| !matches!(k.as_ref(), "time" | "Time" | "__time__" | "timeNs" | "Time_ns")).map(|(k, v)| (k.to_string(), scalar(v))).collect(),
    };
    Ok(LogItem { time, time_ns, contents })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let g = LogGroup {
            logs: vec![
                LogItem { time: 1_700_000_000, time_ns: Some(5), contents: vec![("message".into(), "hello".into()), ("level".into(), "INFO".into())] },
                LogItem { time: 1_700_000_001, time_ns: None, contents: vec![] },
            ],
            topic: Some("camel".into()),
            source: Some("10.0.0.1".into()),
            tags: vec![("env".into(), "test".into())],
        };
        let bytes = g.encode();
        assert_eq!(LogGroup::decode(&bytes).unwrap(), g);
        // Log { Time: 1, Contents: [{Key:"k", Value:"v"}] } inside LogGroup.Logs, by hand
        let one = LogGroup { logs: vec![LogItem { time: 1, time_ns: None, contents: vec![("k".into(), "v".into())] }], ..Default::default() };
        assert_eq!(one.encode(), vec![0x0a, 0x0a, 0x08, 0x01, 0x12, 0x06, 0x0a, 0x01, b'k', 0x12, 0x01, b'v']);
        assert!(LogGroup::decode(&[0x0a, 0x05, 0x08]).is_err());
    }

    #[test]
    fn bodies_to_log_groups() {
        let g = LogGroup::from_value(&Value::from("plain text"), 42).unwrap();
        assert_eq!(g.logs, vec![LogItem { time: 42, time_ns: None, contents: vec![("message".into(), "plain text".into())] }]);
        let g = LogGroup::from_value(&cameleer_core::json::parse(r#"{"topic":"t","logTags":[{"key":"a","value":"b"}],"logs":[{"time":7,"contents":[{"key":"k","value":"v"}]},{"msg":"x","n":3}]}"#).unwrap(), 9).unwrap();
        assert_eq!(g.topic.as_deref(), Some("t"));
        assert_eq!(g.tags, vec![("a".into(), "b".into())]);
        assert_eq!(g.logs[0], LogItem { time: 7, time_ns: None, contents: vec![("k".into(), "v".into())] });
        assert_eq!(g.logs[1].time, 9);
        assert_eq!(g.logs[1].contents, vec![("msg".into(), "x".into()), ("n".into(), "3".into())]);
        let g = LogGroup::from_value(&Value::from(r#"[{"a":"1","time":"5"}]"#), 9).unwrap();
        assert_eq!(g.logs[0], LogItem { time: 5, time_ns: None, contents: vec![("a".into(), "1".into())] });
        assert!(LogGroup::from_value(&Value::Null, 0).is_err());
        assert!(LogGroup::from_value(&cameleer_core::json::parse(r#"{"time":-1}"#).unwrap(), 0).is_err());
        let back = LogGroup::from_value(&g.to_value(), 0).unwrap();
        assert_eq!(back, g);
    }
}
