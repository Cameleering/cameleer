//! ChaCha20, Poly1305 and the ChaCha20-Poly1305 AEAD (RFC 8439). Constant-time.

use crate::util::ct_eq;

#[inline(always)]
fn qr(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

/// One ChaCha20 block (RFC 8439 section 2.3).
pub fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut st = [0u32; 16];
    st[0] = 0x61707865;
    st[1] = 0x3320646e;
    st[2] = 0x79622d32;
    st[3] = 0x6b206574;
    for i in 0..8 {
        st[4 + i] = u32::from_le_bytes([key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]]);
    }
    st[12] = counter;
    for i in 0..3 {
        st[13 + i] = u32::from_le_bytes([nonce[4 * i], nonce[4 * i + 1], nonce[4 * i + 2], nonce[4 * i + 3]]);
    }
    let mut w = st;
    for _ in 0..10 {
        qr(&mut w, 0, 4, 8, 12);
        qr(&mut w, 1, 5, 9, 13);
        qr(&mut w, 2, 6, 10, 14);
        qr(&mut w, 3, 7, 11, 15);
        qr(&mut w, 0, 5, 10, 15);
        qr(&mut w, 1, 6, 11, 12);
        qr(&mut w, 2, 7, 8, 13);
        qr(&mut w, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[4 * i..4 * i + 4].copy_from_slice(&w[i].wrapping_add(st[i]).to_le_bytes());
    }
    out
}

/// XORs the ChaCha20 keystream (starting at block `counter`) into `data`.
pub fn chacha20_xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = chacha20_block(key, counter.wrapping_add(i as u32), nonce);
        for (d, k) in chunk.iter_mut().zip(ks.iter()) {
            *d ^= k;
        }
    }
}

/// Poly1305 one-time authenticator (26-bit limb implementation).
pub struct Poly1305 {
    r: [u64; 5],
    s: [u64; 4],
    h: [u64; 5],
    pad: u128,
    buf: [u8; 16],
    buf_len: usize,
}

const M26: u64 = 0x3ff_ffff;

impl Poly1305 {
    pub fn new(key: &[u8; 32]) -> Poly1305 {
        let mut rb = [0u8; 16];
        rb.copy_from_slice(&key[..16]);
        let r = u128::from_le_bytes(rb) & 0x0ffffffc0ffffffc0ffffffc0fffffff;
        let mut pb = [0u8; 16];
        pb.copy_from_slice(&key[16..]);
        let rl = [
            (r as u64) & M26,
            ((r >> 26) as u64) & M26,
            ((r >> 52) as u64) & M26,
            ((r >> 78) as u64) & M26,
            ((r >> 104) as u64) & M26,
        ];
        Poly1305 {
            r: rl,
            s: [rl[1] * 5, rl[2] * 5, rl[3] * 5, rl[4] * 5],
            h: [0; 5],
            pad: u128::from_le_bytes(pb),
            buf: [0; 16],
            buf_len: 0,
        }
    }

    fn block(&mut self, m: u128, hibit: u64) {
        let [r0, r1, r2, r3, r4] = self.r;
        let [s1, s2, s3, s4] = self.s;
        let h = &mut self.h;
        h[0] += (m as u64) & M26;
        h[1] += ((m >> 26) as u64) & M26;
        h[2] += ((m >> 52) as u64) & M26;
        h[3] += ((m >> 78) as u64) & M26;
        h[4] += ((m >> 104) as u64) | (hibit << 24);
        let [h0, h1, h2, h3, h4] = *h;
        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;
        let mut c = d0 >> 26;
        h[0] = d0 & M26;
        d1 += c;
        c = d1 >> 26;
        h[1] = d1 & M26;
        d2 += c;
        c = d2 >> 26;
        h[2] = d2 & M26;
        d3 += c;
        c = d3 >> 26;
        h[3] = d3 & M26;
        d4 += c;
        c = d4 >> 26;
        h[4] = d4 & M26;
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= M26;
        h[1] += c;
    }

