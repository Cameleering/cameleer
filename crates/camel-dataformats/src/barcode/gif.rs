//! Minimal GIF codec: writes GIF89a images with a 256-level gray palette (LZW, 8-bit minimum
//! code size) and reads the first image of any GIF87a/89a file (global / local color tables,
//! interlacing, extensions skipped) into an 8-bit luminance raster.

use std::collections::HashMap;

use camel_core::error::Result;

use super::png::Gray;
use crate::df_err;

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, code: u32, size: u32) {
        self.acc |= code << self.n;
        self.n += size;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push(self.acc as u8);
        }
        self.out
    }
}

/// LZW-compress `data` (symbols < 2^min_size) as GIF image data.
pub fn lzw_encode(data: &[u8], min_size: u32) -> Vec<u8> {
    let clear = 1u32 << min_size;
    let eoi = clear + 1;
    let mut w = BitWriter { out: Vec::new(), acc: 0, n: 0 };
    let mut size = min_size + 1;
    let mut dict: HashMap<(u32, u8), u32> = HashMap::new();
    let mut next = eoi + 1;
    w.put(clear, size);
    let mut it = data.iter();
    let Some(&first) = it.next() else {
        w.put(eoi, size);
        return w.finish();
    };
    let mut prefix = first as u32;
    for &b in it {
        if let Some(&c) = dict.get(&(prefix, b)) {
            prefix = c;
            continue;
        }
        w.put(prefix, size);
        if next < 4096 {
            dict.insert((prefix, b), next);
            next += 1;
            if next > (1 << size) && size < 12 {
                size += 1;
            }
        } else {
            w.put(clear, size);
            dict.clear();
            next = eoi + 1;
            size = min_size + 1;
        }
        prefix = b as u32;
    }
    w.put(prefix, size);
    // the decoder adds an entry after this code too
    if next < 4096 && next + 1 > (1 << size) && size < 12 {
        size += 1;
    }
    w.put(eoi, size);
    w.finish()
}

/// LZW-decompress GIF image data.
pub fn lzw_decode(data: &[u8], min_size: u32, max_out: usize) -> Result<Vec<u8>> {
    if !(1..=11).contains(&min_size) {
        return Err(df_err("Invalid GIF LZW code size"));
    }
    let clear = 1usize << min_size;
    let eoi = clear + 1;
    let mut prefix: Vec<u16> = vec![0; 4096];
    let mut suffix: Vec<u8> = vec![0; 4096];
    let mut first: Vec<u8> = vec![0; 4096];
    for i in 0..clear {
        suffix[i] = i as u8;
        first[i] = i as u8;
    }
    let mut size = min_size + 1;
    let mut next = eoi + 1;
    let mut prev: Option<usize> = None;
    let mut out = Vec::new();
    let (mut acc, mut n, mut pos) = (0u32, 0u32, 0usize);
    let mut stack = Vec::new();
    loop {
        while n < size {
            let Some(&b) = data.get(pos) else { return Ok(out) };
            acc |= (b as u32) << n;
            n += 8;
            pos += 1;
        }
        let code = (acc & ((1 << size) - 1)) as usize;
        acc >>= size;
        n -= size;
        if code == clear {
            size = min_size + 1;
            next = eoi + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            return Ok(out);
        }
        let Some(p) = prev else {
            if code >= clear {
                return Err(df_err("Invalid first GIF LZW code"));
            }
            out.push(code as u8);
            prev = Some(code);
            continue;
        };
        let (emit, fc) = if code < next {
            (code, first[code])
        } else if code == next {
            (usize::MAX, first[p])
        } else {
            return Err(df_err("Invalid GIF LZW code"));
        };
        if next < 4096 {
            prefix[next] = p as u16;
            suffix[next] = fc;
            first[next] = first[p];
            next += 1;
            if next == (1 << size) && size < 12 {
                size += 1;
            }
        }
        let mut c = if emit == usize::MAX { next - 1 } else { emit };
        stack.clear();
        while c >= clear {
            stack.push(suffix[c]);
            c = prefix[c] as usize;
        }
        stack.push(c as u8);
        out.extend(stack.iter().rev());
        if out.len() > max_out {
            return Err(df_err("GIF image data larger than its dimensions"));
        }
        prev = Some(if emit == usize::MAX { next - 1 } else { emit });
    }
}

/// Encode a gray raster as GIF89a.
pub fn encode(img: &Gray) -> Vec<u8> {
    let mut out = b"GIF89a".to_vec();
    out.extend_from_slice(&(img.width as u16).to_le_bytes());
    out.extend_from_slice(&(img.height as u16).to_le_bytes());
    out.push(0xf7); // global color table, 8 bits color resolution, 256 entries
    out.push(0); // background
    out.push(0); // aspect
    for i in 0..=255u8 {
        out.extend_from_slice(&[i, i, i]);
    }
    out.push(0x2c);
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(img.width as u16).to_le_bytes());
    out.extend_from_slice(&(img.height as u16).to_le_bytes());
    out.push(0);
    out.push(8);
    let data = lzw_encode(&img.pixels, 8);
    for c in data.chunks(255) {
        out.push(c.len() as u8);
        out.extend_from_slice(c);
    }
    out.push(0);
    out.push(0x3b);
    out
}

/// Whether bytes look like a GIF.
pub fn is_gif(b: &[u8]) -> bool {
    b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")
}

