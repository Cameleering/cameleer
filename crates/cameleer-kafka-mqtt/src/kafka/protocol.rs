//! Kafka wire protocol: big-endian primitives, request/response framing, the
//! request and response bodies of the supported API versions, record batches (magic 2,
//! CRC32C, varint records with headers), CRC32C and the murmur2 partitioner hash.
//!
//! All APIs use non-flexible versions that are supported by Kafka 2.1 up to 4.x:
//!
//! | API | key | version |
//! |---|---|---|
//! | Produce | 0 | 3 |
//! | Fetch | 1 | 4 |
//! | ListOffsets | 2 | 1 |
//! | Metadata | 3 | 5 |
//! | OffsetCommit | 8 | 2 |
//! | OffsetFetch | 9 | 1 |
//! | FindCoordinator | 10 | 1 |
//! | JoinGroup | 11 | 4 |
//! | Heartbeat | 12 | 1 |
//! | LeaveGroup | 13 | 1 |
//! | SyncGroup | 14 | 1 |
//! | ApiVersions | 18 | 0 |
//! | CreateTopics | 19 | 2 |

use std::io::{Read, Write};

use cameleer_core::error::{CamelError, Result};

/// Exception kind used for Kafka protocol and broker errors.
pub const KAFKA_EXCEPTION: &str = "KafkaException";

/// API keys.
pub mod api {
    pub const PRODUCE: i16 = 0;
    pub const FETCH: i16 = 1;
    pub const LIST_OFFSETS: i16 = 2;
    pub const METADATA: i16 = 3;
    pub const OFFSET_COMMIT: i16 = 8;
    pub const OFFSET_FETCH: i16 = 9;
    pub const FIND_COORDINATOR: i16 = 10;
    pub const JOIN_GROUP: i16 = 11;
    pub const HEARTBEAT: i16 = 12;
    pub const LEAVE_GROUP: i16 = 13;
    pub const SYNC_GROUP: i16 = 14;
    pub const SASL_HANDSHAKE: i16 = 17;
    pub const API_VERSIONS: i16 = 18;
    pub const CREATE_TOPICS: i16 = 19;
    pub const SASL_AUTHENTICATE: i16 = 36;

    /// `(api key, version)` used by this client for every API.
    pub const VERSIONS: &[(i16, i16)] = &[
        (PRODUCE, 3),
        (FETCH, 4),
        (LIST_OFFSETS, 1),
        (METADATA, 5),
        (OFFSET_COMMIT, 2),
        (OFFSET_FETCH, 1),
        (FIND_COORDINATOR, 1),
        (JOIN_GROUP, 4),
        (HEARTBEAT, 1),
        (LEAVE_GROUP, 1),
        (SYNC_GROUP, 1),
        (API_VERSIONS, 0),
        (CREATE_TOPICS, 2),
        (SASL_HANDSHAKE, 1),
        (SASL_AUTHENTICATE, 1),
    ];

    /// APIs that a broker may lack (only needed for some features).
    pub fn optional(key: i16) -> bool {
        matches!(key, CREATE_TOPICS | SASL_HANDSHAKE | SASL_AUTHENTICATE)
    }

    /// The version used for an API key.
    pub fn version(key: i16) -> i16 {
        VERSIONS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v).unwrap_or(0)
    }

    /// The API name (for messages).
    pub fn name(key: i16) -> &'static str {
        match key {
            PRODUCE => "Produce",
            FETCH => "Fetch",
            LIST_OFFSETS => "ListOffsets",
            METADATA => "Metadata",
            OFFSET_COMMIT => "OffsetCommit",
            OFFSET_FETCH => "OffsetFetch",
            FIND_COORDINATOR => "FindCoordinator",
            JOIN_GROUP => "JoinGroup",
            HEARTBEAT => "Heartbeat",
            LEAVE_GROUP => "LeaveGroup",
            SYNC_GROUP => "SyncGroup",
            API_VERSIONS => "ApiVersions",
            CREATE_TOPICS => "CreateTopics",
            SASL_HANDSHAKE => "SaslHandshake",
            SASL_AUTHENTICATE => "SaslAuthenticate",
            _ => "Unknown",
        }
    }
}

/// Error codes used by the client and the test broker.
pub mod errors {
    pub const NONE: i16 = 0;
    pub const OFFSET_OUT_OF_RANGE: i16 = 1;
    pub const CORRUPT_MESSAGE: i16 = 2;
    pub const UNKNOWN_TOPIC_OR_PARTITION: i16 = 3;
    pub const LEADER_NOT_AVAILABLE: i16 = 5;
    pub const NOT_LEADER_OR_FOLLOWER: i16 = 6;
    pub const REQUEST_TIMED_OUT: i16 = 7;
    pub const COORDINATOR_LOAD_IN_PROGRESS: i16 = 14;
    pub const COORDINATOR_NOT_AVAILABLE: i16 = 15;
    pub const NOT_COORDINATOR: i16 = 16;
    pub const INVALID_TOPIC_EXCEPTION: i16 = 17;
    pub const ILLEGAL_GENERATION: i16 = 22;
    pub const INCONSISTENT_GROUP_PROTOCOL: i16 = 23;
    pub const UNKNOWN_MEMBER_ID: i16 = 25;
    pub const INVALID_SESSION_TIMEOUT: i16 = 26;
    pub const REBALANCE_IN_PROGRESS: i16 = 27;
    pub const UNSUPPORTED_SASL_MECHANISM: i16 = 33;
    pub const ILLEGAL_SASL_STATE: i16 = 34;
    pub const UNSUPPORTED_VERSION: i16 = 35;
    pub const SASL_AUTHENTICATION_FAILED: i16 = 58;
    pub const TOPIC_ALREADY_EXISTS: i16 = 36;
    pub const INVALID_PARTITIONS: i16 = 37;
    pub const INVALID_REPLICATION_FACTOR: i16 = 38;
    pub const MEMBER_ID_REQUIRED: i16 = 79;
    pub const NOT_CONTROLLER: i16 = 41;
    pub const INVALID_REQUEST: i16 = 42;
    pub const TOPIC_AUTHORIZATION_FAILED: i16 = 29;
    pub const GROUP_AUTHORIZATION_FAILED: i16 = 30;
    pub const UNKNOWN_SERVER_ERROR: i16 = -1;

