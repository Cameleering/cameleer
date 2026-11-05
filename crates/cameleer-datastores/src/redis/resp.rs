//! RESP2 / RESP3 codec (Redis serialization protocol).

use std::io::{self, BufRead, Write};

use cameleer_core::value::Value;

/// A RESP value (RESP2 types plus the RESP3 additions).
#[derive(Clone, Debug, PartialEq)]
pub enum Resp {
    /// `+OK`
    Simple(String),
    /// `-ERR ...`
    Error(String),
    /// `:1`
    Int(i64),
    /// `$3\r\nfoo` (`None` = `$-1`, the RESP2 null bulk string)
    Bulk(Option<Vec<u8>>),
    /// `*2...` (`None` = `*-1`, the RESP2 null array)
    Array(Option<Vec<Resp>>),
    /// RESP3 `_`
    Null,
    /// RESP3 `,1.5`
    Double(f64),
    /// RESP3 `#t` / `#f`
    Bool(bool),
    /// RESP3 `(123...`
    BigNumber(String),
    /// RESP3 `!` blob error
    BlobError(Vec<u8>),
    /// RESP3 `=` verbatim string (format, text)
    Verbatim(String, Vec<u8>),
    /// RESP3 `%` map
    Map(Vec<(Resp, Resp)>),
    /// RESP3 `~` set
    Set(Vec<Resp>),
    /// RESP3 `>` push (out-of-band data, e.g. pub/sub messages)
    Push(Vec<Resp>),
}

impl Resp {
    /// A bulk string.
    pub fn bulk(b: impl Into<Vec<u8>>) -> Resp {
        Resp::Bulk(Some(b.into()))
    }

    /// An array.
    pub fn array(items: Vec<Resp>) -> Resp {
        Resp::Array(Some(items))
    }

    /// `+OK`.
    pub fn ok() -> Resp {
        Resp::Simple("OK".into())
    }

    /// An error reply.
    pub fn err(msg: impl Into<String>) -> Resp {
        Resp::Error(msg.into())
    }

    /// Whether this is a null (RESP2 or RESP3).
    pub fn is_null(&self) -> bool {
        matches!(self, Resp::Null | Resp::Bulk(None) | Resp::Array(None))
    }

    /// The error text of an error reply.
    pub fn error_message(&self) -> Option<String> {
        match self {
            Resp::Error(e) => Some(e.clone()),
            Resp::BlobError(e) => Some(String::from_utf8_lossy(e).into_owned()),
            _ => None,
        }
    }

    /// Bytes of a string-like reply.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Resp::Bulk(Some(b)) | Resp::Verbatim(_, b) => Some(b),
            Resp::Simple(s) => Some(s.as_bytes()),
            _ => None,
        }
    }

    /// Text of a string-like reply.
    pub fn as_text(&self) -> Option<String> {
        self.as_bytes().map(|b| String::from_utf8_lossy(b).into_owned())
    }

    /// An integer reply (or numeric text).
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Resp::Int(i) => Some(*i),
            Resp::Bool(b) => Some(*b as i64),
            other => other.as_text().and_then(|t| t.trim().parse().ok()),
        }
    }

    /// Items of an aggregate reply (array, set, push).
    pub fn items(&self) -> Option<&[Resp]> {
        match self {
            Resp::Array(Some(a)) | Resp::Set(a) | Resp::Push(a) => Some(a),
            _ => None,
        }
    }

    /// Converts the reply to a Camel value: strings become text when valid UTF-8
    /// (bytes otherwise), maps become objects, sets and arrays arrays.
    pub fn to_value(&self) -> Value {
        match self {
            Resp::Simple(s) => Value::String(s.clone()),
            Resp::Error(e) => Value::String(e.clone()),
            Resp::Int(i) => Value::Int(*i),
            Resp::Bulk(Some(b)) | Resp::Verbatim(_, b) | Resp::BlobError(b) => bytes_to_value(b),
            Resp::Bulk(None) | Resp::Array(None) | Resp::Null => Value::Null,
            Resp::Array(Some(a)) | Resp::Set(a) | Resp::Push(a) => Value::Array(a.iter().map(Resp::to_value).collect()),
            Resp::Double(d) => Value::Float(*d),
            Resp::Bool(b) => Value::Bool(*b),
            Resp::BigNumber(n) => n.parse::<i64>().map(Value::Int).unwrap_or_else(|_| Value::String(n.clone())),
            Resp::Map(m) => Value::Object(m.iter().map(|(k, v)| (k.to_value().to_string(), v.to_value())).collect()),
        }
    }

    /// Encodes the value.
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Resp::Simple(s) => line(out, b'+', s.as_bytes()),
            Resp::Error(s) => line(out, b'-', s.as_bytes()),
            Resp::Int(i) => line(out, b':', i.to_string().as_bytes()),
            Resp::Bulk(None) => out.extend_from_slice(b"$-1\r\n"),
            Resp::Bulk(Some(b)) => blob(out, b'$', b),
            Resp::Array(None) => out.extend_from_slice(b"*-1\r\n"),
            Resp::Array(Some(a)) => agg(out, b'*', a),
            Resp::Null => out.extend_from_slice(b"_\r\n"),
            Resp::Double(d) => {
                let s = if d.is_infinite() {
                    if *d > 0.0 { "inf".to_string() } else { "-inf".to_string() }
                } else if d.is_nan() {
                    "nan".to_string()
                } else {
                    format_double(*d)
                };
                line(out, b',', s.as_bytes())
            }
            Resp::Bool(b) => line(out, b'#', if *b { b"t" } else { b"f" }),
            Resp::BigNumber(n) => line(out, b'(', n.as_bytes()),
            Resp::BlobError(b) => blob(out, b'!', b),
            Resp::Verbatim(f, b) => {
                let mut data = format!("{}:", f).into_bytes();
                data.extend_from_slice(b);
                blob(out, b'=', &data)
            }
            Resp::Map(m) => {
                line(out, b'%', m.len().to_string().as_bytes());
                for (k, v) in m {
                    k.encode(out);
                    v.encode(out);
                }
            }
            Resp::Set(a) => agg(out, b'~', a),
            Resp::Push(a) => agg(out, b'>', a),
        }
    }

    /// Encoded bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.encode(&mut v);
        v
    }
}

