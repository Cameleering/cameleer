//! Reliability and cost options of a model endpoint, applied in-process around every model
//! call: retries of transient failures, fallback models, an exact response cache and
//! single-flight coalescing of identical concurrent requests.
//!
//! | option | default | |
//! |---|---|---|
//! | `maxRetries` | `2` | retries of 429, 5xx, 529, timeouts and resets, with exponential backoff |
//! | `retryDelay` | `500ms` | the first backoff (doubled each retry, at most 10 s) |
//! | `fallbackModels` | | endpoint URIs tried in order when the model still fails (encode `&` as `%26`) |
//! | `responseCache` | `off` | `exact`: identical requests are answered from memory |
//! | `responseCacheTtl` | `5m` | how long a cached answer is used |
//! | `responseCacheSize` | `1000` | at most this many answers are kept |
//! | `coalesce` | `false` | identical requests in flight at the same time share one call |
//!
//! A retried or coalesced model turn never repeats a tool call: tools run in the `ai-tool`
//! envelope after the model answers.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::{ChatModel, ChatRequest, ChatResponse, Usage};
use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};

/// The reliability options of a model endpoint. Clones share the cache.
#[derive(Clone, Default)]
pub struct Reliability {
    pub max_retries: u32,
    pub retry_delay: Duration,
    pub fallbacks: Vec<String>,
    pub cache_ttl: Option<Duration>,
    pub cache_size: usize,
    pub coalesce: bool,
    ctx: Option<WeakContext>,
    state: Arc<State>,
}

impl fmt::Debug for Reliability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reliability")
            .field("max_retries", &self.max_retries)
            .field("retry_delay", &self.retry_delay)
            .field("fallbacks", &self.fallbacks)
            .field("cache_ttl", &self.cache_ttl)
            .field("coalesce", &self.coalesce)
            .finish()
    }
}

type Flight = Arc<(Mutex<Option<Result<ChatResponse>>>, Condvar)>;

#[derive(Default)]
struct State {
    cache: Mutex<HashMap<[u8; 32], (Instant, ChatResponse)>>,
    inflight: Mutex<HashMap<[u8; 32], Flight>>,
}

impl Reliability {
    /// Take the options from endpoint parameters.
    pub fn take(params: &mut crate::util::Parameters) -> Result<Reliability> {
        let max_retries = params.take_usize("maxRetries", 2)? as u32;
        let retry_delay = params.take_duration("retryDelay", Duration::from_millis(500))?;
        let fallbacks = params.take("fallbackModels").map(|f| f.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default();
        let cache = match params.take_string("responseCache", "off").to_ascii_lowercase().as_str() {
            "off" | "none" | "false" => false,
            "exact" | "true" | "on" => true,
            other => return Err(CamelError::illegal_argument(format!("responseCache must be off or exact, got {}", other))),
        };
        let ttl = params.take_duration("responseCacheTtl", Duration::from_secs(300))?;
        let cache_size = params.take_usize("responseCacheSize", 1000)?.max(1);
        let coalesce = params.take_bool("coalesce", false)?;
        Ok(Reliability { max_retries, retry_delay, fallbacks, cache_ttl: if cache && !ttl.is_zero() { Some(ttl) } else { None }, cache_size, coalesce, ctx: None, state: Arc::default() })
    }

    /// The context that resolves `fallbackModels`.
    pub fn with_context(mut self, ctx: &CamelContext) -> Reliability {
        self.ctx = Some(ctx.downgrade());
        self
    }

    /// The same options without retries (for a model whose transport already retries).
    pub fn without_retries(&self) -> Reliability {
        Reliability { max_retries: 0, ..self.clone() }
    }

    /// Whether anything beyond retries is configured.
    pub fn needs_wrapper(&self) -> bool {
        !self.fallbacks.is_empty() || self.cache_ttl.is_some() || self.coalesce
    }

    /// Wrap a model; `id` tells apart models that share these options (the model name).
    pub fn wrap(&self, model: Arc<dyn ChatModel>, id: &str) -> Arc<dyn ChatModel> {
        if self.max_retries == 0 && !self.needs_wrapper() {
            return model;
        }
        Arc::new(ReliableModel { inner: model, opts: self.clone(), id: id.to_string() })
    }

    /// Run `f`, retrying transient failures with exponential backoff.
    pub fn retry<T>(&self, mut f: impl FnMut() -> Result<T>) -> Result<T> {
        let mut delay = self.retry_delay;
        let mut attempt = 0;
        loop {
            match f() {
                Err(e) if attempt < self.max_retries && is_transient(&e) => {
                    attempt += 1;
                    crate::camel_debug!("ai", "model call failed ({}), retry {} of {} in {:?}", e.message, attempt, self.max_retries, delay);
                    std::thread::sleep(delay);
                    delay = (delay * 2).min(Duration::from_secs(10));
                }
                r => return r,
            }
        }
    }

    fn key(&self, id: &str, req: &ChatRequest) -> [u8; 32] {
        crate::language::support::sha256(format!("{}\u{0}{:?}", id, req).as_bytes())
    }

    fn cached(&self, key: &[u8; 32]) -> Option<ChatResponse> {
        let ttl = self.cache_ttl?;
        let c = self.state.cache.lock().unwrap_or_else(|p| p.into_inner());
        c.get(key).filter(|(at, _)| at.elapsed() < ttl).map(|(_, r)| ChatResponse { usage: Usage::default(), ..r.clone() })
    }

    fn store(&self, key: [u8; 32], resp: &ChatResponse) {
        let Some(ttl) = self.cache_ttl else { return };
        let mut c = self.state.cache.lock().unwrap_or_else(|p| p.into_inner());
        if c.len() >= self.cache_size {
            c.retain(|_, (at, _)| at.elapsed() < ttl);
            if c.len() >= self.cache_size {
                if let Some(oldest) = c.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| *k) {
                    c.remove(&oldest);
                }
            }
        }
        c.insert(key, (Instant::now(), resp.clone()));
    }

