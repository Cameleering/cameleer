//! Aztec Code (ISO/IEC 24778): compact (1-4 layers) and full-range (1-32 layers) symbols.
//!
//! Encoding: the text (ISO-8859-1, or UTF-8 behind an ECI 26 `FLG(2)` when it has other
//! characters) is written with the Upper / Lower / Digit / Punct modes and Binary Shift runs,
//! bit-stuffed into codewords of 6, 8, 10 or 12 bits, protected with Reed-Solomon over
//! GF(64)/GF(256)/GF(1024)/GF(4096) (at least 23% + 3 codewords of error correction like
//! ZXing's default 33% of the data), and placed in layers around the bullseye with the mode
//! message (GF(16)) and, for full-range symbols, the reference grid. Decoding reads upright
//! module matrices: bullseye / mode message, layer data, RS correction, unstuffing and every
//! character mode (Upper, Lower, Mixed, Punct, Digit, shifts and latches, Binary Shift,
//! `FLG(n)` ECIs).

use camel_core::error::Result;

use super::Matrix;
use crate::df_err;

// ------------------------------------------------------------------------- GF(2^m) RS

/// A GF(2^m) field (m <= 12) for Reed-Solomon with generator base 1.
pub struct GfN {
    size: usize,
    exp: Vec<u16>,
    log: Vec<u16>,
}

impl GfN {
    pub fn new(poly: u32, size: usize) -> GfN {
        let mut exp = vec![0u16; size * 2];
        let mut log = vec![0u16; size];
        let mut x: u32 = 1;
        for i in 0..size - 1 {
            exp[i] = x as u16;
            log[x as usize] = i as u16;
            x <<= 1;
            if x as usize >= size {
                x ^= poly;
            }
        }
        for i in size - 1..size * 2 {
            exp[i] = exp[i - (size - 1)];
        }
        GfN { size, exp, log }
    }
    fn n(&self) -> usize {
        self.size - 1
    }
    fn mul(&self, a: u16, b: u16) -> u16 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }
    fn div(&self, a: u16, b: u16) -> u16 {
        if a == 0 {
            0
        } else {
            self.exp[(self.log[a as usize] as usize + self.n() - self.log[b as usize] as usize) % self.n()]
        }
    }
    fn pow(&self, e: usize) -> u16 {
        self.exp[e % self.n()]
    }

    /// Check words for `data` (generator roots α^1 .. α^nsym).
    pub fn encode(&self, data: &[u16], nsym: usize) -> Vec<u16> {
        let mut g = vec![1u16];
        for i in 0..nsym {
            let root = self.pow(1 + i);
            let mut ng = vec![0u16; g.len() + 1];
            for (j, c) in g.iter().enumerate() {
                ng[j] ^= *c;
                ng[j + 1] ^= self.mul(*c, root);
            }
            g = ng;
        }
        let mut rem = vec![0u16; nsym];
        for &d in data {
            let f = d ^ rem[0];
            rem.remove(0);
            rem.push(0);
            if f != 0 {
                for i in 0..nsym {
                    rem[i] ^= self.mul(g[i + 1], f);
                }
            }
        }
        rem
    }

    fn eval_hi(&self, p: &[u16], x: u16) -> u16 {
        p.iter().fold(0, |a, c| self.mul(a, x) ^ c)
    }
    fn eval_lo(&self, p: &[u16], x: u16) -> u16 {
        p.iter().rev().fold(0, |a, c| self.mul(a, x) ^ c)
    }

    /// Corrects `block` (data + `nsym` check words) in place.
    pub fn decode(&self, block: &mut [u16], nsym: usize) -> Option<usize> {
        let n = block.len();
        let synd: Vec<u16> = (0..nsym).map(|j| self.eval_hi(block, self.pow(1 + j))).collect();
        if synd.iter().all(|s| *s == 0) {
            return Some(0);
        }
        let (mut c, mut b) = (vec![1u16], vec![1u16]);
        let (mut l, mut m, mut bb) = (0usize, 1usize, 1u16);
        for i in 0..nsym {
            let mut d = synd[i];
            for j in 1..=l.min(c.len() - 1) {
                d ^= self.mul(c[j], synd[i - j]);
            }
            if d == 0 {
                m += 1;
                continue;
            }
            let coef = self.div(d, bb);
            let mut nc = c.clone();
            if nc.len() < b.len() + m {
                nc.resize(b.len() + m, 0);
            }
            for (j, bj) in b.iter().enumerate() {
                nc[j + m] ^= self.mul(coef, *bj);
            }
            if 2 * l <= i {
                b = c;
                l = i + 1 - l;
                bb = d;
                m = 1;
            } else {
                m += 1;
            }
            c = nc;
        }
        while c.len() > 1 && *c.last().unwrap() == 0 {
            c.pop();
        }
        let nerr = c.len() - 1;
        if nerr != l || 2 * nerr > nsym {
            return None;
        }
        let mut positions = Vec::new();
        for d in 0..n {
            if self.eval_lo(&c, self.pow(self.n() - d % self.n())) == 0 {
                positions.push(d);
            }
        }
        if positions.len() != nerr {
            return None;
        }
        let mut omega = vec![0u16; nsym];
        for (i, s) in synd.iter().enumerate() {
            for (j, cj) in c.iter().enumerate() {
                if i + j < nsym {
                    omega[i + j] ^= self.mul(*s, *cj);
                }
            }
        }
        let deriv: Vec<u16> = (1..c.len()).map(|i| if i % 2 == 1 { c[i] } else { 0 }).collect();
        for &d in &positions {
            let x = self.pow(d);
            let xi = self.div(1, x);
            let den = self.eval_lo(&deriv, xi);
            if den == 0 {
                return None;
            }
            let e = self.div(self.eval_lo(&omega, xi), den);
            block[n - 1 - d] ^= e;
        }
        if (0..nsym).any(|j| self.eval_hi(block, self.pow(1 + j)) != 0) {
            return None;
        }
        Some(nerr)
    }
}

