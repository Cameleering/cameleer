//! Arbitrary-precision `xs:decimal` values (sign, decimal digits, scale).
//!
//! Addition, subtraction and multiplication are exact; division is computed to
//! [`DIV_SCALE`] fractional digits (more when the operands have more), then trailing zeros
//! are removed, so `1 div 4` is exactly `0.25` and `1 div 3` is
//! `0.333333333333333333333333`.

use std::cmp::Ordering;
use std::fmt;

/// Minimum number of fractional digits kept by a non-terminating division.
pub const DIV_SCALE: u32 = 24;

/// An arbitrary-precision decimal number.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Decimal {
    neg: bool,
    /// Magnitude digits, most significant first, no leading zeros (empty = zero).
    digits: Vec<u8>,
    /// Number of fractional digits in `digits` (no trailing fractional zeros).
    scale: u32,
}

// ------------------------------------------------------------------ magnitudes

fn trim_leading(mut v: Vec<u8>) -> Vec<u8> {
    let nz = v.iter().position(|&d| d != 0).unwrap_or(v.len());
    v.drain(..nz);
    v
}

fn mag_cmp(a: &[u8], b: &[u8]) -> Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

fn mag_add(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
    let (mut i, mut j, mut carry) = (a.len(), b.len(), 0u8);
    while i > 0 || j > 0 || carry > 0 {
        let x = if i > 0 {
            i -= 1;
            a[i]
        } else {
            0
        };
        let y = if j > 0 {
            j -= 1;
            b[j]
        } else {
            0
        };
        let s = x + y + carry;
        out.push(s % 10);
        carry = s / 10;
    }
    out.reverse();
    trim_leading(out)
}

/// `a - b` with `a >= b`.
fn mag_sub(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(a.len());
    let (mut i, mut j, mut borrow) = (a.len(), b.len(), 0i8);
    while i > 0 {
        i -= 1;
        let y = if j > 0 {
            j -= 1;
            b[j] as i8
        } else {
            0
        };
        let mut d = a[i] as i8 - y - borrow;
        if d < 0 {
            d += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        out.push(d as u8);
    }
    out.reverse();
    trim_leading(out)
}

fn mag_mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    if a.is_empty() || b.is_empty() {
        return Vec::new();
    }
    let mut acc = vec![0u32; a.len() + b.len()];
    for (i, &x) in a.iter().enumerate().rev() {
        for (j, &y) in b.iter().enumerate().rev() {
            acc[i + j + 1] += x as u32 * y as u32;
        }
    }
    for k in (1..acc.len()).rev() {
        let c = acc[k] / 10;
        acc[k] %= 10;
        acc[k - 1] += c;
    }
    trim_leading(acc.into_iter().map(|d| d as u8).collect())
}

/// Long division: `(a / b, a % b)`, `b` non-zero.
fn mag_divmod(a: &[u8], b: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut q = Vec::with_capacity(a.len());
    let mut r: Vec<u8> = Vec::new();
    for &d in a {
        r.push(d);
        r = trim_leading(r);
        let mut c = 0u8;
        while mag_cmp(&r, b) != Ordering::Less {
            r = mag_sub(&r, b);
            c += 1;
        }
        q.push(c);
    }
    (trim_leading(q), r)
}

fn shift(mut v: Vec<u8>, k: u32) -> Vec<u8> {
    if !v.is_empty() {
        v.extend(std::iter::repeat_n(0, k as usize));
    }
    v
}

impl Decimal {
    fn new(neg: bool, digits: Vec<u8>, scale: u32) -> Decimal {
        let mut d = Decimal { neg, digits: trim_leading(digits), scale };
        d.normalize();
        d
    }

    fn normalize(&mut self) {
        while self.scale > 0 && self.digits.last() == Some(&0) {
            self.digits.pop();
            self.scale -= 1;
        }
        if self.digits.is_empty() {
            self.neg = false;
            self.scale = 0;
        }
    }

    /// Zero.
    pub fn zero() -> Decimal {
        Decimal::default()
    }

    pub fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    pub fn is_negative(&self) -> bool {
        self.neg
    }

    /// Whether the value has no fractional part.
    pub fn is_integer(&self) -> bool {
        self.scale == 0
    }

    pub fn from_i64(i: i64) -> Decimal {
        let neg = i < 0;
        let digits = i.unsigned_abs().to_string().bytes().map(|b| b - b'0').collect();
        Decimal::new(neg, digits, 0)
    }

    /// The shortest decimal that round-trips to `f` (`None` for NaN / infinities).
    pub fn from_f64(f: f64) -> Option<Decimal> {
        if !f.is_finite() {
            return None;
        }
        Decimal::parse(&format!("{}", f))
    }