    pub fn update(&mut self, mut data: &[u8]) {
        if self.buf_len > 0 {
            let n = (16 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len == 16 {
                let m = u128::from_le_bytes(self.buf);
                self.block(m, 1);
                self.buf_len = 0;
            }
        }
        while data.len() >= 16 {
            let mut b = [0u8; 16];
            b.copy_from_slice(&data[..16]);
            self.block(u128::from_le_bytes(b), 1);
            data = &data[16..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }

    pub fn finish(mut self) -> [u8; 16] {
        if self.buf_len > 0 {
            let mut b = [0u8; 16];
            b[..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
            b[self.buf_len] = 1;
            self.block(u128::from_le_bytes(b), 0);
        }
        let mut h = self.h;
        // full carry
        let mut c = h[1] >> 26;
        h[1] &= M26;
        h[2] += c;
        c = h[2] >> 26;
        h[2] &= M26;
        h[3] += c;
        c = h[3] >> 26;
        h[3] &= M26;
        h[4] += c;
        c = h[4] >> 26;
        h[4] &= M26;
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= M26;
        h[1] += c;
        // g = h + 5 - 2^130
        let mut g = [0u64; 5];
        g[0] = h[0] + 5;
        c = g[0] >> 26;
        g[0] &= M26;
        g[1] = h[1] + c;
        c = g[1] >> 26;
        g[1] &= M26;
        g[2] = h[2] + c;
        c = g[2] >> 26;
        g[2] &= M26;
        g[3] = h[3] + c;
        c = g[3] >> 26;
        g[3] &= M26;
        g[4] = (h[4] + c).wrapping_sub(1 << 26);
        // if g4 did not underflow (top bit clear) then h >= p: use g
        let use_g = ((g[4] >> 63) ^ 1).wrapping_neg();
        for i in 0..5 {
            h[i] = (h[i] & !use_g) | (g[i] & use_g);
        }
        let hv = (h[0] as u128) | ((h[1] as u128) << 26) | ((h[2] as u128) << 52) | ((h[3] as u128) << 78) | ((h[4] as u128) << 104);
        hv.wrapping_add(self.pad).to_le_bytes()
    }
}

pub fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    let mut p = Poly1305::new(key);
    p.update(msg);
    p.finish()
}

#[derive(Clone)]
pub struct ChaCha20Poly1305 {
    key: [u8; 32],
}

impl ChaCha20Poly1305 {
    pub fn new(key: &[u8]) -> ChaCha20Poly1305 {
        let mut k = [0u8; 32];
        k.copy_from_slice(key);
        ChaCha20Poly1305 { key: k }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
        let block0 = chacha20_block(&self.key, 0, nonce);
        let mut otk = [0u8; 32];
        otk.copy_from_slice(&block0[..32]);
        let mut p = Poly1305::new(&otk);
        let zeros = [0u8; 16];
        p.update(aad);
        p.update(&zeros[..(16 - aad.len() % 16) % 16]);
        p.update(ct);
        p.update(&zeros[..(16 - ct.len() % 16) % 16]);
        p.update(&(aad.len() as u64).to_le_bytes());
        p.update(&(ct.len() as u64).to_le_bytes());
        p.finish()
    }

    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let mut out = plaintext.to_vec();
        chacha20_xor(&self.key, 1, nonce, &mut out);
        let tag = self.tag(nonce, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 16 {
            return None;
        }
        let (ct, tag) = data.split_at(data.len() - 16);
        if !ct_eq(&self.tag(nonce, aad, ct), tag) {
            return None;
        }
        let mut out = ct.to_vec();
        chacha20_xor(&self.key, 1, nonce, &mut out);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    fn k32(s: &str) -> [u8; 32] {
        let mut k = [0u8; 32];
        k.copy_from_slice(&unhex(s));
        k
    }
    fn n12(s: &str) -> [u8; 12] {
        let mut k = [0u8; 12];
        k.copy_from_slice(&unhex(s));
        k
    }

    #[test]
    fn rfc8439_block_function() {
        // 2.3.2
        let key = k32("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let out = chacha20_block(&key, 1, &n12("000000090000004a00000000"));
        assert_eq!(
            hex(&out),
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e"
        );
    }

    #[test]
    fn rfc8439_encryption() {
        // 2.4.2
        let key = k32("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f");
        let mut data = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.".to_vec();
        chacha20_xor(&key, 1, &n12("000000000000004a00000000"), &mut data);
        assert_eq!(
            hex(&data),
            "6e2e359a2568f98041ba0728dd0d6981e97e7aec1d4360c20a27afccfd9fae0bf91b65c5524733ab8f593dabcd62b3571639d624e65152ab8f530c359f0861d807ca0dbf500d6a6156a38e088a22b65e52bc514d16ccf806818ce91ab77937365af90bbf74a35be6b40b8eedf2785e42874d"
        );
    }

    #[test]
    fn rfc8439_poly1305() {
        // 2.5.2
        let key = k32("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b");
        assert_eq!(hex(&poly1305(&key, b"Cryptographic Forum Research Group")), "a8061dc1305136c6c22b8baf0c0127a9");
        // A.3 test vector #1 (all zero)
        assert_eq!(hex(&poly1305(&[0u8; 32], &[0u8; 64])), "00000000000000000000000000000000");
        // A.3 test vector #11-ish edge: h reaching p (vector #6: R=2, S=0, msg = ff*16 -> tag 03 00..)
        let mut key = [0u8; 32];
        key[0] = 2;
        assert_eq!(hex(&poly1305(&key, &[0xff; 16])), "03000000000000000000000000000000");
        // vector #7: R=1, msg = ff..ff f0ff..ff 11 00.. -> tag 05 00..
        let mut key = [0u8; 32];
        key[0] = 1;
        let msg = unhex("ffffffffffffffffffffffffffffffff f0ffffffffffffffffffffffffffffff 11000000000000000000000000000000");
        assert_eq!(hex(&poly1305(&key, &msg)), "05000000000000000000000000000000");
    }

    #[test]
    fn rfc8439_aead() {
        // 2.8.2
        let key = unhex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f");
        let nonce = n12("070000004041424344454647");
        let aad = unhex("50515253c0c1c2c3c4c5c6c7");
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let out = ChaCha20Poly1305::new(&key).seal(&nonce, &aad, pt);
        assert_eq!(
            hex(&out),
            "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116\
1ae10b594f09e26a7e902ecbd0600691"
        );
        assert_eq!(ChaCha20Poly1305::new(&key).open(&nonce, &aad, &out).unwrap(), pt.to_vec());
        let mut bad = out.clone();
        bad[0] ^= 1;
        assert!(ChaCha20Poly1305::new(&key).open(&nonce, &aad, &bad).is_none());
    }
}