/// Text when valid UTF-8, bytes otherwise.
pub fn bytes_to_value(b: &[u8]) -> Value {
    match std::str::from_utf8(b) {
        Ok(s) => Value::String(s.to_string()),
        Err(_) => Value::Bytes(b.to_vec()),
    }
}

/// Formats a double like Redis (`1`, `1.5`, `1e+20` style avoided).
pub fn format_double(d: f64) -> String {
    if d.fract() == 0.0 && d.abs() < 1e17 {
        format!("{}", d as i64)
    } else {
        format!("{}", d)
    }
}

fn line(out: &mut Vec<u8>, t: u8, s: &[u8]) {
    out.push(t);
    out.extend_from_slice(s);
    out.extend_from_slice(b"\r\n");
}

fn blob(out: &mut Vec<u8>, t: u8, b: &[u8]) {
    line(out, t, b.len().to_string().as_bytes());
    out.extend_from_slice(b);
    out.extend_from_slice(b"\r\n");
}

fn agg(out: &mut Vec<u8>, t: u8, a: &[Resp]) {
    line(out, t, a.len().to_string().as_bytes());
    for x in a {
        x.encode(out);
    }
}

/// Encodes a command as an array of bulk strings.
pub fn encode_command(args: &[Vec<u8>], out: &mut Vec<u8>) {
    line(out, b'*', args.len().to_string().as_bytes());
    for a in args {
        blob(out, b'$', a);
    }
}

fn bad(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn read_line<R: BufRead>(r: &mut R) -> io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed"));
    }
    if buf.len() > 64 * 1024 * 1024 {
        return Err(bad("line too long"));
    }
    if buf.ends_with(b"\r\n") {
        buf.truncate(buf.len() - 2);
    } else if buf.ends_with(b"\n") {
        buf.truncate(buf.len() - 1);
    }
    Ok(buf)
}

fn parse_num(b: &[u8]) -> io::Result<i64> {
    std::str::from_utf8(b).ok().and_then(|s| s.trim().parse().ok()).ok_or_else(|| bad(format!("invalid number {:?}", String::from_utf8_lossy(b))))
}

fn read_blob<R: BufRead>(r: &mut R, len: i64) -> io::Result<Vec<u8>> {
    if !(0..=512 * 1024 * 1024).contains(&len) {
        return Err(bad("invalid bulk length"));
    }
    let mut data = vec![0u8; len as usize + 2];
    r.read_exact(&mut data)?;
    if &data[len as usize..] != b"\r\n" {
        return Err(bad("missing CRLF after bulk string"));
    }
    data.truncate(len as usize);
    Ok(data)
}

fn read_items<R: BufRead>(r: &mut R, n: i64) -> io::Result<Vec<Resp>> {
    if !(0..=16 * 1024 * 1024).contains(&n) {
        return Err(bad("invalid aggregate length"));
    }
    (0..n).map(|_| read_resp(r)).collect()
}

