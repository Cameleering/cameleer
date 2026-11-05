//! MQTT 3.1.1 and 5.0 control packet codec, plus topic name/filter helpers.

use std::io::{self, Read};

/// Packet types.
pub mod types {
    pub const CONNECT: u8 = 1;
    pub const CONNACK: u8 = 2;
    pub const PUBLISH: u8 = 3;
    pub const PUBACK: u8 = 4;
    pub const PUBREC: u8 = 5;
    pub const PUBREL: u8 = 6;
    pub const PUBCOMP: u8 = 7;
    pub const SUBSCRIBE: u8 = 8;
    pub const SUBACK: u8 = 9;
    pub const UNSUBSCRIBE: u8 = 10;
    pub const UNSUBACK: u8 = 11;
    pub const PINGREQ: u8 = 12;
    pub const PINGRESP: u8 = 13;
    pub const DISCONNECT: u8 = 14;
    pub const AUTH: u8 = 15;
}

/// Well-known MQTT 5 property identifiers.
pub mod props {
    pub const PAYLOAD_FORMAT_INDICATOR: u8 = 0x01;
    pub const MESSAGE_EXPIRY_INTERVAL: u8 = 0x02;
    pub const CONTENT_TYPE: u8 = 0x03;
    pub const RESPONSE_TOPIC: u8 = 0x08;
    pub const CORRELATION_DATA: u8 = 0x09;
    pub const SUBSCRIPTION_IDENTIFIER: u8 = 0x0B;
    pub const SESSION_EXPIRY_INTERVAL: u8 = 0x11;
    pub const ASSIGNED_CLIENT_IDENTIFIER: u8 = 0x12;
    pub const SERVER_KEEP_ALIVE: u8 = 0x13;
    pub const REASON_STRING: u8 = 0x1F;
    pub const RECEIVE_MAXIMUM: u8 = 0x21;
    pub const TOPIC_ALIAS_MAXIMUM: u8 = 0x22;
    pub const TOPIC_ALIAS: u8 = 0x23;
    pub const USER_PROPERTY: u8 = 0x26;
}

/// A property value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PropValue {
    Byte(u8),
    U16(u16),
    U32(u32),
    VarInt(u32),
    Str(String),
    Bin(Vec<u8>),
    Pair(String, String),
}

/// MQTT 5 properties (ordered list of `(identifier, value)`).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Properties(pub Vec<(u8, PropValue)>);

impl Properties {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// First value of a property.
    pub fn get(&self, id: u8) -> Option<&PropValue> {
        self.0.iter().find(|(i, _)| *i == id).map(|(_, v)| v)
    }
    pub fn push(&mut self, id: u8, v: PropValue) {
        self.0.push((id, v));
    }
    pub fn remove(&mut self, id: u8) {
        self.0.retain(|(i, _)| *i != id);
    }
    /// User properties as `(name, value)` pairs.
    pub fn user_properties(&self) -> Vec<(String, String)> {
        self.0
            .iter()
            .filter_map(|(_, v)| match v {
                PropValue::Pair(k, v) => Some((k.clone(), v.clone())),
                _ => None,
            })
            .collect()
    }
}

fn prop_kind(id: u8) -> Option<u8> {
    // 0 byte, 1 u16, 2 u32, 3 varint, 4 string, 5 binary, 6 pair
    Some(match id {
        0x01 | 0x17 | 0x19 | 0x24 | 0x25 | 0x28 | 0x29 | 0x2A => 0,
        0x13 | 0x21 | 0x22 | 0x23 => 1,
        0x02 | 0x11 | 0x18 | 0x27 => 2,
        0x0B => 3,
        0x03 | 0x08 | 0x12 | 0x15 | 0x1A | 0x1C | 0x1F => 4,
        0x09 | 0x16 => 5,
        0x26 => 6,
        _ => return None,
    })
}

/// The will message of a CONNECT.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Will {
    pub topic: String,
    pub payload: Vec<u8>,
    pub qos: u8,
    pub retain: bool,
    pub props: Properties,
}