    /// Parse the `xs:decimal` lexical form (`-12.50`, `+.5`, `3.`). Exponents are accepted
    /// too (for casting doubles' canonical forms).
    pub fn parse(s: &str) -> Option<Decimal> {
        let t = s.trim();
        let (neg, body) = match t.as_bytes().first() {
            Some(b'-') => (true, &t[1..]),
            Some(b'+') => (false, &t[1..]),
            _ => (false, t),
        };
        let (mant, exp) = match body.find(['e', 'E']) {
            Some(i) => (&body[..i], body[i + 1..].parse::<i32>().ok()?),
            None => (body, 0),
        };
        let (ip, fp) = mant.split_once('.').unwrap_or((mant, ""));
        if (ip.is_empty() && fp.is_empty()) || !ip.bytes().all(|b| b.is_ascii_digit()) || !fp.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let mut digits: Vec<u8> = ip.bytes().chain(fp.bytes()).map(|b| b - b'0').collect();
        let mut scale = fp.len() as i64 - exp as i64;
        if scale < 0 {
            digits = shift(digits, (-scale) as u32);
            scale = 0;
        }
        Some(Decimal::new(neg, digits, scale as u32))
    }

    pub fn to_f64(&self) -> f64 {
        self.to_string().parse().unwrap_or(f64::NAN)
    }

    /// The integer value when it fits an `i64` and has no fractional part.
    pub fn to_i64(&self) -> Option<i64> {
        if self.scale != 0 {
            return None;
        }
        self.to_string().parse().ok()
    }

    fn aligned(&self, other: &Decimal) -> (Vec<u8>, Vec<u8>, u32) {
        let s = self.scale.max(other.scale);
        (shift(self.digits.clone(), s - self.scale), shift(other.digits.clone(), s - other.scale), s)
    }

    pub fn neg(&self) -> Decimal {
        let mut d = self.clone();
        if !d.is_zero() {
            d.neg = !d.neg;
        }
        d
    }

    pub fn abs(&self) -> Decimal {
        Decimal { neg: false, ..self.clone() }
    }

    pub fn add(&self, o: &Decimal) -> Decimal {
        let (a, b, s) = self.aligned(o);
        if self.neg == o.neg {
            return Decimal::new(self.neg, mag_add(&a, &b), s);
        }
        match mag_cmp(&a, &b) {
            Ordering::Equal => Decimal::zero(),
            Ordering::Greater => Decimal::new(self.neg, mag_sub(&a, &b), s),
            Ordering::Less => Decimal::new(o.neg, mag_sub(&b, &a), s),
        }
    }

    pub fn sub(&self, o: &Decimal) -> Decimal {
        self.add(&o.neg())
    }

    pub fn mul(&self, o: &Decimal) -> Decimal {
        Decimal::new(self.neg != o.neg, mag_mul(&self.digits, &o.digits), self.scale + o.scale)
    }

    /// Division (`None` when dividing by zero).
    pub fn div(&self, o: &Decimal) -> Option<Decimal> {
        if o.is_zero() {
            return None;
        }
        let target = DIV_SCALE.max(self.scale).max(o.scale);
        // (a / 10^sa) / (b / 10^sb) = (a * 10^(target + sb - sa)) / b / 10^target
        let k = target as i64 + o.scale as i64 - self.scale as i64;
        let (num, den) = if k >= 0 { (shift(self.digits.clone(), k as u32), o.digits.clone()) } else { (self.digits.clone(), shift(o.digits.clone(), (-k) as u32)) };
        let (q, _) = mag_divmod(&num, &den);
        Some(Decimal::new(self.neg != o.neg, q, target))
    }

    /// Integer division truncating toward zero (`None` when dividing by zero).
    pub fn idiv(&self, o: &Decimal) -> Option<Decimal> {
        if o.is_zero() {
            return None;
        }
        let (a, b, _) = self.aligned(o);
        let (q, _) = mag_divmod(&a, &b);
        Some(Decimal::new(self.neg != o.neg, q, 0))
    }

    /// Remainder with the sign of the dividend (`None` when dividing by zero).
    pub fn rem(&self, o: &Decimal) -> Option<Decimal> {
        if o.is_zero() {
            return None;
        }
        let (a, b, s) = self.aligned(o);
        let (_, r) = mag_divmod(&a, &b);
        Some(Decimal::new(self.neg, r, s))
    }

    /// Truncate to `prec` fractional digits (negative = tens, hundreds...).
    fn trunc_to(&self, prec: i64) -> Decimal {
        if prec >= self.scale as i64 {
            return self.clone();
        }
        let drop = (self.scale as i64 - prec) as usize;
        let mut digits = self.digits.clone();
        let keep = digits.len().saturating_sub(drop);
        digits.truncate(keep);
        if prec >= 0 {
            Decimal::new(self.neg, digits, prec as u32)
        } else {
            Decimal::new(self.neg, shift(digits, (-prec) as u32), 0)
        }
    }

    fn unit(prec: i64) -> Decimal {
        if prec >= 0 {
            Decimal::new(false, vec![1], prec as u32)
        } else {
            Decimal::new(false, shift(vec![1], (-prec) as u32), 0)
        }
    }

