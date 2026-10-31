//! [`NetStream`]: a cloneable TCP stream that is either plain or TLS-protected.
//!
//! Network components are written against `TcpStream` idioms (`try_clone` for a
//! reader thread plus a writer, `set_read_timeout`, `shutdown`...). `NetStream`
//! keeps those idioms for TLS connections: clones share one TLS session whose read
//! and write halves are locked independently, so one thread can block in `read`
//! while another writes.
//!
//! ```no_run
//! use camel_tls::net::NetStream;
//! use camel_tls::TlsClientConfig;
//! use std::io::{BufRead, BufReader, Write};
//! let cfg = TlsClientConfig::builder().build();
//! let s = NetStream::connect_tls("example.com", 443, &cfg, std::time::Duration::from_secs(5)).unwrap();
//! let mut w = s.try_clone().unwrap();
//! w.write_all(b"HEAD / HTTP/1.1\r\nHost: example.com\r\n\r\n").unwrap();
//! let mut line = String::new();
//! BufReader::new(s).read_line(&mut line).unwrap();
//! ```

use crate::tls::config::{TlsClientConfig, TlsServerConfig};
use crate::tls::stream::{TlsReadHalf, TlsStream, TlsWriteHalf};
use crate::tls::{CipherSuite, TlsError, TlsVersion};
use std::fmt;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct TlsShared {
    reader: Mutex<TlsReadHalf<TcpStream>>,
    writer: TlsWriteHalf<TcpStream>,
    peer_certificates: Vec<Vec<u8>>,
    eof: AtomicBool,
}

/// A plain or TLS TCP stream with `TcpStream`-like methods. Cheap to clone with
/// [`NetStream::try_clone`]; all clones refer to the same connection.
pub struct NetStream {
    tcp: TcpStream,
    tls: Option<Arc<TlsShared>>,
}

impl fmt::Debug for NetStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NetStream").field("peer", &self.tcp.peer_addr().ok()).field("tls", &self.is_tls()).finish()
    }
}

impl From<TcpStream> for NetStream {
    fn from(tcp: TcpStream) -> Self {
        NetStream::plain(tcp)
    }
}