/// Reads one RESP value. Inline commands (`PING\r\n`) are returned as an array of bulk strings.
pub fn read_resp<R: BufRead>(r: &mut R) -> io::Result<Resp> {
    let l = read_line(r)?;
    let Some((&t, rest)) = l.split_first() else { return read_resp(r) };
    let text = || String::from_utf8_lossy(rest).into_owned();
    Ok(match t {
        b'+' => Resp::Simple(text()),
        b'-' => Resp::Error(text()),
        b':' => Resp::Int(parse_num(rest)?),
        b'$' => {
            let n = parse_num(rest)?;
            if n < 0 { Resp::Bulk(None) } else { Resp::Bulk(Some(read_blob(r, n)?)) }
        }
        b'*' => {
            let n = parse_num(rest)?;
            if n < 0 { Resp::Array(None) } else { Resp::Array(Some(read_items(r, n)?)) }
        }
        b'_' => Resp::Null,
        b',' => {
            let s = text();
            Resp::Double(match s.as_str() {
                "inf" => f64::INFINITY,
                "-inf" => f64::NEG_INFINITY,
                "nan" => f64::NAN,
                _ => s.parse().map_err(|_| bad("invalid double"))?,
            })
        }
        b'#' => Resp::Bool(rest == b"t"),
        b'(' => Resp::BigNumber(text()),
        b'!' => Resp::BlobError(read_blob(r, parse_num(rest)?)?),
        b'=' => {
            let data = read_blob(r, parse_num(rest)?)?;
            if data.len() >= 4 && data[3] == b':' {
                Resp::Verbatim(String::from_utf8_lossy(&data[..3]).into_owned(), data[4..].to_vec())
            } else {
                Resp::Verbatim("txt".into(), data)
            }
        }
        b'%' => {
            let n = parse_num(rest)?;
            let items = read_items(r, n.saturating_mul(2))?;
            let mut it = items.into_iter();
            let mut m = Vec::new();
            while let (Some(k), Some(v)) = (it.next(), it.next()) {
                m.push((k, v));
            }
            Resp::Map(m)
        }
        b'~' => Resp::Set(read_items(r, parse_num(rest)?)?),
        b'>' => Resp::Push(read_items(r, parse_num(rest)?)?),
        b'|' => {
            // attributes: read and ignore, then return the following value
            let n = parse_num(rest)?;
            read_items(r, n.saturating_mul(2))?;
            return read_resp(r);
        }
        _ => Resp::Array(Some(l.split(|c| *c == b' ').filter(|p| !p.is_empty()).map(|p| Resp::bulk(p.to_vec())).collect())),
    })
}

/// Writes a value and flushes.
pub fn write_resp<W: Write>(w: &mut W, v: &Resp) -> io::Result<()> {
    w.write_all(&v.to_bytes())?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn rt(v: Resp) {
        let b = v.to_bytes();
        assert_eq!(read_resp(&mut BufReader::new(&b[..])).unwrap(), v);
    }

    #[test]
    fn known_bytes() {
        let mut out = Vec::new();
        encode_command(&[b"SET".to_vec(), b"k".to_vec(), b"v".to_vec()], &mut out);
        assert_eq!(out, b"*3\r\n$3\r\nSET\r\n$1\r\nk\r\n$1\r\nv\r\n");
        assert_eq!(Resp::ok().to_bytes(), b"+OK\r\n");
        assert_eq!(Resp::Bulk(None).to_bytes(), b"$-1\r\n");
        assert_eq!(Resp::Int(-3).to_bytes(), b":-3\r\n");
        assert_eq!(Resp::Map(vec![(Resp::bulk("a"), Resp::Int(1))]).to_bytes(), b"%1\r\n$1\r\na\r\n:1\r\n");
        assert_eq!(Resp::Double(1.5).to_bytes(), b",1.5\r\n");
        assert_eq!(Resp::Bool(true).to_bytes(), b"#t\r\n");
        let v = read_resp(&mut BufReader::new(&b"*2\r\n$5\r\nhello\r\n:7\r\n"[..])).unwrap();
        assert_eq!(v, Resp::array(vec![Resp::bulk("hello"), Resp::Int(7)]));
        let v = read_resp(&mut BufReader::new(&b"PING foo\r\n"[..])).unwrap();
        assert_eq!(v, Resp::array(vec![Resp::bulk("PING"), Resp::bulk("foo")]));
        let v = read_resp(&mut BufReader::new(&b"|1\r\n+a\r\n+b\r\n:5\r\n"[..])).unwrap();
        assert_eq!(v, Resp::Int(5));
        assert!(read_resp(&mut BufReader::new(&b"$3\r\nab"[..])).is_err());
    }

    #[test]
    fn round_trips() {
        rt(Resp::Simple("OK".into()));
        rt(Resp::Error("ERR x".into()));
        rt(Resp::bulk(vec![0, 255, 13, 10]));
        rt(Resp::Array(None));
        rt(Resp::Null);
        rt(Resp::Double(-2.25));
        rt(Resp::BigNumber("1234567890123456789012".into()));
        rt(Resp::Verbatim("txt".into(), b"hi".to_vec()));
        rt(Resp::Set(vec![Resp::Int(1)]));
        rt(Resp::Push(vec![Resp::bulk("message"), Resp::bulk("c"), Resp::bulk("m")]));
        rt(Resp::BlobError(b"SYNTAX x".to_vec()));
        assert_eq!(Resp::Map(vec![(Resp::bulk("a"), Resp::bulk("b"))]).to_value(), Value::object([("a", "b")]));
    }
}