/// Decode the first image of a GIF into luminance.
pub fn decode(b: &[u8]) -> Result<Gray> {
    if !is_gif(b) || b.len() < 13 {
        return Err(df_err("Not a GIF image"));
    }
    let short = || df_err("Truncated GIF image");
    let sw = u16::from_le_bytes([b[6], b[7]]) as usize;
    let sh = u16::from_le_bytes([b[8], b[9]]) as usize;
    let flags = b[10];
    let bg = b[11];
    let mut pos = 13;
    let read_table = |pos: &mut usize, size_bits: u8| -> Result<Vec<u8>> {
        let n = 2usize << size_bits;
        let t = b.get(*pos..*pos + 3 * n).ok_or_else(short)?;
        *pos += 3 * n;
        Ok(t.chunks(3).map(|c| ((c[0] as u32 * 299 + c[1] as u32 * 587 + c[2] as u32 * 114) / 1000) as u8).collect())
    };
    let global = if flags & 0x80 != 0 { Some(read_table(&mut pos, flags & 7)?) } else { None };
    let mut transparent: Option<u8> = None;
    loop {
        match *b.get(pos).ok_or_else(short)? {
            0x21 => {
                let label = *b.get(pos + 1).ok_or_else(short)?;
                pos += 2;
                let mut first = true;
                loop {
                    let n = *b.get(pos).ok_or_else(short)? as usize;
                    if label == 0xf9 && first && n >= 4 {
                        let packed = *b.get(pos + 1).ok_or_else(short)?;
                        if packed & 1 != 0 {
                            transparent = b.get(pos + 4).copied();
                        }
                    }
                    first = false;
                    pos += 1 + n;
                    if n == 0 {
                        break;
                    }
                }
            }
            0x2c => {
                let d = b.get(pos + 1..pos + 10).ok_or_else(short)?;
                let (ix, iy) = (u16::from_le_bytes([d[0], d[1]]) as usize, u16::from_le_bytes([d[2], d[3]]) as usize);
                let (iw, ih) = (u16::from_le_bytes([d[4], d[5]]) as usize, u16::from_le_bytes([d[6], d[7]]) as usize);
                let lflags = d[8];
                pos += 10;
                let table = if lflags & 0x80 != 0 { read_table(&mut pos, lflags & 7)? } else { global.clone().ok_or_else(|| df_err("GIF image without a color table"))? };
                let min = *b.get(pos).ok_or_else(short)? as u32;
                pos += 1;
                let mut data = Vec::new();
                loop {
                    let n = *b.get(pos).ok_or_else(short)? as usize;
                    pos += 1;
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(b.get(pos..pos + n).ok_or_else(short)?);
                    pos += n;
                }
                let idx = lzw_decode(&data, min, iw * ih + 4096)?;
                let (w, h) = (sw.max(ix + iw), sh.max(iy + ih));
                if w == 0 || h == 0 || w * h > 100_000_000 {
                    return Err(df_err("Invalid GIF dimensions"));
                }
                let bgv = table.get(bg as usize).copied().unwrap_or(255);
                let mut g = Gray { width: w, height: h, pixels: vec![if global.is_some() { bgv } else { 255 }; w * h] };
                let rows: Vec<usize> = if lflags & 0x40 != 0 { (0..ih).step_by(8).chain((4..ih).step_by(8)).chain((2..ih).step_by(4)).chain((1..ih).step_by(2)).collect() } else { (0..ih).collect() };
                for (k, row) in rows.iter().enumerate() {
                    for x in 0..iw {
                        if let Some(&i) = idx.get(k * iw + x) {
                            if Some(i) == transparent {
                                continue;
                            }
                            g.pixels[(iy + row) * w + ix + x] = table.get(i as usize).copied().unwrap_or(0);
                        }
                    }
                }
                return Ok(g);
            }
            0x3b => return Err(df_err("GIF without an image")),
            _ => return Err(df_err("Invalid GIF block")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lzw_round_trip() {
        for data in [vec![], vec![7u8], b"TOBEORNOTTOBEORTOBEORNOT".to_vec(), (0..20000).map(|i| (i * 7 % 251) as u8).collect::<Vec<_>>(), vec![0u8; 50000]] {
            let e = lzw_encode(&data, 8);
            assert_eq!(lzw_decode(&e, 8, 1 << 20).unwrap(), data);
        }
    }

    #[test]
    fn image_round_trip_and_pillow_gif() {
        let g = Gray { width: 37, height: 21, pixels: (0..37 * 21).map(|i| if (i / 3) % 2 == 0 { 0 } else { 255 }).collect() };
        assert_eq!(decode(&encode(&g)).unwrap(), g);
        // 9x5 GIF written by Pillow (palette, 5-bit LZW)
        let pillow = camel_tls::util::try_unhex("474946383761090005008400000000000404040e0e0e1414141818181e1e1e2222222828282c2c2c3232323c3c3c4040404646465050505a5a5a6464646e6e6e7878788282828c8c8c969696a0a0a0aaaaaab4b4b4bebebec8c8c8d2d2d2dcdcdce6e6e6f0f0f0fafafa0000002c000000000900050000082f00011450e02002850b1a3a0c48d000c2040b19380438c0e081840a1836782040d020c20e0210347c18318081050101003b").unwrap();
        let want: Vec<u8> = vec![0, 30, 60, 90, 120, 150, 180, 210, 240, 20, 50, 80, 110, 140, 170, 200, 230, 4, 40, 70, 100, 130, 160, 190, 220, 250, 24, 60, 90, 120, 150, 180, 210, 240, 14, 44, 80, 110, 140, 170, 200, 230, 4, 34, 64];
        assert_eq!(decode(&pillow).unwrap(), Gray { width: 9, height: 5, pixels: want });
    }
}