    /// The Kafka error name for a code.
    pub fn name(code: i16) -> &'static str {
        match code {
            NONE => "NONE",
            OFFSET_OUT_OF_RANGE => "OFFSET_OUT_OF_RANGE",
            CORRUPT_MESSAGE => "CORRUPT_MESSAGE",
            UNKNOWN_TOPIC_OR_PARTITION => "UNKNOWN_TOPIC_OR_PARTITION",
            LEADER_NOT_AVAILABLE => "LEADER_NOT_AVAILABLE",
            NOT_LEADER_OR_FOLLOWER => "NOT_LEADER_OR_FOLLOWER",
            REQUEST_TIMED_OUT => "REQUEST_TIMED_OUT",
            COORDINATOR_LOAD_IN_PROGRESS => "COORDINATOR_LOAD_IN_PROGRESS",
            COORDINATOR_NOT_AVAILABLE => "COORDINATOR_NOT_AVAILABLE",
            NOT_COORDINATOR => "NOT_COORDINATOR",
            INVALID_TOPIC_EXCEPTION => "INVALID_TOPIC_EXCEPTION",
            ILLEGAL_GENERATION => "ILLEGAL_GENERATION",
            INCONSISTENT_GROUP_PROTOCOL => "INCONSISTENT_GROUP_PROTOCOL",
            UNKNOWN_MEMBER_ID => "UNKNOWN_MEMBER_ID",
            INVALID_SESSION_TIMEOUT => "INVALID_SESSION_TIMEOUT",
            REBALANCE_IN_PROGRESS => "REBALANCE_IN_PROGRESS",
            UNSUPPORTED_VERSION => "UNSUPPORTED_VERSION",
            UNSUPPORTED_SASL_MECHANISM => "UNSUPPORTED_SASL_MECHANISM",
            ILLEGAL_SASL_STATE => "ILLEGAL_SASL_STATE",
            SASL_AUTHENTICATION_FAILED => "SASL_AUTHENTICATION_FAILED",
            TOPIC_ALREADY_EXISTS => "TOPIC_ALREADY_EXISTS",
            INVALID_PARTITIONS => "INVALID_PARTITIONS",
            INVALID_REPLICATION_FACTOR => "INVALID_REPLICATION_FACTOR",
            NOT_CONTROLLER => "NOT_CONTROLLER",
            MEMBER_ID_REQUIRED => "MEMBER_ID_REQUIRED",
            INVALID_REQUEST => "INVALID_REQUEST",
            TOPIC_AUTHORIZATION_FAILED => "TOPIC_AUTHORIZATION_FAILED",
            GROUP_AUTHORIZATION_FAILED => "GROUP_AUTHORIZATION_FAILED",
            UNKNOWN_SERVER_ERROR => "UNKNOWN_SERVER_ERROR",
            _ => "UNKNOWN_ERROR_CODE",
        }
    }

    /// Whether a metadata/leadership refresh and retry may resolve the error.
    pub fn is_retriable(code: i16) -> bool {
        matches!(
            code,
            LEADER_NOT_AVAILABLE
                | NOT_LEADER_OR_FOLLOWER
                | REQUEST_TIMED_OUT
                | COORDINATOR_LOAD_IN_PROGRESS
                | COORDINATOR_NOT_AVAILABLE
                | NOT_COORDINATOR
                | UNKNOWN_TOPIC_OR_PARTITION
        )
    }
}

/// A `KafkaException` for an error code.
pub fn kafka_error(code: i16, context: &str) -> CamelError {
    CamelError::new(KAFKA_EXCEPTION, format!("{}: {} (error code {})", context, errors::name(code), code))
}

fn short(what: &str) -> CamelError {
    CamelError::new(KAFKA_EXCEPTION, format!("Malformed Kafka message: unexpected end of data reading {}", what))
}

// ---------------------------------------------------------------------------
// primitives
// ---------------------------------------------------------------------------

/// Big-endian encoder.
#[derive(Default, Debug, Clone)]
pub struct Encoder {
    pub buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn i8(&mut self, v: i8) -> &mut Self {
        self.buf.push(v as u8);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.i8(v as i8)
    }
    pub fn i16(&mut self, v: i16) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn string(&mut self, s: &str) -> &mut Self {
        self.i16(s.len() as i16);
        self.buf.extend_from_slice(s.as_bytes());
        self
    }
    pub fn nullable_string(&mut self, s: Option<&str>) -> &mut Self {
        match s {
            Some(s) => self.string(s),
            None => self.i16(-1),
        }
    }
    pub fn bytes(&mut self, b: &[u8]) -> &mut Self {
        self.i32(b.len() as i32);
        self.buf.extend_from_slice(b);
        self
    }
    pub fn nullable_bytes(&mut self, b: Option<&[u8]>) -> &mut Self {
        match b {
            Some(b) => self.bytes(b),
            None => self.i32(-1),
        }
    }
    /// Array length prefix.
    pub fn len(&mut self, n: usize) -> &mut Self {
        self.i32(n as i32)
    }
    pub fn raw(&mut self, b: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(b);
        self
    }
    /// Zig-zag varint (32 bit).
    pub fn varint(&mut self, v: i32) -> &mut Self {
        self.uvarint(((v << 1) ^ (v >> 31)) as u32 as u64)
    }
    /// Zig-zag varlong (64 bit).
    pub fn varlong(&mut self, v: i64) -> &mut Self {
        self.uvarint(((v << 1) ^ (v >> 63)) as u64)
    }
    fn uvarint(&mut self, mut v: u64) -> &mut Self {
        while v >= 0x80 {
            self.buf.push((v as u8 & 0x7f) | 0x80);
            v >>= 7;
        }
        self.buf.push(v as u8);
        self
    }
    /// Varint-length-prefixed bytes (-1 = null), as used in records.
    pub fn varbytes(&mut self, b: Option<&[u8]>) -> &mut Self {
        match b {
            Some(b) => {
                self.varint(b.len() as i32);
                self.buf.extend_from_slice(b);
            }
            None => {
                self.varint(-1);
            }
        }
        self
    }
    pub fn int32_array(&mut self, v: &[i32]) -> &mut Self {
        self.len(v.len());
        for x in v {
            self.i32(*x);
        }
        self
    }
}

/// Big-endian decoder over a byte slice.
pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Decoder { data, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    fn take(&mut self, n: usize, what: &str) -> Result<&'a [u8]> {
        if self.remaining() < n {
            return Err(short(what));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn i8(&mut self) -> Result<i8> {
        Ok(self.take(1, "int8")?[0] as i8)
    }
    pub fn bool(&mut self) -> Result<bool> {
        Ok(self.i8()? != 0)
    }
    pub fn i16(&mut self) -> Result<i16> {
        Ok(i16::from_be_bytes(self.take(2, "int16")?.try_into().unwrap()))
    }
    pub fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.take(4, "int32")?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4, "uint32")?.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8, "int64")?.try_into().unwrap()))
    }
    pub fn nullable_string(&mut self) -> Result<Option<String>> {
        let n = self.i16()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(self.take(n as usize, "string")?).into_owned()))
    }
    pub fn string(&mut self) -> Result<String> {
        Ok(self.nullable_string()?.unwrap_or_default())
    }
    pub fn nullable_bytes(&mut self) -> Result<Option<Vec<u8>>> {
        let n = self.i32()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(self.take(n as usize, "bytes")?.to_vec()))
    }
    pub fn bytes(&mut self) -> Result<Vec<u8>> {
        Ok(self.nullable_bytes()?.unwrap_or_default())
    }
    /// Array length (-1 = null, returned as None).
    pub fn array_len(&mut self) -> Result<Option<usize>> {
        let n = self.i32()?;
        if n < 0 {
            return Ok(None);
        }
        if n as usize > self.remaining() {
            return Err(short("array"));
        }
        Ok(Some(n as usize))
    }
    /// Array length, null treated as empty.
    pub fn len(&mut self) -> Result<usize> {
        Ok(self.array_len()?.unwrap_or(0))
    }
    pub fn raw(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n, "raw bytes")
    }
    fn uvarint(&mut self) -> Result<u64> {
        let mut v: u64 = 0;
        let mut shift = 0;
        loop {
            let b = self.take(1, "varint")?[0];
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
            if shift > 63 {
                return Err(CamelError::new(KAFKA_EXCEPTION, "Malformed varint"));
            }
        }
    }
    pub fn varint(&mut self) -> Result<i32> {
        let u = self.uvarint()? as u32;
        Ok(((u >> 1) as i32) ^ -((u & 1) as i32))
    }
    pub fn varlong(&mut self) -> Result<i64> {
        let u = self.uvarint()?;
        Ok(((u >> 1) as i64) ^ -((u & 1) as i64))
    }
    pub fn varbytes(&mut self) -> Result<Option<Vec<u8>>> {
        let n = self.varint()?;
        if n < 0 {
            return Ok(None);
        }
        Ok(Some(self.take(n as usize, "record bytes")?.to_vec()))
    }
    pub fn int32_array(&mut self) -> Result<Vec<i32>> {
        let n = self.len()?;
        (0..n).map(|_| self.i32()).collect()
    }
}

