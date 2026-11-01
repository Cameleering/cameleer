//! HTTP/1.1 `Upgrade: h2c` (RFC 7540 section 3.2) for cleartext connections.

use std::io::{Read, Write};
use std::net::TcpStream;

use super::conn::{H2Config, H2Error};
use super::frame::{settings_id, ErrorCode};
use crate::hpack::HeaderList;

/// A received upgrade request: the HTTP/2 request headers for stream 1, its body and the
/// client's `HTTP2-Settings`.
pub(crate) struct UpgradeRequest {
    pub headers: HeaderList,
    pub body: Vec<u8>,
    pub settings: Vec<(u16, u32)>,
}

fn b64url_encode(b: &[u8]) -> String {
    camel_tls::pem::base64_encode(b).trim_end_matches('=').replace('+', "-").replace('/', "_")
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut t = s.trim().replace('-', "+").replace('_', "/");
    while t.len() % 4 != 0 {
        t.push('=');
    }
    camel_tls::pem::base64_decode(&t)
}

/// Our SETTINGS payload (as sent in the preface) for the `HTTP2-Settings` header.
pub(crate) fn settings_payload(c: &H2Config) -> Vec<u8> {
    let mut out = Vec::new();
    let mut put = |id: u16, v: u32| {
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&v.to_be_bytes());
    };
    put(settings_id::HEADER_TABLE_SIZE, c.header_table_size);
    put(settings_id::ENABLE_PUSH, 0);
    put(settings_id::INITIAL_WINDOW_SIZE, c.initial_window_size.min(0x7fff_ffff));
    put(settings_id::MAX_FRAME_SIZE, c.max_frame_size.clamp(16_384, (1 << 24) - 1));
    out
}

fn parse_settings(b: &[u8]) -> Vec<(u16, u32)> {
    b.chunks_exact(6).map(|c| (u16::from_be_bytes([c[0], c[1]]), u32::from_be_bytes([c[2], c[3], c[4], c[5]]))).collect()
}

fn read_head(prefix: &[u8], r: &mut dyn Read) -> std::io::Result<Vec<u8>> {
    let mut head = prefix.to_vec();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 64 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "HTTP/1.1 head too large"));
        }
        if r.read(&mut b)? == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed in the HTTP/1.1 head"));
        }
        head.push(b[0]);
    }
    Ok(head)
}

/// Parses an HTTP/1.1 request whose first bytes (`prefix`) were already read. `Ok(None)`:
/// a valid HTTP/1.1 request without an h2c upgrade; `Err`: not HTTP/1.1 at all.
pub(crate) fn read_upgrade_request(prefix: &[u8], r: &mut Box<dyn Read + Send>) -> std::io::Result<Option<UpgradeRequest>> {
    let starts_like_http = prefix.iter().take_while(|b| b.is_ascii_uppercase()).count() >= 3 && prefix.contains(&b' ');
    if !starts_like_http {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP/1.1 request"));
    }
    let head = read_head(prefix, r.as_mut())?;
    let text = String::from_utf8_lossy(&head[..head.len() - 4]).into_owned();
    let mut lines = text.split("\r\n");
    let start = lines.next().unwrap_or("");
    let mut parts = start.split(' ');
    let (method, target, version) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"), parts.next().unwrap_or(""));
    if !version.starts_with("HTTP/1.") {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP/1.1 request"));
    }
    let mut fields: Vec<(String, String)> = Vec::new();
    for l in lines {
        if let Some((k, v)) = l.split_once(':') {
            fields.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let get = |n: &str| fields.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str());
    let has_token = |n: &str, t: &str| get(n).is_some_and(|v| v.split(',').any(|x| x.trim().eq_ignore_ascii_case(t)));
    let settings = match (has_token("upgrade", "h2c"), get("http2-settings")) {
        (true, Some(s)) => b64url_decode(s).map(|b| parse_settings(&b)).unwrap_or_default(),
        _ => return Ok(None),
    };
    let len: usize = get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if len > 16 * 1024 * 1024 {
        return Ok(None);
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let mut headers: HeaderList = vec![
        (":method".to_string(), method.to_string()),
        (":scheme".to_string(), "http".to_string()),
        (":path".to_string(), target.to_string()),
        (":authority".to_string(), get("host").unwrap_or("").to_string()),
    ];
    for (k, v) in &fields {
        if !matches!(k.as_str(), "connection" | "upgrade" | "http2-settings" | "host" | "keep-alive" | "proxy-connection" | "transfer-encoding" | "content-length") {
            headers.push((k.clone(), v.clone()));
        }
    }
    Ok(Some(UpgradeRequest { headers, body, settings }))
}

/// Client side: send the upgrade request and wait for `101 Switching Protocols`.
pub(crate) fn client_upgrade(tcp: &mut TcpStream, authority: &str, config: &H2Config) -> Result<(), H2Error> {
    let req = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: Upgrade, HTTP2-Settings\r\nUpgrade: h2c\r\nHTTP2-Settings: {}\r\nUser-Agent: rust-camel-h2c\r\n\r\n",
        authority,
        b64url_encode(&settings_payload(config))
    );
    tcp.write_all(req.as_bytes()).map_err(|e| H2Error::Io(format!("sending the h2c upgrade request: {}", e)))?;
    let head = read_head(&[], tcp).map_err(|e| H2Error::Io(format!("reading the h2c upgrade response: {}", e)))?;
    let text = String::from_utf8_lossy(&head);
    let status = text.split(' ').nth(1).unwrap_or("");
    if status != "101" {
        return Err(H2Error::Protocol(ErrorCode::HTTP_1_1_REQUIRED, format!("server refused the h2c upgrade: {}", text.lines().next().unwrap_or(""))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_request_parsing() {
        let c = H2Config::default();
        let enc = b64url_encode(&settings_payload(&c));
        assert!(!enc.contains('=') && !enc.contains('+') && !enc.contains('/'));
        let s = parse_settings(&b64url_decode(&enc).unwrap());
        assert!(s.contains(&(settings_id::INITIAL_WINDOW_SIZE, c.initial_window_size)));
        let rest = format!("HTTP/1.1\r\nHost: h:1\r\nConnection: Upgrade, HTTP2-Settings\r\nUpgrade: h2c\r\nHTTP2-Settings: {}\r\nContent-Length: 2\r\nX-A: b\r\n\r\nhi", enc);
        let mut r: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(rest.into_bytes()));
        let req = read_upgrade_request(b"POST /x ", &mut r).unwrap().unwrap();
        assert_eq!(req.body, b"hi");
        assert!(req.headers.contains(&(":path".to_string(), "/x".to_string())));
        assert!(req.headers.contains(&(":authority".to_string(), "h:1".to_string())));
        assert!(req.headers.contains(&("x-a".to_string(), "b".to_string())));
        assert!(!req.headers.iter().any(|(k, _)| k == "upgrade" || k == "connection"));
        let mut r: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(b"HTTP/1.1\r\nHost: h\r\n\r\n".to_vec()));
        assert!(read_upgrade_request(b"GET / ", &mut r).unwrap().is_none());
        let mut r: Box<dyn Read + Send> = Box::new(std::io::Cursor::new(Vec::new()));
        assert!(read_upgrade_request(&[0u8; 24], &mut r).is_err());
    }
}
