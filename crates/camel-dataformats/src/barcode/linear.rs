//! Further linear symbologies, following ZXing's readers and writers: Codabar, Code 93 (full
//! ASCII), ITF (Interleaved 2 of 5), EAN-8, UPC-E and the UPC/EAN 2- and 5-digit add-ons
//! (`UPC_EAN_EXTENSION`, decode only like ZXing). Encoders produce module rows (`true` = bar,
//! no quiet zone); decoders read a thresholded pixel row left to right (callers also try the
//! reversed row).

use camel_core::error::Result;

use super::oned::{ean_check_digit, push_widths, variance, FIRST_DIGIT, L_PATTERNS};
use crate::df_err;

/// Run lengths of a row, starting with a (possibly empty) light run, so dark runs are at odd
/// indices; a row ending dark gets an empty trailing light run.
pub fn runs(row: &[bool]) -> Vec<usize> {
    let mut out = vec![0usize];
    let mut cur = false;
    for &b in row {
        if b == cur {
            *out.last_mut().unwrap() += 1;
        } else {
            out.push(1);
            cur = b;
        }
    }
    if out.len() % 2 == 0 {
        out.push(0);
    }
    out
}

fn sum(r: &[usize]) -> usize {
    r.iter().sum()
}

// ---------------------------------------------------------------------------------------
// Codabar

const CODABAR_ALPHABET: &[u8] = b"0123456789-$:/.+ABCD";
const CODABAR: [u16; 20] = [0x003, 0x006, 0x009, 0x060, 0x012, 0x042, 0x021, 0x024, 0x030, 0x048, 0x00c, 0x018, 0x045, 0x051, 0x054, 0x015, 0x01A, 0x029, 0x00B, 0x00E];

/// Encode Codabar like ZXing's `CodaBarWriter`: start/stop `A`..`D` (or `T`, `N`, `*`, `E`)
/// may be given, otherwise `A` is added on both sides; wide elements are two modules.
pub fn encode_codabar(text: &str) -> Result<Vec<bool>> {
    let up: Vec<u8> = text.bytes().map(|b| b.to_ascii_uppercase()).collect();
    let normal = |c: u8| b"ABCD".contains(&c);
    let alt = |c: u8| b"TN*E".contains(&c);
    let contents: Vec<u8> = if up.len() < 2 {
        [b"A".as_slice(), &up, b"A"].concat()
    } else {
        let (f, l) = (up[0], up[up.len() - 1]);
        if normal(f) {
            if !normal(l) {
                return Err(df_err(format!("Invalid start/end guards: {}", text)));
            }
            up.clone()
        } else if alt(f) {
            if !alt(l) {
                return Err(df_err(format!("Invalid start/end guards: {}", text)));
            }
            up.clone()
        } else {
            if normal(l) || alt(l) {
                return Err(df_err(format!("Invalid start/end guards: {}", text)));
            }
            [b"A".as_slice(), &up, b"A"].concat()
        }
    };
    let mut out = Vec::new();
    let n = contents.len();
    for (i, &c0) in contents.iter().enumerate() {
        let c = if i == 0 || i == n - 1 {
            match c0 {
                b'T' => b'A',
                b'N' => b'B',
                b'*' => b'C',
                b'E' => b'D',
                o => o,
            }
        } else {
            if !(c0.is_ascii_digit() || b"-$/:+.".contains(&c0)) {
                return Err(df_err(format!("Cannot encode : '{}'", c0 as char)));
            }
            c0
        };
        let k = CODABAR_ALPHABET.iter().position(|a| *a == c).ok_or_else(|| df_err(format!("Cannot encode : '{}'", c as char)))?;
        let widths: Vec<u8> = (0..7).map(|b| if CODABAR[k] >> (6 - b) & 1 == 1 { 2 } else { 1 }).collect();
        push_widths(&mut out, &widths, true);
        if i < n - 1 {
            out.push(false);
        }
    }
    Ok(out)
}

/// Classify the seven elements of a Codabar character (bars and spaces thresholded
/// separately like ZXing; a class without a clearly wider element is all narrow).
fn codabar_char(r: &[usize]) -> Option<usize> {
    let class = |idx: &[usize]| -> f64 {
        let (mn, mx) = idx.iter().fold((usize::MAX, 0), |a, i| (a.0.min(r[*i]), a.1.max(r[*i])));
        if (mx as f64) < mn as f64 * 1.5 {
            f64::MAX
        } else {
            (mn + mx) as f64 / 2.0
        }
    };
    let tb = class(&[0, 2, 4, 6]);
    let ts = class(&[1, 3, 5]);
    let mut p = 0u16;
    for (i, w) in r.iter().enumerate().take(7) {
        let t = if i % 2 == 0 { tb } else { ts };
        p = (p << 1) | (*w as f64 > t) as u16;
    }
    CODABAR.iter().position(|c| *c == p)
}