// ---------------------------------------------------------------------------
// framing
// ---------------------------------------------------------------------------

/// Request header v1.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestHeader {
    pub api_key: i16,
    pub api_version: i16,
    pub correlation_id: i32,
    pub client_id: Option<String>,
}

/// Build a size-prefixed request frame.
pub fn encode_request(header: &RequestHeader, body: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.i32(0).i16(header.api_key).i16(header.api_version).i32(header.correlation_id).nullable_string(header.client_id.as_deref()).raw(body);
    let n = (e.buf.len() - 4) as i32;
    e.buf[..4].copy_from_slice(&n.to_be_bytes());
    e.buf
}

/// Build a size-prefixed response frame (response header v0).
pub fn encode_response(correlation_id: i32, body: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.i32((body.len() + 4) as i32).i32(correlation_id).raw(body);
    e.buf
}

/// Read one size-prefixed frame. `Ok(None)` on a clean EOF before the size.
pub fn read_frame<R: Read>(r: &mut R, max: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut size = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        match r.read(&mut size[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed inside a frame")),
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let n = i32::from_be_bytes(size);
    if n < 0 || n as usize > max {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("invalid Kafka frame size {}", n)));
    }
    let mut buf = vec![0u8; n as usize];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

/// Write a frame and flush.
pub fn write_frame<W: Write>(w: &mut W, frame: &[u8]) -> std::io::Result<()> {
    w.write_all(frame)?;
    w.flush()
}

/// Decode a request frame body into its header and the remaining body bytes.
pub fn decode_request_header(frame: &[u8]) -> Result<(RequestHeader, &[u8])> {
    let mut d = Decoder::new(frame);
    let h = RequestHeader { api_key: d.i16()?, api_version: d.i16()?, correlation_id: d.i32()?, client_id: d.nullable_string()? };
    let pos = d.position();
    Ok((h, &frame[pos..]))
}

// ---------------------------------------------------------------------------
// checksums and hashing
// ---------------------------------------------------------------------------

fn crc32c_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, slot) in t.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 { 0x82F6_3B78 ^ (c >> 1) } else { c >> 1 };
            }
            *slot = c;
        }
        t
    })
}

