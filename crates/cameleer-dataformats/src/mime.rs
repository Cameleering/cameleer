//! `mimeMultipart` data format (Camel's `MimeMultipartDataFormat`).
//!
//! Attachments live in the `CamelAttachments` exchange property as an object
//! `file name -> content` (the convention of cameleer-protocols' mail component).
//!
//! * marshal: the body becomes the first part (Content-Type from the `Content-Type` header,
//!   default `text/plain; charset=UTF-8` for text and `application/octet-stream` for
//!   bytes) and each attachment a further part (`Content-Disposition: attachment;
//!   filename=...`, type guessed from the extension). Without attachments the body is left
//!   unchanged unless `multipartWithoutAttachment=true`. Non-text parts are base64 encoded
//!   unless `binaryContent=true`. With `headersInline=false` (default) the MIME headers
//!   (`MIME-Version`, `Content-Type` with the boundary) become message headers; with
//!   `headersInline=true` they are written in front of the body, together with the message
//!   headers whose names match the `includeHeaders` regex. `multipartSubType` (default
//!   `mixed`).
//! * unmarshal: parses a multipart body (headers inline or from the `Content-Type` message
//!   header), the first part becomes the body (with its Content-Type header) and the others
//!   attachments. Non-multipart content is left unchanged.

use std::sync::Arc;

use cameleer_core::dataformat::base64;
use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::Result;
use cameleer_core::message::props::CONTENT_TYPE;
use cameleer_core::message::Exchange;
use cameleer_core::value::ObjectMap;
use cameleer_core::Value;

use crate::{body_bytes, df_err, Opts};

/// Exchange property with the attachments.
pub const ATTACHMENTS: &str = "CamelAttachments";

