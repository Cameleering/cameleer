//! Byte streams under the AMQP frame layer: a plain / TLS socket ([`NetStream`]) or AMQP
//! over WebSockets (the OASIS AMQP WebSocket binding as used by Azure Service Bus and
//! Event Hubs: an RFC 6455 connection to `/$servicebus/websocket` with the `AMQPWSB10`
//! subprotocol, AMQP bytes carried in binary messages).
//!
//! [`AmqpStream`] is cloneable like `NetStream` (all clones share one connection). For
//! WebSockets every `write` becomes one binary frame (masked on the client side); reads
//! reassemble frames, answer pings and treat a close frame as end of stream.

use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_tls::net::NetStream;

/// The WebSocket subprotocol of the AMQP WebSocket binding (Azure).
pub const AMQP_WS_PROTOCOL: &str = "AMQPWSB10";
/// The path Azure Service Bus / Event Hubs serve AMQP over WebSockets on.
pub const AMQP_WS_PATH: &str = "/$servicebus/websocket";
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

struct WsInner {
    client: bool,
    read: Mutex<WsRead>,
    write: Mutex<()>,
    close_sent: AtomicBool,
}

#[derive(Default)]
struct WsRead {
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

/// A socket carrying AMQP: plain/TLS, optionally with the WebSocket framing on top.
pub struct AmqpStream {
    net: NetStream,
    ws: Option<Arc<WsInner>>,
}

impl std::fmt::Debug for AmqpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AmqpStream({:?}, websocket={})", self.net, self.ws.is_some())
    }
}

impl From<NetStream> for AmqpStream {
    fn from(net: NetStream) -> Self {
        AmqpStream { net, ws: None }
    }
}

impl AmqpStream {
    /// Whether AMQP is carried over WebSockets.
    pub fn is_websocket(&self) -> bool {
        self.ws.is_some()
    }
    /// The underlying socket.
    pub fn net(&self) -> &NetStream {
        &self.net
    }
    pub fn try_clone(&self) -> io::Result<AmqpStream> {
        Ok(AmqpStream { net: self.net.try_clone()?, ws: self.ws.clone() })
    }
    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.net.set_read_timeout(d)
    }
    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.net.set_write_timeout(d)
    }
    pub fn set_nodelay(&self, v: bool) -> io::Result<()> {
        self.net.set_nodelay(v)
    }
    /// Close (a WebSocket close frame is sent first).
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        if let Some(ws) = &self.ws {
            if !ws.close_sent.swap(true, Ordering::SeqCst) {
                let _ = self.write_ws_frame(ws, 0x8, &1000u16.to_be_bytes());
            }
        }
        self.net.shutdown(how)
    }

    /// Client side: upgrade `net` to a WebSocket (`GET path`, `Sec-WebSocket-Protocol`).
    pub fn websocket_client(mut net: NetStream, host: &str, port: u16, path: &str, protocol: &str) -> io::Result<AmqpStream> {
        let key = crate::util::base64_encode(&camel_tls::crypto::rng::random_bytes(16));
        let default_port = if net.is_tls() { 443 } else { 80 };
        let host_header = if port == default_port { host.to_string() } else { format!("{}:{}", host, port) };
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Protocol: {}\r\n\r\n",
            path, host_header, key, protocol
        );
        net.write_all(req.as_bytes())?;
        net.flush()?;
        let head = read_http_head(&mut net, &[])?;
        let (status_line, headers) = parse_head(&head);
        let ok = status_line.split_whitespace().nth(1) == Some("101");
        if !ok {
            return Err(io::Error::new(io::ErrorKind::ConnectionRefused, format!("WebSocket upgrade refused: {}", status_line)));
        }
        let expect = accept_key(&key);
        if header(&headers, "sec-websocket-accept") != Some(expect.as_str()) {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "WebSocket upgrade: bad Sec-WebSocket-Accept"));
        }
        Ok(AmqpStream { net, ws: Some(Arc::new(WsInner { client: true, read: Mutex::new(WsRead::default()), write: Mutex::new(()), close_sent: AtomicBool::new(false) })) })
    }

    /// Server side: answer the upgrade request whose first bytes (`prefix`) were already
    /// read. Accepts the `AMQPWSB10` and `amqp` subprotocols.
    pub fn websocket_server(mut net: NetStream, prefix: &[u8]) -> io::Result<AmqpStream> {
        let head = read_http_head(&mut net, prefix)?;
        let (_, headers) = parse_head(&head);
        let key = header(&headers, "sec-websocket-key").unwrap_or("").to_string();
        let upgrade = header(&headers, "upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        let protocol = header(&headers, "sec-websocket-protocol").and_then(|p| p.split(',').map(str::trim).find(|p| p.eq_ignore_ascii_case(AMQP_WS_PROTOCOL) || p.eq_ignore_ascii_case("amqp")).map(String::from));
        if key.is_empty() || !upgrade || protocol.is_none() {
            let _ = net.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not an AMQP WebSocket upgrade"));
        }
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: {}\r\n\r\n",
            accept_key(&key),
            protocol.unwrap()
        );
        net.write_all(resp.as_bytes())?;
        net.flush()?;
        Ok(AmqpStream { net, ws: Some(Arc::new(WsInner { client: false, read: Mutex::new(WsRead::default()), write: Mutex::new(()), close_sent: AtomicBool::new(false) })) })
    }

    fn write_ws_frame(&self, ws: &WsInner, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x80 | opcode);
        let mask_bit = if ws.client { 0x80 } else { 0 };
        let n = payload.len();
        if n < 126 {
            frame.push(mask_bit | n as u8);
        } else if n <= 0xffff {
            frame.push(mask_bit | 126);
            frame.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            frame.push(mask_bit | 127);
            frame.extend_from_slice(&(n as u64).to_be_bytes());
        }
        if ws.client {
            let mut mask = [0u8; 4];
            camel_tls::crypto::rng::fill(&mut mask);
            frame.extend_from_slice(&mask);
            frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        } else {
            frame.extend_from_slice(payload);
        }
        let _g = ws.write.lock().unwrap_or_else(|p| p.into_inner());
        let mut w = &self.net;
        w.write_all(&frame)?;
        w.flush()
    }

    fn ws_read(&self, ws: &WsInner, out: &mut [u8]) -> io::Result<usize> {
        let mut st = ws.read.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            if st.pos < st.buf.len() {
                let n = out.len().min(st.buf.len() - st.pos);
                out[..n].copy_from_slice(&st.buf[st.pos..st.pos + n]);
                st.pos += n;
                return Ok(n);
            }
            if st.eof || out.is_empty() {
                return Ok(0);
            }
            let mut r = &self.net;
            let mut h = [0u8; 2];
            match r.read(&mut h[..1])? {
                0 => {
                    st.eof = true;
                    return Ok(0);
                }
                _ => r.read_exact(&mut h[1..])?,
            }
            let opcode = h[0] & 0x0f;
            let masked = h[1] & 0x80 != 0;
            let mut len = (h[1] & 0x7f) as u64;
            if len == 126 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b)?;
                len = u16::from_be_bytes(b) as u64;
            } else if len == 127 {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                len = u64::from_be_bytes(b);
            }
            if len > 256 * 1024 * 1024 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "WebSocket frame too large"));
            }
            let mut mask = [0u8; 4];
            if masked {
                r.read_exact(&mut mask)?;
            }
            let mut payload = vec![0u8; len as usize];
            r.read_exact(&mut payload)?;
            if masked {
                for (i, b) in payload.iter_mut().enumerate() {
                    *b ^= mask[i % 4];
                }
            }
            match opcode {
                0x0..=0x2 => {
                    st.buf = payload;
                    st.pos = 0;
                }
                0x8 => {
                    st.eof = true;
                    if !ws.close_sent.swap(true, Ordering::SeqCst) {
                        let _ = self.write_ws_frame(ws, 0x8, &payload[..payload.len().min(2)]);
                    }
                    return Ok(0);
                }
                0x9 => self.write_ws_frame(ws, 0xA, &payload)?,
                _ => {}
            }
        }
    }

    fn do_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.ws {
            None => (&self.net).read(buf),
            Some(ws) => self.ws_read(ws, buf),
        }
    }

    fn do_write(&self, buf: &[u8]) -> io::Result<usize> {
        match &self.ws {
            None => (&self.net).write(buf),
            Some(ws) => {
                self.write_ws_frame(ws, 0x2, buf)?;
                Ok(buf.len())
            }
        }
    }
}