/// CRC-32C (Castagnoli), as used by record batches.
pub fn crc32c(data: &[u8]) -> u32 {
    let t = crc32c_table();
    let mut c = !0u32;
    for &b in data {
        c = t[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

/// Kafka's murmur2 hash (`org.apache.kafka.common.utils.Utils.murmur2`).
pub fn murmur2(data: &[u8]) -> i32 {
    let len = data.len();
    let m: u32 = 0x5bd1_e995;
    let r = 24;
    let mut h: u32 = 0x9747_b28c ^ (len as u32);
    let chunks = len / 4;
    for i in 0..chunks {
        let i4 = i * 4;
        let mut k = u32::from_le_bytes([data[i4], data[i4 + 1], data[i4 + 2], data[i4 + 3]]);
        k = k.wrapping_mul(m);
        k ^= k >> r;
        k = k.wrapping_mul(m);
        h = h.wrapping_mul(m);
        h ^= k;
    }
    let tail = len & !3;
    let rem = len % 4;
    if rem >= 3 {
        h ^= (data[tail + 2] as u32) << 16;
    }
    if rem >= 2 {
        h ^= (data[tail + 1] as u32) << 8;
    }
    if rem >= 1 {
        h ^= data[tail] as u32;
        h = h.wrapping_mul(m);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(m);
    h ^= h >> 15;
    h as i32
}

/// The default partitioner for keyed records: `toPositive(murmur2(key)) % partitions`.
pub fn partition_for_key(key: &[u8], partitions: usize) -> i32 {
    ((murmur2(key) & 0x7fff_ffff) as usize % partitions.max(1)) as i32
}

// ---------------------------------------------------------------------------
// record batches
// ---------------------------------------------------------------------------

/// A record (key, value, headers) with its offset and timestamp.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Record {
    pub offset: i64,
    pub timestamp: i64,
    pub key: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    pub headers: Vec<(String, Option<Vec<u8>>)>,
}

/// Record batch compression (attribute bits 0-2, Kafka's `compression.type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Compression {
    #[default]
    None,
    Gzip,
    Snappy,
    Lz4,
    Zstd,
}

/// Upper bound for a decompressed record batch.
const MAX_DECOMPRESSED: usize = 256 * 1024 * 1024;

impl Compression {
    /// Parses `none` / `gzip` / `snappy` / `lz4` / `zstd` (case-insensitive).
    pub fn from_name(name: &str) -> Result<Compression> {
        Ok(match name.trim().to_ascii_lowercase().as_str() {
            "" | "none" => Compression::None,
            "gzip" => Compression::Gzip,
            "snappy" => Compression::Snappy,
            "lz4" => Compression::Lz4,
            "zstd" => Compression::Zstd,
            other => return Err(CamelError::illegal_argument(format!("Unknown Kafka compression codec: {} (none, gzip, snappy, lz4, zstd)", other))),
        })
    }

    /// The codec id in record batch / message attributes.
    pub fn id(self) -> i16 {
        match self {
            Compression::None => 0,
            Compression::Gzip => 1,
            Compression::Snappy => 2,
            Compression::Lz4 => 3,
            Compression::Zstd => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Compression::None => "none",
            Compression::Gzip => "gzip",
            Compression::Snappy => "snappy",
            Compression::Lz4 => "lz4",
            Compression::Zstd => "zstd",
        }
    }

    /// Compresses record data the way the Kafka Java client does (gzip stream, xerial
    /// snappy-java framing, LZ4 frame). zstd needs Produce v7 and is not supported.
    pub fn compress(self, data: &[u8]) -> Result<Vec<u8>> {
        use cameleer_core::dataformat::{codecs, compress};
        Ok(match self {
            Compression::None => data.to_vec(),
            Compression::Gzip => compress::gzip_compress(data, 6),
            Compression::Snappy => codecs::snappy_java_compress(data),
            Compression::Lz4 => codecs::lz4_frame_compress(data),
            Compression::Zstd => {
                return Err(CamelError::new(
                    "UnsupportedCompressionTypeException",
                    "Kafka zstd compression requires Produce v7, which Cameleer does not use (none, gzip, snappy, lz4)",
                ))
            }
        })
    }

    /// Decompresses record data written with codec `id`.
    pub fn decompress(id: i16, data: &[u8]) -> Result<Vec<u8>> {
        use cameleer_core::dataformat::{codecs, compress};
        let fail = |name: &str, e: CamelError| CamelError::new(KAFKA_EXCEPTION, format!("Cannot decompress {} record batch: {}", name, e.message));
        match id {
            1 => compress::gzip_decompress(data).map_err(|e| fail("gzip", e)),
            2 => codecs::snappy_java_decompress(data, MAX_DECOMPRESSED).map_err(|e| fail("snappy", e)),
            3 => codecs::lz4_frame_decompress(data, MAX_DECOMPRESSED).map_err(|e| fail("lz4", e)),
            4 => Err(CamelError::new(KAFKA_EXCEPTION, "Unsupported record batch compression codec: zstd")),
            c => Err(CamelError::new(KAFKA_EXCEPTION, format!("Unknown record batch compression codec {}", c))),
        }
    }
}

/// Encode records as one uncompressed record batch (magic 2). Offsets are relative to
/// `base_offset` (records' own offsets are ignored, they are numbered consecutively).
pub fn encode_record_batch(base_offset: i64, records: &[Record]) -> Vec<u8> {
    encode_record_batch_with(base_offset, records, Compression::None).expect("uncompressed batches always encode")
}

/// Encode records as one record batch (magic 2) compressed with `codec`.
pub fn encode_record_batch_with(base_offset: i64, records: &[Record], codec: Compression) -> Result<Vec<u8>> {
    let first_ts = records.first().map(|r| r.timestamp).unwrap_or(0);
    let max_ts = records.iter().map(|r| r.timestamp).max().unwrap_or(first_ts);
    let mut recs = Encoder::new();
    for (i, r) in records.iter().enumerate() {
        let mut rec = Encoder::new();
        rec.i8(0).varlong(r.timestamp - first_ts).varint(i as i32).varbytes(r.key.as_deref()).varbytes(r.value.as_deref()).varint(r.headers.len() as i32);
        for (k, v) in &r.headers {
            rec.varbytes(Some(k.as_bytes())).varbytes(v.as_deref());
        }
        recs.varint(rec.buf.len() as i32).raw(&rec.buf);
    }
    let payload = codec.compress(&recs.buf)?;
    // everything covered by the CRC: attributes .. records
    let mut body = Encoder::new();
    body.i16(codec.id()) // attributes: codec, create time
        .i32(records.len().saturating_sub(1) as i32)
        .i64(first_ts)
        .i64(max_ts)
        .i64(-1) // producer id
        .i16(-1) // producer epoch
        .i32(-1) // base sequence
        .len(records.len())
        .raw(&payload);
    let crc = crc32c(&body.buf);
    let mut e = Encoder::new();
    // batchLength counts from partitionLeaderEpoch to the end
    let batch_len = 4 + 1 + 4 + body.buf.len();
    e.i64(base_offset).i32(batch_len as i32).i32(-1).i8(2).u32(crc).raw(&body.buf);
    Ok(e.buf)
}

/// Splits `data` into its complete record batches / legacy message set entries (each with
/// its 12-byte offset + length prefix). A truncated trailing entry is dropped.
pub fn split_record_batches(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while data.len() - i >= 12 {
        let len = i32::from_be_bytes([data[i + 8], data[i + 9], data[i + 10], data[i + 11]]);
        if len < 0 || len as usize > data.len() - i - 12 {
            break;
        }
        let end = i + 12 + len as usize;
        out.push(&data[i..end]);
        i = end;
    }
    out
}

/// Decode all complete record batches in `data` (a fetch response's records or a produce
/// request's payload). A truncated trailing batch is ignored; control batches are
/// skipped; gzip, snappy (raw or snappy-java framed) and lz4 (frame format) compression
/// are supported. Legacy message sets (magic 0 and 1, message format before Kafka 0.11)
/// are decoded too, including compressed wrapper messages.
pub fn decode_record_batches(data: &[u8]) -> Result<Vec<Record>> {
    let mut out = Vec::new();
    let mut d = Decoder::new(data);
    while d.remaining() >= 12 {
        let base_offset = d.i64()?;
        let batch_len = d.i32()?;
        if batch_len < 0 || batch_len as usize > d.remaining() {
            break; // partial batch at the end of a fetch response
        }
        let batch = d.raw(batch_len as usize)?;
        if batch.len() < 5 {
            return Err(CamelError::new(KAFKA_EXCEPTION, "Record batch too short (corrupt message)"));
        }
        let magic = batch[4] as i8;
        if magic == 0 || magic == 1 {
            decode_legacy_message(base_offset, batch, &mut out, 0)?;
            continue;
        }
        let mut b = Decoder::new(batch);
        let _leader_epoch = b.i32()?;
        let magic = b.i8()?;
        if magic != 2 {
            return Err(CamelError::new(KAFKA_EXCEPTION, format!("Unsupported record batch magic {} (only v0, v1 and v2 are supported)", magic)));
        }
        let crc = b.u32()?;
        let rest = &batch[b.position()..];
        if crc32c(rest) != crc {
            return Err(CamelError::new(KAFKA_EXCEPTION, "Record batch CRC32C mismatch (corrupt message)"));
        }
        let attributes = b.i16()?;
        let _last_offset_delta = b.i32()?;
        let first_ts = b.i64()?;
        let max_ts = b.i64()?;
        let _pid = b.i64()?;
        let _epoch = b.i16()?;
        let _seq = b.i32()?;
        let count = b.i32()?;
        if attributes & 0x20 != 0 {
            continue; // control batch (transaction markers)
        }
        let log_append_time = attributes & 0x08 != 0;
        let records_data = &batch[b.position()..];
        let decompressed;
        let recs: &[u8] = match attributes & 0x07 {
            0 => records_data,
            c => {
                decompressed = Compression::decompress(c, records_data)?;
                &decompressed
            }
        };
        let mut r = Decoder::new(recs);
        for _ in 0..count.max(0) {
            let len = r.varint()?;
            let rec = r.raw(len.max(0) as usize)?;
            let mut x = Decoder::new(rec);
            let _attrs = x.i8()?;
            let ts_delta = x.varlong()?;
            let off_delta = x.varint()?;
            let key = x.varbytes()?;
            let value = x.varbytes()?;
            let hn = x.varint()?;
            let mut headers = Vec::new();
            for _ in 0..hn.max(0) {
                let k = x.varbytes()?.unwrap_or_default();
                let v = x.varbytes()?;
                headers.push((String::from_utf8_lossy(&k).into_owned(), v));
            }
            out.push(Record {
                offset: base_offset + off_delta as i64,
                timestamp: if log_append_time { max_ts } else { first_ts + ts_delta },
                key,
                value,
                headers,
            });
        }
    }
    Ok(out)
}

/// Decodes one legacy message (magic 0 / 1: `crc attributes [timestamp] key value`) stored
/// at `offset`. A compressed message wraps an inner message set: with magic 1 the inner
/// offsets are relative and the wrapper carries the offset of the last inner message.
fn decode_legacy_message(offset: i64, msg: &[u8], out: &mut Vec<Record>, depth: usize) -> Result<()> {
    let mut m = Decoder::new(msg);
    let crc = m.u32()?;
    if cameleer_core::dataformat::compress::crc32(&msg[4..]) != crc {
        return Err(CamelError::new(KAFKA_EXCEPTION, "Message CRC32 mismatch (corrupt message)"));
    }
    let magic = m.i8()?;
    let attributes = m.i8()?;
    let timestamp = if magic >= 1 { m.i64()? } else { -1 };
    let key = m.nullable_bytes()?;
    let value = m.nullable_bytes()?;
    let codec = (attributes & 0x07) as i16;
    if codec == 0 {
        out.push(Record { offset, timestamp, key, value, headers: Vec::new() });
        return Ok(());
    }
    if depth > 0 {
        return Err(CamelError::new(KAFKA_EXCEPTION, "Nested compressed messages (corrupt message)"));
    }
    let inner = Compression::decompress(codec, value.as_deref().unwrap_or_default())?;
    let entries = split_record_batches(&inner);
    let mut inner_records = Vec::new();
    for e in &entries {
        let rel = i64::from_be_bytes(e[..8].try_into().unwrap());
        decode_legacy_message(rel, &e[12..], &mut inner_records, depth + 1)?;
    }
    let last_rel = inner_records.last().map(|r| r.offset).unwrap_or(0);
    let log_append_time = attributes & 0x08 != 0;
    for mut r in inner_records {
        if magic >= 1 {
            r.offset = offset - last_rel + r.offset;
            if log_append_time {
                r.timestamp = timestamp;
            }
        }
        out.push(r);
    }
    Ok(())
}

/// Encode records as an uncompressed legacy message set (magic 1, the format of Produce /
/// Fetch v2 and older). Each record keeps its own absolute offset; headers are dropped
/// (message format v1 has none).
pub fn encode_message_set_v1(records: &[Record]) -> Vec<u8> {
    let mut e = Encoder::new();
    for r in records {
        let mut m = Encoder::new();
        m.i8(1).i8(0).i64(r.timestamp).nullable_bytes(r.key.as_deref()).nullable_bytes(r.value.as_deref());
        let crc = cameleer_core::dataformat::compress::crc32(&m.buf);
        e.i64(r.offset).i32(4 + m.buf.len() as i32).u32(crc).raw(&m.buf);
    }
    e.buf
}

// ---------------------------------------------------------------------------
// API bodies
// ---------------------------------------------------------------------------

/// SaslHandshake v1 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SaslHandshakeResponse {
    pub error_code: i16,
    pub mechanisms: Vec<String>,
}

impl SaslHandshakeResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i16(self.error_code).len(self.mechanisms.len());
        for m in &self.mechanisms {
            e.string(m);
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let error_code = d.i16()?;
        let n = d.len()?;
        let mut mechanisms = Vec::with_capacity(n);
        for _ in 0..n {
            mechanisms.push(d.string()?);
        }
        Ok(SaslHandshakeResponse { error_code, mechanisms })
    }
}

