//! Streaming message bodies shared by the network components.
//!
//! camel-core's `Value` has no stream variant, so a stream body is represented by a small
//! handle object `{"@stream": <id>, "length": <bytes or null>, "source": "..."}` whose reader
//! lives in a process-wide registry. It plays the role of Camel's `InputStream` bodies with
//! stream caching disabled: the reader can be consumed once ([`take`]), e.g. by the `http`
//! producer which then sends it with chunked transfer coding, or materialised into bytes on
//! demand ([`materialize`], [`body_bytes`]). Readers backed by a temporary file delete it when
//! they are dropped. Components that create stream bodies release unconsumed handles when the
//! exchange is done ([`release`]).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use camel_core::value::ObjectMap;
use camel_core::Value;

/// Key of the handle object.
pub const STREAM_KEY: &str = "@stream";

struct Entry {
    reader: Box<dyn Read + Send>,
    length: Option<u64>,
}

fn registry() -> &'static Mutex<HashMap<u64, Entry>> {
    static R: OnceLock<Mutex<HashMap<u64, Entry>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

static NEXT: AtomicU64 = AtomicU64::new(1);

/// Register a reader and return the handle value for a message body.
pub fn register(reader: Box<dyn Read + Send>, length: Option<u64>, source: &str) -> Value {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    registry().lock().unwrap().insert(id, Entry { reader, length });
    let mut m = ObjectMap::new();
    m.insert(STREAM_KEY.to_string(), Value::Int(id as i64));
    m.insert("length".to_string(), length.map(|l| Value::Int(l as i64)).unwrap_or(Value::Null));
    m.insert("source".to_string(), Value::String(source.to_string()));
    Value::Object(m)
}

/// A file reader that deletes the file when dropped (temp-file backed streams).
pub struct TempFileReader {
    file: File,
    path: PathBuf,
    delete: bool,
}

impl TempFileReader {
    /// Open `path`; when `delete` the file is removed once the reader is dropped.
    pub fn open(path: PathBuf, delete: bool) -> io::Result<TempFileReader> {
        Ok(TempFileReader { file: File::open(&path)?, path, delete })
    }
}

impl Read for TempFileReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.file.read(buf)
    }
}

impl Drop for TempFileReader {
    fn drop(&mut self) {
        if self.delete {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Register a file as a stream body (deleted after consumption when `delete`).
pub fn register_file(path: PathBuf, delete: bool, source: &str) -> io::Result<Value> {
    let len = std::fs::metadata(&path)?.len();
    Ok(register(Box::new(TempFileReader::open(path, delete)?), Some(len), source))
}

fn id_of(v: &Value) -> Option<u64> {
    match v {
        Value::Object(m) if m.len() <= 3 => match m.get(STREAM_KEY) {
            Some(Value::Int(i)) if *i > 0 => Some(*i as u64),
            _ => None,
        },
        _ => None,
    }
}

/// Whether the value is a stream handle (registered or already consumed).
pub fn is_stream(v: &Value) -> bool {
    id_of(v).is_some()
}

/// Whether the handle still has its reader.
pub fn is_available(v: &Value) -> bool {
    id_of(v).is_some_and(|id| registry().lock().unwrap().contains_key(&id))
}

/// Take the reader (and its known length) out of the registry; a stream can be read once.
pub fn take(v: &Value) -> Option<(Box<dyn Read + Send>, Option<u64>)> {
    let id = id_of(v)?;
    registry().lock().unwrap().remove(&id).map(|e| (e.reader, e.length))
}

/// Read the whole stream (consuming the handle). `None` if `v` is not an available stream.
pub fn materialize(v: &Value) -> io::Result<Option<Vec<u8>>> {
    match take(v) {
        Some((mut r, len)) => {
            let mut out = Vec::with_capacity(len.unwrap_or(0).min(1 << 24) as usize);
            r.read_to_end(&mut out)?;
            Ok(Some(out))
        }
        None => Ok(None),
    }
}

/// The bytes of a body: a stream handle is materialised, other values converted.
pub fn body_bytes(v: &Value) -> io::Result<Vec<u8>> {
    if is_stream(v) {
        return materialize(v)?.ok_or_else(|| io::Error::other("The stream body was already consumed"));
    }
    Ok(v.to_bytes())
}

/// Drop an unconsumed stream (closing the reader, deleting temp files).
pub fn release(v: &Value) {
    let _ = take(v);
}

/// Number of registered (unconsumed) streams.
pub fn pending() -> usize {
    registry().lock().unwrap().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_take_and_temp_files() {
        let v = register(Box::new(io::Cursor::new(b"hello".to_vec())), Some(5), "test");
        assert!(is_stream(&v) && is_available(&v));
        assert_eq!(v.get("length"), Some(&Value::Int(5)));
        assert_eq!(body_bytes(&v).unwrap(), b"hello");
        assert!(!is_available(&v));
        assert!(body_bytes(&v).is_err());
        assert!(!is_stream(&Value::object([("a", Value::Int(1))])));
        assert_eq!(body_bytes(&Value::from("x")).unwrap(), b"x");
        let dir = std::env::temp_dir().join(format!("camel-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.bin");
        std::fs::write(&p, b"file data").unwrap();
        let v = register_file(p.clone(), true, "file").unwrap();
        assert_eq!(materialize(&v).unwrap().unwrap(), b"file data");
        assert!(!p.exists(), "temp file deleted after consumption");
        std::fs::write(&p, b"x").unwrap();
        let v = register_file(p.clone(), true, "file").unwrap();
        release(&v);
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
