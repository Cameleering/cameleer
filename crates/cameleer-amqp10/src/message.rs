//! The AMQP 1.0 message format (part 3.2): header, delivery-annotations,
//! message-annotations, properties, application-properties, body (data /
//! amqp-sequence / amqp-value) and footer sections.

use crate::codec::*;

/// Section descriptor codes.
pub mod section {
    pub const HEADER: u64 = 0x70;
    pub const DELIVERY_ANNOTATIONS: u64 = 0x71;
    pub const MESSAGE_ANNOTATIONS: u64 = 0x72;
    pub const PROPERTIES: u64 = 0x73;
    pub const APPLICATION_PROPERTIES: u64 = 0x74;
    pub const DATA: u64 = 0x75;
    pub const AMQP_SEQUENCE: u64 = 0x76;
    pub const AMQP_VALUE: u64 = 0x77;
    pub const FOOTER: u64 = 0x78;
}

/// The header section.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub durable: bool,
    pub priority: u8,
    /// Milliseconds.
    pub ttl: Option<u32>,
    pub first_acquirer: bool,
    pub delivery_count: u32,
    /// Decoded `first-acquirer` was explicitly `false`: the message was acquired before
    /// (RabbitMQ marks redeliveries this way and keeps `delivery-count` at 0 on classic
    /// queues). Not encoded.
    pub acquired_before: bool,
}

impl Default for Header {
    fn default() -> Self {
        Header { durable: false, priority: 4, ttl: None, first_acquirer: false, delivery_count: 0, acquired_before: false }
    }
}

/// The properties section.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Properties {
    /// message-id: ulong, uuid, binary or string.
    pub message_id: Option<AmqpValue>,
    pub user_id: Option<Vec<u8>>,
    pub to: Option<String>,
    pub subject: Option<String>,
    pub reply_to: Option<String>,
    pub correlation_id: Option<AmqpValue>,
    pub content_type: Option<String>,
    pub content_encoding: Option<String>,
    pub absolute_expiry_time: Option<i64>,
    pub creation_time: Option<i64>,
    pub group_id: Option<String>,
    pub group_sequence: Option<u32>,
    pub reply_to_group_id: Option<String>,
}

/// The body of a message.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Body {
    #[default]
    Empty,
    /// One or more data sections.
    Data(Vec<Vec<u8>>),
    /// One or more amqp-sequence sections.
    Sequence(Vec<Vec<AmqpValue>>),
    /// An amqp-value section.
    Value(AmqpValue),
}

/// An annotations map (keys are symbols or ulongs).
pub type Annotations = Vec<(AmqpValue, AmqpValue)>;

/// An AMQP 1.0 message.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Message {
    pub header: Option<Header>,
    pub delivery_annotations: Annotations,
    pub message_annotations: Annotations,
    pub properties: Option<Properties>,
    /// application-properties (string keys, simple values).
    pub application_properties: Vec<(String, AmqpValue)>,
    pub body: Body,
    pub footer: Annotations,
}

/// Look up an annotation by symbol key.
pub fn annotation<'a>(a: &'a Annotations, key: &str) -> Option<&'a AmqpValue> {
    a.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v)
}

/// Set (replace) an annotation with a symbol key.
pub fn set_annotation(a: &mut Annotations, key: &str, value: AmqpValue) {
    a.retain(|(k, _)| k.as_str() != Some(key));
    a.push((AmqpValue::sym(key), value));
}

