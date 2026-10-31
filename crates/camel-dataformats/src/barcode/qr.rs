//! QR Code (ISO/IEC 18004) model 2: encoder (versions 1-40, ECC L/M/Q/H, numeric /
//! alphanumeric / byte modes with an UTF-8 ECI, Reed-Solomon, the eight masks chosen by the
//! four penalty rules) and decoder (format / version information with BCH error tolerance,
//! unmasking, de-interleaving, Reed-Solomon correction, numeric / alphanumeric / byte / kanji /
//! ECI segments). Locating a symbol in an image lives in [`super::detect`].

use camel_core::error::Result;

use super::rs::Gf;
use super::Matrix;
use crate::df_err;

/// Error correction level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ecc {
    L,
    M,
    Q,
    H,
}

impl Ecc {
    fn index(self) -> usize {
        self as usize
    }
    /// Format information bits.
    fn format_bits(self) -> u32 {
        match self {
            Ecc::L => 1,
            Ecc::M => 0,
            Ecc::Q => 3,
            Ecc::H => 2,
        }
    }
    /// Parse `L`/`M`/`Q`/`H` (also ZXing / Camel numeric 0..3 in L, M, Q, H order).
    pub fn parse(s: &str) -> Option<Ecc> {
        match s.trim().to_ascii_uppercase().as_str() {
            "L" | "0" | "LOW" => Some(Ecc::L),
            "M" | "1" | "MEDIUM" => Some(Ecc::M),
            "Q" | "2" | "QUARTILE" => Some(Ecc::Q),
            "H" | "3" | "HIGH" => Some(Ecc::H),
            _ => None,
        }
    }
}

