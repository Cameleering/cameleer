//! Closed-loop HTTP/1.1 keep-alive load generator, used against both the rust-camel and the
//! Apache Camel HTTP consumers. Each connection has its own thread and sends one small JSON
//! POST at a time, reconnecting when the server closes the connection.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::driver::{self, Outcome, Plan};

pub const REQUEST_BODY: &str = "{\"id\":42,\"name\":\"bench\",\"tags\":[\"a\",\"b\"]}";

/// The raw request bytes for `POST path` on `host:port`.
pub fn request_bytes(port: u16, path: &str) -> Vec<u8> {
    format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{REQUEST_BODY}",
        REQUEST_BODY.len()
    )
    .into_bytes()
}

/// Read one response; returns `(status, keep_alive)`. The body is read and discarded.
pub fn read_response<R: BufRead>(r: &mut R, line: &mut String) -> io::Result<(u16, bool)> {
    line.clear();
    if r.read_line(line)? == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| bad("bad status line"))?;
    let http10 = line.starts_with("HTTP/1.0");
    let mut len: Option<usize> = None;
    let mut chunked = false;
    let mut keep_alive = !http10;
    loop {
        line.clear();
        if r.read_line(line)? == 0 {
            return Err(bad("eof in headers"));
        }
        let h = line.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            let v = v.trim();
            if k.eq_ignore_ascii_case("content-length") {
                len = Some(v.parse().map_err(|_| bad("bad content-length"))?);
            } else if k.eq_ignore_ascii_case("transfer-encoding") {
                chunked = v.eq_ignore_ascii_case("chunked");
            } else if k.eq_ignore_ascii_case("connection") {
                keep_alive = !v.eq_ignore_ascii_case("close");
            }
        }
    }
    if chunked {
        loop {
            line.clear();
            r.read_line(line)?;
            let n = usize::from_str_radix(line.trim().split(';').next().unwrap_or(""), 16).map_err(|_| bad("bad chunk"))?;
            if n == 0 {
                line.clear();
                r.read_line(line)?;
                break;
            }
            skip(r, n + 2)?;
        }
    } else if let Some(n) = len {
        skip(r, n)?;
    } else {
        keep_alive = false;
        io::copy(r, &mut io::sink())?;
    }
    Ok((status, keep_alive))
}

fn skip<R: Read>(r: &mut R, n: usize) -> io::Result<()> {
    let got = io::copy(&mut r.by_ref().take(n as u64), &mut io::sink())?;
    if got as usize != n {
        return Err(bad("short body"));
    }
    Ok(())
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

struct Conn {
    port: u16,
    reader: Option<BufReader<TcpStream>>,
    connects: Arc<AtomicU64>,
}

impl Conn {
    fn exchange(&mut self, req: &[u8], line: &mut String) -> io::Result<u16> {
        if self.reader.is_none() {
            let s = TcpStream::connect(("127.0.0.1", self.port))?;
            self.connects.fetch_add(1, Ordering::Relaxed);
            s.set_nodelay(true)?;
            s.set_read_timeout(Some(Duration::from_secs(30)))?;
            self.reader = Some(BufReader::with_capacity(16 * 1024, s));
        }
        let r = self.reader.as_mut().unwrap();
        r.get_mut().write_all(req)?;
        let (status, keep_alive) = read_response(r, line)?;
        if !keep_alive {
            self.reader = None;
        }
        Ok(status)
    }
}

/// Load `127.0.0.1:port/path` with `conns` connections. Every non-200 response or I/O error
/// fails the run: an error rate would make the numbers meaningless. Also returns how many
/// TCP connections were opened (more than `conns` means the server closed keep-alive
/// connections).
pub fn run(port: u16, path: &str, conns: usize, plan: Plan) -> (Outcome, u64) {
    let req = request_bytes(port, path);
    let connects = Arc::new(AtomicU64::new(0));
    let out = driver::run(plan, conns, None, |_| {
        let req = req.clone();
        let mut conn = Conn { port, reader: None, connects: connects.clone() };
        let mut line = String::new();
        move || {
            // a server may close a keep-alive connection between requests: retry once
            let status = match conn.exchange(&req, &mut line) {
                Ok(s) => s,
                Err(_) => {
                    conn.reader = None;
                    conn.exchange(&req, &mut line).expect("http request failed")
                }
            };
            assert_eq!(status, 200, "unexpected HTTP status");
        }
    });
    (out, connects.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_content_length_chunked_and_close() {
        let mut line = String::new();
        let mut r = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabcHTTP/1.1 404 Not Found\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n2\r\nab\r\n0\r\n\r\n".to_vec());
        assert_eq!(read_response(&mut r, &mut line).unwrap(), (200, true));
        assert_eq!(read_response(&mut r, &mut line).unwrap(), (404, false));
        assert!(read_response(&mut r, &mut line).is_err());
    }

    #[test]
    fn loads_a_rust_camel_http_consumer() {
        use camel_core::prelude::*;
        let ctx = CamelContext::new();
        camel_http::register(&ctx);
        let port = crate::free_port();
        crate::scenarios::add_http_route(&ctx, port).unwrap();
        ctx.start().unwrap();
        let plan = Plan { warmup: Duration::from_millis(50), iteration: Duration::from_millis(100), iters: 2 };
        let (out, connects) = run(port, "/bench", 4, plan);
        assert!(out.msgs > 10, "{out:?}");
        // keep-alive: connections are reused (a busy machine may force a few reconnects)
        assert!(connects >= 4 && connects * 10 < out.msgs, "{connects} connections for {} requests", out.msgs);
        ctx.stop().unwrap();
    }
}
