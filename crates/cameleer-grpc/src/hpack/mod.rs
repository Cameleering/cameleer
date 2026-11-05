//! HPACK header compression (RFC 7541): static and dynamic tables, prefix-integer and
//! string literal coding, Huffman coding, and a stateful [`Encoder`] / [`Decoder`] pair.
//!
//! ```
//! use cameleer_grpc::hpack::{Decoder, Encoder};
//! let mut enc = Encoder::new();
//! let mut dec = Decoder::new(4096);
//! let headers = vec![(":method".to_string(), "GET".to_string()), ("x-id".to_string(), "42".to_string())];
//! let block = enc.encode(&headers);
//! assert_eq!(dec.decode(&block).unwrap(), headers);
//! ```

pub mod huffman;
mod huffman_table;

use std::collections::VecDeque;
use std::fmt;

/// A decoded header field list (lower-case names, in order).
pub type HeaderList = Vec<(String, String)>;

/// HPACK decoding errors (all map to the HTTP/2 `COMPRESSION_ERROR`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HpackError {
    /// Truncated input.
    Truncated,
    /// Integer does not fit.
    IntegerOverflow,
    /// Index not in the static or dynamic table.
    InvalidIndex(usize),
    /// Huffman decoding failure.
    Huffman(&'static str),
    /// Dynamic table size update larger than allowed or not at the start of a block.
    InvalidTableSizeUpdate,
    /// The decoded header list is larger than allowed.
    HeaderListTooLarge,
}

impl fmt::Display for HpackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HpackError::Truncated => write!(f, "truncated header block"),
            HpackError::IntegerOverflow => write!(f, "integer overflow"),
            HpackError::InvalidIndex(i) => write!(f, "invalid table index {}", i),
            HpackError::Huffman(m) => write!(f, "huffman: {}", m),
            HpackError::InvalidTableSizeUpdate => write!(f, "invalid dynamic table size update"),
            HpackError::HeaderListTooLarge => write!(f, "header list too large"),
        }
    }
}

impl std::error::Error for HpackError {}

/// The static table (RFC 7541 Appendix A), 1-based.
pub const STATIC_TABLE: [(&str, &str); 61] = [
    (":authority", ""),
    (":method", "GET"),
    (":method", "POST"),
    (":path", "/"),
    (":path", "/index.html"),
    (":scheme", "http"),
    (":scheme", "https"),
    (":status", "200"),
    (":status", "204"),
    (":status", "206"),
    (":status", "304"),
    (":status", "400"),
    (":status", "404"),
    (":status", "500"),
    ("accept-charset", ""),
    ("accept-encoding", "gzip, deflate"),
    ("accept-language", ""),
    ("accept-ranges", ""),
    ("accept", ""),
    ("access-control-allow-origin", ""),
    ("age", ""),
    ("allow", ""),
    ("authorization", ""),
    ("cache-control", ""),
    ("content-disposition", ""),
    ("content-encoding", ""),
    ("content-language", ""),
    ("content-length", ""),
    ("content-location", ""),
    ("content-range", ""),
    ("content-type", ""),
    ("cookie", ""),
    ("date", ""),
    ("etag", ""),
    ("expect", ""),
    ("expires", ""),
    ("from", ""),
    ("host", ""),
    ("if-match", ""),
    ("if-modified-since", ""),
    ("if-none-match", ""),
    ("if-range", ""),
    ("if-unmodified-since", ""),
    ("last-modified", ""),
    ("link", ""),
    ("location", ""),
    ("max-forwards", ""),
    ("proxy-authenticate", ""),
    ("proxy-authorization", ""),
    ("range", ""),
    ("referer", ""),
    ("refresh", ""),
    ("retry-after", ""),
    ("server", ""),
    ("set-cookie", ""),
    ("strict-transport-security", ""),
    ("transfer-encoding", ""),
    ("user-agent", ""),
    ("vary", ""),
    ("via", ""),
    ("www-authenticate", ""),
];

