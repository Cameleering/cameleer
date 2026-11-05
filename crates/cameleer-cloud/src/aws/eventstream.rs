//! The AWS **event stream** binary framing (`application/vnd.amazon.eventstream`), used by
//! the streaming APIs (Bedrock `InvokeModelWithResponseStream` / `ConverseStream`, Bedrock
//! Agent Runtime `InvokeAgent`, Kinesis `SubscribeToShard`, Transcribe streaming...).
//!
//! A message is `total length (u32 BE) | headers length (u32 BE) | prelude CRC32 |
//! headers | payload | message CRC32`; both CRCs are IEEE CRC32, the prelude CRC over the
//! first 8 bytes and the message CRC over everything before it. Headers are
//! `name length (u8) | name | type (u8) | value` with the types of the specification
//! (bool true/false, byte, short, int, long, bytes, string, timestamp, uuid).
//!
//! [`Decoder`] is incremental: it implements [`std::io::Write`] so a response body can be
//! streamed into it (`HttpClient::send_to`) and each complete message is handed to a callback
//! as soon as its last byte arrives.

use std::io;

use cameleer_core::dataformat::compress::crc32;
use cameleer_core::error::{CamelError, Result};
use cameleer_core::value::Value;

use crate::util::{cloud_error, parse_json};

/// A header value.
#[derive(Clone, Debug, PartialEq)]
pub enum HeaderValue {
    Bool(bool),
    Byte(i8),
    Short(i16),
    Int(i32),
    Long(i64),
    Bytes(Vec<u8>),
    String(String),
    Timestamp(i64),
    Uuid([u8; 16]),
}

impl HeaderValue {
    /// The value as text (strings as is, others formatted).
    pub fn as_text(&self) -> String {
        match self {
            HeaderValue::String(s) => s.clone(),
            HeaderValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            HeaderValue::Bool(b) => b.to_string(),
            HeaderValue::Byte(n) => n.to_string(),
            HeaderValue::Short(n) => n.to_string(),
            HeaderValue::Int(n) => n.to_string(),
            HeaderValue::Long(n) | HeaderValue::Timestamp(n) => n.to_string(),
            HeaderValue::Uuid(u) => crate::util::hex(u),
        }
    }
}

/// One event stream message.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Message {
    pub headers: Vec<(String, HeaderValue)>,
    pub payload: Vec<u8>,
}

impl Message {
    /// An `event` message of `event_type` with a JSON payload.
    pub fn event(event_type: &str, payload: &Value) -> Message {
        Message {
            headers: vec![
                (":event-type".into(), HeaderValue::String(event_type.into())),
                (":content-type".into(), HeaderValue::String("application/json".into())),
                (":message-type".into(), HeaderValue::String("event".into())),
            ],
            payload: payload.to_json().into_bytes(),
        }
    }

    /// An `exception` message.
    pub fn exception(exception_type: &str, message: &str) -> Message {
        Message {
            headers: vec![
                (":exception-type".into(), HeaderValue::String(exception_type.into())),
                (":content-type".into(), HeaderValue::String("application/json".into())),
                (":message-type".into(), HeaderValue::String("exception".into())),
            ],
            payload: Value::object([("message", Value::from(message))]).to_json().into_bytes(),
        }
    }

