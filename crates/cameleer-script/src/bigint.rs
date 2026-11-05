//! Arbitrary precision signed integers (JavaScript `BigInt`, Java `BigInteger` literals).
//!
//! Sign-magnitude with little-endian base 2^32 limbs; the magnitude never has leading zero
//! limbs and zero is never negative.

use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct BigInt {
    pub neg: bool,
    pub mag: Vec<u32>,
}

fn trim(v: &mut Vec<u32>) {
    while v.last() == Some(&0) {
        v.pop();
    }
}

fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i].cmp(&b[i]);
        }
    }
    Ordering::Equal
}

fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut r = Vec::with_capacity(a.len().max(b.len()) + 1);
    let mut carry = 0u64;
    for i in 0..a.len().max(b.len()) {
        let s = *a.get(i).unwrap_or(&0) as u64 + *b.get(i).unwrap_or(&0) as u64 + carry;
        r.push(s as u32);
        carry = s >> 32;
    }
    if carry > 0 {
        r.push(carry as u32);
    }
    r
}

/// a - b with a >= b.
fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut r = Vec::with_capacity(a.len());
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let mut d = a[i] as i64 - *b.get(i).unwrap_or(&0) as i64 - borrow;
        if d < 0 {
            d += 1 << 32;
            borrow = 1;
        } else {
            borrow = 0;
        }
        r.push(d as u32);
    }
    trim(&mut r);
    r
}

fn mul_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
    if a.is_empty() || b.is_empty() {
        return vec![];
    }
    let mut r = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &y) in b.iter().enumerate() {
            let t = r[i + j] as u64 + x as u64 * y as u64 + carry;
            r[i + j] = t as u32;
            carry = t >> 32;
        }
        let mut k = i + b.len();
        while carry > 0 {
            let t = r[k] as u64 + carry;
            r[k] = t as u32;
            carry = t >> 32;
            k += 1;
        }
    }
    trim(&mut r);
    r
}

fn divrem_small(a: &[u32], d: u32) -> (Vec<u32>, u32) {
    let mut q = vec![0u32; a.len()];
    let mut rem = 0u64;
    for i in (0..a.len()).rev() {
        let cur = (rem << 32) | a[i] as u64;
        q[i] = (cur / d as u64) as u32;
        rem = cur % d as u64;
    }
    trim(&mut q);
    (q, rem as u32)
}