fn field(word: usize) -> GfN {
    match word {
        4 => GfN::new(0x13, 16),
        6 => GfN::new(0x43, 64),
        8 => GfN::new(0x12d, 256),
        10 => GfN::new(0x409, 1024),
        _ => GfN::new(0x1069, 4096),
    }
}

// ------------------------------------------------------------------------- geometry

fn word_size(layers: usize) -> usize {
    match layers {
        0..=2 => 6,
        3..=8 => 8,
        9..=22 => 10,
        _ => 12,
    }
}

fn total_bits(layers: usize, compact: bool) -> usize {
    ((if compact { 88 } else { 112 }) + 16 * layers) * layers
}

fn base_size(layers: usize, compact: bool) -> usize {
    (if compact { 11 } else { 14 }) + layers * 4
}

fn matrix_size(layers: usize, compact: bool) -> usize {
    let b = base_size(layers, compact);
    if compact {
        b
    } else {
        b + 1 + 2 * ((b / 2 - 1) / 15)
    }
}

fn alignment_map(layers: usize, compact: bool) -> Vec<usize> {
    let base = base_size(layers, compact);
    if compact {
        return (0..base).collect();
    }
    let size = matrix_size(layers, compact);
    let mut map = vec![0; base];
    let (orig, center) = (base / 2, size / 2);
    for i in 0..orig {
        let off = i + i / 15;
        map[orig - i - 1] = center - off - 1;
        map[orig + i] = center + off + 1;
    }
    map
}