/// SaslAuthenticate v1 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SaslAuthenticateResponse {
    pub error_code: i16,
    pub error_message: Option<String>,
    pub auth_bytes: Vec<u8>,
    pub session_lifetime_ms: i64,
}

impl SaslAuthenticateResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i16(self.error_code).nullable_string(self.error_message.as_deref()).bytes(&self.auth_bytes).i64(self.session_lifetime_ms);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(SaslAuthenticateResponse { error_code: d.i16()?, error_message: d.nullable_string()?, auth_bytes: d.bytes()?, session_lifetime_ms: d.i64()? })
    }
}

/// ApiVersions v0 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ApiVersionsResponse {
    pub error_code: i16,
    /// `(api_key, min_version, max_version)`
    pub apis: Vec<(i16, i16, i16)>,
}

impl ApiVersionsResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i16(self.error_code).len(self.apis.len());
        for (k, min, max) in &self.apis {
            e.i16(*k).i16(*min).i16(*max);
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let error_code = d.i16()?;
        let n = d.len()?;
        let mut apis = Vec::with_capacity(n);
        for _ in 0..n {
            apis.push((d.i16()?, d.i16()?, d.i16()?));
        }
        Ok(ApiVersionsResponse { error_code, apis })
    }
}

/// Metadata v5 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MetadataRequest {
    /// None = all topics.
    pub topics: Option<Vec<String>>,
    pub allow_auto_topic_creation: bool,
}

impl MetadataRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        match &self.topics {
            None => {
                e.i32(-1);
            }
            Some(t) => {
                e.len(t.len());
                for s in t {
                    e.string(s);
                }
            }
        }
        e.bool(self.allow_auto_topic_creation);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let topics = match d.array_len()? {
            None => None,
            Some(n) => Some((0..n).map(|_| d.string()).collect::<Result<Vec<_>>>()?),
        };
        let allow_auto_topic_creation = if d.remaining() > 0 { d.bool()? } else { true };
        Ok(MetadataRequest { topics, allow_auto_topic_creation })
    }
}

/// A broker in a metadata response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BrokerMeta {
    pub node_id: i32,
    pub host: String,
    pub port: i32,
    pub rack: Option<String>,
}

/// A partition in a metadata response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PartitionMeta {
    pub error_code: i16,
    pub partition: i32,
    pub leader: i32,
    pub replicas: Vec<i32>,
    pub isr: Vec<i32>,
    pub offline: Vec<i32>,
}

/// A topic in a metadata response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct TopicMeta {
    pub error_code: i16,
    pub name: String,
    pub is_internal: bool,
    pub partitions: Vec<PartitionMeta>,
}

/// Metadata v5 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct MetadataResponse {
    pub throttle_time_ms: i32,
    pub brokers: Vec<BrokerMeta>,
    pub cluster_id: Option<String>,
    pub controller_id: i32,
    pub topics: Vec<TopicMeta>,
}

impl MetadataResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms).len(self.brokers.len());
        for b in &self.brokers {
            e.i32(b.node_id).string(&b.host).i32(b.port).nullable_string(b.rack.as_deref());
        }
        e.nullable_string(self.cluster_id.as_deref()).i32(self.controller_id).len(self.topics.len());
        for t in &self.topics {
            e.i16(t.error_code).string(&t.name).bool(t.is_internal).len(t.partitions.len());
            for p in &t.partitions {
                e.i16(p.error_code).i32(p.partition).i32(p.leader).int32_array(&p.replicas).int32_array(&p.isr).int32_array(&p.offline);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let throttle_time_ms = d.i32()?;
        let n = d.len()?;
        let mut brokers = Vec::with_capacity(n);
        for _ in 0..n {
            brokers.push(BrokerMeta { node_id: d.i32()?, host: d.string()?, port: d.i32()?, rack: d.nullable_string()? });
        }
        let cluster_id = d.nullable_string()?;
        let controller_id = d.i32()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let error_code = d.i16()?;
            let name = d.string()?;
            let is_internal = d.bool()?;
            let pn = d.len()?;
            let mut partitions = Vec::with_capacity(pn);
            for _ in 0..pn {
                partitions.push(PartitionMeta {
                    error_code: d.i16()?,
                    partition: d.i32()?,
                    leader: d.i32()?,
                    replicas: d.int32_array()?,
                    isr: d.int32_array()?,
                    offline: d.int32_array()?,
                });
            }
            topics.push(TopicMeta { error_code, name, is_internal, partitions });
        }
        Ok(MetadataResponse { throttle_time_ms, brokers, cluster_id, controller_id, topics })
    }
}

/// Produce v3 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProduceRequest {
    pub transactional_id: Option<String>,
    pub acks: i16,
    pub timeout_ms: i32,
    /// topic -> [(partition, record batch bytes)]
    pub topics: Vec<(String, Vec<(i32, Vec<u8>)>)>,
}

impl ProduceRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.nullable_string(self.transactional_id.as_deref()).i16(self.acks).i32(self.timeout_ms).len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, records) in parts {
                e.i32(*p).bytes(records);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let transactional_id = d.nullable_string()?;
        let acks = d.i16()?;
        let timeout_ms = d.i32()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.bytes()?));
            }
            topics.push((t, parts));
        }
        Ok(ProduceRequest { transactional_id, acks, timeout_ms, topics })
    }
}

/// A partition result in a produce response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProducePartitionResponse {
    pub partition: i32,
    pub error_code: i16,
    pub base_offset: i64,
    pub log_append_time_ms: i64,
}

/// Produce v3 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProduceResponse {
    pub topics: Vec<(String, Vec<ProducePartitionResponse>)>,
    pub throttle_time_ms: i32,
}