const ECC_PER_BLOCK: [[u8; 41]; 4] = [
    [0, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    [0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28],
    [0, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30, 30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
    [0, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
];

const NUM_BLOCKS: [[u8; 41]; 4] = [
    [0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25],
    [0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49],
    [0, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68],
    [0, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32, 35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81],
];

const ALNUM: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ $%*+-./:";

/// Number of data modules (data + EC bits incl. remainder bits) of a version.
pub fn raw_data_modules(ver: usize) -> usize {
    let mut r = (16 * ver + 128) * ver + 64;
    if ver >= 2 {
        let na = ver / 7 + 2;
        r -= (25 * na - 10) * na - 55;
        if ver >= 7 {
            r -= 36;
        }
    }
    r
}

/// Number of data codewords of a version / level.
pub fn data_codewords(ver: usize, ecc: Ecc) -> usize {
    raw_data_modules(ver) / 8 - ECC_PER_BLOCK[ecc.index()][ver] as usize * NUM_BLOCKS[ecc.index()][ver] as usize
}

/// Alignment pattern centre coordinates.
pub fn alignment_positions(ver: usize) -> Vec<usize> {
    if ver == 1 {
        return vec![];
    }
    let na = ver / 7 + 2;
    let step = (ver * 8 + na * 3 + 5) / (na * 4 - 4) * 2;
    let size = ver * 4 + 17;
    let mut r: Vec<usize> = (0..na - 1).map(|i| size - 7 - i * step).collect();
    r.push(6);
    r.reverse();
    r
}

/// BCH-coded format information (15 bits, masked).
pub fn format_info(ecc: Ecc, mask: u8) -> u32 {
    let data = (ecc.format_bits() << 3) | mask as u32;
    let mut rem = data;
    for _ in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    ((data << 10) | (rem & 0x3ff)) ^ 0x5412
}

/// BCH-coded version information (18 bits).
pub fn version_info(ver: usize) -> u32 {
    let mut rem = ver as u32;
    for _ in 0..12 {
        rem = (rem << 1) ^ ((rem >> 11) * 0x1F25);
    }
    ((ver as u32) << 12) | (rem & 0xfff)
}

fn mask_bit(mask: u8, x: usize, y: usize) -> bool {
    match mask {
        0 => (x + y) % 2 == 0,
        1 => y % 2 == 0,
        2 => x % 3 == 0,
        3 => (x + y) % 3 == 0,
        4 => (x / 3 + y / 2) % 2 == 0,
        5 => x * y % 2 + x * y % 3 == 0,
        6 => (x * y % 2 + x * y % 3) % 2 == 0,
        _ => ((x + y) % 2 + x * y % 3) % 2 == 0,
    }
}

/// Symbol under construction: modules plus the function-pattern map.
struct Symbol {
    size: usize,
    m: Matrix,
    func: Vec<bool>,
}

impl Symbol {
    fn new(ver: usize) -> Symbol {
        let size = ver * 4 + 17;
        let mut s = Symbol { size, m: Matrix::new(size, size), func: vec![false; size * size] };
        s.draw_function_patterns(ver);
        s
    }

    fn set_func(&mut self, x: usize, y: usize, dark: bool) {
        self.m.set(x, y, dark);
        self.func[y * self.size + x] = true;
    }

    fn draw_function_patterns(&mut self, ver: usize) {
        let size = self.size;
        for i in 0..size {
            self.set_func(6, i, i % 2 == 0);
            self.set_func(i, 6, i % 2 == 0);
        }
        for (cx, cy) in [(3, 3), (size - 4, 3), (3, size - 4)] {
            for dy in -4i32..=4 {
                for dx in -4i32..=4 {
                    let (x, y) = (cx as i32 + dx, cy as i32 + dy);
                    if x >= 0 && y >= 0 && (x as usize) < size && (y as usize) < size {
                        let d = dx.abs().max(dy.abs());
                        self.set_func(x as usize, y as usize, d != 2 && d != 4);
                    }
                }
            }
        }
        let al = alignment_positions(ver);
        let n = al.len();
        for i in 0..n {
            for j in 0..n {
                if (i == 0 && j == 0) || (i == 0 && j == n - 1) || (i == n - 1 && j == 0) {
                    continue;
                }
                for dy in -2i32..=2 {
                    for dx in -2i32..=2 {
                        let d = dx.abs().max(dy.abs());
                        self.set_func((al[i] as i32 + dx) as usize, (al[j] as i32 + dy) as usize, d != 1);
                    }
                }
            }
        }
        self.draw_format(0);
        if ver >= 7 {
            let bits = version_info(ver);
            for i in 0..18 {
                let bit = (bits >> i) & 1 == 1;
                let (a, b) = (size - 11 + i % 3, i / 3);
                self.set_func(a, b, bit);
                self.set_func(b, a, bit);
            }
        }
    }

    fn draw_format(&mut self, bits: u32) {
        let size = self.size;
        let bit = |i: usize| (bits >> i) & 1 == 1;
        for i in 0..=5 {
            self.set_func(8, i, bit(i));
        }
        self.set_func(8, 7, bit(6));
        self.set_func(8, 8, bit(7));
        self.set_func(7, 8, bit(8));
        for i in 9..15 {
            self.set_func(14 - i, 8, bit(i));
        }
        for i in 0..8 {
            self.set_func(size - 1 - i, 8, bit(i));
        }
        for i in 8..15 {
            self.set_func(8, size - 15 + i, bit(i));
        }
        self.set_func(8, size - 8, true);
    }

    /// Data module coordinates in placement order.
    fn data_positions(&self) -> Vec<(usize, usize)> {
        let size = self.size;
        let mut out = Vec::new();
        let mut right = size as i32 - 1;
        while right >= 1 {
            if right == 6 {
                right = 5;
            }
            for vert in 0..size {
                for j in 0..2 {
                    let x = (right - j) as usize;
                    let upward = (right + 1) & 2 == 0;
                    let y = if upward { size - 1 - vert } else { vert };
                    if !self.func[y * size + x] {
                        out.push((x, y));
                    }
                }
            }
            right -= 2;
        }
        out
    }

    fn apply_mask(&mut self, mask: u8) {
        for y in 0..self.size {
            for x in 0..self.size {
                if !self.func[y * self.size + x] && mask_bit(mask, x, y) {
                    let v = self.m.get(x, y);
                    self.m.set(x, y, !v);
                }
            }
        }
    }
}

/// Penalty score of a symbol (rules N1..N4).
pub fn penalty(m: &Matrix) -> u32 {
    let n = m.width;
    let mut score = 0u32;
    // N1: runs of >= 5 same-coloured modules; N3: 1:1:3:1:1 patterns with 4 light modules
    for horizontal in [true, false] {
        for a in 0..n {
            let get = |b: usize| if horizontal { m.get(b, a) } else { m.get(a, b) };
            let mut run = 1;
            for b in 1..n {
                if get(b) == get(b - 1) {
                    run += 1;
                } else {
                    if run >= 5 {
                        score += 3 + (run - 5);
                    }
                    run = 1;
                }
            }
            if run >= 5 {
                score += 3 + (run - 5);
            }
            let pat = [true, false, true, true, true, false, true];
            for b in 0..n.saturating_sub(6) {
                if (0..7).all(|i| get(b + i) == pat[i]) {
                    let light = |from: i32, to: i32| (from..to).all(|i| i < 0 || i >= n as i32 || !get(i as usize));
                    if light(b as i32 - 4, b as i32) || light(b as i32 + 7, b as i32 + 11) {
                        score += 40;
                    }
                }
            }
        }
    }
    // N2: 2x2 blocks
    for y in 0..n - 1 {
        for x in 0..n - 1 {
            let c = m.get(x, y);
            if m.get(x + 1, y) == c && m.get(x, y + 1) == c && m.get(x + 1, y + 1) == c {
                score += 3;
            }
        }
    }
    // N4: dark proportion
    let dark = m.bits.iter().filter(|b| **b).count();
    let total = n * n;
    let k = (dark * 20).abs_diff(total * 10).div_ceil(total).saturating_sub(1);
    score + k as u32 * 10
}

struct Bits(Vec<bool>);

impl Bits {
    fn push(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            self.0.push((v >> i) & 1 == 1);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Numeric,
    Alnum,
    Byte,
}

fn count_bits(mode: Mode, ver: usize) -> usize {
    let idx = if ver <= 9 {
        0
    } else if ver <= 26 {
        1
    } else {
        2
    };
    match mode {
        Mode::Numeric => [10, 12, 14][idx],
        Mode::Alnum => [9, 11, 13][idx],
        Mode::Byte => [8, 16, 16][idx],
    }
}

/// Encoding options.
#[derive(Clone, Debug)]
pub struct QrOptions {
    pub ecc: Ecc,
    /// Force a version (1..40).
    pub version: Option<usize>,
    /// Force a mask (0..7).
    pub mask: Option<u8>,
}

impl Default for QrOptions {
    fn default() -> Self {
        QrOptions { ecc: Ecc::L, version: None, mask: None }
    }
}

/// Data codewords (before EC) of `text` for a version.
fn data_bits(text: &str, ver: usize) -> (Bits, Mode) {
    let mode = if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
        Mode::Numeric
    } else if !text.is_empty() && text.chars().all(|c| ALNUM.contains(c)) {
        Mode::Alnum
    } else {
        Mode::Byte
    };
    let mut bb = Bits(Vec::new());
    match mode {
        Mode::Numeric => {
            bb.push(1, 4);
            bb.push(text.len() as u32, count_bits(mode, ver));
            for chunk in text.as_bytes().chunks(3) {
                let v: u32 = std::str::from_utf8(chunk).unwrap().parse().unwrap();
                bb.push(v, chunk.len() * 3 + 1);
            }
        }
        Mode::Alnum => {
            bb.push(2, 4);
            bb.push(text.len() as u32, count_bits(mode, ver));
            let idx: Vec<u32> = text.chars().map(|c| ALNUM.find(c).unwrap() as u32).collect();
            for p in idx.chunks(2) {
                if p.len() == 2 {
                    bb.push(p[0] * 45 + p[1], 11);
                } else {
                    bb.push(p[0], 6);
                }
            }
        }
        Mode::Byte => {
            if !text.is_ascii() {
                bb.push(7, 4); // ECI
                bb.push(26, 8); // UTF-8
            }
            bb.push(4, 4);
            bb.push(text.len() as u32, count_bits(mode, ver));
            for b in text.bytes() {
                bb.push(b as u32, 8);
            }
        }
    }
    (bb, mode)
}

/// Encode `text` into a QR Code module matrix (without quiet zone).
pub fn encode(text: &str, opts: &QrOptions) -> Result<Matrix> {
    let ecc = opts.ecc;
    let versions: Vec<usize> = match opts.version {
        Some(v) if (1..=40).contains(&v) => vec![v],
        Some(v) => return Err(df_err(format!("Invalid QR Code version {}", v))),
        None => (1..=40).collect(),
    };
    let mut chosen = None;
    for ver in versions {
        let (bb, mode) = data_bits(text, ver);
        let cap = data_codewords(ver, ecc) * 8;
        let len_ok = match mode {
            Mode::Byte => text.len() < (1 << count_bits(mode, ver)),
            _ => text.len() < (1 << count_bits(mode, ver)),
        };
        if bb.0.len() <= cap && len_ok {
            chosen = Some((ver, bb, cap));
            break;
        }
    }
    let (ver, mut bb, cap) = chosen.ok_or_else(|| df_err(format!("Data too big for a QR Code at error correction level {:?}", ecc)))?;
    let term = (cap - bb.0.len()).min(4);
    bb.push(0, term);
    while bb.0.len() % 8 != 0 {
        bb.0.push(false);
    }
    let mut data: Vec<u8> = bb.0.chunks(8).map(|c| c.iter().fold(0u8, |a, b| (a << 1) | *b as u8)).collect();
    let mut pad = 0xEC;
    while data.len() < cap / 8 {
        data.push(pad);
        pad ^= 0xEC ^ 0x11;
    }
    let codewords = add_ecc_and_interleave(&data, ver, ecc);
    let mut sym = Symbol::new(ver);
    let positions = sym.data_positions();
    for (i, (x, y)) in positions.iter().enumerate() {
        let dark = i < codewords.len() * 8 && (codewords[i >> 3] >> (7 - (i & 7))) & 1 == 1;
        sym.m.set(*x, *y, dark);
    }
    let mask = match opts.mask {
        Some(m) if m < 8 => m,
        Some(m) => return Err(df_err(format!("Invalid QR Code mask {}", m))),
        None => {
            let mut best = (u32::MAX, 0u8);
            for m in 0..8u8 {
                sym.apply_mask(m);
                sym.draw_format(format_info(ecc, m));
                let p = penalty(&sym.m);
                if p < best.0 {
                    best = (p, m);
                }
                sym.apply_mask(m);
            }
            best.1
        }
    };
    sym.apply_mask(mask);
    sym.draw_format(format_info(ecc, mask));
    Ok(sym.m)
}

fn block_layout(ver: usize, ecc: Ecc) -> (usize, usize, usize, usize) {
    let nb = NUM_BLOCKS[ecc.index()][ver] as usize;
    let ec = ECC_PER_BLOCK[ecc.index()][ver] as usize;
    let raw = raw_data_modules(ver) / 8;
    let num_short = nb - raw % nb;
    let short_len = raw / nb;
    (nb, ec, num_short, short_len)
}

/// Split into blocks, append EC codewords and interleave.
pub fn add_ecc_and_interleave(data: &[u8], ver: usize, ecc: Ecc) -> Vec<u8> {
    let gf = Gf::qr();
    let (nb, ec, num_short, short_len) = block_layout(ver, ecc);
    let mut blocks: Vec<Vec<u8>> = Vec::new();
    let mut k = 0;
    for i in 0..nb {
        let dlen = short_len - ec + usize::from(i >= num_short);
        let d = &data[k..k + dlen];
        k += dlen;
        let mut b = d.to_vec();
        if i < num_short {
            b.push(0);
        }
        b.extend(gf.encode(d, ec));
        blocks.push(b);
    }
    let mut out = Vec::new();
    for i in 0..blocks[0].len() {
        for (j, b) in blocks.iter().enumerate() {
            if i != short_len - ec || j >= num_short {
                out.push(b[i]);
            }
        }
    }
    out
}

/// Result of decoding a module matrix.
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub version: usize,
    pub ecc: Ecc,
    pub mask: u8,
    pub corrected: usize,
    pub raw_bytes: Vec<u8>,
}

/// Read the format information (ECC, mask) tolerating up to 3 bit errors.
fn read_format(m: &Matrix) -> Result<(Ecc, u8)> {
    let size = m.width;
    let g = |x: usize, y: usize| m.get(x, y) as u32;
    let mut a = 0u32;
    for i in 0..=5 {
        a |= g(8, i) << i;
    }
    a |= g(8, 7) << 6 | g(8, 8) << 7 | g(7, 8) << 8;
    for i in 9..15 {
        a |= g(14 - i, 8) << i;
    }
    let mut b = 0u32;
    for i in 0..8 {
        b |= g(size - 1 - i, 8) << i;
    }
    for i in 8..15 {
        b |= g(8, size - 15 + i) << i;
    }
    let mut best = (u32::MAX, Ecc::L, 0u8);
    for e in [Ecc::L, Ecc::M, Ecc::Q, Ecc::H] {
        for mask in 0..8u8 {
            let f = format_info(e, mask);
            let d = (f ^ a).count_ones().min((f ^ b).count_ones());
            if d < best.0 {
                best = (d, e, mask);
            }
        }
    }
    if best.0 > 3 {
        return Err(df_err("Cannot read the QR Code format information"));
    }
    Ok((best.1, best.2))
}

/// Read the version information (v >= 7) near the top-right finder; None if unreadable.
pub fn read_version(m: &Matrix) -> Option<usize> {
    let size = m.width;
    let mut best = (u32::MAX, 0usize);
    for copy in 0..2 {
        let mut bits = 0u32;
        for i in 0..18 {
            let (a, b) = (size - 11 + i % 3, i / 3);
            let v = if copy == 0 { m.get(a, b) } else { m.get(b, a) };
            bits |= (v as u32) << i;
        }
        for ver in 7..=40 {
            let d = (version_info(ver) ^ bits).count_ones();
            if d < best.0 {
                best = (d, ver);
            }
        }
    }
    (best.0 <= 3).then_some(best.1)
}

/// Decode a QR Code module matrix (square, without quiet zone).
pub fn decode_matrix(m: &Matrix) -> Result<Decoded> {
    let size = m.width;
    if size < 21 || size > 177 || (size - 17) % 4 != 0 || m.height != size {
        return Err(df_err(format!("Invalid QR Code dimension {}", size)));
    }
    let mut ver = (size - 17) / 4;
    if ver >= 7 {
        if let Some(v) = read_version(m) {
            if v != ver {
                return Err(df_err(format!("QR Code version information ({}) does not match the dimension ({})", v, ver)));
            }
            ver = v;
        }
    }
    let (ecc, mask) = read_format(m)?;
    let sym = Symbol::new(ver);
    let positions = sym.data_positions();
    let raw_len = raw_data_modules(ver) / 8;
    let mut cw = vec![0u8; raw_len];
    for (i, (x, y)) in positions.iter().enumerate().take(raw_len * 8) {
        let bit = m.get(*x, *y) ^ mask_bit(mask, *x, *y);
        if bit {
            cw[i >> 3] |= 0x80 >> (i & 7);
        }
    }
    // de-interleave
    let (nb, ec, num_short, short_len) = block_layout(ver, ecc);
    let mut blocks: Vec<Vec<u8>> = vec![vec![0u8; short_len + 1]; nb];
    let mut k = 0;
    for i in 0..short_len + 1 {
        for (j, b) in blocks.iter_mut().enumerate() {
            if i != short_len - ec || j >= num_short {
                b[i] = cw[k];
                k += 1;
            }
        }
    }
    let gf = Gf::qr();
    let mut data = Vec::new();
    let mut corrected = 0;
    for (j, mut b) in blocks.into_iter().enumerate() {
        if j < num_short {
            b.remove(short_len - ec);
        }
        let n = gf.decode(&mut b, ec).ok_or_else(|| df_err("QR Code has too many errors (Reed-Solomon decoding failed)"))?;
        corrected += n;
        let dlen = b.len() - ec;
        data.extend_from_slice(&b[..dlen]);
    }
    let text = parse_segments(&data, ver)?;
    Ok(Decoded { text, version: ver, ecc, mask, corrected, raw_bytes: data })
}

struct BitReader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn available(&self) -> usize {
        self.d.len() * 8 - self.pos
    }
    fn read(&mut self, n: usize) -> Result<u32> {
        if n > self.available() {
            return Err(df_err("QR Code data ends prematurely"));
        }
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | ((self.d[self.pos >> 3] >> (7 - (self.pos & 7))) & 1) as u32;
            self.pos += 1;
        }
        Ok(v)
    }
}