/// CONNECT.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Connect {
    /// Protocol level: 3 (3.1), 4 (3.1.1) or 5.
    pub level: u8,
    pub client_id: String,
    pub clean_session: bool,
    pub keep_alive: u16,
    pub username: Option<String>,
    pub password: Option<Vec<u8>>,
    pub will: Option<Will>,
    pub props: Properties,
}

/// PUBLISH.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Publish {
    pub dup: bool,
    pub qos: u8,
    pub retain: bool,
    pub topic: String,
    pub pkid: u16,
    pub payload: Vec<u8>,
    pub props: Properties,
}

/// An MQTT control packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Connect(Connect),
    ConnAck { session_present: bool, code: u8, props: Properties },
    Publish(Publish),
    PubAck { pkid: u16, reason: u8, props: Properties },
    PubRec { pkid: u16, reason: u8, props: Properties },
    PubRel { pkid: u16, reason: u8, props: Properties },
    PubComp { pkid: u16, reason: u8, props: Properties },
    /// filters: (filter, options byte: qos in bits 0-1, v5 flags above)
    Subscribe { pkid: u16, filters: Vec<(String, u8)>, props: Properties },
    SubAck { pkid: u16, codes: Vec<u8>, props: Properties },
    Unsubscribe { pkid: u16, filters: Vec<String>, props: Properties },
    UnsubAck { pkid: u16, codes: Vec<u8>, props: Properties },
    PingReq,
    PingResp,
    Disconnect { reason: u8, props: Properties },
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("Malformed MQTT packet: {}", msg))
}

// ---------------------------------------------------------------------------
// encoding
// ---------------------------------------------------------------------------

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_be_bytes());
}
fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u16(b, s.len() as u16);
    b.extend_from_slice(s.as_bytes());
}
fn put_bin(b: &mut Vec<u8>, s: &[u8]) {
    put_u16(b, s.len() as u16);
    b.extend_from_slice(s);
}
/// Encode a variable byte integer.
pub fn put_varint(b: &mut Vec<u8>, mut v: u32) {
    loop {
        let mut byte = (v % 128) as u8;
        v /= 128;
        if v > 0 {
            byte |= 0x80;
        }
        b.push(byte);
        if v == 0 {
            break;
        }
    }
}
fn put_props(b: &mut Vec<u8>, p: &Properties) {
    let mut body = Vec::new();
    for (id, v) in &p.0 {
        body.push(*id);
        match v {
            PropValue::Byte(x) => body.push(*x),
            PropValue::U16(x) => put_u16(&mut body, *x),
            PropValue::U32(x) => body.extend_from_slice(&x.to_be_bytes()),
            PropValue::VarInt(x) => put_varint(&mut body, *x),
            PropValue::Str(s) => put_str(&mut body, s),
            PropValue::Bin(s) => put_bin(&mut body, s),
            PropValue::Pair(k, v) => {
                put_str(&mut body, k);
                put_str(&mut body, v);
            }
        }
    }
    put_varint(b, body.len() as u32);
    b.extend_from_slice(&body);
}

fn ack_body(pkid: u16, reason: u8, props: &Properties, v5: bool) -> Vec<u8> {
    let mut b = Vec::new();
    put_u16(&mut b, pkid);
    if v5 && (reason != 0 || !props.is_empty()) {
        b.push(reason);
        if !props.is_empty() {
            put_props(&mut b, props);
        }
    }
    b
}

