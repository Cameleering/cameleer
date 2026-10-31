//! Blocking `TlsStream` wrapper implementing `Read` + `Write`.

use super::config::{TlsClientConfig, TlsServerConfig};
use super::conn::Conn;
use super::{CipherSuite, NamedGroup, TlsError, TlsVersion};
use std::fmt;
use std::io::{self, Read, Write};

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
    pub fn connect(stream: S, server_name: &str, config: &TlsClientConfig) -> Result<TlsStream<S>, TlsError> {
        let mut conn = Conn::new(stream, true);
        if let Err(e) = conn.client_handshake(config, server_name) {
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

    pub fn named_group(&self) -> NamedGroup {
        self.conn.group.expect("handshake complete")
    }

    /// The negotiated ALPN protocol, if any.
    pub fn alpn_protocol(&self) -> Option<&[u8]> {
        self.conn.alpn.as_deref()
    }

    /// Peer certificate chain as sent (DER, leaf first). Empty on the server side.
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        &self.conn.peer_certs
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