/// Decode a Codabar row; like ZXing the start/stop characters are not returned.
pub fn decode_codabar(row: &[bool]) -> Option<String> {
    let r = runs(row);
    let mut i = 1;
    while i + 7 < r.len() {
        let width = sum(&r[i..i + 7]);
        // ZXing: whitespace of half the start character before it, unless it is the row start
        if r[i - 1] * 2 >= width || (i == 1 && r[0] > 0) {
            if codabar_char(&r[i..i + 7]).is_some_and(|k| k >= 16) {
                let mut out = String::new();
                let mut j = i + 8;
                while j + 7 < r.len() {
                    let w = sum(&r[j..j + 7]) as f64;
                    let gap = r[j - 1] as f64;
                    if w < width as f64 * 0.65 || w > width as f64 * 1.5 || gap > width as f64 * 0.5 {
                        break;
                    }
                    let Some(c) = codabar_char(&r[j..j + 7]) else { break };
                    if c >= 16 {
                        // stop character: needs a quiet zone of half a character
                        let after = r[j + 7];
                        let at_end = j + 8 >= r.len();
                        if !out.is_empty() && (after as f64 * 2.0 >= w || (at_end && after > 0)) {
                            return Some(out);
                        }
                        break;
                    }
                    out.push(CODABAR_ALPHABET[c] as char);
                    j += 8;
                }
            }
        }
        i += 2;
    }
    None
}

// ---------------------------------------------------------------------------------------
// Code 93

const CODE93_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-. $/+%abcd*";
const CODE93: [u16; 48] = [
    0x114, 0x148, 0x144, 0x142, 0x128, 0x124, 0x122, 0x150, 0x112, 0x10A, 0x1A8, 0x1A4, 0x1A2, 0x194, 0x192, 0x18A, 0x168, 0x164, 0x162, 0x134, 0x11A, 0x158, 0x14C, 0x146, 0x12C, 0x116, 0x1B4, 0x1B2,
    0x1AC, 0x1A6, 0x196, 0x19A, 0x16C, 0x166, 0x136, 0x13A, 0x12E, 0x1D4, 0x1D2, 0x1CA, 0x16E, 0x176, 0x1AE, 0x126, 0x1DA, 0x1D6, 0x132, 0x15E,
];
const CODE93_STAR: usize = 47;

fn code93_widths(code: u16) -> [u8; 6] {
    let mut w = [0u8; 6];
    let mut k = 0;
    let mut prev = true;
    for b in (0..9).rev() {
        let bit = code >> b & 1 == 1;
        if bit != prev {
            k += 1;
            prev = bit;
        }
        w[k] += 1;
    }
    w
}

/// ZXing `Code93Writer.convertToExtended`: full ASCII via the shift characters a-d.
fn code93_extended(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for ch in text.chars() {
        let c = ch as u32;
        if c > 127 {
            return Err(df_err(format!("Requested content contains a non-encodable character: '{}'", ch)));
        }
        let c = c as u8;
        match c {
            0 => out.extend_from_slice(b"bU"),
            1..=26 => out.extend_from_slice(&[b'a', b'A' + c - 1]),
            27..=31 => out.extend_from_slice(&[b'b', b'A' + c - 27]),
            b' ' | b'$' | b'%' | b'+' => out.push(c),
            b'!'..=b',' => out.extend_from_slice(&[b'c', b'A' + c - b'!']),
            b'-'..=b'9' => out.push(c),
            b':' => out.extend_from_slice(b"cZ"),
            b';'..=b'?' => out.extend_from_slice(&[b'b', b'F' + c - b';']),
            b'@' => out.extend_from_slice(b"bV"),
            b'A'..=b'Z' => out.push(c),
            b'['..=b'_' => out.extend_from_slice(&[b'b', b'K' + c - b'[']),
            b'`' => out.extend_from_slice(b"bW"),
            b'a'..=b'z' => out.extend_from_slice(&[b'd', b'A' + c - b'a']),
            _ => out.extend_from_slice(&[b'b', b'P' + c - b'{']),
        }
    }
    Ok(out)
}

fn code93_check(idx: &[usize], max_weight: usize) -> usize {
    let mut w = 1;
    let mut total = 0;
    for v in idx.iter().rev() {
        total += v * w;
        w += 1;
        if w > max_weight {
            w = 1;
        }
    }
    total % 47
}

/// Encode Code 93 like ZXing's `Code93Writer` (full ASCII, two check characters, termination bar).
pub fn encode_code93(text: &str) -> Result<Vec<bool>> {
    let ext = code93_extended(text)?;
    if ext.len() > 80 {
        return Err(df_err(format!("Requested contents should be less than 80 digits long after converting to extended encoding, but got {}", ext.len())));
    }
    let mut idx: Vec<usize> = ext.iter().map(|c| CODE93_ALPHABET.iter().position(|a| a == c).unwrap()).collect();
    let c = code93_check(&idx, 20);
    idx.push(c);
    let k = code93_check(&idx, 15);
    idx.push(k);
    let mut out = Vec::new();
    for v in std::iter::once(CODE93_STAR).chain(idx).chain(std::iter::once(CODE93_STAR)) {
        push_widths(&mut out, &code93_widths(CODE93[v]), true);
    }
    out.push(true);
    Ok(out)
}

