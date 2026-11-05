//! SSH binary data representation (RFC 4251 section 5).

use crate::error::{SshError, SshResult};

/// Builder for SSH wire data.
#[derive(Default, Clone, Debug)]
pub struct Writer {
    pub buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Writer {
        Writer { buf: Vec::new() }
    }
    /// Starts a message with its type byte.
    pub fn msg(t: u8) -> Writer {
        Writer { buf: vec![t] }
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.buf.push(v);
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.buf.push(v as u8);
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.buf.extend_from_slice(&v.to_be_bytes());
        self
    }
    /// Raw bytes without a length prefix.
    pub fn raw(&mut self, v: &[u8]) -> &mut Self {
        self.buf.extend_from_slice(v);
        self
    }
    /// `string`: uint32 length + bytes.
    pub fn string(&mut self, v: &[u8]) -> &mut Self {
        self.u32(v.len() as u32);
        self.buf.extend_from_slice(v);
        self
    }
    pub fn str(&mut self, v: &str) -> &mut Self {
        self.string(v.as_bytes())
    }
    /// `mpint` from an unsigned big-endian magnitude.
    pub fn mpint(&mut self, v: &[u8]) -> &mut Self {
        let mut i = 0;
        while i < v.len() && v[i] == 0 {
            i += 1;
        }
        let v = &v[i..];
        if !v.is_empty() && v[0] & 0x80 != 0 {
            self.u32(v.len() as u32 + 1);
            self.buf.push(0);
        } else {
            self.u32(v.len() as u32);
        }
        self.buf.extend_from_slice(v);
        self
    }
    /// `name-list`: comma separated names as a string.
    pub fn name_list<S: AsRef<str>>(&mut self, names: &[S]) -> &mut Self {
        let joined: Vec<&str> = names.iter().map(|s| s.as_ref()).collect();
        self.str(&joined.join(","))
    }
    pub fn finish(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.buf)
    }
}

/// Encodes an unsigned big-endian magnitude as a standalone `mpint`.
pub fn mpint_bytes(v: &[u8]) -> Vec<u8> {
    Writer::new().mpint(v).finish()
}

/// Reader over SSH wire data.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

fn short() -> SshError {
    SshError::protocol("truncated message")
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Reader<'a> {
        Reader { data, pos: 0 }
    }
    pub fn remaining(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }
    pub fn take(&mut self, n: usize) -> SshResult<&'a [u8]> {
        if self.data.len() - self.pos < n {
            return Err(short());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> SshResult<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn bool(&mut self) -> SshResult<bool> {
        Ok(self.u8()? != 0)
    }
    pub fn u32(&mut self) -> SshResult<u32> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
    pub fn u64(&mut self) -> SshResult<u64> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }
    pub fn string(&mut self) -> SshResult<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    /// A string decoded as UTF-8 (invalid sequences replaced).
    pub fn str(&mut self) -> SshResult<String> {
        Ok(String::from_utf8_lossy(self.string()?).into_owned())
    }
    /// An `mpint` that must be non-negative; returns the magnitude without leading zeros.
    pub fn mpint(&mut self) -> SshResult<&'a [u8]> {
        let v = self.string()?;
        if !v.is_empty() && v[0] & 0x80 != 0 {
            return Err(SshError::protocol("negative mpint"));
        }
        let mut i = 0;
        while i < v.len() && v[i] == 0 {
            i += 1;
        }
        Ok(&v[i..])
    }
    pub fn name_list(&mut self) -> SshResult<Vec<String>> {
        let s = self.str()?;
        Ok(if s.is_empty() { Vec::new() } else { s.split(',').map(|x| x.to_string()).collect() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc4251_mpint_examples() {
        assert_eq!(mpint_bytes(&[]), vec![0, 0, 0, 0]);
        assert_eq!(
            mpint_bytes(&[0x09, 0xa3, 0x78, 0xf9, 0xb2, 0xe3, 0x32, 0xa7]),
            vec![0, 0, 0, 8, 0x09, 0xa3, 0x78, 0xf9, 0xb2, 0xe3, 0x32, 0xa7]
        );
        assert_eq!(mpint_bytes(&[0x80]), vec![0, 0, 0, 2, 0, 0x80]);
        assert_eq!(mpint_bytes(&[0, 0, 0x7f]), vec![0, 0, 0, 1, 0x7f]);
        let enc = Writer::new().mpint(&[0x80]).u32(7).name_list(&["a", "bc"]).finish();
        let mut r = Reader::new(&enc);
        assert_eq!(r.mpint().unwrap(), &[0x80]);
        assert_eq!(r.u32().unwrap(), 7);
        assert_eq!(r.name_list().unwrap(), vec!["a", "bc"]);
        assert!(r.is_empty());
        assert!(r.u8().is_err());
        assert!(Reader::new(&[0, 0, 0, 1, 0xff]).mpint().is_err());
    }
}
