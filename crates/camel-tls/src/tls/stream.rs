//! Blocking `TlsStream` wrapper implementing `Read` + `Write`.

use super::config::{TlsClientConfig, TlsServerConfig};
use super::conn::Conn;
use super::{CipherSuite, NamedGroup, TlsError, TlsVersion};
use std::fmt;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

/// A TLS connection over any blocking `Read + Write` transport (typically a
/// `TcpStream`). The handshake is performed by [`TlsStream::connect`] /
/// [`TlsStream::accept`]; afterwards the stream reads and writes application data.
///
/// `read` returns `Ok(0)` only after the peer's `close_notify`; an unexpected TCP
/// close is reported as `UnexpectedEof`. Call [`TlsStream::close`] to send
/// `close_notify` before dropping.
pub struct TlsStream<S: Read + Write> {
    conn: Conn<S>,
}

impl<S: Read + Write> fmt::Debug for TlsStream<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsStream")
            .field("client", &self.conn.is_client)
            .field("version", &self.conn.version)
            .field("suite", &self.conn.suite)
            .finish()
    }
}

impl<S: Read + Write> TlsStream<S> {
    /// Performs a client handshake. `server_name` is used for SNI (unless it is an IP
    /// address) and for certificate hostname verification.
    /// Sessions are cached (when the configuration has a session cache) under
    /// `server_name`; see [`TlsStream::connect_with_session_key`].
    pub fn connect(stream: S, server_name: &str, config: &TlsClientConfig) -> Result<TlsStream<S>, TlsError> {
        TlsStream::connect_with_session_key(stream, server_name, config, None)
    }

    /// Client handshake that resumes / stores the session under `session_key`
    /// (e.g. `host:port`; default `server_name`). Connections sharing a key (and a
    /// configuration) resume each other's sessions, e.g. FTPS data connections use
    /// the control connection's key.
    pub fn connect_with_session_key(stream: S, server_name: &str, config: &TlsClientConfig, session_key: Option<&str>) -> Result<TlsStream<S>, TlsError> {
        let mut conn = Conn::new(stream, true);
        if let Err(e) = conn.client_handshake(config, server_name, session_key) {
            return Err(conn.fail(e));
        }
        Ok(TlsStream { conn })
    }

    /// Performs a server handshake.
    pub fn accept(stream: S, config: &TlsServerConfig) -> Result<TlsStream<S>, TlsError> {
        let mut conn = Conn::new(stream, false);
        if let Err(e) = conn.server_handshake(config) {
            return Err(conn.fail(e));
        }
        Ok(TlsStream { conn })
    }

    pub fn protocol_version(&self) -> TlsVersion {
        self.conn.version.expect("handshake complete")
    }

    pub fn cipher_suite(&self) -> CipherSuite {
        self.conn.suite.expect("handshake complete")
    }

    /// The key exchange group (None for a resumed TLS 1.2 session, which has no key exchange).
    pub fn named_group(&self) -> Option<NamedGroup> {
        self.conn.group
    }

    /// Whether the session was resumed (TLS 1.3 PSK or TLS 1.2 abbreviated handshake).
    pub fn session_resumed(&self) -> bool {
        self.conn.resumed
    }

    /// Server side: an opaque identifier of the full handshake this session descends
    /// from - equal for a connection and every connection resuming its session (used
    /// e.g. to enforce FTPS data-channel session reuse). Empty on the client side.
    pub fn session_origin_id(&self) -> &[u8] {
        &self.conn.session_origin
    }

    /// The negotiated ALPN protocol, if any.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn.as_deref()
    }

    /// Peer certificate chain (DER, leaf first): the server chain on the client side,
    /// the verified client certificate chain (if any) on the server side.
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        &self.conn.peer_certs
    }

    /// Client side: the server's stapled OCSP response (DER, already validated), if any.
    pub fn ocsp_response(&self) -> Option<&[u8]> {
        self.conn.peer_ocsp.as_deref()
    }

    /// Server side: the SNI host name sent by the client.
    pub fn sni_hostname(&self) -> Option<&str> {
        self.conn.sni.as_deref()
    }

    /// TLS 1.3 only: rotates our sending key; with `request_peer` asks the peer to do the same.
    pub fn send_key_update(&mut self, request_peer: bool) -> Result<(), TlsError> {
        let r = self.conn.send_key_update(request_peer);
        r.map_err(|e| self.conn.fail(e))
    }

    /// Sends close_notify. Further writes fail; reads continue until the peer closes.
    pub fn close(&mut self) -> io::Result<()> {
        self.conn.close().map_err(io::Error::from)
    }

    /// Whether the peer has sent close_notify.
    pub fn peer_closed(&self) -> bool {
        self.conn.read_closed()
    }

    pub fn get_ref(&self) -> &S {
        &self.conn.io
    }

    pub fn get_mut(&mut self) -> &mut S {
        &mut self.conn.io
    }

    pub fn into_inner(self) -> S {
        self.conn.io
    }

    /// Splits the connection into independently usable read and write halves so one
    /// thread can block in `read` while others write (full duplex). `write_io` is a
    /// second handle to the same transport (e.g. `TcpStream::try_clone`); the read
    /// half keeps the original. TLS 1.3 KeyUpdate requests and alerts detected while
    /// reading are sent through the write half.
    pub fn split<W: Read + Write + Send + 'static>(self, write_io: W) -> Result<(TlsReadHalf<S>, TlsWriteHalf<W>), TlsError> {
        let (r, w) = self.conn.split(write_io)?;
        Ok((TlsReadHalf { conn: r }, TlsWriteHalf { conn: w }))
    }
}

