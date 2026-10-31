//! Ed25519 signatures (RFC 8032, PureEdDSA over edwards25519).
//!
//! Field arithmetic over GF(2^255 - 19) uses 5 x 51-bit limbs; scalar
//! multiplication is a fixed 256-step double-and-add-always ladder with masked
//! selection, so signing has no secret-dependent branches or memory indices.
//! Scalars modulo the group order L are reduced with a bitwise constant-time
//! shift-and-subtract loop.

use super::hash::{Digest, Sha512};
use std::sync::OnceLock;

const MASK: u64 = (1 << 51) - 1;

#[derive(Clone, Copy, Debug)]
struct Fe([u64; 5]);

fn load64(b: &[u8], i: usize) -> u64 {
    let mut x = [0u8; 8];
    x.copy_from_slice(&b[i..i + 8]);
    u64::from_le_bytes(x)
}

impl Fe {
    const ZERO: Fe = Fe([0; 5]);
    const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    fn from_u64(v: u64) -> Fe {
        Fe([v & MASK, v >> 51, 0, 0, 0])
    }

    /// Decodes 32 little-endian bytes, ignoring the top bit.
    fn from_bytes(b: &[u8; 32]) -> Fe {
        Fe([
            load64(b, 0) & MASK,
            (load64(b, 6) >> 3) & MASK,
            (load64(b, 12) >> 6) & MASK,
            (load64(b, 19) >> 1) & MASK,
            (load64(b, 24) >> 12) & MASK,
        ])
    }

    fn carry(mut l: [u64; 5]) -> Fe {
        for i in 0..4 {
            l[i + 1] += l[i] >> 51;
            l[i] &= MASK;
        }
        l[0] += 19 * (l[4] >> 51);
        l[4] &= MASK;
        l[1] += l[0] >> 51;
        l[0] &= MASK;
        Fe(l)
    }

    /// Canonical little-endian encoding (fully reduced modulo p).
    fn to_bytes(self) -> [u8; 32] {
        let mut h = Fe::carry(self.0).0;
        // q = 1 iff h >= p
        let mut q = (h[0] + 19) >> 51;
        for limb in h.iter().skip(1) {
            q = (limb + q) >> 51;
        }
        h[0] += 19 * q;
        for i in 0..4 {
            h[i + 1] += h[i] >> 51;
            h[i] &= MASK;
        }
        h[4] &= MASK;
        let mut out = [0u8; 32];
        let (mut acc, mut bits, mut idx) = (0u128, 0u32, 0usize);
        for limb in h {
            acc |= (limb as u128) << bits;
            bits += 51;
            while bits >= 8 {
                out[idx] = acc as u8;
                acc >>= 8;
                bits -= 8;
                idx += 1;
            }
        }
        if idx < 32 {
            out[idx] = acc as u8;
        }
        out
    }

    fn add(self, b: Fe) -> Fe {
        let mut l = [0u64; 5];
        for (i, v) in l.iter_mut().enumerate() {
            *v = self.0[i] + b.0[i];
        }
        Fe::carry(l)
    }

    fn sub(self, b: Fe) -> Fe {
        // add 4p to stay positive
        const P4: [u64; 5] = [0x1fffffffffffb4, 0x1ffffffffffffc, 0x1ffffffffffffc, 0x1ffffffffffffc, 0x1ffffffffffffc];
        let mut l = [0u64; 5];
        for (i, v) in l.iter_mut().enumerate() {
            *v = self.0[i] + P4[i] - b.0[i];
        }
        Fe::carry(l)
    }

    fn neg(self) -> Fe {
        Fe::ZERO.sub(self)
    }

