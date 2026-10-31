//! AES-OCB (RFC 7253, 128-bit tags) decryption and encryption, used by the LibrePGP / GnuPG
//! "OCB Encrypted Data" packet (tag 20) that GnuPG 2.3+ produces for keys advertising AEAD.

use camel_tls::crypto::aes::Aes;
use camel_tls::util::ct_eq;

fn double(b: &[u8; 16]) -> [u8; 16] {
    let mut o = [0u8; 16];
    let carry = b[0] >> 7;
    for i in 0..16 {
        o[i] = (b[i] << 1) | if i < 15 { b[i + 1] >> 7 } else { 0 };
    }
    o[15] ^= 0x87 * carry;
    o
}

fn xor(a: &mut [u8; 16], b: &[u8; 16]) {
    for i in 0..16 {
        a[i] ^= b[i];
    }
}

/// An OCB-AES instance.
pub struct Ocb {
    aes: Aes,
    l_star: [u8; 16],
    l_dollar: [u8; 16],
    l: Vec<[u8; 16]>,
}

impl Ocb {
    pub fn new(key: &[u8]) -> Ocb {
        let aes = Aes::new(key);
        let mut l_star = [0u8; 16];
        aes.encrypt_block(&mut l_star);
        let l_dollar = double(&l_star);
        let mut l = vec![double(&l_dollar)];
        for i in 1..32 {
            let n = double(&l[i - 1]);
            l.push(n);
        }
        Ocb { aes, l_star, l_dollar, l }
    }

    fn hash(&self, a: &[u8]) -> [u8; 16] {
        let mut sum = [0u8; 16];
        let mut off = [0u8; 16];
        let mut chunks = a.chunks_exact(16);
        for (i, c) in chunks.by_ref().enumerate() {
            xor(&mut off, &self.l[(i + 1).trailing_zeros() as usize]);
            let mut b: [u8; 16] = c.try_into().unwrap();
            xor(&mut b, &off);
            self.aes.encrypt_block(&mut b);
            xor(&mut sum, &b);
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            xor(&mut off, &self.l_star);
            let mut b = [0u8; 16];
            b[..rem.len()].copy_from_slice(rem);
            b[rem.len()] = 0x80;
            xor(&mut b, &off);
            self.aes.encrypt_block(&mut b);
            xor(&mut sum, &b);
        }
        sum
    }

    fn initial_offset(&self, nonce: &[u8]) -> [u8; 16] {
        let mut n = [0u8; 16];
        n[16 - nonce.len()..].copy_from_slice(nonce);
        n[15 - nonce.len()] |= 1;
        let bottom = (n[15] & 0x3f) as usize;
        n[15] &= 0xc0;
        let mut ktop = n;
        self.aes.encrypt_block(&mut ktop);
        let mut stretch = [0u8; 24];
        stretch[..16].copy_from_slice(&ktop);
        for i in 0..8 {
            stretch[16 + i] = ktop[i] ^ ktop[i + 1];
        }
        let mut off = [0u8; 16];
        let (byte, bit) = (bottom / 8, bottom % 8);
        for i in 0..16 {
            off[i] = if bit == 0 { stretch[i + byte] } else { (stretch[i + byte] << bit) | (stretch[i + byte + 1] >> (8 - bit)) };
        }
        off
    }

    fn crypt(&self, nonce: &[u8], aad: &[u8], input: &[u8], encrypt: bool) -> (Vec<u8>, [u8; 16]) {
        let mut off = self.initial_offset(nonce);
        let mut checksum = [0u8; 16];
        let mut out = Vec::with_capacity(input.len());
        let mut chunks = input.chunks_exact(16);
        for (i, c) in chunks.by_ref().enumerate() {
            xor(&mut off, &self.l[(i + 1).trailing_zeros() as usize]);
            let mut b: [u8; 16] = c.try_into().unwrap();
            if encrypt {
                xor(&mut checksum, &b);
            }
            xor(&mut b, &off);
            if encrypt {
                self.aes.encrypt_block(&mut b);
            } else {
                self.aes.decrypt_block(&mut b);
            }
            xor(&mut b, &off);
            if !encrypt {
                xor(&mut checksum, &b);
            }
            out.extend_from_slice(&b);
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            xor(&mut off, &self.l_star);
            let mut pad = off;
            self.aes.encrypt_block(&mut pad);
            let mut p = [0u8; 16];
            for (i, x) in rem.iter().enumerate() {
                let y = x ^ pad[i];
                out.push(y);
                p[i] = if encrypt { *x } else { y };
            }
            p[rem.len()] = 0x80;
            xor(&mut checksum, &p);
        }
        let mut t = checksum;
        xor(&mut t, &off);
        xor(&mut t, &self.l_dollar);
        self.aes.encrypt_block(&mut t);
        xor(&mut t, &self.hash(aad));
        (out, t)
    }

    /// Encrypt; returns ciphertext || tag.
    pub fn seal(&self, nonce: &[u8], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let (mut c, t) = self.crypt(nonce, aad, plaintext, true);
        c.extend_from_slice(&t);
        c
    }

    /// Verify and decrypt ciphertext || tag.
    pub fn open(&self, nonce: &[u8], aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 16 {
            return None;
        }
        let (c, tag) = data.split_at(data.len() - 16);
        let (p, t) = self.crypt(nonce, aad, c, false);
        if ct_eq(&t, tag) {
            Some(p)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_tls::util::{hex, unhex};

    #[test]
    fn rfc7253_vectors() {
        let k = unhex("000102030405060708090A0B0C0D0E0F");
        let o = Ocb::new(&k);
        let cases = [
            ("BBAA99887766554433221100", "", "", "785407BFFFC8AD9EDCC5520AC9111EE6"),
            ("BBAA99887766554433221101", "0001020304050607", "0001020304050607", "6820B3657B6F615A5725BDA0D3B4EB3A257C9AF1F8F03009"),
            ("BBAA99887766554433221102", "0001020304050607", "", "81017F8203F081277152FADE694A0A00"),
            ("BBAA99887766554433221103", "", "0001020304050607", "45DD69F8F5AAE72414054CD1F35D82760B2CD00D2F99BFA9"),
            (
                "BBAA99887766554433221104",
                "000102030405060708090A0B0C0D0E0F",
                "000102030405060708090A0B0C0D0E0F",
                "571D535B60B277188BE5147170A9A22C3AD7A4FF3835B8C5701C1CCEC8FC3358",
            ),
        ];
        for (n, a, p, c) in cases {
            let out = o.seal(&unhex(n), &unhex(a), &unhex(p));
            assert_eq!(hex(&out).to_uppercase(), c, "{}", n);
            assert_eq!(o.open(&unhex(n), &unhex(a), &out).unwrap(), unhex(p));
        }
    }
}
