//! Frame transport shared by the client and the embedded broker: a socket with a
//! serialized writer, activity timestamps and an idle-timeout / heartbeat thread.

use std::io::Write;
use std::net::Shutdown;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::ws::AmqpStream;

use crate::types::{Frame, FrameBody};

/// Whether frames are traced to stderr: `CAMEL_AMQP10_TRACE=1` (protocol debugging, e.g.
/// against a real broker). Read once.
pub fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("CAMEL_AMQP10_TRACE").map(|v| !v.is_empty() && v != "0").unwrap_or(false))
}

/// Trace one frame (`dir` is `->` for outgoing, `<-` for incoming) when tracing is on.
pub fn trace_frame(dir: &str, f: &Frame) {
    if trace_enabled() && !matches!(f.body, FrameBody::Empty) {
        eprintln!("[amqp10 {:?}] {} ch{} {:?}", std::thread::current().name().unwrap_or("?"), dir, f.channel, f.body);
    }
}

fn trace_bytes(bytes: &[u8]) {
    let mut cur = std::io::Cursor::new(bytes);
    while (cur.position() as usize) < bytes.len() {
        match crate::types::read_frame(&mut cur, u32::MAX) {
            Ok(Some(f)) => trace_frame("->", &f),
            _ => break,
        }
    }
}

/// A connected socket (plain, TLS or WebSocket) carrying AMQP frames.
pub struct Transport {
    stream: AmqpStream,
    writer: Mutex<AmqpStream>,
    start: Instant,
    last_read: AtomicU64,
    last_write: AtomicU64,
    closed: AtomicBool,
    /// The peer's idle timeout in ms (0 = none): we send a heartbeat every half of it.
    pub remote_idle_ms: AtomicU64,
    /// Our idle timeout in ms (0 = none): the connection fails when nothing arrives for that long.
    pub local_idle_ms: AtomicU64,
}

impl Transport {
    pub fn new(stream: AmqpStream) -> std::io::Result<Arc<Transport>> {
        let writer = stream.try_clone()?;
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        Ok(Arc::new(Transport {
            stream,
            writer: Mutex::new(writer),
            start: Instant::now(),
            last_read: AtomicU64::new(0),
            last_write: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            remote_idle_ms: AtomicU64::new(0),
            local_idle_ms: AtomicU64::new(0),
        }))
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// A clone for a reader thread.
    pub fn reader(&self) -> std::io::Result<AmqpStream> {
        self.stream.try_clone()
    }

    /// The underlying stream.
    pub fn stream(&self) -> &AmqpStream {
        &self.stream
    }

    /// Lock the writer (frames written under one lock are contiguous on the wire).
    pub fn lock_writer(&self) -> MutexGuard<'_, AmqpStream> {
        self.writer.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Write bytes with an already held writer lock.
    pub fn write_locked(&self, w: &mut MutexGuard<'_, AmqpStream>, bytes: &[u8]) -> std::io::Result<()> {
        if self.is_closed() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotConnected, "connection closed"));
        }
        if trace_enabled() {
            trace_bytes(bytes);
        }
        w.write_all(bytes)?;
        w.flush()?;
        self.last_write.store(self.now(), Ordering::Relaxed);
        Ok(())
    }

    /// Write bytes.
    pub fn write_bytes(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut w = self.lock_writer();
        self.write_locked(&mut w, bytes)
    }

    /// Write one frame.
    pub fn write_frame(&self, f: &Frame) -> std::io::Result<()> {
        self.write_bytes(&f.encode())
    }

    /// Record inbound activity.
    pub fn touch_read(&self) {
        self.last_read.store(self.now(), Ordering::Relaxed);
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Close the socket (unblocks the reader).
    pub fn close(&self) {
        if !self.closed.swap(true, Ordering::SeqCst) {
            let _ = self.stream.shutdown(Shutdown::Both);
        }
    }

    /// Start the heartbeat / idle watchdog thread. `on_idle_expired` runs (once) when no
    /// frame arrived within our idle timeout; the thread ends when the transport closes.
    pub fn spawn_heartbeat(self: &Arc<Self>, name: &str, on_idle_expired: impl FnOnce() + Send + 'static) {
        let t = self.clone();
        self.touch_read();
        let _ = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            let mut on_idle = Some(on_idle_expired);
            while !t.is_closed() {
                std::thread::sleep(Duration::from_millis(20));
                let now = t.now();
                let remote = t.remote_idle_ms.load(Ordering::Relaxed);
                if remote > 0 && now.saturating_sub(t.last_write.load(Ordering::Relaxed)) >= remote / 2 {
                    let _ = t.write_frame(&Frame { channel: 0, body: FrameBody::Empty });
                }
                let local = t.local_idle_ms.load(Ordering::Relaxed);
                if local > 0 && now.saturating_sub(t.last_read.load(Ordering::Relaxed)) > local {
                    if let Some(f) = on_idle.take() {
                        f();
                    }
                    t.close();
                }
            }
        });
    }
}
