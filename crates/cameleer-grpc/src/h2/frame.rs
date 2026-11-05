//! HTTP/2 frame codec (RFC 9113 section 4 and 6).

use std::fmt;
use std::io::{self, Read, Write};

/// The client connection preface.
pub const PREFACE: &[u8; 24] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// Frame types.
pub mod frame_type {
    pub const DATA: u8 = 0x0;
    pub const HEADERS: u8 = 0x1;
    pub const PRIORITY: u8 = 0x2;
    pub const RST_STREAM: u8 = 0x3;
    pub const SETTINGS: u8 = 0x4;
    pub const PUSH_PROMISE: u8 = 0x5;
    pub const PING: u8 = 0x6;
    pub const GOAWAY: u8 = 0x7;
    pub const WINDOW_UPDATE: u8 = 0x8;
    pub const CONTINUATION: u8 = 0x9;
}

/// Frame flags.
pub mod flags {
    pub const END_STREAM: u8 = 0x1;
    pub const ACK: u8 = 0x1;
    pub const END_HEADERS: u8 = 0x4;
    pub const PADDED: u8 = 0x8;
    pub const PRIORITY: u8 = 0x20;
}

/// SETTINGS identifiers.
pub mod settings_id {
    pub const HEADER_TABLE_SIZE: u16 = 0x1;
    pub const ENABLE_PUSH: u16 = 0x2;
    pub const MAX_CONCURRENT_STREAMS: u16 = 0x3;
    pub const INITIAL_WINDOW_SIZE: u16 = 0x4;
    pub const MAX_FRAME_SIZE: u16 = 0x5;
    pub const MAX_HEADER_LIST_SIZE: u16 = 0x6;
}

/// HTTP/2 error codes (RFC 9113 section 7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ErrorCode(pub u32);

impl ErrorCode {
    pub const NO_ERROR: ErrorCode = ErrorCode(0x0);
    pub const PROTOCOL_ERROR: ErrorCode = ErrorCode(0x1);
    pub const INTERNAL_ERROR: ErrorCode = ErrorCode(0x2);
    pub const FLOW_CONTROL_ERROR: ErrorCode = ErrorCode(0x3);
    pub const SETTINGS_TIMEOUT: ErrorCode = ErrorCode(0x4);
    pub const STREAM_CLOSED: ErrorCode = ErrorCode(0x5);
    pub const FRAME_SIZE_ERROR: ErrorCode = ErrorCode(0x6);
    pub const REFUSED_STREAM: ErrorCode = ErrorCode(0x7);
    pub const CANCEL: ErrorCode = ErrorCode(0x8);
    pub const COMPRESSION_ERROR: ErrorCode = ErrorCode(0x9);
    pub const CONNECT_ERROR: ErrorCode = ErrorCode(0xa);
    pub const ENHANCE_YOUR_CALM: ErrorCode = ErrorCode(0xb);
    pub const INADEQUATE_SECURITY: ErrorCode = ErrorCode(0xc);
    pub const HTTP_1_1_REQUIRED: ErrorCode = ErrorCode(0xd);

    pub fn name(&self) -> &'static str {
        match self.0 {
            0x0 => "NO_ERROR",
            0x1 => "PROTOCOL_ERROR",
            0x2 => "INTERNAL_ERROR",
            0x3 => "FLOW_CONTROL_ERROR",
            0x4 => "SETTINGS_TIMEOUT",
            0x5 => "STREAM_CLOSED",
            0x6 => "FRAME_SIZE_ERROR",
            0x7 => "REFUSED_STREAM",
            0x8 => "CANCEL",
            0x9 => "COMPRESSION_ERROR",
            0xa => "CONNECT_ERROR",
            0xb => "ENHANCE_YOUR_CALM",
            0xc => "INADEQUATE_SECURITY",
            0xd => "HTTP_1_1_REQUIRED",
            _ => "UNKNOWN",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (0x{:x})", self.name(), self.0)
    }
}

