//! Minimal JPEG codec: writes baseline grayscale JFIF (Annex K luminance quantization table
//! scaled by `quality`, standard Huffman tables) and reads baseline / extended sequential
//! Huffman JPEGs (SOF0/SOF1, 1 or 3 components with any sampling factors, restart intervals)
//! into an 8-bit luminance raster (the Y component). Progressive and arithmetic-coded files
//! are rejected.

use camel_core::error::Result;

use super::png::Gray;
use crate::df_err;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38,
    31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const STD_LUMA_Q: [u16; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69, 56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81, 104, 113, 92, 49, 64,
    78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
];

const DC_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const AC_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];

/// Annex K.3.3.2 AC luminance symbol order.
fn ac_vals() -> Vec<u8> {
    let mut v: Vec<u8> = vec![
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33,
        0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a,
    ];
    for hi in [4u8, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
        let start = match hi {
            4..=7 => 3,
            8 => 3,
            9..=13 => 2,
            14 => 1,
            _ => 1,
        };
        for lo in start..=10u8 {
            let s = (hi << 4) | lo;
            if !v.contains(&s) {
                v.push(s);
            }
        }
    }
    // any symbols not yet listed (keeps the table complete: 162 symbols)
    for hi in 0..16u8 {
        for lo in 1..=10u8 {
            let s = (hi << 4) | lo;
            if !v.contains(&s) {
                v.push(s);
            }
        }
    }
    v
}

/// Canonical Huffman codes: symbol -> (code, length).
fn build_codes(bits: &[u8; 16], vals: &[u8]) -> Vec<(u16, u8)> {
    let mut table = vec![(0u16, 0u8); 256];
    let mut code = 0u16;
    let mut k = 0;
    for (l, &n) in bits.iter().enumerate() {
        for _ in 0..n {
            table[vals[k] as usize] = (code, l as u8 + 1);
            code += 1;
            k += 1;
        }
        code <<= 1;
    }
    table
}

