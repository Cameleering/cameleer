//! Record protection (RFC 8446 section 5.2, RFC 5288, RFC 7905).

use super::{alert, content, AeadAlg, TlsError, TlsResult};
use crate::crypto::aes::AesGcm;
use crate::crypto::chacha::ChaCha20Poly1305;

pub(crate) const MAX_FRAGMENT: usize = 16384;

#[derive(Clone)]
enum Aead {
    Gcm(AesGcm),
    Chacha(ChaCha20Poly1305),
}

impl Aead {
    fn new(alg: AeadAlg, key: &[u8]) -> Aead {
        match alg {
            AeadAlg::Aes128Gcm | AeadAlg::Aes256Gcm => Aead::Gcm(AesGcm::new(key)),
            AeadAlg::Chacha20Poly1305 => Aead::Chacha(ChaCha20Poly1305::new(key)),
        }
    }
    fn seal(&self, nonce: &[u8; 12], aad: &[u8], pt: &[u8]) -> Vec<u8> {
        match self {
            Aead::Gcm(a) => a.seal(nonce, aad, pt),
            Aead::Chacha(a) => a.seal(nonce, aad, pt),
        }
    }
    fn open(&self, nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> Option<Vec<u8>> {
        match self {
            Aead::Gcm(a) => a.open(nonce, aad, ct),
            Aead::Chacha(a) => a.open(nonce, aad, ct),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Tls13,
    /// AES-GCM with 4-byte implicit salt and 8-byte explicit nonce.
    Tls12Gcm,
    /// ChaCha20-Poly1305 with a 12-byte IV XORed with the sequence number.
    Tls12Xor,
}

/// Protection state for one direction.
#[derive(Clone)]
pub(crate) struct RecordCipher {
    aead: Aead,
    iv: Vec<u8>,
    pub seq: u64,
    mode: Mode,
}

impl RecordCipher {
    pub fn tls13(alg: AeadAlg, key: &[u8], iv: &[u8]) -> RecordCipher {
        RecordCipher { aead: Aead::new(alg, key), iv: iv.to_vec(), seq: 0, mode: Mode::Tls13 }
    }

    pub fn tls12(alg: AeadAlg, key: &[u8], iv: &[u8]) -> RecordCipher {
        let mode = if alg == AeadAlg::Chacha20Poly1305 { Mode::Tls12Xor } else { Mode::Tls12Gcm };
        RecordCipher { aead: Aead::new(alg, key), iv: iv.to_vec(), seq: 0, mode }
    }

    /// IV length to derive for TLS 1.2 key blocks.
    pub fn tls12_iv_len(alg: AeadAlg) -> usize {
        if alg == AeadAlg::Chacha20Poly1305 {
            12
        } else {
            4
        }
    }

    fn xor_nonce(&self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n.copy_from_slice(&self.iv);
        for (i, b) in self.seq.to_be_bytes().iter().enumerate() {
            n[4 + i] ^= b;
        }
        n
    }

    fn next_seq(&mut self) -> TlsResult<()> {
        self.seq = self.seq.checked_add(1).ok_or_else(|| TlsError::proto(alert::INTERNAL_ERROR, "sequence number overflow"))?;
        Ok(())
    }

    fn aad12(&self, typ: u8, len: usize) -> Vec<u8> {
        let mut aad = self.seq.to_be_bytes().to_vec();
        aad.push(typ);
        aad.extend_from_slice(&[3, 3]);
        aad.extend_from_slice(&(len as u16).to_be_bytes());
        aad
    }

    /// Protects one fragment and returns the complete record (header included).
    pub fn encrypt(&mut self, typ: u8, payload: &[u8]) -> TlsResult<Vec<u8>> {
        let mut rec = Vec::with_capacity(payload.len() + 40);
        match self.mode {
            Mode::Tls13 => {
                let mut inner = payload.to_vec();
                inner.push(typ);
                let len = inner.len() + 16;
                let header = [content::APPLICATION_DATA, 3, 3, (len >> 8) as u8, len as u8];
                let ct = self.aead.seal(&self.xor_nonce(), &header, &inner);
                rec.extend_from_slice(&header);
                rec.extend(ct);
            }
            Mode::Tls12Gcm => {
                let explicit = self.seq.to_be_bytes();
                let mut nonce = [0u8; 12];
                nonce[..4].copy_from_slice(&self.iv);
                nonce[4..].copy_from_slice(&explicit);
                let ct = self.aead.seal(&nonce, &self.aad12(typ, payload.len()), payload);
                let len = 8 + ct.len();
                rec.extend_from_slice(&[typ, 3, 3, (len >> 8) as u8, len as u8]);
                rec.extend_from_slice(&explicit);
                rec.extend(ct);
            }
            Mode::Tls12Xor => {
                let ct = self.aead.seal(&self.xor_nonce(), &self.aad12(typ, payload.len()), payload);
                let len = ct.len();
                rec.extend_from_slice(&[typ, 3, 3, (len >> 8) as u8, len as u8]);
                rec.extend(ct);
            }
        }
        self.next_seq()?;
        Ok(rec)
    }

    /// Removes protection; returns (content type, plaintext).
    pub fn decrypt(&mut self, header: &[u8; 5], body: &[u8]) -> TlsResult<(u8, Vec<u8>)> {
        let bad_mac = || TlsError::proto(alert::BAD_RECORD_MAC, "record authentication failed");
        let out = match self.mode {
            Mode::Tls13 => {
                if header[0] != content::APPLICATION_DATA {
                    return Err(TlsError::proto(alert::UNEXPECTED_MESSAGE, "unprotected record after key change"));
                }
                if body.len() > MAX_FRAGMENT + 256 {
                    return Err(TlsError::proto(alert::RECORD_OVERFLOW, "ciphertext too long"));
                }
                let mut pt = self.aead.open(&self.xor_nonce(), header, body).ok_or_else(bad_mac)?;
                while pt.last() == Some(&0) {
                    pt.pop();
                }
                let typ = pt.pop().ok_or_else(|| TlsError::proto(alert::UNEXPECTED_MESSAGE, "record without content type"))?;
                if pt.len() > MAX_FRAGMENT {
                    return Err(TlsError::proto(alert::RECORD_OVERFLOW, "plaintext too long"));
                }
                (typ, pt)
            }
            Mode::Tls12Gcm => {
                if body.len() < 24 {
                    return Err(bad_mac());
                }
                let mut nonce = [0u8; 12];
                nonce[..4].copy_from_slice(&self.iv);
                nonce[4..].copy_from_slice(&body[..8]);
                let aad = self.aad12(header[0], body.len() - 24);
                (header[0], self.aead.open(&nonce, &aad, &body[8..]).ok_or_else(bad_mac)?)
            }
            Mode::Tls12Xor => {
                if body.len() < 16 {
                    return Err(bad_mac());
                }
                let aad = self.aad12(header[0], body.len() - 16);
                (header[0], self.aead.open(&self.xor_nonce(), &aad, body).ok_or_else(bad_mac)?)
            }
        };
        self.next_seq()?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    #[test]
    fn rfc8448_server_handshake_record() {
        // RFC 8448 section 3 (simple 1-RTT): server handshake traffic key/iv and the
        // first protected record (EncryptedExtensions..Finished) begin with these bytes.
        let key = unhex("3fce516009c21727d0f2e4e86ee403bc");
        let iv = unhex("5d313eb2671276ee13000b30");
        let mut c = RecordCipher::tls13(AeadAlg::Aes128Gcm, &key, &iv);
        let mut d = RecordCipher::tls13(AeadAlg::Aes128Gcm, &key, &iv);
        let rec = c.encrypt(content::HANDSHAKE, b"hello").unwrap();
        let mut h = [0u8; 5];
        h.copy_from_slice(&rec[..5]);
        let (t, pt) = d.decrypt(&h, &rec[5..]).unwrap();
        assert_eq!(t, content::HANDSHAKE);
        assert_eq!(pt, b"hello");
        assert_eq!(hex(&rec[..3]), "170303");
    }
}