    fn mul(self, b: Fe) -> Fe {
        let a = self.0;
        let b = b.0;
        let m = |x: u64, y: u64| (x as u128) * (y as u128);
        let b1 = b[1] * 19;
        let b2 = b[2] * 19;
        let b3 = b[3] * 19;
        let b4 = b[4] * 19;
        let r0 = m(a[0], b[0]) + m(a[1], b4) + m(a[2], b3) + m(a[3], b2) + m(a[4], b1);
        let r1 = m(a[0], b[1]) + m(a[1], b[0]) + m(a[2], b4) + m(a[3], b3) + m(a[4], b2);
        let r2 = m(a[0], b[2]) + m(a[1], b[1]) + m(a[2], b[0]) + m(a[3], b4) + m(a[4], b3);
        let r3 = m(a[0], b[3]) + m(a[1], b[2]) + m(a[2], b[1]) + m(a[3], b[0]) + m(a[4], b4);
        let r4 = m(a[0], b[4]) + m(a[1], b[3]) + m(a[2], b[2]) + m(a[3], b[1]) + m(a[4], b[0]);
        let mut r = [r0, r1, r2, r3, r4];
        for i in 0..4 {
            r[i + 1] += r[i] >> 51;
            r[i] &= MASK as u128;
        }
        let c = r[4] >> 51;
        r[4] &= MASK as u128;
        let l0 = r[0] + c * 19;
        Fe::carry([l0 as u64 & MASK, r[1] as u64 + (l0 >> 51) as u64, r[2] as u64, r[3] as u64, r[4] as u64])
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    fn pow2k(self, k: u32) -> Fe {
        let mut r = self;
        for _ in 0..k {
            r = r.square();
        }
        r
    }

    /// self^(2^250 - 1) and self^11 helpers (standard addition chain).
    fn pow_chain(self) -> (Fe, Fe) {
        let z2 = self.square();
        let z9 = z2.pow2k(2).mul(self);
        let z11 = z9.mul(z2);
        let z2_5_0 = z11.square().mul(z9);
        let z2_10_0 = z2_5_0.pow2k(5).mul(z2_5_0);
        let z2_20_0 = z2_10_0.pow2k(10).mul(z2_10_0);
        let z2_40_0 = z2_20_0.pow2k(20).mul(z2_20_0);
        let z2_50_0 = z2_40_0.pow2k(10).mul(z2_10_0);
        let z2_100_0 = z2_50_0.pow2k(50).mul(z2_50_0);
        let z2_200_0 = z2_100_0.pow2k(100).mul(z2_100_0);
        let z2_250_0 = z2_200_0.pow2k(50).mul(z2_50_0);
        (z2_250_0, z11)
    }

    /// self^(p-2)
    fn invert(self) -> Fe {
        let (t, z11) = self.pow_chain();
        t.pow2k(5).mul(z11)
    }

    /// self^((p-5)/8) = self^(2^252 - 3)
    fn pow22523(self) -> Fe {
        let (t, _) = self.pow_chain();
        t.pow2k(2).mul(self)
    }

    fn is_zero(self) -> bool {
        self.to_bytes().iter().fold(0u8, |a, b| a | b) == 0
    }

    fn is_negative(self) -> u8 {
        self.to_bytes()[0] & 1
    }

    fn equals(self, b: Fe) -> bool {
        crate::util::ct_eq(&self.to_bytes(), &b.to_bytes())
    }

    /// Returns `b` when flag is 1, self when 0 (constant time).
    fn select(self, b: Fe, flag: u64) -> Fe {
        let mask = flag.wrapping_neg();
        let mut l = self.0;
        for (i, v) in l.iter_mut().enumerate() {
            *v ^= mask & (*v ^ b.0[i]);
        }
        Fe(l)
    }
}

struct Consts {
    d: Fe,
    d2: Fe,
    sqrt_m1: Fe,
    base: Point,
}

fn consts() -> &'static Consts {
    static C: OnceLock<Consts> = OnceLock::new();
    C.get_or_init(|| {
        let d = Fe::from_u64(121665).neg().mul(Fe::from_u64(121666).invert());
        // sqrt(-1) = 2^((p-1)/4)
        let two = Fe::from_u64(2);
        // (p-1)/4 = 2^253 - 5; 2^(2^253 - 5) = 2^(2^253) / 32
        let mut t = two;
        for _ in 0..253 {
            t = t.square();
        }
        let sqrt_m1 = t.mul(Fe::from_u64(32).invert());
        let mut enc = [0x66u8; 32];
        enc[0] = 0x58;
        let mut c = Consts { d, d2: d.add(d), sqrt_m1, base: Point::IDENTITY };
        c.base = Point::decode_with(&enc, &c).expect("ed25519 base point");
        c
    })
}

/// A point in extended twisted Edwards coordinates (X:Y:Z:T), x = X/Z, y = Y/Z, xy = T/Z.
#[derive(Clone, Copy, Debug)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

impl Point {
    const IDENTITY: Point = Point { x: Fe::ZERO, y: Fe::ONE, z: Fe::ONE, t: Fe::ZERO };

    fn decode_with(b: &[u8; 32], c: &Consts) -> Option<Point> {
        let x0 = b[31] >> 7;
        let y = Fe::from_bytes(b);
        // reject non-canonical y (y >= p)
        let mut yb = *b;
        yb[31] &= 0x7f;
        if y.to_bytes() != yb {
            return None;
        }
        let yy = y.square();
        let u = yy.sub(Fe::ONE);
        let v = c.d.mul(yy).add(Fe::ONE);
        let v3 = v.square().mul(v);
        let v7 = v3.square().mul(v);
        let mut x = u.mul(v3).mul(u.mul(v7).pow22523());
        let vxx = v.mul(x.square());
        if vxx.equals(u) {
        } else if vxx.equals(u.neg()) {
            x = x.mul(c.sqrt_m1);
        } else {
            return None;
        }
        if x.is_zero() && x0 == 1 {
            return None;
        }
        if x.is_negative() != x0 {
            x = x.neg();
        }
        Some(Point { x, y, z: Fe::ONE, t: x.mul(y) })
    }