    /// A header as text.
    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_text())
    }

    /// `:message-type` (`event`, `exception` or `error`).
    pub fn message_type(&self) -> String {
        self.header(":message-type").unwrap_or_else(|| "event".into())
    }

    /// `:event-type`.
    pub fn event_type(&self) -> String {
        self.header(":event-type").unwrap_or_default()
    }

    /// The payload parsed as JSON (`Null` when empty).
    pub fn json(&self) -> Result<Value> {
        if self.payload.is_empty() {
            Ok(Value::Null)
        } else {
            parse_json(&self.payload)
        }
    }

    /// An exception / error message as a Camel error of `kind`; `None` for events.
    pub fn error(&self, kind: &str) -> Option<CamelError> {
        match self.message_type().as_str() {
            "exception" => {
                let msg = self.json().ok().and_then(|v| v.get("message").or_else(|| v.get("Message")).map(|m| m.to_string())).unwrap_or_else(|| String::from_utf8_lossy(&self.payload).into_owned());
                Some(cloud_error(kind, format!("{}: {}", self.header(":exception-type").unwrap_or_default(), msg)))
            }
            "error" => Some(cloud_error(kind, format!("{}: {}", self.header(":error-code").unwrap_or_default(), self.header(":error-message").unwrap_or_default()))),
            _ => None,
        }
    }

    /// Encode the message (with both CRCs).
    pub fn encode(&self) -> Vec<u8> {
        let mut h = Vec::new();
        for (name, v) in &self.headers {
            h.push(name.len() as u8);
            h.extend_from_slice(name.as_bytes());
            match v {
                HeaderValue::Bool(true) => h.push(0),
                HeaderValue::Bool(false) => h.push(1),
                HeaderValue::Byte(n) => {
                    h.push(2);
                    h.push(*n as u8);
                }
                HeaderValue::Short(n) => {
                    h.push(3);
                    h.extend_from_slice(&n.to_be_bytes());
                }
                HeaderValue::Int(n) => {
                    h.push(4);
                    h.extend_from_slice(&n.to_be_bytes());
                }
                HeaderValue::Long(n) => {
                    h.push(5);
                    h.extend_from_slice(&n.to_be_bytes());
                }
                HeaderValue::Bytes(b) => {
                    h.push(6);
                    h.extend_from_slice(&(b.len() as u16).to_be_bytes());
                    h.extend_from_slice(b);
                }
                HeaderValue::String(s) => {
                    h.push(7);
                    h.extend_from_slice(&(s.len() as u16).to_be_bytes());
                    h.extend_from_slice(s.as_bytes());
                }
                HeaderValue::Timestamp(n) => {
                    h.push(8);
                    h.extend_from_slice(&n.to_be_bytes());
                }
                HeaderValue::Uuid(u) => {
                    h.push(9);
                    h.extend_from_slice(u);
                }
            }
        }
        let total = 12 + h.len() + self.payload.len() + 4;
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&(total as u32).to_be_bytes());
        out.extend_from_slice(&(h.len() as u32).to_be_bytes());
        let pc = crc32(&out[..8]);
        out.extend_from_slice(&pc.to_be_bytes());
        out.extend_from_slice(&h);
        out.extend_from_slice(&self.payload);
        let mc = crc32(&out);
        out.extend_from_slice(&mc.to_be_bytes());
        out
    }
}

fn bad(msg: impl Into<String>) -> CamelError {
    CamelError::io(format!("Invalid event stream message: {}", msg.into()))
}

/// Decode one complete message (exactly `data`).
pub fn decode_message(data: &[u8]) -> Result<Message> {
    if data.len() < 16 {
        return Err(bad("too short"));
    }
    let total = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
    let hlen = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    if total != data.len() || 12 + hlen + 4 > total {
        return Err(bad("length mismatch"));
    }
    if crc32(&data[..8]) != u32::from_be_bytes(data[8..12].try_into().unwrap()) {
        return Err(bad("prelude CRC mismatch"));
    }
    if crc32(&data[..total - 4]) != u32::from_be_bytes(data[total - 4..].try_into().unwrap()) {
        return Err(bad("message CRC mismatch"));
    }
    let mut headers = Vec::new();
    let h = &data[12..12 + hlen];
    let mut i = 0;
    let take = |i: &mut usize, n: usize| -> Result<&[u8]> {
        if *i + n > h.len() {
            return Err(bad("truncated header"));
        }
        let s = &h[*i..*i + n];
        *i += n;
        Ok(s)
    };
    while i < h.len() {
        let nlen = take(&mut i, 1)?[0] as usize;
        let name = String::from_utf8_lossy(take(&mut i, nlen)?).into_owned();
        let t = take(&mut i, 1)?[0];
        let v = match t {
            0 => HeaderValue::Bool(true),
            1 => HeaderValue::Bool(false),
            2 => HeaderValue::Byte(take(&mut i, 1)?[0] as i8),
            3 => HeaderValue::Short(i16::from_be_bytes(take(&mut i, 2)?.try_into().unwrap())),
            4 => HeaderValue::Int(i32::from_be_bytes(take(&mut i, 4)?.try_into().unwrap())),
            5 => HeaderValue::Long(i64::from_be_bytes(take(&mut i, 8)?.try_into().unwrap())),
            8 => HeaderValue::Timestamp(i64::from_be_bytes(take(&mut i, 8)?.try_into().unwrap())),
            6 | 7 => {
                let l = u16::from_be_bytes(take(&mut i, 2)?.try_into().unwrap()) as usize;
                let b = take(&mut i, l)?.to_vec();
                if t == 6 {
                    HeaderValue::Bytes(b)
                } else {
                    HeaderValue::String(String::from_utf8_lossy(&b).into_owned())
                }
            }
            9 => HeaderValue::Uuid(take(&mut i, 16)?.try_into().unwrap()),
            other => return Err(bad(format!("unknown header type {}", other))),
        };
        headers.push((name, v));
    }
    Ok(Message { headers, payload: data[12 + hlen..total - 4].to_vec() })
}

