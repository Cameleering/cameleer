//! Linear symbologies: Code 128 (code sets A/B/C with automatic switching), Code 39,
//! EAN-13 and UPC-A. Encoders produce module rows (`true` = bar, no quiet zone); decoders read
//! a row of pixels (run lengths, scale independent).

use camel_core::error::Result;

use crate::df_err;

const CODE128: [[u8; 6]; 106] = [
    [2, 1, 2, 2, 2, 2], [2, 2, 2, 1, 2, 2], [2, 2, 2, 2, 2, 1], [1, 2, 1, 2, 2, 3], [1, 2, 1, 3, 2, 2],
    [1, 3, 1, 2, 2, 2], [1, 2, 2, 2, 1, 3], [1, 2, 2, 3, 1, 2], [1, 3, 2, 2, 1, 2], [2, 2, 1, 2, 1, 3],
    [2, 2, 1, 3, 1, 2], [2, 3, 1, 2, 1, 2], [1, 1, 2, 2, 3, 2], [1, 2, 2, 1, 3, 2], [1, 2, 2, 2, 3, 1],
    [1, 1, 3, 2, 2, 2], [1, 2, 3, 1, 2, 2], [1, 2, 3, 2, 2, 1], [2, 2, 3, 2, 1, 1], [2, 2, 1, 1, 3, 2],
    [2, 2, 1, 2, 3, 1], [2, 1, 3, 2, 1, 2], [2, 2, 3, 1, 1, 2], [3, 1, 2, 1, 3, 1], [3, 1, 1, 2, 2, 2],
    [3, 2, 1, 1, 2, 2], [3, 2, 1, 2, 2, 1], [3, 1, 2, 2, 1, 2], [3, 2, 2, 1, 1, 2], [3, 2, 2, 2, 1, 1],
    [2, 1, 2, 1, 2, 3], [2, 1, 2, 3, 2, 1], [2, 3, 2, 1, 2, 1], [1, 1, 1, 3, 2, 3], [1, 3, 1, 1, 2, 3],
    [1, 3, 1, 3, 2, 1], [1, 1, 2, 3, 1, 3], [1, 3, 2, 1, 1, 3], [1, 3, 2, 3, 1, 1], [2, 1, 1, 3, 1, 3],
    [2, 3, 1, 1, 1, 3], [2, 3, 1, 3, 1, 1], [1, 1, 2, 1, 3, 3], [1, 1, 2, 3, 3, 1], [1, 3, 2, 1, 3, 1],
    [1, 1, 3, 1, 2, 3], [1, 1, 3, 3, 2, 1], [1, 3, 3, 1, 2, 1], [3, 1, 3, 1, 2, 1], [2, 1, 1, 3, 3, 1],
    [2, 3, 1, 1, 3, 1], [2, 1, 3, 1, 1, 3], [2, 1, 3, 3, 1, 1], [2, 1, 3, 1, 3, 1], [3, 1, 1, 1, 2, 3],
    [3, 1, 1, 3, 2, 1], [3, 3, 1, 1, 2, 1], [3, 1, 2, 1, 1, 3], [3, 1, 2, 3, 1, 1], [3, 3, 2, 1, 1, 1],
    [3, 1, 4, 1, 1, 1], [2, 2, 1, 4, 1, 1], [4, 3, 1, 1, 1, 1], [1, 1, 1, 2, 2, 4], [1, 1, 1, 4, 2, 2],
    [1, 2, 1, 1, 2, 4], [1, 2, 1, 4, 2, 1], [1, 4, 1, 1, 2, 2], [1, 4, 1, 2, 2, 1], [1, 1, 2, 2, 1, 4],
    [1, 1, 2, 4, 1, 2], [1, 2, 2, 1, 1, 4], [1, 2, 2, 4, 1, 1], [1, 4, 2, 1, 1, 2], [1, 4, 2, 2, 1, 1],
    [2, 4, 1, 2, 1, 1], [2, 2, 1, 1, 1, 4], [4, 1, 3, 1, 1, 1], [2, 4, 1, 1, 1, 2], [1, 3, 4, 1, 1, 1],
    [1, 1, 1, 2, 4, 2], [1, 2, 1, 1, 4, 2], [1, 2, 1, 2, 4, 1], [1, 1, 4, 2, 1, 2], [1, 2, 4, 1, 1, 2],
    [1, 2, 4, 2, 1, 1], [4, 1, 1, 2, 1, 2], [4, 2, 1, 1, 1, 2], [4, 2, 1, 2, 1, 1], [2, 1, 2, 1, 4, 1],
    [2, 1, 4, 1, 2, 1], [4, 1, 2, 1, 2, 1], [1, 1, 1, 1, 4, 3], [1, 1, 1, 3, 4, 1], [1, 3, 1, 1, 4, 1],
    [1, 1, 4, 1, 1, 3], [1, 1, 4, 3, 1, 1], [4, 1, 1, 1, 1, 3], [4, 1, 1, 3, 1, 1], [1, 1, 3, 1, 4, 1],
    [1, 1, 4, 1, 3, 1], [3, 1, 1, 1, 4, 1], [4, 1, 1, 1, 3, 1], [2, 1, 1, 4, 1, 2], [2, 1, 1, 2, 1, 4],
    [2, 1, 1, 2, 3, 2],
];
const CODE128_STOP: [u8; 7] = [2, 3, 3, 1, 1, 1, 2];
const START_A: usize = 103;
const START_B: usize = 104;
const START_C: usize = 105;
const CODE_C: usize = 99;
const CODE_B: usize = 100;
const CODE_A: usize = 101;

