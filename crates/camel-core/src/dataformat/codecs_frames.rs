//! Containers around the LZ4 / Snappy block codecs used by Kafka record batches: the LZ4
//! frame format (with xxHash32 checksums) and the xerial `snappy-java` stream framing.

use super::{corrupt, lz4_compress, lz4_decompress, snappy_compress, snappy_decompress, MIN_MATCH};
use crate::error::{CamelError, Result};

/// xxHash32 (used by LZ4 frame checksums).
pub fn xxhash32(data: &[u8], seed: u32) -> u32 {
    const P1: u32 = 2654435761;
    const P2: u32 = 2246822519;
    const P3: u32 = 3266489917;
    const P4: u32 = 668265263;
    const P5: u32 = 374761393;
    let round = |acc: u32, lane: u32| acc.wrapping_add(lane.wrapping_mul(P2)).rotate_left(13).wrapping_mul(P1);
    let le = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let n = data.len();
    let mut i = 0;
    let mut h = if n >= 16 {
        let (mut v1, mut v2, mut v3, mut v4) = (seed.wrapping_add(P1).wrapping_add(P2), seed.wrapping_add(P2), seed, seed.wrapping_sub(P1));
        while i + 16 <= n {
            v1 = round(v1, le(&data[i..]));
            v2 = round(v2, le(&data[i + 4..]));
            v3 = round(v3, le(&data[i + 8..]));
            v4 = round(v4, le(&data[i + 12..]));
            i += 16;
        }
        v1.rotate_left(1).wrapping_add(v2.rotate_left(7)).wrapping_add(v3.rotate_left(12)).wrapping_add(v4.rotate_left(18))
    } else {
        seed.wrapping_add(P5)
    };
    h = h.wrapping_add(n as u32);
    while i + 4 <= n {
        h = h.wrapping_add(le(&data[i..]).wrapping_mul(P3)).rotate_left(17).wrapping_mul(P4);
        i += 4;
    }
    while i < n {
        h = h.wrapping_add((data[i] as u32).wrapping_mul(P5)).rotate_left(11).wrapping_mul(P1);
        i += 1;
    }
    h ^= h >> 15;
    h = h.wrapping_mul(P2);
    h ^= h >> 13;
    h = h.wrapping_mul(P3);
    h ^ (h >> 16)
}

const LZ4_FRAME_MAGIC: u32 = 0x184D2204;
const LZ4_BLOCK_MAX: usize = 64 * 1024;

/// LZ4 frame compression (independent 64 KiB blocks, content checksum), the format of
/// Kafka's `compression.type=lz4` record batches.
pub fn lz4_frame_compress(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len() / 2 + 32);
    out.extend_from_slice(&LZ4_FRAME_MAGIC.to_le_bytes());
    // FLG: version 01, block independence, content checksum; BD: 64 KiB maximum block size
    let header = [0x40 | 0x20 | 0x04, 0x40];
    out.extend_from_slice(&header);
    out.push((xxhash32(&header, 0) >> 8) as u8);
    for chunk in src.chunks(LZ4_BLOCK_MAX) {
        let c = lz4_compress(chunk);
        if c.len() < chunk.len() {
            out.extend_from_slice(&(c.len() as u32).to_le_bytes());
            out.extend_from_slice(&c);
        } else {
            out.extend_from_slice(&(chunk.len() as u32 | 0x8000_0000).to_le_bytes());
            out.extend_from_slice(chunk);
        }
    }
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&xxhash32(src, 0).to_le_bytes());
    out
}

fn take<'a>(src: &'a [u8], i: &mut usize, n: usize) -> Result<&'a [u8]> {
    let b = src.get(*i..*i + n).ok_or_else(|| corrupt("lz4 frame"))?;
    *i += n;
    Ok(b)
}

