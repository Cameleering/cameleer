//! `stream:` component - write to / read from standard streams and files.
//!
//! Producers: `stream:out`, `stream:err`, `stream:file?fileName=/path` (appends).
//! Options: `delay` (ms before each write), `encoding` (`UTF-8`/`ISO-8859-1`),
//! `appendNewLine` (default true).
//!
//! Consumers: `stream:in` (standard input, one message per line) and
//! `stream:file?fileName=/path` (one message per line, `scanStream=true` keeps tailing the
//! file every `scanStreamDelay` ms). Options: `delay` (between messages), `encoding`,
//! `groupLines` (N lines per message, joined by `\n`), `initialPromptDelay`,
//! `promptMessage`, `promptDelay`. Headers: `CamelStreamIndex` (0-based), `CamelStreamComplete`.

use std::any::Any;
use std::fs;
use std::io::{BufRead, Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::context::{CamelContext, WeakContext};
use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::spi::{Component, Consumer, Endpoint, Processor, Producer, Service};
use crate::util::Parameters;
use crate::value::Value;

use super::support::{new_exchange, process_consumer_exchange, BackgroundThreads, StopFlag};

pub const STREAM_INDEX: &str = "CamelStreamIndex";
pub const STREAM_COMPLETE: &str = "CamelStreamComplete";

#[derive(Default)]
pub struct StreamComponent;

#[derive(Clone, Debug, PartialEq)]
enum Kind {
    Out,
    Err,
    In,
    File(PathBuf),
}

#[derive(Clone, Debug)]
struct StreamConfig {
    kind: Kind,
    delay: Duration,
    latin1: bool,
    append_new_line: bool,
    scan_stream: bool,
    scan_stream_delay: Duration,
    group_lines: usize,
    initial_prompt_delay: Duration,
    prompt_message: Option<String>,
    prompt_delay: Duration,
}

impl Component for StreamComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        let file_name = params.take("fileName");
        let kind = match remaining {
            "out" => Kind::Out,
            "err" => Kind::Err,
            "in" => Kind::In,
            "file" => Kind::File(PathBuf::from(file_name.ok_or_else(|| CamelError::illegal_argument("fileName option is required for stream:file"))?)),
            other => return Err(CamelError::illegal_argument(format!("Unknown stream kind: {} (use out, err, in or file)", other))),
        };
        let encoding = params.take("encoding").unwrap_or_else(|| "UTF-8".into()).to_ascii_uppercase();
        let latin1 = match encoding.as_str() {
            "UTF-8" | "UTF8" => false,
            "ISO-8859-1" | "LATIN1" | "ISO8859_1" => true,
            other => return Err(CamelError::illegal_argument(format!("Unsupported encoding: {}", other))),
        };
        let config = StreamConfig {
            kind,
            delay: params.take_duration("delay", Duration::ZERO)?,
            latin1,
            append_new_line: params.take_bool("appendNewLine", true)?,
            scan_stream: params.take_bool("scanStream", false)?,
            scan_stream_delay: params.take_duration("scanStreamDelay", Duration::from_millis(100))?.max(Duration::from_millis(1)),
            group_lines: params.take_usize("groupLines", 0)?,
            initial_prompt_delay: params.take_duration("initialPromptDelay", Duration::from_millis(2000))?,
            prompt_message: params.take("promptMessage"),
            prompt_delay: params.take_duration("promptDelay", Duration::ZERO)?,
        };
        for k in ["closeOnDone", "autoCloseCount", "retry", "fileWatcher", "readTimeout", "readLine", "groupStrategy", "lazyStartProducer", "bridgeErrorHandler", "readTimeout"] {
            params.take(k);
        }
        Ok(Arc::new(StreamEndpoint { uri: uri.to_string(), ctx: ctx.downgrade(), config }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct StreamEndpoint {
    uri: String,
    ctx: WeakContext,
    config: StreamConfig,
}

impl Endpoint for StreamEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        if self.config.kind == Kind::In {
            return Err(CamelError::new(crate::error::kinds::UNSUPPORTED_OPERATION, "stream:in does not support producers"));
        }
        Ok(Arc::new(StreamProducer { config: self.config.clone(), lock: Mutex::new(()), stop: StopFlag::new() }))
    }
    fn create_consumer(&self, processor: Arc<dyn Processor>) -> Result<Arc<dyn Consumer>> {
        if matches!(self.config.kind, Kind::Out | Kind::Err) {
            return Err(CamelError::new(crate::error::kinds::UNSUPPORTED_OPERATION, "stream:out/err do not support consumers"));
        }
        Ok(Arc::new(StreamConsumer { uri: self.uri.clone(), config: self.config.clone(), ctx: self.ctx.clone(), processor, threads: BackgroundThreads::new() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct StreamProducer {
    config: StreamConfig,
    lock: Mutex<()>,
    stop: Arc<StopFlag>,
}

impl Service for StreamProducer {
    fn start(&self) -> Result<()> {
        self.stop.reset();
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.stop.stop();
        Ok(())
    }
}

fn encode(v: &Value, latin1: bool) -> Vec<u8> {
    match v {
        Value::String(s) if latin1 => s.chars().map(|c| if (c as u32) < 256 { c as u8 } else { b'?' }).collect(),
        other => other.to_bytes(),
    }
}

fn decode(bytes: &[u8], latin1: bool) -> String {
    if latin1 {
        bytes.iter().map(|&b| b as char).collect()
    } else {
        String::from_utf8_lossy(bytes).to_string()
    }
}

impl Processor for StreamProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        if !self.config.delay.is_zero() {
            self.stop.sleep(self.config.delay);
        }
        let mut data = encode(ex.body(), self.config.latin1);
        if self.config.append_new_line {
            data.push(b'\n');
        }
        let _g = self.lock.lock().unwrap();
        let r = match &self.config.kind {
            Kind::Out => {
                let mut o = std::io::stdout().lock();
                o.write_all(&data).and_then(|_| o.flush())
            }
            Kind::Err => {
                let mut o = std::io::stderr().lock();
                o.write_all(&data).and_then(|_| o.flush())
            }
            Kind::File(p) => {
                if let Some(parent) = p.parent().filter(|p| !p.as_os_str().is_empty()) {
                    let _ = fs::create_dir_all(parent);
                }
                fs::OpenOptions::new().create(true).append(true).open(p).and_then(|mut f| f.write_all(&data))
            }
            Kind::In => unreachable!(),
        };
        r.map_err(|e| CamelError::io(format!("Cannot write to stream: {}", e)))
    }
}

struct StreamConsumer {
    uri: String,
    config: StreamConfig,
    ctx: WeakContext,
    processor: Arc<dyn Processor>,
    threads: BackgroundThreads,
}

/// Process-wide stdin line reader (a blocking stdin read cannot be interrupted, so a
/// single detached reader thread feeds lines to whichever consumer is active).
fn stdin_lines() -> &'static Mutex<Receiver<Vec<u8>>> {
    static RX: OnceLock<Mutex<Receiver<Vec<u8>>>> = OnceLock::new();
    RX.get_or_init(|| {
        let (tx, rx) = mpsc::channel();
        let _ = std::thread::Builder::new().name("Camel stream:in reader".into()).spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            loop {
                let mut line = Vec::new();
                match lock.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
                            line.pop();
                        }
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        Mutex::new(rx)
    })
}

struct LineEmitter<'a> {
    config: &'a StreamConfig,
    ctx: &'a WeakContext,
    processor: &'a dyn Processor,
    logger: &'a str,
    index: u64,
    group: Vec<String>,
    flag: &'a StopFlag,
}

impl LineEmitter<'_> {
    fn emit(&mut self, body: String, complete: bool) {
        let mut ex = new_exchange(self.ctx);
        ex.set_body(body);
        ex.set_header(STREAM_INDEX, Value::from(self.index));
        ex.set_header(STREAM_COMPLETE, complete);
        self.index += 1;
        process_consumer_exchange(self.logger, self.processor, &mut ex);
        if !self.config.delay.is_zero() {
            self.flag.sleep(self.config.delay);
        }
    }
    fn line(&mut self, line: String, last: bool) {
        if self.config.group_lines > 0 {
            self.group.push(line);
            if self.group.len() >= self.config.group_lines || last {
                let body = std::mem::take(&mut self.group).join("\n");
                self.emit(body, last);
            }
        } else {
            self.emit(line, last);
        }
    }
    fn flush(&mut self) {
        if !self.group.is_empty() {
            let body = std::mem::take(&mut self.group).join("\n");
            self.emit(body, true);
        }
    }
}

fn run_consumer(config: &StreamConfig, ctx: &WeakContext, processor: &dyn Processor, logger: &str, flag: &StopFlag) {
    let mut em = LineEmitter { config, ctx, processor, logger, index: 0, group: Vec::new(), flag };
    match &config.kind {
        Kind::In => {
            if let Some(p) = &config.prompt_message {
                if flag.sleep(config.initial_prompt_delay) {
                    return;
                }
                print!("{}", p);
                let _ = std::io::stdout().flush();
            }
            let rx = stdin_lines();
            while !flag.is_stopped() {
                let r = rx.lock().unwrap().recv_timeout(Duration::from_millis(20));
                match r {
                    Ok(line) => {
                        em.line(decode(&line, config.latin1), false);
                        if let Some(p) = &config.prompt_message {
                            if flag.sleep(config.prompt_delay) {
                                break;
                            }
                            print!("{}", p);
                            let _ = std::io::stdout().flush();
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            em.flush();
        }
        Kind::File(path) => {
            let mut pos: u64 = 0;
            let mut pending: Vec<u8> = Vec::new();
            loop {
                if flag.is_stopped() {
                    break;
                }
                let mut chunk = Vec::new();
                if let Ok(mut f) = fs::File::open(path) {
                    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
                    if len < pos {
                        pos = 0; // truncated/rotated
                    }
                    if f.seek(SeekFrom::Start(pos)).is_ok() {
                        let _ = f.read_to_end(&mut chunk);
                        pos += chunk.len() as u64;
                    }
                } else if !config.scan_stream {
                    crate::camel_warn!(logger, "Cannot open file {}", path.display());
                    break;
                }
                pending.extend_from_slice(&chunk);
                let mut lines: Vec<String> = Vec::new();
                while let Some(i) = pending.iter().position(|&b| b == b'\n') {
                    let mut l: Vec<u8> = pending.drain(..=i).collect();
                    l.pop();
                    if l.last() == Some(&b'\r') {
                        l.pop();
                    }
                    lines.push(decode(&l, config.latin1));
                }
                if !config.scan_stream && !pending.is_empty() {
                    lines.push(decode(&std::mem::take(&mut pending), config.latin1));
                }
                let n = lines.len();
                for (i, l) in lines.into_iter().enumerate() {
                    if flag.is_stopped() {
                        return;
                    }
                    em.line(l, !config.scan_stream && i + 1 == n);
                }
                if !config.scan_stream {
                    break;
                }
                if flag.sleep(config.scan_stream_delay) {
                    break;
                }
            }
            em.flush();
        }
        _ => {}
    }
}

impl Service for StreamConsumer {
    fn start(&self) -> Result<()> {
        let (config, ctx, processor, logger) = (self.config.clone(), self.ctx.clone(), self.processor.clone(), self.uri.clone());
        self.threads.spawn(&format!("Camel {}", self.uri), move |flag| run_consumer(&config, &ctx, processor.as_ref(), &logger, &flag))
    }
    fn stop(&self) -> Result<()> {
        self.threads.stop();
        Ok(())
    }
}