struct BitOut {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitOut {
    fn put(&mut self, code: u32, len: u32) {
        for i in (0..len).rev() {
            self.acc = (self.acc << 1) | ((code >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                let b = self.acc as u8;
                self.out.push(b);
                if b == 0xff {
                    self.out.push(0);
                }
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn flush(&mut self) {
        while self.n != 0 {
            self.put(1, 1);
        }
    }
}

fn magnitude(v: i32) -> (u32, u32) {
    let a = v.unsigned_abs();
    let size = 32 - a.leading_zeros();
    let bits = if v < 0 { (v - 1) as u32 & ((1 << size) - 1) } else { v as u32 };
    (size, bits)
}

fn fdct(block: &[f32; 64]) -> [f32; 64] {
    let mut out = [0f32; 64];
    for v in 0..8 {
        for u in 0..8 {
            let mut s = 0f32;
            for y in 0..8 {
                for x in 0..8 {
                    s += block[y * 8 + x] * (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos() * (((2 * y + 1) as f32 * v as f32 * std::f32::consts::PI) / 16.0).cos();
                }
            }
            let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            let cv = if v == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            out[v * 8 + u] = 0.25 * cu * cv * s;
        }
    }
    out
}

/// Quantization table for a quality (1-100, libjpeg scaling).
fn quant(quality: u8) -> [u16; 64] {
    let q = quality.clamp(1, 100) as u32;
    let scale = if q < 50 { 5000 / q } else { 200 - 2 * q };
    let mut t = [0u16; 64];
    for i in 0..64 {
        t[i] = ((STD_LUMA_Q[i] as u32 * scale + 50) / 100).clamp(1, 255) as u16;
    }
    t
}

/// Encode a gray raster as a baseline JPEG.
pub fn encode(img: &Gray, quality: u8) -> Vec<u8> {
    let q = quant(quality);
    let acv = ac_vals();
    let dc_codes = build_codes(&DC_BITS, &(0..12).collect::<Vec<u8>>());
    let ac_codes = build_codes(&AC_BITS, &acv);
    let mut out = vec![0xff, 0xd8];
    // APP0 JFIF
    out.extend_from_slice(&[0xff, 0xe0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
    // DQT (zigzag order)
    out.extend_from_slice(&[0xff, 0xdb, 0, 67, 0]);
    for &z in ZIGZAG.iter() {
        out.push(q[z] as u8);
    }
    // SOF0
    out.extend_from_slice(&[0xff, 0xc0, 0, 11, 8]);
    out.extend_from_slice(&(img.height as u16).to_be_bytes());
    out.extend_from_slice(&(img.width as u16).to_be_bytes());
    out.extend_from_slice(&[1, 1, 0x11, 0]);
    // DHT
    let mut dht = vec![0x00];
    dht.extend_from_slice(&DC_BITS);
    dht.extend(0..12u8);
    dht.push(0x10);
    dht.extend_from_slice(&AC_BITS);
    dht.extend_from_slice(&acv);
    out.extend_from_slice(&[0xff, 0xc4]);
    out.extend_from_slice(&((dht.len() + 2) as u16).to_be_bytes());
    out.extend(dht);
    // SOS
    out.extend_from_slice(&[0xff, 0xda, 0, 8, 1, 1, 0x00, 0, 63, 0]);
    let mut bw = BitOut { out: Vec::new(), acc: 0, n: 0 };
    let mut pred = 0i32;
    let (bw_n, bh_n) = (img.width.div_ceil(8), img.height.div_ceil(8));
    for by in 0..bh_n {
        for bx in 0..bw_n {
            let mut block = [0f32; 64];
            for y in 0..8 {
                for x in 0..8 {
                    let px = (bx * 8 + x).min(img.width - 1);
                    let py = (by * 8 + y).min(img.height - 1);
                    block[y * 8 + x] = img.get(px, py) as f32 - 128.0;
                }
            }
            let d = fdct(&block);
            let mut zz = [0i32; 64];
            for (k, &z) in ZIGZAG.iter().enumerate() {
                zz[k] = (d[z] / q[z] as f32).round() as i32;
            }
            let diff = zz[0] - pred;
            pred = zz[0];
            let (s, b) = magnitude(diff);
            let (c, l) = dc_codes[s as usize];
            bw.put(c as u32, l as u32);
            bw.put(b, s);
            let mut run = 0;
            for &coef in zz.iter().skip(1) {
                if coef == 0 {
                    run += 1;
                    continue;
                }
                while run > 15 {
                    let (c, l) = ac_codes[0xf0];
                    bw.put(c as u32, l as u32);
                    run -= 16;
                }
                let (s, b) = magnitude(coef);
                let (c, l) = ac_codes[((run << 4) | s) as usize];
                bw.put(c as u32, l as u32);
                bw.put(b, s);
                run = 0;
            }
            if run > 0 {
                let (c, l) = ac_codes[0];
                bw.put(c as u32, l as u32);
            }
        }
    }
    bw.flush();
    out.extend(bw.out);
    out.extend_from_slice(&[0xff, 0xd9]);
    out
}

/// Whether bytes look like a JPEG.
pub fn is_jpeg(b: &[u8]) -> bool {
    b.starts_with(&[0xff, 0xd8])
}

// --------------------------------------------------------------------------- decoder

#[derive(Clone, Default)]
struct Huff {
    /// (length, code) -> symbol via maxcode/valptr tables
    mincode: [i32; 17],
    maxcode: [i32; 18],
    valptr: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn new(bits: &[u8], vals: &[u8]) -> Huff {
        let mut h = Huff { vals: vals.to_vec(), ..Default::default() };
        let mut code = 0i32;
        let mut k = 0i32;
        for l in 1..=16 {
            let n = bits[l - 1] as i32;
            h.valptr[l] = k;
            h.mincode[l] = code;
            code += n;
            k += n;
            h.maxcode[l] = if n > 0 { code - 1 } else { -1 };
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        h
    }
}

struct BitIn<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl BitIn<'_> {
    fn bit(&mut self) -> Result<u32> {
        if self.n == 0 {
            let b = *self.d.get(self.pos).ok_or_else(|| df_err("Truncated JPEG data"))?;
            self.pos += 1;
            if b == 0xff {
                let nb = self.d.get(self.pos).copied().unwrap_or(0);
                if nb == 0 {
                    self.pos += 1;
                } else if (0xd0..=0xd7).contains(&nb) || nb == 0xd9 {
                    // marker: feed zeros
                    self.pos -= 1;
                    self.acc = 0;
                    self.n = 8;
                    let r = (self.acc >> 7) & 1;
                    self.acc <<= 1;
                    self.n -= 1;
                    return Ok(r);
                }
            }
            self.acc = b as u32;
            self.n = 8;
        }
        self.n -= 1;
        Ok((self.acc >> self.n) & 1)
    }
    fn bits(&mut self, k: u32) -> Result<i32> {
        let mut v = 0i32;
        for _ in 0..k {
            v = (v << 1) | self.bit()? as i32;
        }
        Ok(v)
    }
    fn decode(&mut self, h: &Huff) -> Result<u8> {
        let mut code = 0i32;
        for l in 1..=16 {
            code = (code << 1) | self.bit()? as i32;
            if h.maxcode[l] >= 0 && code <= h.maxcode[l] && code >= h.mincode[l] {
                return h.vals.get((h.valptr[l] + code - h.mincode[l]) as usize).copied().ok_or_else(|| df_err("Invalid JPEG Huffman table"));
            }
        }
        Err(df_err("Invalid JPEG Huffman code"))
    }
    fn reset(&mut self) {
        self.n = 0;
        // skip RSTn marker
        if self.d.get(self.pos) == Some(&0xff) && self.d.get(self.pos + 1).map(|b| (0xd0..=0xd7).contains(b)).unwrap_or(false) {
            self.pos += 2;
        }
    }
}

fn extend(v: i32, s: u32) -> i32 {
    if s == 0 {
        0
    } else if v < (1 << (s - 1)) {
        v - (1 << s) + 1
    } else {
        v
    }
}

fn idct(c: &[f32; 64]) -> [f32; 64] {
    let mut cos = [[0f32; 8]; 8];
    for (x, row) in cos.iter_mut().enumerate() {
        for (u, v) in row.iter_mut().enumerate() {
            let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *v = cu * (((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI) / 16.0).cos();
        }
    }
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            let mut s = 0f32;
            for v in 0..8 {
                s += cos[y][v] * c[v * 8 + u];
            }
            tmp[y * 8 + u] = s;
        }
    }
    let mut out = [0f32; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0f32;
            for u in 0..8 {
                s += cos[x][u] * tmp[y * 8 + u];
            }
            out[y * 8 + x] = s / 4.0;
        }
    }
    out
}

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    pred: i32,
}

/// Decode a JPEG into its luminance (Y) channel.
pub fn decode(b: &[u8]) -> Result<Gray> {
    if !is_jpeg(b) {
        return Err(df_err("Not a JPEG image"));
    }
    let short = || df_err("Truncated JPEG");
    let mut pos = 2;
    let mut qt = vec![[1u16; 64]; 4];
    let mut dc: Vec<Huff> = vec![Huff::default(); 4];
    let mut ac: Vec<Huff> = vec![Huff::default(); 4];
    let mut comps: Vec<Comp> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    let mut restart = 0usize;
    loop {
        while b.get(pos) == Some(&0xff) && b.get(pos + 1) == Some(&0xff) {
            pos += 1;
        }
        if b.get(pos) != Some(&0xff) {
            return Err(df_err("Invalid JPEG marker"));
        }
        let m = *b.get(pos + 1).ok_or_else(short)?;
        pos += 2;
        if m == 0xd9 {
            return Err(df_err("JPEG without scan"));
        }
        let len = u16::from_be_bytes([*b.get(pos).ok_or_else(short)?, *b.get(pos + 1).ok_or_else(short)?]) as usize;
        let seg = b.get(pos + 2..pos + len).ok_or_else(short)?;
        match m {
            0xdb => {
                let mut i = 0;
                while i < seg.len() {
                    let pq = seg[i] >> 4;
                    let tq = (seg[i] & 3) as usize;
                    i += 1;
                    for &z in ZIGZAG.iter() {
                        if pq == 0 {
                            qt[tq][z] = *seg.get(i).ok_or_else(short)? as u16;
                            i += 1;
                        } else {
                            qt[tq][z] = u16::from_be_bytes([*seg.get(i).ok_or_else(short)?, *seg.get(i + 1).ok_or_else(short)?]);
                            i += 2;
                        }
                    }
                }
            }
            0xc4 => {
                let mut i = 0;
                while i + 17 <= seg.len() {
                    let tc = seg[i] >> 4;
                    let th = (seg[i] & 3) as usize;
                    let bits = &seg[i + 1..i + 17];
                    let n: usize = bits.iter().map(|x| *x as usize).sum();
                    let vals = seg.get(i + 17..i + 17 + n).ok_or_else(short)?;
                    let h = Huff::new(bits, vals);
                    if tc == 0 {
                        dc[th] = h;
                    } else {
                        ac[th] = h;
                    }
                    i += 17 + n;
                }
            }
            0xc0 | 0xc1 => {
                if seg[0] != 8 {
                    return Err(df_err("Only 8-bit JPEGs are supported"));
                }
                height = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                width = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let n = seg[5] as usize;
                for k in 0..n {
                    let c = seg.get(6 + 3 * k..9 + 3 * k).ok_or_else(short)?;
                    comps.push(Comp { id: c[0], h: (c[1] >> 4).max(1) as usize, v: (c[1] & 15).max(1) as usize, tq: (c[2] & 3) as usize, td: 0, ta: 0, pred: 0 });
                }
            }
            0xc2 | 0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return Err(df_err("Progressive, lossless and arithmetic-coded JPEGs are not supported")),
            0xdd => restart = u16::from_be_bytes([seg[0], seg[1]]) as usize,
            0xda => {
                if comps.is_empty() || width == 0 || height == 0 {
                    return Err(df_err("JPEG scan before frame header"));
                }
                let ns = seg[0] as usize;
                let mut order = Vec::new();
                for k in 0..ns {
                    let id = seg[1 + 2 * k];
                    let t = seg[2 + 2 * k];
                    let ci = comps.iter().position(|c| c.id == id).ok_or_else(|| df_err("Unknown JPEG scan component"))?;
                    comps[ci].td = (t >> 4) as usize & 3;
                    comps[ci].ta = (t & 15) as usize & 3;
                    order.push(ci);
                }
                let hmax = comps.iter().map(|c| c.h).max().unwrap();
                let vmax = comps.iter().map(|c| c.v).max().unwrap();
                let (mcux, mcuy) = (width.div_ceil(8 * hmax), height.div_ceil(8 * vmax));
                let y0 = &comps[0];
                let (yw, yh) = (mcux * y0.h * 8, mcuy * y0.v * 8);
                let mut plane = vec![0u8; yw * yh];
                let mut bits = BitIn { d: b, pos: pos + len, acc: 0, n: 0 };
                let single = ns == 1;
                let total_mcus = if single {
                    let c = &comps[order[0]];
                    let cw = (width * c.h).div_ceil(hmax);
                    let ch = (height * c.v).div_ceil(vmax);
                    cw.div_ceil(8) * ch.div_ceil(8)
                } else {
                    mcux * mcuy
                };
                for mcu in 0..total_mcus {
                    if restart > 0 && mcu > 0 && mcu % restart == 0 {
                        bits.reset();
                        for c in comps.iter_mut() {
                            c.pred = 0;
                        }
                    }
                    for &ci in &order {
                        let (bh, bv) = if single { (1, 1) } else { (comps[ci].h, comps[ci].v) };
                        for by in 0..bv {
                            for bx in 0..bh {
                                let mut coef = [0f32; 64];
                                let t = bits.decode(&dc[comps[ci].td])? as u32;
                                let diff = extend(bits.bits(t)?, t);
                                comps[ci].pred += diff;
                                let q = &qt[comps[ci].tq];
                                coef[0] = comps[ci].pred as f32 * q[0] as f32;
                                let mut k = 1;
                                while k < 64 {
                                    let rs = bits.decode(&ac[comps[ci].ta])?;
                                    let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
                                    if s == 0 {
                                        if r == 15 {
                                            k += 16;
                                            continue;
                                        }
                                        break;
                                    }
                                    k += r;
                                    if k > 63 {
                                        return Err(df_err("Invalid JPEG AC coefficient index"));
                                    }
                                    let z = ZIGZAG[k];
                                    coef[z] = extend(bits.bits(s)?, s) as f32 * q[z] as f32;
                                    k += 1;
                                }
                                if ci == 0 {
                                    let px = idct(&coef);
                                    let (ox, oy) = if single {
                                        let bw = (width * comps[0].h).div_ceil(hmax).div_ceil(8);
                                        ((mcu % bw) * 8, (mcu / bw) * 8)
                                    } else {
                                        (((mcu % mcux) * comps[0].h + bx) * 8, ((mcu / mcux) * comps[0].v + by) * 8)
                                    };
                                    for y in 0..8 {
                                        for x in 0..8 {
                                            let (xx, yy) = (ox + x, oy + y);
                                            if xx < yw && yy < yh {
                                                plane[yy * yw + xx] = (px[y * 8 + x] + 128.0).round().clamp(0.0, 255.0) as u8;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                // the Y plane covers the image at full resolution
                let (yh_full, yv_full) = (comps[0].h, comps[0].v);
                let mut g = Gray { width, height, pixels: vec![0; width * height] };
                for y in 0..height {
                    for x in 0..width {
                        let sx = x * yh_full / hmax;
                        let sy = y * yv_full / vmax;
                        g.pixels[y * width + x] = plane[sy.min(yh - 1) * yw + sx.min(yw - 1)];
                    }
                }
                return Ok(g);
            }
            _ => {}
        }
        pos += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_is_close() {
        let g = Gray { width: 45, height: 30, pixels: (0..45 * 30).map(|i| if ((i % 45) / 5 + (i / 45) / 5) % 2 == 0 { 0 } else { 255 }).collect() };
        let j = encode(&g, 95);
        let d = decode(&j).unwrap();
        assert_eq!((d.width, d.height), (45, 30));
        let worst = g.pixels.iter().zip(&d.pixels).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        assert!(worst < 60, "{}", worst);
        assert_eq!(ac_vals().len(), 162);
    }

    #[test]
    fn decodes_pillow_jpegs() {
        // 64x40 written by Pillow (quality 92): grayscale, and RGB with 4:2:0 subsampling;
        // compared with Pillow's own decoding (IDCT rounding differs slightly)
        for (jpg, luma) in [
            (&include_bytes!("testdata/pillow_gray.jpg")[..], &include_bytes!("testdata/pillow_gray.L")[..]),
            (&include_bytes!("testdata/pillow_rgb420.jpg")[..], &include_bytes!("testdata/pillow_rgb420.L")[..]),
        ] {
            let d = decode(jpg).unwrap();
            assert_eq!((d.width, d.height), (64, 40));
            let worst = d.pixels.iter().zip(luma).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
            assert!(worst <= 4, "{}", worst);
        }
    }
}