    fn decode(b: &[u8; 32]) -> Option<Point> {
        Point::decode_with(b, consts())
    }

    fn encode(&self) -> [u8; 32] {
        let zi = self.z.invert();
        let x = self.x.mul(zi);
        let y = self.y.mul(zi);
        let mut out = y.to_bytes();
        out[31] |= x.is_negative() << 7;
        out
    }

    /// Unified (complete) addition, RFC 8032 section 5.1.4.
    fn add(&self, q: &Point) -> Point {
        let d2 = consts().d2;
        let a = self.y.sub(self.x).mul(q.y.sub(q.x));
        let b = self.y.add(self.x).mul(q.y.add(q.x));
        let c = self.t.mul(d2).mul(q.t);
        let d = self.z.add(self.z).mul(q.z);
        let e = b.sub(a);
        let f = d.sub(c);
        let g = d.add(c);
        let h = b.add(a);
        Point { x: e.mul(f), y: g.mul(h), t: e.mul(h), z: f.mul(g) }
    }

    fn neg(&self) -> Point {
        Point { x: self.x.neg(), y: self.y, z: self.z, t: self.t.neg() }
    }

    fn select(&self, b: &Point, flag: u64) -> Point {
        Point { x: self.x.select(b.x, flag), y: self.y.select(b.y, flag), z: self.z.select(b.z, flag), t: self.t.select(b.t, flag) }
    }

    /// Constant-time scalar multiplication by a 256-bit little-endian scalar.
    fn mul(&self, k: &[u8; 32]) -> Point {
        let mut r = Point::IDENTITY;
        for i in (0..256).rev() {
            r = r.add(&r);
            let s = r.add(self);
            let bit = ((k[i / 8] >> (i % 8)) & 1) as u64;
            r = r.select(&s, bit);
        }
        r
    }
}

// ---------------------------------------------------------------- scalars mod L

const L: [u64; 4] = [0x5812631a5cf5d3ed, 0x14def9dea2f79cd6, 0, 0x1000000000000000];