/// (x, y) of every data bit in order.
fn data_positions(layers: usize, compact: bool) -> Vec<(usize, usize)> {
    let map = alignment_map(layers, compact);
    let base = base_size(layers, compact);
    let mut pos = vec![(0, 0); total_bits(layers, compact)];
    let mut row_offset = 0;
    for i in 0..layers {
        let row = (layers - i) * 4 + if compact { 9 } else { 12 };
        for j in 0..row {
            let col = j * 2;
            for k in 0..2 {
                pos[row_offset + col + k] = (map[i * 2 + k], map[i * 2 + j]);
                pos[row_offset + row * 2 + col + k] = (map[i * 2 + j], map[base - 1 - i * 2 - k]);
                pos[row_offset + row * 4 + col + k] = (map[base - 1 - i * 2 - k], map[base - 1 - i * 2 - j]);
                pos[row_offset + row * 6 + col + k] = (map[base - 1 - i * 2 - j], map[i * 2 + k]);
            }
        }
        row_offset += row * 8;
    }
    pos
}

fn mode_positions(compact: bool, size: usize) -> Vec<(usize, usize)> {
    let c = size / 2;
    if compact {
        let mut p = vec![(0, 0); 28];
        for i in 0..7 {
            let o = c - 3 + i;
            p[i] = (o, c - 5);
            p[i + 7] = (c + 5, o);
            p[20 - i] = (o, c + 5);
            p[27 - i] = (c - 5, o);
        }
        p
    } else {
        let mut p = vec![(0, 0); 40];
        for i in 0..10 {
            let o = c - 5 + i + i / 5;
            p[i] = (o, c - 7);
            p[i + 10] = (c + 7, o);
            p[29 - i] = (o, c + 7);
            p[39 - i] = (c - 7, o);
        }
        p
    }
}

fn to_words(bits: &[bool], w: usize) -> Vec<u16> {
    bits.chunks(w).map(|c| c.iter().fold(0u16, |a, b| (a << 1) | *b as u16)).collect()
}

fn push_bits(out: &mut Vec<bool>, v: u32, n: usize) {
    for i in (0..n).rev() {
        out.push((v >> i) & 1 == 1);
    }
}

/// Check words appended (right-aligned in `total` bits, leading zero padding).
fn with_check_words(bits: &[bool], total: usize, w: usize) -> Vec<bool> {
    let words = to_words(bits, w);
    let nwords = total / w;
    let check = field(w).encode(&words, nwords - words.len());
    let mut out = vec![false; total % w];
    for x in words.iter().chain(check.iter()) {
        push_bits(&mut out, *x as u32, w);
    }
    out
}

fn stuff(bits: &[bool], w: usize) -> Vec<bool> {
    let mut out = Vec::new();
    let mask = (1u32 << w) - 2;
    let mut i = 0;
    while i < bits.len() {
        let mut word = 0u32;
        for j in 0..w {
            if i + j >= bits.len() || bits[i + j] {
                word |= 1 << (w - 1 - j);
            }
        }
        if word & mask == mask {
            push_bits(&mut out, word & mask, w);
            i += w - 1;
        } else if word & mask == 0 {
            push_bits(&mut out, word | 1, w);
            i += w - 1;
        } else {
            push_bits(&mut out, word, w);
            i += w;
        }
    }
    out
}

// ------------------------------------------------------------------------- high level

const PUNCT: [&str; 31] = ["", "\r", "\r\n", ". ", ", ", ": ", "!", "\"", "#", "$", "%", "&", "'", "(", ")", "*", "+", ",", "-", ".", "/", ":", ";", "<", "=", ">", "?", "[", "]", "{", "}"];
const MIXED: [u8; 28] = [0, b' ', 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 27, 28, 29, 30, 31, b'@', b'\\', b'^', b'_', b'`', b'|', b'~', 127];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Mode {
    Upper,
    Lower,
    Mixed,
    Punct,
    Digit,
}