    pub fn trunc(&self) -> Decimal {
        self.trunc_to(0)
    }

    pub fn floor(&self) -> Decimal {
        let t = self.trunc_to(0);
        if self.neg && t != *self {
            t.sub(&Decimal::from_i64(1))
        } else {
            t
        }
    }

    pub fn ceil(&self) -> Decimal {
        let t = self.trunc_to(0);
        if !self.neg && t != *self {
            t.add(&Decimal::from_i64(1))
        } else {
            t
        }
    }

    /// `fn:round` (half toward positive infinity) at `prec` fractional digits.
    pub fn round(&self, prec: i64) -> Decimal {
        let half = Decimal::unit(prec).mul(&Decimal::parse("0.5").unwrap());
        let shifted = self.add(&half);
        // floor at precision
        let t = shifted.trunc_to(prec);
        if shifted.neg && t != shifted {
            t.sub(&Decimal::unit(prec))
        } else {
            t
        }
    }

    /// `fn:round-half-to-even` at `prec` fractional digits.
    pub fn round_half_even(&self, prec: i64) -> Decimal {
        let t = self.trunc_to(prec);
        let rest = self.sub(&t).abs();
        let half = Decimal::unit(prec).mul(&Decimal::parse("0.5").unwrap());
        let away = match rest.cmp(&half) {
            Ordering::Less => false,
            Ordering::Greater => true,
            Ordering::Equal => {
                // is the kept last digit odd?
                let k = t.abs().mul(&Decimal::unit(-prec));
                k.digits.last().is_some_and(|d| d % 2 == 1)
            }
        };
        if away {
            if self.neg {
                t.sub(&Decimal::unit(prec))
            } else {
                t.add(&Decimal::unit(prec))
            }
        } else {
            t
        }
    }
}

impl Ord for Decimal {
    fn cmp(&self, o: &Decimal) -> Ordering {
        match (self.neg, o.neg) {
            (false, true) => Ordering::Greater,
            (true, false) => Ordering::Less,
            (neg, _) => {
                let (a, b, _) = self.aligned(o);
                let c = mag_cmp(&a, &b);
                if neg {
                    c.reverse()
                } else {
                    c
                }
            }
        }
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, o: &Decimal) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.digits.is_empty() {
            return f.write_str("0");
        }
        let mut s = String::new();
        if self.neg {
            s.push('-');
        }
        let n = self.digits.len();
        let sc = self.scale as usize;
        if sc >= n {
            s.push_str("0.");
            s.extend(std::iter::repeat_n('0', sc - n));
            s.extend(self.digits.iter().map(|d| (b'0' + d) as char));
        } else {
            s.extend(self.digits[..n - sc].iter().map(|d| (b'0' + d) as char));
            if sc > 0 {
                s.push('.');
                s.extend(self.digits[n - sc..].iter().map(|d| (b'0' + d) as char));
            }
        }
        f.write_str(&s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::parse(s).unwrap()
    }

    #[test]
    fn arithmetic_is_exact() {
        assert_eq!(d("0.1").add(&d("0.2")).to_string(), "0.3");
        assert_eq!(d("1.10").to_string(), "1.1");
        assert_eq!(d("-0.0").to_string(), "0");
        assert_eq!(d("12345678901234567890.123456789").mul(&d("10")).to_string(), "123456789012345678901.23456789");
        assert_eq!(d("1").div(&d("4")).unwrap().to_string(), "0.25");
        assert_eq!(d("1").div(&d("3")).unwrap().to_string(), "0.333333333333333333333333");
        assert_eq!(d("-7").idiv(&d("2")).unwrap().to_string(), "-3");
        assert_eq!(d("-7.5").rem(&d("2")).unwrap().to_string(), "-1.5");
        assert_eq!(d("5").sub(&d("7.25")).to_string(), "-2.25");
        assert!(d("2.5") > d("2.49"));
        assert!(d("-2.5") < d("-2.49"));
        assert_eq!(d("2.5").round(0).to_string(), "3");
        assert_eq!(d("-2.5").round(0).to_string(), "-2");
        assert_eq!(d("2.5").round_half_even(0).to_string(), "2");
        assert_eq!(d("3.5").round_half_even(0).to_string(), "4");
        assert_eq!(d("1.2345").round_half_even(2).to_string(), "1.23");
        assert_eq!(d("1.235").round_half_even(2).to_string(), "1.24");
        assert_eq!(d("1250").round_half_even(-2).to_string(), "1200");
        assert_eq!(d("1.305").round_half_even(2).to_string(), "1.3");
        assert_eq!(d("-1.5").floor().to_string(), "-2");
        assert_eq!(d("1.5").ceil().to_string(), "2");
        assert_eq!(d("1e3").to_string(), "1000");
        assert_eq!(Decimal::from_f64(0.1).unwrap().to_string(), "0.1");
    }
}