/// Reduces a little-endian integer (any length) modulo L, constant time in the value.
fn reduce_mod_l(bytes: &[u8]) -> [u8; 32] {
    let mut r = [0u64; 4];
    for i in (0..bytes.len() * 8).rev() {
        let bit = ((bytes[i / 8] >> (i % 8)) & 1) as u64;
        // r = 2r + bit  (r < L < 2^253, so no overflow)
        let mut carry = bit;
        for limb in r.iter_mut() {
            let nc = *limb >> 63;
            *limb = (*limb << 1) | carry;
            carry = nc;
        }
        // conditional subtract
        let mut t = [0u64; 4];
        let mut borrow = 0u64;
        for j in 0..4 {
            let (d1, b1) = r[j].overflowing_sub(L[j]);
            let (d2, b2) = d1.overflowing_sub(borrow);
            t[j] = d2;
            borrow = (b1 | b2) as u64;
        }
        let mask = borrow.wrapping_sub(1); // all ones when no borrow (r >= L)
        for j in 0..4 {
            r[j] = (t[j] & mask) | (r[j] & !mask);
        }
    }
    let mut out = [0u8; 32];
    for (j, limb) in r.iter().enumerate() {
        out[j * 8..j * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    out
}

/// (a * b + c) mod L for 32-byte little-endian inputs.
fn mul_add_mod_l(a: &[u8; 32], b: &[u8; 32], c: &[u8; 32]) -> [u8; 32] {
    let limbs = |x: &[u8; 32]| -> [u64; 4] {
        let mut l = [0u64; 4];
        for (j, v) in l.iter_mut().enumerate() {
            *v = load64(x, j * 8);
        }
        l
    };
    let (al, bl, cl) = (limbs(a), limbs(b), limbs(c));
    let mut p = [0u64; 9];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let t = (al[i] as u128) * (bl[j] as u128) + p[i + j] as u128 + carry;
            p[i + j] = t as u64;
            carry = t >> 64;
        }
        p[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for (j, v) in p.iter_mut().enumerate() {
        let t = *v as u128 + if j < 4 { cl[j] as u128 } else { 0 } + carry;
        *v = t as u64;
        carry = t >> 64;
    }
    let mut bytes = Vec::with_capacity(72);
    for limb in p {
        bytes.extend_from_slice(&limb.to_le_bytes());
    }
    reduce_mod_l(&bytes)
}

fn is_canonical_scalar(s: &[u8]) -> bool {
    // s < L, compared from the most significant byte
    let mut lb = [0u8; 32];
    for (j, limb) in L.iter().enumerate() {
        lb[j * 8..j * 8 + 8].copy_from_slice(&limb.to_le_bytes());
    }
    for i in (0..32).rev() {
        if s[i] != lb[i] {
            return s[i] < lb[i];
        }
    }
    false
}

// ---------------------------------------------------------------- public API

fn expand(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let h = Sha512::digest(seed);
    let mut a = [0u8; 32];
    a.copy_from_slice(&h[..32]);
    a[0] &= 248;
    a[31] &= 127;
    a[31] |= 64;
    let mut prefix = [0u8; 32];
    prefix.copy_from_slice(&h[32..]);
    (a, prefix)
}

/// Derives the 32-byte public key from a 32-byte private seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    let (a, _) = expand(seed);
    consts().base.mul(&a).encode()
}

/// Generates a random private seed.
pub fn generate_seed() -> [u8; 32] {
    let mut s = [0u8; 32];
    crate::crypto::rng::fill(&mut s);
    s
}

/// Signs `msg` with the private seed (public key derived from it).
pub fn sign(seed: &[u8; 32], msg: &[u8]) -> [u8; 64] {
    let (a, prefix) = expand(seed);
    let base = consts().base;
    let public = base.mul(&a).encode();
    let mut h = Sha512::new();
    h.update(&prefix);
    h.update(msg);
    let r = reduce_mod_l(&h.finish());
    let rp = base.mul(&r).encode();
    let mut h = Sha512::new();
    h.update(&rp);
    h.update(&public);
    h.update(msg);
    let k = reduce_mod_l(&h.finish());
    let s = mul_add_mod_l(&k, &a, &r);
    let mut sig = [0u8; 64];
    sig[..32].copy_from_slice(&rp);
    sig[32..].copy_from_slice(&s);
    sig
}

/// Verifies a 64-byte signature (cofactorless equation [S]B = R + [k]A).
pub fn verify(public: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    if public.len() != 32 || sig.len() != 64 {
        return false;
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(public);
    let a = match Point::decode(&pk) {
        Some(p) => p,
        None => return false,
    };
    let s: [u8; 32] = sig[32..].try_into().unwrap();
    if !is_canonical_scalar(&s) {
        return false;
    }
    let mut h = Sha512::new();
    h.update(&sig[..32]);
    h.update(public);
    h.update(msg);
    let k = reduce_mod_l(&h.finish());
    let check = consts().base.mul(&s).add(&a.neg().mul(&k));
    crate::util::ct_eq(&check.encode(), &sig[..32])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    fn arr32(h: &str) -> [u8; 32] {
        unhex(h).try_into().unwrap()
    }

    // RFC 8032 section 7.1 test vectors 1-3 and SHA(abc).
    const VECTORS: &[(&str, &str, &str, &str)] = &[
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        ),
        (
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        ),
        (
            "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
            "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
        ),
    ];

    #[test]
    fn rfc8032_vectors() {
        for (sk, pk, msg, sig) in VECTORS {
            let seed = arr32(sk);
            assert_eq!(hex(&public_key(&seed)), *pk);
            let m = unhex(msg);
            let s = sign(&seed, &m);
            assert_eq!(hex(&s), *sig);
            assert!(verify(&unhex(pk), &m, &s));
            let mut bad = s;
            bad[10] ^= 1;
            assert!(!verify(&unhex(pk), &m, &bad));
            assert!(!verify(&unhex(pk), b"other", &s));
        }
    }

    #[test]
    fn rejects_non_canonical_s_and_bad_points() {
        let seed = arr32(VECTORS[0].0);
        let pk = public_key(&seed);
        let mut s = sign(&seed, b"x");
        // S + L is non-canonical
        let mut carry = 0u16;
        let mut lb = [0u8; 32];
        for (j, limb) in L.iter().enumerate() {
            lb[j * 8..j * 8 + 8].copy_from_slice(&limb.to_le_bytes());
        }
        for i in 0..32 {
            let v = s[32 + i] as u16 + lb[i] as u16 + carry;
            s[32 + i] = v as u8;
            carry = v >> 8;
        }
        assert!(!verify(&pk, b"x", &s));
        assert!(!verify(&[0xffu8; 32], b"x", &sign(&seed, b"x")));
    }

    #[test]
    fn scalar_reduction() {
        // L itself reduces to 0, L+1 to 1
        let mut lb = Vec::new();
        for limb in L {
            lb.extend_from_slice(&limb.to_le_bytes());
        }
        assert_eq!(reduce_mod_l(&lb), [0u8; 32]);
        lb[0] += 1;
        let mut one = [0u8; 32];
        one[0] = 1;
        assert_eq!(reduce_mod_l(&lb), one);
        let seed = generate_seed();
        let msg = b"random roundtrip";
        assert!(verify(&public_key(&seed), msg, &sign(&seed, msg)));
    }
}
