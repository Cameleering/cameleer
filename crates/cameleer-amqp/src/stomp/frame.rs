//! STOMP 1.2 frames: `COMMAND EOL *(header EOL) EOL body NUL`.
//!
//! Header values are escaped (`\\`, `\n`, `\r`, `\c`) except in `CONNECT`/`CONNECTED`
//! frames; a repeated header keeps its first value. Bodies carry a `content-length`
//! header when non-empty so that binary payloads containing NUL survive. Heart-beat EOLs
//! between frames are skipped by [`read_frame`].

use std::io::{self, BufRead, Read};

/// A STOMP frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub command: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Frame {
    pub fn new(command: &str) -> Frame {
        Frame { command: command.to_string(), headers: Vec::new(), body: Vec::new() }
    }
    /// Builder: append a header.
    pub fn header(mut self, k: &str, v: impl Into<String>) -> Frame {
        self.headers.push((k.to_string(), v.into()));
        self
    }
    /// Builder: set the body.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Frame {
        self.body = body.into();
        self
    }
    /// First value of a header.
    pub fn get(&self, k: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
    /// Replace (or add) a header.
    pub fn set(&mut self, k: &str, v: impl Into<String>) {
        let v = v.into();
        self.headers.retain(|(n, _)| n != k);
        self.headers.push((k.to_string(), v));
    }
    /// Remove a header.
    pub fn remove(&mut self, k: &str) {
        self.headers.retain(|(n, _)| n != k);
    }
    /// The body as (lossy) UTF-8.
    pub fn body_string(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn escapes(&self) -> bool {
        self.command != "CONNECT" && self.command != "CONNECTED"
    }

    /// Serialize the frame.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.body.len() + 128);
        out.extend_from_slice(self.command.as_bytes());
        out.push(b'\n');
        let esc = self.escapes();
        for (k, v) in &self.headers {
            if k == "content-length" {
                continue;
            }
            if esc {
                out.extend_from_slice(escape(k).as_bytes());
                out.push(b':');
                out.extend_from_slice(escape(v).as_bytes());
            } else {
                out.extend_from_slice(k.as_bytes());
                out.push(b':');
                out.extend_from_slice(v.as_bytes());
            }
            out.push(b'\n');
        }
        if !self.body.is_empty() {
            out.extend_from_slice(format!("content-length:{}\n", self.body.len()).as_bytes());
        }
        out.push(b'\n');
        out.extend_from_slice(&self.body);
        out.push(0);
        out
    }
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            ':' => o.push_str("\\c"),
            c => o.push(c),
        }
    }
    o
}

fn unescape(s: &str) -> io::Result<String> {
    let mut o = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            o.push(c);
            continue;
        }
        match it.next() {
            Some('\\') => o.push('\\'),
            Some('n') => o.push('\n'),
            Some('r') => o.push('\r'),
            Some('c') => o.push(':'),
            other => return Err(io::Error::new(io::ErrorKind::InvalidData, format!("invalid header escape \\{}", other.map(String::from).unwrap_or_default()))),
        }
    }
    Ok(o)
}

fn read_line<R: BufRead>(r: &mut R, max: usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = r.by_ref().take(max as u64 + 2).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        if buf.len() > max {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "STOMP header line too long"));
        }
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed inside a STOMP frame"));
    }
    buf.pop();
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map(Some).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "STOMP headers must be UTF-8"))
}

/// Read one frame, skipping heart-beat EOLs. `Ok(None)` on a clean EOF.
pub fn read_frame<R: BufRead>(r: &mut R, max_body: usize) -> io::Result<Option<Frame>> {
    let command = loop {
        match read_line(r, 64 * 1024)? {
            None => return Ok(None),
            Some(l) if l.is_empty() => continue,
            Some(l) => break l,
        }
    };
    let mut f = Frame::new(&command);
    let esc = f.escapes();
    loop {
        let line = read_line(r, 64 * 1024)?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed inside STOMP headers"))?;
        if line.is_empty() {
            break;
        }
        let (k, v) = line.split_once(':').ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, format!("invalid STOMP header line: {}", line)))?;
        let (k, v) = if esc { (unescape(k)?, unescape(v)?) } else { (k.to_string(), v.to_string()) };
        if f.get(&k).is_none() {
            f.headers.push((k, v));
        }
    }
    match f.get("content-length").map(|s| s.trim().parse::<usize>()) {
        Some(Ok(n)) => {
            if n > max_body {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("STOMP body of {} bytes exceeds the limit", n)));
            }
            let mut body = vec![0u8; n];
            r.read_exact(&mut body)?;
            let mut nul = [0u8; 1];
            r.read_exact(&mut nul)?;
            if nul[0] != 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "STOMP frame not terminated by NUL"));
            }
            f.body = body;
        }
        Some(Err(_)) => return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid content-length")),
        None => {
            let mut body = Vec::new();
            r.by_ref().take(max_body as u64 + 1).read_until(0, &mut body)?;
            if body.last() != Some(&0) {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "STOMP frame not terminated by NUL"));
            }
            body.pop();
            f.body = body;
        }
    }
    f.remove("content-length");
    Ok(Some(f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_escapes_and_binary_body() {
        let f = Frame::new("SEND").header("destination", "/queue/a").header("weird:key", "line1\nline2\\x").with_body(vec![1, 0, 2]);
        let enc = f.encode();
        let mut r = &b"\n\r\n"[..];
        let mut all = r.to_vec();
        all.extend_from_slice(&enc);
        all.extend_from_slice(b"\n");
        r = &all[..];
        let d = read_frame(&mut r, 1 << 20).unwrap().unwrap();
        assert_eq!(d, f);
        assert!(read_frame(&mut r, 1 << 20).unwrap().is_none());
    }

    #[test]
    fn frames_without_content_length_and_repeated_headers() {
        let raw = b"MESSAGE\ndestination:/topic/x\nfoo:first\nfoo:second\n\nhello\0CONNECTED\nversion:1.2\nsession:a:b\n\n\0";
        let mut r = &raw[..];
        let m = read_frame(&mut r, 100).unwrap().unwrap();
        assert_eq!((m.command.as_str(), m.get("foo"), m.body_string().as_str()), ("MESSAGE", Some("first"), "hello"));
        let c = read_frame(&mut r, 100).unwrap().unwrap();
        assert_eq!(c.get("session"), Some("a:b"), "CONNECTED headers are not unescaped");
        let mut bad = &b"SEND\nx:\\t\n\n\0"[..];
        assert!(read_frame(&mut bad, 100).is_err());
    }
}
