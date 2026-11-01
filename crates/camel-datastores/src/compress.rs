//! Std-only LZ4 (block format) and Snappy (raw format) codecs, as used by the CQL native
//! protocol frame compression (`COMPRESSION` = `lz4` / `snappy`).

use camel_core::error::{CamelError, Result};

fn corrupt(what: &str) -> CamelError {
    CamelError::io(format!("corrupt {} data", what))
}

const MIN_MATCH: usize = 4;
const HASH_BITS: u32 = 14;

fn hash4(b: &[u8]) -> usize {
    let v = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    (v.wrapping_mul(2654435761) >> (32 - HASH_BITS)) as usize
}

fn write_lz4_len(out: &mut Vec<u8>, mut n: usize) {
    while n >= 255 {
        out.push(255);
        n -= 255;
    }
    out.push(n as u8);
}

/// LZ4 block compression (no frame header).
pub fn lz4_compress(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / 2 + 16);
    let mut table = vec![usize::MAX; 1 << HASH_BITS];
    let n = src.len();
    let mut anchor = 0;
    let mut i = 0;
    // the last match must start at least 12 bytes before the end, the last 5 bytes are literals
    let match_limit = n.saturating_sub(12);
    while i < match_limit {
        let h = hash4(&src[i..]);
        let cand = table[h];
        table[h] = i;
        if cand != usize::MAX && i - cand <= 0xFFFF && src[cand..cand + 4] == src[i..i + 4] {
            let mut len = MIN_MATCH;
            while i + len < n - 5 && src[cand + len] == src[i + len] {
                len += 1;
            }
            let lit = i - anchor;
            let ml = len - MIN_MATCH;
            out.push(((lit.min(15) as u8) << 4) | ml.min(15) as u8);
            if lit >= 15 {
                write_lz4_len(&mut out, lit - 15);
            }
            out.extend_from_slice(&src[anchor..i]);
            out.extend_from_slice(&((i - cand) as u16).to_le_bytes());
            if ml >= 15 {
                write_lz4_len(&mut out, ml - 15);
            }
            i += len;
            anchor = i;
        } else {
            i += 1;
        }
    }
    let lit = n - anchor;
    out.push((lit.min(15) as u8) << 4);
    if lit >= 15 {
        write_lz4_len(&mut out, lit - 15);
    }
    out.extend_from_slice(&src[anchor..]);
    out
}

/// LZ4 block decompression; `max` bounds the output size.
pub fn lz4_decompress(src: &[u8], max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(max.min(1 << 24));
    let mut i = 0;
    let read_len = |i: &mut usize, mut n: usize| -> Result<usize> {
        loop {
            let b = *src.get(*i).ok_or_else(|| corrupt("lz4"))?;
            *i += 1;
            n += b as usize;
            if b != 255 {
                return Ok(n);
            }
        }
    };
    while i < src.len() {
        let token = src[i];
        i += 1;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            lit = read_len(&mut i, lit)?;
        }
        let lits = src.get(i..i + lit).ok_or_else(|| corrupt("lz4"))?;
        out.extend_from_slice(lits);
        i += lit;
        if out.len() > max {
            return Err(corrupt("lz4"));
        }
        if i >= src.len() {
            break; // last sequence: literals only
        }
        let off = u16::from_le_bytes([*src.get(i).ok_or_else(|| corrupt("lz4"))?, *src.get(i + 1).ok_or_else(|| corrupt("lz4"))?]) as usize;
        i += 2;
        if off == 0 || off > out.len() {
            return Err(corrupt("lz4"));
        }
        let mut ml = (token & 15) as usize;
        if ml == 15 {
            ml = read_len(&mut i, ml)?;
        }
        ml += MIN_MATCH;
        if out.len() + ml > max {
            return Err(corrupt("lz4"));
        }
        let start = out.len() - off;
        for k in 0..ml {
            let b = out[start + k];
            out.push(b);
        }
    }
    Ok(out)
}

fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn snappy_literal(out: &mut Vec<u8>, lit: &[u8]) {
    let n = lit.len() - 1;
    if n < 60 {
        out.push((n as u8) << 2);
    } else if n < 1 << 8 {
        out.push(60 << 2);
        out.push(n as u8);
    } else if n < 1 << 16 {
        out.push(61 << 2);
        out.extend_from_slice(&(n as u16).to_le_bytes());
    } else if n < 1 << 24 {
        out.push(62 << 2);
        out.extend_from_slice(&(n as u32).to_le_bytes()[..3]);
    } else {
        out.push(63 << 2);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    }
    out.extend_from_slice(lit);
}

fn snappy_copy(out: &mut Vec<u8>, off: usize, mut len: usize) {
    while len > 0 {
        let l = if len > 64 { if len - 64 < 4 { 60 } else { 64 } } else { len };
        if (4..12).contains(&l) && off < 2048 {
            out.push((((off >> 8) as u8) << 5) | (((l - 4) as u8) << 2) | 1);
            out.push(off as u8);
        } else {
            out.push((((l - 1) as u8) << 2) | 2);
            out.extend_from_slice(&(off as u16).to_le_bytes());
        }
        len -= l;
    }
}

