//! S3 multipart uploads, streaming (file based) uploads and downloads, and Camel's
//! `streamingUploadMode` (many exchanges aggregated into multipart objects), used by the
//! `aws2-s3` component.

use std::io::Read;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use camel_core::error::{CamelError, Result};

use super::AwsConfig;
use crate::http::{Request, Response, Url};
use crate::util::{kinds, uri_encode, xml_escape, Xml};

/// Camel's default part size (25 MiB).
pub const DEFAULT_PART_SIZE: i64 = 25 * 1024 * 1024;

/// The bucket/key addressing plus the signed transport of an S3 endpoint.
#[derive(Clone, Debug)]
pub struct S3Client {
    pub aws: AwsConfig,
    pub force_path_style: bool,
    /// Headers sent with every `UploadPart` / download (SSE-C customer key headers).
    pub part_headers: Vec<(String, String)>,
}

impl S3Client {
    fn path_style(&self, bucket: &str) -> bool {
        self.force_path_style || self.aws.is_overridden() || bucket.contains('.')
    }

    /// URL of a bucket or object; `query` is an already encoded query string.
    pub fn url(&self, bucket: &str, key: Option<&str>, query: Option<String>) -> Result<Url> {
        let mut u = self.aws.endpoint("s3")?;
        let key_path = key.map(|k| format!("/{}", uri_encode(k, true))).unwrap_or_default();
        let base = u.path.trim_end_matches('/').to_string();
        if bucket.is_empty() {
            u.path = if base.is_empty() { "/".into() } else { base };
        } else if self.path_style(bucket) {
            u.path = format!("{}/{}{}", base, uri_encode(bucket, false), key_path);
        } else {
            u.host = format!("{}.{}", bucket, u.host);
            u.path = if key_path.is_empty() { "/".into() } else { key_path };
        }
        u.query = query.filter(|q| !q.is_empty());
        Ok(u)
    }

    pub fn send(&self, req: Request) -> Result<Response> {
        self.aws.send(req, "s3", kinds::S3)
    }

    /// `CreateMultipartUpload`; returns the upload id.
    pub fn create_multipart(&self, bucket: &str, key: &str, content_type: Option<&str>, extra: &[(String, String)]) -> Result<String> {
        let mut req = Request::new("POST", self.url(bucket, Some(key), Some("uploads".into()))?);
        req.set_header("Content-Type", content_type.unwrap_or("application/octet-stream"));
        for (k, v) in extra {
            req.set_header(k, v.clone());
        }
        let resp = self.send(req)?;
        Xml::parse(&resp.text())?.find_text("UploadId").filter(|u| !u.is_empty()).ok_or_else(|| crate::util::cloud_error(kinds::S3, "CreateMultipartUpload response without UploadId"))
    }

    /// `UploadPart`; returns the part ETag.
    pub fn upload_part(&self, bucket: &str, key: &str, upload_id: &str, part: i32, data: Vec<u8>) -> Result<String> {
        let q = format!("partNumber={}&uploadId={}", part, uri_encode(upload_id, false));
        let mut req = Request::new("PUT", self.url(bucket, Some(key), Some(q))?).body(data);
        for (k, v) in &self.part_headers {
            req.set_header(k, v.clone());
        }
        let resp = self.send(req)?;
        Ok(resp.header("ETag").unwrap_or("").to_string())
    }