fn code93_char(r: &[usize]) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, c) in CODE93.iter().enumerate() {
        let v = variance(r, &code93_widths(*c));
        if best.is_none_or(|b| v < b.1) {
            best = Some((i, v));
        }
    }
    best.filter(|b| b.1 < 0.3).map(|b| b.0)
}

/// ZXing `Code93Reader.decodeExtended`.
fn code93_decode_extended(s: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if (b'a'..=b'd').contains(&c) {
            let next = *s.get(i + 1)?;
            let d = match c {
                b'd' if next.is_ascii_uppercase() => next + 32,
                b'a' if next.is_ascii_uppercase() => next - 64,
                b'b' => match next {
                    b'A'..=b'E' => next - 38,
                    b'F'..=b'J' => next - 11,
                    b'K'..=b'O' => next + 16,
                    b'P'..=b'T' => next + 43,
                    b'U' => 0,
                    b'V' => b'@',
                    b'W' => b'`',
                    b'X' | b'Y' | b'Z' => 127,
                    _ => return None,
                },
                b'c' => match next {
                    b'A'..=b'O' => next - 32,
                    b'Z' => b':',
                    _ => return None,
                },
                _ => return None,
            };
            out.push(d as char);
            i += 2;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    Some(out)
}

/// Decode a Code 93 row (check characters verified and removed, full ASCII expanded).
pub fn decode_code93(row: &[bool]) -> Option<String> {
    let r = runs(row);
    let mut i = 1;
    while i + 6 < r.len() {
        if code93_char(&r[i..i + 6]) == Some(CODE93_STAR) {
            let mut width = sum(&r[i..i + 6]) as f64;
            let mut idx = Vec::new();
            let mut j = i + 6;
            while j + 6 < r.len() {
                let w = sum(&r[j..j + 6]) as f64;
                if w < width * 0.75 || w > width * 1.33 {
                    break;
                }
                let Some(c) = code93_char(&r[j..j + 6]) else { break };
                width = w;
                j += 6;
                if c == CODE93_STAR {
                    // termination bar
                    let unit = width / 9.0;
                    if j < r.len() && (r[j] as f64) < unit * 2.0 && idx.len() >= 3 {
                        let n = idx.len();
                        if code93_check(&idx[..n - 2], 20) == idx[n - 2] && code93_check(&idx[..n - 1], 15) == idx[n - 1] {
                            let s: Vec<u8> = idx[..n - 2].iter().map(|v| CODE93_ALPHABET[*v]).collect();
                            if let Some(t) = code93_decode_extended(&s) {
                                return Some(t);
                            }
                        }
                    }
                    break;
                }
                idx.push(c);
            }
        }
        i += 2;
    }
    None
}

// ---------------------------------------------------------------------------------------
// ITF

const ITF: [[u8; 5]; 10] = [[1, 1, 3, 3, 1], [3, 1, 1, 1, 3], [1, 3, 1, 1, 3], [3, 3, 1, 1, 1], [1, 1, 3, 1, 3], [3, 1, 3, 1, 1], [1, 3, 3, 1, 1], [1, 1, 1, 3, 3], [3, 1, 1, 3, 1], [1, 3, 1, 3, 1]];
// ZXing ITFReader thresholds (wide elements of 2 or 3 modules are accepted)
const ITF_MAX_AVG: f64 = 0.38;
const ITF_MAX_INDIVIDUAL: f64 = 0.5;
/// ZXing's default allowed ITF lengths (longer ones are accepted too).
const ITF_LENGTHS: [usize; 5] = [6, 8, 10, 12, 14];

/// Encode ITF like ZXing's `ITFWriter` (even number of digits, wide = 3 modules, no check digit).
pub fn encode_itf(text: &str) -> Result<Vec<bool>> {
    if text.len() % 2 != 0 {
        return Err(df_err("The length of the input should be even"));
    }
    if text.len() > 80 {
        return Err(df_err(format!("Requested contents should be less than 80 digits long, but got {}", text.len())));
    }
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(df_err("Input should only contain digits 0-9"));
    }
    let d: Vec<usize> = text.bytes().map(|b| (b - b'0') as usize).collect();
    let mut out = Vec::new();
    push_widths(&mut out, &[1, 1, 1, 1], true);
    for p in d.chunks(2) {
        let mut w = [0u8; 10];
        for j in 0..5 {
            w[2 * j] = ITF[p[0]][j];
            w[2 * j + 1] = ITF[p[1]][j];
        }
        push_widths(&mut out, &w, true);
    }
    push_widths(&mut out, &[3, 1, 1], true);
    Ok(out)
}

/// The digit of five elements (two of them wide).
fn itf_digit(w: [usize; 5]) -> Option<usize> {
    let mut s = w;
    s.sort_unstable_by(|a, b| b.cmp(a));
    if (s[1] as f64) < s[2] as f64 * 1.4 {
        return None;
    }
    let thr = (s[1] + s[2]) as f64 / 2.0;
    let pat: Vec<u8> = w.iter().map(|x| if *x as f64 > thr { 3 } else { 1 }).collect();
    ITF.iter().position(|p| p[..] == pat[..])
}