impl Packet {
    /// Encode the packet; `v5` selects the MQTT 5 layout (ignored for CONNECT, whose
    /// layout follows its `level`).
    pub fn encode(&self, v5: bool) -> Vec<u8> {
        let (first, body): (u8, Vec<u8>) = match self {
            Packet::Connect(c) => {
                let v5 = c.level >= 5;
                let mut b = Vec::new();
                put_str(&mut b, if c.level == 3 { "MQIsdp" } else { "MQTT" });
                b.push(c.level);
                let mut flags = 0u8;
                if c.username.is_some() {
                    flags |= 0x80;
                }
                if c.password.is_some() {
                    flags |= 0x40;
                }
                if let Some(w) = &c.will {
                    flags |= 0x04 | ((w.qos & 3) << 3);
                    if w.retain {
                        flags |= 0x20;
                    }
                }
                if c.clean_session {
                    flags |= 0x02;
                }
                b.push(flags);
                put_u16(&mut b, c.keep_alive);
                if v5 {
                    put_props(&mut b, &c.props);
                }
                put_str(&mut b, &c.client_id);
                if let Some(w) = &c.will {
                    if v5 {
                        put_props(&mut b, &w.props);
                    }
                    put_str(&mut b, &w.topic);
                    put_bin(&mut b, &w.payload);
                }
                if let Some(u) = &c.username {
                    put_str(&mut b, u);
                }
                if let Some(p) = &c.password {
                    put_bin(&mut b, p);
                }
                (types::CONNECT << 4, b)
            }
            Packet::ConnAck { session_present, code, props } => {
                let mut b = vec![u8::from(*session_present), *code];
                if v5 {
                    put_props(&mut b, props);
                }
                (types::CONNACK << 4, b)
            }
            Packet::Publish(p) => {
                let mut b = Vec::new();
                put_str(&mut b, &p.topic);
                if p.qos > 0 {
                    put_u16(&mut b, p.pkid);
                }
                if v5 {
                    put_props(&mut b, &p.props);
                }
                b.extend_from_slice(&p.payload);
                let mut f = (types::PUBLISH << 4) | ((p.qos & 3) << 1);
                if p.dup {
                    f |= 0x08;
                }
                if p.retain {
                    f |= 0x01;
                }
                (f, b)
            }
            Packet::PubAck { pkid, reason, props } => (types::PUBACK << 4, ack_body(*pkid, *reason, props, v5)),
            Packet::PubRec { pkid, reason, props } => (types::PUBREC << 4, ack_body(*pkid, *reason, props, v5)),
            Packet::PubRel { pkid, reason, props } => ((types::PUBREL << 4) | 0x02, ack_body(*pkid, *reason, props, v5)),
            Packet::PubComp { pkid, reason, props } => (types::PUBCOMP << 4, ack_body(*pkid, *reason, props, v5)),
            Packet::Subscribe { pkid, filters, props } => {
                let mut b = Vec::new();
                put_u16(&mut b, *pkid);
                if v5 {
                    put_props(&mut b, props);
                }
                for (f, o) in filters {
                    put_str(&mut b, f);
                    b.push(if v5 { *o } else { *o & 3 });
                }
                ((types::SUBSCRIBE << 4) | 0x02, b)
            }
            Packet::SubAck { pkid, codes, props } => {
                let mut b = Vec::new();
                put_u16(&mut b, *pkid);
                if v5 {
                    put_props(&mut b, props);
                }
                b.extend_from_slice(codes);
                (types::SUBACK << 4, b)
            }
            Packet::Unsubscribe { pkid, filters, props } => {
                let mut b = Vec::new();
                put_u16(&mut b, *pkid);
                if v5 {
                    put_props(&mut b, props);
                }
                for f in filters {
                    put_str(&mut b, f);
                }
                ((types::UNSUBSCRIBE << 4) | 0x02, b)
            }
            Packet::UnsubAck { pkid, codes, props } => {
                let mut b = Vec::new();
                put_u16(&mut b, *pkid);
                if v5 {
                    put_props(&mut b, props);
                    b.extend_from_slice(codes);
                }
                (types::UNSUBACK << 4, b)
            }
            Packet::PingReq => (types::PINGREQ << 4, Vec::new()),
            Packet::PingResp => (types::PINGRESP << 4, Vec::new()),
            Packet::Disconnect { reason, props } => {
                let mut b = Vec::new();
                if v5 && (*reason != 0 || !props.is_empty()) {
                    b.push(*reason);
                    put_props(&mut b, props);
                }
                (types::DISCONNECT << 4, b)
            }
        };
        let mut out = Vec::with_capacity(body.len() + 5);
        out.push(first);
        put_varint(&mut out, body.len() as u32);
        out.extend_from_slice(&body);
        out
    }
}

// ---------------------------------------------------------------------------
// decoding
// ---------------------------------------------------------------------------

