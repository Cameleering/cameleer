//! PDF417 (ISO/IEC 15438): high-level encoding (text / byte / numeric compaction, ECI),
//! Reed-Solomon error correction over GF(929) (levels 0-8), row/column layout with the
//! row indicators, start/stop patterns and the three cluster tables; decoding of upright,
//! rotated (90° steps) and mirrored-free synthetic images by row scanning with per-row voting,
//! erasure-aware RS correction and the high-level decoder (incl. Macro PDF417 control blocks,
//! which are skipped).
//!
//! The encoder follows ZXing's `PDF417` / `PDF417HighLevelEncoder` (AUTO compaction, default
//! error correction level 2, 2..30 columns and rows with a preferred aspect ratio of 3, row
//! height of 4 modules, 30 pixel margin) so that symbols match Camel's ZXing based output.

use std::collections::HashMap;
use std::sync::OnceLock;

use camel_core::error::Result;

use super::detect::Bin;
use super::pdf417_tables::CODEWORD_TABLE;
use crate::df_err;

const START_PATTERN: u32 = 0x1fea8; // 17 modules: 8 1 1 1 1 1 1 3
const STOP_PATTERN: u32 = 0x3fa29; // 18 modules: 7 1 1 3 1 1 1 2 1
const MOD: u32 = 929;

const TEXT_LATCH: u32 = 900;
const BYTE_LATCH: u32 = 901;
const NUMERIC_LATCH: u32 = 902;
const BYTE_SHIFT: u32 = 913;
const MACRO_TERMINATOR: u32 = 922;
const MACRO_OPTIONAL: u32 = 923;
const BYTE_LATCH_6: u32 = 924;
const ECI_USER: u32 = 925;
const ECI_GENERAL: u32 = 926;
const ECI_CHARSET: u32 = 927;
const MACRO_CONTROL: u32 = 928;

/// Compaction mode (`pdf417Compaction`, ZXing `Compaction`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compaction {
    Auto,
    Text,
    Byte,
    Numeric,
}

impl Compaction {
    pub fn parse(s: &str) -> Option<Compaction> {
        Some(match s.to_ascii_uppercase().as_str() {
            "AUTO" => Compaction::Auto,
            "TEXT" => Compaction::Text,
            "BYTE" => Compaction::Byte,
            "NUMERIC" => Compaction::Numeric,
            _ => return None,
        })
    }
}

/// Encoder options.
#[derive(Clone, Debug)]
pub struct Pdf417Options {
    /// Error correction level 0..8 (2^(level+1) EC codewords), ZXing default 2.
    pub ec_level: u8,
    pub compaction: Compaction,
    /// (min columns, max columns, min rows, max rows), ZXing default (2, 30, 2, 30).
    pub dimensions: (usize, usize, usize, usize),
}

impl Default for Pdf417Options {
    fn default() -> Self {
        Pdf417Options { ec_level: 2, compaction: Compaction::Auto, dimensions: (2, 30, 2, 30) }
    }
}

// ---------------------------------------------------------------------------------------
// GF(929) arithmetic and Reed-Solomon

fn add(a: u32, b: u32) -> u32 {
    (a + b) % MOD
}
fn sub(a: u32, b: u32) -> u32 {
    (a + MOD - b) % MOD
}
fn mul(a: u32, b: u32) -> u32 {
    a * b % MOD
}
fn pow(mut a: u32, mut e: u32) -> u32 {
    let mut r = 1;
    while e > 0 {
        if e & 1 == 1 {
            r = mul(r, a);
        }
        a = mul(a, a);
        e >>= 1;
    }
    r
}
fn inv(a: u32) -> u32 {
    pow(a, MOD - 2)
}

/// Number of error correction codewords for a level.
pub fn ec_count(level: u8) -> usize {
    1 << (level as usize + 1)
}

/// Generator polynomial (x - 3)(x - 3^2)...(x - 3^k), highest degree first (monic).
pub fn generator(k: usize) -> Vec<u32> {
    let mut g = vec![1u32];
    for i in 1..=k {
        let root = pow(3, i as u32);
        let mut n = vec![0u32; g.len() + 1];
        for (j, c) in g.iter().enumerate() {
            n[j] = add(n[j], *c);
            n[j + 1] = sub(n[j + 1], mul(*c, root));
        }
        g = n;
    }
    g
}

/// Error correction codewords for `data` (highest degree first, appended after the data).
pub fn ec_codewords(data: &[u32], level: u8) -> Vec<u32> {
    let k = ec_count(level);
    let g = generator(k);
    // remainder of data(x) * x^k mod g(x)
    let mut rem = vec![0u32; k];
    for d in data {
        let factor = add(*d, rem[0]);
        rem.remove(0);
        rem.push(0);
        for j in 0..k {
            rem[j] = sub(rem[j], mul(factor, g[j + 1]));
        }
    }
    // the remainder r satisfies data * x^k == q*g - r..., ISO stores the complement
    rem.iter().map(|r| (MOD - r) % MOD).collect()
}

fn poly_eval(p: &[u32], x: u32) -> u32 {
    // p lowest degree first
    p.iter().rev().fold(0, |acc, c| add(mul(acc, x), *c))
}

