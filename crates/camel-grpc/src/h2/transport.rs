//! Byte transports for HTTP/2 connections: plain TCP (h2c) or TLS (`camel-tls`).
//!
//! An HTTP/2 connection reads and writes concurrently. A `TcpStream` is simply cloned;
//! a [`TlsStream`] cannot be split, so it is shared behind a mutex: the reader waits for
//! socket readability with `peek` (without holding the lock) and then decrypts records
//! with a very short read timeout, so the writer is never blocked for long.

use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use camel_tls::TlsStream;

/// A connected byte transport split into a reader and a writer half.
pub struct Transport {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Socket handle used to shut the connection down (unblocks the reader).
    pub socket: TcpStream,
    /// The negotiated ALPN protocol (TLS only).
    pub alpn: Option<Vec<u8>>,
    pub tls: bool,
    pub peer_addr: Option<SocketAddr>,
    pub local_addr: Option<SocketAddr>,
}

impl Transport {
    /// Cleartext TCP (h2c with prior knowledge).
    pub fn tcp(stream: TcpStream) -> io::Result<Transport> {
        let _ = stream.set_nodelay(true);
        let reader = stream.try_clone()?;
        let socket = stream.try_clone()?;
        Ok(Transport {
            peer_addr: stream.peer_addr().ok(),
            local_addr: stream.local_addr().ok(),
            reader: Box::new(reader),
            writer: Box::new(stream),
            socket,
            alpn: None,
            tls: false,
        })
    }

    /// A TLS stream whose handshake has completed.
    pub fn tls(stream: TlsStream<TcpStream>) -> io::Result<Transport> {
        let socket = stream.get_ref().try_clone()?;
        let peek = stream.get_ref().try_clone()?;
        let _ = socket.set_nodelay(true);
        let alpn = stream.alpn_protocol().map(|p| p.to_vec());
        let shared = Arc::new(Mutex::new(stream));
        Ok(Transport {
            peer_addr: socket.peer_addr().ok(),
            local_addr: socket.local_addr().ok(),
            reader: Box::new(TlsReader { tls: shared.clone(), peek }),
            writer: Box::new(TlsWriter { tls: shared }),
            socket,
            alpn,
            tls: true,
        })
    }

    /// Shuts the socket down in both directions.
    pub fn shutdown(&self) {
        let _ = self.socket.shutdown(Shutdown::Both);
    }
}

struct TlsReader {
    tls: Arc<Mutex<TlsStream<TcpStream>>>,
    peek: TcpStream,
}

impl Read for TlsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            {
                let mut tls = self.tls.lock().unwrap_or_else(|e| e.into_inner());
                let _ = tls.get_ref().set_read_timeout(Some(Duration::from_millis(1)));
                match tls.read(buf) {
                    Ok(n) => return Ok(n),
                    Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted) => {}
                    Err(e) => return Err(e),
                }
            }
            // wait (without the lock) until the socket has more bytes or is closed
            let _ = self.peek.set_read_timeout(None);
            let mut b = [0u8; 1];
            match self.peek.peek(&mut b) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }
}

struct TlsWriter {
    tls: Arc<Mutex<TlsStream<TcpStream>>>,
}

impl Write for TlsWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tls.lock().unwrap_or_else(|e| e.into_inner()).write(buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.tls.lock().unwrap_or_else(|e| e.into_inner()).write_all(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.tls.lock().unwrap_or_else(|e| e.into_inner()).flush()
    }
}