// ---------------------------------------------------------------------------
// primitives
// ---------------------------------------------------------------------------

/// Encodes `value` with an N-bit prefix (RFC 7541 5.1); `first` holds the high bits of the first byte.
pub fn encode_integer(value: usize, prefix_bits: u8, first: u8, out: &mut Vec<u8>) {
    let max = (1usize << prefix_bits) - 1;
    if value < max {
        out.push(first | value as u8);
        return;
    }
    out.push(first | max as u8);
    let mut v = value - max;
    while v >= 128 {
        out.push((v % 128) as u8 | 0x80);
        v /= 128;
    }
    out.push(v as u8);
}

/// Decodes an N-bit prefix integer starting at `buf[*pos]`.
pub fn decode_integer(buf: &[u8], pos: &mut usize, prefix_bits: u8) -> Result<usize, HpackError> {
    let max = (1usize << prefix_bits) - 1;
    let first = *buf.get(*pos).ok_or(HpackError::Truncated)? as usize & max;
    *pos += 1;
    if first < max {
        return Ok(first);
    }
    let mut value = max;
    let mut shift = 0u32;
    loop {
        let b = *buf.get(*pos).ok_or(HpackError::Truncated)? as usize;
        *pos += 1;
        if shift > 28 {
            return Err(HpackError::IntegerOverflow);
        }
        value = value.checked_add((b & 0x7f) << shift).ok_or(HpackError::IntegerOverflow)?;
        shift += 7;
        if b & 0x80 == 0 {
            return Ok(value);
        }
    }
}

/// Encodes a string literal, Huffman-coded when that is shorter.
pub fn encode_string(s: &[u8], huffman_allowed: bool, out: &mut Vec<u8>) {
    let hlen = huffman::encoded_len(s);
    if huffman_allowed && hlen < s.len() {
        encode_integer(hlen, 7, 0x80, out);
        huffman::encode(s, out);
    } else {
        encode_integer(s.len(), 7, 0, out);
        out.extend_from_slice(s);
    }
}

/// Decodes a string literal.
pub fn decode_string(buf: &[u8], pos: &mut usize) -> Result<Vec<u8>, HpackError> {
    let huff = *buf.get(*pos).ok_or(HpackError::Truncated)? & 0x80 != 0;
    let len = decode_integer(buf, pos, 7)?;
    let end = pos.checked_add(len).ok_or(HpackError::Truncated)?;
    if end > buf.len() {
        return Err(HpackError::Truncated);
    }
    let raw = &buf[*pos..end];
    *pos = end;
    if huff {
        huffman::decode(raw)
    } else {
        Ok(raw.to_vec())
    }
}

fn entry_size(name: &str, value: &str) -> usize {
    name.len() + value.len() + 32
}

/// The dynamic table (RFC 7541 2.3.2), newest entry first.
#[derive(Debug, Clone)]
pub struct DynamicTable {
    entries: VecDeque<(String, String)>,
    size: usize,
    max_size: usize,
}