fn u32_le(src: &[u8], i: &mut usize) -> Result<u32> {
    let b = take(src, i, 4)?;
    Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

/// LZ4 frame decompression (concatenated and skippable frames are accepted); header and
/// content checksums are verified when present, `max` bounds the output size.
pub fn lz4_frame_decompress(src: &[u8], max: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < src.len() {
        let magic = u32_le(src, &mut i)?;
        if magic & 0xFFFF_FFF0 == 0x184D2A50 {
            let n = u32_le(src, &mut i)? as usize;
            take(src, &mut i, n)?;
            continue;
        }
        if magic != LZ4_FRAME_MAGIC {
            return Err(corrupt("lz4 frame"));
        }
        let start = i;
        let flg = take(src, &mut i, 1)?[0];
        let bd = take(src, &mut i, 1)?[0];
        if flg >> 6 != 1 {
            return Err(corrupt("lz4 frame"));
        }
        if flg & 0x08 != 0 {
            take(src, &mut i, 8)?; // content size
        }
        if flg & 0x01 != 0 {
            take(src, &mut i, 4)?; // dictionary id
        }
        let hc = take(src, &mut i, 1)?[0];
        if hc != (xxhash32(&src[start..i - 1], 0) >> 8) as u8 {
            return Err(CamelError::io("corrupt lz4 frame data (header checksum)"));
        }
        let block_max = match (bd >> 4) & 7 {
            4 => 64 << 10,
            5 => 256 << 10,
            6 => 1 << 20,
            7 => 4 << 20,
            _ => return Err(corrupt("lz4 frame")),
        };
        let frame_start = out.len();
        loop {
            let size = u32_le(src, &mut i)?;
            if size == 0 {
                break;
            }
            let n = (size & 0x7FFF_FFFF) as usize;
            if n > block_max {
                return Err(corrupt("lz4 frame"));
            }
            let block = take(src, &mut i, n)?;
            if size & 0x8000_0000 != 0 {
                out.extend_from_slice(block);
            } else if flg & 0x20 != 0 {
                out.extend(lz4_decompress(block, block_max)?);
            } else {
                // linked blocks may copy from the previous 64 KiB of the frame
                let dict_start = out.len().saturating_sub(64 << 10).max(frame_start);
                let decoded = lz4_decompress_with_prefix(&out[dict_start..], block, block_max)?;
                out.extend(decoded);
            }
            if flg & 0x10 != 0 {
                take(src, &mut i, 4)?; // block checksum
            }
            if out.len() > max {
                return Err(corrupt("lz4 frame"));
            }
        }
        if flg & 0x04 != 0 {
            let c = u32_le(src, &mut i)?;
            if c != xxhash32(&out[frame_start..], 0) {
                return Err(CamelError::io("corrupt lz4 frame data (content checksum)"));
            }
        }
    }
    Ok(out)
}

fn lz4_len(src: &[u8], i: &mut usize, mut n: usize) -> Result<usize> {
    if n == 15 {
        loop {
            let b = *src.get(*i).ok_or_else(|| corrupt("lz4"))?;
            *i += 1;
            n += b as usize;
            if b != 255 {
                break;
            }
        }
    }
    Ok(n)
}

/// LZ4 block decompression whose matches may reach into `prefix` (linked blocks).
fn lz4_decompress_with_prefix(prefix: &[u8], src: &[u8], max: usize) -> Result<Vec<u8>> {
    let mut joined = prefix.to_vec();
    let base = prefix.len();
    let mut i = 0;
    loop {
        let token = *src.get(i).ok_or_else(|| corrupt("lz4"))?;
        i += 1;
        let lit = lz4_len(src, &mut i, (token >> 4) as usize)?;
        joined.extend_from_slice(src.get(i..i + lit).ok_or_else(|| corrupt("lz4"))?);
        i += lit;
        if i == src.len() {
            break;
        }
        let off = src.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(|| corrupt("lz4"))?;
        i += 2;
        let ml = lz4_len(src, &mut i, (token & 15) as usize)? + MIN_MATCH;
        if off == 0 || off > joined.len() || joined.len() - base + ml > max {
            return Err(corrupt("lz4"));
        }
        let start = joined.len() - off;
        for k in 0..ml {
            let b = joined[start + k];
            joined.push(b);
        }
    }
    Ok(joined.split_off(base))
}

const XERIAL_MAGIC: [u8; 8] = [0x82, b'S', b'N', b'A', b'P', b'P', b'Y', 0];

/// Snappy compression in the xerial `snappy-java` stream framing (the Kafka Java client's
/// `compression.type=snappy` format): magic, version 1, compatible version 1, then
/// length-prefixed raw Snappy chunks of up to 32 KiB.
pub fn snappy_java_compress(src: &[u8]) -> Vec<u8> {
    let mut out = XERIAL_MAGIC.to_vec();
    out.extend_from_slice(&1i32.to_be_bytes());
    out.extend_from_slice(&1i32.to_be_bytes());
    for chunk in src.chunks(32 * 1024) {
        let c = snappy_compress(chunk);
        out.extend_from_slice(&(c.len() as u32).to_be_bytes());
        out.extend_from_slice(&c);
    }
    out
}

/// Snappy decompression of either the xerial `snappy-java` framing or a raw Snappy block
/// (librdkafka writes raw blocks; Java's `SnappyInputStream` accepts both).
pub fn snappy_java_decompress(src: &[u8], max: usize) -> Result<Vec<u8>> {
    if src.len() < 16 || src[..8] != XERIAL_MAGIC {
        return snappy_decompress(src, max);
    }
    let mut out = Vec::new();
    let mut i = 16;
    while i < src.len() {
        let b = src.get(i..i + 4).ok_or_else(|| corrupt("snappy-java"))?;
        let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize;
        i += 4;
        let chunk = src.get(i..i + n).ok_or_else(|| corrupt("snappy-java"))?;
        i += n;
        out.extend(snappy_decompress(chunk, max.saturating_sub(out.len()))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<Vec<u8>> {
        vec![
            Vec::new(),
            b"a".to_vec(),
            b"hello hello hello hello hello world".to_vec(),
            (0..70000u32).map(|i| (i.wrapping_mul(2654435761u32) >> 24) as u8).collect(),
            vec![0u8; 200_000],
            "record batch payload -- ".repeat(5000).into_bytes(),
        ]
    }

    #[test]
    fn xxhash32_reference_values() {
        // reference values from the xxHash test vectors / python-xxhash
        assert_eq!(xxhash32(b"", 0), 0x02CC5D05);
        assert_eq!(xxhash32(b"a", 0), 0x550D7456);
        assert_eq!(xxhash32(b"abc", 0), 0x32D153FF);
        assert_eq!(xxhash32(b"Nobody inspects the spammish repetition", 0), 0xE2293B2F);
    }

    #[test]
    fn lz4_frame_roundtrip_and_checksums() {
        for s in samples() {
            let c = lz4_frame_compress(&s);
            assert_eq!(lz4_frame_decompress(&c, s.len() + 1).unwrap(), s);
        }
        let mut bad = lz4_frame_compress(b"hello hello hello hello");
        let n = bad.len();
        bad[n - 1] ^= 1;
        assert!(lz4_frame_decompress(&bad, 100).is_err(), "content checksum is verified");
        let mut bad_header = lz4_frame_compress(b"x");
        bad_header[6] ^= 1;
        assert!(lz4_frame_decompress(&bad_header, 100).is_err(), "header checksum is verified");
        assert!(lz4_frame_decompress(&lz4_frame_compress(&[7u8; 1000]), 10).is_err(), "bounded output");
    }

    #[test]
    fn snappy_java_framing() {
        for s in samples() {
            let c = snappy_java_compress(&s);
            assert_eq!(&c[..8], &XERIAL_MAGIC);
            assert_eq!(snappy_java_decompress(&c, s.len()).unwrap(), s);
            // raw blocks (librdkafka) are accepted too
            assert_eq!(snappy_java_decompress(&snappy_compress(&s), s.len()).unwrap(), s);
        }
    }
}