fn push_widths(out: &mut Vec<bool>, widths: &[u8], start_bar: bool) {
    let mut bar = start_bar;
    for w in widths {
        out.extend(std::iter::repeat_n(bar, *w as usize));
        bar = !bar;
    }
}

/// Code 128 symbol values (start, data, checksum, without stop) for `text`.
pub fn code128_values(text: &str) -> Result<Vec<usize>> {
    let b = text.as_bytes();
    if b.is_empty() {
        return Err(df_err("Code 128 contents must not be empty"));
    }
    if let Some(c) = text.chars().find(|c| (*c as u32) > 127) {
        return Err(df_err(format!("Code 128 cannot encode the character '{}'", c)));
    }
    let mut vals: Vec<usize> = Vec::new();
    let mut set: Option<char> = None;
    let mut i = 0;
    let n = b.len();
    let switch = |vals: &mut Vec<usize>, set: &mut Option<char>, to: char| {
        if *set == Some(to) {
            return;
        }
        vals.push(match (set.is_none(), to) {
            (true, 'A') => START_A,
            (true, 'B') => START_B,
            (true, _) => START_C,
            (false, 'A') => CODE_A,
            (false, 'B') => CODE_B,
            (false, _) => CODE_C,
        });
        *set = Some(to);
    };
    while i < n {
        let d = b[i..].iter().take_while(|c| c.is_ascii_digit()).count();
        let use_c = (d >= 4 && (i == 0 || i + d == n || d >= 6)) || (d >= 2 && i == 0 && d == n);
        if use_c {
            // an odd run keeps its last digit for A/B
            let take = d - d % 2;
            switch(&mut vals, &mut set, 'C');
            for k in (0..take).step_by(2) {
                vals.push(((b[i + k] - b'0') * 10 + (b[i + k + 1] - b'0')) as usize);
            }
            i += take;
            continue;
        }
        let c = b[i];
        let need = if c < 32 { 'A' } else if set == Some('A') && c < 96 { 'A' } else { 'B' };
        switch(&mut vals, &mut set, need);
        vals.push(if need == 'A' { if c < 32 { c as usize + 64 } else { c as usize - 32 } } else { c as usize - 32 });
        i += 1;
    }
    let check = vals.iter().enumerate().map(|(k, v)| if k == 0 { *v } else { k * v }).sum::<usize>() % 103;
    vals.push(check);
    Ok(vals)
}

/// Encode Code 128 modules.
pub fn encode_code128(text: &str) -> Result<Vec<bool>> {
    let mut out = Vec::new();
    for v in code128_values(text)? {
        push_widths(&mut out, &CODE128[v], true);
    }
    push_widths(&mut out, &CODE128_STOP, true);
    Ok(out)
}

const CODE39_ALPHABET: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ-. $/+%";
const CODE39: [u16; 43] = [
    0x034, 0x121, 0x061, 0x160, 0x031, 0x130, 0x070, 0x025, 0x124, 0x064, 0x109, 0x049, 0x148, 0x019, 0x118, 0x058, 0x00D, 0x10C, 0x04C, 0x01C, 0x103, 0x043,
    0x142, 0x013, 0x112, 0x052, 0x007, 0x106, 0x046, 0x016, 0x181, 0x0C1, 0x1C0, 0x091, 0x190, 0x0D0, 0x085, 0x184, 0x0C4, 0x0A8, 0x0A2, 0x08A, 0x02A,
];
const CODE39_STAR: u16 = 0x094;