/// High-level encoding of bytes (optionally behind an ECI).
fn high_level(data: &[u8], eci: Option<u32>) -> Vec<bool> {
    let mut out = Vec::new();
    let mut mode = Mode::Upper;
    if let Some(e) = eci {
        // P/S FLG(n) digits
        let digits = e.to_string();
        push_bits(&mut out, 0, 5);
        push_bits(&mut out, 0, 5);
        push_bits(&mut out, digits.len() as u32, 3);
        for d in digits.bytes() {
            push_bits(&mut out, (d - b'0') as u32 + 2, 4);
        }
    }
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        // digit runs (3+ digits) in Digit mode
        let run = data[i..].iter().take_while(|b| b.is_ascii_digit() || **b == b' ' || **b == b',' || **b == b'.').count();
        let digits = data[i..i + run].iter().filter(|b| b.is_ascii_digit()).count();
        if mode != Mode::Digit && digits >= 3 {
            match mode {
                Mode::Upper | Mode::Lower => push_bits(&mut out, 30, 5),
                _ => unreachable!(),
            }
            mode = Mode::Digit;
        }
        if mode == Mode::Digit {
            let code = match c {
                b' ' => Some(1),
                b'0'..=b'9' => Some((c - b'0') as u32 + 2),
                b',' => Some(12),
                b'.' => Some(13),
                _ => None,
            };
            if let Some(code) = code {
                push_bits(&mut out, code, 4);
                i += 1;
                continue;
            }
            push_bits(&mut out, 14, 4); // U/L
            mode = Mode::Upper;
        }
        match c {
            b'A'..=b'Z' => {
                if mode == Mode::Lower {
                    if data.get(i + 1).map(|n| n.is_ascii_uppercase()).unwrap_or(false) {
                        // latch back to upper: D/L then U/L
                        push_bits(&mut out, 30, 5);
                        push_bits(&mut out, 14, 4);
                        mode = Mode::Upper;
                    } else {
                        push_bits(&mut out, 28, 5); // U/S
                    }
                }
                push_bits(&mut out, (c - b'A') as u32 + 2, 5);
                i += 1;
            }
            b'a'..=b'z' => {
                if mode != Mode::Lower {
                    push_bits(&mut out, 28, 5); // L/L
                    mode = Mode::Lower;
                }
                push_bits(&mut out, (c - b'a') as u32 + 2, 5);
                i += 1;
            }
            b' ' => {
                push_bits(&mut out, 1, 5);
                i += 1;
            }
            _ => {
                // two-character punctuation, then single
                let two = data.get(i..i + 2).and_then(|p| PUNCT.iter().position(|x| x.len() == 2 && x.as_bytes() == p));
                let one = PUNCT.iter().position(|x| x.len() == 1 && x.as_bytes()[0] == c);
                if let Some(p) = two.or(one) {
                    push_bits(&mut out, 0, 5); // P/S
                    push_bits(&mut out, p as u32, 5);
                    i += if two.is_some() { 2 } else { 1 };
                    continue;
                }
                if let Some(p) = MIXED.iter().position(|x| *x == c).filter(|p| *p >= 2) {
                    // M/L code, then back
                    push_bits(&mut out, 29, 5);
                    push_bits(&mut out, p as u32, 5);
                    push_bits(&mut out, if mode == Mode::Lower { 28 } else { 29 }, 5);
                    i += 1;
                    continue;
                }
                // binary shift run of bytes not encodable otherwise
                let mut j = i;
                while j < data.len() && j - i < 2078 {
                    let b = data[j];
                    if b.is_ascii_alphanumeric() || b == b' ' {
                        break;
                    }
                    j += 1;
                }
                let n = j - i;
                push_bits(&mut out, 31, 5);
                if n <= 31 {
                    push_bits(&mut out, n as u32, 5);
                } else {
                    push_bits(&mut out, 0, 5);
                    push_bits(&mut out, (n - 31) as u32, 11);
                }
                for b in &data[i..j] {
                    push_bits(&mut out, *b as u32, 8);
                }
                i = j;
            }
        }
    }
    out
}