/// Decode an ITF row (ZXing's quiet zones and allowed lengths).
pub fn decode_itf(row: &[bool]) -> Option<String> {
    let r = runs(row);
    let mut i = 1;
    while i + 4 < r.len() {
        let start = &r[i..i + 4];
        if pattern_variance(start, &[1, 1, 1, 1], ITF_MAX_INDIVIDUAL) < ITF_MAX_AVG {
            let narrow = sum(start) as f64 / 4.0;
            let lead = r[i - 1] as f64;
            let before: usize = r[..i].iter().sum();
            if lead >= (narrow * 10.0).min(before as f64) * 0.9 && lead > 0.0 {
                if let Some(t) = itf_from(&r, i + 4, narrow) {
                    return Some(t);
                }
            }
        }
        i += 2;
    }
    None
}

fn itf_from(r: &[usize], mut j: usize, narrow: f64) -> Option<String> {
    let mut out = String::new();
    loop {
        // end pattern: wide bar, narrow space, narrow bar, quiet zone
        if j + 3 < r.len() && [[3, 1, 1], [2, 1, 1]].iter().any(|p| pattern_variance(&r[j..j + 3], p, ITF_MAX_INDIVIDUAL) < ITF_MAX_AVG) && !out.is_empty() {
            let after = r[j + 3] as f64;
            let rest: usize = r[j + 3..].iter().sum();
            if after >= (narrow * 10.0).min(rest as f64) * 0.9 && after > 0.0 {
                let ok = ITF_LENGTHS.contains(&out.len()) || out.len() > 14;
                return ok.then_some(out);
            }
        }
        if j + 10 > r.len() {
            return None;
        }
        let w = &r[j..j + 10];
        let unit = sum(w) as f64 / 18.0;
        if unit < narrow * 0.6 || unit > narrow * 1.6 {
            return None;
        }
        let a = itf_digit([w[0], w[2], w[4], w[6], w[8]])?;
        let b = itf_digit([w[1], w[3], w[5], w[7], w[9]])?;
        out.push((b'0' + a as u8) as char);
        out.push((b'0' + b as u8) as char);
        j += 10;
    }
}

// ---------------------------------------------------------------------------------------
// EAN-8, UPC-E, EAN-13 with add-ons

/// UPC-E parity patterns by number system and check digit (bit set = G pattern).
const UPCE_PARITY: [[u8; 10]; 2] = [[0x38, 0x34, 0x32, 0x31, 0x2C, 0x26, 0x23, 0x2A, 0x29, 0x25], [0x07, 0x0B, 0x0D, 0x0E, 0x13, 0x19, 0x1C, 0x15, 0x16, 0x1A]];
/// Add-on 5 parity patterns by check digit (bit set = G pattern).
const EXT5_PARITY: [u8; 10] = [0x18, 0x14, 0x12, 0x11, 0x0C, 0x06, 0x03, 0x0A, 0x09, 0x05];

fn digits_of(text: &str, name: &str) -> Result<Vec<u8>> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(df_err(format!("{} contents must be digits", name)));
    }
    Ok(text.bytes().map(|b| b - b'0').collect())
}

/// Encode EAN-8 modules (7 digits get their check digit, 8 are verified).
pub fn encode_ean8(text: &str) -> Result<Vec<bool>> {
    let mut d = digits_of(text, "EAN-8")?;
    match d.len() {
        7 => d.push(ean_check_digit(&d)),
        8 => {
            if ean_check_digit(&d[..7]) != d[7] {
                return Err(df_err("Contents do not pass checksum"));
            }
        }
        n => return Err(df_err(format!("Requested contents should be 7 or 8 digits long, but got {}", n))),
    }
    let mut out = Vec::new();
    push_widths(&mut out, &[1, 1, 1], true);
    for x in &d[..4] {
        push_widths(&mut out, &L_PATTERNS[*x as usize], false);
    }
    push_widths(&mut out, &[1, 1, 1, 1, 1], false);
    for x in &d[4..] {
        push_widths(&mut out, &L_PATTERNS[*x as usize], true);
    }
    push_widths(&mut out, &[1, 1, 1], true);
    Ok(out)
}

/// ZXing `UPCEReader.convertUPCEtoUPCA` (7 or 8 digits in, 11 or 12 out).
pub fn upce_to_upca(upce: &[u8]) -> Vec<u8> {
    let c = &upce[1..7];
    let mut out = vec![upce[0]];
    match c[5] {
        0..=2 => {
            out.extend_from_slice(&c[..2]);
            out.push(c[5]);
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&c[2..5]);
        }
        3 => {
            out.extend_from_slice(&c[..3]);
            out.extend_from_slice(&[0, 0, 0, 0, 0]);
            out.extend_from_slice(&c[3..5]);
        }
        4 => {
            out.extend_from_slice(&c[..4]);
            out.extend_from_slice(&[0, 0, 0, 0, 0]);
            out.push(c[4]);
        }
        _ => {
            out.extend_from_slice(&c[..5]);
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.push(c[5]);
        }
    }
    if upce.len() >= 8 {
        out.push(upce[7]);
    }
    out
}