impl DynamicTable {
    pub fn new(max_size: usize) -> Self {
        DynamicTable { entries: VecDeque::new(), size: 0, max_size }
    }
    /// Current size in octets (entries + 32 bytes overhead each).
    pub fn size(&self) -> usize {
        self.size
    }
    pub fn max_size(&self) -> usize {
        self.max_size
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    /// Entry by 0-based dynamic index (0 = newest).
    pub fn get(&self, i: usize) -> Option<&(String, String)> {
        self.entries.get(i)
    }
    pub fn set_max_size(&mut self, max: usize) {
        self.max_size = max;
        self.evict(0);
    }
    fn evict(&mut self, incoming: usize) {
        while self.size + incoming > self.max_size {
            match self.entries.pop_back() {
                Some((n, v)) => self.size -= entry_size(&n, &v),
                None => break,
            }
        }
    }
    pub fn insert(&mut self, name: String, value: String) {
        let s = entry_size(&name, &value);
        if s > self.max_size {
            self.entries.clear();
            self.size = 0;
            return;
        }
        self.evict(s);
        self.size += s;
        self.entries.push_front((name, value));
    }
}

fn lookup(table: &DynamicTable, index: usize) -> Result<(String, String), HpackError> {
    if index == 0 {
        return Err(HpackError::InvalidIndex(0));
    }
    if index <= STATIC_TABLE.len() {
        let (n, v) = STATIC_TABLE[index - 1];
        return Ok((n.to_string(), v.to_string()));
    }
    table.get(index - STATIC_TABLE.len() - 1).cloned().ok_or(HpackError::InvalidIndex(index))
}

// ---------------------------------------------------------------------------
// decoder
// ---------------------------------------------------------------------------

/// Stateful header block decoder.
#[derive(Debug, Clone)]
pub struct Decoder {
    table: DynamicTable,
    /// Upper bound for dynamic table size updates (our SETTINGS_HEADER_TABLE_SIZE).
    max_allowed: usize,
    /// Maximum decoded header list size (name+value+32 per field); 0 = unlimited.
    pub max_header_list_size: usize,
}

impl Decoder {
    pub fn new(max_table_size: usize) -> Self {
        Decoder { table: DynamicTable::new(max_table_size), max_allowed: max_table_size, max_header_list_size: 0 }
    }
    pub fn table(&self) -> &DynamicTable {
        &self.table
    }
    /// Changes the allowed maximum (after our SETTINGS were acknowledged).
    pub fn set_max_allowed(&mut self, max: usize) {
        self.max_allowed = max;
        if self.table.max_size() > max {
            self.table.set_max_size(max);
        }
    }

    /// Decodes one complete header block.
    pub fn decode(&mut self, block: &[u8]) -> Result<HeaderList, HpackError> {
        let mut out = Vec::new();
        let mut pos = 0;
        let mut list_size = 0usize;
        let mut seen_field = false;
        while pos < block.len() {
            let b = block[pos];
            let (name, value) = if b & 0x80 != 0 {
                // indexed field
                let idx = decode_integer(block, &mut pos, 7)?;
                lookup(&self.table, idx)?
            } else if b & 0xc0 == 0x40 {
                // literal with incremental indexing
                let (n, v) = self.literal(block, &mut pos, 6)?;
                self.table.insert(n.clone(), v.clone());
                (n, v)
            } else if b & 0xe0 == 0x20 {
                // dynamic table size update: only at the beginning of a block
                if seen_field {
                    return Err(HpackError::InvalidTableSizeUpdate);
                }
                let size = decode_integer(block, &mut pos, 5)?;
                if size > self.max_allowed {
                    return Err(HpackError::InvalidTableSizeUpdate);
                }
                self.table.set_max_size(size);
                continue;
            } else {
                // literal without indexing (0000) / never indexed (0001)
                self.literal(block, &mut pos, 4)?
            };
            seen_field = true;
            list_size += entry_size(&name, &value);
            if self.max_header_list_size > 0 && list_size > self.max_header_list_size {
                return Err(HpackError::HeaderListTooLarge);
            }
            out.push((name, value));
        }
        Ok(out)
    }

    fn literal(&self, block: &[u8], pos: &mut usize, prefix: u8) -> Result<(String, String), HpackError> {
        let idx = decode_integer(block, pos, prefix)?;
        let name = if idx == 0 { bytes_to_string(decode_string(block, pos)?) } else { lookup(&self.table, idx)?.0 };
        let value = bytes_to_string(decode_string(block, pos)?);
        Ok((name, value))
    }
}

fn bytes_to_string(b: Vec<u8>) -> String {
    match String::from_utf8(b) {
        Ok(s) => s,
        Err(e) => e.into_bytes().iter().map(|&c| c as char).collect(),
    }
}

// ---------------------------------------------------------------------------
// encoder
// ---------------------------------------------------------------------------

/// Stateful header block encoder.
///
/// Strategy: exact static/dynamic matches are indexed; other fields are literals with
/// incremental indexing (sensitive ones — `authorization`, `cookie`, `set-cookie`,
/// `proxy-authorization` — use never-indexed literals); strings are Huffman-coded when shorter.
#[derive(Debug, Clone)]
pub struct Encoder {
    table: DynamicTable,
    /// Pending size updates to emit at the start of the next block.
    pending_updates: Vec<usize>,
    /// Use Huffman coding when beneficial (default true).
    pub huffman: bool,
    /// Add literals to the dynamic table (default true).
    pub use_indexing: bool,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder::new()
    }
}