/// Encode text into an Aztec symbol (smallest size with ~33% error correction).
pub fn encode(text: &str, min_ecc_percent: usize) -> Result<Matrix> {
    let (bytes, eci) = if text.chars().all(|c| (c as u32) < 256) { (text.chars().map(|c| c as u8).collect::<Vec<u8>>(), None) } else { (text.as_bytes().to_vec(), Some(26)) };
    let bits = high_level(&bytes, eci);
    let ecc_bits = bits.len() * min_ecc_percent / 100 + 11;
    let total_needed = bits.len() + ecc_bits;
    for i in 0..=32 {
        // like ZXing: compact 1-4 layers, then full-range from 4 layers
        let compact = i <= 3;
        let layers = if compact { i + 1 } else { i };
        if layers > 32 {
            break;
        }
        let tb = total_bits(layers, compact);
        if total_needed > tb {
            continue;
        }
        let w = word_size(layers);
        let usable = tb - tb % w;
        let stuffed = stuff(&bits, w);
        if stuffed.len() + ecc_bits > usable {
            continue;
        }
        if compact && stuffed.len() > w * 64 {
            continue;
        }
        if !compact && stuffed.len() > w * 2048 {
            continue;
        }
        return Ok(build(&stuffed, layers, compact, w));
    }
    Err(df_err("Data too big for an Aztec symbol"))
}

fn build(stuffed: &[bool], layers: usize, compact: bool, w: usize) -> Matrix {
    let tb = total_bits(layers, compact);
    let message = with_check_words(stuffed, tb, w);
    let nwords = stuffed.len() / w;
    let mut mode = Vec::new();
    if compact {
        push_bits(&mut mode, (layers - 1) as u32, 2);
        push_bits(&mut mode, (nwords - 1) as u32, 6);
        mode = with_check_words(&mode, 28, 4);
    } else {
        push_bits(&mut mode, (layers - 1) as u32, 5);
        push_bits(&mut mode, (nwords - 1) as u32, 11);
        mode = with_check_words(&mode, 40, 4);
    }
    let size = matrix_size(layers, compact);
    let mut m = Matrix::new(size, size);
    for (bit, (x, y)) in message.iter().zip(data_positions(layers, compact)) {
        if *bit {
            m.set(x, y, true);
        }
    }
    for (bit, (x, y)) in mode.iter().zip(mode_positions(compact, size)) {
        if *bit {
            m.set(x, y, true);
        }
    }
    let c = size / 2;
    let eye = if compact { 5 } else { 7 };
    for i in (0..eye).step_by(2) {
        for j in c - i..=c + i {
            m.set(j, c - i, true);
            m.set(j, c + i, true);
            m.set(c - i, j, true);
            m.set(c + i, j, true);
        }
    }
    // orientation marks
    m.set(c - eye, c - eye, true);
    m.set(c - eye + 1, c - eye, true);
    m.set(c - eye, c - eye + 1, true);
    m.set(c + eye, c - eye, true);
    m.set(c + eye, c - eye + 1, true);
    m.set(c + eye, c + eye - 1, true);
    if !compact {
        let base = base_size(layers, compact);
        let (mut i, mut j) = (0, 0);
        while i < base / 2 - 1 {
            let mut k = c & 1;
            while k < size {
                m.set(c - j, k, true);
                m.set(c + j, k, true);
                m.set(k, c - j, true);
                m.set(k, c + j, true);
                k += 2;
            }
            i += 15;
            j += 16;
        }
    }
    m
}

// ------------------------------------------------------------------------- decoding

/// Reads the mode message around the centre of a symbol of `size` modules.
fn read_mode(m: &Matrix, compact: bool) -> Option<(usize, usize)> {
    let bits: Vec<bool> = mode_positions(compact, m.width).iter().map(|&(x, y)| m.get(x, y)).collect();
    let mut words = to_words(&bits, 4);
    let nd = if compact { 2 } else { 4 };
    let nsym = words.len() - nd;
    field(4).decode(&mut words, nsym)?;
    let v = words[..nd].iter().fold(0u32, |a, w| (a << 4) | *w as u32);
    Some(if compact { (((v >> 6) + 1) as usize, ((v & 0x3f) + 1) as usize) } else { (((v >> 11) + 1) as usize, ((v & 0x7ff) + 1) as usize) })
}

