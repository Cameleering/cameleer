//! gRPC over HTTP/2: status codes, length-prefixed message framing, `grpc-timeout`,
//! metadata, and a byte-level client ([`GrpcClient`]) and server ([`GrpcServer`]).
//!
//! Messages are opaque bytes at this level; the Camel component encodes them with the
//! dynamic protobuf codec.

pub mod client;
pub mod server;

use std::fmt;
use std::time::Duration;

use camel_core::dataformat::compress;
use camel_core::error::{kinds, CamelError};

pub use client::{CallSender, ClientCall, GrpcClient};
pub use server::{GrpcServer, GrpcService, ServerCall};

/// Metadata (lower-case names). Binary `-bin` values are kept base64-encoded.
pub type Metadata = Vec<(String, String)>;

/// gRPC status codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Code {
    Ok = 0,
    Cancelled = 1,
    Unknown = 2,
    InvalidArgument = 3,
    DeadlineExceeded = 4,
    NotFound = 5,
    AlreadyExists = 6,
    PermissionDenied = 7,
    ResourceExhausted = 8,
    FailedPrecondition = 9,
    Aborted = 10,
    OutOfRange = 11,
    Unimplemented = 12,
    Internal = 13,
    Unavailable = 14,
    DataLoss = 15,
    Unauthenticated = 16,
}

impl Code {
    pub const ALL: [Code; 17] = [
        Code::Ok,
        Code::Cancelled,
        Code::Unknown,
        Code::InvalidArgument,
        Code::DeadlineExceeded,
        Code::NotFound,
        Code::AlreadyExists,
        Code::PermissionDenied,
        Code::ResourceExhausted,
        Code::FailedPrecondition,
        Code::Aborted,
        Code::OutOfRange,
        Code::Unimplemented,
        Code::Internal,
        Code::Unavailable,
        Code::DataLoss,
        Code::Unauthenticated,
    ];

    pub fn from_i32(n: i32) -> Code {
        Code::ALL.get(n as usize).copied().filter(|_| n >= 0).unwrap_or(Code::Unknown)
    }

    /// The canonical upper-case name (`INVALID_ARGUMENT`).
    pub fn name(&self) -> &'static str {
        match self {
            Code::Ok => "OK",
            Code::Cancelled => "CANCELLED",
            Code::Unknown => "UNKNOWN",
            Code::InvalidArgument => "INVALID_ARGUMENT",
            Code::DeadlineExceeded => "DEADLINE_EXCEEDED",
            Code::NotFound => "NOT_FOUND",
            Code::AlreadyExists => "ALREADY_EXISTS",
            Code::PermissionDenied => "PERMISSION_DENIED",
            Code::ResourceExhausted => "RESOURCE_EXHAUSTED",
            Code::FailedPrecondition => "FAILED_PRECONDITION",
            Code::Aborted => "ABORTED",
            Code::OutOfRange => "OUT_OF_RANGE",
            Code::Unimplemented => "UNIMPLEMENTED",
            Code::Internal => "INTERNAL",
            Code::Unavailable => "UNAVAILABLE",
            Code::DataLoss => "DATA_LOSS",
            Code::Unauthenticated => "UNAUTHENTICATED",
        }
    }

    pub fn from_name(s: &str) -> Option<Code> {
        Code::ALL.iter().copied().find(|c| c.name().eq_ignore_ascii_case(s))
    }
}

/// A gRPC status (code + message).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub code: Code,
    pub message: String,
    /// Trailing metadata that came with the status.
    pub metadata: Metadata,
}

/// The exception kind used for failed calls (as in grpc-java).
pub const STATUS_RUNTIME_EXCEPTION: &str = "StatusRuntimeException";

impl Status {
    pub fn new(code: Code, message: impl Into<String>) -> Status {
        Status { code, message: message.into(), metadata: Vec::new() }
    }
    pub fn ok() -> Status {
        Status::new(Code::Ok, "")
    }
    pub fn is_ok(&self) -> bool {
        self.code == Code::Ok
    }

    /// Maps a Camel exception to a status: `IllegalArgumentException` (and subclasses) ->
    /// INVALID_ARGUMENT, timeouts -> DEADLINE_EXCEEDED, `UnsupportedOperationException` ->
    /// UNIMPLEMENTED, a `StatusRuntimeException` keeps its code, everything else -> INTERNAL.
    pub fn from_camel_error(e: &CamelError) -> Status {
        if e.kind == STATUS_RUNTIME_EXCEPTION {
            let (name, msg) = e.message.split_once(": ").unwrap_or((e.message.as_str(), ""));
            if let Some(code) = Code::from_name(name.trim()) {
                return Status::new(code, msg);
            }
        }
        let code = if e.is_a(kinds::ILLEGAL_ARGUMENT) {
            Code::InvalidArgument
        } else if e.is_a(kinds::TIMEOUT) || e.is_a(kinds::EXCHANGE_TIMED_OUT) {
            Code::DeadlineExceeded
        } else if e.is_a(kinds::UNSUPPORTED_OPERATION) {
            Code::Unimplemented
        } else {
            Code::Internal
        };
        Status::new(code, e.message.clone())
    }

