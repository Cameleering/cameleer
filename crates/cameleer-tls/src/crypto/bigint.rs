//! Multi-precision arithmetic on little-endian `u64` limb slices: Montgomery
//! multiplication (CIOS), modular add/sub with branch-free final reduction,
//! modular exponentiation. Used by RSA (variable size) and the NIST curves
//! (fixed size).

#[inline(always)]
pub fn adc(a: u64, b: u64, carry: u64) -> (u64, u64) {
    let t = a as u128 + b as u128 + carry as u128;
    (t as u64, (t >> 64) as u64)
}

#[inline(always)]
pub fn sbb(a: u64, b: u64, borrow: u64) -> (u64, u64) {
    let t = (a as u128).wrapping_sub(b as u128 + borrow as u128);
    (t as u64, ((t >> 64) as u64) & 1)
}

#[inline(always)]
pub fn mac(acc: u64, a: u64, b: u64, carry: u64) -> (u64, u64) {
    let t = acc as u128 + (a as u128) * (b as u128) + carry as u128;
    (t as u64, (t >> 64) as u64)
}

/// -m0^{-1} mod 2^64 (m0 odd).
pub fn mont_m0inv(m0: u64) -> u64 {
    let mut inv: u64 = 1;
    for _ in 0..7 {
        inv = inv.wrapping_mul(2u64.wrapping_sub(m0.wrapping_mul(inv)));
    }
    inv.wrapping_neg()
}

/// r = a - b, returns borrow.
pub fn sub_into(r: &mut [u64], a: &[u64], b: &[u64]) -> u64 {
    let mut borrow = 0;
    for i in 0..r.len() {
        let (v, bo) = sbb(a[i], b[i], borrow);
        r[i] = v;
        borrow = bo;
    }
    borrow
}

/// Constant-time select: r = if flag {a} else {b}; flag must be 0 or 1.
pub fn select(r: &mut [u64], flag: u64, a: &[u64], b: &[u64]) {
    let mask = flag.wrapping_neg();
    for i in 0..r.len() {
        r[i] = (a[i] & mask) | (b[i] & !mask);
    }
}

/// r = (a + b) mod m, with a, b < m.
pub fn mod_add(r: &mut [u64], a: &[u64], b: &[u64], m: &[u64]) {
    let n = m.len();
    let mut carry = 0;
    for i in 0..n {
        let (v, c) = adc(a[i], b[i], carry);
        r[i] = v;
        carry = c;
    }
    let mut borrow = 0;
    for i in 0..n {
        let (_, bo) = sbb(r[i], m[i], borrow);
        borrow = bo;
    }
    // subtract m if the sum overflowed or is >= m
    let mask = (carry | (borrow ^ 1)).wrapping_neg();
    let mut borrow = 0;
    for i in 0..n {
        let (v, bo) = sbb(r[i], m[i] & mask, borrow);
        r[i] = v;
        borrow = bo;
    }
}

/// r = (a - b) mod m, with a, b < m.
pub fn mod_sub(r: &mut [u64], a: &[u64], b: &[u64], m: &[u64]) {
    let n = m.len();
    let borrow = sub_into(r, a, b);
    let mask = borrow.wrapping_neg();
    let mut carry = 0;
    for i in 0..n {
        let (v, c) = adc(r[i], m[i] & mask, carry);
        r[i] = v;
        carry = c;
    }
}

/// Montgomery product r = a * b * R^{-1} mod m (CIOS). `t` is scratch of length n+2.
pub fn mont_mul(r: &mut [u64], a: &[u64], b: &[u64], m: &[u64], m0inv: u64, t: &mut [u64]) {
    let n = m.len();
    for x in t.iter_mut() {
        *x = 0;
    }
    for i in 0..n {
        let mut c = 0;
        for j in 0..n {
            let (v, cc) = mac(t[j], a[j], b[i], c);
            t[j] = v;
            c = cc;
        }
        let (v, cc) = adc(t[n], c, 0);
        t[n] = v;
        t[n + 1] = cc;
        let q = t[0].wrapping_mul(m0inv);
        let (_, mut c) = mac(t[0], q, m[0], 0);
        for j in 1..n {
            let (v, cc) = mac(t[j], q, m[j], c);
            t[j - 1] = v;
            c = cc;
        }
        let (v, cc) = adc(t[n], c, 0);
        t[n - 1] = v;
        t[n] = t[n + 1] + cc;
    }
    // final conditional subtraction
    let hi = t[n];
    let mut borrow = 0;
    for i in 0..n {
        let (v, bo) = sbb(t[i], m[i], borrow);
        r[i] = v;
        borrow = bo;
    }
    let use_d = (hi != 0) as u64 | (borrow ^ 1);
    let mask = use_d.wrapping_neg();
    for i in 0..n {
        r[i] = (r[i] & mask) | (t[i] & !mask);
    }
}

/// R^2 mod m where R = 2^(64 n).
pub fn mont_r2(m: &[u64]) -> Vec<u64> {
    let n = m.len();
    let mut x = vec![0u64; n];
    x[0] = 1;
    // if m == 1 everything is 0; not a concern for our moduli
    let mut tmp = vec![0u64; n];
    for _ in 0..(128 * n) {
        mod_add(&mut tmp, &x, &x, m);
        x.copy_from_slice(&tmp);
    }
    x
}