/// Snappy raw compression.
pub fn snappy_compress(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / 2 + 16);
    put_uvarint(&mut out, src.len() as u64);
    let mut table = vec![usize::MAX; 1 << HASH_BITS];
    let n = src.len();
    let mut anchor = 0;
    let mut i = 0;
    while i + MIN_MATCH <= n {
        let h = hash4(&src[i..]);
        let cand = table[h];
        table[h] = i;
        if cand != usize::MAX && i - cand <= 0xFFFF && src[cand..cand + 4] == src[i..i + 4] {
            let mut len = MIN_MATCH;
            while i + len < n && src[cand + len] == src[i + len] {
                len += 1;
            }
            if anchor < i {
                snappy_literal(&mut out, &src[anchor..i]);
            }
            snappy_copy(&mut out, i - cand, len);
            i += len;
            anchor = i;
        } else {
            i += 1;
        }
    }
    if anchor < n {
        snappy_literal(&mut out, &src[anchor..]);
    }
    out
}

/// Snappy raw decompression; `max` bounds the output size.
pub fn snappy_decompress(src: &[u8], max: usize) -> Result<Vec<u8>> {
    let mut i = 0;
    let mut len: u64 = 0;
    let mut shift = 0;
    loop {
        let b = *src.get(i).ok_or_else(|| corrupt("snappy"))?;
        i += 1;
        len |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 35 {
            return Err(corrupt("snappy"));
        }
    }
    let len = len as usize;
    if len > max {
        return Err(corrupt("snappy"));
    }
    let mut out = Vec::with_capacity(len);
    while i < src.len() {
        let tag = src[i];
        i += 1;
        match tag & 3 {
            0 => {
                let mut l = (tag >> 2) as usize;
                if l >= 60 {
                    let nb = l - 59;
                    let b = src.get(i..i + nb).ok_or_else(|| corrupt("snappy"))?;
                    l = b.iter().rev().fold(0usize, |a, &x| (a << 8) | x as usize);
                    i += nb;
                }
                let lit = src.get(i..i + l + 1).ok_or_else(|| corrupt("snappy"))?;
                out.extend_from_slice(lit);
                i += l + 1;
            }
            t => {
                let (l, off) = match t {
                    1 => {
                        let b = *src.get(i).ok_or_else(|| corrupt("snappy"))? as usize;
                        i += 1;
                        (((tag >> 2) & 7) as usize + 4, (((tag >> 5) as usize) << 8) | b)
                    }
                    2 => {
                        let b = src.get(i..i + 2).ok_or_else(|| corrupt("snappy"))?;
                        i += 2;
                        ((tag >> 2) as usize + 1, u16::from_le_bytes([b[0], b[1]]) as usize)
                    }
                    _ => {
                        let b = src.get(i..i + 4).ok_or_else(|| corrupt("snappy"))?;
                        i += 4;
                        ((tag >> 2) as usize + 1, u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
                    }
                };
                if off == 0 || off > out.len() || out.len() + l > len {
                    return Err(corrupt("snappy"));
                }
                let start = out.len() - off;
                for k in 0..l {
                    let b = out[start + k];
                    out.push(b);
                }
            }
        }
        if out.len() > len {
            return Err(corrupt("snappy"));
        }
    }
    if out.len() != len {
        return Err(corrupt("snappy"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Vec<u8>> {
        let mut v = vec![Vec::new(), b"a".to_vec(), b"abcdefgh".to_vec(), b"hello hello hello hello hello world".to_vec()];
        v.push((0..5000u32).map(|i| (i % 7) as u8).collect());
        v.push((0..70000u32).map(|i| (i.wrapping_mul(2654435761u32) >> 24) as u8).collect());
        v.push(vec![0u8; 100_000]);
        v.push("SELECT * FROM ks.table WHERE id = ? -- ".repeat(300).into_bytes());
        v
    }

    #[test]
    fn lz4_roundtrip_and_reference() {
        for s in samples() {
            let c = lz4_compress(&s);
            assert_eq!(lz4_decompress(&c, s.len()).unwrap(), s);
        }
        // reference block from the lz4 tool: "aaaaaaaaaaaaaaaaaaaa" (20 x 'a')
        let refblock = [0x1Au8, b'a', 0x01, 0x00, 0x50, b'a', b'a', b'a', b'a', b'a'];
        assert_eq!(lz4_decompress(&refblock, 100).unwrap(), vec![b'a'; 20]);
        assert!(lz4_decompress(&[0x10, b'a', 0x05, 0x00], 100).is_err());
        assert!(lz4_compress(&vec![0u8; 100_000]).len() < 1000);
    }

    #[test]
    fn snappy_roundtrip_and_reference() {
        for s in samples() {
            let c = snappy_compress(&s);
            assert_eq!(snappy_decompress(&c, s.len()).unwrap(), s);
        }
        // reference (python-snappy): compress(b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaa") (30 x 'a')
        let r2 = [0x1Eu8, 0x00, b'a', 0x72, 0x01, 0x00];
        assert_eq!(snappy_decompress(&r2, 100).unwrap(), vec![b'a'; 30]);
        assert!(snappy_decompress(&[0x05, 0x08, b'a'], 100).is_err());
    }
}
