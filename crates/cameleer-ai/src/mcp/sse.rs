//! Server-sent events over a streamed HTTP response body.
//!
//! An [`SseStream`] is the writing end; [`SseStream::response_body`] registers the reading end
//! as a `cameleer_tls::stream_body` handle that the HTTP server writes with chunked transfer
//! coding as events arrive. When the client goes away the server drops the reader, and the
//! next [`SseStream::send`] returns false.

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use cameleer_core::value::Value;

/// The writing end of an event stream.
#[derive(Clone)]
pub struct SseStream {
    tx: Arc<Mutex<Option<SyncSender<Vec<u8>>>>>,
    open: Arc<AtomicBool>,
}

struct ChannelReader {
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
    open: Arc<AtomicBool>,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.pending.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0), // every sender is gone: end of the stream
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Drop for ChannelReader {
    fn drop(&mut self) {
        self.open.store(false, Ordering::SeqCst);
    }
}

impl SseStream {
    /// A new stream and the response body handle that reads it.
    pub fn new() -> (SseStream, Value) {
        let (tx, rx) = sync_channel::<Vec<u8>>(256);
        let open = Arc::new(AtomicBool::new(true));
        let reader = ChannelReader { rx, pending: Vec::new(), pos: 0, open: open.clone() };
        let handle = cameleer_tls::stream_body::register(Box::new(reader), None, "mcp-sse");
        (SseStream { tx: Arc::new(Mutex::new(Some(tx))), open }, handle)
    }

    fn push(&self, bytes: Vec<u8>) -> bool {
        if !self.open.load(Ordering::SeqCst) {
            return false;
        }
        let guard = self.tx.lock().unwrap_or_else(|p| p.into_inner());
        let Some(tx) = guard.as_ref() else { return false };
        match tx.try_send(bytes) {
            Ok(()) => true,
            // a client that stopped reading: wait a little, then give up on it
            Err(TrySendError::Full(b)) => {
                let tx = tx.clone();
                drop(guard);
                std::thread::sleep(Duration::from_millis(50));
                tx.try_send(b).is_ok()
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Send one JSON-RPC message as a `message` event.
    pub fn send(&self, msg: &Value) -> bool {
        self.push(format!("event: message\ndata: {}\n\n", msg.to_json()).into_bytes())
    }

    /// Send a comment line (keep-alive).
    pub fn comment(&self, text: &str) -> bool {
        self.push(format!(": {}\n\n", text).into_bytes())
    }

    /// Whether the client is still reading.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// End the stream (the response finishes once the reader drained it).
    pub fn close(&self) {
        self.tx.lock().unwrap_or_else(|p| p.into_inner()).take();
    }

    /// Send keep-alive comments every `interval` until the stream closes.
    pub fn keep_alive(&self, interval: Duration) {
        if interval.is_zero() {
            return;
        }
        let me = self.clone();
        let _ = std::thread::Builder::new().name("mcp-sse-keepalive".into()).spawn(move || loop {
            std::thread::sleep(interval);
            if me.tx.lock().unwrap_or_else(|p| p.into_inner()).is_none() || !me.comment("keep-alive") {
                return;
            }
        });
    }
}
