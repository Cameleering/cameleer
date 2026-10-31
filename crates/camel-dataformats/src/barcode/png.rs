//! Minimal PNG codec: writes 8-bit grayscale images (zlib from camel-core, CRC-32) and reads
//! every standard PNG (grayscale / RGB / palette / with alpha, bit depths 1-16, all five
//! filters, Adam7 interlacing) into an 8-bit luminance raster.

use camel_core::dataformat::compress::{crc32, zlib_compress, zlib_decompress};
use camel_core::error::Result;

use crate::df_err;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// An 8-bit luminance raster.
#[derive(Clone, Debug, PartialEq)]
pub struct Gray {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Gray {
    pub fn get(&self, x: usize, y: usize) -> u8 {
        self.pixels[y * self.width + x]
    }
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encode an 8-bit grayscale PNG (filter: `Up` for rows equal to the previous one, else `None`).
pub fn encode(img: &Gray) -> Vec<u8> {
    let mut out = SIGNATURE.to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(img.width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(img.height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]); // depth 8, grayscale, deflate, adaptive filter, no interlace
    chunk(&mut out, b"IHDR", &ihdr);
    let mut raw = Vec::with_capacity((img.width + 1) * img.height);
    for y in 0..img.height {
        let row = &img.pixels[y * img.width..(y + 1) * img.width];
        if y > 0 && row == &img.pixels[(y - 1) * img.width..y * img.width] {
            raw.push(2);
            raw.extend(std::iter::repeat_n(0u8, img.width));
        } else {
            raw.push(0);
            raw.extend_from_slice(row);
        }
    }
    chunk(&mut out, b"IDAT", &zlib_compress(&raw, 6));
    chunk(&mut out, b"IEND", &[]);
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Undo the filters of one (sub)image; returns the raw scanlines without filter bytes.
fn unfilter(data: &[u8], stride: usize, rows: usize, bpp: usize) -> Result<Vec<u8>> {
    if data.len() < (stride + 1) * rows {
        return Err(df_err("Truncated PNG image data"));
    }
    let mut out = vec![0u8; stride * rows];
    for y in 0..rows {
        let ft = data[y * (stride + 1)];
        let src = &data[y * (stride + 1) + 1..(y + 1) * (stride + 1)];
        for x in 0..stride {
            let a = if x >= bpp { out[y * stride + x - bpp] } else { 0 };
            let b = if y > 0 { out[(y - 1) * stride + x] } else { 0 };
            let c = if y > 0 && x >= bpp { out[(y - 1) * stride + x - bpp] } else { 0 };
            let pred = match ft {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => paeth(a, b, c),
                f => return Err(df_err(format!("Invalid PNG filter type {}", f))),
            };
            out[y * stride + x] = src[x].wrapping_add(pred);
        }
    }
    Ok(out)
}

struct Header {
    width: usize,
    height: usize,
    depth: u8,
    color: u8,
    interlace: u8,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color {
            0 | 3 => 1,
            2 => 3,
            4 => 2,
            _ => 4,
        }
    }
    fn stride(&self, w: usize) -> usize {
        (w * self.channels() * self.depth as usize).div_ceil(8)
    }
}

/// Decode a PNG image to luminance (alpha composited over white).
pub fn decode(b: &[u8]) -> Result<Gray> {
    if b.len() < 8 || b[..8] != SIGNATURE {
        return Err(df_err("Not a PNG image"));
    }
    let mut pos = 8;
    let mut hdr: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat = Vec::new();
    while pos + 8 <= b.len() {
        let len = u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &b[pos + 4..pos + 8];
        if pos + 12 + len > b.len() {
            return Err(df_err("Truncated PNG chunk"));
        }
        let data = &b[pos + 8..pos + 8 + len];
        let crc = u32::from_be_bytes(b[pos + 8 + len..pos + 12 + len].try_into().unwrap());
        if crc32(&b[pos + 4..pos + 8 + len]) != crc {
            return Err(df_err(format!("PNG chunk {} has a bad CRC", String::from_utf8_lossy(kind))));
        }
        match kind {
            b"IHDR" if len >= 13 => {
                let h = Header {
                    width: u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize,
                    height: u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize,
                    depth: data[8],
                    color: data[9],
                    interlace: data[12],
                };
                let ok = matches!((h.color, h.depth), (0, 1 | 2 | 4 | 8 | 16) | (2 | 4 | 6, 8 | 16) | (3, 1 | 2 | 4 | 8));
                if !ok || h.width == 0 || h.height == 0 || h.width * h.height > 64 * 1024 * 1024 {
                    return Err(df_err("Unsupported PNG header"));
                }
                hdr = Some(h);
            }
            b"PLTE" => palette = data.chunks(3).filter(|c| c.len() == 3).map(|c| [c[0], c[1], c[2]]).collect(),
            b"tRNS" => trns = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    let h = hdr.ok_or_else(|| df_err("PNG without IHDR"))?;
    let raw = zlib_decompress(&idat)?;
    let bpp = (h.channels() * h.depth as usize).div_ceil(8);
    let mut gray = Gray { width: h.width, height: h.height, pixels: vec![255; h.width * h.height] };
    // (x0, y0, dx, dy) of each pass
    let passes: Vec<(usize, usize, usize, usize)> =
        if h.interlace == 1 { vec![(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)] } else { vec![(0, 0, 1, 1)] };
    let mut off = 0;
    for (x0, y0, dx, dy) in passes {
        if x0 >= h.width || y0 >= h.height {
            continue;
        }
        let pw = (h.width - x0).div_ceil(dx);
        let ph = (h.height - y0).div_ceil(dy);
        let stride = h.stride(pw);
        let need = (stride + 1) * ph;
        if off + need > raw.len() {
            return Err(df_err("Truncated PNG image data"));
        }
        let lines = unfilter(&raw[off..off + need], stride, ph, bpp)?;
        off += need;
        for py in 0..ph {
            let line = &lines[py * stride..(py + 1) * stride];
            for px in 0..pw {
                let l = luminance(&h, line, px, &palette, &trns)?;
                gray.pixels[(y0 + py * dy) * h.width + x0 + px * dx] = l;
            }
        }
    }
    Ok(gray)
}

fn sample(h: &Header, line: &[u8], index: usize) -> u16 {
    match h.depth {
        16 => u16::from_be_bytes([line[index * 2], line[index * 2 + 1]]),
        8 => line[index] as u16,
        d => {
            let bit = index * d as usize;
            let byte = line[bit / 8];
            let shift = 8 - d as usize - bit % 8;
            ((byte >> shift) & ((1u8 << d) - 1)) as u16
        }
    }
}

fn luminance(h: &Header, line: &[u8], px: usize, palette: &[[u8; 3]], trns: &[u8]) -> Result<u8> {
    let ch = h.channels();
    let to8 = |v: u16| -> f64 {
        match h.depth {
            16 => v as f64 / 257.0,
            8 => v as f64,
            d => v as f64 * 255.0 / ((1u16 << d) - 1) as f64,
        }
    };
    let (lum, alpha) = match h.color {
        0 => (to8(sample(h, line, px)), 1.0),
        2 => {
            let (r, g, b) = (to8(sample(h, line, px * 3)), to8(sample(h, line, px * 3 + 1)), to8(sample(h, line, px * 3 + 2)));
            (0.299 * r + 0.587 * g + 0.114 * b, 1.0)
        }
        3 => {
            let i = sample(h, line, px) as usize;
            let c = palette.get(i).ok_or_else(|| df_err("PNG palette index out of range"))?;
            let a = trns.get(i).map(|a| *a as f64 / 255.0).unwrap_or(1.0);
            (0.299 * c[0] as f64 + 0.587 * c[1] as f64 + 0.114 * c[2] as f64, a)
        }
        4 => (to8(sample(h, line, px * 2)), to8(sample(h, line, px * 2 + 1)) / 255.0),
        _ => {
            let (r, g, b) = (to8(sample(h, line, px * ch)), to8(sample(h, line, px * ch + 1)), to8(sample(h, line, px * ch + 2)));
            (0.299 * r + 0.587 * g + 0.114 * b, to8(sample(h, line, px * ch + 3)) / 255.0)
        }
    };
    Ok((lum * alpha + 255.0 * (1.0 - alpha)).round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunked(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        chunk(&mut v, kind, data);
        v
    }

    fn png(w: u32, h: u32, depth: u8, color: u8, interlace: u8, extra: &[Vec<u8>], raw: &[u8]) -> Vec<u8> {
        let mut out = SIGNATURE.to_vec();
        let mut ihdr = w.to_be_bytes().to_vec();
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[depth, color, 0, 0, interlace]);
        out.extend(chunked(b"IHDR", &ihdr));
        for e in extra {
            out.extend_from_slice(e);
        }
        out.extend(chunked(b"IDAT", &zlib_compress(raw, 6)));
        out.extend(chunked(b"IEND", &[]));
        out
    }

    #[test]
    fn round_trip_grayscale() {
        let img = Gray { width: 5, height: 3, pixels: vec![0, 255, 0, 255, 0, 0, 255, 0, 255, 0, 10, 20, 30, 40, 50] };
        let b = encode(&img);
        assert_eq!(&b[..8], &SIGNATURE);
        assert_eq!(decode(&b).unwrap(), img);
        let mut bad = b.clone();
        bad[20] ^= 1;
        assert!(decode(&bad).is_err());
    }

    #[test]
    fn decodes_color_types_and_filters() {
        // 2x2 RGB with Sub / Up / Paeth / Average filters
        // pixels: black, white / white, black
        let raw = [
            1, 0, 0, 0, 255, 255, 255, // Sub: second pixel = 0 + 255
            2, 255, 255, 255, 1, 1, 1, // Up: (0+255, 255+1=0)
        ];
        let g = decode(&png(2, 2, 8, 2, 0, &[], &raw)).unwrap();
        assert_eq!(g.pixels, vec![0, 255, 255, 0]);
        let raw = [4, 0, 0, 0, 255, 255, 255, 3, 255, 255, 255, 128, 128, 128];
        // Paeth on row0 = identity (no prior row, left pred for px1 = 0); Average row1: px0 = 255 + 0/2... = 255+0 -> 0? compute: pred=(0+0)/2=0 -> 255; px1: (255+255)/2=255 -> 128+255 = 127
        let g = decode(&png(2, 2, 8, 2, 0, &[], &raw)).unwrap();
        assert_eq!(g.pixels, vec![0, 255, 255, 127]);
        // 1-bit palette with transparency: index 0 = black, 1 = transparent red -> white
        let plte = chunked(b"PLTE", &[0, 0, 0, 255, 0, 0]);
        let trns = chunked(b"tRNS", &[255, 0]);
        let g = decode(&png(3, 1, 1, 3, 0, &[plte, trns], &[0, 0b0100_0000])).unwrap();
        assert_eq!(g.pixels, vec![0, 255, 0]);
        // 16-bit gray + alpha
        let g = decode(&png(1, 1, 16, 4, 0, &[], &[0, 0, 0, 0xff, 0xff])).unwrap();
        assert_eq!(g.pixels, vec![0]);
        // 2-bit grayscale
        let g = decode(&png(4, 1, 2, 0, 0, &[], &[0, 0b00_01_10_11])).unwrap();
        assert_eq!(g.pixels, vec![0, 85, 170, 255]);
    }

    #[test]
    fn decodes_adam7() {
        // 3x3 grayscale interlaced: passes 1 (0,0), 4? -> pass1:(0,0) pass2: none (x0=4) pass3: none (y0=4)
        // pass4 (2,0); pass5 (0,2),(2,2); pass6 (1,0),(1,2); pass7 row1: (0,1),(1,1),(2,1)
        let raw = [0, 1, /*p4*/ 0, 3, /*p5*/ 0, 7, 9, /*p6*/ 0, 2, 0, 8, /*p7*/ 0, 4, 5, 6];
        let g = decode(&png(3, 3, 8, 0, 1, &[], &raw)).unwrap();
        assert_eq!(g.pixels, vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
    }
}