/// Whether the matrix is a compact symbol (the ring at distance 6 is part of the bullseye
/// only for full-range symbols).
fn is_compact(m: &Matrix) -> bool {
    let c = m.width / 2;
    if m.width < 19 || c < 6 {
        return true;
    }
    !(c - 6..=c + 6).all(|j| m.get(j, c - 6) && m.get(j, c + 6) && m.get(c - 6, j) && m.get(c + 6, j))
}

/// Decode an Aztec module matrix to bytes and the ECI (if any).
pub fn decode_matrix(m: &Matrix) -> Result<String> {
    let compact = is_compact(m);
    let (layers, nwords) = read_mode(m, compact).ok_or_else(|| df_err("Aztec mode message is unreadable"))?;
    if matrix_size(layers, compact) != m.width {
        return Err(df_err("Aztec mode message does not match the symbol size"));
    }
    let w = word_size(layers);
    let tb = total_bits(layers, compact);
    let bits: Vec<bool> = data_positions(layers, compact).iter().map(|&(x, y)| m.get(x, y)).collect();
    let mut words = to_words(&bits[tb % w..], w);
    if nwords >= words.len() {
        return Err(df_err("Invalid Aztec data word count"));
    }
    let nsym = words.len() - nwords;
    field(w).decode(&mut words, nsym).ok_or_else(|| df_err("Aztec symbol has too many errors (Reed-Solomon decoding failed)"))?;
    // unstuff
    let mask = (1u16 << w) - 1;
    let mut data = Vec::new();
    for &x in &words[..nwords] {
        if x == 0 || x == mask {
            return Err(df_err("Invalid Aztec data word"));
        }
        if x == 1 || x == mask - 1 {
            for _ in 0..w - 1 {
                data.push(x > 1);
            }
        } else {
            push_bits(&mut data, x as u32, w);
        }
    }
    read_high_level(&data)
}