/// Correct `cw` (data + EC, highest degree first) in place; `erasures` are indexes known to
/// be unreadable. Returns the number of corrected codewords.
pub fn correct(cw: &mut [u32], k: usize, erasures: &[usize]) -> Option<usize> {
    let n = cw.len();
    if n > MOD as usize || k == 0 || erasures.len() > k {
        return None;
    }
    // codeword i is the coefficient of x^(n-1-i)
    let syn: Vec<u32> = (1..=k)
        .map(|j| {
            let x = pow(3, j as u32);
            cw.iter().fold(0, |acc, c| add(mul(acc, x), *c))
        })
        .collect();
    if syn.iter().all(|s| *s == 0) {
        return Some(0);
    }
    // Berlekamp-Massey with erasures: Lambda starts as the erasure locator
    let mut lambda = vec![1u32];
    for e in erasures {
        let xl = pow(3, (n - 1 - e) as u32);
        let mut nl = vec![0u32; lambda.len() + 1];
        for (i, c) in lambda.iter().enumerate() {
            nl[i] = add(nl[i], *c);
            nl[i + 1] = sub(nl[i + 1], mul(*c, xl));
        }
        lambda = nl;
    }
    let rho = erasures.len();
    let mut b = lambda.clone();
    let mut l = rho;
    for r in rho..k {
        // discrepancy
        let mut delta = 0u32;
        for (i, c) in lambda.iter().enumerate() {
            if i <= r {
                delta = add(delta, mul(*c, syn[r - i]));
            }
        }
        // b <- x * b
        b.insert(0, 0);
        if delta != 0 {
            let t: Vec<u32> = {
                let len = lambda.len().max(b.len());
                (0..len).map(|i| sub(*lambda.get(i).unwrap_or(&0), mul(delta, *b.get(i).unwrap_or(&0)))).collect()
            };
            if 2 * l <= r + rho {
                l = r + 1 + rho - l;
                let di = inv(delta);
                b = lambda.iter().map(|c| mul(*c, di)).collect();
            }
            lambda = t;
        }
    }
    while lambda.len() > 1 && *lambda.last().unwrap() == 0 {
        lambda.pop();
    }
    let deg = lambda.len() - 1;
    if deg == 0 || 2 * (deg - rho) + rho > k {
        return None;
    }
    // Omega = S(x) * Lambda(x) mod x^k
    let mut omega = vec![0u32; k];
    for i in 0..k {
        for (j, c) in lambda.iter().enumerate() {
            if j <= i {
                omega[i] = add(omega[i], mul(*c, syn[i - j]));
            }
        }
    }
    // formal derivative
    let dl: Vec<u32> = lambda.iter().enumerate().skip(1).map(|(i, c)| mul(*c, i as u32 % MOD)).collect();
    let mut found = 0;
    for pos in 0..n {
        let xl = pow(3, (n - 1 - pos) as u32);
        let xi = inv(xl);
        if poly_eval(&lambda, xi) == 0 {
            let den = poly_eval(&dl, xi);
            if den == 0 {
                return None;
            }
            // Forney with first consecutive root 3^1: e = -Omega(X^-1) / Lambda'(X^-1)
            let e = sub(0, mul(poly_eval(&omega, xi), inv(den)));
            cw[pos] = sub(cw[pos], e);
            found += 1;
        }
    }
    if found != deg {
        return None;
    }
    // verify
    let ok = (1..=k).all(|j| {
        let x = pow(3, j as u32);
        cw.iter().fold(0, |acc, c| add(mul(acc, x), *c)) == 0
    });
    ok.then_some(found)
}

// ---------------------------------------------------------------------------------------
// High-level encoding (ZXing PDF417HighLevelEncoder)

const MIXED_RAW: [u8; 30] = [48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 38, 13, 9, 44, 58, 35, 45, 46, 36, 47, 43, 37, 42, 61, 94, 0, 32, 0, 0, 0];
const PUNCT_RAW: [u8; 30] = [59, 60, 62, 64, 91, 92, 93, 95, 96, 126, 33, 13, 9, 44, 58, 10, 45, 46, 36, 47, 34, 124, 42, 40, 41, 63, 123, 125, 39, 0];

