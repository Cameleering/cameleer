//! A Lumberjack client for tests: builds frames the way Beats (`go-lumber`) and
//! logstash-forwarder write them, sends windows and reads the ACKs.

use std::io::{Read, Write};
use std::time::Duration;

use cameleer_core::dataformat::compress::zlib_compress;
use cameleer_tls::net::NetStream;
use cameleer_tls::TlsClientConfig;

use crate::lumberjack::{TYPE_ACKNOWLEDGE, TYPE_COMPRESS, TYPE_DATA, TYPE_JSON, TYPE_WINDOW};

/// `version 'W' size`.
pub fn window_frame(version: u8, size: u32) -> Vec<u8> {
    let mut b = vec![version, TYPE_WINDOW];
    b.extend(size.to_be_bytes());
    b
}

/// `version 'J' seq length json`.
pub fn json_frame(version: u8, seq: u32, json: &str) -> Vec<u8> {
    let mut b = vec![version, TYPE_JSON];
    b.extend(seq.to_be_bytes());
    b.extend((json.len() as u32).to_be_bytes());
    b.extend(json.as_bytes());
    b
}

/// `version 'D' seq pairs (keyLength key valueLength value)*`.
pub fn data_frame(version: u8, seq: u32, pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut b = vec![version, TYPE_DATA];
    b.extend(seq.to_be_bytes());
    b.extend((pairs.len() as u32).to_be_bytes());
    for (k, v) in pairs {
        b.extend((k.len() as u32).to_be_bytes());
        b.extend(k.as_bytes());
        b.extend((v.len() as u32).to_be_bytes());
        b.extend(v.as_bytes());
    }
    b
}

/// `version 'C' length zlib(frames)`.
pub fn compressed_frame(version: u8, frames: &[u8]) -> Vec<u8> {
    let packed = zlib_compress(frames, 6);
    let mut b = vec![version, TYPE_COMPRESS];
    b.extend((packed.len() as u32).to_be_bytes());
    b.extend(packed);
    b
}

/// A connected client.
pub struct LumberjackClient {
    stream: NetStream,
}

impl LumberjackClient {
    /// Connects over plain TCP.
    pub fn connect(host: &str, port: u16) -> std::io::Result<LumberjackClient> {
        LumberjackClient::open(host, port, None)
    }

    /// Connects over TLS (the server name is `host`).
    pub fn connect_tls(host: &str, port: u16, tls: &TlsClientConfig) -> std::io::Result<LumberjackClient> {
        LumberjackClient::open(host, port, Some(tls))
    }

    fn open(host: &str, port: u16, tls: Option<&TlsClientConfig>) -> std::io::Result<LumberjackClient> {
        let stream = NetStream::connect(host, port, tls, Duration::from_secs(10))?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        Ok(LumberjackClient { stream })
    }

    /// Writes raw bytes.
    pub fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(bytes)?;
        self.stream.flush()
    }

    /// Sends a v2 window of JSON events, compressed like Filebeat when `compress` is set, and
    /// returns the ACK `(version, sequence)`.
    pub fn send_json(&mut self, events: &[&str], compress: bool) -> std::io::Result<(u8, u32)> {
        let mut frames = Vec::new();
        for (i, e) in events.iter().enumerate() {
            frames.extend(json_frame(b'2', i as u32 + 1, e));
        }
        let mut b = window_frame(b'2', events.len() as u32);
        b.extend(if compress { compressed_frame(b'2', &frames) } else { frames });
        self.write(&b)?;
        self.read_ack()
    }

    /// Reads one ACK frame `(version, sequence)`.
    pub fn read_ack(&mut self) -> std::io::Result<(u8, u32)> {
        let mut b = [0u8; 6];
        self.stream.read_exact(&mut b)?;
        if b[1] != TYPE_ACKNOWLEDGE {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("not an ACK frame: {:?}", b)));
        }
        Ok((b[0], u32::from_be_bytes([b[2], b[3], b[4], b[5]])))
    }
}