struct Cur<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn left(&self) -> usize {
        self.b.len() - self.pos
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.left() < n {
            return Err(bad("truncated"));
        }
        let s = &self.b[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> io::Result<u16> {
        let s = self.take(2)?;
        Ok(u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> io::Result<u32> {
        let s = self.take(4)?;
        Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn varint(&mut self) -> io::Result<u32> {
        let mut v: u32 = 0;
        let mut mult = 1u32;
        for _ in 0..4 {
            let b = self.u8()?;
            v += (b & 0x7f) as u32 * mult;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            mult *= 128;
        }
        Err(bad("variable byte integer too long"))
    }
    fn bin(&mut self) -> io::Result<Vec<u8>> {
        let n = self.u16()? as usize;
        Ok(self.take(n)?.to_vec())
    }
    fn str(&mut self) -> io::Result<String> {
        String::from_utf8(self.bin()?).map_err(|_| bad("invalid UTF-8 string"))
    }
    fn rest(&mut self) -> Vec<u8> {
        let r = self.b[self.pos..].to_vec();
        self.pos = self.b.len();
        r
    }
    fn props(&mut self) -> io::Result<Properties> {
        let len = self.varint()? as usize;
        let data = self.take(len)?;
        let mut c = Cur { b: data, pos: 0 };
        let mut p = Properties::new();
        while c.left() > 0 {
            let id = c.varint()? as u8;
            let v = match prop_kind(id).ok_or_else(|| bad(&format!("unknown property 0x{:02x}", id)))? {
                0 => PropValue::Byte(c.u8()?),
                1 => PropValue::U16(c.u16()?),
                2 => PropValue::U32(c.u32()?),
                3 => PropValue::VarInt(c.varint()?),
                4 => PropValue::Str(c.str()?),
                5 => PropValue::Bin(c.bin()?),
                _ => PropValue::Pair(c.str()?, c.str()?),
            };
            p.push(id, v);
        }
        Ok(p)
    }
}

fn read_ack(c: &mut Cur, v5: bool) -> io::Result<(u16, u8, Properties)> {
    let pkid = c.u16()?;
    let mut reason = 0;
    let mut props = Properties::new();
    if v5 && c.left() > 0 {
        reason = c.u8()?;
        if c.left() > 0 {
            props = c.props()?;
        }
    }
    Ok((pkid, reason, props))
}

/// Read one packet. `Ok(None)` on a clean EOF before the first byte.
pub fn read_packet<R: Read>(r: &mut R, v5: bool, max_size: usize) -> io::Result<Option<Packet>> {
    let mut first = [0u8; 1];
    loop {
        match r.read(&mut first) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    read_packet_after(first[0], r, v5, max_size).map(Some)
}

/// Read the rest of a packet whose first byte has already been read.
pub fn read_packet_after<R: Read>(first: u8, r: &mut R, v5: bool, max_size: usize) -> io::Result<Packet> {
    let mut len: usize = 0;
    let mut mult = 1usize;
    for i in 0..4 {
        let mut b = [0u8; 1];
        r.read_exact(&mut b)?;
        len += (b[0] & 0x7f) as usize * mult;
        if b[0] & 0x80 == 0 {
            break;
        }
        if i == 3 {
            return Err(bad("remaining length too long"));
        }
        mult *= 128;
    }
    if len > max_size {
        return Err(bad("packet too large"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    decode(first, &body, v5)
}

/// Decode a packet from its first byte and body.
pub fn decode(first: u8, body: &[u8], v5: bool) -> io::Result<Packet> {
    let mut c = Cur { b: body, pos: 0 };
    let flags = first & 0x0f;
    Ok(match first >> 4 {
        types::CONNECT => {
            let name = c.str()?;
            if name != "MQTT" && name != "MQIsdp" {
                return Err(bad("invalid protocol name"));
            }
            let level = c.u8()?;
            let f = c.u8()?;
            let keep_alive = c.u16()?;
            let v5 = level >= 5;
            let props = if v5 { c.props()? } else { Properties::new() };
            let client_id = c.str()?;
            let will = if f & 0x04 != 0 {
                let wprops = if v5 { c.props()? } else { Properties::new() };
                let topic = c.str()?;
                let payload = c.bin()?;
                Some(Will { topic, payload, qos: (f >> 3) & 3, retain: f & 0x20 != 0, props: wprops })
            } else {
                None
            };
            let username = if f & 0x80 != 0 { Some(c.str()?) } else { None };
            let password = if f & 0x40 != 0 { Some(c.bin()?) } else { None };
            Packet::Connect(Connect { level, client_id, clean_session: f & 0x02 != 0, keep_alive, username, password, will, props })
        }
        types::CONNACK => {
            let sp = c.u8()? & 1 == 1;
            let code = c.u8()?;
            let props = if v5 && c.left() > 0 { c.props()? } else { Properties::new() };
            Packet::ConnAck { session_present: sp, code, props }
        }
        types::PUBLISH => {
            let qos = (flags >> 1) & 3;
            if qos == 3 {
                return Err(bad("invalid QoS 3"));
            }
            let topic = c.str()?;
            let pkid = if qos > 0 { c.u16()? } else { 0 };
            let props = if v5 { c.props()? } else { Properties::new() };
            Packet::Publish(Publish { dup: flags & 0x08 != 0, qos, retain: flags & 1 != 0, topic, pkid, payload: c.rest(), props })
        }
        types::PUBACK => {
            let (pkid, reason, props) = read_ack(&mut c, v5)?;
            Packet::PubAck { pkid, reason, props }
        }
        types::PUBREC => {
            let (pkid, reason, props) = read_ack(&mut c, v5)?;
            Packet::PubRec { pkid, reason, props }
        }
        types::PUBREL => {
            let (pkid, reason, props) = read_ack(&mut c, v5)?;
            Packet::PubRel { pkid, reason, props }
        }
        types::PUBCOMP => {
            let (pkid, reason, props) = read_ack(&mut c, v5)?;
            Packet::PubComp { pkid, reason, props }
        }
        types::SUBSCRIBE => {
            let pkid = c.u16()?;
            let props = if v5 { c.props()? } else { Properties::new() };
            let mut filters = Vec::new();
            while c.left() > 0 {
                filters.push((c.str()?, c.u8()?));
            }
            if filters.is_empty() {
                return Err(bad("SUBSCRIBE without topic filters"));
            }
            Packet::Subscribe { pkid, filters, props }
        }
        types::SUBACK => {
            let pkid = c.u16()?;
            let props = if v5 { c.props()? } else { Properties::new() };
            Packet::SubAck { pkid, codes: c.rest(), props }
        }
        types::UNSUBSCRIBE => {
            let pkid = c.u16()?;
            let props = if v5 { c.props()? } else { Properties::new() };
            let mut filters = Vec::new();
            while c.left() > 0 {
                filters.push(c.str()?);
            }
            Packet::Unsubscribe { pkid, filters, props }
        }
        types::UNSUBACK => {
            let pkid = c.u16()?;
            let (props, codes) = if v5 && c.left() > 0 { (c.props()?, c.rest()) } else { (Properties::new(), Vec::new()) };
            Packet::UnsubAck { pkid, codes, props }
        }
        types::PINGREQ => Packet::PingReq,
        types::PINGRESP => Packet::PingResp,
        types::DISCONNECT => {
            let reason = if c.left() > 0 { c.u8()? } else { 0 };
            let props = if v5 && c.left() > 0 { c.props()? } else { Properties::new() };
            Packet::Disconnect { reason, props }
        }
        t => return Err(bad(&format!("unsupported packet type {}", t))),
    })
}

// ---------------------------------------------------------------------------
// topics
// ---------------------------------------------------------------------------

/// Whether a topic name matches a topic filter (`+` one level, `#` the rest including
/// the parent level). Topics starting with `$` are not matched by a leading wildcard.
pub fn topic_matches(filter: &str, topic: &str) -> bool {
    if topic.starts_with('$') && (filter.starts_with('+') || filter.starts_with('#')) {
        return false;
    }
    let mut f = filter.split('/');
    let mut t = topic.split('/');
    loop {
        match (f.next(), t.next()) {
            (Some("#"), _) => return true,
            (Some("+"), Some(_)) => {}
            (Some(a), Some(b)) if a == b => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// Validate a topic filter (wildcards must occupy whole levels, `#` must be last).
pub fn valid_filter(filter: &str) -> bool {
    if filter.is_empty() {
        return false;
    }
    let levels: Vec<&str> = filter.split('/').collect();
    for (i, l) in levels.iter().enumerate() {
        if l.contains('#') && (*l != "#" || i != levels.len() - 1) {
            return false;
        }
        if l.contains('+') && *l != "+" {
            return false;
        }
    }
    true
}

/// Validate a topic name for PUBLISH (no wildcards, not empty).
pub fn valid_topic(topic: &str) -> bool {
    !topic.is_empty() && !topic.contains('+') && !topic.contains('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt(p: Packet, v5: bool) {
        let bytes = p.encode(v5);
        let back = read_packet(&mut &bytes[..], v5, 1 << 20).unwrap().unwrap();
        assert_eq!(back, p);
    }

    #[test]
    fn packets_round_trip() {
        for v5 in [false, true] {
            let mut props = Properties::new();
            if v5 {
                props.push(props::USER_PROPERTY, PropValue::Pair("a".into(), "b".into()));
                props.push(props::SESSION_EXPIRY_INTERVAL, PropValue::U32(60));
            }
            rt(
                Packet::Connect(Connect {
                    level: if v5 { 5 } else { 4 },
                    client_id: "c1".into(),
                    clean_session: true,
                    keep_alive: 30,
                    username: Some("u".into()),
                    password: Some(b"p".to_vec()),
                    will: Some(Will { topic: "w".into(), payload: b"bye".to_vec(), qos: 1, retain: true, props: Properties::new() }),
                    props: props.clone(),
                }),
                v5,
            );
            rt(Packet::ConnAck { session_present: true, code: 0, props: Properties::new() }, v5);
            rt(Packet::Publish(Publish { dup: false, qos: 2, retain: true, topic: "a/b".into(), pkid: 7, payload: b"x".to_vec(), props: props.clone() }), v5);
            rt(Packet::Publish(Publish { qos: 0, topic: "a".into(), payload: vec![], props: props.clone(), ..Default::default() }), v5);
            rt(Packet::PubRel { pkid: 9, reason: 0, props: Properties::new() }, v5);
            rt(Packet::Subscribe { pkid: 3, filters: vec![("a/+".into(), 1), ("#".into(), 2)], props: Properties::new() }, v5);
            rt(Packet::SubAck { pkid: 3, codes: vec![1, 0x80], props: Properties::new() }, v5);
            rt(Packet::Unsubscribe { pkid: 4, filters: vec!["a/+".into()], props: Properties::new() }, v5);
            rt(Packet::PingReq, v5);
            rt(Packet::Disconnect { reason: 0, props: Properties::new() }, v5);
        }
        rt(Packet::UnsubAck { pkid: 4, codes: vec![0], props: Properties::new() }, true);
        rt(Packet::UnsubAck { pkid: 4, codes: vec![], props: Properties::new() }, false);
    }

    #[test]
    fn varint_lengths() {
        let mut b = Vec::new();
        put_varint(&mut b, 321);
        assert_eq!(b, vec![0xC1, 0x02]);
        let big = Packet::Publish(Publish { topic: "t".into(), payload: vec![7u8; 20000], ..Default::default() });
        rt(big, false);
    }

    #[test]
    fn topic_matching() {
        assert!(topic_matches("sport/tennis/#", "sport/tennis/player1/ranking"));
        assert!(topic_matches("sport/#", "sport"));
        assert!(topic_matches("#", "a/b"));
        assert!(topic_matches("sport/+/player1", "sport/tennis/player1"));
        assert!(!topic_matches("sport/+", "sport/tennis/player1"));
        assert!(topic_matches("+/+", "/finance"));
        assert!(!topic_matches("#", "$SYS/x"));
        assert!(topic_matches("a/b", "a/b"));
        assert!(!topic_matches("a/b", "a/b/c"));
        assert!(valid_filter("a/+/#"));
        assert!(!valid_filter("a/#/b"));
        assert!(!valid_filter("a+"));
        assert!(!valid_topic("a/+"));
    }
}
