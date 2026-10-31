//! Small helpers: hex encoding, constant-time comparison.

pub fn hex(data: &[u8]) -> String {
    const D: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(data.len() * 2);
    for b in data {
        s.push(D[(b >> 4) as usize] as char);
        s.push(D[(b & 15) as usize] as char);
    }
    s
}

/// Decodes hex, ignoring ASCII whitespace and ':' separators. Panics on invalid input
/// (intended for constants and tests).
pub fn unhex(s: &str) -> Vec<u8> {
    try_unhex(s).expect("invalid hex")
}

pub fn try_unhex(s: &str) -> Option<Vec<u8>> {
    let digits: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b':').collect();
    if digits.len() % 2 != 0 {
        return None;
    }
    let val = |c: u8| -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    };
    digits.chunks(2).map(|p| Some(val(p[0])? << 4 | val(p[1])?)).collect()
}

/// Constant-time equality for equal-length slices (length itself is not secret).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut d = 0u8;
    for (x, y) in a.iter().zip(b) {
        d |= x ^ y;
    }
    std::hint::black_box(d) == 0
}
