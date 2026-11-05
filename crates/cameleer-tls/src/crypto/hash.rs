//! SHA-1 (legacy only), SHA-256, SHA-384, SHA-512 (FIPS 180-4), HMAC (RFC 2104)
//! and HKDF (RFC 5869).

/// A streaming hash function.
pub trait Digest: Clone {
    const BLOCK_LEN: usize;
    const OUT_LEN: usize;
    fn new() -> Self;
    fn update(&mut self, data: &[u8]);
    fn finish(self) -> Vec<u8>;
    fn digest(data: &[u8]) -> Vec<u8> {
        let mut h = Self::new();
        h.update(data);
        h.finish()
    }
}

// ---------------------------------------------------------------- SHA-256

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
pub struct Sha256 {
    state: [u32; 8],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Sha256 {
    fn compress(state: &mut [u32; 8], block: &[u8]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

impl Digest for Sha256 {
    const BLOCK_LEN: usize = 64;
    const OUT_LEN: usize = 32;
    fn new() -> Self {
        Sha256 {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            buf: [0; 64],
            buf_len: 0,
            total: 0,
        }
    }
    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let n = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len == 64 {
                let b = self.buf;
                Self::compress(&mut self.state, &b);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            Self::compress(&mut self.state, &data[..64]);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }
    fn finish(mut self) -> Vec<u8> {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf_len + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        self.update(&pad);
        self.update(&bits.to_be_bytes());
        self.state.iter().flat_map(|w| w.to_be_bytes()).collect()
    }
}

// ---------------------------------------------------------------- SHA-512 / SHA-384

const K512: [u64; 80] = [
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc, 0x3956c25bf348b538,
    0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118, 0xd807aa98a3030242, 0x12835b0145706fbe,
    0x243185be4ee4b28c, 0x550c7dc3d5ffb4e2, 0x72be5d74f27b896f, 0x80deb1fe3b1696b1, 0x9bdc06a725c71235,
    0xc19bf174cf692694, 0xe49b69c19ef14ad2, 0xefbe4786384f25e3, 0x0fc19dc68b8cd5b5, 0x240ca1cc77ac9c65,
    0x2de92c6f592b0275, 0x4a7484aa6ea6e483, 0x5cb0a9dcbd41fbd4, 0x76f988da831153b5, 0x983e5152ee66dfab,
    0xa831c66d2db43210, 0xb00327c898fb213f, 0xbf597fc7beef0ee4, 0xc6e00bf33da88fc2, 0xd5a79147930aa725,
    0x06ca6351e003826f, 0x142929670a0e6e70, 0x27b70a8546d22ffc, 0x2e1b21385c26c926, 0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df, 0x650a73548baf63de, 0x766a0abb3c77b2a8, 0x81c2c92e47edaee6, 0x92722c851482353b,
    0xa2bfe8a14cf10364, 0xa81a664bbc423001, 0xc24b8b70d0f89791, 0xc76c51a30654be30, 0xd192e819d6ef5218,
    0xd69906245565a910, 0xf40e35855771202a, 0x106aa07032bbd1b8, 0x19a4c116b8d2d0c8, 0x1e376c085141ab53,
    0x2748774cdf8eeb99, 0x34b0bcb5e19b48a8, 0x391c0cb3c5c95a63, 0x4ed8aa4ae3418acb, 0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3, 0x748f82ee5defb2fc, 0x78a5636f43172f60, 0x84c87814a1f0ab72, 0x8cc702081a6439ec,
    0x90befffa23631e28, 0xa4506cebde82bde9, 0xbef9a3f7b2c67915, 0xc67178f2e372532b, 0xca273eceea26619c,
    0xd186b8c721c0c207, 0xeada7dd6cde0eb1e, 0xf57d4f7fee6ed178, 0x06f067aa72176fba, 0x0a637dc5a2c898a6,
    0x113f9804bef90dae, 0x1b710b35131c471b, 0x28db77f523047d84, 0x32caab7b40c72493, 0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c, 0x4cc5d4becb3e42b6, 0x597f299cfc657e2a, 0x5fcb6fab3ad6faec, 0x6c44198c4a475817,
];

#[derive(Clone)]
struct Sha512Core {
    state: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    total: u128,
}

impl Sha512Core {
    fn with_iv(iv: [u64; 8]) -> Self {
        Sha512Core { state: iv, buf: [0; 128], buf_len: 0, total: 0 }
    }
    fn compress(state: &mut [u64; 8], block: &[u8]) {
        let mut w = [0u64; 80];
        for i in 0..16 {
            let mut b = [0u8; 8];
            b.copy_from_slice(&block[8 * i..8 * i + 8]);
            w[i] = u64::from_be_bytes(b);
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K512[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u128);
        if self.buf_len > 0 {
            let n = (128 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len == 128 {
                let b = self.buf;
                Self::compress(&mut self.state, &b);
                self.buf_len = 0;
            }
        }
        while data.len() >= 128 {
            Self::compress(&mut self.state, &data[..128]);
            data = &data[128..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }
    fn finish(mut self, out_words: usize) -> Vec<u8> {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf_len + pad.len()) % 128 != 112 {
            pad.push(0);
        }
        self.update(&pad);
        self.update(&bits.to_be_bytes());
        self.state[..out_words].iter().flat_map(|w| w.to_be_bytes()).collect()
    }
}

#[derive(Clone)]
pub struct Sha512(Sha512Core);
#[derive(Clone)]
pub struct Sha384(Sha512Core);

impl Digest for Sha512 {
    const BLOCK_LEN: usize = 128;
    const OUT_LEN: usize = 64;
    fn new() -> Self {
        Sha512(Sha512Core::with_iv([
            0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1, 0x510e527fade682d1,
            0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
        ]))
    }
    fn update(&mut self, data: &[u8]) {
        self.0.update(data)
    }
    fn finish(self) -> Vec<u8> {
        self.0.finish(8)
    }
}

impl Digest for Sha384 {
    const BLOCK_LEN: usize = 128;
    const OUT_LEN: usize = 48;
    fn new() -> Self {
        Sha384(Sha512Core::with_iv([
            0xcbbb9d5dc1059ed8, 0x629a292a367cd507, 0x9159015a3070dd17, 0x152fecd8f70e5939, 0x67332667ffc00b31,
            0x8eb44a8768581511, 0xdb0c2e0d64f98fa7, 0x47b5481dbefa4fa4,
        ]))
    }
    fn update(&mut self, data: &[u8]) {
        self.0.update(data)
    }
    fn finish(self) -> Vec<u8> {
        self.0.finish(6)
    }
}

// ---------------------------------------------------------------- SHA-1

/// SHA-1. Only used to verify legacy certificate signatures; never for anything new.
#[derive(Clone)]
pub struct Sha1 {
    state: [u32; 5],
    buf: [u8; 64],
    buf_len: usize,
    total: u64,
}

impl Sha1 {
    fn compress(state: &mut [u32; 5], block: &[u8]) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([block[4 * i], block[4 * i + 1], block[4 * i + 2], block[4 * i + 3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = *state;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5a827999),
                20..=39 => (b ^ c ^ d, 0x6ed9eba1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (s, v) in state.iter_mut().zip([a, b, c, d, e]) {
            *s = s.wrapping_add(v);
        }
    }
}

impl Digest for Sha1 {
    const BLOCK_LEN: usize = 64;
    const OUT_LEN: usize = 20;
    fn new() -> Self {
        Sha1 { state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0], buf: [0; 64], buf_len: 0, total: 0 }
    }
    fn update(&mut self, mut data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        if self.buf_len > 0 {
            let n = (64 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len == 64 {
                let b = self.buf;
                Self::compress(&mut self.state, &b);
                self.buf_len = 0;
            }
        }
        while data.len() >= 64 {
            Self::compress(&mut self.state, &data[..64]);
            data = &data[64..];
        }
        if !data.is_empty() {
            self.buf[..data.len()].copy_from_slice(data);
            self.buf_len = data.len();
        }
    }
    fn finish(mut self) -> Vec<u8> {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf_len + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        self.update(&pad);
        self.update(&bits.to_be_bytes());
        self.state.iter().flat_map(|w| w.to_be_bytes()).collect()
    }
}

// ---------------------------------------------------------------- HMAC / HKDF (generic)

pub fn hmac<D: Digest>(key: &[u8], data: &[u8]) -> Vec<u8> {
    hmac_parts::<D>(key, &[data])
}

pub fn hmac_parts<D: Digest>(key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
    let mut k = if key.len() > D::BLOCK_LEN { D::digest(key) } else { key.to_vec() };
    k.resize(D::BLOCK_LEN, 0);
    let mut inner = D::new();
    inner.update(&k.iter().map(|b| b ^ 0x36).collect::<Vec<_>>());
    for p in parts {
        inner.update(p);
    }
    let ih = inner.finish();
    let mut outer = D::new();
    outer.update(&k.iter().map(|b| b ^ 0x5c).collect::<Vec<_>>());
    outer.update(&ih);
    outer.finish()
}

// ---------------------------------------------------------------- runtime-selected hash

/// Hash algorithm selected at runtime (TLS cipher suites, signature schemes, certificates).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlg {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl HashAlg {
    pub fn output_len(self) -> usize {
        match self {
            HashAlg::Sha1 => 20,
            HashAlg::Sha256 => 32,
            HashAlg::Sha384 => 48,
            HashAlg::Sha512 => 64,
        }
    }
    pub fn hash(self, data: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Sha1 => Sha1::digest(data),
            HashAlg::Sha256 => Sha256::digest(data),
            HashAlg::Sha384 => Sha384::digest(data),
            HashAlg::Sha512 => Sha512::digest(data),
        }
    }
    pub fn hmac(self, key: &[u8], data: &[u8]) -> Vec<u8> {
        self.hmac_parts(key, &[data])
    }
    pub fn hmac_parts(self, key: &[u8], parts: &[&[u8]]) -> Vec<u8> {
        match self {
            HashAlg::Sha1 => hmac_parts::<Sha1>(key, parts),
            HashAlg::Sha256 => hmac_parts::<Sha256>(key, parts),
            HashAlg::Sha384 => hmac_parts::<Sha384>(key, parts),
            HashAlg::Sha512 => hmac_parts::<Sha512>(key, parts),
        }
    }
    /// HKDF-Extract (RFC 5869 section 2.2). An empty salt means HashLen zero bytes.
    pub fn hkdf_extract(self, salt: &[u8], ikm: &[u8]) -> Vec<u8> {
        let zeros;
        let salt = if salt.is_empty() {
            zeros = vec![0u8; self.output_len()];
            &zeros[..]
        } else {
            salt
        };
        self.hmac(salt, ikm)
    }
    /// HKDF-Expand (RFC 5869 section 2.3).
    pub fn hkdf_expand(self, prk: &[u8], info: &[u8], len: usize) -> Vec<u8> {
        assert!(len <= 255 * self.output_len(), "hkdf length too large");
        let mut out = Vec::with_capacity(len);
        let mut t: Vec<u8> = Vec::new();
        let mut counter = 1u8;
        while out.len() < len {
            t = self.hmac_parts(prk, &[&t, info, &[counter]]);
            let take = (len - out.len()).min(t.len());
            out.extend_from_slice(&t[..take]);
            counter = counter.wrapping_add(1);
        }
        out
    }
    /// TLS 1.2 PRF (RFC 5246 section 5) P_hash construction.
    pub fn tls12_prf(self, secret: &[u8], label: &[u8], seed: &[u8], len: usize) -> Vec<u8> {
        let mut ls = label.to_vec();
        ls.extend_from_slice(seed);
        let mut out = Vec::with_capacity(len);
        let mut a = self.hmac(secret, &ls);
        while out.len() < len {
            let block = self.hmac_parts(secret, &[&a, &ls]);
            let take = (len - out.len()).min(block.len());
            out.extend_from_slice(&block[..take]);
            a = self.hmac(secret, &a);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    #[test]
    fn sha256_vectors() {
        // FIPS 180-4 examples / NIST CAVP SHA256ShortMsg
        assert_eq!(hex(&Sha256::digest(b"")), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(hex(&Sha256::digest(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            hex(&Sha256::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(hex(&Sha256::digest(&million_a)), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // incremental update across block boundaries
        let mut h = Sha256::new();
        for chunk in million_a.chunks(37) {
            h.update(chunk);
        }
        assert_eq!(hex(&h.finish()), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // CAVP SHA256ShortMsg Len=8 Msg=d3
        assert_eq!(hex(&Sha256::digest(&unhex("d3"))), "28969cdfa74a12c82f3bad960b0b000aca2ac329deea5c2328ebc6f2ba9802c1");
    }

    #[test]
    fn sha384_sha512_vectors() {
        assert_eq!(
            hex(&Sha384::digest(b"abc")),
            "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
        );
        assert_eq!(
            hex(&Sha384::digest(b"")),
            "38b060a751ac96384cd9327eb1b1e36a21fdb71114be07434c0cc7bf63f6e1da274edebfe76f65fbd51ad2f14898b95b"
        );
        assert_eq!(
            hex(&Sha384::digest(b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu")),
            "09330c33f71147e83d192fc782cd1b4753111b173b3b05d22fa08086e3b0f712fcc7c71a557e2db966c3e9fa91746039"
        );
        assert_eq!(
            hex(&Sha512::digest(b"abc")),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
    }

    #[test]
    fn sha1_vectors() {
        assert_eq!(hex(&Sha1::digest(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(&Sha1::digest(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(
            hex(&Sha1::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn hmac_rfc4231() {
        // Test case 1
        let key = vec![0x0b; 20];
        assert_eq!(
            hex(&hmac::<Sha256>(&key, b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
        assert_eq!(
            hex(&hmac::<Sha384>(&key, b"Hi There")),
            "afd03944d84895626b0825f4ab46907f15f9dadbe4101ec682aa034c7cebc59cfaea9ea9076ede7f4af152e8b2fa9cb6"
        );
        // Test case 2
        assert_eq!(
            hex(&hmac::<Sha256>(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // Test case 6: key larger than block size
        let key = vec![0xaa; 131];
        assert_eq!(
            hex(&hmac::<Sha256>(&key, b"Test Using Larger Than Block-Size Key - Hash Key First")),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        // RFC 2202 HMAC-SHA1 test case 2
        assert_eq!(hex(&hmac::<Sha1>(b"Jefe", b"what do ya want for nothing?")), "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79");
    }

    #[test]
    fn hkdf_rfc5869() {
        // Test case 1
        let ikm = vec![0x0b; 22];
        let salt = unhex("000102030405060708090a0b0c");
        let info = unhex("f0f1f2f3f4f5f6f7f8f9");
        let prk = HashAlg::Sha256.hkdf_extract(&salt, &ikm);
        assert_eq!(hex(&prk), "077709362c2e32df0ddc3f0dc47bba6390b6c73bb50f9c3122ec844ad7c2b3e5");
        let okm = HashAlg::Sha256.hkdf_expand(&prk, &info, 42);
        assert_eq!(
            hex(&okm),
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
        );
        // Test case 3: zero-length salt and info
        let prk = HashAlg::Sha256.hkdf_extract(&[], &ikm);
        assert_eq!(hex(&prk), "19ef24a32c717b167f33a91d6f648bdf96596776afdb6377ac434c1c293ccb04");
        let okm = HashAlg::Sha256.hkdf_expand(&prk, &[], 42);
        assert_eq!(
            hex(&okm),
            "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
        );
    }
}
