//! MongoDB wire protocol framing: `OP_MSG` (MongoDB 3.6+) plus the legacy
//! `OP_QUERY`/`OP_REPLY` pair used by old handshakes.

use std::io::{self, Read};

use cameleer_core::error::{CamelError, Result};

use super::bson::{decode_document, encode_document, Document, Bson};

pub const OP_REPLY: i32 = 1;
pub const OP_QUERY: i32 = 2004;
pub const OP_COMPRESSED: i32 = 2012;
pub const OP_MSG: i32 = 2013;

/// `OP_MSG` flag bits.
pub const FLAG_CHECKSUM_PRESENT: u32 = 1;
pub const FLAG_MORE_TO_COME: u32 = 1 << 1;
pub const FLAG_EXHAUST_ALLOWED: u32 = 1 << 16;

/// Maximum accepted message size (48 MB, the server default).
pub const MAX_MESSAGE_SIZE: usize = 48_000_000;

/// The standard message header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MsgHeader {
    pub length: i32,
    pub request_id: i32,
    pub response_to: i32,
    pub op_code: i32,
}

/// An `OP_MSG`: flags, the body (section kind 0) and document sequences (kind 1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpMsg {
    pub flags: u32,
    pub body: Document,
    pub sequences: Vec<(String, Vec<Document>)>,
}

impl OpMsg {
    pub fn new(body: Document) -> OpMsg {
        OpMsg { flags: 0, body, sequences: Vec::new() }
    }

    /// The body with each document sequence merged in as an array field.
    pub fn merged(&self) -> Document {
        let mut d = self.body.clone();
        for (id, docs) in &self.sequences {
            d.insert(id.clone(), Bson::Array(docs.iter().cloned().map(Bson::Document).collect()));
        }
        d
    }
}

/// Encodes an `OP_MSG` message (header included).
pub fn encode_op_msg(request_id: i32, response_to: i32, msg: &OpMsg) -> Vec<u8> {
    let mut out = vec![0u8; 16];
    out.extend_from_slice(&(msg.flags & !FLAG_CHECKSUM_PRESENT).to_le_bytes());
    out.push(0);
    encode_document(&msg.body, &mut out);
    for (id, docs) in &msg.sequences {
        out.push(1);
        let start = out.len();
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend(id.bytes());
        out.push(0);
        for d in docs {
            encode_document(d, &mut out);
        }
        let len = (out.len() - start) as i32;
        out[start..start + 4].copy_from_slice(&len.to_le_bytes());
    }
    finish(out, request_id, response_to, OP_MSG)
}

fn finish(mut out: Vec<u8>, request_id: i32, response_to: i32, op: i32) -> Vec<u8> {
    let len = out.len() as i32;
    out[0..4].copy_from_slice(&len.to_le_bytes());
    out[4..8].copy_from_slice(&request_id.to_le_bytes());
    out[8..12].copy_from_slice(&response_to.to_le_bytes());
    out[12..16].copy_from_slice(&op.to_le_bytes());
    out
}

fn wire_err(m: impl Into<String>) -> CamelError {
    CamelError::new(crate::kinds::MONGO, format!("Invalid wire message: {}", m.into()))
}

/// Reads one message: the header and the bytes after it.
pub fn read_message<R: Read>(r: &mut R) -> io::Result<(MsgHeader, Vec<u8>)> {
    let mut h = [0u8; 16];
    r.read_exact(&mut h)?;
    let i = |o: usize| i32::from_le_bytes(h[o..o + 4].try_into().unwrap());
    let header = MsgHeader { length: i(0), request_id: i(4), response_to: i(8), op_code: i(12) };
    if header.length < 16 || header.length as usize > MAX_MESSAGE_SIZE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid message length {}", header.length)));
    }
    let mut body = vec![0u8; header.length as usize - 16];
    r.read_exact(&mut body)?;
    Ok((header, body))
}

/// Decodes the part of an `OP_MSG` after the header.
pub fn decode_op_msg(b: &[u8]) -> Result<OpMsg> {
    if b.len() < 5 {
        return Err(wire_err("OP_MSG too short"));
    }
    let flags = u32::from_le_bytes(b[..4].try_into().unwrap());
    let end = if flags & FLAG_CHECKSUM_PRESENT != 0 { b.len().checked_sub(4).ok_or_else(|| wire_err("missing checksum"))? } else { b.len() };
    let mut pos = 4;
    let mut body = None;
    let mut sequences = Vec::new();
    while pos < end {
        let kind = b[pos];
        pos += 1;
        match kind {
            0 => {
                let (d, n) = decode_document(&b[pos..end])?;
                pos += n;
                body = Some(d);
            }
            1 => {
                if pos + 4 > end {
                    return Err(wire_err("truncated document sequence"));
                }
                let size = i32::from_le_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
                let seq_end = pos + size;
                if size < 5 || seq_end > end {
                    return Err(wire_err("invalid document sequence size"));
                }
                let mut p = pos + 4;
                let nul = b[p..seq_end].iter().position(|c| *c == 0).ok_or_else(|| wire_err("unterminated sequence identifier"))?;
                let id = String::from_utf8_lossy(&b[p..p + nul]).into_owned();
                p += nul + 1;
                let mut docs = Vec::new();
                while p < seq_end {
                    let (d, n) = decode_document(&b[p..seq_end])?;
                    p += n;
                    docs.push(d);
                }
                sequences.push((id, docs));
                pos = seq_end;
            }
            other => return Err(wire_err(format!("unknown section kind {}", other))),
        }
    }
    Ok(OpMsg { flags, body: body.ok_or_else(|| wire_err("OP_MSG without body section"))?, sequences })
}