impl Encoder {
    /// An encoder with the default 4096-byte dynamic table.
    pub fn new() -> Self {
        Encoder { table: DynamicTable::new(4096), pending_updates: Vec::new(), huffman: true, use_indexing: true }
    }
    pub fn table(&self) -> &DynamicTable {
        &self.table
    }
    /// Applies the peer's SETTINGS_HEADER_TABLE_SIZE (a size update is emitted with the next block).
    pub fn set_max_table_size(&mut self, size: usize) {
        if size != self.table.max_size() || !self.pending_updates.is_empty() {
            let min_pending = self.pending_updates.iter().copied().min().unwrap_or(usize::MAX);
            self.pending_updates.clear();
            if min_pending < size {
                self.pending_updates.push(min_pending);
            }
            self.pending_updates.push(size);
            self.table.set_max_size(size);
        }
    }

    fn find(&self, name: &str, value: &str) -> (Option<usize>, Option<usize>) {
        let mut name_idx = None;
        for (i, (n, v)) in STATIC_TABLE.iter().enumerate() {
            if *n == name {
                if *v == value {
                    return (Some(i + 1), Some(i + 1));
                }
                name_idx.get_or_insert(i + 1);
            }
        }
        for i in 0..self.table.len() {
            let (n, v) = self.table.get(i).unwrap();
            if n == name {
                let idx = STATIC_TABLE.len() + 1 + i;
                if v == value {
                    return (Some(idx), Some(idx));
                }
                name_idx.get_or_insert(idx);
            }
        }
        (None, name_idx)
    }