/// Encode Code 39 modules (wide = 2 narrow, like ZXing), with `*` delimiters.
pub fn encode_code39(text: &str) -> Result<Vec<bool>> {
    let mut codes = vec![CODE39_STAR];
    for c in text.bytes() {
        let i = CODE39_ALPHABET
            .iter()
            .position(|a| *a == c.to_ascii_uppercase())
            .ok_or_else(|| df_err(format!("Code 39 cannot encode the character '{}'", c as char)))?;
        codes.push(CODE39[i]);
    }
    codes.push(CODE39_STAR);
    let mut out = Vec::new();
    for (k, code) in codes.iter().enumerate() {
        if k > 0 {
            out.push(false);
        }
        let widths: Vec<u8> = (0..9).map(|i| if code & (1 << (8 - i)) != 0 { 2 } else { 1 }).collect();
        push_widths(&mut out, &widths, true);
    }
    Ok(out)
}

const L_PATTERNS: [[u8; 4]; 10] = [[3, 2, 1, 1], [2, 2, 2, 1], [2, 1, 2, 2], [1, 4, 1, 1], [1, 1, 3, 2], [1, 2, 3, 1], [1, 1, 1, 4], [1, 3, 1, 2], [1, 2, 1, 3], [3, 1, 1, 2]];
const FIRST_DIGIT: [u8; 10] = [0x00, 0x0B, 0x0D, 0x0E, 0x13, 0x19, 0x1C, 0x15, 0x16, 0x1A];

/// EAN/UPC check digit of the leading digits.
pub fn ean_check_digit(digits: &[u8]) -> u8 {
    let sum: u32 = digits.iter().rev().enumerate().map(|(i, d)| *d as u32 * if i % 2 == 0 { 3 } else { 1 }).sum();
    ((10 - sum % 10) % 10) as u8
}

fn ean_digits(text: &str, len: usize, name: &str) -> Result<Vec<u8>> {
    if !text.bytes().all(|b| b.is_ascii_digit()) || (text.len() != len && text.len() != len - 1) {
        return Err(df_err(format!("{} requires {} or {} digits", name, len - 1, len)));
    }
    let mut d: Vec<u8> = text.bytes().map(|b| b - b'0').collect();
    let check = ean_check_digit(&d[..len - 1]);
    if d.len() == len {
        if d[len - 1] != check {
            return Err(df_err(format!("{} contents have an invalid check digit", name)));
        }
    } else {
        d.push(check);
    }
    Ok(d)
}

/// Encode EAN-13 modules (12 digits get their check digit appended).
pub fn encode_ean13(text: &str) -> Result<Vec<bool>> {
    let d = ean_digits(text, 13, "EAN-13")?;
    let parity = FIRST_DIGIT[d[0] as usize];
    let mut out = Vec::new();
    push_widths(&mut out, &[1, 1, 1], true);
    for (k, digit) in d[1..7].iter().enumerate() {
        let mut w = L_PATTERNS[*digit as usize];
        if parity & (1 << (5 - k)) != 0 {
            w.reverse();
        }
        push_widths(&mut out, &w, false);
    }
    push_widths(&mut out, &[1, 1, 1, 1, 1], false);
    for digit in &d[7..13] {
        push_widths(&mut out, &L_PATTERNS[*digit as usize], true);
    }
    push_widths(&mut out, &[1, 1, 1], true);
    Ok(out)
}

/// Encode UPC-A modules (11 or 12 digits).
pub fn encode_upca(text: &str) -> Result<Vec<bool>> {
    let d = ean_digits(text, 12, "UPC-A")?;
    let s: String = d.iter().map(|x| (b'0' + x) as char).collect();
    encode_ean13(&format!("0{}", s))
}

/// Run lengths of a pixel row starting at the first bar.
pub fn runs(row: &[bool]) -> Vec<usize> {
    let mut out = Vec::new();
    let Some(start) = row.iter().position(|b| *b) else { return out };
    let mut cur = true;
    let mut n = 0;
    for b in &row[start..] {
        if *b == cur {
            n += 1;
        } else {
            out.push(n);
            cur = *b;
            n = 1;
        }
    }
    if cur {
        out.push(n);
    }
    out
}

/// Mean absolute deviation (in modules) of `runs` from `pattern` scaled to the same width.
fn variance(runs: &[usize], pattern: &[u8]) -> f64 {
    let total: usize = runs.iter().sum();
    let modules: u32 = pattern.iter().map(|p| *p as u32).sum();
    let unit = total as f64 / modules as f64;
    runs.iter().zip(pattern).map(|(r, p)| (*r as f64 / unit - *p as f64).abs()).sum::<f64>() / runs.len() as f64
}

