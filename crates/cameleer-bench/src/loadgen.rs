//! Closed-loop HTTP/1.1 keep-alive load generator, used against both the Cameleer and the
//! Apache Camel HTTP consumers, on the same machine or over the network. Each connection has
//! its own thread and sends one JSON POST at a time, reconnecting when the server closes the
//! connection. The body is either a fixed small document or an order whose id changes with
//! every request (so Kafka keys spread over the partitions).

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

/// An order request for the real-use scenarios: `POST path` with an order of id `id`.
pub fn order_request(host: &str, port: u16, path: &str, id: u64) -> Vec<u8> {
    let body = format!("{{\"orderId\":{id},\"customer\":\"ACME\",\"items\":[{{\"sku\":\"SKU-1\",\"qty\":2,\"price\":9.99}}]}}");
    format!("POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len())
        .into_bytes()
}

/// Read one response and return `(status, body)` (chunked bodies are not supported here).
pub fn read_response_body<R: BufRead>(r: &mut R, line: &mut String) -> io::Result<(u16, String)> {
    line.clear();
    r.read_line(line)?;
    let status: u16 = line.split_whitespace().nth(1).and_then(|s| s.parse().ok()).ok_or_else(|| bad("bad status line"))?;
    let mut len = 0usize;
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
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().map_err(|_| bad("bad content-length"))?;
            }
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
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
    host: String,
    port: u16,
    reader: Option<BufReader<TcpStream>>,
    connects: Arc<AtomicU64>,
}

impl Conn {
    fn exchange(&mut self, req: &[u8], line: &mut String) -> io::Result<u16> {
        if self.reader.is_none() {
            let s = TcpStream::connect((self.host.as_str(), self.port))?;
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
    run_remote("127.0.0.1", port, path, "small", conns, plan)
}

/// Load `host:port/path` with `conns` connections. `body` is `small` (the fixed document of
/// [`run`]) or `order` (an order with a new id on every request).
pub fn run_remote(host: &str, port: u16, path: &str, body: &str, conns: usize, plan: Plan) -> (Outcome, u64) {
    if body == "grpc" {
        return run_grpc(host, port, conns, plan);
    }
    if body == "order-h2" {
        return run_h2(host, port, path, conns, plan);
    }
    if body == "mcp-session" {
        let connects = Arc::new(AtomicU64::new(0));
        let out = driver::run(plan, conns, None, |w| {
            let mut s = crate::ai::McpSession::new(host, port, connects.clone());
            let mut n = 0u64;
            move || {
                n += 1;
                s.call((w as u64 + 1) * 1_000_000_000 + n).expect("mcp call failed");
            }
        });
        return (out, connects.load(Ordering::Relaxed));
    }
    let small = request_bytes(port, path);
    let orders = body == "order";
    let mcp = body == "mcp";
    let connects = Arc::new(AtomicU64::new(0));
    let out = driver::run(plan, conns, None, |w| {
        let small = small.clone();
        let mut conn = Conn { host: host.to_string(), port, reader: None, connects: connects.clone() };
        let mut line = String::new();
        let (host, path) = (host.to_string(), path.to_string());
        let mut n = 0u64;
        move || {
            n += 1;
            let order;
            let req: &[u8] = if mcp {
                order = crate::ai::mcp_request(&host, port, (w as u64 + 1) * 1_000_000_000 + n);
                &order
            } else if orders {
                order = order_request(&host, port, &path, (w as u64 + 1) * 1_000_000_000 + n);
                &order
            } else {
                &small
            };
            // a server may close a keep-alive connection between requests: retry once
            let status = match conn.exchange(req, &mut line) {
                Ok(s) => s,
                Err(_) => {
                    conn.reader = None;
                    conn.exchange(req, &mut line).expect("http request failed")
                }
            };
            assert_eq!(status, 200, "unexpected HTTP status");
        }
    });
    (out, connects.load(Ordering::Relaxed))
}

/// HTTP/2 connections (and gRPC channels) the load of `conns` concurrent calls is spread over: a
/// few connections carrying many streams, the way HTTP/2 clients run. `BENCH_H2_CONNS` changes it:
/// a client that shares few connections between many threads can become the limit of the run.
pub fn h2_connections() -> usize {
    std::env::var("BENCH_H2_CONNS").ok().and_then(|v| v.parse().ok()).filter(|n| *n > 0).unwrap_or(4)
}

/// A protobuf `bench.Req`: field 1 `name` (string) and field 2 `id` (int64).
pub fn grpc_request(id: u64) -> Vec<u8> {
    let name = b"bench";
    let mut m = vec![0x0a, name.len() as u8];
    m.extend_from_slice(name);
    m.push(0x10);
    let mut v = id;
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            m.push(b);
            break;
        }
        m.push(b | 0x80);
    }
    m
}