/// Encode a sequence of messages.
pub fn encode_all(messages: &[Message]) -> Vec<u8> {
    messages.iter().flat_map(|m| m.encode()).collect()
}

/// Decode a complete buffer of messages.
pub fn decode_all(data: &[u8]) -> Result<Vec<Message>> {
    let mut out = Vec::new();
    let mut d = Decoder::new(|m| {
        out.push(m);
        Ok(())
    });
    d.push(data)?;
    d.finish()?;
    drop(d);
    Ok(out)
}

/// Incremental decoder calling `on_message` for each complete message.
pub struct Decoder<F: FnMut(Message) -> Result<()>> {
    buf: Vec<u8>,
    on_message: F,
    /// First callback / decoding error (kept so `write` can fail with an I/O error).
    pub error: Option<CamelError>,
    /// Messages decoded so far.
    pub count: usize,
}

impl<F: FnMut(Message) -> Result<()>> Decoder<F> {
    pub fn new(on_message: F) -> Self {
        Decoder { buf: Vec::new(), on_message, error: None, count: 0 }
    }

    /// Feed bytes; complete messages are delivered immediately.
    pub fn push(&mut self, data: &[u8]) -> Result<()> {
        self.buf.extend_from_slice(data);
        loop {
            if self.buf.len() < 4 {
                return Ok(());
            }
            let total = u32::from_be_bytes(self.buf[0..4].try_into().unwrap()) as usize;
            if total < 16 {
                return Err(bad("bad total length"));
            }
            if self.buf.len() < total {
                return Ok(());
            }
            let rest = self.buf.split_off(total);
            let msg = decode_message(&self.buf)?;
            self.buf = rest;
            self.count += 1;
            (self.on_message)(msg)?;
        }
    }

    /// Fails when a partial message is left over.
    pub fn finish(&self) -> Result<()> {
        if let Some(e) = &self.error {
            return Err(e.clone());
        }
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(bad(format!("{} trailing bytes", self.buf.len())))
        }
    }
}

impl<F: FnMut(Message) -> Result<()>> io::Write for Decoder<F> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.error.is_some() {
            return Err(io::Error::other("event stream aborted"));
        }
        if let Err(e) = self.push(data) {
            let msg = e.to_string();
            self.error = Some(e);
            return Err(io::Error::other(msg));
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_crc() {
        let m = Message::event("chunk", &Value::object([("bytes", Value::from("aGk="))]));
        let mut all = m.encode();
        let mut m2 = m.clone();
        m2.headers.push(("n".into(), HeaderValue::Long(-5)));
        m2.headers.push(("b".into(), HeaderValue::Bool(false)));
        m2.headers.push(("u".into(), HeaderValue::Uuid([7; 16])));
        all.extend(m2.encode());
        // byte-by-byte feeding
        let mut got = Vec::new();
        let mut d = Decoder::new(|x| {
            got.push(x);
            Ok(())
        });
        for b in &all {
            d.push(std::slice::from_ref(b)).unwrap();
        }
        d.finish().unwrap();
        drop(d);
        assert_eq!(got, vec![m.clone(), m2]);
        assert_eq!(got[0].event_type(), "chunk");
        // corrupted payload is detected
        let mut bad = m.encode();
        let n = bad.len();
        bad[n - 6] ^= 1;
        assert!(decode_message(&bad).unwrap_err().to_string().contains("CRC"));
    }

    #[test]
    fn known_vector() {
        // the empty message of the AWS test suite: total 16, headers 0
        let enc = Message::default().encode();
        assert_eq!(enc, vec![0, 0, 0, 0x10, 0, 0, 0, 0, 0x05, 0xc2, 0x48, 0xeb, 0x7d, 0x98, 0xc8, 0xff]);
    }
}
