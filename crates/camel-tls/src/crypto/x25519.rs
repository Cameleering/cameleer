//! X25519 Diffie-Hellman (RFC 7748). Constant-time Montgomery ladder over
//! GF(2^255 - 19) with 5 x 51-bit limbs.

#[derive(Clone, Copy)]
struct Fe([u64; 5]);

const MASK51: u64 = (1 << 51) - 1;

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn from_bytes(b: &[u8; 32]) -> Fe {
        let load = |i: usize| -> u64 {
            let mut x = [0u8; 8];
            let end = (i + 8).min(32);
            x[..end - i].copy_from_slice(&b[i..end]);
            u64::from_le_bytes(x)
        };
        Fe([
            load(0) & MASK51,
            (load(6) >> 3) & MASK51,
            (load(12) >> 6) & MASK51,
            (load(19) >> 1) & MASK51,
            (load(24) >> 12) & MASK51, // ignores bit 255
        ])
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut h = self.carry().0;
        // compute h mod p fully: q = (h + 19) >> 255
        let mut q = (h[0] + 19) >> 51;
        q = (h[1] + q) >> 51;
        q = (h[2] + q) >> 51;
        q = (h[3] + q) >> 51;
        q = (h[4] + q) >> 51;
        h[0] += 19 * q;
        let mut c = h[0] >> 51;
        h[0] &= MASK51;
        h[1] += c;
        c = h[1] >> 51;
        h[1] &= MASK51;
        h[2] += c;
        c = h[2] >> 51;
        h[2] &= MASK51;
        h[3] += c;
        c = h[3] >> 51;
        h[3] &= MASK51;
        h[4] += c;
        h[4] &= MASK51;
        let v: [u64; 4] = [
            h[0] | (h[1] << 51),
            (h[1] >> 13) | (h[2] << 38),
            (h[2] >> 26) | (h[3] << 25),
            (h[3] >> 39) | (h[4] << 12),
        ];
        let mut out = [0u8; 32];
        for i in 0..4 {
            out[8 * i..8 * i + 8].copy_from_slice(&v[i].to_le_bytes());
        }
        out
    }

    fn carry(self) -> Fe {
        let mut h = self.0;
        let mut c;
        c = h[0] >> 51;
        h[0] &= MASK51;
        h[1] += c;
        c = h[1] >> 51;
        h[1] &= MASK51;
        h[2] += c;
        c = h[2] >> 51;
        h[2] &= MASK51;
        h[3] += c;
        c = h[3] >> 51;
        h[3] &= MASK51;
        h[4] += c;
        c = h[4] >> 51;
        h[4] &= MASK51;
        h[0] += c * 19;
        c = h[0] >> 51;
        h[0] &= MASK51;
        h[1] += c;
        Fe(h)
    }

    fn add(self, o: Fe) -> Fe {
        let mut r = [0u64; 5];
        for i in 0..5 {
            r[i] = self.0[i] + o.0[i];
        }
        Fe(r).carry()
    }

    fn sub(self, o: Fe) -> Fe {
        // add 4p to avoid underflow (inputs are carried, limbs < 2^52)
        const P4: [u64; 5] = [0x1fffffffffffb4, 0x1ffffffffffffc, 0x1ffffffffffffc, 0x1ffffffffffffc, 0x1ffffffffffffc];
        let mut r = [0u64; 5];
        for i in 0..5 {
            r[i] = self.0[i] + P4[i] - o.0[i];
        }
        Fe(r).carry()
    }

    fn mul(self, o: Fe) -> Fe {
        let a = self.0;
        let b = o.0;
        let m = |x: u64, y: u64| (x as u128) * (y as u128);
        let b1 = b[1] * 19;
        let b2 = b[2] * 19;
        let b3 = b[3] * 19;
        let b4 = b[4] * 19;
        let t0 = m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1);
        let mut t1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2);
        let mut t2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3);
        let mut t3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4);
        let mut t4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
        let mut r = [0u64; 5];
        let mut c = t0 >> 51;
        r[0] = (t0 as u64) & MASK51;
        t1 += c;
        c = t1 >> 51;
        r[1] = (t1 as u64) & MASK51;
        t2 += c;
        c = t2 >> 51;
        r[2] = (t2 as u64) & MASK51;
        t3 += c;
        c = t3 >> 51;
        r[3] = (t3 as u64) & MASK51;
        t4 += c;
        c = t4 >> 51;
        r[4] = (t4 as u64) & MASK51;
        r[0] += (c as u64) * 19;
        let c2 = r[0] >> 51;
        r[0] &= MASK51;
        r[1] += c2;
        Fe(r)
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    fn mul_small(self, k: u64) -> Fe {
        let mut r = [0u128; 5];
        for i in 0..5 {
            r[i] = self.0[i] as u128 * k as u128;
        }
        let mut out = [0u64; 5];
        let mut c = 0u128;
        for i in 0..5 {
            let v = r[i] + c;
            out[i] = (v as u64) & MASK51;
            c = v >> 51;
        }
        out[0] += (c as u64) * 19;
        Fe(out).carry()
    }

    fn invert(self) -> Fe {
        // self^(p-2), p-2 = 2^255 - 21
        let mut result = Fe::ONE;
        let exp: [u8; 32] = {
            let mut e = [0xffu8; 32];
            e[0] = 0xeb;
            e[31] = 0x7f;
            e
        };
        for i in (0..255).rev() {
            result = result.square();
            if (exp[i / 8] >> (i % 8)) & 1 == 1 {
                result = result.mul(self);
            }
        }
        result
    }

    fn cswap(a: &mut Fe, b: &mut Fe, swap: u64) {
        let mask = swap.wrapping_neg();
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}