fn lookup(raw: &[u8; 30], c: char) -> Option<u32> {
    if c == '\0' || (c as u32) > 127 {
        return None;
    }
    raw.iter().position(|r| *r as char == c).map(|p| p as u32)
}
fn is_mixed(c: char) -> bool {
    lookup(&MIXED_RAW, c).is_some()
}
fn is_punct(c: char) -> bool {
    lookup(&PUNCT_RAW, c).is_some()
}
fn is_upper(c: char) -> bool {
    c == ' ' || c.is_ascii_uppercase()
}
fn is_lower(c: char) -> bool {
    c == ' ' || c.is_ascii_lowercase()
}
fn is_text(c: char) -> bool {
    c == '\t' || c == '\n' || c == '\r' || (' '..='~').contains(&c)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Sub {
    Alpha,
    Lower,
    Mixed,
    Punct,
}

fn encode_text(msg: &[char], start: usize, count: usize, out: &mut Vec<u32>, initial: Sub) -> Result<Sub> {
    let mut tmp: Vec<u32> = Vec::new();
    let mut sub = initial;
    let mut idx = 0;
    while idx < count {
        let ch = msg[start + idx];
        if !is_text(ch) {
            return Err(df_err(format!("Non-encodable character detected: {} (Unicode: {}); use Compaction.AUTO instead of Compaction.TEXT", ch, ch as u32)));
        }
        match sub {
            Sub::Alpha => {
                if is_upper(ch) {
                    tmp.push(if ch == ' ' { 26 } else { ch as u32 - 65 });
                } else if is_lower(ch) {
                    sub = Sub::Lower;
                    tmp.push(27);
                    continue;
                } else if is_mixed(ch) {
                    sub = Sub::Mixed;
                    tmp.push(28);
                    continue;
                } else {
                    tmp.push(29);
                    tmp.push(lookup(&PUNCT_RAW, ch).unwrap());
                }
            }
            Sub::Lower => {
                if is_lower(ch) {
                    tmp.push(if ch == ' ' { 26 } else { ch as u32 - 97 });
                } else if is_upper(ch) {
                    tmp.push(27);
                    tmp.push(ch as u32 - 65);
                } else if is_mixed(ch) {
                    sub = Sub::Mixed;
                    tmp.push(28);
                    continue;
                } else {
                    tmp.push(29);
                    tmp.push(lookup(&PUNCT_RAW, ch).unwrap());
                }
            }
            Sub::Mixed => {
                if let Some(v) = lookup(&MIXED_RAW, ch) {
                    tmp.push(v);
                } else if is_upper(ch) {
                    sub = Sub::Alpha;
                    tmp.push(28);
                    continue;
                } else if is_lower(ch) {
                    sub = Sub::Lower;
                    tmp.push(27);
                    continue;
                } else {
                    if start + idx + 1 < msg.len() && is_punct(msg[start + idx + 1]) {
                        sub = Sub::Punct;
                        tmp.push(25);
                        continue;
                    }
                    tmp.push(29);
                    tmp.push(lookup(&PUNCT_RAW, ch).unwrap());
                }
            }
            Sub::Punct => {
                if let Some(v) = lookup(&PUNCT_RAW, ch) {
                    tmp.push(v);
                } else {
                    sub = Sub::Alpha;
                    tmp.push(29);
                    continue;
                }
            }
        }
        idx += 1;
    }
    let mut h = 0;
    for (i, v) in tmp.iter().enumerate() {
        if i % 2 == 1 {
            h = h * 30 + v;
            out.push(h);
        } else {
            h = *v;
        }
    }
    if tmp.len() % 2 == 1 {
        out.push(h * 30 + 29);
        // the padding value is "AL" in the punctuation sub-mode: the decoder is back in Alpha
        if sub == Sub::Punct {
            sub = Sub::Alpha;
        }
    }
    Ok(sub)
}

fn encode_binary(bytes: &[u8], text_mode_single: bool, out: &mut Vec<u32>) {
    let count = bytes.len();
    if count == 1 && text_mode_single {
        out.push(BYTE_SHIFT);
    } else if count % 6 == 0 {
        out.push(BYTE_LATCH_6);
    } else {
        out.push(BYTE_LATCH);
    }
    let mut idx = 0;
    while count - idx >= 6 {
        let mut t: u64 = 0;
        for b in &bytes[idx..idx + 6] {
            t = (t << 8) + *b as u64;
        }
        let mut chars = [0u32; 5];
        for c in chars.iter_mut().rev() {
            *c = (t % 900) as u32;
            t /= 900;
        }
        out.extend_from_slice(&chars);
        idx += 6;
    }
    out.extend(bytes[idx..].iter().map(|b| *b as u32));
}

/// Base conversion of a decimal digit string to base 900 (most significant first).
fn decimal_to_900(digits: &str) -> Vec<u32> {
    let mut num: Vec<u32> = digits.bytes().map(|b| (b - b'0') as u32).collect();
    let mut out = Vec::new();
    while !(num.is_empty() || num.iter().all(|d| *d == 0)) {
        let mut rem = 0u32;
        let mut q = Vec::with_capacity(num.len());
        for d in &num {
            let cur = rem * 10 + d;
            let qd = cur / 900;
            rem = cur % 900;
            if !(q.is_empty() && qd == 0) {
                q.push(qd);
            }
        }
        out.push(rem);
        num = q;
    }
    out.reverse();
    out
}

fn encode_numeric(msg: &[char], start: usize, count: usize, out: &mut Vec<u32>) {
    let mut idx = 0;
    while idx < count {
        let len = 44.min(count - idx);
        let part: String = std::iter::once('1').chain(msg[start + idx..start + idx + len].iter().copied()).collect();
        out.extend(decimal_to_900(&part));
        idx += len;
    }
}

fn digit_count(msg: &[char], start: usize) -> usize {
    msg[start..].iter().take_while(|c| c.is_ascii_digit()).count()
}

fn text_count(msg: &[char], start: usize) -> usize {
    let len = msg.len();
    let mut idx = start;
    while idx < len {
        let mut numeric = 0;
        while numeric < 13 && idx < len && msg[idx].is_ascii_digit() {
            numeric += 1;
            idx += 1;
        }
        if numeric >= 13 {
            return idx - start - numeric;
        }
        if numeric > 0 {
            continue;
        }
        if !is_text(msg[idx]) {
            break;
        }
        idx += 1;
    }
    idx - start
}

fn binary_count(msg: &[char], start: usize) -> usize {
    let len = msg.len();
    let mut idx = start;
    while idx < len {
        let mut numeric = 0;
        while numeric < 13 && idx + numeric < len && msg[idx + numeric].is_ascii_digit() {
            numeric += 1;
        }
        if numeric >= 13 {
            return idx - start;
        }
        idx += 1;
    }
    idx - start
}

/// The character set used for bytes: ISO-8859-1 when every character fits, else UTF-8
/// announced with ECI 26 (like ZXing with `CHARACTER_SET=UTF-8`).
fn to_bytes(chars: &[char], utf8: bool) -> Vec<u8> {
    if utf8 {
        chars.iter().collect::<String>().into_bytes()
    } else {
        chars.iter().map(|c| *c as u32 as u8).collect()
    }
}

/// High-level encoding into data codewords (without the length descriptor).
pub fn encode_high_level(text: &str, compaction: Compaction) -> Result<Vec<u32>> {
    if text.is_empty() {
        return Err(df_err("Empty message not allowed"));
    }
    let msg: Vec<char> = text.chars().collect();
    let utf8 = msg.iter().any(|c| (*c as u32) > 255);
    let mut out = Vec::new();
    if utf8 {
        out.push(ECI_CHARSET);
        out.push(26);
    }
    let len = msg.len();
    match compaction {
        Compaction::Text => {
            encode_text(&msg, 0, len, &mut out, Sub::Alpha)?;
        }
        Compaction::Byte => {
            let bytes = to_bytes(&msg, utf8);
            encode_binary(&bytes, false, &mut out);
        }
        Compaction::Numeric => {
            if !msg.iter().all(|c| c.is_ascii_digit()) {
                return Err(df_err("Compaction.NUMERIC requires digits only"));
            }
            out.push(NUMERIC_LATCH);
            encode_numeric(&msg, 0, len, &mut out);
        }
        Compaction::Auto => {
            #[derive(PartialEq)]
            enum M {
                Text,
                Byte,
                Numeric,
            }
            let mut mode = M::Text;
            let mut sub = Sub::Alpha;
            let mut p = 0;
            while p < len {
                let n = digit_count(&msg, p);
                if n >= 13 {
                    out.push(NUMERIC_LATCH);
                    mode = M::Numeric;
                    sub = Sub::Alpha;
                    encode_numeric(&msg, p, n, &mut out);
                    p += n;
                } else {
                    let t = text_count(&msg, p);
                    if t >= 5 || n == len {
                        if mode != M::Text {
                            out.push(TEXT_LATCH);
                            mode = M::Text;
                            sub = Sub::Alpha;
                        }
                        sub = encode_text(&msg, p, t, &mut out, sub)?;
                        p += t;
                    } else {
                        let b = binary_count(&msg, p).max(1);
                        let bytes = to_bytes(&msg[p..p + b], utf8);
                        if bytes.len() == 1 && mode == M::Text {
                            encode_binary(&bytes, true, &mut out);
                        } else {
                            encode_binary(&bytes, false, &mut out);
                            mode = M::Byte;
                            sub = Sub::Alpha;
                        }
                        p += b;
                    }
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------
// Symbol layout

/// A PDF417 symbol: codeword grid plus layout parameters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub rows: usize,
    pub cols: usize,
    pub ec_level: u8,
    /// All codewords (length descriptor + data + padding + EC), row major.
    pub codewords: Vec<u32>,
}

fn rows_for(m: usize, k: usize, c: usize) -> usize {
    let mut r = (m + 1 + k) / c + 1;
    if c * r >= m + 1 + k + c {
        r -= 1;
    }
    r
}

fn dimensions(m: usize, k: usize, (min_c, max_c, min_r, max_r): (usize, usize, usize, usize)) -> Option<(usize, usize)> {
    let mut ratio = 0.0f64;
    let mut best = None;
    for c in min_c..=max_c {
        let r = rows_for(m, k, c);
        if r < min_r {
            break;
        }
        if r > max_r {
            continue;
        }
        let nr = ((17 * c + 69) as f64 * 0.357) / (r as f64 * 2.0);
        if best.is_some() && (nr - 3.0).abs() > (ratio - 3.0).abs() {
            continue;
        }
        ratio = nr;
        best = Some((c, r));
    }
    if best.is_none() && rows_for(m, k, min_c) < min_r {
        best = Some((min_c, min_r));
    }
    best
}

/// Encode text into a PDF417 codeword symbol.
pub fn encode_symbol(text: &str, opts: &Pdf417Options) -> Result<Symbol> {
    if opts.ec_level > 8 {
        return Err(df_err(format!("PDF417 error correction level must be 0..8, got {}", opts.ec_level)));
    }
    let high = encode_high_level(text, opts.compaction)?;
    let k = ec_count(opts.ec_level);
    let m = high.len();
    if m + k + 1 > 929 {
        return Err(df_err("Encoded message contains too many code words, message too big"));
    }
    let (min_c, max_c, min_r, max_r) = opts.dimensions;
    let dims = dimensions(m, k, opts.dimensions).or_else(|| {
        // beyond ZXing's default 30 rows: the standard allows 3..90 rows
        if (min_c, max_c, min_r, max_r) == (2, 30, 2, 30) {
            dimensions(m, k, (2, 30, 3, 90))
        } else {
            None
        }
    });
    let (cols, rows) = dims.ok_or_else(|| df_err("Unable to fit message in columns"))?;
    let n = cols * rows - k;
    let pad = n.saturating_sub(m + 1);
    let mut cw = Vec::with_capacity(cols * rows);
    cw.push((m + pad + 1) as u32);
    cw.extend_from_slice(&high);
    cw.extend(std::iter::repeat_n(TEXT_LATCH, pad));
    let ec = ec_codewords(&cw, opts.ec_level);
    cw.extend(ec);
    Ok(Symbol { rows, cols, ec_level: opts.ec_level, codewords: cw })
}

fn left_indicator(y: usize, rows: usize, cols: usize, ecl: usize) -> u32 {
    let base = 30 * (y / 3);
    (base
        + match y % 3 {
            0 => (rows - 1) / 3,
            1 => ecl * 3 + (rows - 1) % 3,
            _ => cols - 1,
        }) as u32
}

fn right_indicator(y: usize, rows: usize, cols: usize, ecl: usize) -> u32 {
    let base = 30 * (y / 3);
    (base
        + match y % 3 {
            0 => cols - 1,
            1 => (rows - 1) / 3,
            _ => ecl * 3 + (rows - 1) % 3,
        }) as u32
}

fn push_bits(row: &mut Vec<bool>, pattern: u32, n: usize) {
    for i in (0..n).rev() {
        row.push((pattern >> i) & 1 == 1);
    }
}

/// Module rows of a symbol (one entry per symbol row, `17 * (cols + 4) + 1` modules each).
pub fn module_rows(s: &Symbol) -> Vec<Vec<bool>> {
    let mut out = Vec::with_capacity(s.rows);
    let ecl = s.ec_level as usize;
    for y in 0..s.rows {
        let cluster = y % 3;
        let mut row = Vec::with_capacity(17 * (s.cols + 4) + 1);
        push_bits(&mut row, START_PATTERN, 17);
        push_bits(&mut row, CODEWORD_TABLE[cluster][left_indicator(y, s.rows, s.cols, ecl) as usize], 17);
        for x in 0..s.cols {
            push_bits(&mut row, CODEWORD_TABLE[cluster][s.codewords[y * s.cols + x] as usize], 17);
        }
        push_bits(&mut row, CODEWORD_TABLE[cluster][right_indicator(y, s.rows, s.cols, ecl) as usize], 17);
        push_bits(&mut row, STOP_PATTERN, 18);
        out.push(row);
    }
    out
}

/// Render like ZXing's `PDF417Writer`: rows 4 modules high, integer scaling to fit
/// `width` x `height`, rotated by 90° when the requested image is portrait, 30 px margin.
pub fn render(s: &Symbol, width: usize, height: usize) -> super::png::Gray {
    const ASPECT: usize = 4;
    const MARGIN: usize = 30;
    let rows = module_rows(s);
    let (mw, mh) = (rows[0].len(), rows.len() * ASPECT);
    let rotated = (height > width) != (mw < mh);
    let (ow, oh) = if rotated { (mh, mw) } else { (mw, mh) };
    let scale = (width / ow).min(height / oh).max(1);
    let (iw, ih) = (ow * scale + 2 * MARGIN, oh * scale + 2 * MARGIN);
    let mut g = super::png::Gray { width: iw, height: ih, pixels: vec![255; iw * ih] };
    for (ry, row) in rows.iter().enumerate() {
        for (x, dark) in row.iter().enumerate() {
            if !*dark {
                continue;
            }
            for sy in 0..ASPECT {
                let y = ry * ASPECT + sy;
                // unrotated module (x, y); rotated: ZXing turns the symbol counter-clockwise
                let (px, py) = if rotated { (y, mw - 1 - x) } else { (x, y) };
                for dy in 0..scale {
                    let row0 = (MARGIN + py * scale + dy) * iw + MARGIN + px * scale;
                    g.pixels[row0..row0 + scale].fill(0);
                }
            }
        }
    }
    g
}

// ---------------------------------------------------------------------------------------
// Decoding: image -> codewords

fn pattern_map() -> &'static HashMap<u32, (usize, u32)> {
    static MAP: OnceLock<HashMap<u32, (usize, u32)>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (cl, table) in CODEWORD_TABLE.iter().enumerate() {
            for (cw, p) in table.iter().enumerate() {
                m.insert(*p, (cl, cw as u32));
            }
        }
        m
    })
}

/// Quantise 8 runs (bar first) to a 17-module codeword pattern.
fn runs_to_pattern(runs: &[usize]) -> Option<u32> {
    let widths = quantize(runs, 17)?;
    if widths.iter().any(|w| *w > 6) {
        return None;
    }
    let mut p = 0u32;
    for (i, w) in widths.iter().enumerate() {
        for _ in 0..*w {
            p = (p << 1) | ((i % 2 == 0) as u32);
        }
    }
    Some(p)
}

fn runs_of(row: &[bool]) -> Vec<(bool, usize)> {
    let mut out: Vec<(bool, usize)> = Vec::new();
    for &b in row {
        match out.last_mut() {
            Some((c, n)) if *c == b => *n += 1,
            _ => out.push((b, 1)),
        }
    }
    out
}

/// Quantise runs to `modules` module widths (cumulative rounding, robust against drift).
fn quantize(runs: &[usize], modules: usize) -> Option<Vec<usize>> {
    let total: usize = runs.iter().sum();
    if total < modules {
        return None;
    }
    let mut out = Vec::with_capacity(runs.len());
    let (mut acc, mut prev) = (0usize, 0usize);
    for r in runs {
        acc += r;
        let edge = (acc * modules + total / 2) / total;
        if edge <= prev {
            return None;
        }
        out.push(edge - prev);
        prev = edge;
    }
    Some(out)
}

fn is_start(runs: &[(bool, usize)]) -> bool {
    if runs.len() < 8 || !runs[0].0 {
        return false;
    }
    let w: Vec<usize> = runs[..8].iter().map(|r| r.1).collect();
    quantize(&w, 17).as_deref() == Some(&[8, 1, 1, 1, 1, 1, 1, 3][..])
}

fn is_stop(runs: &[(bool, usize)]) -> bool {
    if runs.len() < 9 || !runs[0].0 {
        return false;
    }
    let w: Vec<usize> = runs[..9].iter().map(|r| r.1).collect();
    quantize(&w, 18).as_deref() == Some(&[7, 1, 1, 3, 1, 1, 1, 2, 1][..])
}

/// Codewords of one scan line: (cluster, codeword or None per slot after the start pattern,
/// whether the stop pattern was seen). Slots are located by position (start pattern origin
/// plus multiples of 17 modules) so a damaged codeword becomes an erasure instead of
/// desynchronising the rest of the row.
fn scan_line(row: &[bool]) -> Option<(usize, Vec<Option<u32>>, bool)> {
    let runs = runs_of(row);
    let mut pos = Vec::with_capacity(runs.len() + 1);
    let mut acc = 0usize;
    for r in &runs {
        pos.push(acc);
        acc += r.1;
    }
    pos.push(acc);
    for s in 0..runs.len() {
        if !is_start(&runs[s..]) {
            continue;
        }
        let x0 = pos[s] as f64;
        let mw0 = (pos[s + 8] - pos[s]) as f64 / 17.0;
        // the stop pattern (first one after the start), refines the module width
        let stop = (s + 8..runs.len()).find(|t| is_stop(&runs[*t..]) && (t + 9 == runs.len() || runs[t + 9].1 as f64 >= 2.0 * mw0));
        let (slots, mw, stopped) = match stop {
            Some(t) => {
                let span = (pos[t] - pos[s]) as f64;
                let n = (span / (17.0 * mw0)).round() as usize;
                if n < 3 {
                    continue;
                }
                (n - 1, span / (17.0 * n as f64), true)
            }
            None => {
                let n = ((acc as f64 - x0) / (17.0 * mw0)) as usize;
                if n < 4 {
                    continue;
                }
                (n - 1, mw0, false)
            }
        };
        let mut words = Vec::with_capacity(slots);
        let mut clusters = [0usize; 3];
        let mut r = s + 8;
        for j in 0..slots {
            let xj = x0 + 17.0 * mw * (j + 1) as f64;
            while r < runs.len() && (pos[r] as f64) < xj - 1.5 * mw {
                r += 1;
            }
            // the dark run starting closest to the expected position
            let mut best: Option<usize> = None;
            let mut k = r;
            while k < runs.len() && (pos[k] as f64) <= xj + 1.5 * mw {
                if runs[k].0 && best.is_none_or(|b| (pos[k] as f64 - xj).abs() < (pos[b] as f64 - xj).abs()) {
                    best = Some(k);
                }
                k += 1;
            }
            let word = best.filter(|b| b + 8 <= runs.len()).and_then(|b| {
                let w: Vec<usize> = runs[b..b + 8].iter().map(|r| r.1).collect();
                let total: usize = w.iter().sum();
                if (total as f64 - 17.0 * mw).abs() > 3.0 * mw {
                    return None;
                }
                runs_to_pattern(&w).and_then(|p| pattern_map().get(&p)).copied()
            });
            if let Some((cl, _)) = word {
                clusters[cl] += 1;
            }
            words.push(word);
        }
        let cluster = (0..3).max_by_key(|c| clusters[*c]).unwrap();
        if clusters[cluster] == 0 {
            continue;
        }
        let vals = words.into_iter().map(|w| w.and_then(|(cl, cw)| (cl == cluster).then_some(cw))).collect();
        return Some((cluster, vals, stopped));
    }
    None
}


/// Read the codeword grid from a binarised image view.
fn read_grid(get: &dyn Fn(usize, usize) -> bool, w: usize, h: usize) -> Option<Symbol> {
    // votes: row -> column -> codeword -> count
    let mut grid: HashMap<usize, Vec<HashMap<u32, usize>>> = HashMap::new();
    let mut votes_rows: HashMap<usize, usize> = HashMap::new();
    let mut votes_cols: HashMap<usize, usize> = HashMap::new();
    let mut votes_ecl: HashMap<usize, usize> = HashMap::new();
    let mut lines = Vec::new();
    let mut row = vec![false; w];
    for y in 0..h {
        for (x, r) in row.iter_mut().enumerate() {
            *r = get(x, y);
        }
        if let Some((cluster, words, stopped)) = scan_line(&row) {
            if stopped && words.len() >= 3 {
                *votes_cols.entry(words.len() - 2).or_default() += 1;
            }
            let left = words[0];
            let right = if stopped { *words.last().unwrap() } else { None };
            let row_no = left.map(|l| 3 * (l as usize / 30) + cluster).or_else(|| right.map(|r| 3 * (r as usize / 30) + cluster));
            let Some(row_no) = row_no else { continue };
            let mut info = |v: Option<u32>, left_side: bool| {
                let Some(v) = v else { return };
                let v = v as usize % 30;
                match (cluster, left_side) {
                    (0, true) | (1, false) => *votes_rows.entry(v * 3).or_default() += 1, // (rows-1)/3
                    (1, true) | (2, false) => {
                        *votes_ecl.entry(v / 3).or_default() += 1;
                        *votes_rows.entry(1000 + v % 3).or_default() += 1; // (rows-1)%3
                    }
                    _ => *votes_cols.entry(v + 1).or_default() += 1,
                }
            };
            info(left, true);
            info(right, false);
            lines.push((row_no, stopped, words));
        }
    }
    let best = |m: &HashMap<usize, usize>| m.iter().max_by_key(|(k, v)| (**v, usize::MAX - **k)).map(|(k, _)| *k);
    let cols = best(&votes_cols)?;
    let r_div: HashMap<usize, usize> = votes_rows.iter().filter(|(k, _)| **k < 1000).map(|(k, v)| (*k, *v)).collect();
    let r_mod: HashMap<usize, usize> = votes_rows.iter().filter(|(k, _)| **k >= 1000).map(|(k, v)| (*k - 1000, *v)).collect();
    let rows = best(&r_div)? + best(&r_mod)? + 1;
    let ecl = best(&votes_ecl)?;
    if !(3..=90).contains(&rows) || !(1..=30).contains(&cols) || ecl > 8 {
        return None;
    }
    for (row_no, stopped, words) in lines {
        if row_no >= rows {
            continue;
        }
        let data: Vec<Option<u32>> = if stopped && words.len() == cols + 2 { words[1..cols + 1].to_vec() } else { words.iter().skip(1).take(cols).copied().collect() };
        let entry = grid.entry(row_no).or_insert_with(|| vec![HashMap::new(); cols]);
        for (x, v) in data.into_iter().enumerate() {
            if let Some(v) = v {
                *entry[x].entry(v).or_default() += 1;
            }
        }
    }
    let mut codewords = Vec::with_capacity(rows * cols);
    let mut erasures = Vec::new();
    for y in 0..rows {
        for x in 0..cols {
            let v = grid.get(&y).and_then(|r| r[x].iter().max_by_key(|(k, v)| (**v, u32::MAX - **k)).map(|(k, _)| *k));
            if v.is_none() {
                erasures.push(y * cols + x);
            }
            codewords.push(v.unwrap_or(0));
        }
    }
    let mut sym = Symbol { rows, cols, ec_level: ecl as u8, codewords };
    correct_symbol(&mut sym, &erasures)?;
    Some(sym)
}

fn correct_symbol(sym: &mut Symbol, erasures: &[usize]) -> Option<usize> {
    let k = ec_count(sym.ec_level);
    if sym.codewords.len() <= k {
        return None;
    }
    correct(&mut sym.codewords, k, erasures)
}

/// Locate and decode a PDF417 symbol in a binarised image (all four 90° orientations).
pub fn find_and_decode(b: &Bin) -> Option<String> {
    let (w, h) = (b.width, b.height);
    let views: [(usize, usize, Box<dyn Fn(usize, usize) -> bool + '_>); 4] = [
        (w, h, Box::new(|x, y| b.get(x, y))),
        (w, h, Box::new(move |x, y| b.get(w - 1 - x, h - 1 - y))),
        (h, w, Box::new(move |x, y| b.get(w - 1 - y, x))),
        (h, w, Box::new(move |x, y| b.get(y, h - 1 - x))),
    ];
    for (vw, vh, get) in views.iter() {
        if let Some(sym) = read_grid(get.as_ref(), *vw, *vh) {
            if let Ok(t) = decode_codewords(&sym) {
                return Some(t);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------------------
// High-level decoding

struct Out {
    text: String,
    bytes: Vec<u8>,
    eci: Option<u32>,
}

impl Out {
    fn flush(&mut self) {
        if self.bytes.is_empty() {
            return;
        }
        let b = std::mem::take(&mut self.bytes);
        match self.eci {
            Some(26) => self.text.push_str(&String::from_utf8_lossy(&b)),
            _ => self.text.extend(b.iter().map(|c| *c as char)),
        }
    }
    fn byte(&mut self, b: u8) {
        self.bytes.push(b);
    }
    fn ch(&mut self, c: char) {
        self.flush();
        self.text.push(c);
    }
    fn set_eci(&mut self, e: u32) {
        self.flush();
        self.eci = Some(e);
    }
}

fn is_mode(c: u32) -> bool {
    c >= 900
}

/// Decode the data codewords of a corrected symbol to text.
pub fn decode_codewords(sym: &Symbol) -> Result<String> {
    let k = ec_count(sym.ec_level);
    let n = sym.codewords[0] as usize;
    if n == 0 || n > sym.codewords.len() - k {
        return Err(df_err(format!("Invalid PDF417 symbol length descriptor {}", n)));
    }
    decode_data(&sym.codewords[1..n])
}

/// Decode data codewords (after the length descriptor).
pub fn decode_data(cw: &[u32]) -> Result<String> {
    let mut out = Out { text: String::new(), bytes: Vec::new(), eci: None };
    let mut i = 0;
    let mut sub = Sub::Alpha;
    // text mode is the default at the start of a symbol
    let mut mode = TEXT_LATCH;
    while i < cw.len() {
        let c = cw[i];
        if is_mode(c) {
            i += 1;
            match c {
                TEXT_LATCH => {
                    mode = TEXT_LATCH;
                    sub = Sub::Alpha;
                }
                BYTE_LATCH | BYTE_LATCH_6 | NUMERIC_LATCH => mode = c,
                BYTE_SHIFT => {
                    if i < cw.len() && cw[i] < 256 {
                        out.byte(cw[i] as u8);
                        i += 1;
                    }
                }
                ECI_CHARSET => {
                    if i < cw.len() {
                        out.set_eci(cw[i]);
                        i += 1;
                    }
                }
                ECI_GENERAL => i += 2,
                ECI_USER => i += 1,
                MACRO_CONTROL | MACRO_OPTIONAL | MACRO_TERMINATOR => break,
                _ => {}
            }
            continue;
        }
        let end = (i..cw.len()).find(|j| is_mode(cw[*j]) && cw[*j] != BYTE_SHIFT || (mode != TEXT_LATCH && is_mode(cw[*j]))).unwrap_or(cw.len());
        match mode {
            TEXT_LATCH => {
                // text runs may embed byte shifts (913)
                let mut j = i;
                while j < end {
                    if cw[j] == BYTE_SHIFT {
                        if j + 1 < end {
                            out.byte(cw[j + 1] as u8);
                        }
                        j += 2;
                        continue;
                    }
                    let mut vals = Vec::new();
                    while j < end && cw[j] != BYTE_SHIFT {
                        vals.push(cw[j] / 30);
                        vals.push(cw[j] % 30);
                        j += 1;
                    }
                    sub = decode_text(&vals, sub, &mut out);
                }
            }
            NUMERIC_LATCH => {
                for chunk in cw[i..end].chunks(15) {
                    let digits = base900_to_decimal(chunk);
                    if !digits.starts_with('1') {
                        return Err(df_err("Invalid PDF417 numeric compaction group"));
                    }
                    for d in digits[1..].chars() {
                        out.ch(d);
                    }
                }
            }
            _ => {
                let seq = &cw[i..end];
                let groups = if mode == BYTE_LATCH_6 {
                    seq.len() / 5
                } else {
                    let s = if seq.len() % 5 == 0 { 5 } else { seq.len() % 5 };
                    (seq.len() - s.min(seq.len())) / 5
                };
                for g in 0..groups {
                    let mut t: u64 = 0;
                    for c in &seq[g * 5..g * 5 + 5] {
                        t = t * 900 + *c as u64;
                    }
                    for s in (0..6).rev() {
                        out.byte((t >> (8 * s)) as u8);
                    }
                }
                for c in &seq[groups * 5..] {
                    out.byte(*c as u8);
                }
            }
        }
        i = end;
    }
    out.flush();
    Ok(out.text)
}

fn base900_to_decimal(cw: &[u32]) -> String {
    // big number in base 10^9 limbs (least significant first)
    let mut limbs: Vec<u64> = vec![0];
    for c in cw {
        let mut carry = *c as u64;
        for l in limbs.iter_mut() {
            let v = *l * 900 + carry;
            *l = v % 1_000_000_000;
            carry = v / 1_000_000_000;
        }
        while carry > 0 {
            limbs.push(carry % 1_000_000_000);
            carry /= 1_000_000_000;
        }
    }
    let mut s = limbs.last().unwrap().to_string();
    for l in limbs.iter().rev().skip(1) {
        s.push_str(&format!("{:09}", l));
    }
    s
}

fn decode_text(vals: &[u32], initial: Sub, out: &mut Out) -> Sub {
    let mut sub = initial;
    let mut shift: Option<Sub> = None;
    for &v in vals {
        let cur = shift.unwrap_or(sub);
        shift = None;
        match cur {
            Sub::Alpha => match v {
                0..=25 => out.ch((b'A' + v as u8) as char),
                26 => out.ch(' '),
                27 => sub = Sub::Lower,
                28 => sub = Sub::Mixed,
                _ => shift = Some(Sub::Punct),
            },
            Sub::Lower => match v {
                0..=25 => out.ch((b'a' + v as u8) as char),
                26 => out.ch(' '),
                27 => shift = Some(Sub::Alpha),
                28 => sub = Sub::Mixed,
                _ => shift = Some(Sub::Punct),
            },
            Sub::Mixed => match v {
                25 => sub = Sub::Punct,
                26 => out.ch(' '),
                27 => sub = Sub::Lower,
                28 => sub = Sub::Alpha,
                29 => shift = Some(Sub::Punct),
                _ => out.ch(MIXED_RAW[v as usize] as char),
            },
            Sub::Punct => match v {
                29 => sub = Sub::Alpha,
                _ => out.ch(PUNCT_RAW[v as usize] as char),
            },
        }
    }
    sub
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elements(p: u32, n: usize) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        let mut prev = None;
        for i in (0..n).rev() {
            let b = (p >> i) & 1;
            if Some(b) == prev {
                *out.last_mut().unwrap() += 1;
            } else {
                out.push(1);
                prev = Some(b);
            }
        }
        out
    }

    #[test]
    fn cluster_tables_are_well_formed() {
        for (k, table) in CODEWORD_TABLE.iter().enumerate() {
            let mut seen = std::collections::HashSet::new();
            for p in table.iter() {
                assert!(seen.insert(*p), "duplicate {:x}", p);
                assert_eq!(p >> 16, 1, "starts with a bar");
                assert_eq!(p & 1, 0, "ends with a space");
                let e = elements(*p, 17);
                assert_eq!(e.len(), 8);
                assert!(e.iter().all(|w| (1..=6).contains(w)));
                // cluster number (b1 - b2 + b3 - b4 + 9) mod 9 over the bar widths
                assert_eq!((e[0] + e[4] + 9 - e[2] - e[6]) % 9, 3 * k);
            }
        }
        assert_eq!(elements(START_PATTERN, 17), vec![8, 1, 1, 1, 1, 1, 1, 3]);
        assert_eq!(elements(STOP_PATTERN, 18), vec![7, 1, 1, 3, 1, 1, 1, 2, 1]);
        // spot checks against ZXing's decoder table (PDF417Common SYMBOL_TABLE/CODEWORD_TABLE)
        assert_eq!(pattern_map()[&0x1025e], (2, (2627 - 1) % 929));
        assert_eq!(pattern_map()[&0x1027a], (1, (1819 - 1) % 929));
        assert_eq!(pattern_map()[&0x10422], (0, (902 - 1) % 929));
    }

    #[test]
    fn generator_matches_iso_coefficients() {
        // level 0: (x-3)(x-9) = x^2 - 12x + 27  (ZXing EC_COEFFICIENTS[0] = {27, 917})
        assert_eq!(generator(2), vec![1, 917, 27]);
        // level 1 low coefficients from ISO/IEC 15438 Annex F / ZXing: {522, 568, 723, 809}
        let g = generator(4);
        assert_eq!(&[g[4], g[3], g[2], g[1]], &[522, 568, 723, 809]);
        for level in 0..=8u8 {
            let k = ec_count(level);
            let data: Vec<u32> = (0..20).map(|i| (i * 37 + 5) % 900).collect();
            let mut cw = data.clone();
            cw.extend(ec_codewords(&data, level));
            for j in 1..=k {
                let x = pow(3, j as u32);
                assert_eq!(cw.iter().fold(0, |a, c| add(mul(a, x), *c)), 0);
            }
        }
    }

    #[test]
    fn reed_solomon_errors_and_erasures() {
        let data: Vec<u32> = (0..40).map(|i| (i * 131 + 7) % 929).collect();
        for level in [1u8, 3, 5] {
            let k = ec_count(level);
            let mut good = data.clone();
            good.extend(ec_codewords(&data, level));
            // k/2 errors
            let mut bad = good.clone();
            for e in 0..k / 2 {
                bad[(e * 7 + 3) % good.len()] = (bad[(e * 7 + 3) % good.len()] + 100 + e as u32) % 929;
            }
            assert_eq!(correct(&mut bad, k, &[]), Some(k / 2), "level {}", level);
            assert_eq!(bad, good);
            // k erasures
            let mut bad = good.clone();
            let er: Vec<usize> = (0..k).map(|i| (i * 5 + 1) % good.len()).collect();
            for e in &er {
                bad[*e] = 0;
            }
            assert!(correct(&mut bad, k, &er).is_some());
            assert_eq!(bad, good);
            // mixed: e errors + f erasures with 2e + f <= k
            let mut bad = good.clone();
            let er: Vec<usize> = (0..k / 2).map(|i| i * 3).collect();
            for e in &er {
                bad[*e] = 1;
            }
            for e in 0..k / 4 {
                let p = good.len() - 1 - e * 2;
                bad[p] = (bad[p] + 1) % 929;
            }
            assert!(correct(&mut bad, k, &er).is_some());
            assert_eq!(bad, good);
        }
    }

    #[test]
    fn zxing_high_level_vectors() {
        // ZXing PDF417EncoderTestCase (with UTF-8 the codewords start with ECI 927 26; plain
        // ISO-8859-1 text has no ECI prefix here)
        assert_eq!(encode_high_level("ABCD", Compaction::Text).unwrap(), vec![1, 63]);
        assert_eq!(encode_high_level("1234", Compaction::Numeric).unwrap(), vec![902, 12, 434]);
        assert_eq!(encode_high_level("abcd", Compaction::Byte).unwrap(), vec![901, 97, 98, 99, 100]);
        assert_eq!(encode_high_level("ABCD", Compaction::Auto).unwrap(), vec![901, 65, 66, 67, 68]);
        assert_eq!(encode_high_level("€ 1", Compaction::Auto).unwrap()[..2], [927, 26]);
        assert!(encode_high_level("€ 123,45", Compaction::Text).unwrap_err().message.contains("8364"));
        assert!(encode_high_level("", Compaction::Auto).is_err());
    }

    #[test]
    fn high_level_round_trip() {
        let samples = [
            "Hello, World!",
            "PDF417 mixed CASE text with punctuation: [a@b.c] {x|y} ~`\"'?",
            "12345678901234567890123456789012345678901234567890123",
            "Invoice 2026-11-01 total 1234567890123 EUR",
            "Grüße aus Köln — 日本語 ✓",
            "\u{1}\u{2}\u{ff} binary bytes",
            "a",
            "1",
            "tab\tcr\rlf\n",
            "ABCDEF",
            "abc123def456ghi",
            "#$%&*+-./:=^",
        ];
        for s in samples {
            for comp in [Compaction::Auto, Compaction::Byte] {
                let cw = encode_high_level(s, comp).unwrap();
                assert_eq!(decode_data(&cw).unwrap(), s, "{:?} {:?}", s, comp);
            }
            if s.chars().all(is_text) {
                let cw = encode_high_level(s, Compaction::Text).unwrap();
                assert_eq!(decode_data(&cw).unwrap(), s, "text {:?}", s);
            }
        }
        let digits: String = (0..200).map(|i| char::from(b'0' + (i * 7 % 10) as u8)).collect();
        let cw = encode_high_level(&digits, Compaction::Numeric).unwrap();
        assert_eq!(decode_data(&cw).unwrap(), digits);
        let bytes6 = "abcdef";
        assert_eq!(encode_high_level(bytes6, Compaction::Byte).unwrap()[0], 924);
        assert_eq!(decode_data(&encode_high_level(bytes6, Compaction::Byte).unwrap()).unwrap(), bytes6);
    }

    #[test]
    fn layout_dimensions_and_indicators() {
        let s = encode_symbol("Hello PDF417", &Pdf417Options::default()).unwrap();
        assert_eq!(s.codewords.len(), s.rows * s.cols);
        assert_eq!(s.codewords[0] as usize, s.rows * s.cols - 8);
        let rows = module_rows(&s);
        assert_eq!(rows[0].len(), 17 * (s.cols + 4) + 1);
        // every row starts with the start pattern and ends with the stop pattern
        for r in &rows {
            let start: u32 = r[..17].iter().fold(0, |a, b| (a << 1) | *b as u32);
            let stop: u32 = r[r.len() - 18..].iter().fold(0, |a, b| (a << 1) | *b as u32);
            assert_eq!((start, stop), (START_PATTERN, STOP_PATTERN));
        }
        // ZXing determineDimensions sample (issue 1831): 20 source + 8 EC codewords
        assert_eq!(dimensions(20, 8, (1, 30, 7, 10)).map(|(c, r)| c * r >= 29 && (7..=10).contains(&r)), Some(true));
        assert_eq!(dimensions(20, 8, (12, 12, 1, 17)), Some((12, 3)));
        for level in 0..=8u8 {
            let s = encode_symbol("Error correction level test 0123456789", &Pdf417Options { ec_level: level, ..Default::default() }).unwrap();
            let d = decode_codewords(&s).unwrap();
            assert_eq!(d, "Error correction level test 0123456789");
            assert_eq!(left_indicator(1, s.rows, s.cols, level as usize) as usize % 30 / 3, level as usize);
        }
        assert!(encode_symbol(&"x".repeat(2000), &Pdf417Options::default()).is_err());
    }

    fn bin_of(g: &super::super::png::Gray) -> Bin {
        super::super::detect::binarize(g).unwrap()
    }

    #[test]
    fn image_round_trip_orientations_and_damage() {
        let text = "PDF417 image round trip: https://camel.apache.org ✓ 1234567890123";
        let s = encode_symbol(text, &Pdf417Options::default()).unwrap();
        let img = render(&s, 100, 100);
        assert_eq!(find_and_decode(&bin_of(&img)).as_deref(), Some(text));
        // portrait request rotates the symbol
        let tall = render(&s, 100, 600);
        assert!(tall.height > tall.width);
        assert_eq!(find_and_decode(&bin_of(&tall)).as_deref(), Some(text));
        // scaled and turned upside down
        let big = render(&s, 900, 300);
        let mut flipped = big.clone();
        flipped.pixels.reverse();
        assert_eq!(find_and_decode(&bin_of(&flipped)).as_deref(), Some(text));
        // a damaged stripe through the middle columns (erasures / errors corrected by RS)
        let s = encode_symbol(text, &Pdf417Options { ec_level: 5, ..Default::default() }).unwrap();
        let mut img = render(&s, 500, 200);
        let w = img.width;
        for y in 0..img.height {
            for x in w / 2..w / 2 + 25 {
                img.pixels[y * w + x] = 255;
            }
        }
        assert_eq!(find_and_decode(&bin_of(&img)).as_deref(), Some(text));
        // long payloads use more than 30 rows
        let long: String = (0..1500).map(|i| char::from(b'a' + (i % 26) as u8)).collect();
        let s = encode_symbol(&long, &Pdf417Options { ec_level: 1, ..Default::default() }).unwrap();
        assert_eq!(find_and_decode(&bin_of(&render(&s, 0, 0))).as_deref(), Some(long.as_str()));
    }

    #[test]
    fn macro_pdf417_control_block_is_skipped() {
        let mut cw = encode_high_level("Part one", Compaction::Auto).unwrap();
        cw.extend([MACRO_CONTROL, 1, 2, MACRO_TERMINATOR]);
        assert_eq!(decode_data(&cw).unwrap(), "Part one");
    }
}