impl Read for AmqpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.do_read(buf)
    }
}

impl Read for &AmqpStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.do_read(buf)
    }
}

impl Write for AmqpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.do_write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.net).flush()
    }
}

impl Write for &AmqpStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.do_write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.net).flush()
    }
}

/// `Sec-WebSocket-Accept` for a key.
pub fn accept_key(key: &str) -> String {
    crate::util::base64_encode(&camel_core::language::support::sha1(format!("{}{}", key.trim(), WS_GUID).as_bytes()))
}

/// Read an HTTP head (up to the blank line) byte by byte, starting with `prefix`.
fn read_http_head(net: &mut NetStream, prefix: &[u8]) -> io::Result<String> {
    let mut head = prefix.to_vec();
    let mut b = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() > 16 * 1024 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "HTTP head too large"));
        }
        if net.read(&mut b)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed during the WebSocket handshake"));
        }
        head.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&head).into_owned())
}

fn parse_head(head: &str) -> (String, Vec<(String, String)>) {
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or("").to_string();
    let headers = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    (first, headers)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_accept_key() {
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn websocket_roundtrip() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let srv = std::thread::spawn(move || {
            let (tcp, _) = l.accept().unwrap();
            let mut net = NetStream::plain(tcp);
            let mut prefix = [0u8; 4];
            net.read_exact(&mut prefix).unwrap();
            let s = AmqpStream::websocket_server(net, &prefix).unwrap();
            let mut buf = [0u8; 5];
            (&s).read_exact(&mut buf).unwrap();
            (&s).write_all(&buf).unwrap();
            (&s).write_all(&vec![7u8; 70_000]).unwrap();
        });
        let net = NetStream::plain(std::net::TcpStream::connect(("127.0.0.1", port)).unwrap());
        let mut c = AmqpStream::websocket_client(net, "127.0.0.1", port, AMQP_WS_PATH, AMQP_WS_PROTOCOL).unwrap();
        c.write_all(b"hello").unwrap();
        let mut buf = [0u8; 5];
        c.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        let mut big = vec![0u8; 70_000];
        c.read_exact(&mut big).unwrap();
        assert!(big.iter().all(|b| *b == 7));
        srv.join().unwrap();
    }
}