/// A legacy `OP_QUERY` (only what handshakes use).
#[derive(Clone, Debug, PartialEq)]
pub struct OpQuery {
    pub flags: i32,
    pub collection: String,
    pub skip: i32,
    pub limit: i32,
    pub query: Document,
}

/// Decodes the part of an `OP_QUERY` after the header.
pub fn decode_op_query(b: &[u8]) -> Result<OpQuery> {
    if b.len() < 4 {
        return Err(wire_err("OP_QUERY too short"));
    }
    let flags = i32::from_le_bytes(b[..4].try_into().unwrap());
    let nul = b[4..].iter().position(|c| *c == 0).ok_or_else(|| wire_err("unterminated collection name"))?;
    let collection = String::from_utf8_lossy(&b[4..4 + nul]).into_owned();
    let mut p = 4 + nul + 1;
    if p + 8 > b.len() {
        return Err(wire_err("OP_QUERY too short"));
    }
    let skip = i32::from_le_bytes(b[p..p + 4].try_into().unwrap());
    let limit = i32::from_le_bytes(b[p + 4..p + 8].try_into().unwrap());
    p += 8;
    let (query, _) = decode_document(&b[p..])?;
    Ok(OpQuery { flags, collection, skip, limit, query })
}

/// Encodes an `OP_REPLY` with the given documents.
pub fn encode_op_reply(request_id: i32, response_to: i32, docs: &[Document]) -> Vec<u8> {
    let mut out = vec![0u8; 16];
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&0i64.to_le_bytes());
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&(docs.len() as i32).to_le_bytes());
    for d in docs {
        encode_document(d, &mut out);
    }
    finish(out, request_id, response_to, OP_REPLY)
}

/// Encodes an `OP_QUERY` (used by tests of the legacy path).
pub fn encode_op_query(request_id: i32, collection: &str, query: &Document) -> Vec<u8> {
    let mut out = vec![0u8; 16];
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend(collection.bytes());
    out.push(0);
    out.extend_from_slice(&0i32.to_le_bytes());
    out.extend_from_slice(&(-1i32).to_le_bytes());
    encode_document(query, &mut out);
    finish(out, request_id, 0, OP_QUERY)
}

/// Decodes the documents of an `OP_REPLY`.
pub fn decode_op_reply(b: &[u8]) -> Result<Vec<Document>> {
    if b.len() < 20 {
        return Err(wire_err("OP_REPLY too short"));
    }
    let n = i32::from_le_bytes(b[16..20].try_into().unwrap());
    let mut p = 20;
    let mut docs = Vec::new();
    for _ in 0..n.max(0) {
        let (d, used) = decode_document(&b[p..])?;
        p += used;
        docs.push(d);
    }
    Ok(docs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_msg_known_bytes() {
        let msg = OpMsg::new(Document::new().with("ping", 1));
        let b = encode_op_msg(7, 0, &msg);
        let expected: Vec<u8> = [
            &[0x24, 0, 0, 0][..], // length 36
            &[7, 0, 0, 0],
            &[0, 0, 0, 0],
            &[0xdd, 0x07, 0, 0], // 2013
            &[0, 0, 0, 0],       // flags
            &[0],                // kind 0
            &[0x0f, 0, 0, 0, 0x10, b'p', b'i', b'n', b'g', 0, 1, 0, 0, 0, 0],
        ]
        .concat();
        assert_eq!(b, expected);
        let (h, rest) = read_message(&mut &b[..]).unwrap();
        assert_eq!(h, MsgHeader { length: b.len() as i32, request_id: 7, response_to: 0, op_code: OP_MSG });
        assert_eq!(decode_op_msg(&rest).unwrap(), msg);
    }

    #[test]
    fn document_sequences_and_checksum() {
        let mut msg = OpMsg::new(Document::new().with("insert", "c").with("$db", "d"));
        msg.sequences.push(("documents".into(), vec![Document::new().with("a", 1), Document::new().with("a", 2)]));
        let b = encode_op_msg(1, 0, &msg);
        let (_, rest) = read_message(&mut &b[..]).unwrap();
        let back = decode_op_msg(&rest).unwrap();
        assert_eq!(back, msg);
        assert_eq!(back.merged().get_array("documents").unwrap().len(), 2);
        // a checksum (4 trailing bytes) is tolerated
        let mut with_sum = rest.clone();
        with_sum[0] |= 1;
        with_sum.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(decode_op_msg(&with_sum).unwrap().body, msg.body);
        assert!(decode_op_msg(&[0, 0, 0, 0, 9]).is_err());
    }

    #[test]
    fn legacy_query_reply() {
        let q = encode_op_query(3, "admin.$cmd", &Document::new().with("isMaster", 1));
        let (h, rest) = read_message(&mut &q[..]).unwrap();
        assert_eq!(h.op_code, OP_QUERY);
        let parsed = decode_op_query(&rest).unwrap();
        assert_eq!(parsed.collection, "admin.$cmd");
        let r = encode_op_reply(4, 3, &[Document::new().with("ok", 1.0)]);
        let (h, rest) = read_message(&mut &r[..]).unwrap();
        assert_eq!((h.op_code, h.response_to), (OP_REPLY, 3));
        assert_eq!(decode_op_reply(&rest).unwrap()[0].get_f64("ok"), Some(1.0));
    }
}