    fn call(&self, inner: &dyn ChatModel, req: &ChatRequest) -> Result<ChatResponse> {
        let first = self.retry(|| inner.chat(req));
        let Err(mut err) = first else { return first };
        for uri in &self.fallbacks {
            let model = match self.fallback(uri) {
                Ok(m) => m,
                Err(e) => {
                    crate::camel_warn!("ai", "fallback model {} is not usable: {}", uri, e.message);
                    continue;
                }
            };
            crate::camel_info!("ai", "model call failed ({}), falling back to {}", err.message, uri);
            match self.retry(|| model.chat(req)) {
                Ok(r) => return Ok(r),
                Err(e) => err = e,
            }
        }
        Err(err)
    }

    fn fallback(&self, uri: &str) -> Result<Arc<dyn ChatModel>> {
        let ctx = self.ctx.as_ref().and_then(|c| c.upgrade()).ok_or_else(|| CamelError::illegal_state("fallbackModels need a context"))?;
        ctx.get_endpoint(uri)?.chat_model().ok_or_else(|| CamelError::illegal_argument(format!("{} is not a chat model", uri)))
    }
}

/// 429, 5xx, 529, timeouts, resets and throttling messages.
pub fn is_transient(e: &CamelError) -> bool {
    let m = e.message.to_ascii_lowercase();
    for marker in ["statuscode: ", "status code ", "status: ", "http "] {
        if let Some(i) = m.find(marker) {
            let code: String = m[i + marker.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(c) = code.parse::<u16>() {
                return c == 408 || c == 409 || c == 425 || c == 429 || (500..600).contains(&c);
            }
        }
    }
    ["timed out", "timeout", "connection reset", "broken pipe", "unexpected eof", "throttl", "overloaded", "rate limit", "too many requests"].iter().any(|k| m.contains(k))
}

struct ReliableModel {
    inner: Arc<dyn ChatModel>,
    opts: Reliability,
    id: String,
}

impl ChatModel for ReliableModel {
    fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
        let o = &self.opts;
        if o.cache_ttl.is_none() && !o.coalesce {
            return o.call(self.inner.as_ref(), req);
        }
        let key = o.key(&self.id, req);
        if let Some(r) = o.cached(&key) {
            return Ok(r);
        }
        if !o.coalesce {
            let r = o.call(self.inner.as_ref(), req)?;
            o.store(key, &r);
            return Ok(r);
        }
        // single flight: the first caller calls the model, the others wait for its answer
        let (flight, leader) = {
            let mut m = o.state.inflight.lock().unwrap_or_else(|p| p.into_inner());
            match m.get(&key) {
                Some(f) => (f.clone(), false),
                None => {
                    let f: Flight = Arc::default();
                    m.insert(key, f.clone());
                    (f, true)
                }
            }
        };
        if leader {
            let r = o.call(self.inner.as_ref(), req);
            if let Ok(resp) = &r {
                o.store(key, resp);
            }
            o.state.inflight.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
            let (lock, cv) = &*flight;
            *lock.lock().unwrap_or_else(|p| p.into_inner()) = Some(r.clone());
            cv.notify_all();
            return r;
        }
        let (lock, cv) = &*flight;
        let mut g = lock.lock().unwrap_or_else(|p| p.into_inner());
        while g.is_none() {
            g = cv.wait(g).unwrap_or_else(|p| p.into_inner());
        }
        // the leader's call was paid once; followers report no tokens
        g.clone().expect("answer").map(|r| ChatResponse { usage: Usage::default(), ..r })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::ChatMessage;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Flaky {
        calls: AtomicUsize,
        fail_first: usize,
        error: &'static str,
        delay: Duration,
    }
    impl ChatModel for Flaky {
        fn chat(&self, req: &ChatRequest) -> Result<ChatResponse> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(self.delay);
            if n < self.fail_first {
                return Err(CamelError::illegal_state(self.error));
            }
            Ok(ChatResponse { message: ChatMessage::assistant(format!("a:{}", req.messages[0].text())), stop_reason: "stop".into(), usage: Usage::new(5, 1) })
        }
    }

    fn flaky(fail_first: usize, error: &'static str) -> Arc<Flaky> {
        Arc::new(Flaky { calls: AtomicUsize::new(0), fail_first, error, delay: Duration::ZERO })
    }

    fn req(t: &str) -> ChatRequest {
        ChatRequest { messages: vec![ChatMessage::user(t)], ..ChatRequest::default() }
    }

    fn opts(q: &str) -> Reliability {
        let mut p = crate::util::Parameters::new(crate::util::parse_query(q));
        Reliability::take(&mut p).unwrap()
    }

    #[test]
    fn retries_transient_failures_only() {
        let m = flaky(2, "HTTP operation failed invoking X with statusCode: 429: slow down");
        let r = opts("retryDelay=1ms").wrap(m.clone(), "x");
        assert_eq!(r.chat(&req("q")).unwrap().message.text(), "a:q");
        assert_eq!(m.calls.load(Ordering::SeqCst), 3);
        // a client error is not retried
        let m = flaky(1, "HTTP operation failed invoking X with statusCode: 400: bad");
        assert!(opts("retryDelay=1ms").wrap(m.clone(), "x").chat(&req("q")).is_err());
        assert_eq!(m.calls.load(Ordering::SeqCst), 1);
        // maxRetries=0 leaves the model as it is
        let m = flaky(1, "statusCode: 503");
        assert!(opts("maxRetries=0").wrap(m.clone(), "x").chat(&req("q")).is_err());
        assert!(is_transient(&CamelError::illegal_state("Read timed out")));
        assert!(!is_transient(&CamelError::illegal_state("statusCode: 401")));
    }

    #[test]
    fn caches_exact_requests_without_charging_tokens() {
        let m = flaky(0, "");
        let r = opts("responseCache=exact").wrap(m.clone(), "x");
        assert_eq!(r.chat(&req("q")).unwrap().usage, Usage::new(5, 1));
        let again = r.chat(&req("q")).unwrap();
        assert_eq!((again.message.text().as_str(), again.usage), ("a:q", Usage::default()));
        r.chat(&req("other")).unwrap();
        assert_eq!(m.calls.load(Ordering::SeqCst), 2);
        // another model id does not share answers
        opts("responseCache=exact").wrap(m.clone(), "y").chat(&req("q")).unwrap();
        assert_eq!(m.calls.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn coalesces_identical_concurrent_requests() {
        let m = Arc::new(Flaky { calls: AtomicUsize::new(0), fail_first: 0, error: "", delay: Duration::from_millis(150) });
        let r = opts("coalesce=true").wrap(m.clone(), "x");
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let r = r.clone();
                std::thread::spawn(move || r.chat(&req("same")).unwrap())
            })
            .collect();
        let answers: Vec<ChatResponse> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(answers.iter().all(|a| a.message.text() == "a:same"));
        assert_eq!(m.calls.load(Ordering::SeqCst), 1, "one call for eight callers");
        assert_eq!(answers.iter().filter(|a| a.usage.input > 0).count(), 1, "tokens are reported once");
    }
}