/// Computes X25519(scalar, u).
pub fn x25519(scalar: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let x1 = Fe::from_bytes(u);
    let mut x2 = Fe::ONE;
    let mut z2 = Fe::ZERO;
    let mut x3 = x1;
    let mut z3 = Fe::ONE;
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let kt = ((k[t / 8] >> (t % 8)) & 1) as u64;
        swap ^= kt;
        Fe::cswap(&mut x2, &mut x3, swap);
        Fe::cswap(&mut z2, &mut z3, swap);
        swap = kt;
        let a = x2.add(z2);
        let aa = a.square();
        let b = x2.sub(z2);
        let bb = b.square();
        let e = aa.sub(bb);
        let c = x3.add(z3);
        let d = x3.sub(z3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x3 = da.add(cb).square();
        z3 = x1.mul(da.sub(cb).square());
        x2 = aa.mul(bb);
        z2 = e.mul(aa.add(e.mul_small(121665)));
    }
    Fe::cswap(&mut x2, &mut x3, swap);
    Fe::cswap(&mut z2, &mut z3, swap);
    x2.mul(z2.invert()).to_bytes()
}

pub const BASEPOINT: [u8; 32] = {
    let mut b = [0u8; 32];
    b[0] = 9;
    b
};

pub fn x25519_base(scalar: &[u8; 32]) -> [u8; 32] {
    x25519(scalar, &BASEPOINT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    fn b32(s: &str) -> [u8; 32] {
        let mut k = [0u8; 32];
        k.copy_from_slice(&unhex(s));
        k
    }

    #[test]
    fn rfc7748_vectors() {
        // 5.2
        let out = x25519(
            &b32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4"),
            &b32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c"),
        );
        assert_eq!(hex(&out), "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552");
        let out = x25519(
            &b32("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d"),
            &b32("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493"),
        );
        assert_eq!(hex(&out), "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957");
    }

    #[test]
    fn rfc7748_iterated() {
        let mut k = BASEPOINT;
        let mut u = BASEPOINT;
        for i in 1..=1000 {
            let r = x25519(&k, &u);
            u = k;
            k = r;
            if i == 1 {
                assert_eq!(hex(&k), "422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079");
            }
        }
        assert_eq!(hex(&k), "684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51");
    }

    #[test]
    fn rfc7748_dh() {
        // 6.1
        let a = b32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b = b32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let apub = x25519_base(&a);
        assert_eq!(hex(&apub), "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a");
        let bpub = x25519_base(&b);
        assert_eq!(hex(&bpub), "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f");
        let s1 = x25519(&a, &bpub);
        assert_eq!(hex(&s1), "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742");
        assert_eq!(s1, x25519(&b, &apub));
    }
}