fn best_match(runs: &[usize], patterns: &[[u8; 6]]) -> Option<(usize, f64)> {
    patterns.iter().enumerate().map(|(i, p)| (i, variance(runs, p))).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).filter(|(_, v)| *v < 0.35)
}

/// Decode a Code 128 row.
pub fn decode_code128(row: &[bool]) -> Option<String> {
    let r = runs(row);
    let mut s = 0;
    while s + 6 <= r.len() {
        if let Some((v, _)) = best_match(&r[s..s + 6], &CODE128) {
            if (START_A..=START_C).contains(&v) {
                if let Some(t) = decode_code128_from(&r[s..]) {
                    return Some(t);
                }
            }
        }
        s += 2;
    }
    None
}

fn decode_code128_from(r: &[usize]) -> Option<String> {
    let mut vals = Vec::new();
    let mut p = 0;
    loop {
        if p + 7 <= r.len() && variance(&r[p..p + 7], &CODE128_STOP) < 0.35 && !vals.is_empty() {
            break;
        }
        if p + 6 > r.len() {
            return None;
        }
        let (v, _) = best_match(&r[p..p + 6], &CODE128)?;
        vals.push(v);
        p += 6;
    }
    if vals.len() < 2 {
        return None;
    }
    let check = vals.pop()?;
    let sum = vals.iter().enumerate().map(|(k, v)| if k == 0 { *v } else { k * v }).sum::<usize>() % 103;
    if sum != check {
        return None;
    }
    let mut set = match vals[0] {
        START_A => 'A',
        START_B => 'B',
        START_C => 'C',
        _ => return None,
    };
    let mut out = String::new();
    let mut shift = false;
    for &v in &vals[1..] {
        let cur = if shift { if set == 'A' { 'B' } else { 'A' } } else { set };
        shift = false;
        match cur {
            'C' => match v {
                0..=99 => out.push_str(&format!("{:02}", v)),
                CODE_B => set = 'B',
                CODE_A => set = 'A',
                _ => {}
            },
            'A' => match v {
                0..=63 => out.push((v as u8 + 32) as char),
                64..=95 => out.push((v as u8 - 64) as char),
                98 => shift = true,
                CODE_C => set = 'C',
                CODE_B => set = 'B',
                _ => {}
            },
            _ => match v {
                0..=95 => out.push((v as u8 + 32) as char),
                98 => shift = true,
                CODE_C => set = 'C',
                CODE_A => set = 'A',
                _ => {}
            },
        }
    }
    Some(out)
}

/// Decode a Code 39 row (without check digit).
pub fn decode_code39(row: &[bool]) -> Option<String> {
    let r = runs(row);
    let code_of = |w: &[usize]| -> Option<u16> {
        let mut sorted: Vec<usize> = w.to_vec();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        if (sorted[2] as f64) < sorted[3] as f64 * 1.5 {
            return None;
        }
        let thr = sorted[2];
        Some(w.iter().fold(0u16, |acc, x| (acc << 1) | (*x >= thr) as u16))
    };
    let mut s = 0;
    while s + 9 <= r.len() {
        if code_of(&r[s..s + 9]) == Some(CODE39_STAR) {
            let mut out = String::new();
            let mut p = s + 10;
            while p + 9 <= r.len() {
                let c = code_of(&r[p..p + 9])?;
                if c == CODE39_STAR {
                    return Some(out);
                }
                let i = CODE39.iter().position(|x| *x == c)?;
                out.push(CODE39_ALPHABET[i] as char);
                p += 10;
            }
            return None;
        }
        s += 2;
    }
    None
}

fn match_digit(r: &[usize], reversed_ok: bool) -> Option<(u8, bool)> {
    let mut best: Option<(u8, bool, f64)> = None;
    for (d, p) in L_PATTERNS.iter().enumerate() {
        let mut cands = vec![(*p, false)];
        if reversed_ok {
            let mut g = *p;
            g.reverse();
            cands.push((g, true));
        }
        for (pat, g) in cands {
            let v = variance(r, &pat);
            if best.is_none_or(|b| v < b.2) {
                best = Some((d as u8, g, v));
            }
        }
    }
    best.filter(|b| b.2 < 0.35).map(|b| (b.0, b.1))
}