/// Encode UPC-E modules like ZXing's `UPCEWriter` (number system 0 or 1, 7 or 8 digits).
pub fn encode_upce(text: &str) -> Result<Vec<bool>> {
    let mut d = digits_of(text, "UPC-E")?;
    match d.len() {
        7 => {
            let a = upce_to_upca(&d);
            d.push(ean_check_digit(&a));
        }
        8 => {
            let a = upce_to_upca(&d);
            if ean_check_digit(&a[..11]) != a[11] {
                return Err(df_err("Contents do not pass checksum"));
            }
        }
        n => return Err(df_err(format!("Requested contents should be 7 or 8 digits long, but got {}", n))),
    }
    if d[0] > 1 {
        return Err(df_err("Number system must be 0 or 1"));
    }
    let parities = UPCE_PARITY[d[0] as usize][d[7] as usize];
    let mut out = Vec::new();
    push_widths(&mut out, &[1, 1, 1], true);
    for (i, x) in d[1..7].iter().enumerate() {
        let mut w = L_PATTERNS[*x as usize];
        if parities >> (5 - i) & 1 == 1 {
            w.reverse();
        }
        push_widths(&mut out, &w, false);
    }
    push_widths(&mut out, &[1, 1, 1, 1, 1, 1], false);
    Ok(out)
}

/// An add-on symbol's modules (2 or 5 digits), starting with its guard. Not a ZXing writer
/// (ZXing only reads add-ons); used to build test images.
pub fn encode_extension(text: &str) -> Result<Vec<bool>> {
    let d = digits_of(text, "UPC/EAN extension")?;
    let parity: u8 = match d.len() {
        2 => {
            let v = (d[0] * 10 + d[1]) % 4;
            v // bits for the two digits, 1 = G
        }
        5 => EXT5_PARITY[ext5_checksum(&d) as usize],
        _ => return Err(df_err("An add-on has 2 or 5 digits")),
    };
    let n = d.len();
    let mut out = Vec::new();
    push_widths(&mut out, &[1, 1, 2], true);
    for (i, x) in d.iter().enumerate() {
        if i > 0 {
            push_widths(&mut out, &[1, 1], false);
        }
        let mut w = L_PATTERNS[*x as usize];
        if parity >> (n - 1 - i) & 1 == 1 {
            w.reverse();
        }
        push_widths(&mut out, &w, false);
    }
    Ok(out)
}

fn ext5_checksum(d: &[u8]) -> u8 {
    ((3 * (d[0] + d[2] + d[4]) as u32 + 9 * (d[1] + d[3]) as u32) % 10) as u8
}

/// A decoded UPC/EAN symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpcEan {
    /// `EAN_13`, `EAN_8` or `UPC_E` (UPC-A is reported by the caller from a leading 0).
    pub kind: UpcKind,
    pub text: String,
    /// The add-on digits, if one follows the symbol.
    pub extension: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpcKind {
    Ean13,
    Ean8,
    UpcE,
}

/// ZXing `OneDReader.patternMatchVariance`: total deviation in pixels over the total width,
/// infinite when one element deviates by more than `max_individual` modules.
pub fn pattern_variance(r: &[usize], pat: &[u8], max_individual: f64) -> f64 {
    let total = sum(r) as f64;
    let len: u32 = pat.iter().map(|p| *p as u32).sum();
    if total < len as f64 {
        return f64::INFINITY;
    }
    let unit = total / len as f64;
    let max = max_individual * unit;
    let mut v = 0.0;
    for (c, p) in r.iter().zip(pat) {
        let d = (*c as f64 - *p as f64 * unit).abs();
        if d > max {
            return f64::INFINITY;
        }
        v += d;
    }
    v / total
}

// ZXing UPCEANReader thresholds
const UPC_MAX_AVG: f64 = 0.48;
const UPC_MAX_INDIVIDUAL: f64 = 0.7;

fn guard_ok(r: &[usize], pat: &[u8]) -> bool {
    pattern_variance(r, pat, UPC_MAX_INDIVIDUAL) < UPC_MAX_AVG
}

/// ZXing `UPCEANReader.decodeDigit` over the L (and, if `g_ok`, G) patterns.
fn upc_digit(r: &[usize], g_ok: bool) -> Option<(u8, bool)> {
    let mut best: Option<(u8, bool, f64)> = None;
    for (d, p) in L_PATTERNS.iter().enumerate() {
        let mut rev = *p;
        rev.reverse();
        for (pat, g) in [(*p, false), (rev, true)] {
            if g && !g_ok {
                continue;
            }
            let v = pattern_variance(r, &pat, UPC_MAX_INDIVIDUAL);
            if v < UPC_MAX_AVG && best.is_none_or(|b| v < b.2) {
                best = Some((d as u8, g, v));
            }
        }
    }
    best.map(|b| (b.0, b.1))
}

fn digits_str(d: &[u8]) -> String {
    d.iter().map(|x| (b'0' + x) as char).collect()
}