impl Message {
    /// A message with an amqp-value string body.
    pub fn text(s: impl Into<String>) -> Message {
        Message { body: Body::Value(AmqpValue::String(s.into())), ..Default::default() }
    }
    /// A message with a single data section.
    pub fn data(b: impl Into<Vec<u8>>) -> Message {
        Message { body: Body::Data(vec![b.into()]), ..Default::default() }
    }
    /// The properties section, created on demand.
    pub fn props_mut(&mut self) -> &mut Properties {
        self.properties.get_or_insert_with(Properties::default)
    }
    /// The header section, created on demand.
    pub fn header_mut(&mut self) -> &mut Header {
        self.header.get_or_insert_with(Header::default)
    }
    pub fn props(&self) -> Properties {
        self.properties.clone().unwrap_or_default()
    }
    /// An application property.
    pub fn app_property(&self, key: &str) -> Option<&AmqpValue> {
        self.application_properties.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
    /// Set (replace) an application property.
    pub fn set_app_property(&mut self, key: &str, v: AmqpValue) {
        self.application_properties.retain(|(k, _)| k != key);
        self.application_properties.push((key.to_string(), v));
    }
    /// A message annotation.
    pub fn annotation(&self, key: &str) -> Option<&AmqpValue> {
        annotation(&self.message_annotations, key)
    }
    pub fn set_annotation(&mut self, key: &str, v: AmqpValue) {
        set_annotation(&mut self.message_annotations, key, v)
    }
    /// The body as bytes (data sections concatenated, string/binary values).
    pub fn body_bytes(&self) -> Option<Vec<u8>> {
        match &self.body {
            Body::Data(d) => Some(d.concat()),
            Body::Value(AmqpValue::Binary(b)) => Some(b.clone()),
            Body::Value(AmqpValue::String(s)) => Some(s.as_bytes().to_vec()),
            _ => None,
        }
    }

    /// Encode the message (the payload of a transfer).
    pub fn encode(&self) -> Vec<u8> {
        use AmqpValue as V;
        let mut out = Vec::new();
        if let Some(h) = &self.header {
            V::composite(
                section::HEADER,
                vec![
                    if h.durable { V::Boolean(true) } else { V::Null },
                    if h.priority == 4 { V::Null } else { V::Ubyte(h.priority) },
                    opt_uint(h.ttl),
                    if h.first_acquirer { V::Boolean(true) } else { V::Null },
                    // explicit, also when 0: SDKs expose the field as given (null -> no count)
                    V::Uint(h.delivery_count),
                ],
            )
            .encode_to(&mut out);
        }
        if !self.delivery_annotations.is_empty() {
            V::described(section::DELIVERY_ANNOTATIONS, V::Map(self.delivery_annotations.clone())).encode_to(&mut out);
        }
        if !self.message_annotations.is_empty() {
            V::described(section::MESSAGE_ANNOTATIONS, V::Map(self.message_annotations.clone())).encode_to(&mut out);
        }
        if let Some(p) = &self.properties {
            V::composite(
                section::PROPERTIES,
                vec![
                    p.message_id.clone().unwrap_or(V::Null),
                    p.user_id.clone().map(V::Binary).unwrap_or(V::Null),
                    opt_str(&p.to),
                    opt_str(&p.subject),
                    opt_str(&p.reply_to),
                    p.correlation_id.clone().unwrap_or(V::Null),
                    opt_sym(&p.content_type),
                    opt_sym(&p.content_encoding),
                    p.absolute_expiry_time.map(V::Timestamp).unwrap_or(V::Null),
                    p.creation_time.map(V::Timestamp).unwrap_or(V::Null),
                    opt_str(&p.group_id),
                    opt_uint(p.group_sequence),
                    opt_str(&p.reply_to_group_id),
                ],
            )
            .encode_to(&mut out);
        }
        if !self.application_properties.is_empty() {
            let m = self.application_properties.iter().map(|(k, v)| (V::str(k.clone()), v.clone())).collect();
            V::described(section::APPLICATION_PROPERTIES, V::Map(m)).encode_to(&mut out);
        }
        match &self.body {
            Body::Empty => {}
            Body::Data(sections) => {
                for d in sections {
                    V::described(section::DATA, V::Binary(d.clone())).encode_to(&mut out);
                }
            }
            Body::Sequence(seqs) => {
                for s in seqs {
                    V::described(section::AMQP_SEQUENCE, V::List(s.clone())).encode_to(&mut out);
                }
            }
            Body::Value(v) => V::described(section::AMQP_VALUE, v.clone()).encode_to(&mut out),
        }
        if !self.footer.is_empty() {
            V::described(section::FOOTER, V::Map(self.footer.clone())).encode_to(&mut out);
        }
        out
    }

    /// Decode a message from a (complete) transfer payload.
    pub fn decode(bytes: &[u8]) -> DResult<Message> {
        let mut m = Message::default();
        let mut d = Decoder::new(bytes);
        while !d.is_empty() {
            let v = d.read_value()?;
            let code = v.descriptor_code().ok_or_else(|| DecodeError("message section is not a described type".into()))?;
            let inner = v.undescribed();
            let f: &[AmqpValue] = inner.as_list().unwrap_or(&[]);
            match code {
                section::HEADER => {
                    m.header = Some(Header {
                        durable: field(f, 0).as_bool().unwrap_or(false),
                        priority: field(f, 1).as_u64().map(|p| p.min(255) as u8).unwrap_or(4),
                        ttl: field(f, 2).as_u32(),
                        first_acquirer: field(f, 3).as_bool().unwrap_or(false),
                        delivery_count: field(f, 4).as_u32().unwrap_or(0),
                        acquired_before: field(f, 3).as_bool() == Some(false),
                    })
                }
                section::DELIVERY_ANNOTATIONS => m.delivery_annotations = map_of(inner),
                section::MESSAGE_ANNOTATIONS => m.message_annotations = map_of(inner),
                section::PROPERTIES => {
                    let opt = |i: usize| Some(field(f, i).clone()).filter(|v| !v.is_null());
                    let s = |i: usize| field(f, i).as_str().map(String::from);
                    m.properties = Some(Properties {
                        message_id: opt(0),
                        user_id: field(f, 1).as_binary().map(|b| b.to_vec()),
                        to: s(2),
                        subject: s(3),
                        reply_to: s(4),
                        correlation_id: opt(5),
                        content_type: s(6),
                        content_encoding: s(7),
                        absolute_expiry_time: field(f, 8).as_i64(),
                        creation_time: field(f, 9).as_i64(),
                        group_id: s(10),
                        group_sequence: field(f, 11).as_u32(),
                        reply_to_group_id: s(12),
                    })
                }
                section::APPLICATION_PROPERTIES => {
                    m.application_properties = map_of(inner).into_iter().map(|(k, v)| (k.as_str().unwrap_or("").to_string(), v)).collect()
                }
                section::DATA => {
                    let b = inner.as_binary().unwrap_or(&[]).to_vec();
                    match &mut m.body {
                        Body::Data(d) => d.push(b),
                        _ => m.body = Body::Data(vec![b]),
                    }
                }
                section::AMQP_SEQUENCE => {
                    let l = inner.as_list().unwrap_or(&[]).to_vec();
                    match &mut m.body {
                        Body::Sequence(s) => s.push(l),
                        _ => m.body = Body::Sequence(vec![l]),
                    }
                }
                section::AMQP_VALUE => m.body = Body::Value(inner.clone()),
                section::FOOTER => m.footer = map_of(inner),
                other => return Err(DecodeError(format!("unknown message section 0x{:x}", other))),
            }
        }
        Ok(m)
    }
}

/// A message id / correlation id rendered as text (`ID:` prefixes are kept as sent).
pub fn id_to_string(v: &AmqpValue) -> String {
    match v {
        AmqpValue::String(s) | AmqpValue::Symbol(s) => s.clone(),
        AmqpValue::Ulong(n) => n.to_string(),
        AmqpValue::Uuid(u) => format_uuid(u),
        AmqpValue::Binary(b) => b.iter().map(|x| format!("{:02x}", x)).collect(),
        other => format!("{:?}", other),
    }
}

/// Format 16 bytes as a canonical UUID.
pub fn format_uuid(u: &[u8; 16]) -> String {
    let h: Vec<String> = u.iter().map(|b| format!("{:02x}", b)).collect();
    format!("{}-{}-{}-{}-{}", h[0..4].concat(), h[4..6].concat(), h[6..8].concat(), h[8..10].concat(), h[10..16].concat())
}

/// Parse a canonical UUID.
pub fn parse_uuid(s: &str) -> Option<[u8; 16]> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// A random (v4) UUID.
pub fn random_uuid() -> [u8; 16] {
    let mut u = [0u8; 16];
    cameleer_tls::crypto::rng::fill(&mut u);
    u[6] = (u[6] & 0x0f) | 0x40;
    u[8] = (u[8] & 0x3f) | 0x80;
    u
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression (live RabbitMQ): a redelivered message has `first-acquirer = false` and
    /// `delivery-count = 0`; an absent first-acquirer is not a redelivery.
    #[test]
    fn explicit_first_acquirer_false_marks_redelivery() {
        let header = |fa: AmqpValue| AmqpValue::described(section::HEADER, AmqpValue::List(vec![AmqpValue::Boolean(true), AmqpValue::Null, AmqpValue::Null, fa, AmqpValue::Uint(0)])).encode();
        let m = Message::decode(&header(AmqpValue::Boolean(false))).unwrap();
        assert!(m.header.unwrap().acquired_before);
        let m = Message::decode(&header(AmqpValue::Null)).unwrap();
        assert!(!m.header.unwrap().acquired_before);
        let m = Message::decode(&header(AmqpValue::Boolean(true))).unwrap();
        assert!(!m.header.unwrap().acquired_before);
    }

    #[test]
    fn sections_round_trip() {
        let mut m = Message {
            header: Some(Header { durable: true, priority: 7, ttl: Some(1000), first_acquirer: true, delivery_count: 2, acquired_before: false }),
            delivery_annotations: vec![(AmqpValue::sym("x-da"), AmqpValue::Int(1))],
            message_annotations: vec![(AmqpValue::sym("x-opt-jms-msg-type"), AmqpValue::Byte(5))],
            properties: Some(Properties {
                message_id: Some(AmqpValue::str("ID:1")),
                user_id: Some(b"u".to_vec()),
                to: Some("q".into()),
                subject: Some("s".into()),
                reply_to: Some("r".into()),
                correlation_id: Some(AmqpValue::Ulong(9)),
                content_type: Some("text/plain".into()),
                content_encoding: Some("utf-8".into()),
                absolute_expiry_time: Some(5),
                creation_time: Some(4),
                group_id: Some("g".into()),
                group_sequence: Some(3),
                reply_to_group_id: Some("rg".into()),
            }),
            application_properties: vec![("a".into(), AmqpValue::Long(1)), ("b".into(), AmqpValue::str("x"))],
            body: Body::Data(vec![b"one".to_vec(), b"two".to_vec()]),
            footer: vec![(AmqpValue::sym("x-f"), AmqpValue::Null)],
        };
        assert_eq!(Message::decode(&m.encode()).unwrap(), m);
        assert_eq!(m.body_bytes().unwrap(), b"onetwo");
        m.body = Body::Sequence(vec![vec![AmqpValue::Int(1)], vec![AmqpValue::str("2")]]);
        assert_eq!(Message::decode(&m.encode()).unwrap(), m);
        m.body = Body::Value(AmqpValue::Map(vec![(AmqpValue::str("k"), AmqpValue::Double(1.5))]));
        assert_eq!(Message::decode(&m.encode()).unwrap(), m);
        let t = Message::text("hi");
        // amqp-value section: 00 53 77 a1 02 'h' 'i'
        assert_eq!(t.encode(), [0x00, 0x53, 0x77, 0xa1, 2, b'h', b'i']);
        assert_eq!(Message::decode(&Message::default().encode()).unwrap(), Message::default());
    }

    #[test]
    fn uuids() {
        let u = random_uuid();
        assert_eq!(parse_uuid(&format_uuid(&u)).unwrap(), u);
        assert_eq!(format_uuid(&[0; 16]), "00000000-0000-0000-0000-000000000000");
    }
}