impl ProduceResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for p in parts {
                e.i32(p.partition).i16(p.error_code).i64(p.base_offset).i64(p.log_append_time_ms);
            }
        }
        e.i32(self.throttle_time_ms);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push(ProducePartitionResponse { partition: d.i32()?, error_code: d.i16()?, base_offset: d.i64()?, log_append_time_ms: d.i64()? });
            }
            topics.push((t, parts));
        }
        Ok(ProduceResponse { topics, throttle_time_ms: d.i32()? })
    }
}

/// Fetch v4 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FetchRequest {
    pub replica_id: i32,
    pub max_wait_ms: i32,
    pub min_bytes: i32,
    pub max_bytes: i32,
    pub isolation_level: i8,
    /// topic -> [(partition, fetch offset, partition max bytes)]
    pub topics: Vec<(String, Vec<(i32, i64, i32)>)>,
}

impl FetchRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.replica_id).i32(self.max_wait_ms).i32(self.min_bytes).i32(self.max_bytes).i8(self.isolation_level).len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, off, max) in parts {
                e.i32(*p).i64(*off).i32(*max);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let (replica_id, max_wait_ms, min_bytes, max_bytes, isolation_level) = (d.i32()?, d.i32()?, d.i32()?, d.i32()?, d.i8()?);
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.i64()?, d.i32()?));
            }
            topics.push((t, parts));
        }
        Ok(FetchRequest { replica_id, max_wait_ms, min_bytes, max_bytes, isolation_level, topics })
    }
}

/// A partition in a fetch response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FetchPartitionResponse {
    pub partition: i32,
    pub error_code: i16,
    pub high_watermark: i64,
    pub last_stable_offset: i64,
    /// Raw record batches (may end with a partial batch).
    pub records: Vec<u8>,
}

/// Fetch v4 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FetchResponse {
    pub throttle_time_ms: i32,
    pub topics: Vec<(String, Vec<FetchPartitionResponse>)>,
}

impl FetchResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms).len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for p in parts {
                e.i32(p.partition).i16(p.error_code).i64(p.high_watermark).i64(p.last_stable_offset).i32(-1).bytes(&p.records);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let throttle_time_ms = d.i32()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                let partition = d.i32()?;
                let error_code = d.i16()?;
                let high_watermark = d.i64()?;
                let last_stable_offset = d.i64()?;
                for _ in 0..d.len()? {
                    d.i64()?;
                    d.i64()?;
                }
                let records = d.bytes()?;
                parts.push(FetchPartitionResponse { partition, error_code, high_watermark, last_stable_offset, records });
            }
            topics.push((t, parts));
        }
        Ok(FetchResponse { throttle_time_ms, topics })
    }
}

/// Timestamp for [`ListOffsetsRequest`]: latest offset.
pub const LATEST_TIMESTAMP: i64 = -1;
/// Timestamp for [`ListOffsetsRequest`]: earliest offset.
pub const EARLIEST_TIMESTAMP: i64 = -2;

/// ListOffsets v1 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ListOffsetsRequest {
    pub replica_id: i32,
    /// topic -> [(partition, timestamp)]
    pub topics: Vec<(String, Vec<(i32, i64)>)>,
}

impl ListOffsetsRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.replica_id).len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, ts) in parts {
                e.i32(*p).i64(*ts);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let replica_id = d.i32()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.i64()?));
            }
            topics.push((t, parts));
        }
        Ok(ListOffsetsRequest { replica_id, topics })
    }
}

/// ListOffsets v1 response: topic -> [(partition, error, timestamp, offset)].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ListOffsetsResponse {
    pub topics: Vec<(String, Vec<(i32, i16, i64, i64)>)>,
}

impl ListOffsetsResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, err, ts, off) in parts {
                e.i32(*p).i16(*err).i64(*ts).i64(*off);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.i16()?, d.i64()?, d.i64()?));
            }
            topics.push((t, parts));
        }
        Ok(ListOffsetsResponse { topics })
    }
}

/// FindCoordinator v1 request (key type 0 = group).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FindCoordinatorRequest {
    pub key: String,
    pub key_type: i8,
}

impl FindCoordinatorRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.key).i8(self.key_type);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(FindCoordinatorRequest { key: d.string()?, key_type: if d.remaining() > 0 { d.i8()? } else { 0 } })
    }
}

/// FindCoordinator v1 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FindCoordinatorResponse {
    pub throttle_time_ms: i32,
    pub error_code: i16,
    pub error_message: Option<String>,
    pub node_id: i32,
    pub host: String,
    pub port: i32,
}

impl FindCoordinatorResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms).i16(self.error_code).nullable_string(self.error_message.as_deref()).i32(self.node_id).string(&self.host).i32(self.port);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(FindCoordinatorResponse {
            throttle_time_ms: d.i32()?,
            error_code: d.i16()?,
            error_message: d.nullable_string()?,
            node_id: d.i32()?,
            host: d.string()?,
            port: d.i32()?,
        })
    }
}

/// JoinGroup v4 request (the v2 - v4 layouts are equal; v4 brokers answer a first join
/// without member id with MEMBER_ID_REQUIRED and the id to rejoin with, KIP-394).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct JoinGroupRequest {
    pub group_id: String,
    pub session_timeout_ms: i32,
    pub rebalance_timeout_ms: i32,
    pub member_id: String,
    pub protocol_type: String,
    /// (protocol name, metadata)
    pub protocols: Vec<(String, Vec<u8>)>,
}

impl JoinGroupRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.group_id).i32(self.session_timeout_ms).i32(self.rebalance_timeout_ms).string(&self.member_id).string(&self.protocol_type).len(self.protocols.len());
        for (n, m) in &self.protocols {
            e.string(n).bytes(m);
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let group_id = d.string()?;
        let session_timeout_ms = d.i32()?;
        let rebalance_timeout_ms = d.i32()?;
        let member_id = d.string()?;
        let protocol_type = d.string()?;
        let n = d.len()?;
        let mut protocols = Vec::with_capacity(n);
        for _ in 0..n {
            protocols.push((d.string()?, d.bytes()?));
        }
        Ok(JoinGroupRequest { group_id, session_timeout_ms, rebalance_timeout_ms, member_id, protocol_type, protocols })
    }
}

/// JoinGroup v2 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct JoinGroupResponse {
    pub throttle_time_ms: i32,
    pub error_code: i16,
    pub generation_id: i32,
    pub protocol_name: String,
    pub leader: String,
    pub member_id: String,
    /// (member id, metadata) - only for the leader.
    pub members: Vec<(String, Vec<u8>)>,
}

impl JoinGroupResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms)
            .i16(self.error_code)
            .i32(self.generation_id)
            .string(&self.protocol_name)
            .string(&self.leader)
            .string(&self.member_id)
            .len(self.members.len());
        for (m, meta) in &self.members {
            e.string(m).bytes(meta);
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let throttle_time_ms = d.i32()?;
        let error_code = d.i16()?;
        let generation_id = d.i32()?;
        let protocol_name = d.string()?;
        let leader = d.string()?;
        let member_id = d.string()?;
        let n = d.len()?;
        let mut members = Vec::with_capacity(n);
        for _ in 0..n {
            members.push((d.string()?, d.bytes()?));
        }
        Ok(JoinGroupResponse { throttle_time_ms, error_code, generation_id, protocol_name, leader, member_id, members })
    }
}

