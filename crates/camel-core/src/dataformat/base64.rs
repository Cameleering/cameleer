//! Base64 encoding/decoding and the `base64` data format.

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::value::Value;

use super::DataFormat;

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// Encode bytes as base64 (standard or URL-safe alphabet; URL-safe output has no padding,
/// like commons-codec).
pub fn encode(data: &[u8], url_safe: bool) -> String {
    let alpha = if url_safe { URL } else { STD };
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(alpha[(n >> 18) as usize & 63] as char);
        out.push(alpha[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(alpha[(n >> 6) as usize & 63] as char);
        } else if !url_safe {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(alpha[n as usize & 63] as char);
        } else if !url_safe {
            out.push('=');
        }
    }
    out
}

/// Encode with line wrapping every `line_length` chars (0 = no wrapping).
pub fn encode_lines(data: &[u8], url_safe: bool, line_length: usize, separator: &str) -> String {
    let s = encode(data, url_safe);
    if line_length == 0 || s.len() <= line_length {
        return s;
    }
    let lines: Vec<&str> = s.as_bytes().chunks(line_length).map(|c| std::str::from_utf8(c).unwrap()).collect();
    lines.join(separator)
}

/// Decode base64 (both alphabets accepted, whitespace ignored, padding optional).
pub fn decode(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0;
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            other => {
                return Err(CamelError::new(
                    "DataFormatException",
                    format!("Illegal base64 character '{}'", other as char),
                ))
            }
        };
        buf = (buf << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

/// The `base64` data format. Options: `lineLength` (default 76, 0 = no wrapping),
/// `lineSeparator` (default CRLF), `urlSafe`.
pub struct Base64DataFormat {
    pub line_length: usize,
    pub line_separator: String,
    pub url_safe: bool,
}

impl Default for Base64DataFormat {
    fn default() -> Self {
        Base64DataFormat { line_length: 76, line_separator: "\r\n".into(), url_safe: false }
    }
}

impl DataFormat for Base64DataFormat {
    fn name(&self) -> &str {
        "base64"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let b = ex.message.body.to_bytes();
        ex.message.body = Value::String(encode_lines(&b, self.url_safe, self.line_length, &self.line_separator));
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let s = match &ex.message.body {
            Value::Bytes(b) => String::from_utf8_lossy(b).to_string(),
            other => other.to_string(),
        };
        ex.message.body = Value::Bytes(decode(&s)?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vectors() {
        for (plain, enc) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
            assert_eq!(encode(plain.as_bytes(), false), enc);
            assert_eq!(decode(enc).unwrap(), plain.as_bytes());
        }
        assert_eq!(encode(&[0xfb, 0xff], true), "-_8");
        assert_eq!(decode("-_8").unwrap(), vec![0xfb, 0xff]);
        assert!(decode("ab$c").is_err());
        let long = vec![7u8; 100];
        let wrapped = encode_lines(&long, false, 76, "\r\n");
        assert!(wrapped.contains("\r\n"));
        assert_eq!(decode(&wrapped).unwrap(), long);
    }
}