    /// `CompleteMultipartUpload`; returns the object ETag.
    pub fn complete_multipart(&self, bucket: &str, key: &str, upload_id: &str, parts: &[(i32, String)]) -> Result<String> {
        let mut xml = String::from("<CompleteMultipartUpload xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
        for (n, etag) in parts {
            xml.push_str(&format!("<Part><PartNumber>{}</PartNumber><ETag>{}</ETag></Part>", n, xml_escape(etag)));
        }
        xml.push_str("</CompleteMultipartUpload>");
        let q = format!("uploadId={}", uri_encode(upload_id, false));
        let resp = self.send(Request::new("POST", self.url(bucket, Some(key), Some(q))?).header("Content-Type", "application/xml").body(xml.into_bytes()))?;
        let x = Xml::parse(&resp.text())?;
        if x.name(x.root()) == "Error" {
            return Err(crate::util::cloud_error(kinds::S3, format!("CompleteMultipartUpload failed: {} (Service: s3, Status Code: 200, Error Code: {})", x.find_text("Message").unwrap_or_default(), x.find_text("Code").unwrap_or_default())));
        }
        Ok(x.find_text("ETag").unwrap_or_default())
    }

    /// `AbortMultipartUpload`.
    pub fn abort_multipart(&self, bucket: &str, key: &str, upload_id: &str) -> Result<()> {
        let q = format!("uploadId={}", uri_encode(upload_id, false));
        self.send(Request::new("DELETE", self.url(bucket, Some(key), Some(q))?))?;
        Ok(())
    }

    /// Upload `next_part` chunks (each at most `part_size` bytes) as one multipart object;
    /// aborts the upload on failure. Returns the ETag.
    pub fn multipart_upload(
        &self,
        bucket: &str,
        key: &str,
        content_type: Option<&str>,
        extra: &[(String, String)],
        part_size: usize,
        next_part: &mut dyn FnMut(usize) -> Result<Option<Vec<u8>>>,
    ) -> Result<String> {
        let upload_id = self.create_multipart(bucket, key, content_type, extra)?;
        let mut run = || -> Result<String> {
            let mut parts = Vec::new();
            while let Some(chunk) = next_part(part_size)? {
                let n = parts.len() as i32 + 1;
                parts.push((n, self.upload_part(bucket, key, &upload_id, n, chunk)?));
            }
            self.complete_multipart(bucket, key, &upload_id, &parts)
        };
        match run() {
            Ok(etag) => Ok(etag),
            Err(e) => {
                let _ = self.abort_multipart(bucket, key, &upload_id);
                Err(e)
            }
        }
    }

    /// Upload a local file without loading it in memory: a streamed single `PUT` up to
    /// `part_size` (or when `multipart` is false), otherwise a multipart upload reading one
    /// part at a time. Returns (ETag, size).
    pub fn upload_file(&self, bucket: &str, key: &str, path: &str, content_type: Option<&str>, extra: &[(String, String)], multipart: bool, part_size: usize) -> Result<(String, u64)> {
        let io = |e: std::io::Error| CamelError::io(format!("Cannot read {}: {}", path, e));
        let mut f = std::fs::File::open(path).map_err(io)?;
        let len = f.metadata().map_err(io)?.len();
        if multipart && len > part_size as u64 {
            let etag = self.multipart_upload(bucket, key, content_type, extra, part_size, &mut |max| {
                let mut buf = Vec::with_capacity(max);
                (&mut f).take(max as u64).read_to_end(&mut buf).map_err(io)?;
                Ok(if buf.is_empty() { None } else { Some(buf) })
            })?;
            return Ok((etag, len));
        }
        let mut req = Request::new("PUT", self.url(bucket, Some(key), None)?);
        req.set_header("Content-Type", content_type.unwrap_or("application/octet-stream"));
        for (k, v) in extra {
            req.set_header(k, v.clone());
        }
        let resp = self.aws.send_streaming(req, "s3", kinds::S3, &mut f, len)?;
        Ok((resp.header("ETag").unwrap_or("").to_string(), len))
    }

    /// Stream an object into a local file (written to `path.part`, then renamed).
    /// Returns the response (headers) and the size.
    pub fn download_file(&self, bucket: &str, key: &str, path: &str) -> Result<(Response, u64)> {
        let tmp = format!("{}.part", path);
        let io = |e: std::io::Error| CamelError::io(format!("Cannot write {}: {}", path, e));
        let mut f = std::fs::File::create(&tmp).map_err(io)?;
        let mut req = Request::new("GET", self.url(bucket, Some(key), None)?);
        for (k, v) in &self.part_headers {
            req.set_header(k, v.clone());
        }
        let res = self.aws.send_to(req, "s3", kinds::S3, &mut f);
        drop(f);
        match res {
            Ok(r) => {
                std::fs::rename(&tmp, path).map_err(io)?;
                Ok(r)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e)
            }
        }
    }
}