fn bit_len(a: &[u32]) -> usize {
    match a.last() {
        None => 0,
        Some(&top) => (a.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
    }
}

fn shl_mag(a: &[u32], n: usize) -> Vec<u32> {
    if a.is_empty() {
        return vec![];
    }
    let (w, b) = (n / 32, n % 32);
    let mut r = vec![0u32; w];
    if b == 0 {
        r.extend_from_slice(a);
    } else {
        let mut carry = 0u32;
        for &x in a {
            r.push((x << b) | carry);
            carry = x >> (32 - b);
        }
        if carry > 0 {
            r.push(carry);
        }
    }
    trim(&mut r);
    r
}

fn shr_mag(a: &[u32], n: usize) -> Vec<u32> {
    let (w, b) = (n / 32, n % 32);
    if w >= a.len() {
        return vec![];
    }
    let s = &a[w..];
    let mut r = Vec::with_capacity(s.len());
    for i in 0..s.len() {
        let lo = s[i] >> b;
        let hi = if b > 0 && i + 1 < s.len() { s[i + 1] << (32 - b) } else { 0 };
        r.push(lo | hi);
    }
    trim(&mut r);
    r
}

/// Long division (shift-subtract on bits; adequate for script sized numbers).
fn divrem_mag(a: &[u32], b: &[u32]) -> (Vec<u32>, Vec<u32>) {
    if cmp_mag(a, b) == Ordering::Less {
        return (vec![], a.to_vec());
    }
    if b.len() == 1 {
        let (q, r) = divrem_small(a, b[0]);
        return (q, if r == 0 { vec![] } else { vec![r] });
    }
    let mut q = vec![0u32; a.len()];
    let mut r: Vec<u32> = vec![];
    for i in (0..bit_len(a)).rev() {
        r = shl_mag(&r, 1);
        if (a[i / 32] >> (i % 32)) & 1 == 1 {
            if r.is_empty() {
                r.push(1);
            } else {
                r[0] |= 1;
            }
        }
        if cmp_mag(&r, b) != Ordering::Less {
            r = sub_mag(&r, b);
            q[i / 32] |= 1 << (i % 32);
        }
    }
    trim(&mut q);
    (q, r)
}

impl BigInt {
    pub fn zero() -> BigInt {
        BigInt::default()
    }
    fn make(neg: bool, mut mag: Vec<u32>) -> BigInt {
        trim(&mut mag);
        let neg = neg && !mag.is_empty();
        BigInt { neg, mag }
    }
    pub fn from_i64(v: i64) -> BigInt {
        let u = v.unsigned_abs();
        BigInt::make(v < 0, vec![u as u32, (u >> 32) as u32])
    }
    pub fn from_u128(v: u128, neg: bool) -> BigInt {
        BigInt::make(neg, vec![v as u32, (v >> 32) as u32, (v >> 64) as u32, (v >> 96) as u32])
    }
    /// From an integral float (None for NaN / infinite / fractional values).
    pub fn from_f64(f: f64) -> Option<BigInt> {
        if !f.is_finite() || f.fract() != 0.0 {
            return None;
        }
        let neg = f < 0.0;
        let mut a = f.abs();
        let mut limbs = vec![];
        while a >= 1.0 {
            let r = a % 4294967296.0;
            limbs.push(r as u32);
            a = ((a - r) / 4294967296.0).floor();
        }
        Some(BigInt::make(neg, limbs))
    }
    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }
    pub fn to_f64(&self) -> f64 {
        let mut f = 0.0;
        for &l in self.mag.iter().rev() {
            f = f * 4294967296.0 + l as f64;
        }
        if self.neg {
            -f
        } else {
            f
        }
    }
    /// The value as i64 when it fits.
    pub fn to_i64(&self) -> Option<i64> {
        if self.mag.len() > 2 {
            return None;
        }
        let u = *self.mag.first().unwrap_or(&0) as u64 | (*self.mag.get(1).unwrap_or(&0) as u64) << 32;
        if self.neg {
            if u <= i64::MAX as u64 + 1 {
                return Some((u as i128).wrapping_neg() as i64);
            }
            None
        } else if u <= i64::MAX as u64 {
            Some(u as i64)
        } else {
            None
        }
    }
    /// Low 64 bits in two's complement (`BigInt.asIntN(64, x)`).
    pub fn low_u64(&self) -> u64 {
        let u = *self.mag.first().unwrap_or(&0) as u64 | (*self.mag.get(1).unwrap_or(&0) as u64) << 32;
        if self.neg {
            u.wrapping_neg()
        } else {
            u
        }
    }
    pub fn parse(s: &str) -> Option<BigInt> {
        let s = s.trim();
        let (neg, s) = match s.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, s.strip_prefix('+').unwrap_or(s)),
        };
        let (radix, digits) = if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            (16, r)
        } else if let Some(r) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
            (2, r)
        } else if let Some(r) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
            (8, r)
        } else {
            (10, s)
        };
        BigInt::parse_radix(digits, radix).map(|b| BigInt::make(neg, b.mag))
    }
    pub fn parse_radix(digits: &str, radix: u32) -> Option<BigInt> {
        if digits.is_empty() {
            return if radix == 10 { Some(BigInt::zero()) } else { None };
        }
        let mut mag: Vec<u32> = vec![];
        for c in digits.chars() {
            if c == '_' {
                continue;
            }
            let d = c.to_digit(radix)?;
            // mag = mag * radix + d
            let mut carry = d as u64;
            for l in mag.iter_mut() {
                let t = *l as u64 * radix as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
            }
            if carry > 0 {
                mag.push(carry as u32);
            }
        }
        Some(BigInt::make(false, mag))
    }
    pub fn to_string_radix(&self, radix: u32) -> String {
        if self.mag.is_empty() {
            return "0".into();
        }
        let mut digits = vec![];
        let mut cur = self.mag.clone();
        while !cur.is_empty() {
            let (q, r) = divrem_small(&cur, radix);
            digits.push(std::char::from_digit(r, radix).unwrap());
            cur = q;
        }
        if self.neg {
            digits.push('-');
        }
        digits.iter().rev().collect()
    }
    pub fn neg(&self) -> BigInt {
        BigInt::make(!self.neg, self.mag.clone())
    }
    pub fn abs(&self) -> BigInt {
        BigInt::make(false, self.mag.clone())
    }
    pub fn add(&self, o: &BigInt) -> BigInt {
        if self.neg == o.neg {
            return BigInt::make(self.neg, add_mag(&self.mag, &o.mag));
        }
        match cmp_mag(&self.mag, &o.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => BigInt::make(self.neg, sub_mag(&self.mag, &o.mag)),
            Ordering::Less => BigInt::make(o.neg, sub_mag(&o.mag, &self.mag)),
        }
    }
    pub fn sub(&self, o: &BigInt) -> BigInt {
        self.add(&o.neg())
    }
    pub fn mul(&self, o: &BigInt) -> BigInt {
        BigInt::make(self.neg != o.neg, mul_mag(&self.mag, &o.mag))
    }
    /// Truncating division and remainder (sign of the remainder follows the dividend); None on
    /// division by zero.
    pub fn divrem(&self, o: &BigInt) -> Option<(BigInt, BigInt)> {
        if o.is_zero() {
            return None;
        }
        let (q, r) = divrem_mag(&self.mag, &o.mag);
        Some((BigInt::make(self.neg != o.neg, q), BigInt::make(self.neg, r)))
    }

    pub fn pow(&self, mut e: u64) -> BigInt {
        let mut base = self.clone();
        let mut r = BigInt::from_i64(1);
        while e > 0 {
            if e & 1 == 1 {
                r = r.mul(&base);
            }
            e >>= 1;
            if e > 0 {
                base = base.mul(&base);
            }
        }
        r
    }
    pub fn cmp(&self, o: &BigInt) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (false, false) => cmp_mag(&self.mag, &o.mag),
            (true, true) => cmp_mag(&o.mag, &self.mag),
        }
    }
    pub fn shl(&self, n: i64) -> BigInt {
        if n < 0 {
            return self.shr(-n);
        }
        BigInt::make(self.neg, shl_mag(&self.mag, n as usize))
    }
    /// Arithmetic (flooring) right shift.
    pub fn shr(&self, n: i64) -> BigInt {
        if n < 0 {
            return self.shl(-n);
        }
        if !self.neg {
            return BigInt::make(false, shr_mag(&self.mag, n as usize));
        }
        // floor(-m / 2^n) = -ceil(m / 2^n)
        let q = shr_mag(&self.mag, n as usize);
        let back = shl_mag(&q, n as usize);
        let q = if cmp_mag(&back, &self.mag) != Ordering::Equal { add_mag(&q, &[1]) } else { q };
        BigInt::make(true, q)
    }
    /// Two's complement limbs of `n` words.
    fn twos(&self, n: usize) -> Vec<u32> {
        let mut v = self.mag.clone();
        v.resize(n, 0);
        if self.neg {
            for l in v.iter_mut() {
                *l = !*l;
            }
            let mut carry = 1u64;
            for l in v.iter_mut() {
                let t = *l as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
            }
        }
        v
    }
    fn from_twos(mut v: Vec<u32>) -> BigInt {
        let neg = v.last().is_some_and(|x| x >> 31 == 1);
        if neg {
            for l in v.iter_mut() {
                *l = !*l;
            }
            let mut carry = 1u64;
            for l in v.iter_mut() {
                let t = *l as u64 + carry;
                *l = t as u32;
                carry = t >> 32;
            }
        }
        BigInt::make(neg, v)
    }
    pub fn bitop(&self, o: &BigInt, op: char) -> BigInt {
        let n = self.mag.len().max(o.mag.len()) + 1;
        let (a, b) = (self.twos(n), o.twos(n));
        let r = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| match op {
                '&' => x & y,
                '|' => x | y,
                _ => x ^ y,
            })
            .collect();
        BigInt::from_twos(r)
    }
    pub fn not(&self) -> BigInt {
        // ~x = -x - 1
        self.neg().sub(&BigInt::from_i64(1))
    }
    /// `BigInt.asUintN(bits, x)`.
    pub fn as_uint_n(&self, bits: usize) -> BigInt {
        let n = bits.div_ceil(32) + 1;
        let mut v = self.twos(n.max(self.mag.len() + 1));
        v.truncate(bits.div_ceil(32));
        if bits % 32 != 0 {
            if let Some(l) = v.last_mut() {
                *l &= (1u32 << (bits % 32)) - 1;
            }
        }
        BigInt::make(false, v)
    }
    /// `BigInt.asIntN(bits, x)`.
    pub fn as_int_n(&self, bits: usize) -> BigInt {
        if bits == 0 {
            return BigInt::zero();
        }
        let u = self.as_uint_n(bits);
        let top = BigInt::from_i64(1).shl(bits as i64 - 1);
        if u.cmp(&top) != Ordering::Less {
            u.sub(&BigInt::from_i64(1).shl(bits as i64))
        } else {
            u
        }
    }
}

impl std::fmt::Display for BigInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_string_radix(10))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic() {
        let a = BigInt::parse("123456789012345678901234567890").unwrap();
        let b = BigInt::parse("987654321").unwrap();
        assert_eq!(a.mul(&b).to_string(), "121932631124828532112482853211126352690");
        let (q, r) = a.divrem(&b).unwrap();
        assert_eq!(q.to_string(), "124999998873437499901");
        assert_eq!(r.to_string(), "574845669");
        assert_eq!(BigInt::from_i64(2).pow(100).to_string(), "1267650600228229401496703205376");
        assert_eq!(BigInt::from_i64(-7).shr(1).to_string(), "-4");
        assert_eq!(BigInt::from_i64(-6).bitop(&BigInt::from_i64(3), '&').to_string(), "2");
        assert_eq!(BigInt::from_i64(255).as_int_n(8).to_string(), "-1");
        assert_eq!(BigInt::from_i64(-1).as_uint_n(8).to_string(), "255");
        assert_eq!(BigInt::parse("-0xff").unwrap().to_string(), "-255");
        assert_eq!(BigInt::from_i64(i64::MIN).to_i64(), Some(i64::MIN));
    }
}