    /// Converts to a `StatusRuntimeException` Camel error (`"CODE: message"`).
    pub fn to_camel_error(&self) -> CamelError {
        let msg = if self.message.is_empty() { self.code.name().to_string() } else { format!("{}: {}", self.code.name(), self.message) };
        CamelError::new(STATUS_RUNTIME_EXCEPTION, msg)
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.code.name())?;
        if !self.message.is_empty() {
            write!(f, ": {}", self.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for Status {}

impl From<crate::h2::H2Error> for Status {
    fn from(e: crate::h2::H2Error) -> Status {
        use crate::h2::{ErrorCode, H2Error};
        match e {
            H2Error::Timeout => Status::new(Code::DeadlineExceeded, "deadline exceeded"),
            H2Error::StreamReset(ErrorCode::CANCEL) => Status::new(Code::Cancelled, "stream cancelled"),
            H2Error::StreamReset(ErrorCode::REFUSED_STREAM) | H2Error::GoAway(_) => Status::new(Code::Unavailable, e.to_string()),
            H2Error::StreamReset(_) | H2Error::Protocol(..) => Status::new(Code::Internal, e.to_string()),
            H2Error::Io(_) | H2Error::Closed(_) => Status::new(Code::Unavailable, e.to_string()),
        }
    }
}

/// Maps a non-200 HTTP status to a gRPC code (gRPC "HTTP to gRPC status code mapping").
pub fn code_from_http_status(status: u16) -> Code {
    match status {
        400 => Code::Internal,
        401 => Code::Unauthenticated,
        403 => Code::PermissionDenied,
        404 => Code::Unimplemented,
        429 | 502 | 503 | 504 => Code::Unavailable,
        _ => Code::Unknown,
    }
}

/// Frames one message: compressed flag + 4-byte big-endian length + payload.
pub fn frame_message(msg: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + msg.len());
    out.push(0);
    out.extend_from_slice(&(msg.len() as u32).to_be_bytes());
    out.extend_from_slice(msg);
    out
}

/// Incremental decoder of length-prefixed messages from DATA chunks.
#[derive(Default, Debug)]
pub struct MessageDecoder {
    buf: Vec<u8>,
    /// Negotiated `grpc-encoding` of the peer (`identity` or `gzip`).
    pub encoding: Option<String>,
    /// Maximum accepted message size.
    pub max_message_size: usize,
}

impl MessageDecoder {
    pub fn new(encoding: Option<String>, max_message_size: usize) -> Self {
        MessageDecoder { buf: Vec::new(), encoding, max_message_size }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Next complete message, if any.
    pub fn next_message(&mut self) -> Result<Option<Vec<u8>>, Status> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let compressed = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if self.max_message_size > 0 && len > self.max_message_size {
            return Err(Status::new(Code::ResourceExhausted, format!("message of {} bytes exceeds the maximum of {}", len, self.max_message_size)));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let payload: Vec<u8> = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        match compressed {
            0 => Ok(Some(payload)),
            1 => match self.encoding.as_deref() {
                Some("gzip") => compress::gzip_decompress(&payload).map(Some).map_err(|e| Status::new(Code::Internal, format!("gzip: {}", e.message))),
                Some("deflate") => compress::zlib_decompress(&payload).map(Some).map_err(|e| Status::new(Code::Internal, format!("deflate: {}", e.message))),
                other => Err(Status::new(Code::Internal, format!("compressed message with unsupported grpc-encoding {:?}", other))),
            },
            f => Err(Status::new(Code::Internal, format!("invalid compressed flag {}", f))),
        }
    }

    /// True when no partial message is buffered.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
}

/// Formats a timeout for `grpc-timeout` (at most 8 digits, coarsest exact unit).
pub fn format_timeout(d: Duration) -> String {
    let nanos = d.as_nanos();
    let units: [(u128, char); 6] = [(1, 'n'), (1_000, 'u'), (1_000_000, 'm'), (1_000_000_000, 'S'), (60_000_000_000, 'M'), (3_600_000_000_000, 'H')];
    for (div, unit) in units {
        let v = nanos.div_ceil(div);
        if v < 100_000_000 {
            return format!("{}{}", v, unit);
        }
    }
    "99999999H".into()
}

/// Parses a `grpc-timeout` value (`100m`, `5S`, `1H`...).
pub fn parse_timeout(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.len() < 2 || s.len() > 9 {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: u64 = num.parse().ok()?;
    Some(match unit {
        "H" => Duration::from_secs(n.checked_mul(3600)?),
        "M" => Duration::from_secs(n.checked_mul(60)?),
        "S" => Duration::from_secs(n),
        "m" => Duration::from_millis(n),
        "u" => Duration::from_micros(n),
        "n" => Duration::from_nanos(n),
        _ => return None,
    })
}

/// Percent-encodes a `grpc-message` value.
pub fn encode_grpc_message(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if (0x20..=0x7e).contains(&b) && b != b'%' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

/// Decodes a percent-encoded `grpc-message`.
pub fn decode_grpc_message(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Headers that are not user metadata.
pub fn is_reserved_header(name: &str) -> bool {
    name.starts_with(':') || matches!(name, "content-type" | "te" | "grpc-timeout" | "grpc-encoding" | "grpc-accept-encoding" | "grpc-status" | "grpc-message" | "grpc-status-details-bin" | "content-length" | "connection" | "user-agent")
}

/// Extracts the status from trailers (or trailers-only headers).
pub fn status_from_trailers(trailers: &[(String, String)]) -> Option<Status> {
    let code = trailers.iter().find(|(n, _)| n == "grpc-status")?.1.trim().parse::<i32>().ok().map(Code::from_i32).unwrap_or(Code::Unknown);
    let message = trailers.iter().find(|(n, _)| n == "grpc-message").map(|(_, v)| decode_grpc_message(v)).unwrap_or_default();
    let metadata = trailers.iter().filter(|(n, _)| !is_reserved_header(n)).cloned().collect();
    Some(Status { code, message, metadata })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts() {
        assert_eq!(format_timeout(Duration::from_millis(100)), "100000u");
        assert_eq!(parse_timeout("100m"), Some(Duration::from_millis(100)));
        assert_eq!(parse_timeout("5S"), Some(Duration::from_secs(5)));
        assert_eq!(parse_timeout("2H"), Some(Duration::from_secs(7200)));
        assert_eq!(parse_timeout("1x"), None);
        assert_eq!(parse_timeout("123456789m"), None);
        for d in [Duration::from_nanos(1), Duration::from_millis(250), Duration::from_secs(30), Duration::from_secs(86_400 * 400)] {
            let p = parse_timeout(&format_timeout(d)).unwrap();
            assert!(p >= d, "{:?} -> {:?}", d, p);
        }
    }

    #[test]
    fn framing_and_messages() {
        let f = frame_message(b"abc");
        assert_eq!(f, [0, 0, 0, 0, 3, b'a', b'b', b'c']);
        let mut d = MessageDecoder::new(None, 0);
        let mut both = f.clone();
        both.extend(frame_message(b""));
        d.push(&both[..4]);
        assert_eq!(d.next_message().unwrap(), None);
        d.push(&both[4..]);
        assert_eq!(d.next_message().unwrap(), Some(b"abc".to_vec()));
        assert_eq!(d.next_message().unwrap(), Some(Vec::new()));
        assert!(d.is_empty());
        let mut gz = vec![1u8];
        let c = compress::gzip_compress(b"hello", 6);
        gz.extend_from_slice(&(c.len() as u32).to_be_bytes());
        gz.extend_from_slice(&c);
        let mut d = MessageDecoder::new(Some("gzip".into()), 0);
        d.push(&gz);
        assert_eq!(d.next_message().unwrap(), Some(b"hello".to_vec()));
        let mut d = MessageDecoder::new(None, 2);
        d.push(&frame_message(b"abc"));
        assert_eq!(d.next_message().unwrap_err().code, Code::ResourceExhausted);
    }

    #[test]
    fn status_mapping() {
        assert_eq!(encode_grpc_message("bad 100%\n"), "bad 100%25%0A");
        assert_eq!(decode_grpc_message("bad 100%25%0A"), "bad 100%\n");
        assert_eq!(decode_grpc_message("caf%C3%A9"), "café");
        assert_eq!(Status::from_camel_error(&CamelError::illegal_argument("x")).code, Code::InvalidArgument);
        assert_eq!(Status::from_camel_error(&CamelError::new(kinds::NUMBER_FORMAT, "x")).code, Code::InvalidArgument);
        assert_eq!(Status::from_camel_error(&CamelError::new(kinds::CAMEL_EXCHANGE, "boom")), Status::new(Code::Internal, "boom"));
        let s = Status::new(Code::NotFound, "no such user");
        let e = s.to_camel_error();
        assert_eq!(e.message, "NOT_FOUND: no such user");
        assert_eq!(Status::from_camel_error(&e), s);
        assert_eq!(Code::from_i32(99), Code::Unknown);
        assert_eq!(code_from_http_status(404), Code::Unimplemented);
        let st = status_from_trailers(&[("grpc-status".into(), "5".into()), ("grpc-message".into(), "gone%21".into()), ("x-a".into(), "1".into())]).unwrap();
        assert_eq!((st.code, st.message.as_str(), st.metadata.len()), (Code::NotFound, "gone!", 1));
    }
}