/// EAN-13 at dark run `i`; returns the digits and the index of the light run after the end guard.
fn ean13_at(r: &[usize], i: usize) -> Option<(String, usize)> {
    if i + 59 >= r.len() {
        return None;
    }
    let mut digits = Vec::new();
    let mut parity = 0u8;
    for k in 0..6 {
        let (d, g) = upc_digit(&r[i + 3 + 4 * k..i + 7 + 4 * k], true)?;
        digits.push(d);
        if g {
            parity |= 1 << (5 - k);
        }
    }
    if !guard_ok(&r[i + 27..i + 32], &[1, 1, 1, 1, 1]) || !guard_ok(&r[i + 56..i + 59], &[1, 1, 1]) {
        return None;
    }
    for k in 0..6 {
        let (d, g) = upc_digit(&r[i + 32 + 4 * k..i + 36 + 4 * k], false)?;
        if g {
            return None;
        }
        digits.push(d);
    }
    let first = FIRST_DIGIT.iter().position(|p| *p == parity)? as u8;
    let mut all = vec![first];
    all.extend(digits);
    (ean_check_digit(&all[..12]) == all[12]).then(|| (digits_str(&all), i + 59))
}

fn ean8_at(r: &[usize], i: usize) -> Option<(String, usize)> {
    if i + 43 >= r.len() {
        return None;
    }
    let mut d = Vec::new();
    for k in 0..4 {
        let (x, g) = upc_digit(&r[i + 3 + 4 * k..i + 7 + 4 * k], false)?;
        if g {
            return None;
        }
        d.push(x);
    }
    if !guard_ok(&r[i + 19..i + 24], &[1, 1, 1, 1, 1]) || !guard_ok(&r[i + 40..i + 43], &[1, 1, 1]) {
        return None;
    }
    for k in 0..4 {
        let (x, g) = upc_digit(&r[i + 24 + 4 * k..i + 28 + 4 * k], false)?;
        if g {
            return None;
        }
        d.push(x);
    }
    (ean_check_digit(&d[..7]) == d[7]).then(|| (digits_str(&d), i + 43))
}

fn upce_at(r: &[usize], i: usize) -> Option<(String, usize)> {
    if i + 33 >= r.len() {
        return None;
    }
    let mut d = Vec::new();
    let mut lg = 0u8;
    for k in 0..6 {
        let (x, g) = upc_digit(&r[i + 3 + 4 * k..i + 7 + 4 * k], true)?;
        d.push(x);
        if g {
            lg |= 1 << (5 - k);
        }
    }
    if !guard_ok(&r[i + 27..i + 33], &[1, 1, 1, 1, 1, 1]) {
        return None;
    }
    let (ns, check) = (0..2).flat_map(|ns| (0..10).map(move |c| (ns, c))).find(|(ns, c)| UPCE_PARITY[*ns][*c] == lg)?;
    let mut all = vec![ns as u8];
    all.extend(d);
    all.push(check as u8);
    let a = upce_to_upca(&all);
    (ean_check_digit(&a[..11]) == a[11]).then(|| (digits_str(&all), i + 33))
}

/// Every digit (and guard) of a UPC/EAN symbol must have its nominal width within 30% (a
/// sanity check beyond ZXing's per-digit matching that keeps degraded rows of other
/// symbologies from passing as short EAN-8 / UPC-E symbols).
fn widths_consistent(r: &[usize], kind: UpcKind, module: f64) -> bool {
    let groups: &[usize] = match kind {
        UpcKind::Ean13 => &[3, 4, 4, 4, 4, 4, 4, 5, 4, 4, 4, 4, 4, 4, 3],
        UpcKind::Ean8 => &[3, 4, 4, 4, 4, 5, 4, 4, 4, 4, 3],
        UpcKind::UpcE => &[3, 4, 4, 4, 4, 4, 4, 6],
    };
    let mut p = 0;
    for &g in groups {
        let modules = match g {
            4 => 7.0,
            n => n as f64,
        };
        let w = sum(&r[p..p + g]) as f64 / module;
        if (w - modules).abs() > modules * 0.3 {
            return false;
        }
        p += g;
    }
    true
}

/// An add-on starting after the light run `e` (the gap after the main symbol).
fn extension_at(r: &[usize], e: usize, module: f64) -> Option<String> {
    let gap = r[e] as f64 / module;
    if !(5.0..=16.0).contains(&gap) || e + 4 >= r.len() || !guard_ok(&r[e + 1..e + 4], &[1, 1, 2]) {
        return None;
    }
    let unit = sum(&r[e + 1..e + 4]) as f64 / 4.0;
    'len: for n in [5usize, 2] {
        let mut d = Vec::new();
        let mut lg = 0u8;
        let mut j = e + 4;
        for k in 0..n {
            if k > 0 {
                if j + 2 > r.len() || !guard_ok(&r[j..j + 2], &[1, 1]) || (sum(&r[j..j + 2]) as f64) > unit * 3.5 {
                    continue 'len;
                }
                j += 2;
            }
            if j + 4 > r.len() {
                continue 'len;
            }
            let w = sum(&r[j..j + 4]) as f64;
            if w < unit * 4.5 || w > unit * 10.0 {
                continue 'len;
            }
            let Some((x, g)) = upc_digit(&r[j..j + 4], true) else { continue 'len };
            d.push(x);
            if g {
                lg |= 1 << (n - 1 - k);
            }
            j += 4;
        }
        // the add-on must end here: a quiet zone follows
        if j < r.len() && (r[j] as f64) < unit * 4.0 && j + 1 < r.len() {
            continue;
        }
        let ok = if n == 5 { EXT5_PARITY.iter().position(|p| *p == lg) == Some(ext5_checksum(&d) as usize) } else { (d[0] * 10 + d[1]) % 4 == lg };
        if ok {
            return Some(digits_str(&d));
        }
    }
    None
}