/// SyncGroup v1 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SyncGroupRequest {
    pub group_id: String,
    pub generation_id: i32,
    pub member_id: String,
    /// (member id, assignment) - sent by the leader only.
    pub assignments: Vec<(String, Vec<u8>)>,
}

impl SyncGroupRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.group_id).i32(self.generation_id).string(&self.member_id).len(self.assignments.len());
        for (m, a) in &self.assignments {
            e.string(m).bytes(a);
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let group_id = d.string()?;
        let generation_id = d.i32()?;
        let member_id = d.string()?;
        let n = d.len()?;
        let mut assignments = Vec::with_capacity(n);
        for _ in 0..n {
            assignments.push((d.string()?, d.bytes()?));
        }
        Ok(SyncGroupRequest { group_id, generation_id, member_id, assignments })
    }
}

/// SyncGroup v1 response.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SyncGroupResponse {
    pub throttle_time_ms: i32,
    pub error_code: i16,
    pub assignment: Vec<u8>,
}

impl SyncGroupResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms).i16(self.error_code).bytes(&self.assignment);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(SyncGroupResponse { throttle_time_ms: d.i32()?, error_code: d.i16()?, assignment: d.bytes()? })
    }
}

/// Heartbeat v1 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct HeartbeatRequest {
    pub group_id: String,
    pub generation_id: i32,
    pub member_id: String,
}

impl HeartbeatRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.group_id).i32(self.generation_id).string(&self.member_id);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(HeartbeatRequest { group_id: d.string()?, generation_id: d.i32()?, member_id: d.string()? })
    }
}

/// Heartbeat v1 / LeaveGroup v1 response (throttle time + error code).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SimpleResponse {
    pub throttle_time_ms: i32,
    pub error_code: i16,
}

impl SimpleResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms).i16(self.error_code);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(SimpleResponse { throttle_time_ms: d.i32()?, error_code: d.i16()? })
    }
}

/// LeaveGroup v1 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LeaveGroupRequest {
    pub group_id: String,
    pub member_id: String,
}

impl LeaveGroupRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.group_id).string(&self.member_id);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        Ok(LeaveGroupRequest { group_id: d.string()?, member_id: d.string()? })
    }
}

/// OffsetCommit v2 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OffsetCommitRequest {
    pub group_id: String,
    pub generation_id: i32,
    pub member_id: String,
    pub retention_time_ms: i64,
    /// topic -> [(partition, offset, metadata)]
    pub topics: Vec<(String, Vec<(i32, i64, Option<String>)>)>,
}

impl OffsetCommitRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.group_id).i32(self.generation_id).string(&self.member_id).i64(self.retention_time_ms).len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, off, meta) in parts {
                e.i32(*p).i64(*off).nullable_string(meta.as_deref());
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let group_id = d.string()?;
        let generation_id = d.i32()?;
        let member_id = d.string()?;
        let retention_time_ms = d.i64()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.i64()?, d.nullable_string()?));
            }
            topics.push((t, parts));
        }
        Ok(OffsetCommitRequest { group_id, generation_id, member_id, retention_time_ms, topics })
    }
}

/// OffsetCommit v2 response: topic -> [(partition, error)].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OffsetCommitResponse {
    pub topics: Vec<(String, Vec<(i32, i16)>)>,
}

impl OffsetCommitResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, err) in parts {
                e.i32(*p).i16(*err);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.i16()?));
            }
            topics.push((t, parts));
        }
        Ok(OffsetCommitResponse { topics })
    }
}

/// OffsetFetch v1 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OffsetFetchRequest {
    pub group_id: String,
    pub topics: Vec<(String, Vec<i32>)>,
}

impl OffsetFetchRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.string(&self.group_id).len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).int32_array(parts);
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let group_id = d.string()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            topics.push((d.string()?, d.int32_array()?));
        }
        Ok(OffsetFetchRequest { group_id, topics })
    }
}

/// OffsetFetch v1 response: topic -> [(partition, offset, metadata, error)].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct OffsetFetchResponse {
    pub topics: Vec<(String, Vec<(i32, i64, Option<String>, i16)>)>,
}

impl OffsetFetchResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.len(self.topics.len());
        for (t, parts) in &self.topics {
            e.string(t).len(parts.len());
            for (p, off, meta, err) in parts {
                e.i32(*p).i64(*off).nullable_string(meta.as_deref()).i16(*err);
            }
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let t = d.string()?;
            let pn = d.len()?;
            let mut parts = Vec::with_capacity(pn);
            for _ in 0..pn {
                parts.push((d.i32()?, d.i64()?, d.nullable_string()?, d.i16()?));
            }
            topics.push((t, parts));
        }
        Ok(OffsetFetchResponse { topics })
    }
}

/// A topic to create.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct NewTopic {
    pub name: String,
    pub num_partitions: i32,
    pub replication_factor: i16,
    pub configs: Vec<(String, Option<String>)>,
}

/// CreateTopics v2 request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CreateTopicsRequest {
    pub topics: Vec<NewTopic>,
    pub timeout_ms: i32,
    pub validate_only: bool,
}

impl CreateTopicsRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.len(self.topics.len());
        for t in &self.topics {
            e.string(&t.name).i32(t.num_partitions).i16(t.replication_factor).len(0).len(t.configs.len());
            for (k, v) in &t.configs {
                e.string(k).nullable_string(v.as_deref());
            }
        }
        e.i32(self.timeout_ms).bool(self.validate_only);
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            let name = d.string()?;
            let num_partitions = d.i32()?;
            let replication_factor = d.i16()?;
            for _ in 0..d.len()? {
                d.i32()?;
                d.int32_array()?;
            }
            let cn = d.len()?;
            let mut configs = Vec::with_capacity(cn);
            for _ in 0..cn {
                configs.push((d.string()?, d.nullable_string()?));
            }
            topics.push(NewTopic { name, num_partitions, replication_factor, configs });
        }
        Ok(CreateTopicsRequest { topics, timeout_ms: d.i32()?, validate_only: d.bool()? })
    }
}

/// CreateTopics v2 response: [(topic, error, message)].
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CreateTopicsResponse {
    pub throttle_time_ms: i32,
    pub topics: Vec<(String, i16, Option<String>)>,
}

impl CreateTopicsResponse {
    pub fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new();
        e.i32(self.throttle_time_ms).len(self.topics.len());
        for (t, err, msg) in &self.topics {
            e.string(t).i16(*err).nullable_string(msg.as_deref());
        }
        e.buf
    }
    pub fn decode(data: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(data);
        let throttle_time_ms = d.i32()?;
        let n = d.len()?;
        let mut topics = Vec::with_capacity(n);
        for _ in 0..n {
            topics.push((d.string()?, d.i16()?, d.nullable_string()?));
        }
        Ok(CreateTopicsResponse { throttle_time_ms, topics })
    }
}

// ---------------------------------------------------------------------------
// consumer protocol (embedded in JoinGroup / SyncGroup)
// ---------------------------------------------------------------------------

/// `ConsumerProtocolSubscription` v0: the subscribed topics.
pub fn encode_subscription(topics: &[String]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.i16(0).len(topics.len());
    for t in topics {
        e.string(t);
    }
    e.nullable_bytes(None);
    e.buf
}

/// Decode a `ConsumerProtocolSubscription` (any version; only the topics are read).
pub fn decode_subscription(data: &[u8]) -> Result<Vec<String>> {
    let mut d = Decoder::new(data);
    let _version = d.i16()?;
    let n = d.len()?;
    (0..n).map(|_| d.string()).collect()
}