/// Configuration of `streamingUploadMode`.
#[derive(Clone, Debug)]
pub struct StreamingConfig {
    pub bucket: String,
    pub key_name: String,
    pub part_size: usize,
    pub batch_message_number: i64,
    pub batch_size: i64,
    pub timeout: Option<Duration>,
    /// `progressive` or `random`.
    pub naming_strategy: String,
    /// `override` or `lastPart`.
    pub restarting_policy: String,
}

#[derive(Default)]
struct StreamState {
    key: Option<String>,
    upload_id: Option<String>,
    parts: Vec<(i32, String)>,
    buffer: Vec<u8>,
    messages: i64,
    bytes: i64,
    index: u64,
    started: Option<Instant>,
    initialized: bool,
}

/// Aggregates exchange bodies into multipart objects (Camel `AWS2S3StreamUploadProducer`).
pub struct StreamingUploader {
    client: S3Client,
    cfg: StreamingConfig,
    state: Mutex<StreamState>,
    stopped: std::sync::atomic::AtomicBool,
}

/// Object key for the `index`-th object of a stream (`progressive`: `name`, `name-1`, ...
/// keeping the extension last; `random`: `name-<uuid>`).
pub fn streaming_key(key_name: &str, strategy: &str, index: u64) -> String {
    let (stem, ext) = split_ext(key_name);
    if strategy.eq_ignore_ascii_case("random") {
        format!("{}-{}{}", stem, crate::util::uuid(), ext)
    } else if index == 0 {
        key_name.to_string()
    } else {
        format!("{}-{}{}", stem, index, ext)
    }
}

fn split_ext(key_name: &str) -> (&str, &str) {
    match key_name.rfind('.') {
        Some(i) if i > 0 && !key_name[i..].contains('/') => (&key_name[..i], &key_name[i..]),
        _ => (key_name, ""),
    }
}

impl StreamingUploader {
    pub fn new(client: S3Client, cfg: StreamingConfig) -> Arc<StreamingUploader> {
        Arc::new(StreamingUploader { client, cfg, state: Mutex::new(StreamState::default()), stopped: std::sync::atomic::AtomicBool::new(false) })
    }

    /// Start the timeout watcher (when `streamingUploadTimeout` is set).
    pub fn start(self: &Arc<Self>) {
        self.stopped.store(false, std::sync::atomic::Ordering::SeqCst);
        let Some(timeout) = self.cfg.timeout else { return };
        let weak: Weak<StreamingUploader> = Arc::downgrade(self);
        let tick = (timeout / 4).clamp(Duration::from_millis(10), Duration::from_millis(200));
        std::thread::spawn(move || loop {
            std::thread::sleep(tick);
            let Some(me) = weak.upgrade() else { return };
            if me.stopped.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            let due = me.state.lock().unwrap().started.is_some_and(|s| s.elapsed() >= timeout);
            if due {
                if let Err(e) = me.flush() {
                    camel_core::camel_warn!("camel-cloud.aws2-s3", "Error completing streaming upload after timeout: {}", e);
                }
            }
        });
    }

    fn init(&self, st: &mut StreamState) -> Result<()> {
        if st.initialized {
            return Ok(());
        }
        st.initialized = true;
        if self.cfg.restarting_policy.eq_ignore_ascii_case("lastPart") && !self.cfg.naming_strategy.eq_ignore_ascii_case("random") {
            // continue after the highest existing progressive index
            let (stem, ext) = split_ext(&self.cfg.key_name);
            let mut start_after: Option<String> = None;
            let mut max: Option<u64> = None;
            loop {
                let mut q = vec!["list-type=2".to_string(), format!("prefix={}", uri_encode(stem, false))];
                if let Some(s) = &start_after {
                    q.push(format!("start-after={}", uri_encode(s, false)));
                }
                let resp = self.client.send(Request::new("GET", self.client.url(&self.cfg.bucket, None, Some(q.join("&")))?))?;
                let r = super::s3::parse_list_result(&resp.text())?;
                for o in &r.objects {
                    let n = if o.key == self.cfg.key_name {
                        Some(0)
                    } else {
                        o.key.strip_prefix(stem).and_then(|r| r.strip_prefix('-')).and_then(|r| r.strip_suffix(ext)).and_then(|n| n.parse::<u64>().ok())
                    };
                    if let Some(n) = n {
                        max = Some(max.map_or(n, |m: u64| m.max(n)));
                    }
                }
                start_after = r.objects.last().map(|o| o.key.clone()).filter(|_| r.truncated);
                if start_after.is_none() {
                    break;
                }
            }
            st.index = max.map_or(0, |m| m + 1);
        }
        Ok(())
    }

