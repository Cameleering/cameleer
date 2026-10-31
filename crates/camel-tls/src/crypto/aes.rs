//! AES-128 / AES-256 block cipher (FIPS-197, encryption direction only) and
//! AES-GCM (NIST SP 800-38D).
//!
//! Limitation: SubBytes uses a 256-byte lookup table, so the block cipher is not
//! constant-time with respect to cache-timing attackers sharing the CPU. GHASH is
//! implemented with branch-free bitwise multiplication.

use crate::util::ct_eq;

const fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    let mut i = 0;
    while i < 8 {
        if b & 1 != 0 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
        i += 1;
    }
    p
}

const fn make_sbox() -> [u8; 256] {
    let mut sbox = [0u8; 256];
    let mut x = 0usize;
    while x < 256 {
        // multiplicative inverse = x^254
        let mut inv = 1u8;
        let mut base = x as u8;
        let mut e = 254u32;
        while e > 0 {
            if e & 1 == 1 {
                inv = gf_mul(inv, base);
            }
            base = gf_mul(base, base);
            e >>= 1;
        }
        if x == 0 {
            inv = 0;
        }
        let s = inv ^ inv.rotate_left(1) ^ inv.rotate_left(2) ^ inv.rotate_left(3) ^ inv.rotate_left(4) ^ 0x63;
        sbox[x] = s;
        x += 1;
    }
    sbox
}

static SBOX: [u8; 256] = make_sbox();

#[inline]
fn xtime(b: u8) -> u8 {
    (b << 1) ^ (((b >> 7) & 1) * 0x1b)
}

#[derive(Clone)]
pub struct Aes {
    round_keys: Vec<[u8; 16]>,
}

impl Aes {
    /// Creates an AES instance; key must be 16 or 32 bytes (24 also accepted).
    pub fn new(key: &[u8]) -> Aes {
        let nk = key.len() / 4;
        assert!(matches!(key.len(), 16 | 24 | 32), "AES key must be 16/24/32 bytes");
        let nr = nk + 6;
        let total = 4 * (nr + 1);
        let mut w: Vec<[u8; 4]> = Vec::with_capacity(total);
        for i in 0..nk {
            w.push([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
        }
        let mut rcon = 1u8;
        for i in nk..total {
            let mut t = w[i - 1];
            if i % nk == 0 {
                t = [SBOX[t[1] as usize] ^ rcon, SBOX[t[2] as usize], SBOX[t[3] as usize], SBOX[t[0] as usize]];
                rcon = xtime(rcon);
            } else if nk > 6 && i % nk == 4 {
                t = [SBOX[t[0] as usize], SBOX[t[1] as usize], SBOX[t[2] as usize], SBOX[t[3] as usize]];
            }
            let p = w[i - nk];
            w.push([p[0] ^ t[0], p[1] ^ t[1], p[2] ^ t[2], p[3] ^ t[3]]);
        }
        let round_keys = w
            .chunks(4)
            .map(|c| {
                let mut k = [0u8; 16];
                for (j, word) in c.iter().enumerate() {
                    k[4 * j..4 * j + 4].copy_from_slice(word);
                }
                k
            })
            .collect();
        Aes { round_keys }
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let nr = self.round_keys.len() - 1;
        let s = block;
        for i in 0..16 {
            s[i] ^= self.round_keys[0][i];
        }
        for round in 1..=nr {
            // SubBytes
            for b in s.iter_mut() {
                *b = SBOX[*b as usize];
            }
            // ShiftRows (state is column-major: s[4*c + r])
            let t = *s;
            for c in 0..4 {
                for r in 0..4 {
                    s[4 * c + r] = t[4 * ((c + r) % 4) + r];
                }
            }
            // MixColumns
            if round != nr {
                for c in 0..4 {
                    let a0 = s[4 * c];
                    let a1 = s[4 * c + 1];
                    let a2 = s[4 * c + 2];
                    let a3 = s[4 * c + 3];
                    let all = a0 ^ a1 ^ a2 ^ a3;
                    s[4 * c] = a0 ^ all ^ xtime(a0 ^ a1);
                    s[4 * c + 1] = a1 ^ all ^ xtime(a1 ^ a2);
                    s[4 * c + 2] = a2 ^ all ^ xtime(a2 ^ a3);
                    s[4 * c + 3] = a3 ^ all ^ xtime(a3 ^ a0);
                }
            }
            for i in 0..16 {
                s[i] ^= self.round_keys[round][i];
            }
        }
    }
}

/// Branch-free GF(2^128) multiplication in the GCM bit order.
fn gf128_mul(x: u128, y: u128) -> u128 {
    let r: u128 = 0xe1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        let bit = (x >> (127 - i)) & 1;
        z ^= v & 0u128.wrapping_sub(bit);
        let lsb = v & 1;
        v = (v >> 1) ^ (r & 0u128.wrapping_sub(lsb));
    }
    z
}

#[derive(Clone)]
pub struct AesGcm {
    aes: Aes,
    h: u128,
}

impl AesGcm {
    pub fn new(key: &[u8]) -> AesGcm {
        let aes = Aes::new(key);
        let mut zero = [0u8; 16];
        aes.encrypt_block(&mut zero);
        AesGcm { aes, h: u128::from_be_bytes(zero) }
    }

    fn ghash(&self, aad: &[u8], ct: &[u8]) -> u128 {
        let mut y = 0u128;
        for data in [aad, ct] {
            for chunk in data.chunks(16) {
                let mut b = [0u8; 16];
                b[..chunk.len()].copy_from_slice(chunk);
                y = gf128_mul(y ^ u128::from_be_bytes(b), self.h);
            }
        }
        let lens = ((aad.len() as u128 * 8) << 64) | (ct.len() as u128 * 8);
        gf128_mul(y ^ lens, self.h)
    }