fn read_high_level(bits: &[bool]) -> Result<String> {
    let mut pos = 0;
    let read = |pos: &mut usize, n: usize| -> Option<u32> {
        if *pos + n > bits.len() {
            return None;
        }
        let v = bits[*pos..*pos + n].iter().fold(0u32, |a, b| (a << 1) | *b as u32);
        *pos += n;
        Some(v)
    };
    let mut out: Vec<u8> = Vec::new();
    let mut eci: Option<u32> = None;
    let mut latch = Mode::Upper;
    let mut shift: Option<Mode> = None;
    loop {
        let mode = shift.take().unwrap_or(latch);
        let size = if mode == Mode::Digit { 4 } else { 5 };
        let Some(code) = read(&mut pos, size) else { break };
        let shifted = mode != latch;
        match (mode, code) {
            (Mode::Digit, 0) | (Mode::Upper | Mode::Lower | Mode::Mixed, 0) => shift = Some(Mode::Punct),
            (Mode::Upper | Mode::Lower, 1) | (Mode::Mixed, 1) | (Mode::Digit, 1) => out.push(b' '),
            (Mode::Upper, 2..=27) => out.push(b'A' + code as u8 - 2),
            (Mode::Lower, 2..=27) => out.push(b'a' + code as u8 - 2),
            (Mode::Upper, 28) | (Mode::Mixed, 28) => latch = Mode::Lower,
            (Mode::Lower, 28) => shift = Some(Mode::Upper),
            (Mode::Upper | Mode::Lower, 29) => latch = Mode::Mixed,
            (Mode::Upper | Mode::Lower, 30) => latch = Mode::Digit,
            (Mode::Mixed, 29) => latch = Mode::Upper,
            (Mode::Mixed, 30) => latch = Mode::Punct,
            (Mode::Upper | Mode::Lower | Mode::Mixed, 31) => {
                // like ZXing, a binary shift running into the padding ends the data
                let Some(mut n) = read(&mut pos, 5).map(|v| v as usize) else { break };
                if n == 0 {
                    let Some(x) = read(&mut pos, 11) else { break };
                    n = x as usize + 31;
                }
                for _ in 0..n {
                    let Some(b) = read(&mut pos, 8) else { break };
                    out.push(b as u8);
                }
            }
            (Mode::Mixed, 2..=27) => out.push(MIXED[code as usize]),
            (Mode::Punct, 0) => {
                // FLG(n)
                let Some(n) = read(&mut pos, 3) else { break };
                if n == 0 {
                    out.push(0x1d); // FNC1
                } else if n == 7 {
                    return Err(df_err("Invalid Aztec FLG(7)"));
                } else {
                    let mut v = 0u32;
                    for _ in 0..n {
                        let d = read(&mut pos, 4).ok_or_else(|| df_err("Truncated Aztec ECI"))?;
                        if !(2..=11).contains(&d) {
                            return Err(df_err("Invalid Aztec ECI digit"));
                        }
                        v = v * 10 + d - 2;
                    }
                    eci = Some(v);
                }
            }
            (Mode::Punct, 31) => latch = Mode::Upper,
            (Mode::Punct, c) => out.extend_from_slice(PUNCT[c as usize].as_bytes()),
            (Mode::Digit, 2..=11) => out.push(b'0' + code as u8 - 2),
            (Mode::Digit, 12) => out.push(b','),
            (Mode::Digit, 13) => out.push(b'.'),
            (Mode::Digit, 14) => latch = Mode::Upper,
            (Mode::Digit, 15) => shift = Some(Mode::Upper),
            _ => {}
        }
        // a shift applies to one character only
        if shifted && shift.is_none() && latch == mode {
            // punct latch from mixed lands here: nothing to undo
        }
    }
    Ok(match eci {
        Some(26) => String::from_utf8_lossy(&out).into_owned(),
        _ => match std::str::from_utf8(&out) {
            Ok(s) if eci.is_some() => s.to_string(),
            _ => out.iter().map(|b| *b as char).collect(),
        },
    })
}

