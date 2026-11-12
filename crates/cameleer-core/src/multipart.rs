//! MIME multipart bodies (RFC 2046): splitting a body into parts and building one, binary
//! safe. Shared by the `mimeMultipart` data format and DICOMweb (`multipart/related`).

use crate::error::{CamelError, Result};

/// One part: its header fields and its content (transfer encodings are not decoded).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Part {
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Part {
    /// A part with a `Content-Type`.
    pub fn new(content_type: &str, body: Vec<u8>) -> Part {
        Part { headers: vec![("Content-Type".to_string(), content_type.to_string())], body }
    }

    /// A header value (case-insensitive name).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The `Content-Type` (lower-case media type without parameters).
    pub fn media_type(&self) -> Option<String> {
        self.header("Content-Type").map(|v| v.split(';').next().unwrap_or("").trim().to_ascii_lowercase())
    }
}

/// A parameter of a header value such as `multipart/related; type="application/dicom"; boundary=x`
/// (case-insensitive name, quotes removed).
pub fn param(value: &str, name: &str) -> Option<String> {
    for p in value.split(';').skip(1) {
        if let Some((k, v)) = p.split_once('=') {
            if k.trim().eq_ignore_ascii_case(name) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// The boundary of a multipart content type.
pub fn boundary(content_type: &str) -> Option<String> {
    param(content_type, "boundary").filter(|b| !b.is_empty())
}

/// A new random boundary.
pub fn new_boundary() -> String {
    let (a, b) = (crate::language::support::random_u64(), crate::language::support::random_u64());
    format!("cameleer-{:016x}{:016x}", a, b)
}

/// Find `needle` in `hay` from `from` (first-byte scan, then compare).
pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() || from > hay.len() - needle.len() {
        return None;
    }
    let first = needle[0];
    let last = hay.len() - needle.len();
    let mut i = from;
    while i <= last {
        match hay[i..=last].iter().position(|&b| b == first) {
            Some(off) => {
                i += off;
                if &hay[i..i + needle.len()] == needle {
                    return Some(i);
                }
                i += 1;
            }
            None => return None,
        }
    }
    None
}

/// Parse `Name: value` header lines (with folding) until an empty line; returns the headers
/// and the offset of the content.
pub fn parse_headers(d: &[u8]) -> (Vec<(String, String)>, usize) {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let end = d[i..].iter().position(|b| *b == b'\n').map(|p| i + p).unwrap_or(d.len());
        let line = &d[i..end];
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        i = end + 1;
        if line.is_empty() {
            return (headers, i.min(d.len()));
        }
        let s = String::from_utf8_lossy(line).to_string();
        if (s.starts_with(' ') || s.starts_with('\t')) && !headers.is_empty() {
            let last = headers.last_mut().unwrap();
            last.1.push(' ');
            last.1.push_str(s.trim());
        } else if let Some((k, v)) = s.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    (headers, d.len())
}

/// Split a multipart body into its parts.
pub fn parse(body: &[u8], boundary: &str) -> Result<Vec<Part>> {
    let err = |m: &str| CamelError::new("IllegalArgumentException", m.to_string());
    let delim = format!("--{}", boundary).into_bytes();
    let mut parts = Vec::new();
    let mut pos = find(body, &delim, 0).ok_or_else(|| err("Multipart boundary not found"))?;
    loop {
        let after = pos + delim.len();
        if body.get(after..after + 2) == Some(b"--") {
            break;
        }
        let start = match find(body, b"\n", after) {
            Some(p) => p + 1,
            None => break,
        };
        // the next delimiter must start a line
        let mut search = start;
        let next = loop {
            let n = find(body, &delim, search).ok_or_else(|| err("Unterminated multipart body"))?;
            if n == start || body[n - 1] == b'\n' {
                break n;
            }
            search = n + 1;
        };
        let mut end = next;
        if end > start && body[end - 1] == b'\n' {
            end -= 1;
            if end > start && body[end - 1] == b'\r' {
                end -= 1;
            }
        }
        let raw = &body[start..end];
        let (headers, off) = parse_headers(raw);
        parts.push(Part { headers, body: raw[off..].to_vec() });
        pos = next;
    }
    Ok(parts)
}

/// Build a multipart body (CRLF line ends).
pub fn build(parts: &[Part], boundary: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(parts.iter().map(|p| p.body.len() + 128).sum::<usize>() + 64);
    for p in parts {
        out.extend_from_slice(b"--");
        out.extend_from_slice(boundary.as_bytes());
        out.extend_from_slice(b"\r\n");
        for (k, v) in &p.headers {
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&p.body);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"--");
    out.extend_from_slice(boundary.as_bytes());
    out.extend_from_slice(b"--\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_and_parse_binary_parts() {
        let bin: Vec<u8> = (0..=255u8).cycle().take(3000).collect();
        // a body that contains the boundary text not at a line start, and CR LF bytes
        let mut tricky = b"x--b0und\r\nnot a delimiter".to_vec();
        tricky.extend_from_slice(&bin);
        let parts = vec![Part::new("application/dicom", bin.clone()), Part::new("application/octet-stream", tricky.clone()), Part::new("text/plain", Vec::new())];
        let body = build(&parts, "b0und");
        let back = parse(&body, "b0und").unwrap();
        assert_eq!(back, parts);
        assert_eq!(back[0].media_type().as_deref(), Some("application/dicom"));
        assert_eq!(boundary("multipart/related; type=\"application/dicom\"; boundary=\"b0und\"").as_deref(), Some("b0und"));
        assert_eq!(param("multipart/related; type=\"application/dicom\"", "TYPE").as_deref(), Some("application/dicom"));
        assert!(parse(b"no boundary here", "zz").is_err());
        assert!(new_boundary().len() > 20);
        // LF-only line ends and a preamble
        let lf = b"preamble\n--q\nContent-Type: a/b\n\nhello\n--q--\n";
        let p = parse(lf, "q").unwrap();
        assert_eq!(p[0].body, b"hello");
        assert_eq!(p[0].header("content-type"), Some("a/b"));
    }
}
