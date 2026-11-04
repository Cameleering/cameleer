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
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The TCP socket shared by every clone of a `NetStream` and by both TLS halves.
/// On Windows socket options such as the read timeout belong to the handle, so a
/// `try_clone`d handle would not see a timeout set on another one; sharing one
/// handle gives every platform the Unix (`dup`) behaviour.
#[derive(Clone)]
struct SharedTcp(Arc<TcpStream>);

impl Read for SharedTcp {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for SharedTcp {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

struct TlsShared {
    reader: Mutex<TlsReadHalf<SharedTcp>>,
    writer: TlsWriteHalf<SharedTcp>,
    peer_certificates: Vec<Vec<u8>>,
    resumed: bool,
    origin: Vec<u8>,
    session_key: Option<String>,
    eof: AtomicBool,
}

/// A plain or TLS TCP stream with `TcpStream`-like methods. Cheap to clone with
/// [`NetStream::try_clone`]; all clones refer to the same connection.
pub struct NetStream {
    tcp: Arc<TcpStream>,
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

/// Binds a listener, trying IPv4 addresses first like Java's default
/// (`java.net.preferIPv6Addresses=false`). Without this `localhost` binds to `::1`
/// only on Windows, where it resolves IPv6-first, and IPv4 clients are refused.
pub fn bind_listener(addr: impl ToSocketAddrs) -> io::Result<TcpListener> {
    let mut addrs: Vec<SocketAddr> = addr.to_socket_addrs()?.collect();
    addrs.sort_by_key(|a| a.is_ipv6());
    TcpListener::bind(&addrs[..])
}

/// Shuts `tcp` down and wakes threads blocked reading it, as `shutdown` does on
/// Unix. On Windows `shutdown` does not interrupt a blocked `recv`, so the socket's
/// pending I/O is cancelled too (the reader then gets an error).
pub fn shutdown_socket(tcp: &TcpStream, how: Shutdown) -> io::Result<()> {
    let r = tcp.shutdown(how);
    #[cfg(windows)]
    if matches!(how, Shutdown::Read | Shutdown::Both) {
        cancel_pending_io(tcp);
    }
    r
}

#[cfg(windows)]
fn cancel_pending_io(tcp: &TcpStream) {
    use std::os::windows::io::AsRawSocket;
    #[link(name = "kernel32")]
    extern "system" {
        fn CancelIoEx(handle: *mut core::ffi::c_void, overlapped: *mut core::ffi::c_void) -> i32;
    }
    // SAFETY: the handle is a live socket owned by `tcp`; a null OVERLAPPED cancels
    // all of its pending I/O, whichever thread issued it.
    unsafe {
        CancelIoEx(tcp.as_raw_socket() as *mut core::ffi::c_void, std::ptr::null_mut());
    }
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
        NetStream { tcp: Arc::new(tcp), tls: None }
    }

    fn from_tls(tls: TlsStream<SharedTcp>, session_key: Option<String>) -> io::Result<NetStream> {
        let tcp = tls.get_ref().0.clone();
        let wio = tls.get_ref().clone();
        let peer_certificates = tls.peer_certificates().to_vec();
        let resumed = tls.session_resumed();
        let origin = tls.session_origin_id().to_vec();
        let (r, w) = tls.split(wio).map_err(tls_io_error)?;
        Ok(NetStream {
            tcp,
            tls: Some(Arc::new(TlsShared {
                reader: Mutex::new(r),
                writer: w,
                peer_certificates,
                resumed,
                origin,
                session_key,
                eof: AtomicBool::new(false),
            })),
        })
    }

    /// Performs a TLS client handshake over `tcp`. `server_name` drives SNI and
    /// hostname verification. If `tcp` has no read timeout, a 30 s timeout is applied
    /// during the handshake only. Sessions are resumed per `server_name:port`.
    pub fn client(tcp: TcpStream, server_name: &str, config: &TlsClientConfig) -> io::Result<NetStream> {
        NetStream::client_shared(Arc::new(tcp), server_name, config)
    }

    fn client_shared(tcp: Arc<TcpStream>, server_name: &str, config: &TlsClientConfig) -> io::Result<NetStream> {
        let key = match tcp.peer_addr() {
            Ok(a) => format!("{}:{}", server_name, a.port()),
            Err(_) => server_name.to_string(),
        };
        NetStream::client_shared_with_session_key(tcp, server_name, config, &key)
    }

    /// Client handshake resuming / storing the TLS session under `session_key`
    /// (e.g. the FTPS control connection's key for its data connections).
    pub fn client_with_session_key(tcp: TcpStream, server_name: &str, config: &TlsClientConfig, session_key: &str) -> io::Result<NetStream> {
        NetStream::client_shared_with_session_key(Arc::new(tcp), server_name, config, session_key)
    }