fn guess_type(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "xml" => "application/xml",
        "json" => "application/json",
        "csv" => "text/csv",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// The `mimeMultipart` data format.
pub struct MimeMultipartDataFormat {
    pub sub_type: String,
    pub without_attachment: bool,
    pub headers_inline: bool,
    pub include_headers: Option<cameleer_core::regex::Regex>,
    pub binary_content: bool,
}

fn boundary() -> String {
    let r = cameleer_tls::crypto::rng::random_bytes(12);
    format!("----=_Part_{}", r.iter().map(|b| format!("{:02x}", b)).collect::<String>())
}

impl MimeMultipartDataFormat {
    fn part(&self, out: &mut Vec<u8>, ct: &str, disposition: Option<&str>, data: &[u8]) {
        out.extend_from_slice(format!("Content-Type: {}\r\n", ct).as_bytes());
        let text = ct.to_ascii_lowercase().starts_with("text/") || ct.contains("xml") || ct.contains("json");
        let enc = if self.binary_content { "binary" } else if text { "8bit" } else { "base64" };
        out.extend_from_slice(format!("Content-Transfer-Encoding: {}\r\n", enc).as_bytes());
        if let Some(d) = disposition {
            out.extend_from_slice(format!("Content-Disposition: {}\r\n", d).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        if enc == "base64" {
            out.extend_from_slice(base64::encode_lines(data, false, 76, "\r\n").as_bytes());
        } else {
            out.extend_from_slice(data);
        }
        out.extend_from_slice(b"\r\n");
    }
}

use cameleer_core::multipart::parse_headers;

fn header<'a>(h: &'a [(String, String)], name: &str) -> Option<&'a str> {
    h.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
}

use cameleer_core::multipart::param;

/// Split a multipart body into parts (headers, decoded content).
pub fn parse_multipart(body: &[u8], boundary: &str) -> Result<Vec<(Vec<(String, String)>, Vec<u8>)>> {
    let parts = cameleer_core::multipart::parse(body, boundary).map_err(|e| df_err(&e.message))?;
    let mut out = Vec::with_capacity(parts.len());
    for p in parts {
        let data = match header(&p.headers, "Content-Transfer-Encoding").map(|e| e.to_ascii_lowercase()) {
            Some(e) if e == "base64" => base64::decode(&String::from_utf8_lossy(&p.body).replace(['\r', '\n'], ""))?,
            Some(e) if e == "quoted-printable" => decode_qp(&p.body),
            _ => p.body,
        };
        out.push((p.headers, data));
    }
    Ok(out)
}

fn decode_qp(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < d.len() {
        if d[i] == b'=' {
            if d.get(i + 1) == Some(&b'\r') && d.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if d.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if let Some(h) = d.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(d[i]);
        i += 1;
    }
    out
}

impl DataFormat for MimeMultipartDataFormat {
    fn name(&self) -> &str {
        "mimeMultipart"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let atts = ex.property(ATTACHMENTS).and_then(|a| a.as_object()).cloned().unwrap_or_default();
        if atts.is_empty() && !self.without_attachment {
            return Ok(());
        }
        let b = boundary();
        let ct = format!("multipart/{}; boundary=\"{}\"", self.sub_type, b);
        let mut out = Vec::new();
        if self.headers_inline {
            out.extend_from_slice(b"MIME-Version: 1.0\r\n");
            out.extend_from_slice(format!("Content-Type: {}\r\n", ct).as_bytes());
            if let Some(re) = &self.include_headers {
                for (k, v) in ex.message.headers.iter() {
                    if k != CONTENT_TYPE && re.is_full_match(k) {
                        out.extend_from_slice(format!("{}: {}\r\n", k, v).as_bytes());
                    }
                }
            }
            out.extend_from_slice(b"\r\n");
        }
        let body_ct = ex.message.headers.get(CONTENT_TYPE).map(|v| v.to_string()).unwrap_or_else(|| match ex.message.body {
            Value::Bytes(_) => "application/octet-stream".into(),
            _ => "text/plain; charset=UTF-8".into(),
        });
        out.extend_from_slice(format!("--{}\r\n", b).as_bytes());
        self.part(&mut out, &body_ct, None, &body_bytes(&ex.message.body));
        for (name, v) in atts.iter() {
            out.extend_from_slice(format!("--{}\r\n", b).as_bytes());
            let disp = format!("attachment; filename=\"{}\"", name);
            self.part(&mut out, &format!("{}; name=\"{}\"", guess_type(name), name), Some(&disp), &body_bytes(v));
        }
        out.extend_from_slice(format!("--{}--\r\n", b).as_bytes());
        ex.message.body = Value::Bytes(out);
        if self.headers_inline {
            ex.message.headers.remove(CONTENT_TYPE);
        } else {
            ex.message.headers.set(CONTENT_TYPE, ct);
            ex.message.headers.set("MIME-Version", "1.0");
        }
        ex.remove_property(ATTACHMENTS);
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let data = body_bytes(&ex.message.body);
        let (ct, content): (String, &[u8]) = if self.headers_inline || data.starts_with(b"MIME-Version:") || data.starts_with(b"Content-Type:") {
            let (h, off) = parse_headers(&data);
            match header(&h, "Content-Type") {
                Some(c) => {
                    for (k, v) in &h {
                        if !k.eq_ignore_ascii_case("Content-Type") && !k.eq_ignore_ascii_case("MIME-Version") {
                            ex.message.headers.set(k.clone(), v.clone());
                        }
                    }
                    (c.to_string(), &data[off..])
                }
                None => return Ok(()),
            }
        } else {
            match ex.message.headers.get(CONTENT_TYPE) {
                Some(c) => (c.to_string(), &data[..]),
                None => return Ok(()),
            }
        };
        if !ct.to_ascii_lowercase().starts_with("multipart/") {
            return Ok(());
        }
        let b = param(&ct, "boundary").ok_or_else(|| df_err("Multipart Content-Type without boundary"))?;
        let mut parts = parse_multipart(content, &b)?.into_iter();
        let mut atts = ObjectMap::new();
        let (body_ct, body) = match parts.next() {
            Some((h, d)) => (header(&h, "Content-Type").map(String::from), d),
            None => (None, Vec::new()),
        };
        for (i, (h, d)) in parts.enumerate() {
            let name = header(&h, "Content-Disposition").and_then(|v| param(v, "filename")).or_else(|| header(&h, "Content-Type").and_then(|v| param(v, "name"))).unwrap_or_else(|| format!("attachment{}", i + 1));
            atts.insert(name, Value::Bytes(d));
        }
        let text = body_ct.as_deref().map(|c| c.to_ascii_lowercase().starts_with("text/")).unwrap_or(false);
        ex.message.body = if text { Value::String(String::from_utf8_lossy(&body).to_string()) } else { Value::Bytes(body) };
        match body_ct {
            Some(c) => ex.message.headers.set(CONTENT_TYPE, c),
            None => {
                ex.message.headers.remove(CONTENT_TYPE);
            }
        }
        ex.message.headers.remove("MIME-Version");
        if !atts.is_empty() {
            ex.set_property(ATTACHMENTS, Value::Object(atts));
        }
        Ok(())
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let include_headers = match o.get(&["includeHeaders"]) {
        Some(r) => Some(cameleer_core::regex::Regex::new(r).map_err(|e| cameleer_core::CamelError::illegal_argument(format!("Invalid includeHeaders regex: {}", e)))?),
        None => None,
    };
    Ok(Arc::new(MimeMultipartDataFormat {
        sub_type: o.string(&["multipartSubType"], "mixed"),
        without_attachment: o.bool(&["multipartWithoutAttachment"], false)?,
        headers_inline: o.bool(&["headersInline"], false)?,
        include_headers,
        binary_content: o.bool(&["binaryContent"], false)?,
    }))
}
