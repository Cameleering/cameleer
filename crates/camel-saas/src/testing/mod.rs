//! Embedded mock servers for the SaaS connectors, so tests never reach the internet.
//!
//! Each mock runs an HTTP server on an ephemeral localhost port, validates credentials and
//! request formats like the real API, keeps state in memory and records requests. Point
//! an endpoint at it with the endpoint-override option (`serverUrl`, `baseUri`, `baseUrl`,
//! `jiraUrl`, `webhookUrl`...) set to [`MockServer::url`].
//!
//! | mock | API subset |
//! |---|---|
//! | [`SlackMock`] | incoming webhook, `chat.postMessage`, `conversations.list`, `conversations.history` |
//! | [`TelegramMock`] | `sendMessage`, `sendPhoto`/`sendDocument` (JSON and multipart), `getUpdates`, `setWebhook`, `deleteWebhook` |
//! | [`GithubMock`] | pulls, issues (+ comments), commits, tags, statuses, hooks |
//! | [`TwilioMock`] | `Messages.json` create / fetch / list |
//! | [`JiraMock`] | REST v2 issue create / delete, comment, search |
//! | [`OpenAiMock`] | `chat/completions`, `embeddings` |
//! | [`GraphqlMock`] | a GraphQL endpoint with a tiny books schema |
//! | [`FeedMock`] | RSS / Atom documents |

mod services;

use std::sync::{Arc, Mutex};

use camel_core::value::Value;
use camel_http::{HttpRequest, HttpResponse, HttpServer, ServerConfig};

pub use services::*;

/// A recorded request.
#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn query_param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Body parsed as JSON (Null when not JSON).
    pub fn json(&self) -> Value {
        camel_core::json::parse(&self.text()).unwrap_or(Value::Null)
    }

    /// Body parsed as `application/x-www-form-urlencoded`.
    pub fn form(&self) -> Vec<(String, String)> {
        self.text()
            .split('&')
            .filter(|p| !p.is_empty())
            .map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (camel_core::util::url_decode(&k.replace('+', " ")), camel_core::util::url_decode(&v.replace('+', " ")))
            })
            .collect()
    }

    pub fn form_param(&self, name: &str) -> Option<String> {
        self.form().into_iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }

    /// Multipart parts as `(name, filename, data)`.
    pub fn multipart(&self) -> Vec<(String, Option<String>, Vec<u8>)> {
        let ct = self.header("Content-Type").unwrap_or("");
        let Some(b) = ct.split("boundary=").nth(1) else { return Vec::new() };
        let boundary = format!("--{}", b.trim_matches('"'));
        let body = &self.body;
        let mut parts = Vec::new();
        let positions: Vec<usize> = find_all(body, boundary.as_bytes());
        for w in positions.windows(2) {
            let seg = &body[w[0] + boundary.len()..w[1]];
            let seg = seg.strip_prefix(b"\r\n").unwrap_or(seg);
            let Some(hend) = find_all(seg, b"\r\n\r\n").first().copied() else { continue };
            let head = String::from_utf8_lossy(&seg[..hend]).into_owned();
            let mut data = seg[hend + 4..].to_vec();
            if data.ends_with(b"\r\n") {
                data.truncate(data.len() - 2);
            }
            let attr = |n: &str| head.split(';').map(|s| s.trim()).find_map(|s| s.strip_prefix(&format!("{}=", n)).map(|v| v.trim_matches('"').to_string()));
            parts.push((attr("name").unwrap_or_default(), attr("filename"), data));
        }
        parts
    }
}

fn find_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut v = Vec::new();
    if needle.is_empty() || hay.len() < needle.len() {
        return v;
    }
    for i in 0..=hay.len() - needle.len() {
        if &hay[i..i + needle.len()] == needle {
            v.push(i);
        }
    }
    v
}

type Handler = dyn Fn(&Recorded) -> HttpResponse + Send + Sync;

/// A generic mock HTTP server recording every request.
pub struct MockServer {
    server: HttpServer,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl MockServer {
    /// Start a server on `127.0.0.1` with an ephemeral port.
    pub fn start(handler: impl Fn(&Recorded) -> HttpResponse + Send + Sync + 'static) -> MockServer {
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::new(Mutex::new(Vec::new()));
        let reqs = requests.clone();
        let h: Arc<Handler> = Arc::new(handler);
        let server = HttpServer::bind("127.0.0.1:0", ServerConfig::default(), move |r: &mut HttpRequest| {
            let rec = Recorded {
                method: r.method.clone(),
                path: r.path.clone(),
                query: r.query_params(),
                headers: r.headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
                body: r.body.clone(),
            };
            reqs.lock().unwrap().push(rec.clone());
            h(&rec)
        })
        .expect("bind mock server");
        MockServer { server, requests }
    }

    /// Base URL (`http://127.0.0.1:port`).
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.server.port())
    }

    pub fn port(&self) -> u16 {
        self.server.port()
    }

    /// All requests received so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }

    /// Requests to a path.
    pub fn requests_to(&self, path: &str) -> Vec<Recorded> {
        self.requests().into_iter().filter(|r| r.path == path).collect()
    }

    pub fn stop(&self) {
        self.server.stop();
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.server.stop();
    }
}

/// JSON response helper.
pub fn json(status: u16, v: &Value) -> HttpResponse {
    HttpResponse::json(status, v.to_json())
}

/// Bearer token of a request.
pub fn bearer(r: &Recorded) -> Option<String> {
    r.header("Authorization").and_then(|a| a.strip_prefix("Bearer ").or_else(|| a.strip_prefix("token "))).map(|s| s.to_string())
}

/// Basic credentials `(user, password)` of a request.
pub fn basic(r: &Recorded) -> Option<(String, String)> {
    let b = r.header("Authorization")?.strip_prefix("Basic ")?;
    let decoded = decode_base64(b)?;
    let s = String::from_utf8(decoded).ok()?;
    let (u, p) = s.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let bytes: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// A free local TCP port (for embedded consumers such as the webhook component).
pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}
