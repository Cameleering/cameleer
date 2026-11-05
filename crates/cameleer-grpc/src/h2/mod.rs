//! HTTP/2 (RFC 9113) client and server, std only.
//!
//! * [`frame`] — frame codec (DATA, HEADERS, PRIORITY, RST_STREAM, SETTINGS, PUSH_PROMISE,
//!   PING, GOAWAY, WINDOW_UPDATE, CONTINUATION).
//! * [`H2Connection`] — client connection (h2c prior knowledge via [`H2Connection::connect`],
//!   TLS with ALPN `h2` via [`H2Connection::connect_tls`]).
//! * [`H2Server`] — server, one thread per request stream ([`H2Handler`]).
//! * [`H2Stream`] — a stream handle used by both sides: `send_headers`, `send_data`
//!   (blocks on flow control), `send_trailers`, `recv_headers`, `recv_data`, `reset`.
//!
//! Implemented: connection preface, SETTINGS with ACK (header table size, push disabled,
//! max concurrent streams, initial window size, max frame size, max header list size),
//! HEADERS + CONTINUATION with HPACK ([`crate::hpack`]), padding, connection- and
//! stream-level flow control with WINDOW_UPDATE, PING, GOAWAY (graceful shutdown),
//! RST_STREAM, PRIORITY (parsed and ignored), the stream state machine
//! (idle/open/half-closed/closed with the RFC's connection/stream error rules) and
//! concurrent streams bounded by `SETTINGS_MAX_CONCURRENT_STREAMS`.
//!
//! HTTP/1.1 `Upgrade: h2c` (RFC 7540 section 3.2): servers answer an upgrade request with
//! `101 Switching Protocols` (applying its `HTTP2-Settings`) and serve the request as stream
//! 1; [`H2Connection::connect_upgrade`] is the client side. Not implemented: server push
//! (always disabled), RFC 8441 extended CONNECT, priority scheduling.
//!
//! This module is public so it can be reused, e.g. to add HTTP/2 to `cameleer-http`:
//! accept a connection, and when TLS negotiated ALPN `h2` (or the first 24 bytes are the
//! [`frame::PREFACE`]) hand it to the HTTP/2 engine instead of the HTTP/1.1 parser. 
//! [`serve_transport`] runs the server side on such an externally accepted [`Transport`];
//! [`H2Connection::with_transport`] is the client-side counterpart.
//!
//! ```no_run
//! use cameleer_grpc::h2::{H2Config, H2Connection, H2Request, H2Server, H2Stream, ServerRequest};
//! let server = H2Server::bind("127.0.0.1:0", H2Config::default(), None, |req: ServerRequest, stream: H2Stream| {
//!     let body = stream.read_body(None).unwrap_or_default();
//!     let _ = ServerRequest::respond(&stream, 200, &[("content-type", "text/plain")], &body);
//! }).unwrap();
//! let conn = H2Connection::connect(&server.local_addr().to_string(), H2Config::default()).unwrap();
//! let resp = conn.request(&H2Request::post("/echo", b"hi".to_vec()), None).unwrap();
//! assert_eq!(resp.body, b"hi");
//! ```

mod conn;
pub mod client;
pub mod frame;
pub mod server;
pub mod transport;
pub(crate) mod upgrade;

pub use client::{read_response, H2Connection};
pub use conn::{H2Config, H2Error, H2Stream, DEFAULT_WINDOW};
pub use frame::{ErrorCode, Frame};
pub use server::{serve_transport, H2Handler, H2Server, ServerRequest};
pub use transport::Transport;

use crate::hpack::HeaderList;

/// A simple request for [`H2Connection::request`].
#[derive(Clone, Debug, Default)]
pub struct H2Request {
    pub method: String,
    pub path: String,
    /// `:authority` (defaults to the connection address).
    pub authority: Option<String>,
    pub headers: HeaderList,
    pub body: Vec<u8>,
}

impl H2Request {
    pub fn new(method: &str, path: &str) -> Self {
        H2Request { method: method.to_string(), path: path.to_string(), ..Default::default() }
    }
    pub fn get(path: &str) -> Self {
        Self::new("GET", path)
    }
    pub fn post(path: &str, body: impl Into<Vec<u8>>) -> Self {
        H2Request { body: body.into(), ..Self::new("POST", path) }
    }
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_ascii_lowercase(), value.to_string()));
        self
    }
}

/// A complete response.
#[derive(Clone, Debug, Default)]
pub struct H2Response {
    pub status: u16,
    /// Response headers (pseudo-headers removed).
    pub headers: HeaderList,
    pub body: Vec<u8>,
    /// Trailing headers (empty when none were sent).
    pub trailers: HeaderList,
}

impl H2Response {
    /// First value of a header (or trailer) named `name`.
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().chain(self.trailers.iter()).find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }
}