    /// Encodes a header list into one header block.
    pub fn encode(&mut self, headers: &[(String, String)]) -> Vec<u8> {
        let mut out = Vec::new();
        for size in self.pending_updates.drain(..) {
            encode_integer(size, 5, 0x20, &mut out);
        }
        for (name, value) in headers {
            let (full, name_idx) = self.find(name, value);
            if let Some(i) = full {
                encode_integer(i, 7, 0x80, &mut out);
                continue;
            }
            let sensitive = matches!(name.as_str(), "authorization" | "proxy-authorization" | "cookie" | "set-cookie");
            let index = self.use_indexing && !sensitive && entry_size(name, value) <= self.table.max_size() / 2;
            let (prefix, first) = if sensitive {
                (4, 0x10)
            } else if index {
                (6, 0x40)
            } else {
                (4, 0x00)
            };
            match name_idx {
                Some(i) => encode_integer(i, prefix, first, &mut out),
                None => {
                    encode_integer(0, prefix, first, &mut out);
                    encode_string(name.as_bytes(), self.huffman, &mut out);
                }
            }
            encode_string(value.as_bytes(), self.huffman, &mut out);
            if index {
                self.table.insert(name.clone(), value.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    fn h(list: &[(&str, &str)]) -> HeaderList {
        list.iter().map(|(n, v)| (n.to_string(), v.to_string())).collect()
    }

    #[test]
    fn integer_examples_c1() {
        let mut out = Vec::new();
        encode_integer(10, 5, 0, &mut out);
        assert_eq!(out, [0b01010]);
        out.clear();
        encode_integer(1337, 5, 0, &mut out);
        assert_eq!(out, [31, 154, 10]);
        let mut pos = 0;
        assert_eq!(decode_integer(&out, &mut pos, 5).unwrap(), 1337);
        assert_eq!(pos, 3);
        out.clear();
        encode_integer(42, 8, 0, &mut out);
        assert_eq!(out, [42]);
        // overflow protection
        let mut pos = 0;
        assert!(decode_integer(&[0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x7f], &mut pos, 5).is_err());
    }

    #[test]
    fn c2_literals() {
        // C.2.1 literal with indexing
        let mut d = Decoder::new(4096);
        let block = hex("400a 6375 7374 6f6d 2d6b 6579 0d63 7573 746f 6d2d 6865 6164 6572");
        assert_eq!(d.decode(&block).unwrap(), h(&[("custom-key", "custom-header")]));
        assert_eq!(d.table().size(), 55);
        // C.2.2 literal without indexing
        let mut d = Decoder::new(4096);
        assert_eq!(d.decode(&hex("040c 2f73 616d 706c 652f 7061 7468")).unwrap(), h(&[(":path", "/sample/path")]));
        assert_eq!(d.table().len(), 0);
        // C.2.3 never indexed
        let mut d = Decoder::new(4096);
        assert_eq!(d.decode(&hex("1008 7061 7373 776f 7264 0673 6563 7265 74")).unwrap(), h(&[("password", "secret")]));
        assert_eq!(d.table().len(), 0);
        // C.2.4 indexed
        assert_eq!(d.decode(&hex("82")).unwrap(), h(&[(":method", "GET")]));
    }

    fn requests() -> Vec<HeaderList> {
        vec![
            h(&[(":method", "GET"), (":scheme", "http"), (":path", "/"), (":authority", "www.example.com")]),
            h(&[(":method", "GET"), (":scheme", "http"), (":path", "/"), (":authority", "www.example.com"), ("cache-control", "no-cache")]),
            h(&[(":method", "GET"), (":scheme", "https"), (":path", "/index.html"), (":authority", "www.example.com"), ("custom-key", "custom-value")]),
        ]
    }

    #[test]
    fn c3_requests_without_huffman() {
        let blocks = [
            hex("8286 8441 0f77 7777 2e65 7861 6d70 6c65 2e63 6f6d"),
            hex("8286 84be 5808 6e6f 2d63 6163 6865"),
            hex("8287 85bf 400a 6375 7374 6f6d 2d6b 6579 0c63 7573 746f 6d2d 7661 6c75 65"),
        ];
        let mut d = Decoder::new(4096);
        let mut e = Encoder::new();
        e.huffman = false;
        let sizes = [57, 110, 164];
        for (i, req) in requests().iter().enumerate() {
            assert_eq!(&d.decode(&blocks[i]).unwrap(), req);
            assert_eq!(d.table().size(), sizes[i]);
            assert_eq!(e.encode(req), blocks[i], "request {}", i + 1);
        }
    }

    #[test]
    fn c4_requests_with_huffman() {
        let blocks = [
            hex("8286 8441 8cf1 e3c2 e5f2 3a6b a0ab 90f4 ff"),
            hex("8286 84be 5886 a8eb 1064 9cbf"),
            hex("8287 85bf 4088 25a8 49e9 5ba9 7d7f 8925 a849 e95b b8e8 b4bf"),
        ];
        let mut d = Decoder::new(4096);
        let mut e = Encoder::new();
        for (i, req) in requests().iter().enumerate() {
            assert_eq!(&d.decode(&blocks[i]).unwrap(), req);
            assert_eq!(e.encode(req), blocks[i], "request {}", i + 1);
        }
        assert_eq!(d.table().size(), 164);
    }

    fn responses() -> Vec<HeaderList> {
        vec![
            h(&[(":status", "302"), ("cache-control", "private"), ("date", "Mon, 21 Oct 2013 20:13:21 GMT"), ("location", "https://www.example.com")]),
            h(&[(":status", "307"), ("cache-control", "private"), ("date", "Mon, 21 Oct 2013 20:13:21 GMT"), ("location", "https://www.example.com")]),
            h(&[
                (":status", "200"),
                ("cache-control", "private"),
                ("date", "Mon, 21 Oct 2013 20:13:22 GMT"),
                ("location", "https://www.example.com"),
                ("content-encoding", "gzip"),
                ("set-cookie", "foo=ASDJKHQKBZXOQWEOPIUAXQWEOIU; max-age=3600; version=1"),
            ]),
        ]
    }

    #[test]
    fn c5_responses_without_huffman() {
        let blocks = [
            hex("4803 3330 3258 0770 7269 7661 7465 611d 4d6f 6e2c 2032 3120 4f63 7420 3230 3133 2032 303a 3133 3a32 3120 474d 546e 1768 7474 7073 3a2f 2f77 7777 2e65 7861 6d70 6c65 2e63 6f6d"),
            hex("4803 3330 37c1 c0bf"),
            hex("88c1 611d 4d6f 6e2c 2032 3120 4f63 7420 3230 3133 2032 303a 3133 3a32 3220 474d 54c0 5a04 677a 6970 7738 666f 6f3d 4153 444a 4b48 514b 425a 584f 5157 454f 5049 5541 5851 5745 4f49 553b 206d 6178 2d61 6765 3d33 3630 303b 2076 6572 7369 6f6e 3d31"),
        ];
        let mut d = Decoder::new(256);
        for (i, resp) in responses().iter().enumerate() {
            assert_eq!(&d.decode(&blocks[i]).unwrap(), resp, "response {}", i + 1);
        }
        assert_eq!(d.table().size(), 215);
        assert_eq!(d.table().len(), 3);
        assert_eq!(d.table().get(0).unwrap().0, "set-cookie");
    }

    #[test]
    fn c6_responses_with_huffman() {
        let blocks = [
            hex("4882 6402 5885 aec3 771a 4b61 96d0 7abe 9410 54d4 44a8 2005 9504 0b81 66e0 82a6 2d1b ff6e 919d 29ad 1718 63c7 8f0b 97c8 e9ae 82ae 43d3"),
            hex("4883 640e ff c1 c0 bf"),
            hex("88c1 6196 d07a be94 1054 d444 a820 0595 040b 8166 e084 a62d 1bff c05a 839b d9ab 77ad 94e7 821d d7f2 e6c7 b335 dfdf cd5b 3960 d5af 2708 7f36 72c1 ab27 0fb5 291f 9587 3160 65c0 03ed 4ee5 b106 3d50 07"),
        ];
        let mut d = Decoder::new(256);
        for (i, resp) in responses().iter().enumerate() {
            assert_eq!(&d.decode(&blocks[i]).unwrap(), resp, "response {}", i + 1);
        }
        assert_eq!(d.table().size(), 215);
        // our encoder (with a 256 byte table) round-trips the same lists
        let mut e = Encoder::new();
        e.set_max_table_size(256);
        let mut d2 = Decoder::new(4096);
        for resp in responses() {
            assert_eq!(d2.decode(&e.encode(&resp)).unwrap(), resp);
        }
    }

    #[test]
    fn table_size_update_rules() {
        let mut d = Decoder::new(4096);
        // size update to 0 at start is fine
        assert_eq!(d.decode(&[0x20, 0x82]).unwrap(), h(&[(":method", "GET")]));
        // after a field: error
        assert_eq!(d.decode(&[0x82, 0x20]), Err(HpackError::InvalidTableSizeUpdate));
        // larger than allowed
        let mut block = Vec::new();
        encode_integer(8192, 5, 0x20, &mut block);
        assert_eq!(d.decode(&block), Err(HpackError::InvalidTableSizeUpdate));
        // index out of range
        assert_eq!(d.decode(&[0xff, 0x00]), Err(HpackError::InvalidIndex(127)));
        // encoder emits a pending size update
        let mut e = Encoder::new();
        e.set_max_table_size(100);
        let b = e.encode(&h(&[("a", "b")]));
        assert_eq!(b[0] & 0xe0, 0x20);
        let mut d = Decoder::new(4096);
        assert_eq!(d.decode(&b).unwrap(), h(&[("a", "b")]));
        assert_eq!(d.table().max_size(), 100);
    }
}
