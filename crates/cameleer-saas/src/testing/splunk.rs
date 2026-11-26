//! A Splunk HTTP Event Collector mock: `POST /services/collector/event` (one or more
//! concatenated JSON event objects), `POST /services/collector/raw` and
//! `GET /services/collector/health`, with Splunk's status codes and `{"text","code"}` answers.

use std::sync::{Arc, Mutex};

use cameleer_core::value::Value;
use cameleer_http::{HttpResponse, ServerConfig};

use super::{json, MockServer, Recorded};

/// The HEC mock. Accepts the token [`SplunkHecMock::TOKEN`] and the indexes in `indexes`
/// (default `camel` and `main`), and records each accepted event.
pub struct SplunkHecMock {
    pub server: MockServer,
    events: Arc<Mutex<Vec<Value>>>,
    raw: Arc<Mutex<Vec<String>>>,
    busy: Arc<Mutex<bool>>,
}

fn answer(status: u16, text: &str, code: i64) -> HttpResponse {
    let mut m = cameleer_core::value::ObjectMap::new();
    m.insert("text".into(), Value::from(text));
    m.insert("code".into(), Value::Int(code));
    json(status, &Value::Object(m))
}

/// Splits concatenated JSON objects (`{...}{...}` or one per line), as HEC accepts them.
fn split_events(text: &str) -> Option<Vec<Value>> {
    let mut out = Vec::new();
    let (mut depth, mut in_str, mut esc, mut start) = (0usize, false, false, None);
    for (i, c) in text.char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    out.push(cameleer_core::json::parse(&text[start?..=i]).ok()?);
                }
            }
            c if depth == 0 && !c.is_whitespace() => return None,
            _ => {}
        }
    }
    (depth == 0 && !in_str).then_some(out)
}

impl SplunkHecMock {
    /// The accepted token.
    pub const TOKEN: &'static str = "11111111-2222-3333-4444-555555555555";

    /// Starts the mock over plain HTTP.
    pub fn start() -> SplunkHecMock {
        SplunkHecMock::start_with(ServerConfig::default(), &["camel", "main"])
    }

    /// Starts the mock with a server configuration (`tls` for HTTPS) and the accepted indexes.
    pub fn start_with(config: ServerConfig, indexes: &[&str]) -> SplunkHecMock {
        let events: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let raw: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let busy = Arc::new(Mutex::new(false));
        let indexes: Vec<String> = indexes.iter().map(|s| s.to_string()).collect();
        let (ev, rw, b) = (events.clone(), raw.clone(), busy.clone());
        let server = MockServer::start_with(config, move |r: &Recorded| {
            if r.path == "/services/collector/health" {
                return answer(200, "HEC is healthy", 17);
            }
            if r.method != "POST" || !(r.path == "/services/collector/event" || r.path == "/services/collector/raw" || r.path == "/services/collector") {
                return answer(404, "The requested URL was not found on this server.", 404);
            }
            if *b.lock().unwrap() {
                return answer(503, "Server is busy", 9);
            }
            let Some(auth) = r.header("Authorization") else { return answer(401, "Token is required", 2) };
            let Some(token) = auth.trim().strip_prefix("Splunk ") else { return answer(401, "Invalid authorization", 3) };
            if token.trim() != Self::TOKEN {
                return answer(403, "Invalid token", 4);
            }
            let text = r.text();
            if text.trim().is_empty() {
                return answer(400, "No data", 5);
            }
            if r.path == "/services/collector/raw" {
                rw.lock().unwrap().push(text);
                return answer(200, "Success", 0);
            }
            let Some(parsed) = split_events(&text) else { return answer(400, "Invalid data format", 6) };
            for e in &parsed {
                match e.get("event") {
                    None => return answer(400, "Event field is required", 12),
                    Some(Value::Null) => return answer(400, "Event field cannot be blank", 13),
                    Some(Value::String(s)) if s.is_empty() => return answer(400, "Event field cannot be blank", 13),
                    _ => {}
                }
                if let Some(i) = e.get("index").and_then(|i| i.as_str()) {
                    if !indexes.iter().any(|x| x == i) {
                        return answer(400, "Incorrect index", 7);
                    }
                }
            }
            ev.lock().unwrap().extend(parsed);
            answer(200, "Success", 0)
        });
        SplunkHecMock { server, events, raw, busy }
    }

    /// `host:port` for the `splunk-hec:` URI.
    pub fn splunk_url(&self) -> String {
        format!("localhost:{}", self.server.port())
    }

    /// The accepted events.
    pub fn events(&self) -> Vec<Value> {
        self.events.lock().unwrap().clone()
    }

    /// The bodies accepted on `/services/collector/raw`.
    pub fn raw(&self) -> Vec<String> {
        self.raw.lock().unwrap().clone()
    }

    /// Makes the mock answer 503 "Server is busy".
    pub fn set_busy(&self, busy: bool) {
        *self.busy.lock().unwrap() = busy;
    }
}

#[cfg(test)]
mod tests {
    use super::split_events;

    #[test]
    fn concatenated_events_are_split() {
        assert_eq!(split_events("{\"event\":\"a}\"}\n{\"event\":{\"b\":1}}").unwrap().len(), 2);
        assert!(split_events("{\"event\":1} x").is_none());
        assert!(split_events("{\"event\":1").is_none());
    }
}
