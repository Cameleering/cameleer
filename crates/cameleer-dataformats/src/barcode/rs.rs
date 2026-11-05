//! GF(256) arithmetic and Reed-Solomon encoding / decoding (Berlekamp-Massey, Chien search,
//! Forney), parameterised by the field polynomial and the generator base (QR Code: 0x11D,
//! base 0; Data Matrix: 0x12D, base 1).

/// A GF(2^8) field with α = 2.
pub struct Gf {
    exp: [u8; 512],
    log: [u8; 256],
    /// First root exponent of the generator polynomial.
    pub base: usize,
}

impl Gf {
    pub fn new(poly: u16, base: usize) -> Gf {
        let mut exp = [0u8; 512];
        let mut log = [0u8; 256];
        let mut x: u16 = 1;
        for (i, e) in exp.iter_mut().enumerate().take(255) {
            *e = x as u8;
            log[x as usize] = i as u8;
            x <<= 1;
            if x & 0x100 != 0 {
                x ^= poly;
            }
        }
        for i in 255..512 {
            exp[i] = exp[i - 255];
        }
        Gf { exp, log, base }
    }

    /// The QR Code field.
    pub fn qr() -> Gf {
        Gf::new(0x11D, 0)
    }

    /// The Data Matrix field.
    pub fn datamatrix() -> Gf {
        Gf::new(0x12D, 1)
    }

    pub fn mul(&self, a: u8, b: u8) -> u8 {
        if a == 0 || b == 0 {
            0
        } else {
            self.exp[self.log[a as usize] as usize + self.log[b as usize] as usize]
        }
    }

    pub fn div(&self, a: u8, b: u8) -> u8 {
        assert!(b != 0, "division by zero in GF(256)");
        if a == 0 {
            0
        } else {
            self.exp[(self.log[a as usize] as usize + 255 - self.log[b as usize] as usize) % 255]
        }
    }

    pub fn pow_a(&self, e: usize) -> u8 {
        self.exp[e % 255]
    }

    fn inv(&self, a: u8) -> u8 {
        self.div(1, a)
    }

    /// Generator polynomial Π (x - α^(base+i)), coefficients highest degree first.
    pub fn generator(&self, nsym: usize) -> Vec<u8> {
        let mut g = vec![1u8];
        for i in 0..nsym {
            let root = self.pow_a(self.base + i);
            let mut ng = vec![0u8; g.len() + 1];
            for (j, c) in g.iter().enumerate() {
                ng[j] ^= *c;
                ng[j + 1] ^= self.mul(*c, root);
            }
            g = ng;
        }
        g
    }

    /// Error correction codewords for `data`.
    pub fn encode(&self, data: &[u8], nsym: usize) -> Vec<u8> {
        let g = self.generator(nsym);
        let mut rem = vec![0u8; nsym];
        for &d in data {
            let factor = d ^ rem[0];
            rem.remove(0);
            rem.push(0);
            if factor != 0 {
                for i in 0..nsym {
                    rem[i] ^= self.mul(g[i + 1], factor);
                }
            }
        }
        rem
    }

    /// Evaluate a polynomial (highest degree first) at x.
    fn eval_hi(&self, p: &[u8], x: u8) -> u8 {
        p.iter().fold(0u8, |acc, c| self.mul(acc, x) ^ c)
    }

    /// Evaluate a polynomial (lowest degree first) at x.
    fn eval_lo(&self, p: &[u8], x: u8) -> u8 {
        p.iter().rev().fold(0u8, |acc, c| self.mul(acc, x) ^ c)
    }

    /// Correct `block` (data followed by `nsym` EC codewords) in place; returns the number of
    /// corrected codewords, or None when the errors exceed the correction capacity.
    pub fn decode(&self, block: &mut [u8], nsym: usize) -> Option<usize> {
        let n = block.len();
        let synd: Vec<u8> = (0..nsym).map(|j| self.eval_hi(block, self.pow_a(self.base + j))).collect();
        if synd.iter().all(|s| *s == 0) {
            return Some(0);
        }
        // Berlekamp-Massey (polynomials lowest degree first)
        let mut c = vec![1u8];
        let mut b = vec![1u8];
        let mut l = 0usize;
        let mut m = 1usize;
        let mut bb = 1u8;
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
        // Chien search: degree d is an error position when Λ(α^-d) = 0
        let mut positions = Vec::new();
        for d in 0..n {
            let x_inv = self.pow_a(255 - d % 255);
            if self.eval_lo(&c, x_inv) == 0 {
                positions.push(d);
            }
        }
        if positions.len() != nerr {
            return None;
        }
        // Ω = S·Λ mod x^nsym
        let mut omega = vec![0u8; nsym];
        for (i, s) in synd.iter().enumerate() {
            for (j, cj) in c.iter().enumerate() {
                if i + j < nsym {
                    omega[i + j] ^= self.mul(*s, *cj);
                }
            }
        }
        // Λ' (formal derivative)
        let deriv: Vec<u8> = (1..c.len()).map(|i| if i % 2 == 1 { c[i] } else { 0 }).collect();
        for &d in &positions {
            let x = self.pow_a(d);
            let x_inv = self.inv(x);
            let num = self.eval_lo(&omega, x_inv);
            let den = self.eval_lo(&deriv, x_inv);
            if den == 0 {
                return None;
            }
            let mut e = self.div(num, den);
            // multiply by X^(1-base)
            if self.base == 0 {
                e = self.mul(e, x);
            } else {
                for _ in 1..self.base {
                    e = self.mul(e, x_inv);
                }
            }
            block[n - 1 - d] ^= e;
        }
        // verify
        if (0..nsym).any(|j| self.eval_hi(block, self.pow_a(self.base + j)) != 0) {
            return None;
        }
        Some(nerr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qr_reference_codewords() {
        let gf = Gf::qr();
        // ISO/IEC 18004 Annex I: "01234567", version 1-M
        let data = [0x10, 0x20, 0x0C, 0x56, 0x61, 0x80, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11];
        assert_eq!(gf.encode(&data, 10), vec![0xA5, 0x24, 0xD4, 0xC1, 0xED, 0x36, 0xC7, 0x87, 0x2C, 0x55]);
        // "HELLO WORLD" 1-M
        let data = [0x20, 0x5B, 0x0B, 0x78, 0xD1, 0x72, 0xDC, 0x4D, 0x43, 0x40, 0xEC, 0x11, 0xEC, 0x11, 0xEC, 0x11];
        assert_eq!(gf.encode(&data, 10), vec![196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }

    #[test]
    fn corrects_errors_in_both_fields() {
        for gf in [Gf::qr(), Gf::datamatrix()] {
            let data: Vec<u8> = (0..40u8).map(|i| i.wrapping_mul(37).wrapping_add(5)).collect();
            let ecc = gf.encode(&data, 16);
            let mut block = [data.clone(), ecc].concat();
            let orig = block.clone();
            for (i, p) in [0usize, 7, 20, 33, 41, 50, 55, 3].iter().enumerate() {
                block[*p] ^= (i as u8 + 1) * 29;
            }
            assert_eq!(gf.decode(&mut block, 16), Some(8));
            assert_eq!(block, orig);
            // too many errors
            let mut bad = orig.clone();
            for p in 0..9 {
                bad[p * 6] ^= 0x55;
            }
            assert!(gf.decode(&mut bad, 16).is_none() || bad != orig);
        }
    }
}