/// Decode an EAN-13 row (13 digits).
pub fn decode_ean13(row: &[bool]) -> Option<String> {
    let r = runs(row);
    // 3 + 24 + 5 + 24 + 3 = 59 runs
    let mut s = 0;
    while s + 59 <= r.len() {
        let guard = &r[s..s + 3];
        if variance(guard, &[1, 1, 1]) < 0.35 {
            if let Some(t) = decode_ean_at(&r[s..s + 59]) {
                return Some(t);
            }
        }
        s += 2;
    }
    None
}

fn decode_ean_at(r: &[usize]) -> Option<String> {
    let mut digits = Vec::new();
    let mut parity = 0u8;
    for k in 0..6 {
        let (d, g) = match_digit(&r[3 + 4 * k..7 + 4 * k], true)?;
        digits.push(d);
        if g {
            parity |= 1 << (5 - k);
        }
    }
    if variance(&r[27..32], &[1, 1, 1, 1, 1]) >= 0.35 || variance(&r[56..59], &[1, 1, 1]) >= 0.35 {
        return None;
    }
    for k in 0..6 {
        let (d, g) = match_digit(&r[32 + 4 * k..36 + 4 * k], false)?;
        if g {
            return None;
        }
        digits.push(d);
    }
    let first = FIRST_DIGIT.iter().position(|p| *p == parity)? as u8;
    let mut all = vec![first];
    all.extend(digits);
    if ean_check_digit(&all[..12]) != all[12] {
        return None;
    }
    Some(all.iter().map(|d| (b'0' + d) as char).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widths_to_string(m: &[bool]) -> String {
        m.iter().map(|b| if *b { '1' } else { '0' }).collect()
    }

    #[test]
    fn code128_table_is_consistent() {
        let mut seen = std::collections::HashSet::new();
        for p in CODE128.iter() {
            assert_eq!(p.iter().map(|x| *x as u32).sum::<u32>(), 11, "{:?}", p);
            assert!(seen.insert(*p), "duplicate {:?}", p);
        }
        assert_eq!(CODE128_STOP.iter().map(|x| *x as u32).sum::<u32>(), 13);
    }

    #[test]
    fn code128_values_and_round_trip() {
        // "Wikipedia" (set B): checksum 101? computed: start B 104
        let v = code128_values("Wikipedia").unwrap();
        assert_eq!(v[0], START_B);
        assert_eq!(v.len(), 11);
        // pure digits use set C
        assert_eq!(code128_values("123456").unwrap()[..4], [START_C, 12, 34, 56]);
        let m = encode_code128("PJJ123C").unwrap();
        assert_eq!(&widths_to_string(&m)[..11], "11010010000"); // start B
        assert!(widths_to_string(&m).ends_with("1100011101011"));
        for t in ["Hello, World!", "123456789012", "ABC1234567890xyz", "a\tb", "00", "X"] {
            let m = encode_code128(t).unwrap();
            let mut row = vec![false; 10];
            for b in &m {
                row.extend([*b, *b, *b]);
            }
            row.extend(vec![false; 10]);
            assert_eq!(decode_code128(&row).as_deref(), Some(t));
        }
        assert!(encode_code128("é").is_err());
    }

    #[test]
    fn code39_round_trip() {
        for c in CODE39.iter().chain([&CODE39_STAR]) {
            assert_eq!(c.count_ones(), 3);
        }
        let m = encode_code39("CODE-39 $").unwrap();
        let mut row = vec![false; 7];
        row.extend(m.iter().flat_map(|b| [*b, *b]));
        row.extend([false; 7]);
        assert_eq!(decode_code39(&row).as_deref(), Some("CODE-39 $"));
        assert!(encode_code39("a~").is_err());
    }

    #[test]
    fn ean13_reference_and_round_trip() {
        // 4006381333931: well-known valid EAN-13
        assert_eq!(ean_check_digit(&[4, 0, 0, 6, 3, 8, 1, 3, 3, 3, 9, 3]), 1);
        let m = encode_ean13("400638133393").unwrap();
        assert_eq!(m.len(), 95);
        // first digit 4 -> parity LGLLGG; digit 0 in L = 0001101
        assert_eq!(&widths_to_string(&m)[..10], "1010001101");
        let mut row = vec![false; 9];
        row.extend(m.iter().flat_map(|b| [*b; 2]));
        row.extend([false; 9]);
        assert_eq!(decode_ean13(&row).as_deref(), Some("4006381333931"));
        assert!(encode_ean13("4006381333932").is_err());
        // UPC-A 036000291452
        let m = encode_upca("03600029145").unwrap();
        let mut row = vec![false; 9];
        row.extend(m);
        row.extend([false; 9]);
        assert_eq!(decode_ean13(&row).as_deref(), Some("0036000291452"));
    }
}