/// A raw frame: header fields plus payload.
#[derive(Clone, PartialEq, Eq)]
pub struct Frame {
    pub typ: u8,
    pub flags: u8,
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

impl fmt::Debug for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Frame{{type={}, flags=0x{:x}, stream={}, len={}}}", self.typ, self.flags, self.stream_id, self.payload.len())
    }
}

impl Frame {
    pub fn new(typ: u8, flags: u8, stream_id: u32, payload: Vec<u8>) -> Frame {
        Frame { typ, flags, stream_id, payload }
    }
    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    /// Serializes the frame (9-byte header + payload).
    pub fn encode(&self, out: &mut Vec<u8>) {
        let len = self.payload.len();
        out.extend_from_slice(&[(len >> 16) as u8, (len >> 8) as u8, len as u8, self.typ, self.flags]);
        out.extend_from_slice(&(self.stream_id & 0x7fff_ffff).to_be_bytes());
        out.extend_from_slice(&self.payload);
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(9 + self.payload.len());
        self.encode(&mut v);
        v
    }

    /// Writes the frame.
    pub fn write_to<W: Write + ?Sized>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.to_bytes())
    }

    /// Parses a frame from a byte slice; returns the frame and the bytes consumed,
    /// or `None` when more data is needed.
    pub fn decode(buf: &[u8], max_frame_size: usize) -> Result<Option<(Frame, usize)>, ErrorCode> {
        if buf.len() < 9 {
            return Ok(None);
        }
        let len = ((buf[0] as usize) << 16) | ((buf[1] as usize) << 8) | buf[2] as usize;
        if len > max_frame_size {
            return Err(ErrorCode::FRAME_SIZE_ERROR);
        }
        if buf.len() < 9 + len {
            return Ok(None);
        }
        let stream_id = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff;
        Ok(Some((Frame { typ: buf[3], flags: buf[4], stream_id, payload: buf[9..9 + len].to_vec() }, 9 + len)))
    }

    /// Reads one frame. `Ok(None)` on a clean EOF before the header.
    pub fn read_from<R: Read + ?Sized>(r: &mut R, max_frame_size: usize) -> Result<Option<Frame>, ReadError> {
        let mut hdr = [0u8; 9];
        let mut got = 0;
        while got < 9 {
            match r.read(&mut hdr[got..]) {
                Ok(0) if got == 0 => return Ok(None),
                Ok(0) => return Err(ReadError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF inside frame header"))),
                Ok(n) => got += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ReadError::Io(e)),
            }
        }
        let len = ((hdr[0] as usize) << 16) | ((hdr[1] as usize) << 8) | hdr[2] as usize;
        if len > max_frame_size {
            return Err(ReadError::Protocol(ErrorCode::FRAME_SIZE_ERROR, format!("frame of {} bytes exceeds SETTINGS_MAX_FRAME_SIZE {}", len, max_frame_size)));
        }
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).map_err(ReadError::Io)?;
        let stream_id = u32::from_be_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]) & 0x7fff_ffff;
        Ok(Some(Frame { typ: hdr[3], flags: hdr[4], stream_id, payload }))
    }

    // ----------------------------------------------------------- constructors

    pub fn data(stream_id: u32, data: &[u8], end_stream: bool) -> Frame {
        Frame::new(frame_type::DATA, if end_stream { flags::END_STREAM } else { 0 }, stream_id, data.to_vec())
    }

    pub fn settings(values: &[(u16, u32)]) -> Frame {
        let mut p = Vec::with_capacity(values.len() * 6);
        for (id, v) in values {
            p.extend_from_slice(&id.to_be_bytes());
            p.extend_from_slice(&v.to_be_bytes());
        }
        Frame::new(frame_type::SETTINGS, 0, 0, p)
    }

    pub fn settings_ack() -> Frame {
        Frame::new(frame_type::SETTINGS, flags::ACK, 0, Vec::new())
    }

    pub fn ping(data: [u8; 8], ack: bool) -> Frame {
        Frame::new(frame_type::PING, if ack { flags::ACK } else { 0 }, 0, data.to_vec())
    }

    pub fn goaway(last_stream_id: u32, code: ErrorCode, debug: &[u8]) -> Frame {
        let mut p = Vec::with_capacity(8 + debug.len());
        p.extend_from_slice(&(last_stream_id & 0x7fff_ffff).to_be_bytes());
        p.extend_from_slice(&code.0.to_be_bytes());
        p.extend_from_slice(debug);
        Frame::new(frame_type::GOAWAY, 0, 0, p)
    }

    pub fn rst_stream(stream_id: u32, code: ErrorCode) -> Frame {
        Frame::new(frame_type::RST_STREAM, 0, stream_id, code.0.to_be_bytes().to_vec())
    }

    pub fn window_update(stream_id: u32, increment: u32) -> Frame {
        Frame::new(frame_type::WINDOW_UPDATE, 0, stream_id, (increment & 0x7fff_ffff).to_be_bytes().to_vec())
    }

    /// Splits a header block into HEADERS + CONTINUATION frames of at most `max_frame_size`.
    pub fn headers(stream_id: u32, block: &[u8], end_stream: bool, max_frame_size: usize) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut chunks = block.chunks(max_frame_size.max(1)).peekable();
        let first = chunks.next().unwrap_or(&[]);
        let mut fl = if end_stream { flags::END_STREAM } else { 0 };
        if chunks.peek().is_none() {
            fl |= flags::END_HEADERS;
        }
        frames.push(Frame::new(frame_type::HEADERS, fl, stream_id, first.to_vec()));
        while let Some(c) = chunks.next() {
            let fl = if chunks.peek().is_none() { flags::END_HEADERS } else { 0 };
            frames.push(Frame::new(frame_type::CONTINUATION, fl, stream_id, c.to_vec()));
        }
        frames
    }

    // ----------------------------------------------------------- payload parsing

    /// Parses SETTINGS payload into (id, value) pairs.
    pub fn parse_settings(&self) -> Result<Vec<(u16, u32)>, ErrorCode> {
        if self.payload.len() % 6 != 0 {
            return Err(ErrorCode::FRAME_SIZE_ERROR);
        }
        Ok(self
            .payload
            .chunks(6)
            .map(|c| (u16::from_be_bytes([c[0], c[1]]), u32::from_be_bytes([c[2], c[3], c[4], c[5]])))
            .collect())
    }

    /// Strips padding (PADDED flag) from DATA/HEADERS payloads; returns (data, padding octets incl. length byte).
    pub fn unpadded(&self) -> Result<(&[u8], usize), ErrorCode> {
        if !self.has(flags::PADDED) {
            return Ok((&self.payload, 0));
        }
        let pad = *self.payload.first().ok_or(ErrorCode::FRAME_SIZE_ERROR)? as usize;
        if pad + 1 > self.payload.len() {
            return Err(ErrorCode::PROTOCOL_ERROR);
        }
        Ok((&self.payload[1..self.payload.len() - pad], pad + 1))
    }

    /// The header block fragment of a HEADERS frame (padding and priority removed).
    pub fn header_fragment(&self) -> Result<&[u8], ErrorCode> {
        let (data, _) = self.unpadded()?;
        if self.typ == frame_type::HEADERS && self.has(flags::PRIORITY) {
            if data.len() < 5 {
                return Err(ErrorCode::FRAME_SIZE_ERROR);
            }
            return Ok(&data[5..]);
        }
        Ok(data)
    }

    pub fn u32_at(&self, off: usize) -> Option<u32> {
        let b = self.payload.get(off..off + 4)?;
        Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Error while reading a frame.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    Protocol(ErrorCode, String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Io(e) => write!(f, "{}", e),
            ReadError::Protocol(c, m) => write!(f, "{}: {}", c, m),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip() {
        let f = Frame::data(3, b"hello", true);
        let bytes = f.to_bytes();
        assert_eq!(bytes, [0, 0, 5, 0, 1, 0, 0, 0, 3, b'h', b'e', b'l', b'l', b'o']);
        let (g, n) = Frame::decode(&bytes, 16384).unwrap().unwrap();
        assert_eq!(n, bytes.len());
        assert_eq!(g, f);
        assert!(Frame::decode(&bytes[..10], 16384).unwrap().is_none());
        assert_eq!(Frame::decode(&bytes, 4), Err(ErrorCode::FRAME_SIZE_ERROR));
        let mut r: &[u8] = &bytes;
        assert_eq!(Frame::read_from(&mut r, 16384).unwrap().unwrap(), f);
        assert!(Frame::read_from(&mut r, 16384).unwrap().is_none());
    }

    #[test]
    fn settings_ping_goaway_window() {
        let s = Frame::settings(&[(settings_id::INITIAL_WINDOW_SIZE, 1 << 20), (settings_id::ENABLE_PUSH, 0)]);
        assert_eq!(s.to_bytes()[..9], [0, 0, 12, 4, 0, 0, 0, 0, 0]);
        assert_eq!(s.parse_settings().unwrap(), vec![(4, 1 << 20), (2, 0)]);
        assert_eq!(Frame::settings_ack().to_bytes(), [0, 0, 0, 4, 1, 0, 0, 0, 0]);
        let p = Frame::ping(*b"12345678", true);
        assert_eq!(p.payload, b"12345678");
        assert!(p.has(flags::ACK));
        let g = Frame::goaway(7, ErrorCode::PROTOCOL_ERROR, b"bad");
        assert_eq!(g.u32_at(0), Some(7));
        assert_eq!(g.u32_at(4), Some(1));
        assert_eq!(&g.payload[8..], b"bad");
        let w = Frame::window_update(5, 1000);
        assert_eq!(w.to_bytes(), [0, 0, 4, 8, 0, 0, 0, 0, 5, 0, 0, 0x03, 0xe8]);
        let r = Frame::rst_stream(1, ErrorCode::CANCEL);
        assert_eq!(r.u32_at(0), Some(8));
        assert_eq!(ErrorCode::CANCEL.to_string(), "CANCEL (0x8)");
        // bad settings length
        assert!(Frame::new(frame_type::SETTINGS, 0, 0, vec![0; 5]).parse_settings().is_err());
    }

    #[test]
    fn headers_continuation_split_and_padding() {
        let block: Vec<u8> = (0..25u8).collect();
        let frames = Frame::headers(1, &block, true, 10);
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].typ, frame_type::HEADERS);
        assert_eq!(frames[0].flags, flags::END_STREAM);
        assert_eq!(frames[1].typ, frame_type::CONTINUATION);
        assert_eq!(frames[1].flags, 0);
        assert_eq!(frames[2].flags, flags::END_HEADERS);
        let joined: Vec<u8> = frames.iter().flat_map(|f| f.payload.clone()).collect();
        assert_eq!(joined, block);
        let single = Frame::headers(1, &[1, 2], false, 10);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].flags, flags::END_HEADERS);

        // padded + priority HEADERS
        let mut payload = vec![2u8]; // pad length
        payload.extend_from_slice(&[0, 0, 0, 0, 16]); // priority
        payload.extend_from_slice(b"abc");
        payload.extend_from_slice(&[0, 0]);
        let f = Frame::new(frame_type::HEADERS, flags::PADDED | flags::PRIORITY | flags::END_HEADERS, 1, payload);
        assert_eq!(f.header_fragment().unwrap(), b"abc");
        let d = Frame::new(frame_type::DATA, flags::PADDED, 1, vec![3, b'x', 0, 0, 0]);
        assert_eq!(d.unpadded().unwrap(), (&b"x"[..], 4));
        let bad = Frame::new(frame_type::DATA, flags::PADDED, 1, vec![9, b'x']);
        assert_eq!(bad.unpadded(), Err(ErrorCode::PROTOCOL_ERROR));
    }
}