/// Decode an EAN-13, EAN-8 or UPC-E symbol (ZXing's order) with an optional add-on.
pub fn decode_upcean(row: &[bool], kinds: &dyn Fn(UpcKind) -> bool) -> Option<UpcEan> {
    let r = runs(row);
    let mut i = 1;
    while i + 3 < r.len() {
        if guard_ok(&r[i..i + 3], &[1, 1, 1]) {
            let guard = sum(&r[i..i + 3]) as f64;
            let before_ok = r[i - 1] as f64 >= guard * 0.9 || r[..i].iter().sum::<usize>() == r[i - 1] && r[i - 1] > 0;
            if before_ok {
                for kind in [UpcKind::Ean13, UpcKind::Ean8, UpcKind::UpcE] {
                    if !kinds(kind) {
                        continue;
                    }
                    let (found, modules, end_guard) = match kind {
                        UpcKind::Ean13 => (ean13_at(&r, i), 95.0, 3.0),
                        UpcKind::Ean8 => (ean8_at(&r, i), 67.0, 3.0),
                        UpcKind::UpcE => (upce_at(&r, i), 51.0, 6.0),
                    };
                    let Some((text, e)) = found else { continue };
                    let width = sum(&r[i..e]) as f64;
                    let module = width / modules;
                    if (r[e] as f64) < end_guard * module * 0.9 || !widths_consistent(&r[i..e], kind, module) {
                        continue;
                    }
                    let extension = extension_at(&r, e, module);
                    return Some(UpcEan { kind, text, extension });
                }
            }
        }
        i += 2;
    }
    None
}

