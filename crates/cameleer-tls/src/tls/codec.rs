//! TLS presentation-language encoding helpers.

use super::{TlsError, TlsResult};

/// Cursor over received bytes; every accessor fails with decode_error on truncation.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    what: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8], what: &'static str) -> Reader<'a> {
        Reader { data, pos: 0, what }
    }
    fn err(&self) -> TlsError {
        TlsError::decode(self.what)
    }
    pub fn is_empty(&self) -> bool {
        self.pos >= self.data.len()
    }
    pub fn take(&mut self, n: usize) -> TlsResult<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(self.err());
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> TlsResult<u8> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> TlsResult<u16> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    pub fn u24(&mut self) -> TlsResult<usize> {
        let b = self.take(3)?;
        Ok((b[0] as usize) << 16 | (b[1] as usize) << 8 | b[2] as usize)
    }
    pub fn vec8(&mut self) -> TlsResult<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }
    pub fn vec16(&mut self) -> TlsResult<&'a [u8]> {
        let n = self.u16()? as usize;
        self.take(n)
    }
    pub fn vec24(&mut self) -> TlsResult<&'a [u8]> {
        let n = self.u24()?;
        self.take(n)
    }
    /// Fails unless every byte has been consumed.
    pub fn finish(&self) -> TlsResult<()> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(self.err())
        }
    }
}

pub(crate) fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}

pub(crate) fn put_u24(out: &mut Vec<u8>, v: usize) {
    out.extend_from_slice(&[(v >> 16) as u8, (v >> 8) as u8, v as u8]);
}

pub(crate) fn put_vec8(out: &mut Vec<u8>, data: &[u8]) {
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

pub(crate) fn put_vec16(out: &mut Vec<u8>, data: &[u8]) {
    put_u16(out, data.len() as u16);
    out.extend_from_slice(data);
}

pub(crate) fn put_vec24(out: &mut Vec<u8>, data: &[u8]) {
    put_u24(out, data.len());
    out.extend_from_slice(data);
}

/// Wraps a handshake body with its 4-byte header.
pub(crate) fn handshake_msg(typ: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![typ];
    put_vec24(&mut m, body);
    m
}

/// Encodes an extension block (u16 total length followed by entries).
pub(crate) fn extensions(exts: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut body = Vec::new();
    for (t, d) in exts {
        put_u16(&mut body, *t);
        put_vec16(&mut body, d);
    }
    let mut out = Vec::new();
    put_vec16(&mut out, &body);
    out
}

/// Parses an extension block body into (type, data) pairs, rejecting duplicates.
pub(crate) fn parse_extensions(data: &[u8]) -> TlsResult<Vec<(u16, Vec<u8>)>> {
    let mut r = Reader::new(data, "extensions");
    let mut out: Vec<(u16, Vec<u8>)> = Vec::new();
    while !r.is_empty() {
        let t = r.u16()?;
        let d = r.vec16()?;
        if out.iter().any(|(x, _)| *x == t) {
            return Err(TlsError::proto(super::alert::ILLEGAL_PARAMETER, format!("duplicate extension {t}")));
        }
        out.push((t, d.to_vec()));
    }
    Ok(out)
}

pub(crate) fn find_ext(exts: &[(u16, Vec<u8>)], t: u16) -> Option<&[u8]> {
    exts.iter().find(|(x, _)| *x == t).map(|(_, d)| d.as_slice())
}

/// Parses a `uint16 list<2..2^16-2>` (signature_algorithms, supported_groups).
pub(crate) fn parse_u16_list(data: &[u8], what: &'static str) -> TlsResult<Vec<u16>> {
    let mut r = Reader::new(data, what);
    let body = r.vec16()?;
    r.finish()?;
    if body.len() % 2 != 0 {
        return Err(TlsError::decode(what));
    }
    Ok(body.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
}

pub(crate) fn u16_list(vals: &[u16]) -> Vec<u8> {
    let mut body = Vec::new();
    for v in vals {
        put_u16(&mut body, *v);
    }
    let mut out = Vec::new();
    put_vec16(&mut out, &body);
    out
}

/// ALPN `ProtocolNameList`.
pub(crate) fn alpn_list(protos: &[Vec<u8>]) -> Vec<u8> {
    let mut body = Vec::new();
    for p in protos {
        put_vec8(&mut body, p);
    }
    let mut out = Vec::new();
    put_vec16(&mut out, &body);
    out
}

pub(crate) fn parse_alpn_list(data: &[u8]) -> TlsResult<Vec<Vec<u8>>> {
    let mut r = Reader::new(data, "ALPN extension");
    let body = r.vec16()?;
    r.finish()?;
    let mut lr = Reader::new(body, "ALPN extension");
    let mut out = Vec::new();
    while !lr.is_empty() {
        let p = lr.vec8()?;
        if p.is_empty() {
            return Err(TlsError::decode("ALPN extension"));
        }
        out.push(p.to_vec());
    }
    if out.is_empty() {
        return Err(TlsError::decode("ALPN extension"));
    }
    Ok(out)
}