/// `conns` concurrent unary gRPC calls over [`h2_connections`] channels (HTTP/2 connections).
fn run_grpc(host: &str, port: u16, conns: usize, plan: Plan) -> (Outcome, u64) {
    let channels: Vec<Arc<cameleer_grpc::grpc::GrpcClient>> = (0..h2_connections().min(conns.max(1))).map(|_| Arc::new(cameleer_grpc::grpc::GrpcClient::plaintext(&format!("{host}:{port}")))).collect();
    let n_channels = channels.len();
    let out = driver::run(plan, conns, None, |w| {
        let channel = channels[w % n_channels].clone();
        let mut n = 0u64;
        move || {
            n += 1;
            let req = grpc_request((w as u64 + 1) * 1_000_000_000 + n);
            let (reply, _) = channel.unary("/bench.Echo/Say", &req, &vec![], Some(Duration::from_secs(30))).expect("grpc call failed");
            assert!(!reply.is_empty(), "empty gRPC reply");
        }
    });
    (out, n_channels as u64)
}

/// `conns` concurrent POSTs of orders over [`h2_connections`] HTTP/2 connections (h2c prior knowledge).
fn run_h2(host: &str, port: u16, path: &str, conns: usize, plan: Plan) -> (Outcome, u64) {
    // BENCH_H2_CLIENT_HTTP1=1: the same library client over HTTP/1.1 (one shared client, each thread
    // reads its own pooled connection), a control that has the library's work but no hand-off
    let http1 = std::env::var("BENCH_H2_CLIENT_HTTP1").is_ok_and(|v| v == "1");
    let n = if http1 { 1 } else { h2_connections().min(conns.max(1)) };
    let clients: Vec<Arc<simoom_client::Client>> = (0..n)
        .map(|_| {
            let mut cfg = simoom_client::Config::default();
            if http1 {
                cfg.version = simoom_client::HttpVersion::Http1;
                cfg.pool_max_idle_per_origin = conns.max(1);
                let client = simoom_client::ClientBuilder::default().config(cfg).build();
                return Arc::new(client);
            }
            cfg.version = simoom_client::HttpVersion::Http2;
            Arc::new(simoom_client::ClientBuilder::default().config(cfg).build())
        })
        .collect();
    let n_clients = clients.len();
    let url = format!("http://{host}:{port}{path}");
    let out = driver::run(plan, conns, None, |w| {
        let client = clients[w % n_clients].clone();
        let url = url.clone();
        let mut n = 0u64;
        move || {
            n += 1;
            let body = crate::scenarios::order_json((w as u64 + 1) * 1_000_000_000 + n).into_bytes();
            let resp = client.post(&url).header("content-type", "application/json").body(body).timeout(Duration::from_secs(30)).send().expect("http/2 request failed");
            assert_eq!(resp.status(), 200, "unexpected HTTP status");
            resp.bytes().expect("http/2 response body");
        }
    });
    (out, n_clients as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_grpc_request_is_a_valid_protobuf_message() {
        assert_eq!(grpc_request(1), [0x0a, 5, b'b', b'e', b'n', b'c', b'h', 0x10, 1]);
        // 300 = 0b1_0010_1100: two varint bytes
        assert_eq!(&grpc_request(300)[7..], [0x10, 0xac, 0x02]);
    }

    #[test]
    fn loads_the_grpc_and_http2_scenarios() {
        use cameleer_core::prelude::*;
        let ctx = CamelContext::new();
        cameleer_http::register(&ctx);
        cameleer_grpc::register(&ctx);
        let (grpc_port, h2_port) = (crate::free_port(), crate::free_port());
        crate::realworld::add_server_route(&ctx, "grpc", grpc_port, &crate::realworld::Env::from_env()).unwrap();
        crate::realworld::add_server_route(&ctx, "rest-json-h2", h2_port, &crate::realworld::Env::from_env()).unwrap();
        ctx.start().unwrap();
        // a slow runner needs a while for the first calls (connect, first request): a long warm-up
        let plan = Plan { warmup: Duration::from_millis(1500), iteration: Duration::from_millis(500), iters: 2 };
        let (out, channels) = run_remote("127.0.0.1", grpc_port, "bench.Echo", "grpc", 8, plan);
        assert!(out.msgs > 0, "grpc: {out:?}");
        assert_eq!(channels, h2_connections() as u64);
        let (out, _) = run_remote("127.0.0.1", h2_port, "/orders", "order-h2", 8, plan);
        assert!(out.msgs > 0, "http/2: {out:?}");
        ctx.stop().unwrap();
    }

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
        use cameleer_core::prelude::*;
        let ctx = CamelContext::new();
        cameleer_http::register(&ctx);
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