/// The read half of a split [`TlsStream`].
pub struct TlsReadHalf<S: Read + Write> {
    conn: Conn<S>,
}

impl<S: Read + Write> fmt::Debug for TlsReadHalf<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TlsReadHalf").field("version", &self.conn.version).finish()
    }
}

impl<S: Read + Write> TlsReadHalf<S> {
    /// Whether the peer has sent close_notify.
    pub fn peer_closed(&self) -> bool {
        self.conn.read_closed()
    }

    pub fn get_ref(&self) -> &S {
        &self.conn.io
    }

    /// Peer certificate chain (DER, leaf first). Empty on the server side.
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        &self.conn.peer_certs
    }
}

impl<S: Read + Write> Read for TlsReadHalf<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.conn.read_app(buf) {
            Ok(n) => Ok(n),
            Err(TlsError::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {
                Err(e)
            }
            Err(e) => Err(self.conn.fail(e).into()),
        }
    }
}

/// The write half of a split [`TlsStream`]; clones share the same connection state.
pub struct TlsWriteHalf<W: Read + Write> {
    conn: Arc<Mutex<Conn<W>>>,
}

impl<W: Read + Write> Clone for TlsWriteHalf<W> {
    fn clone(&self) -> Self {
        TlsWriteHalf { conn: self.conn.clone() }
    }
}

impl<W: Read + Write> fmt::Debug for TlsWriteHalf<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsWriteHalf")
    }
}

impl<W: Read + Write> TlsWriteHalf<W> {
    fn lock(&self) -> io::Result<std::sync::MutexGuard<'_, Conn<W>>> {
        self.conn.lock().map_err(|_| io::Error::other("TLS write half poisoned"))
    }

    /// Writes all of `buf` as application data (one lock for the whole buffer, so
    /// concurrent writers never interleave inside one call).
    pub fn write_all_data(&self, buf: &[u8]) -> io::Result<()> {
        let mut c = self.lock()?;
        match c.write_app(buf) {
            Ok(_) => Ok(()),
            Err(TlsError::Closed) => Err(TlsError::Closed.into()),
            Err(e) => Err(c.fail(e).into()),
        }
    }

    /// Sends close_notify (idempotent).
    pub fn close(&self) -> io::Result<()> {
        self.lock()?.close().map_err(io::Error::from)
    }

    /// The negotiated protocol version.
    pub fn protocol_version(&self) -> Option<TlsVersion> {
        self.conn.lock().ok().and_then(|c| c.version)
    }

    /// The negotiated cipher suite.
    pub fn cipher_suite(&self) -> Option<CipherSuite> {
        self.conn.lock().ok().and_then(|c| c.suite)
    }

    /// The negotiated ALPN protocol.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.conn.lock().ok().and_then(|c| c.alpn.clone())
    }

    /// Server side: the SNI host name sent by the client.
    pub fn sni_hostname(&self) -> Option<String> {
        self.conn.lock().ok().and_then(|c| c.sni.clone())
    }

    /// Whether the session was resumed.
    pub fn session_resumed(&self) -> bool {
        self.conn.lock().map(|c| c.resumed).unwrap_or(false)
    }

    /// See [`TlsStream::session_origin_id`].
    pub fn session_origin_id(&self) -> Vec<u8> {
        self.conn.lock().map(|c| c.session_origin.clone()).unwrap_or_default()
    }
}

impl<W: Read + Write> Write for TlsWriteHalf<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all_data(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.lock()?.flush().map_err(io::Error::from)
    }
}

impl<S: Read + Write> Read for TlsStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.conn.read_app(buf) {
            Ok(n) => Ok(n),
            Err(TlsError::Io(e)) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {
                Err(e)
            }
            Err(e) => Err(self.conn.fail(e).into()),
        }
    }
}

impl<S: Read + Write> Write for TlsStream<S> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.conn.write_app(buf) {
            Ok(n) => Ok(n),
            Err(TlsError::Closed) => Err(TlsError::Closed.into()),
            Err(e) => Err(self.conn.fail(e).into()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.conn.flush().map_err(io::Error::from)
    }
}