/// ZXing `UPCEANExtension5Support.parseExtension5String` (`SUGGESTED_PRICE`).
pub fn suggested_price(ext: &str) -> Option<String> {
    let b = ext.as_bytes();
    if b.len() != 5 {
        return None;
    }
    let currency = match b[0] {
        b'0' => "\u{a3}",
        b'5' => "$",
        b'9' => match ext {
            "90000" => return None,
            "99991" => return Some("0.00".into()),
            "99990" => return Some("Used".into()),
            _ => "",
        },
        _ => "",
    };
    let raw: u32 = ext[1..].parse().ok()?;
    Some(format!("{}{}.{:02}", currency, raw / 100, raw % 100))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaled(m: &[bool], k: usize, quiet: usize) -> Vec<bool> {
        let mut row = vec![false; quiet * k];
        row.extend(m.iter().flat_map(|b| std::iter::repeat_n(*b, k)));
        row.extend(vec![false; quiet * k]);
        row
    }

    fn s(m: &[bool]) -> String {
        m.iter().map(|b| if *b { '1' } else { '0' }).collect()
    }

    #[test]
    fn code93_table_structure() {
        let mut seen = std::collections::HashSet::new();
        for c in CODE93 {
            let w = code93_widths(c);
            assert_eq!(w.iter().map(|x| *x as u32).sum::<u32>(), 9);
            assert!(w.iter().all(|x| (1..=4).contains(x)));
            assert!(seen.insert(w));
        }
    }

    fn core(expected: &str) -> String {
        expected.replace(' ', "").trim_matches('0').to_string()
    }

    /// Module strings from ZXing's writer tests (`Code93WriterTestCase`, `CodaBarWriterTestCase`,
    /// `ITFWriterTestCase`, `EAN8WriterTestCase`, `UPCEWriterTestCase`); ZXing's strings include
    /// the default side margins, which are stripped here.
    #[test]
    fn zxing_writer_vectors() {
        let c93 = "000001010111101101010001101001001101000101100101001100100101100010101011010001011001\
                   001011000101001101001000110101010110001010011001010001101001011001000101101101101001\
                   101100101101011001101001101100101101100110101011011001011001101001101101001110101000\
                   101001010010001010001001010000101001010001001001001001000101010100001000100101000010\
                   10100111010101000010101011110100000";
        assert_eq!(s(&encode_code93("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789").unwrap()), core(c93));
        let c93x = "00000 101011110 111011010 110010110 100100110 110101000 100100110 100111010 111011010 110101000 \
                    111011010 110010010 111010010 111001010 110101110 101110110 111010110 110101000 \
                    111010110 101011000 100010100 100001010 111010110 100111010 111011010 110001010 \
                    111011010 110011010 110101000 100111010 111011010 100011010 111011010 100101100 \
                    111011010 101101100 100110010 110101000 100110010 100111010 111011010 100010110 \
                    111011010 110100110 110100010 110101100 101011110 100000";
        let x = "\u{0}\u{1}\u{1a}\u{1b}\u{1f} $%+!,09:;@AZ[_`az{\u{7f}";
        assert_eq!(s(&encode_code93(x).unwrap()), core(c93x));
        let cb = "00000 1001001011 0110101001 0101011001 0110101001 0101001101 0110010101 01101101011 01001001011 00000";
        assert_eq!(s(&encode_codabar("B515-3/B").unwrap()), core(cb));
        let cb2 = "00000 1011001001 0101011001 0101001011 0110010101 01011001001 00000";
        assert_eq!(s(&encode_codabar("T123T").unwrap()), core(cb2));
        assert_eq!(encode_codabar("T123456789-$T").unwrap(), encode_codabar("A123456789-$A").unwrap());
        let itf = "0000010101010111000111000101110100010101110001110111010001010001110100011100010101000101011100011101011101000111000101110100010101110001110100000";
        assert_eq!(s(&encode_itf("00123456789012").unwrap()), core(itf));
        assert!(encode_itf("00123456789abc").is_err());
        let e8 = "0000001010001011010111101111010110111010101001110111001010001001011100101000000";
        assert_eq!(s(&encode_ean8("96385074").unwrap()), core(e8));
        assert_eq!(s(&encode_ean8("9638507").unwrap()), core(e8));
        assert!(encode_ean8("96385abc").is_err());
        let ue = "0000000000010101110010100111000101101011110110111001011101010100000000000";
        assert_eq!(s(&encode_upce("05096893").unwrap()), core(ue));
        assert_eq!(s(&encode_upce("0509689").unwrap()), core(ue));
        let ue1 = "0000000000010100100110111101010001101110010000101001000101010100000000000";
        assert_eq!(s(&encode_upce("12345670").unwrap()), core(ue1));
        assert!(encode_upce("05096abc").is_err());
    }

    #[test]
    fn round_trips() {
        for t in ["1234567890", "A40156B", "31117013206375", "-$:/.+"] {
            let m = encode_codabar(t).unwrap();
            let want = if t.starts_with('A') { "40156" } else { t };
            assert_eq!(decode_codabar(&scaled(&m, 3, 10)).as_deref(), Some(want), "{}", t);
        }
        for t in ["CODE 93", "Hello, World! ~{}", "a\tb\u{0}", "1234567890"] {
            let m = encode_code93(t).unwrap();
            assert_eq!(decode_code93(&scaled(&m, 2, 10)).as_deref(), Some(t), "{}", t);
        }
        for t in ["123456", "0053611912", "30712345000010", "1234567890123456"] {
            let m = encode_itf(t).unwrap();
            assert_eq!(decode_itf(&scaled(&m, 2, 10)).as_deref(), Some(t));
        }
        // 4 digits: encodable, but not a length ZXing's reader accepts by default
        assert!(decode_itf(&scaled(&encode_itf("1234").unwrap(), 2, 10)).is_none());
        let all = |_: UpcKind| true;
        let d = decode_upcean(&scaled(&encode_ean8("9638507").unwrap(), 2, 9), &all).unwrap();
        assert_eq!((d.kind, d.text.as_str()), (UpcKind::Ean8, "96385074"));
        let d = decode_upcean(&scaled(&encode_upce("0123456").unwrap(), 2, 9), &all).unwrap();
        assert_eq!((d.kind, d.text.as_str()), (UpcKind::UpcE, "01234565"));
        for ext in ["51299", "12", "07", "00000"] {
            let mut m = super::super::oned::encode_ean13("978073520044").unwrap();
            m.extend(vec![false; 9]);
            m.extend(encode_extension(ext).unwrap());
            let d = decode_upcean(&scaled(&m, 2, 9), &all).unwrap();
            assert_eq!((d.text.as_str(), d.extension.as_deref()), ("9780735200449", Some(ext)));
        }
        assert!(encode_upce("2123456").is_err());
        assert!(encode_ean8("12345678").is_err());
        assert!(encode_itf("123").is_err());
        assert!(encode_codabar("A123").is_err());
        assert_eq!(suggested_price("51299").as_deref(), Some("$12.99"));
        assert_eq!(suggested_price("02495").as_deref(), Some("\u{a3}24.95"));
        assert_eq!(suggested_price("90000"), None);
    }

    #[test]
    #[ignore]
    fn dump_runs() {
        let name = std::env::var("IMG").unwrap();
        let b = std::fs::read(format!("{}/src/barcode/testdata/zxing/{}", env!("CARGO_MANIFEST_DIR"), name)).unwrap();
        let g = super::super::read_image(&b).unwrap();
        let bin = super::super::detect::binarize(&g).unwrap();
        let y: usize = std::env::var("Y").map(|v| v.parse().unwrap()).unwrap_or(bin.height / 2);
        let r = runs(bin.row(y));
        println!("{}x{} y={} runs={:?}", g.width, g.height, y, r);
        let p = super::super::vision::threshold_profile(&super::super::vision::row_profile(&g, y, 1));
        println!("camera runs={:?}", runs(&p));
    }
}