/// Locate an upright Aztec symbol by its bullseye and decode it.
pub fn find_and_decode(get: &dyn Fn(usize, usize) -> bool, width: usize, height: usize) -> Option<String> {
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0, 0);
    for y in 0..height {
        for x in 0..width {
            if get(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x0 == usize::MAX {
        return None;
    }
    let (ax, ay) = ((x0 + x1) / 2, (y0 + y1) / 2);
    // search near the approximate centre for a dark module whose light ring is square
    let span = ((x1 - x0) / 10).max(3);
    for dy in 0..=span {
        for sy in [ay as isize + dy as isize, ay as isize - dy as isize] {
            if sy < 0 || sy as usize >= height {
                continue;
            }
            let y = sy as usize;
            for dx in 0..=span {
                for sx in [ax as isize + dx as isize, ax as isize - dx as isize] {
                    if sx < 0 || sx as usize >= width || !get(sx as usize, y) {
                        continue;
                    }
                    let x = sx as usize;
                    // horizontal run of the centre module
                    let (mut l, mut r) = (x, x);
                    while l > 0 && get(l - 1, y) {
                        l -= 1;
                    }
                    while r + 1 < width && get(r + 1, y) {
                        r += 1;
                    }
                    let module = r - l + 1;
                    if module == 0 || module > width / 11 {
                        continue;
                    }
                    let cx = (l + r) / 2;
                    let (mut t, mut b) = (y, y);
                    while t > 0 && get(cx, t - 1) {
                        t -= 1;
                    }
                    while b + 1 < height && get(cx, b + 1) {
                        b += 1;
                    }
                    if (b - t + 1).abs_diff(module) > module / 2 + 1 {
                        continue;
                    }
                    let cy = (t + b) / 2;
                    // the light ring 1 then dark ring 2
                    let ring_ok = |d: usize, dark: bool| -> bool {
                        let off = d * module;
                        cx >= off && cy >= off && cx + off < width && cy + off < height && get(cx + off, cy) == dark && get(cx - off, cy) == dark && get(cx, cy + off) == dark && get(cx, cy - off) == dark
                    };
                    if !(ring_ok(1, false) && ring_ok(2, true) && ring_ok(3, false) && ring_ok(4, true)) {
                        continue;
                    }
                    for n in symbol_sizes() {
                        let half = n / 2;
                        if cx < half * module || cy < half * module || cx + half * module >= width + module || cy + half * module >= height + module {
                            continue;
                        }
                        let mut m = Matrix::new(n, n);
                        for yy in 0..n {
                            for xx in 0..n {
                                let px = (cx + xx * module).checked_sub(half * module);
                                let py = (cy + yy * module).checked_sub(half * module);
                                if let (Some(px), Some(py)) = (px, py) {
                                    if px < width && py < height {
                                        m.set(xx, yy, get(px, py));
                                    }
                                }
                            }
                        }
                        if let Ok(t) = decode_matrix(&m) {
                            return Some(t);
                        }
                    }
                    return None;
                }
            }
        }
    }
    None
}

/// All valid symbol sizes (compact first).
pub fn symbol_sizes() -> Vec<usize> {
    (1..=4).map(|l| matrix_size(l, true)).chain((1..=32).map(|l| matrix_size(l, false))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry() {
        assert_eq!(symbol_sizes()[..4], [15, 19, 23, 27]);
        assert_eq!(matrix_size(1, false), 19);
        assert_eq!(matrix_size(4, false), 31);
        assert_eq!(matrix_size(5, false), 37); // reference grid lines appear
        assert_eq!(matrix_size(32, false), 151);
        for (l, c) in [(1, true), (4, true), (1, false), (5, false), (12, false), (32, false)] {
            let pos = data_positions(l, c);
            let mut seen = std::collections::HashSet::new();
            assert!(pos.iter().all(|p| seen.insert(*p)), "{} {}", l, c);
        }
    }

    #[test]
    fn stuffing() {
        // 6-bit words: 000000 -> 000001 + 0..., 111111 -> 111110 + 1...
        let bits: Vec<bool> = "000000111111".chars().map(|c| c == '1').collect();
        let s: String = stuff(&bits, 6).iter().map(|b| if *b { '1' } else { '0' }).collect();
        assert_eq!(s, "000001011111111110");
    }

    #[test]
    fn round_trips() {
        let long: String = (0..1500).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        for text in ["A", "Hello Aztec", "UPPER lower 12345 digits, punct: !?#", "Code 2026: x@y.z ~ {}", "Grüße 日本 ✓", "\r\n tabs\tand\u{1}ctl", long.as_str()] {
            let m = encode(text, 33).unwrap();
            assert!(symbol_sizes().contains(&m.width), "{}", m.width);
            assert_eq!(decode_matrix(&m).unwrap(), text, "{}", m.width);
        }
        // error correction
        let m = encode("Error correction test 1234567890", 33).unwrap();
        let mut d = m.clone();
        for x in 0..3 {
            d.set(x, 0, !d.get(x, 0));
        }
        assert_eq!(decode_matrix(&d).unwrap(), "Error correction test 1234567890");
    }

    #[test]
    fn gf_rs() {
        for w in [4usize, 6, 8, 10, 12] {
            let f = field(w);
            let data: Vec<u16> = (0..10u16).map(|i| (i * 7 + 3) & ((1 << w) - 1)).collect();
            let ecc = f.encode(&data, 6);
            let mut block = [data.clone(), ecc].concat();
            let orig = block.clone();
            block[1] ^= 1;
            block[8] ^= 3;
            block[12] ^= 2;
            assert_eq!(f.decode(&mut block, 6), Some(3));
            assert_eq!(block, orig);
        }
    }
}
