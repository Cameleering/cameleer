//! An embedded OTLP/HTTP receiver for tests: accepts `POST /v1/traces` (protobuf, optionally
//! gzip), decodes the spans and keeps them; it can answer with scripted statuses to exercise the
//! exporter's retries.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::proto::{decode_export_request, ReceivedSpan};

/// One received request.
#[derive(Clone, Debug)]
pub struct ReceivedRequest {
    pub path: String,
    pub headers: Vec<(String, String)>,
    /// The body as sent (compressed if `Content-Encoding: gzip`).
    pub body: Vec<u8>,
    /// The status the receiver answered.
    pub status: u16,
}

impl ReceivedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// A scripted answer: status, extra headers, body.
pub type Answer = (u16, Vec<(String, String)>, Vec<u8>);

#[derive(Default)]
struct Inner {
    spans: Vec<ReceivedSpan>,
    requests: Vec<ReceivedRequest>,
    script: VecDeque<Answer>,
    /// Delay before answering (to make the exporter wait).
    delay: Duration,
}

/// The receiver; stops when dropped.
pub struct OtlpReceiver {
    port: u16,
    inner: Arc<Mutex<Inner>>,
    stop: Arc<AtomicBool>,
}

impl OtlpReceiver {
    /// Starts on a free port of 127.0.0.1.
    pub fn start() -> OtlpReceiver {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind the OTLP receiver");
        let port = listener.local_addr().unwrap().port();
        let inner = Arc::new(Mutex::new(Inner::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (i2, s2) = (inner.clone(), stop.clone());
        std::thread::Builder::new()
            .name("otlp-receiver".into())
            .spawn(move || {
                for conn in listener.incoming() {
                    if s2.load(Ordering::Relaxed) {
                        return;
                    }
                    let Ok(conn) = conn else { continue };
                    let i3 = i2.clone();
                    let _ = std::thread::Builder::new().name("otlp-receiver-conn".into()).spawn(move || serve(conn, i3));
                }
            })
            .expect("spawn the OTLP receiver");
        OtlpReceiver { port, inner, stop }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The base url (`OTEL_EXPORTER_OTLP_ENDPOINT`).
    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The traces url.
    pub fn traces_url(&self) -> String {
        format!("{}/v1/traces", self.endpoint())
    }

    /// Answers the next requests with these statuses (then 200 again).
    pub fn script(&self, answers: impl IntoIterator<Item = Answer>) {
        self.inner.lock().unwrap().script.extend(answers);
    }

    /// Waits this long before each answer.
    pub fn set_delay(&self, d: Duration) {
        self.inner.lock().unwrap().delay = d;
    }

    pub fn spans(&self) -> Vec<ReceivedSpan> {
        self.inner.lock().unwrap().spans.clone()
    }

    pub fn requests(&self) -> Vec<ReceivedRequest> {
        self.inner.lock().unwrap().requests.clone()
    }

    /// Waits until at least `n` spans arrived, at most `timeout`.
    pub fn wait_for_spans(&self, n: usize, timeout: Duration) -> Vec<ReceivedSpan> {
        let deadline = Instant::now() + timeout;
        loop {
            let s = self.spans();
            if s.len() >= n || Instant::now() >= deadline {
                return s;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for OtlpReceiver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = TcpStream::connect(("127.0.0.1", self.port));
    }
}

fn serve(conn: TcpStream, inner: Arc<Mutex<Inner>>) {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(30)));
    let mut w = match conn.try_clone() {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut r = BufReader::new(conn);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
        let mut headers = Vec::new();
        loop {
            let mut h = String::new();
            if r.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        let get = |n: &str| headers.iter().find(|(k, _): &&(String, String)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
        let mut body = Vec::new();
        if let Some(n) = get("content-length").and_then(|v| v.parse::<usize>().ok()) {
            body.resize(n, 0);
            if r.read_exact(&mut body).is_err() {
                return;
            }
        } else if get("transfer-encoding").map(|v| v.eq_ignore_ascii_case("chunked")).unwrap_or(false) {
            loop {
                let mut sz = String::new();
                if r.read_line(&mut sz).unwrap_or(0) == 0 {
                    return;
                }
                let n = usize::from_str_radix(sz.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
                let mut chunk = vec![0; n + 2];
                if r.read_exact(&mut chunk).is_err() {
                    return;
                }
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
        }
        let (delay, scripted) = {
            let mut i = inner.lock().unwrap();
            (i.delay, i.script.pop_front())
        };
        if !delay.is_zero() {
            std::thread::sleep(delay);
        }
        let (status, extra, resp) = match scripted {
            Some(a) => a,
            None => {
                let raw = if get("content-encoding").map(|v| v.eq_ignore_ascii_case("gzip")).unwrap_or(false) {
                    cameleer_core::dataformat::compress::gzip_decompress(&body).unwrap_or_default()
                } else {
                    body.clone()
                };
                match (path.as_str(), decode_export_request(&raw)) {
                    ("/v1/traces", Ok(spans)) => {
                        inner.lock().unwrap().spans.extend(spans);
                        (200, Vec::new(), Vec::new())
                    }
                    ("/v1/traces", Err(e)) => (400, Vec::new(), e.into_bytes()),
                    _ => (404, Vec::new(), Vec::new()),
                }
            }
        };
        inner.lock().unwrap().requests.push(ReceivedRequest { path, headers: headers.clone(), body, status });
        let mut out = format!("HTTP/1.1 {} X\r\nContent-Type: application/x-protobuf\r\nContent-Length: {}\r\n", status, resp.len());
        for (k, v) in extra {
            out.push_str(&format!("{}: {}\r\n", k, v));
        }
        out.push_str("\r\n");
        if w.write_all(out.as_bytes()).is_err() || w.write_all(&resp).is_err() {
            return;
        }
    }
}