/// Big-endian bytes to limbs (exactly `n` limbs, value must fit).
pub fn from_be_bytes(bytes: &[u8], n: usize) -> Option<Vec<u64>> {
    let bytes = {
        let mut i = 0;
        while i < bytes.len() && bytes[i] == 0 {
            i += 1;
        }
        &bytes[i..]
    };
    if bytes.len() > 8 * n {
        return None;
    }
    let mut out = vec![0u64; n];
    for (i, b) in bytes.iter().rev().enumerate() {
        out[i / 8] |= (*b as u64) << (8 * (i % 8));
    }
    Some(out)
}

/// Limbs to big-endian bytes, left-padded to `len` bytes (truncating high zero limbs).
pub fn to_be_bytes(limbs: &[u64], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for i in 0..len {
        let limb = i / 8;
        if limb < limbs.len() {
            out[len - 1 - i] = (limbs[limb] >> (8 * (i % 8))) as u8;
        }
    }
    out
}

/// a < b (not constant time; for public values).
pub fn lt(a: &[u64], b: &[u64]) -> bool {
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i] < b[i];
        }
    }
    false
}

pub fn is_zero(a: &[u64]) -> bool {
    a.iter().fold(0u64, |acc, x| acc | x) == 0
}

pub fn bit_len(a: &[u64]) -> usize {
    for i in (0..a.len()).rev() {
        if a[i] != 0 {
            return 64 * i + 64 - a[i].leading_zeros() as usize;
        }
    }
    0
}

/// Montgomery context for an odd modulus of arbitrary size.
#[derive(Clone)]
pub struct MontCtx {
    pub m: Vec<u64>,
    pub m0inv: u64,
    pub r2: Vec<u64>,
}

impl MontCtx {
    pub fn new(m: Vec<u64>) -> MontCtx {
        assert!(m[0] & 1 == 1, "modulus must be odd");
        let m0inv = mont_m0inv(m[0]);
        let r2 = mont_r2(&m);
        MontCtx { m, m0inv, r2 }
    }
    pub fn n(&self) -> usize {
        self.m.len()
    }
    pub fn mul(&self, a: &[u64], b: &[u64]) -> Vec<u64> {
        let n = self.n();
        let mut r = vec![0u64; n];
        let mut t = vec![0u64; n + 2];
        mont_mul(&mut r, a, b, &self.m, self.m0inv, &mut t);
        r
    }
    pub fn to_mont(&self, a: &[u64]) -> Vec<u64> {
        self.mul(a, &self.r2)
    }
    pub fn from_mont(&self, a: &[u64]) -> Vec<u64> {
        let mut one = vec![0u64; self.n()];
        one[0] = 1;
        self.mul(a, &one)
    }
    /// base^exp mod m. `base` must be < m (plain representation). When
    /// `secret_exp` is set, every bit performs a multiplication and selects the
    /// result branch-free (memory access pattern independent of the exponent).
    pub fn pow(&self, base: &[u64], exp: &[u64], secret_exp: bool) -> Vec<u64> {
        let n = self.n();
        let b = self.to_mont(base);
        let mut one = vec![0u64; n];
        one[0] = 1;
        let mut acc = self.to_mont(&one);
        let bits = if secret_exp { 64 * exp.len() } else { bit_len(exp) };
        let mut tmp = vec![0u64; n];
        for i in (0..bits).rev() {
            acc = self.mul(&acc, &acc);
            let bit = (exp[i / 64] >> (i % 64)) & 1;
            if secret_exp {
                let prod = self.mul(&acc, &b);
                select(&mut tmp, bit, &prod, &acc);
                acc.copy_from_slice(&tmp);
            } else if bit == 1 {
                acc = self.mul(&acc, &b);
            }
        }
        self.from_mont(&acc)
    }
}

/// Reduce an arbitrary-length big-endian integer modulo m (schoolbook bitwise
/// shift-subtract; used only for key-parsing sanity checks and CRT-free paths).
pub fn reduce_be(bytes: &[u8], m: &[u64]) -> Vec<u64> {
    let n = m.len();
    let mut r = vec![0u64; n];
    let mut tmp = vec![0u64; n];
    for byte in bytes {
        for bit in (0..8).rev() {
            // r = 2r + bit mod m
            mod_add(&mut tmp, &r, &r, m);
            r.copy_from_slice(&tmp);
            if (byte >> bit) & 1 == 1 {
                let mut one = vec![0u64; n];
                one[0] = 1;
                mod_add(&mut tmp, &r, &one, m);
                r.copy_from_slice(&tmp);
            }
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_modexp() {
        // 4^13 mod 497 = 445
        let ctx = MontCtx::new(vec![497]);
        assert_eq!(ctx.pow(&[4], &[13], false), vec![445]);
        assert_eq!(ctx.pow(&[4], &[13], true), vec![445]);
        // multi-limb: (2^64+1) modulus? use m = 2^127 - 1 (prime): 3^(m-1) = 1
        let m = vec![u64::MAX, (1u64 << 63) - 1];
        let ctx = MontCtx::new(m.clone());
        let mut e = m.clone();
        e[0] -= 1;
        assert_eq!(ctx.pow(&[3, 0], &e, false), vec![1, 0]);
        assert_eq!(reduce_be(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], &m), vec![2, 0]);
    }
}