    fn upload_buffer_part(&self, st: &mut StreamState) -> Result<()> {
        let key = st.key.clone().unwrap_or_default();
        if st.upload_id.is_none() {
            st.upload_id = Some(self.client.create_multipart(&self.cfg.bucket, &key, None, &[])?);
        }
        let id = st.upload_id.clone().unwrap_or_default();
        let n = st.parts.len() as i32 + 1;
        let take = st.buffer.len().min(self.cfg.part_size.max(1));
        let chunk: Vec<u8> = st.buffer.drain(..take).collect();
        let etag = self.client.upload_part(&self.cfg.bucket, &key, &id, n, chunk)?;
        st.parts.push((n, etag));
        Ok(())
    }

    /// Append one exchange body; returns `(key, etag)` when an object was completed.
    pub fn write(&self, data: &[u8]) -> Result<Option<(String, String)>> {
        let mut st = self.state.lock().unwrap();
        self.init(&mut st)?;
        if st.key.is_none() {
            st.key = Some(streaming_key(&self.cfg.key_name, &self.cfg.naming_strategy, st.index));
            st.started = Some(Instant::now());
        }
        st.buffer.extend_from_slice(data);
        st.messages += 1;
        st.bytes += data.len() as i64;
        while st.buffer.len() >= self.cfg.part_size.max(1) {
            self.upload_buffer_part(&mut st)?;
        }
        if st.messages >= self.cfg.batch_message_number || st.bytes >= self.cfg.batch_size {
            return self.complete(&mut st);
        }
        Ok(None)
    }

    fn complete(&self, st: &mut StreamState) -> Result<Option<(String, String)>> {
        let Some(key) = st.key.clone() else { return Ok(None) };
        let etag = match st.upload_id.clone() {
            None => {
                let body = std::mem::take(&mut st.buffer);
                let resp = self.client.send(Request::new("PUT", self.client.url(&self.cfg.bucket, Some(&key), None)?).header("Content-Type", "application/octet-stream").body(body))?;
                resp.header("ETag").unwrap_or("").to_string()
            }
            Some(id) => {
                if !st.buffer.is_empty() {
                    self.upload_buffer_part(st)?;
                }
                match self.client.complete_multipart(&self.cfg.bucket, &key, &id, &st.parts) {
                    Ok(e) => e,
                    Err(e) => {
                        let _ = self.client.abort_multipart(&self.cfg.bucket, &key, &id);
                        *st = StreamState { index: st.index + 1, initialized: true, ..Default::default() };
                        return Err(e);
                    }
                }
            }
        };
        *st = StreamState { index: st.index + 1, initialized: true, ..Default::default() };
        Ok(Some((key, etag)))
    }

    /// Complete the pending object, if any.
    pub fn flush(&self) -> Result<Option<(String, String)>> {
        let mut st = self.state.lock().unwrap();
        self.complete(&mut st)
    }

    /// Complete the pending object and stop the timeout watcher.
    pub fn stop(&self) -> Result<()> {
        self.stopped.store(true, std::sync::atomic::Ordering::SeqCst);
        self.flush().map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progressive_names() {
        assert_eq!(streaming_key("logs/app.log", "progressive", 0), "logs/app.log");
        assert_eq!(streaming_key("logs/app.log", "progressive", 2), "logs/app-2.log");
        assert_eq!(streaming_key("data", "progressive", 1), "data-1");
        let r = streaming_key("a.txt", "random", 5);
        assert!(r.starts_with("a-") && r.ends_with(".txt") && r.len() > 10);
    }
}