/// Connects to `host:port`, trying every resolved address (timeout zero = OS default).
pub fn connect_tcp(host: &str, port: u16, timeout: Duration) -> io::Result<TcpStream> {
    let mut last = None;
    for a in (host, port).to_socket_addrs()? {
        let r = if timeout.is_zero() { TcpStream::connect(a) } else { TcpStream::connect_timeout(&a, timeout) };
        match r {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(last.unwrap_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Unknown host: {host}"))))
}

/// Converts a TLS error into an `io::Error` (certificate problems keep their text).
pub fn tls_io_error(e: TlsError) -> io::Error {
    match e {
        TlsError::Io(e) => e,
        other => io::Error::new(io::ErrorKind::InvalidData, other),
    }
}

impl NetStream {
    /// Wraps a plain TCP connection.
    pub fn plain(tcp: TcpStream) -> NetStream {
        NetStream { tcp, tls: None }
    }

    fn from_tls(tls: TlsStream<TcpStream>) -> io::Result<NetStream> {
        let tcp = tls.get_ref().try_clone()?;
        let wio = tls.get_ref().try_clone()?;
        let peer_certificates = tls.peer_certificates().to_vec();
        let (r, w) = tls.split(wio).map_err(tls_io_error)?;
        Ok(NetStream { tcp, tls: Some(Arc::new(TlsShared { reader: Mutex::new(r), writer: w, peer_certificates, eof: AtomicBool::new(false) })) })
    }

    /// Performs a TLS client handshake over `tcp`. `server_name` drives SNI and
    /// hostname verification. If `tcp` has no read timeout, `handshake_timeout`
    /// is applied during the handshake only.
    pub fn client(tcp: TcpStream, server_name: &str, config: &TlsClientConfig) -> io::Result<NetStream> {
        let tls = with_handshake_timeout(&tcp, |t| TlsStream::connect(t, server_name, config))?;
        NetStream::from_tls(tls)
    }

    /// Performs a TLS server handshake over an accepted `tcp` connection.
    pub fn server(tcp: TcpStream, config: &TlsServerConfig) -> io::Result<NetStream> {
        let tls = with_handshake_timeout(&tcp, |t| TlsStream::accept(t, config))?;
        NetStream::from_tls(tls)
    }

    /// Connects to `host:port` and, when `tls` is given, performs a client handshake.
    pub fn connect(host: &str, port: u16, tls: Option<&TlsClientConfig>, timeout: Duration) -> io::Result<NetStream> {
        let tcp = connect_tcp(host, port, timeout)?;
        match tls {
            Some(cfg) => NetStream::client(tcp, host, cfg),
            None => Ok(NetStream::plain(tcp)),
        }
    }

    /// Connects to `host:port` over TLS.
    pub fn connect_tls(host: &str, port: u16, config: &TlsClientConfig, timeout: Duration) -> io::Result<NetStream> {
        NetStream::connect(host, port, Some(config), timeout)
    }

    /// Upgrades a plain connection to TLS as a client (STARTTLS, `AUTH TLS`, SSLRequest).
    /// Other clones of this stream must be dropped and no data may be buffered.
    pub fn start_tls_client(self, server_name: &str, config: &TlsClientConfig) -> io::Result<NetStream> {
        if self.tls.is_some() {
            return Err(io::Error::other("connection already uses TLS"));
        }
        NetStream::client(self.tcp, server_name, config)
    }

    /// Upgrades a plain connection to TLS as a server (STARTTLS on the server side).
    pub fn start_tls_server(self, config: &TlsServerConfig) -> io::Result<NetStream> {
        if self.tls.is_some() {
            return Err(io::Error::other("connection already uses TLS"));
        }
        NetStream::server(self.tcp, config)
    }

    /// Whether the connection is TLS-protected.
    pub fn is_tls(&self) -> bool {
        self.tls.is_some()
    }

    /// The negotiated TLS version (None for plain connections).
    pub fn tls_version(&self) -> Option<TlsVersion> {
        self.tls.as_ref().and_then(|t| t.writer.protocol_version())
    }

    /// The negotiated cipher suite (None for plain connections).
    pub fn cipher_suite(&self) -> Option<CipherSuite> {
        self.tls.as_ref().and_then(|t| t.writer.cipher_suite())
    }

    /// The negotiated ALPN protocol.
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.tls.as_ref().and_then(|t| t.writer.alpn_protocol())
    }

    /// Server side: the SNI host name requested by the client.
    pub fn sni_hostname(&self) -> Option<String> {
        self.tls.as_ref().and_then(|t| t.writer.sni_hostname())
    }

    /// Client side: the server certificate chain (DER, leaf first).
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        self.tls.as_ref().map(|t| t.peer_certificates.as_slice()).unwrap_or(&[])
    }

    /// Another handle to the same connection.
    pub fn try_clone(&self) -> io::Result<NetStream> {
        Ok(NetStream { tcp: self.tcp.try_clone()?, tls: self.tls.clone() })
    }

    /// The underlying TCP socket (do not read or write it directly on TLS streams).
    pub fn tcp(&self) -> &TcpStream {
        &self.tcp
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.tcp.set_read_timeout(d)
    }

    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.tcp.set_write_timeout(d)
    }

    pub fn read_timeout(&self) -> io::Result<Option<Duration>> {
        self.tcp.read_timeout()
    }

    pub fn set_nodelay(&self, v: bool) -> io::Result<()> {
        self.tcp.set_nodelay(v)
    }

    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.peer_addr()
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp.local_addr()
    }

    /// Shuts the socket down; for TLS a `close_notify` is sent first when the write
    /// direction is closed.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        if let (Some(t), Shutdown::Write | Shutdown::Both) = (&self.tls, how) {
            let _ = t.writer.close();
        }
        self.tcp.shutdown(how)
    }

    /// Sends TLS `close_notify` (no-op for plain streams) without closing the socket.
    pub fn close_notify(&self) -> io::Result<()> {
        match &self.tls {
            Some(t) => t.writer.close(),
            None => Ok(()),
        }
    }

    fn do_read(&self, buf: &mut [u8]) -> io::Result<usize> {
        match &self.tls {
            None => (&self.tcp).read(buf),
            Some(t) => {
                if t.eof.load(Ordering::Acquire) {
                    return Ok(0);
                }
                let mut r = t.reader.lock().map_err(|_| io::Error::other("TLS read half poisoned"))?;
                match r.read(buf) {
                    // an unexpected TCP close after the peer stopped talking is reported as EOF,
                    // matching plain TCP behaviour (many protocols just close the socket)
                    Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                        t.eof.store(true, Ordering::Release);
                        Ok(0)
                    }
                    Ok(0) if !buf.is_empty() => {
                        t.eof.store(true, Ordering::Release);
                        Ok(0)
                    }
                    other => other,
                }
            }
        }
    }

    fn do_write(&self, buf: &[u8]) -> io::Result<usize> {
        match &self.tls {
            None => (&self.tcp).write(buf),
            Some(t) => {
                t.writer.write_all_data(buf)?;
                Ok(buf.len())
            }
        }
    }

    fn do_flush(&self) -> io::Result<()> {
        match &self.tls {
            None => (&self.tcp).flush(),
            Some(t) => t.writer.clone().flush(),
        }
    }
}

