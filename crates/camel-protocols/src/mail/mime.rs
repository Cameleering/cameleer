//! RFC 822 / 2045 message parsing and building: header unfolding, RFC 2047 encoded
//! words, `base64`/`quoted-printable` transfer encodings, `multipart/*` bodies,
//! address lists and SMTP/POP3 dot-stuffing.

use camel_core::dataformat::base64;
use camel_core::util_time::{civil_from_epoch, now_millis};

/// An attachment (a non-body MIME part).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// A parsed mail message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MailMessage {
    /// Top level headers in order (values unfolded and RFC 2047 decoded).
    pub headers: Vec<(String, String)>,
    /// The text body: the first `text/plain` part (else the first `text/*` part), with
    /// CRLF line endings converted to `\n`.
    pub body: String,
    /// Content type of the body part.
    pub content_type: String,
    pub attachments: Vec<Attachment>,
}

impl MailMessage {
    /// First value of a header (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Split an entity into (raw header lines unfolded, body).
fn split_entity(raw: &str) -> (Vec<(String, String)>, &str) {
    let (head, body) = if let Some(i) = raw.find("\r\n\r\n") {
        (&raw[..i], &raw[i + 4..])
    } else if let Some(i) = raw.find("\n\n") {
        (&raw[..i], &raw[i + 2..])
    } else if raw.starts_with("\r\n") || raw.starts_with('\n') {
        ("", raw.trim_start_matches(['\r', '\n']))
    } else if raw.lines().all(|l| l.contains(':') || l.starts_with([' ', '\t'])) {
        (raw, "")
    } else {
        ("", raw)
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with([' ', '\t']) {
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(line.trim());
            }
        } else if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    (headers, body)
}

/// Get a parameter of a structured header value: `text/plain; charset="utf-8"`.
pub fn header_param(value: &str, param: &str) -> Option<String> {
    for part in value.split(';').skip(1) {
        if let Some((k, v)) = part.split_once('=') {
            if k.trim().eq_ignore_ascii_case(param) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

fn main_value(value: &str) -> String {
    value.split(';').next().unwrap_or("").trim().to_ascii_lowercase()
}

/// Decode a quoted-printable text.
pub fn decode_quoted_printable(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16);
    while i < b.len() {
        if b[i] == b'=' {
            if i + 2 < b.len() + 1 && b.get(i + 1) == Some(&b'\r') && b.get(i + 2) == Some(&b'\n') {
                i += 3;
                continue;
            }
            if b.get(i + 1) == Some(&b'\n') {
                i += 2;
                continue;
            }
            if let (Some(h), Some(l)) = (b.get(i + 1).and_then(|&c| hex(c)), b.get(i + 2).and_then(|&c| hex(c))) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn decode_charset(data: &[u8], charset: Option<&str>) -> String {
    match charset.map(|c| c.to_ascii_lowercase()) {
        Some(c) if c == "iso-8859-1" || c == "latin1" || c == "us-ascii" || c == "windows-1252" => data.iter().map(|&b| b as char).collect(),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
}

/// Decode RFC 2047 encoded words (`=?UTF-8?B?...?=`, `=?ISO-8859-1?Q?...?=`).
pub fn decode_encoded_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut last_was_word = false;
    while let Some(start) = rest.find("=?") {
        let before = &rest[..start];
        let tail = &rest[start + 2..];
        let parsed = (|| {
            let (charset, t) = tail.split_once('?')?;
            let (enc, t) = t.split_once('?')?;
            let end = t.find("?=")?;
            let text = &t[..end];
            let data = match enc.to_ascii_uppercase().as_str() {
                "B" => base64::decode(text).ok()?,
                "Q" => decode_quoted_printable(&text.replace('_', " ")),
                _ => return None,
            };
            Some((decode_charset(&data, Some(charset)), charset.len() + enc.len() + end + 4 + 2))
        })();
        match parsed {
            Some((text, consumed)) => {
                // whitespace between adjacent encoded words is ignored
                if !(last_was_word && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&text);
                rest = &rest[start + consumed..];
                last_was_word = true;
            }
            None => {
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
                last_was_word = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Encode a header value as an RFC 2047 `B` word when it is not plain ASCII.
pub fn encode_header_value(s: &str) -> String {
    if s.is_ascii() {
        s.to_string()
    } else {
        format!("=?UTF-8?B?{}?=", base64::encode(s.as_bytes(), false))
    }
}

fn decode_body(body: &str, encoding: Option<&str>) -> Vec<u8> {
    match encoding.map(|e| e.trim().to_ascii_lowercase()).as_deref() {
        Some("base64") => base64::decode(&body.split_whitespace().collect::<String>()).unwrap_or_else(|_| body.as_bytes().to_vec()),
        Some("quoted-printable") => decode_quoted_printable(body),
        _ => body.as_bytes().to_vec(),
    }
}

struct Part {
    content_type: String,
    charset: Option<String>,
    filename: Option<String>,
    attachment: bool,
    data: Vec<u8>,
}

fn collect_parts(raw: &str, out: &mut Vec<Part>) {
    let (headers, body) = split_entity(raw);
    let get = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
    let ct_raw = get("Content-Type").unwrap_or_else(|| "text/plain".to_string());
    let ct = main_value(&ct_raw);
    if ct.starts_with("multipart/") {
        if let Some(boundary) = header_param(&ct_raw, "boundary") {
            for p in split_multipart(body, &boundary) {
                collect_parts(p, out);
            }
            return;
        }
    }
    let disp = get("Content-Disposition").unwrap_or_default();
    let filename = header_param(&disp, "filename").or_else(|| header_param(&ct_raw, "name")).map(|f| decode_encoded_words(&f));
    out.push(Part {
        attachment: main_value(&disp) == "attachment" || (filename.is_some() && !ct.starts_with("text/")),
        charset: header_param(&ct_raw, "charset"),
        content_type: ct,
        filename,
        data: decode_body(body, get("Content-Transfer-Encoding").as_deref()),
    });
}

/// Split a multipart body into its raw parts.
pub fn split_multipart<'a>(body: &'a str, boundary: &str) -> Vec<&'a str> {
    let delim = format!("--{}", boundary);
    let mut parts = Vec::new();
    let mut start: Option<usize> = None;
    let mut pos = 0;
    for line in body.split_inclusive('\n') {
        let l = line.trim_end_matches(['\r', '\n']);
        if l == delim || l == format!("{}--", delim) {
            if let Some(s) = start {
                // drop the CRLF preceding the delimiter line
                let mut end = pos;
                if body[..end].ends_with("\r\n") {
                    end -= 2;
                } else if body[..end].ends_with('\n') {
                    end -= 1;
                }
                parts.push(&body[s..end.max(s)]);
            }
            if l.ends_with("--") && l != delim {
                break;
            }
            start = Some(pos + line.len());
        }
        pos += line.len();
    }
    parts
}

/// Parse a raw RFC 822 message.
pub fn parse_message(raw: &[u8]) -> MailMessage {
    let mut text = String::from_utf8_lossy(raw).into_owned();
    // the line break ending the last line belongs to the transport framing (`CRLF.CRLF`)
    if text.ends_with("\r\n") {
        text.truncate(text.len() - 2);
    } else if text.ends_with('\n') {
        text.pop();
    }
    let (headers, _) = split_entity(&text);
    let headers: Vec<(String, String)> = headers.into_iter().map(|(k, v)| (k, decode_encoded_words(&v))).collect();
    let mut parts = Vec::new();
    collect_parts(&text, &mut parts);
    let body_idx = parts
        .iter()
        .position(|p| !p.attachment && p.content_type == "text/plain")
        .or_else(|| parts.iter().position(|p| !p.attachment && p.content_type.starts_with("text/")));
    let mut msg = MailMessage { headers, ..Default::default() };
    for (i, p) in parts.into_iter().enumerate() {
        if Some(i) == body_idx {
            msg.body = decode_charset(&p.data, p.charset.as_deref()).replace("\r\n", "\n");
            msg.content_type = p.content_type;
        } else if p.attachment || p.filename.is_some() {
            msg.attachments.push(Attachment {
                name: p.filename.unwrap_or_else(|| format!("attachment{}", i)),
                content_type: p.content_type,
                data: p.data,
            });
        }
    }
    if msg.content_type.is_empty() {
        msg.content_type = "text/plain".into();
    }
    msg
}

/// Extract the addresses of an address list: `"Doe, John" <john@x.org>, jane@y.org`.
pub fn parse_address_list(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut angle) = (false, false);
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            '<' if !quoted => angle = true,
            '>' if !quoted => angle = false,
            ',' | ';' if !quoted && !angle => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.into_iter()
        .filter_map(|a| {
            let a = a.trim();
            let addr = match (a.rfind('<'), a.rfind('>')) {
                (Some(i), Some(j)) if j > i => a[i + 1..j].trim().to_string(),
                _ => a.trim_matches('"').to_string(),
            };
            (!addr.is_empty()).then_some(addr)
        })
        .collect()
}

/// RFC 2822 date of epoch millis (UTC): `Sat, 31 Oct 2026 18:00:00 +0000`.
pub fn rfc2822_date(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][secs.div_euclid(86_400).rem_euclid(7) as usize];
    let mon = crate::ftp::listing::month_name(mo);
    format!("{}, {:02} {} {} {:02}:{:02}:{:02} +0000", wd, d, mon, y, h, mi, s)
}

fn transfer_encode(data: &[u8], out: &mut String) -> &'static str {
    let ascii_ok = data.is_ascii() && data.split(|&b| b == b'\n').all(|l| l.len() <= 990) && !data.contains(&0);
    if ascii_ok {
        let text = String::from_utf8_lossy(data).replace("\r\n", "\n").replace('\n', "\r\n");
        out.push_str(&text);
        "7bit"
    } else {
        out.push_str(&base64::encode_lines(data, false, 76, "\r\n"));
        "base64"
    }
}

/// Build an RFC 822 message (CRLF line endings, not dot-stuffed). `headers` are written
/// first (in order, values RFC 2047 encoded when needed); MIME headers are added.
pub fn build_message(headers: &[(String, String)], body: &[u8], content_type: &str, attachments: &[Attachment]) -> String {
    let mut out = String::new();
    let has = |n: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
    for (k, v) in headers {
        out.push_str(&format!("{}: {}\r\n", k, encode_header_value(v)));
    }
    if !has("Date") {
        out.push_str(&format!("Date: {}\r\n", rfc2822_date(now_millis())));
    }
    if !has("Message-ID") {
        let id: String = crate::common::random_bytes(12).iter().map(|b| format!("{:02x}", b)).collect();
        out.push_str(&format!("Message-ID: <{}.{}@rust-camel>\r\n", id, now_millis()));
    }
    out.push_str("MIME-Version: 1.0\r\n");
    let ct = if content_type.to_ascii_lowercase().starts_with("text/") && header_param(content_type, "charset").is_none() {
        format!("{}; charset=UTF-8", content_type)
    } else {
        content_type.to_string()
    };
    if attachments.is_empty() {
        let mut b = String::new();
        let enc = transfer_encode(body, &mut b);
        out.push_str(&format!("Content-Type: {}\r\nContent-Transfer-Encoding: {}\r\n\r\n{}", ct, enc, b));
    } else {
        let boundary = format!("----=_Part_{}", crate::common::random_bytes(8).iter().map(|b| format!("{:02x}", b)).collect::<String>());
        out.push_str(&format!("Content-Type: multipart/mixed; boundary=\"{}\"\r\n\r\nThis is a multi-part message in MIME format.\r\n", boundary));
        let mut b = String::new();
        let enc = transfer_encode(body, &mut b);
        out.push_str(&format!("--{}\r\nContent-Type: {}\r\nContent-Transfer-Encoding: {}\r\n\r\n{}\r\n", boundary, ct, enc, b));
        for a in attachments {
            let mut d = String::new();
            let enc = transfer_encode(&a.data, &mut d);
            let name = encode_header_value(&a.name);
            out.push_str(&format!(
                "--{}\r\nContent-Type: {}; name=\"{}\"\r\nContent-Transfer-Encoding: {}\r\nContent-Disposition: attachment; filename=\"{}\"\r\n\r\n{}\r\n",
                boundary, a.content_type, name, enc, name, d
            ));
        }
        out.push_str(&format!("--{}--\r\n", boundary));
    }
    out
}

/// Dot-stuff a message for SMTP `DATA` / POP3 `RETR` (lines starting with `.` get an
/// extra `.`) and normalize line endings to CRLF. The terminating `.` line is not added.
pub fn dot_stuff(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len() + 64);
    for line in msg.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    // `split` yields a trailing empty piece for a message ending with a newline
    if msg.ends_with('\n') {
        out.truncate(out.len() - 2);
    }
    out
}

/// Read a dot-terminated multi-line block (after the status line) and undo dot-stuffing.
pub fn read_dot_terminated<R: std::io::BufRead>(r: &mut R, max: usize) -> std::io::Result<String> {
    let mut out = String::new();
    loop {
        let Some(line) = crate::common::read_line(r, max)? else {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "connection closed in multi-line data"));
        };
        if line == "." {
            return Ok(out);
        }
        out.push_str(line.strip_prefix('.').unwrap_or(&line));
        out.push_str("\r\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoded_words_and_qp() {
        assert_eq!(decode_encoded_words("=?UTF-8?B?SGVsbG8gV8O2cmxk?="), "Hello Wörld");
        assert_eq!(decode_encoded_words("Re: =?ISO-8859-1?Q?caf=E9_au_lait?= now"), "Re: café au lait now");
        assert_eq!(decode_encoded_words("=?UTF-8?Q?a?= =?UTF-8?Q?b?="), "ab");
        assert_eq!(decode_quoted_printable("a=3Db=\r\nc"), b"a=bc");
        assert_eq!(decode_encoded_words(&encode_header_value("Grüße")), "Grüße");
    }

    #[test]
    fn addresses() {
        assert_eq!(parse_address_list("\"Doe, John\" <john@x.org>, jane@y.org;bob@z"), vec!["john@x.org", "jane@y.org", "bob@z"]);
        assert!(parse_address_list(" ").is_empty());
    }

    #[test]
    fn parse_multipart_message() {
        let raw = "From: a@x\r\nTo: b@y\r\nSubject: =?UTF-8?B?SMOpbGxv?=\r\n folded\r\nContent-Type: multipart/mixed; boundary=\"XYZ\"\r\n\r\npreamble\r\n--XYZ\r\nContent-Type: multipart/alternative; boundary=ALT\r\n\r\n--ALT\r\nContent-Type: text/html\r\n\r\n<b>hi</b>\r\n--ALT\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nHi =C3=A9\r\n--ALT--\r\n--XYZ\r\nContent-Type: application/octet-stream; name=data.bin\r\nContent-Disposition: attachment; filename=\"data.bin\"\r\nContent-Transfer-Encoding: base64\r\n\r\nAAEC\r\n--XYZ--\r\n";
        let m = parse_message(raw.as_bytes());
        assert_eq!(m.header("subject"), Some("Héllo folded"));
        assert_eq!(m.body, "Hi é");
        assert_eq!(m.content_type, "text/plain");
        assert_eq!(m.attachments, vec![Attachment { name: "data.bin".into(), content_type: "application/octet-stream".into(), data: vec![0, 1, 2] }]);
    }

    #[test]
    fn build_and_parse_round_trip() {
        let headers = vec![("From".to_string(), "a@x".to_string()), ("Subject".to_string(), "Grüße".to_string())];
        let att = vec![Attachment { name: "n.txt".into(), content_type: "text/plain".into(), data: b"note".to_vec() }];
        let raw = build_message(&headers, "Body ✓\n.line".as_bytes(), "text/plain", &att);
        let m = parse_message(raw.as_bytes());
        assert_eq!(m.header("Subject"), Some("Grüße"));
        assert_eq!(m.body, "Body ✓\n.line");
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(m.attachments[0].data, b"note");
        let plain = build_message(&headers, b"one\ntwo", "text/plain", &[]);
        assert!(plain.contains("Content-Transfer-Encoding: 7bit"));
        assert_eq!(parse_message(plain.as_bytes()).body, "one\ntwo");
    }

    #[test]
    fn dot_stuffing() {
        assert_eq!(dot_stuff("a\n.b\r\n..c"), "a\r\n..b\r\n...c\r\n");
        assert_eq!(dot_stuff("x\r\n"), "x\r\n");
        let mut r = std::io::BufReader::new(&b"..b\r\nc\r\n.\r\n"[..]);
        assert_eq!(read_dot_terminated(&mut r, 1000).unwrap(), ".b\r\nc\r\n");
        assert!(rfc2822_date(0).starts_with("Thu, 01 Jan 1970 00:00:00"));
    }
}