    fn ctr(&self, j0: &[u8; 16], data: &mut [u8]) {
        let mut counter = u32::from_be_bytes([j0[12], j0[13], j0[14], j0[15]]);
        for chunk in data.chunks_mut(16) {
            counter = counter.wrapping_add(1);
            let mut ks = *j0;
            ks[12..].copy_from_slice(&counter.to_be_bytes());
            self.aes.encrypt_block(&mut ks);
            for (d, k) in chunk.iter_mut().zip(ks.iter()) {
                *d ^= k;
            }
        }
    }

    fn j0(nonce: &[u8; 12]) -> [u8; 16] {
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        j0
    }

    fn tag(&self, j0: &[u8; 16], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let s = self.ghash(aad, ct);
        let mut ekj0 = *j0;
        self.aes.encrypt_block(&mut ekj0);
        (s ^ u128::from_be_bytes(ekj0)).to_be_bytes()
    }

    /// Encrypts in place and returns ciphertext || 16-byte tag.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let j0 = Self::j0(nonce);
        let mut out = plaintext.to_vec();
        self.ctr(&j0, &mut out);
        let tag = self.tag(&j0, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    /// Verifies and decrypts ciphertext || tag.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 16 {
            return None;
        }
        let (ct, tag) = data.split_at(data.len() - 16);
        let j0 = Self::j0(nonce);
        let expect = self.tag(&j0, aad, ct);
        if !ct_eq(&expect, tag) {
            return None;
        }
        let mut out = ct.to_vec();
        self.ctr(&j0, &mut out);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    #[test]
    fn sbox_spot_checks() {
        assert_eq!(SBOX[0x00], 0x63);
        assert_eq!(SBOX[0x01], 0x7c);
        assert_eq!(SBOX[0x53], 0xed);
        assert_eq!(SBOX[0xff], 0x16);
    }

    #[test]
    fn fips197_appendix_c() {
        let pt = unhex("00112233445566778899aabbccddeeff");
        let mut b = [0u8; 16];
        b.copy_from_slice(&pt);
        Aes::new(&unhex("000102030405060708090a0b0c0d0e0f")).encrypt_block(&mut b);
        assert_eq!(hex(&b), "69c4e0d86a7b0430d8cdb78070b4c55a");
        b.copy_from_slice(&pt);
        Aes::new(&unhex("000102030405060708090a0b0c0d0e0f1011121314151617")).encrypt_block(&mut b);
        assert_eq!(hex(&b), "dda97ca4864cdfe06eaf70a0ec0d7191");
        b.copy_from_slice(&pt);
        Aes::new(&unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")).encrypt_block(&mut b);
        assert_eq!(hex(&b), "8ea2b7ca516745bfeafc49904b496089");
        // FIPS-197 appendix B
        b.copy_from_slice(&unhex("3243f6a8885a308d313198a2e0370734"));
        Aes::new(&unhex("2b7e151628aed2a6abf7158809cf4f3c")).encrypt_block(&mut b);
        assert_eq!(hex(&b), "3925841d02dc09fbdc118597196a0b32");
    }

    fn n12(s: &str) -> [u8; 12] {
        let v = unhex(s);
        let mut n = [0u8; 12];
        n.copy_from_slice(&v);
        n
    }

    #[test]
    fn gcm_test_cases() {
        // GCM spec (McGrew & Viega) test cases 1-4, 13-16, as used by NIST SP 800-38D validation.
        let g = AesGcm::new(&[0u8; 16]);
        assert_eq!(hex(&g.seal(&[0; 12], &[], &[])), "58e2fccefa7e3061367f1d57a4e7455a");
        assert_eq!(
            hex(&g.seal(&[0; 12], &[], &[0u8; 16])),
            "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf"
        );
        let key = unhex("feffe9928665731c6d6a8f9467308308");
        let p = unhex(
            "d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b39",
        );
        let aad = unhex("feedfacedeadbeeffeedfacedeadbeefabaddad2");
        let nonce = n12("cafebabefacedbaddecaf888");
        let out = AesGcm::new(&key).seal(&nonce, &aad, &p);
        assert_eq!(
            hex(&out),
            "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091\
5bc94fbc3221a5db94fae95ae7121a47"
        );
        assert_eq!(AesGcm::new(&key).open(&nonce, &aad, &out).unwrap(), p);
        let mut bad = out.clone();
        bad[3] ^= 1;
        assert!(AesGcm::new(&key).open(&nonce, &aad, &bad).is_none());
        // Test case 14 (AES-256, 16 zero bytes)
        let g = AesGcm::new(&[0u8; 32]);
        assert_eq!(
            hex(&g.seal(&[0; 12], &[], &[0u8; 16])),
            "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919"
        );
        // Test case 16 (AES-256 with AAD)
        let key = unhex("feffe9928665731c6d6a8f9467308308feffe9928665731c6d6a8f9467308308");
        let out = AesGcm::new(&key).seal(&nonce, &aad, &p);
        assert_eq!(
            hex(&out),
            "522dc1f099567d07f47f37a32a84427d643a8cdcbfe5c0c97598a2bd2555d1aa8cb08e48590dbb3da7b08b1056828838c5f61e6393ba7a0abcc9f662\
76fc6ece0f4e1768cddf8853bb2d551b"
        );
    }
}
