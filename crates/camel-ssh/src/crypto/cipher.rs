//! Symmetric packet protection building blocks: AES-CTR (NIST SP 800-38A, as
//! used by RFC 4344), the OpenSSH `chacha20-poly1305@openssh.com` construction
//! (PROTOCOL.chacha20poly1305) and HMAC-SHA2 MACs (RFC 6668).

use camel_tls::crypto::aes::Aes;
use camel_tls::crypto::chacha::{chacha20_block, chacha20_xor, Poly1305};
use camel_tls::crypto::hash::HashAlg;
use camel_tls::util::ct_eq;

/// AES in counter mode with a 128-bit big-endian counter. The keystream
/// position persists across calls (SSH packets form one continuous stream).
#[derive(Clone)]
pub struct AesCtr {
    aes: Aes,
    counter: [u8; 16],
    ks: [u8; 16],
    ks_pos: usize,
}

impl AesCtr {
    /// Key of 16, 24 or 32 bytes and a 16-byte initial counter block.
    pub fn new(key: &[u8], iv: &[u8]) -> AesCtr {
        let mut counter = [0u8; 16];
        counter.copy_from_slice(&iv[..16]);
        AesCtr { aes: Aes::new(key), counter, ks: [0; 16], ks_pos: 16 }
    }

    /// XORs the keystream into `data` (encrypts or decrypts).
    pub fn apply(&mut self, data: &mut [u8]) {
        for b in data.iter_mut() {
            if self.ks_pos == 16 {
                self.ks = self.counter;
                self.aes.encrypt_block(&mut self.ks);
                for i in (0..16).rev() {
                    self.counter[i] = self.counter[i].wrapping_add(1);
                    if self.counter[i] != 0 {
                        break;
                    }
                }
                self.ks_pos = 0;
            }
            *b ^= self.ks[self.ks_pos];
            self.ks_pos += 1;
        }
    }
}

/// `chacha20-poly1305@openssh.com`: 64 bytes of key material; the first 32
/// bytes (K_2) protect the payload, the last 32 (K_1) encrypt the length.
#[derive(Clone)]
pub struct ChaChaPolyOpenSsh {
    main: [u8; 32],
    header: [u8; 32],
}

fn nonce(seq: u32) -> [u8; 12] {
    // original ChaCha20 with a 64-bit counter (= 0) and the 64-bit sequence number as nonce
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&(seq as u64).to_be_bytes());
    n
}

impl ChaChaPolyOpenSsh {
    pub fn new(key: &[u8]) -> ChaChaPolyOpenSsh {
        let mut main = [0u8; 32];
        let mut header = [0u8; 32];
        main.copy_from_slice(&key[..32]);
        header.copy_from_slice(&key[32..64]);
        ChaChaPolyOpenSsh { main, header }
    }

    fn poly_key(&self, seq: u32) -> [u8; 32] {
        let block = chacha20_block(&self.main, 0, &nonce(seq));
        let mut k = [0u8; 32];
        k.copy_from_slice(&block[..32]);
        k
    }

    /// Decrypts the 4-byte encrypted packet length.
    pub fn decrypt_length(&self, seq: u32, enc: &[u8]) -> u32 {
        let mut l = [enc[0], enc[1], enc[2], enc[3]];
        chacha20_xor(&self.header, 0, &nonce(seq), &mut l);
        u32::from_be_bytes(l)
    }

    /// Encrypts a whole plaintext packet (4-byte length + body); returns ciphertext || tag.
    pub fn seal(&self, seq: u32, packet: &[u8]) -> Vec<u8> {
        let mut out = packet.to_vec();
        let n = nonce(seq);
        chacha20_xor(&self.header, 0, &n, &mut out[..4]);
        chacha20_xor(&self.main, 1, &n, &mut out[4..]);
        let mut p = Poly1305::new(&self.poly_key(seq));
        p.update(&out);
        let tag = p.finish();
        out.extend_from_slice(&tag);
        out
    }

    /// Verifies and decrypts `enc_len || ciphertext || tag`; returns the plaintext body (without length).
    pub fn open(&self, seq: u32, data: &[u8]) -> Option<Vec<u8>> {
        if data.len() < 4 + 16 {
            return None;
        }
        let (body, tag) = data.split_at(data.len() - 16);
        let mut p = Poly1305::new(&self.poly_key(seq));
        p.update(body);
        if !ct_eq(&p.finish(), tag) {
            return None;
        }
        let mut out = body[4..].to_vec();
        chacha20_xor(&self.main, 1, &nonce(seq), &mut out);
        Some(out)
    }
}

/// HMAC algorithm of a non-AEAD cipher.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MacAlg {
    pub hash: HashAlg,
    /// Encrypt-then-MAC (`*-etm@openssh.com`).
    pub etm: bool,
}

