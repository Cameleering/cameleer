//! HTTP/2 client connections (h2c prior knowledge or TLS + ALPN `h2`).

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

use camel_tls::{TlsClientConfig, TlsStream};

use super::conn::{H2Config, H2Error, H2Stream, Role, Shared};
use super::transport::Transport;
use super::{H2Request, H2Response};

/// A client connection. Cheap to clone; streams are multiplexed concurrently.
///
/// ```no_run
/// use camel_grpc::h2::{H2Config, H2Connection, H2Request};
/// let conn = H2Connection::connect("127.0.0.1:8080", H2Config::default()).unwrap();
/// let resp = conn.request(&H2Request::get("/hello"), None).unwrap();
/// println!("{} {}", resp.status, String::from_utf8_lossy(&resp.body));
/// ```
///
/// The connection is closed (GOAWAY) when the last clone is dropped.
#[derive(Clone)]
pub struct H2Connection {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    shared: Arc<Shared>,
    authority: String,
}

impl Drop for ClientInner {
    fn drop(&mut self) {
        self.shared.close();
    }
}


pub(crate) fn tcp_connect(addr: &str, timeout: Duration) -> Result<TcpStream, H2Error> {
    let addrs: Vec<_> = addr.to_socket_addrs().map_err(|e| H2Error::Io(format!("cannot resolve {}: {}", addr, e)))?.collect();
    let mut last = None;
    for a in addrs {
        match TcpStream::connect_timeout(&a, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(H2Error::Io(format!("cannot connect to {}: {}", addr, last.map(|e| e.to_string()).unwrap_or_else(|| "no address".into()))))
}

impl H2Connection {
    /// Connects over cleartext TCP using HTTP/2 prior knowledge (h2c).
    pub fn connect(addr: &str, config: H2Config) -> Result<H2Connection, H2Error> {
        let tcp = tcp_connect(addr, config.connect_timeout)?;
        let t = Transport::tcp(tcp).map_err(|e| H2Error::Io(e.to_string()))?;
        Self::with_transport(t, addr, config)
    }

    /// Connects over TLS, offering ALPN `h2` (the server must select it).
    pub fn connect_tls(addr: &str, server_name: &str, tls: &TlsClientConfig, config: H2Config) -> Result<H2Connection, H2Error> {
        let tcp = tcp_connect(addr, config.connect_timeout)?;
        let _ = tcp.set_read_timeout(Some(config.connect_timeout));
        let _ = tcp.set_write_timeout(Some(config.connect_timeout));
        let stream = TlsStream::connect(tcp, server_name, tls).map_err(|e| H2Error::Io(format!("TLS handshake with {} failed: {}", addr, e)))?;
        if stream.alpn_protocol() != Some(b"h2") {
            return Err(H2Error::Protocol(super::ErrorCode::HTTP_1_1_REQUIRED, "server did not negotiate ALPN h2".into()));
        }
        let _ = stream.get_ref().set_read_timeout(None);
        let t = Transport::tls(stream).map_err(|e| H2Error::Io(e.to_string()))?;
        Self::with_transport(t, addr, config)
    }

    /// Connects over cleartext TCP starting with an HTTP/1.1 `Upgrade: h2c` request
    /// (`GET /` with `HTTP2-Settings`); after `101 Switching Protocols` the connection speaks
    /// HTTP/2 (the upgrade request's response arrives on stream 1 and is ignored).
    pub fn connect_upgrade(addr: &str, config: H2Config) -> Result<H2Connection, H2Error> {
        let mut tcp = tcp_connect(addr, config.connect_timeout)?;
        let _ = tcp.set_read_timeout(Some(config.connect_timeout));
        super::upgrade::client_upgrade(&mut tcp, addr, &config)?;
        let _ = tcp.set_read_timeout(None);
        let t = Transport::tcp(tcp).map_err(|e| H2Error::Io(e.to_string()))?;
        let shared = Shared::start_with(Role::Client, t, config, None, true)?;
        Ok(H2Connection { inner: Arc::new(ClientInner { shared, authority: addr.to_string() }) })
    }

    /// Runs HTTP/2 over an already established transport.
    pub fn with_transport(t: Transport, authority: &str, config: H2Config) -> Result<H2Connection, H2Error> {
        let shared = Shared::start(Role::Client, t, config, None)?;
        Ok(H2Connection { inner: Arc::new(ClientInner { shared, authority: authority.to_string() }) })
    }

    /// The default `:authority` (the address given to `connect`).
    pub fn authority(&self) -> &str {
        &self.inner.authority
    }

    /// Whether the connection runs over TLS.
    pub fn is_tls(&self) -> bool {
        self.inner.shared.tls
    }

    /// The ALPN protocol negotiated by TLS (`h2`), `None` for h2c.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.inner.shared.alpn.as_deref()
    }

    /// Opens a stream sending `headers` (must include the pseudo-headers).
    pub fn open_stream(&self, headers: &[(String, String)], end_stream: bool) -> Result<H2Stream, H2Error> {
        self.inner.shared.open_stream(headers, end_stream, None)
    }

    /// Like [`open_stream`](Self::open_stream), waiting at most `timeout` for a free stream slot.
    pub fn open_stream_timeout(&self, headers: &[(String, String)], end_stream: bool, timeout: Duration) -> Result<H2Stream, H2Error> {
        self.inner.shared.open_stream(headers, end_stream, Some(timeout))
    }

    /// Builds the request header list (pseudo-headers first).
    pub fn request_headers(&self, req: &H2Request) -> Vec<(String, String)> {
        let mut h = vec![
            (":method".to_string(), req.method.clone()),
            (":scheme".to_string(), if self.inner.shared.tls { "https" } else { "http" }.to_string()),
            (":path".to_string(), req.path.clone()),
            (":authority".to_string(), req.authority.clone().unwrap_or_else(|| self.inner.authority.clone())),
        ];
        for (n, v) in &req.headers {
            h.push((n.to_ascii_lowercase(), v.clone()));
        }
        h
    }

    /// Performs a complete request: sends headers and body, reads the response headers,
    /// body and trailers.
    pub fn request(&self, req: &H2Request, timeout: Option<Duration>) -> Result<H2Response, H2Error> {
        let deadline = timeout.map(|t| Instant::now() + t);
        let left = || -> Result<Option<Duration>, H2Error> {
            match deadline {
                Some(d) => Ok(Some(d.checked_duration_since(Instant::now()).ok_or(H2Error::Timeout)?)),
                None => Ok(None),
            }
        };
        let headers = self.request_headers(req);
        // REFUSED_STREAM guarantees the request was not processed: retry a few times
        let mut attempt = 0;
        loop {
            let result = (|| {
                let stream = self.inner.shared.open_stream(&headers, req.body.is_empty(), left()?)?;
                if !req.body.is_empty() {
                    stream.send_data_timeout(&req.body, true, left()?)?;
                }
                read_response(&stream, left()?)
            })();
            match result {
                Err(H2Error::StreamReset(super::ErrorCode::REFUSED_STREAM)) if attempt < 3 => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(5 * attempt));
                }
                other => return other,
            }
        }
    }

    /// Sends a PING and returns the round-trip time.
    pub fn ping(&self, timeout: Duration) -> Result<Duration, H2Error> {
        self.inner.shared.ping(timeout)
    }

    /// Sends GOAWAY and closes the connection.
    pub fn close(&self) {
        self.inner.shared.close();
    }

    /// True once the connection failed, was closed, or received GOAWAY.
    pub fn is_closed(&self) -> bool {
        self.inner.shared.is_closed()
    }

    /// Number of open streams.
    pub fn active_streams(&self) -> u32 {
        self.inner.shared.active_streams()
    }

    /// The peer's SETTINGS_MAX_CONCURRENT_STREAMS (u32::MAX if not advertised).
    pub fn peer_max_concurrent_streams(&self) -> u32 {
        self.inner.shared.peer_max_concurrent()
    }
}