/// `ConsumerProtocolAssignment` v0: topic -> partitions.
pub fn encode_assignment(assignment: &[(String, Vec<i32>)]) -> Vec<u8> {
    let mut e = Encoder::new();
    e.i16(0).len(assignment.len());
    for (t, parts) in assignment {
        e.string(t).int32_array(parts);
    }
    e.nullable_bytes(None);
    e.buf
}

/// Decode a `ConsumerProtocolAssignment` (an empty buffer = nothing assigned).
pub fn decode_assignment(data: &[u8]) -> Result<Vec<(String, Vec<i32>)>> {
    if data.is_empty() {
        return Ok(Vec::new());
    }
    let mut d = Decoder::new(data);
    let _version = d.i16()?;
    let n = d.len()?;
    (0..n).map(|_| Ok((d.string()?, d.int32_array()?))).collect()
}

/// The range assignor: for each topic, members subscribed to it (sorted by member id)
/// get consecutive ranges of partitions; the first `partitions % members` members get
/// one extra partition. `members` = (member id, subscribed topics);
/// `partitions` = topic -> partition count.
pub fn range_assign(members: &[(String, Vec<String>)], partitions: &[(String, usize)]) -> Vec<(String, Vec<(String, Vec<i32>)>)> {
    let mut sorted: Vec<&(String, Vec<String>)> = members.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out: Vec<(String, Vec<(String, Vec<i32>)>)> = sorted.iter().map(|(m, _)| (m.clone(), Vec::new())).collect();
    let mut topics: Vec<&(String, usize)> = partitions.iter().collect();
    topics.sort_by(|a, b| a.0.cmp(&b.0));
    for (topic, count) in topics {
        let subs: Vec<usize> = sorted.iter().enumerate().filter(|(_, (_, t))| t.contains(topic)).map(|(i, _)| i).collect();
        if subs.is_empty() {
            continue;
        }
        let per = count / subs.len();
        let extra = count % subs.len();
        let mut next = 0usize;
        for (j, idx) in subs.iter().enumerate() {
            let n = per + usize::from(j < extra);
            if n > 0 {
                out[*idx].1.push((topic.clone(), (next..next + n).map(|p| p as i32).collect()));
            }
            next += n;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        for v in [0i32, 1, -1, 63, -64, 64, 300, -300, i32::MAX, i32::MIN] {
            let mut e = Encoder::new();
            e.varint(v);
            assert_eq!(Decoder::new(&e.buf).varint().unwrap(), v);
        }
        for v in [0i64, -1, 1 << 40, i64::MIN, i64::MAX] {
            let mut e = Encoder::new();
            e.varlong(v);
            assert_eq!(Decoder::new(&e.buf).varlong().unwrap(), v);
        }
        let mut e = Encoder::new();
        e.varint(-1);
        assert_eq!(e.buf, vec![1]);
        let mut e = Encoder::new();
        e.varint(150);
        assert_eq!(e.buf, vec![0xac, 0x02]);
    }

    #[test]
    fn checksums() {
        assert_eq!(crc32c(b"123456789"), 0xE306_9283);
        assert_eq!(murmur2(b"21"), -973_932_308);
        assert_eq!(murmur2(b"foobar"), -790_332_482);
        assert_eq!(murmur2(b"a-little-bit-long-string"), -985_981_536);
        assert_eq!(murmur2(b"a-little-bit-longer-string"), -1_486_304_829);
        assert_eq!(murmur2(b"lkjh234lh9fiuh90y23oiuhsafujhadof229phr9h19h89h8"), -58_897_971);
        assert_eq!(murmur2(b"abc"), 479_470_107);
    }

    #[test]
    fn record_batch_round_trip() {
        let recs = vec![
            Record { offset: 0, timestamp: 1000, key: Some(b"k1".to_vec()), value: Some(b"hello".to_vec()), headers: vec![("h".into(), Some(b"v".to_vec()))] },
            Record { offset: 0, timestamp: 1005, key: None, value: None, headers: vec![("n".into(), None)] },
        ];
        let mut data = encode_record_batch(42, &recs);
        let decoded = decode_record_batches(&data).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].offset, 42);
        assert_eq!(decoded[1].offset, 43);
        assert_eq!(decoded[1].timestamp, 1005);
        assert_eq!(decoded[0].key.as_deref(), Some(&b"k1"[..]));
        assert_eq!(decoded[0].headers, recs[0].headers);
        assert_eq!(decoded[1].value, None);
        // a truncated trailing batch is ignored
        let mut two = data.clone();
        two.extend_from_slice(&data[..20]);
        assert_eq!(decode_record_batches(&two).unwrap().len(), 2);
        // corruption is detected
        let n = data.len();
        data[n - 2] ^= 0xff;
        assert!(decode_record_batches(&data).is_err());
    }

    #[test]
    fn range_assignor() {
        let members = vec![("b".to_string(), vec!["t".to_string()]), ("a".to_string(), vec!["t".to_string(), "u".to_string()])];
        let a = range_assign(&members, &[("t".to_string(), 5), ("u".to_string(), 2)]);
        assert_eq!(a[0], ("a".to_string(), vec![("t".to_string(), vec![0, 1, 2]), ("u".to_string(), vec![0, 1])]));
        assert_eq!(a[1], ("b".to_string(), vec![("t".to_string(), vec![3, 4])]));
        let enc = encode_assignment(&a[0].1);
        assert_eq!(decode_assignment(&enc).unwrap(), a[0].1);
        assert_eq!(decode_subscription(&encode_subscription(&["x".to_string()])).unwrap(), vec!["x".to_string()]);
    }

    #[test]
    fn api_bodies_round_trip() {
        let m = MetadataResponse {
            throttle_time_ms: 0,
            brokers: vec![BrokerMeta { node_id: 1, host: "h".into(), port: 9092, rack: None }],
            cluster_id: Some("c".into()),
            controller_id: 1,
            topics: vec![TopicMeta { error_code: 0, name: "t".into(), is_internal: false, partitions: vec![PartitionMeta { partition: 0, leader: 1, replicas: vec![1], isr: vec![1], ..Default::default() }] }],
        };
        assert_eq!(MetadataResponse::decode(&m.encode()).unwrap(), m);
        let f = FetchRequest { replica_id: -1, max_wait_ms: 10, min_bytes: 1, max_bytes: 100, isolation_level: 0, topics: vec![("t".into(), vec![(0, 5, 100)])] };
        assert_eq!(FetchRequest::decode(&f.encode()).unwrap(), f);
        let j = JoinGroupRequest { group_id: "g".into(), session_timeout_ms: 1, rebalance_timeout_ms: 2, member_id: "".into(), protocol_type: "consumer".into(), protocols: vec![("range".into(), vec![1, 2])] };
        assert_eq!(JoinGroupRequest::decode(&j.encode()).unwrap(), j);
        let c = CreateTopicsRequest { topics: vec![NewTopic { name: "t".into(), num_partitions: 3, replication_factor: 1, configs: vec![("a".into(), None)] }], timeout_ms: 5, validate_only: false };
        assert_eq!(CreateTopicsRequest::decode(&c.encode()).unwrap(), c);
        let frame = encode_request(&RequestHeader { api_key: 3, api_version: 5, correlation_id: 7, client_id: Some("me".into()) }, &[9]);
        let (h, body) = decode_request_header(&frame[4..]).unwrap();
        assert_eq!((h.api_key, h.api_version, h.correlation_id, h.client_id.as_deref(), body), (3, 5, 7, Some("me"), &[9u8][..]));
    }
}