impl MacAlg {
    pub fn from_name(name: &str) -> Option<MacAlg> {
        Some(match name {
            "hmac-sha2-256" => MacAlg { hash: HashAlg::Sha256, etm: false },
            "hmac-sha2-512" => MacAlg { hash: HashAlg::Sha512, etm: false },
            "hmac-sha2-256-etm@openssh.com" => MacAlg { hash: HashAlg::Sha256, etm: true },
            "hmac-sha2-512-etm@openssh.com" => MacAlg { hash: HashAlg::Sha512, etm: true },
            _ => return None,
        })
    }
    /// Key and tag length.
    pub fn tag_len(&self) -> usize {
        self.hash.output_len()
    }
    /// MAC over `uint32 seq || data`.
    pub fn compute(&self, key: &[u8], seq: u32, data: &[&[u8]]) -> Vec<u8> {
        let s = seq.to_be_bytes();
        let mut parts: Vec<&[u8]> = vec![&s];
        parts.extend_from_slice(data);
        self.hash.hmac_parts(key, &parts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camel_tls::util::{hex, unhex};

    #[test]
    fn aes_ctr_nist_sp800_38a() {
        let iv = unhex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let pt = unhex("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e5130c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710");
        // F.5.1 CTR-AES128.Encrypt
        let mut d = pt.clone();
        let mut c = AesCtr::new(&unhex("2b7e151628aed2a6abf7158809cf4f3c"), &iv);
        // split in odd chunks: the keystream must continue across calls
        let (a, b) = d.split_at_mut(13);
        c.apply(a);
        c.apply(b);
        assert_eq!(
            hex(&d),
            "874d6191b620e3261bef6864990db6ce9806f66b7970fdff8617187bb9fffdff5ae4df3edbd5d35e5b4f09020db03eab1e031dda2fbe03d1792170a0f3009cee"
        );
        // F.5.5 CTR-AES256.Encrypt
        let mut d = pt.clone();
        AesCtr::new(&unhex("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4"), &iv).apply(&mut d);
        assert_eq!(
            hex(&d),
            "601ec313775789a5b7a7f504bbf3d228f443e3ca4d62b59aca84e990cacaf5c52b0930daa23de94ce87017ba2d84988ddfc9c58db67aada613c2dd08457941a6"
        );
        // counter wrap-around carries into the higher bytes
        let mut c = AesCtr::new(&[0u8; 16], &[0xffu8; 16]);
        let mut z = [0u8; 32];
        c.apply(&mut z);
        assert_eq!(c.counter, {
            let mut e = [0u8; 16];
            e[15] = 1;
            e
        });
    }

    #[test]
    fn chacha_openssh_roundtrip_and_structure() {
        let key: Vec<u8> = (0..64u8).collect();
        let c = ChaChaPolyOpenSsh::new(&key);
        let mut pkt = vec![0, 0, 0, 12, 4];
        pkt.extend_from_slice(b"\x05abcdefg");
        pkt.extend_from_slice(&[0u8; 0]);
        let sealed = c.seal(7, &pkt);
        assert_eq!(sealed.len(), pkt.len() + 16);
        assert_eq!(c.decrypt_length(7, &sealed), 12);
        assert_eq!(c.open(7, &sealed).unwrap(), pkt[4..].to_vec());
        // wrong sequence number or tampering fails
        assert!(c.open(8, &sealed).is_none());
        let mut bad = sealed.clone();
        bad[6] ^= 1;
        assert!(c.open(7, &bad).is_none());
        // the length is encrypted with K_1 (bytes 32..64) and keystream block 0
        let mut expect_len = [0, 0, 0, 12u8];
        chacha20_xor(&key[32..64].try_into().unwrap(), 0, &nonce(7), &mut expect_len);
        assert_eq!(&sealed[..4], &expect_len);
        // the payload uses K_2 with block counter 1 (Poly1305 key = block 0)
        let mut body = pkt[4..].to_vec();
        chacha20_xor(&key[..32].try_into().unwrap(), 1, &nonce(7), &mut body);
        assert_eq!(&sealed[4..pkt.len()], &body[..]);
    }

    #[test]
    fn hmac_sha256_rfc4231_case2() {
        let m = MacAlg::from_name("hmac-sha2-256").unwrap();
        // RFC 4231 test case 2 through the (seq || data) interface: split "what do ya want for nothing?"
        let data = b"what do ya want for nothing?";
        let seq = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        assert_eq!(
            hex(&m.compute(b"Jefe", seq, &[&data[4..]])),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert!(MacAlg::from_name("hmac-md5").is_none());
    }
}
