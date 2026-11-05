//! MD5 (RFC 1321). Only used by the JCEKS `PBEWithMD5AndTripleDES` key protector;
//! never for anything security relevant that is new.

use super::hash::Digest;

#[derive(Clone)]
pub struct Md5 {
    state: [u32; 4],
    buf: Vec<u8>,
    total: u64,
}

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11,
    16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

fn k(i: usize) -> u32 {
    ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32
}

impl Md5 {
    fn compress(state: &mut [u32; 4], block: &[u8]) {
        let m: Vec<u32> = block.chunks(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        let [mut a, mut b, mut c, mut d] = *state;
        for (i, shift) in S.iter().enumerate() {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let t = d;
            d = c;
            c = b;
            b = b.wrapping_add(a.wrapping_add(f).wrapping_add(k(i)).wrapping_add(m[g]).rotate_left(*shift));
            a = t;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
    }
}

impl Digest for Md5 {
    const BLOCK_LEN: usize = 64;
    const OUT_LEN: usize = 16;
    fn new() -> Self {
        Md5 { state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476], buf: Vec::new(), total: 0 }
    }
    fn update(&mut self, data: &[u8]) {
        self.total = self.total.wrapping_add(data.len() as u64);
        self.buf.extend_from_slice(data);
        let full = self.buf.len() / 64 * 64;
        for i in (0..full).step_by(64) {
            let block: Vec<u8> = self.buf[i..i + 64].to_vec();
            Md5::compress(&mut self.state, &block);
        }
        self.buf.drain(..full);
    }
    fn finish(mut self) -> Vec<u8> {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        while (self.buf.len() + pad.len()) % 64 != 56 {
            pad.push(0);
        }
        pad.extend_from_slice(&bits.to_le_bytes());
        let total = self.total;
        self.update(&pad);
        self.total = total;
        self.state.iter().flat_map(|w| w.to_le_bytes()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::hex;

    #[test]
    fn rfc1321_vectors() {
        assert_eq!(hex(&Md5::digest(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(&Md5::digest(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex(&Md5::digest(b"12345678901234567890123456789012345678901234567890123456789012345678901234567890")),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }
}