    fn client_shared_with_session_key(tcp: Arc<TcpStream>, server_name: &str, config: &TlsClientConfig, session_key: &str) -> io::Result<NetStream> {
        let tls = with_handshake_timeout(SharedTcp(tcp), |t| TlsStream::connect_with_session_key(t, server_name, config, Some(session_key)))?;
        NetStream::from_tls(tls, Some(session_key.to_string()))
    }

    /// Performs a TLS server handshake over an accepted `tcp` connection.
    pub fn server(tcp: TcpStream, config: &TlsServerConfig) -> io::Result<NetStream> {
        NetStream::server_shared(Arc::new(tcp), config)
    }

    fn server_shared(tcp: Arc<TcpStream>, config: &TlsServerConfig) -> io::Result<NetStream> {
        let tls = with_handshake_timeout(SharedTcp(tcp), |t| TlsStream::accept(t, config))?;
        NetStream::from_tls(tls, None)
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
        NetStream::client_shared(self.tcp, server_name, config)
    }

    /// Upgrades a plain connection to TLS as a server (STARTTLS on the server side).
    pub fn start_tls_server(self, config: &TlsServerConfig) -> io::Result<NetStream> {
        if self.tls.is_some() {
            return Err(io::Error::other("connection already uses TLS"));
        }
        NetStream::server_shared(self.tcp, config)
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

    /// The peer certificate chain (DER, leaf first): the server's on the client side,
    /// the verified client certificate chain (mutual TLS) on the server side.
    pub fn peer_certificates(&self) -> &[Vec<u8>] {
        self.tls.as_ref().map(|t| t.peer_certificates.as_slice()).unwrap_or(&[])
    }

    /// Whether the TLS session was resumed (abbreviated handshake).
    pub fn session_resumed(&self) -> bool {
        self.tls.as_ref().is_some_and(|t| t.resumed)
    }

    /// Server side: identifies the full handshake the session descends from (see
    /// [`TlsStream::session_origin_id`]); empty for plain and client connections.
    pub fn session_origin_id(&self) -> &[u8] {
        self.tls.as_ref().map(|t| t.origin.as_slice()).unwrap_or(&[])
    }

    /// Client side: the key the TLS session is cached under (`host:port` by default).
    pub fn session_key(&self) -> Option<&str> {
        self.tls.as_ref().and_then(|t| t.session_key.as_deref())
    }

    /// Another handle to the same connection.
    pub fn try_clone(&self) -> io::Result<NetStream> {
        Ok(NetStream { tcp: self.tcp.clone(), tls: self.tls.clone() })
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
        shutdown_socket(&self.tcp, how)
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
            None => (&*self.tcp).read(buf),
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
            None => (&*self.tcp).write(buf),
            Some(t) => {
                t.writer.write_all_data(buf)?;
                Ok(buf.len())
            }
        }
    }

    fn do_flush(&self) -> io::Result<()> {
        match &self.tls {
            None => (&*self.tcp).flush(),
            Some(t) => t.writer.clone().flush(),
        }
    }
}

fn with_handshake_timeout<T>(tcp: SharedTcp, f: impl FnOnce(SharedTcp) -> Result<T, TlsError>) -> io::Result<T> {
    let had = tcp.0.read_timeout().ok().flatten();
    if had.is_none() {
        let _ = tcp.0.set_read_timeout(Some(Duration::from_secs(30)));
    }
    let r = f(tcp.clone()).map_err(tls_io_error);
    if had.is_none() {
        let _ = tcp.0.set_read_timeout(None);
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

    #[test]
    fn shutdown_wakes_a_blocked_reader() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let _client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (tcp, _) = listener.accept().unwrap();
        let s = NetStream::plain(tcp);
        let r = s.try_clone().unwrap();
        let reader = thread::spawn(move || {
            let mut b = [0u8; 4];
            (&r).read(&mut b)
        });
        thread::sleep(Duration::from_millis(100));
        s.shutdown(Shutdown::Both).unwrap();
        let start = std::time::Instant::now();
        while !reader.is_finished() && start.elapsed() < Duration::from_secs(5) {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(reader.is_finished(), "a read blocked on a shut-down socket must return");
        assert!(!matches!(reader.join().unwrap(), Ok(n) if n > 0));
    }

    #[test]
    fn bind_listener_prefers_ipv4_for_localhost() {
        let l = bind_listener("localhost:0").unwrap();
        assert!(l.local_addr().unwrap().is_ipv4());
        TcpStream::connect(("127.0.0.1", l.local_addr().unwrap().port())).unwrap();
    }
}