fn with_handshake_timeout<T>(tcp: &TcpStream, f: impl FnOnce(TcpStream) -> Result<T, TlsError>) -> io::Result<T> {
    let had = tcp.read_timeout().ok().flatten();
    if had.is_none() {
        let _ = tcp.set_read_timeout(Some(Duration::from_secs(30)));
    }
    let r = f(tcp.try_clone()?).map_err(tls_io_error);
    if had.is_none() {
        let _ = tcp.set_read_timeout(None);
    }
    r
}

impl Read for NetStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.do_read(buf)
    }
}

impl Read for &NetStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.do_read(buf)
    }
}

impl Write for NetStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.do_write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.do_flush()
    }
}

impl Write for &NetStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.do_write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.do_flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certgen::self_signed;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn duplex_reader_and_writer_threads() {
        let id = self_signed("localhost");
        let scfg = TlsServerConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let s = NetStream::server(s, &scfg).unwrap();
            assert!(s.is_tls());
            // echo lines with a separate writer handle
            let mut w = s.try_clone().unwrap();
            for line in BufReader::new(s).lines() {
                let line = line.unwrap();
                writeln!(w, "echo {line}").unwrap();
                if line == "bye" {
                    break;
                }
            }
            w.shutdown(Shutdown::Both).unwrap();
        });
        let cfg = TlsClientConfig::builder().system_roots(false).add_root_pem(id.cert_pem.as_bytes()).unwrap().build();
        let s = NetStream::connect_tls("localhost", port, &cfg, Duration::from_secs(5)).unwrap();
        let reader = s.try_clone().unwrap();
        let rt = thread::spawn(move || BufReader::new(reader).lines().map(|l| l.unwrap()).collect::<Vec<_>>());
        let mut w = s.try_clone().unwrap();
        for i in 0..20 {
            writeln!(w, "m{i}").unwrap();
        }
        writeln!(w, "bye").unwrap();
        let lines = rt.join().unwrap();
        assert_eq!(lines.len(), 21);
        assert_eq!(lines[0], "echo m0");
        assert_eq!(lines[20], "echo bye");
        server.join().unwrap();
    }

    #[test]
    fn read_timeout_does_not_break_the_session() {
        let id = self_signed("localhost");
        let scfg = TlsServerConfig::from_pem(id.cert_pem.as_bytes(), id.key_pem.as_bytes()).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            let mut s = NetStream::server(s, &scfg).unwrap();
            thread::sleep(Duration::from_millis(150));
            s.write_all(b"late\n").unwrap();
            let mut b = [0u8; 1];
            let _ = s.read(&mut b);
        });
        let cfg = TlsClientConfig::builder().insecure_skip_verify(true).build();
        let s = NetStream::connect_tls("127.0.0.1", port, &cfg, Duration::from_secs(5)).unwrap();
        s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let mut buf = [0u8; 16];
        let e = (&s).read(&mut buf).unwrap_err();
        assert!(matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut));
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let n = (&s).read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"late\n");
        (&s).write_all(b"x").unwrap();
        server.join().unwrap();
    }
}