fn bytes_to_text(b: &[u8], eci: Option<u32>) -> String {
    match eci {
        Some(26) => String::from_utf8_lossy(b).into_owned(),
        Some(1) | Some(3) => b.iter().map(|c| *c as char).collect(),
        _ => match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => b.iter().map(|c| *c as char).collect(),
        },
    }
}

fn parse_segments(data: &[u8], ver: usize) -> Result<String> {
    let mut r = BitReader { d: data, pos: 0 };
    let mut out = String::new();
    let mut eci: Option<u32> = None;
    let mut pending: Vec<u8> = Vec::new();
    let flush = |pending: &mut Vec<u8>, out: &mut String, eci: Option<u32>| {
        if !pending.is_empty() {
            out.push_str(&bytes_to_text(pending, eci));
            pending.clear();
        }
    };
    while r.available() >= 4 {
        let mode = r.read(4)?;
        match mode {
            0 => break,
            1 => {
                flush(&mut pending, &mut out, eci);
                let mut n = r.read(count_bits(Mode::Numeric, ver))? as usize;
                while n >= 3 {
                    let v = r.read(10)?;
                    if v > 999 {
                        return Err(df_err("Invalid numeric QR segment"));
                    }
                    out.push_str(&format!("{:03}", v));
                    n -= 3;
                }
                if n == 2 {
                    out.push_str(&format!("{:02}", r.read(7)?));
                } else if n == 1 {
                    out.push_str(&format!("{}", r.read(4)?));
                }
            }
            2 => {
                flush(&mut pending, &mut out, eci);
                let mut n = r.read(count_bits(Mode::Alnum, ver))? as usize;
                let ch = |i: u32| ALNUM.as_bytes().get(i as usize).map(|c| *c as char).ok_or_else(|| df_err("Invalid alphanumeric QR segment"));
                while n >= 2 {
                    let v = r.read(11)?;
                    out.push(ch(v / 45)?);
                    out.push(ch(v % 45)?);
                    n -= 2;
                }
                if n == 1 {
                    out.push(ch(r.read(6)?)?);
                }
            }
            4 => {
                let n = r.read(count_bits(Mode::Byte, ver))? as usize;
                for _ in 0..n {
                    pending.push(r.read(8)? as u8);
                }
            }
            7 => {
                flush(&mut pending, &mut out, eci);
                let first = r.read(8)?;
                eci = Some(if first & 0x80 == 0 {
                    first
                } else if first & 0xC0 == 0x80 {
                    ((first & 0x3f) << 8) | r.read(8)?
                } else {
                    ((first & 0x1f) << 16) | r.read(16)?
                });
            }
            8 => {
                flush(&mut pending, &mut out, eci);
                let bits = [8, 10, 12][if ver <= 9 { 0 } else if ver <= 26 { 1 } else { 2 }];
                let n = r.read(bits)? as usize;
                let mut sjis = Vec::new();
                for _ in 0..n {
                    let v = r.read(13)?;
                    let mut a = ((v / 0xC0) << 8) | (v % 0xC0);
                    a += if a < 0x1F00 { 0x8140 } else { 0xC140 };
                    sjis.push((a >> 8) as u8);
                    sjis.push(a as u8);
                }
                // no Shift_JIS tables here: keep the code points as escaped hex
                for p in sjis.chunks(2) {
                    out.push_str(&format!("\\x{:02X}{:02X}", p[0], p[1]));
                }
            }
            3 => {
                // structured append: skip sequence + parity
                r.read(16)?;
            }
            5 | 9 => {
                // FNC1 markers (9 also carries an application indicator)
                if mode == 9 {
                    r.read(8)?;
                }
            }
            m => return Err(df_err(format!("Unsupported QR Code mode {}", m))),
        }
    }
    flush(&mut pending, &mut out, eci);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacities_and_tables() {
        assert_eq!(data_codewords(1, Ecc::M), 16);
        assert_eq!(data_codewords(40, Ecc::L), 2956);
        assert_eq!(data_codewords(40, Ecc::H), 1276);
        assert_eq!(data_codewords(5, Ecc::Q), 62);
        assert_eq!(data_codewords(10, Ecc::M), 216);
        assert_eq!(alignment_positions(7), vec![6, 22, 38]);
        assert_eq!(alignment_positions(32), vec![6, 34, 60, 86, 112, 138]);
        assert_eq!(format!("{:015b}", format_info(Ecc::L, 4)), "110011000101111");
        assert_eq!(format!("{:015b}", format_info(Ecc::M, 0)), "101010000010010");
        assert_eq!(format!("{:015b}", format_info(Ecc::H, 7)), "000100000111011");
        assert_eq!(format!("{:018b}", version_info(7)), "000111110010010100");
        assert_eq!(format!("{:018b}", version_info(40)), "101000110001101001");
    }

    #[test]
    fn iso_annex_i_codewords() {
        // ISO/IEC 18004 Annex I: "01234567" as version 1-M numeric
        let (bb, mode) = data_bits("01234567", 1);
        assert_eq!(mode, Mode::Numeric);
        let s: String = bb.0.iter().map(|b| if *b { '1' } else { '0' }).collect();
        assert_eq!(s, "00010000001000000000110001010110011000011");
        let mut data = vec![0x10, 0x20, 0x0C, 0x56, 0x61, 0x80];
        let mut pad = 0xEC;
        while data.len() < 16 {
            data.push(pad);
            pad ^= 0xEC ^ 0x11;
        }
        let cw = add_ecc_and_interleave(&data, 1, Ecc::M);
        assert_eq!(&cw[16..], &[0xA5, 0x24, 0xD4, 0xC1, 0xED, 0x36, 0xC7, 0x87, 0x2C, 0x55]);
        let m = encode("01234567", &QrOptions { ecc: Ecc::M, version: None, mask: None }).unwrap();
        assert_eq!(m.width, 21);
        let d = decode_matrix(&m).unwrap();
        assert_eq!((d.text.as_str(), d.version, d.ecc), ("01234567", 1, Ecc::M));
        assert_eq!(&d.raw_bytes[..6], &[0x10, 0x20, 0x0C, 0x56, 0x61, 0x80]);
    }

    #[test]
    fn all_versions_levels_modes_masks_round_trip() {
        let samples = ["12345678901234567890", "HELLO WORLD $%*+-./:", "Hello, World! héllo ✓ 日本"];
        for ver in [1usize, 2, 6, 7, 10, 14, 21, 27, 33, 40] {
            for ecc in [Ecc::L, Ecc::M, Ecc::Q, Ecc::H] {
                let base = samples[(ver + ecc as usize) % 3];
                // fill most of the capacity (byte count of the sample repeated)
                let cap = data_codewords(ver, ecc).saturating_sub(4);
                let reps = (cap / base.len()).max(1);
                let text = &base.repeat(reps);
                let text = if text.len() > cap { &text[..text.char_indices().map(|(i, _)| i).take_while(|i| *i <= cap / 2).last().unwrap_or(0)] } else { text.as_str() };
                let text = if text.is_empty() { "1" } else { text };
                let mask = Some(((ver + ecc as usize) % 8) as u8);
                let m = encode(text, &QrOptions { ecc, version: Some(ver), mask }).unwrap();
                assert_eq!(m.width, ver * 4 + 17);
                let d = decode_matrix(&m).unwrap();
                assert_eq!(d.text, text, "v{} {:?}", ver, ecc);
                assert_eq!((d.version, d.ecc, Some(d.mask)), (ver, ecc, mask));
            }
        }
        // smallest version is chosen; long payload
        let long = "x".repeat(2000);
        let m = encode(&long, &QrOptions { ecc: Ecc::L, ..Default::default() }).unwrap();
        assert_eq!(decode_matrix(&m).unwrap().text, long);
        assert!(encode(&"x".repeat(3000), &QrOptions::default()).is_err());
    }

    #[test]
    fn corrects_damaged_modules() {
        let m = encode("https://camel.apache.org/components/next/dataformats/barcode-dataformat.html", &QrOptions { ecc: Ecc::H, ..Default::default() }).unwrap();
        let mut damaged = m.clone();
        // wipe a block of modules in the data area (bottom right)
        for y in m.width - 8..m.width - 3 {
            for x in m.width - 8..m.width - 3 {
                damaged.set(x, y, false);
            }
        }
        let d = decode_matrix(&damaged).unwrap();
        assert!(d.corrected > 0);
        assert!(d.text.starts_with("https://camel.apache.org"));
    }

    #[test]
    fn mask_selection_minimises_penalty() {
        let opts = QrOptions { ecc: Ecc::M, version: Some(1), mask: None };
        let auto = encode("01234567", &opts).unwrap();
        let chosen = decode_matrix(&auto).unwrap().mask;
        let p = penalty(&auto);
        for mk in 0..8 {
            let m = encode("01234567", &QrOptions { mask: Some(mk), ..opts.clone() }).unwrap();
            assert!(penalty(&m) >= p, "mask {} beats chosen {}", mk, chosen);
        }
    }
}