/// Reads a full response (headers, body, trailers) from a client stream.
pub fn read_response(stream: &H2Stream, timeout: Option<Duration>) -> Result<H2Response, H2Error> {
    let deadline = timeout.map(|t| Instant::now() + t);
    let left = || -> Result<Option<Duration>, H2Error> {
        match deadline {
            Some(d) => Ok(Some(d.checked_duration_since(Instant::now()).ok_or(H2Error::Timeout)?)),
            None => Ok(None),
        }
    };
    // skip 1xx informational responses
    let headers = loop {
        let h = stream.recv_headers(left()?)?.ok_or_else(|| H2Error::Protocol(super::ErrorCode::PROTOCOL_ERROR, "stream ended without response headers".into()))?;
        let status = h.iter().find(|(n, _)| n == ":status").map(|(_, v)| v.clone()).unwrap_or_default();
        if !status.starts_with('1') {
            break h;
        }
    };
    let status: u16 = headers
        .iter()
        .find(|(n, _)| n == ":status")
        .and_then(|(_, v)| v.parse().ok())
        .ok_or_else(|| H2Error::Protocol(super::ErrorCode::PROTOCOL_ERROR, "missing :status".into()))?;
    let body = stream.read_body(left()?)?;
    let trailers = stream.recv_headers(left()?)?.unwrap_or_default();
    Ok(H2Response { status, headers: headers.into_iter().filter(|(n, _)| !n.starts_with(':')).collect(), body, trailers })
}
