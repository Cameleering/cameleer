//! TLS 1.3 key schedule (RFC 8446 section 7) and TLS 1.2 key derivation.

use super::record::RecordCipher;
use super::{codec, hs, CipherSuite};
use crate::crypto::hash::HashAlg;

pub(crate) fn hkdf_expand_label(h: HashAlg, secret: &[u8], label: &str, context: &[u8], len: usize) -> Vec<u8> {
    let mut info = Vec::with_capacity(10 + label.len() + context.len());
    codec::put_u16(&mut info, len as u16);
    let full = format!("tls13 {label}");
    codec::put_vec8(&mut info, full.as_bytes());
    codec::put_vec8(&mut info, context);
    h.hkdf_expand(secret, &info, len)
}

pub(crate) fn derive_secret(h: HashAlg, secret: &[u8], label: &str, transcript_hash: &[u8]) -> Vec<u8> {
    hkdf_expand_label(h, secret, label, transcript_hash, h.output_len())
}

/// Handshake and master secrets for one TLS 1.3 connection.
pub(crate) struct KeySchedule13 {
    pub h: HashAlg,
    #[allow(dead_code)] // inspected by tests
    pub handshake_secret: Vec<u8>,
    pub client_hs: Vec<u8>,
    pub server_hs: Vec<u8>,
    master: Vec<u8>,
}

impl KeySchedule13 {
    /// Derives the handshake traffic secrets from the (EC)DHE shared secret and
    /// the transcript hash of ClientHello..ServerHello.
    pub fn new(suite: CipherSuite, shared: &[u8], hello_hash: &[u8]) -> KeySchedule13 {
        let zeros = vec![0u8; suite.hash().output_len()];
        Self::with_psk(suite.hash(), &zeros, shared, hello_hash)
    }

    /// Key schedule with an optional resumption PSK.
    pub fn new_psk(suite: CipherSuite, psk: Option<&[u8]>, shared: &[u8], hello_hash: &[u8]) -> KeySchedule13 {
        match psk {
            Some(p) => Self::with_psk(suite.hash(), p, shared, hello_hash),
            None => Self::new(suite, shared, hello_hash),
        }
    }

    /// resumption_master_secret from the transcript hash through client Finished.
    pub fn resumption_master(&self, hash: &[u8]) -> Vec<u8> {
        derive_secret(self.h, &self.master, "res master", hash)
    }

    pub(crate) fn with_psk(h: HashAlg, psk: &[u8], shared: &[u8], hello_hash: &[u8]) -> KeySchedule13 {
        let zeros = vec![0u8; h.output_len()];
        let early = h.hkdf_extract(&zeros, psk);
        let derived = derive_secret(h, &early, "derived", &h.hash(&[]));
        let handshake_secret = h.hkdf_extract(&derived, shared);
        let client_hs = derive_secret(h, &handshake_secret, "c hs traffic", hello_hash);
        let server_hs = derive_secret(h, &handshake_secret, "s hs traffic", hello_hash);
        let derived2 = derive_secret(h, &handshake_secret, "derived", &h.hash(&[]));
        let master = h.hkdf_extract(&derived2, &zeros);
        KeySchedule13 { h, handshake_secret, client_hs, server_hs, master }
    }

    /// (client, server) application traffic secrets from the hash through server Finished.
    pub fn app_secrets(&self, hash: &[u8]) -> (Vec<u8>, Vec<u8>) {
        (derive_secret(self.h, &self.master, "c ap traffic", hash), derive_secret(self.h, &self.master, "s ap traffic", hash))
    }

    pub fn finished_mac(&self, base_key: &[u8], hash: &[u8]) -> Vec<u8> {
        let fk = hkdf_expand_label(self.h, base_key, "finished", &[], self.h.output_len());
        self.h.hmac(&fk, hash)
    }
}

/// PSK binder (RFC 8446 section 4.2.11.2) over the hash of the truncated ClientHello transcript.
/// client_early_traffic_secret (RFC 8446 7.1) for 0-RTT data.
pub(crate) fn client_early_secret(h: HashAlg, psk: &[u8], client_hello_hash: &[u8]) -> Vec<u8> {
    let zeros = vec![0u8; h.output_len()];
    derive_secret(h, &h.hkdf_extract(&zeros, psk), "c e traffic", client_hello_hash)
}

pub(crate) fn psk_binder(h: HashAlg, psk: &[u8], transcript_hash: &[u8]) -> Vec<u8> {
    let zeros = vec![0u8; h.output_len()];
    let early = h.hkdf_extract(&zeros, psk);
    let binder_key = derive_secret(h, &early, "res binder", &h.hash(&[]));
    let fk = hkdf_expand_label(h, &binder_key, "finished", &[], h.output_len());
    h.hmac(&fk, transcript_hash)
}

/// The PSK of a NewSessionTicket.
pub(crate) fn ticket_psk(h: HashAlg, resumption_master: &[u8], nonce: &[u8]) -> Vec<u8> {
    hkdf_expand_label(h, resumption_master, "resumption", nonce, h.output_len())
}

/// Record protection from a TLS 1.3 traffic secret.
pub(crate) fn cipher13(suite: CipherSuite, secret: &[u8]) -> RecordCipher {
    let h = suite.hash();
    let key = hkdf_expand_label(h, secret, "key", &[], suite.aead().key_len());
    let iv = hkdf_expand_label(h, secret, "iv", &[], 12);
    RecordCipher::tls13(suite.aead(), &key, &iv)
}

pub(crate) fn next_traffic_secret(h: HashAlg, secret: &[u8]) -> Vec<u8> {
    hkdf_expand_label(h, secret, "traffic upd", &[], h.output_len())
}

/// The synthetic `message_hash` handshake message that replaces ClientHello1 after HRR.
pub(crate) fn message_hash(h: HashAlg, ch1: &[u8]) -> Vec<u8> {
    codec::handshake_msg(hs::MESSAGE_HASH, &h.hash(ch1))
}

/// TLS 1.2: master secret (extended when `session_hash` is given).
pub(crate) fn master_secret12(h: HashAlg, pms: &[u8], client_random: &[u8], server_random: &[u8], session_hash: Option<&[u8]>) -> Vec<u8> {
    match session_hash {
        Some(sh) => h.tls12_prf(pms, b"extended master secret", sh, 48),
        None => {
            let mut seed = client_random.to_vec();
            seed.extend_from_slice(server_random);
            h.tls12_prf(pms, b"master secret", &seed, 48)
        }
    }
}

/// TLS 1.2: (client_write, server_write) record ciphers.
pub(crate) fn ciphers12(suite: CipherSuite, master: &[u8], client_random: &[u8], server_random: &[u8]) -> (RecordCipher, RecordCipher) {
    let alg = suite.aead();
    let kl = alg.key_len();
    let il = RecordCipher::tls12_iv_len(alg);
    let mut seed = server_random.to_vec();
    seed.extend_from_slice(client_random);
    let kb = suite.hash().tls12_prf(master, b"key expansion", &seed, 2 * kl + 2 * il);
    let ck = &kb[..kl];
    let sk = &kb[kl..2 * kl];
    let civ = &kb[2 * kl..2 * kl + il];
    let siv = &kb[2 * kl + il..];
    (RecordCipher::tls12(alg, ck, civ), RecordCipher::tls12(alg, sk, siv))
}

pub(crate) fn finished12(h: HashAlg, master: &[u8], client: bool, transcript_hash: &[u8]) -> Vec<u8> {
    let label: &[u8] = if client { b"client finished" } else { b"server finished" };
    h.tls12_prf(master, label, transcript_hash, 12)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::{hex, unhex};

    #[test]
    fn rfc8448_simple_1rtt_key_schedule() {
        // RFC 8448 section 3: ECDHE shared secret and the transcript hash of CH..SH.
        let shared = unhex("8bd4054fb55b9d63fdfbacf9f04b9f0d35e6d63f537563efd46272900f89492d");
        let ch = unhex(
            "010000c00303cb34ecb1e78163ba1c38c6dacb196a6dffa21a8d9912ec18a2ef6283024dece7000006130113031302010000910000000b0009000006736572766572ff01000100000a00140012001d0017001800190100010101020103010400230000003300260024001d002099381de560e4bd43d23d8e435a7dbafeb3c06e51c13cae4d5413691e529aaf2c002b0003020304000d0020001e040305030603020308040805080604010501060102010402050206020202002d00020101001c00024001",
        );
        let sh = unhex(
            "020000560303a6af06a4121860dc5e6e60249cd34c95930c8ac5cb1434dac155772ed3e2692800130100002e00330024001d0020c9828876112095fe66762bdbf7c672e156d6cc253b833df1dd69b1b04e751f0f002b00020304",
        );
        let mut t = ch.clone();
        t.extend_from_slice(&sh);
        let hh = HashAlg::Sha256.hash(&t);
        let ks = KeySchedule13::new(CipherSuite::Tls13Aes128GcmSha256, &shared, &hh);
        assert_eq!(hex(&ks.handshake_secret), "1dc826e93606aa6fdc0aadc12f741b01046aa6b99f691ed221a9f0ca043fbeac");
        assert_eq!(hex(&ks.client_hs), "b3eddb126e067f35a780b3abf45e2d8f3b1a950738f52e9600746a0e27a55a21");
        assert_eq!(hex(&ks.server_hs), "b67b7d690cc16c4e75e54213cb2d37b4e9c912bcded9105d42befd59d391ad38");
        let key = hkdf_expand_label(HashAlg::Sha256, &ks.server_hs, "key", &[], 16);
        let iv = hkdf_expand_label(HashAlg::Sha256, &ks.server_hs, "iv", &[], 12);
        assert_eq!(hex(&key), "3fce516009c21727d0f2e4e86ee403bc");
        assert_eq!(hex(&iv), "5d313eb2671276ee13000b30");
    }

    #[test]
    fn acvp_tls13_kdf_vectors() {
        // NIST ACVP TLS 1.3 KDF vector (as used by Go's crypto/tls key_schedule_test.go):
        // the transcript is the concatenation of the given "random" values.
        let psk = unhex("56288B726C73829F7A3E47B103837C8139ACF552E7530C7A710B35ED41191698");
        let dhe = unhex("EFFE9EC26AA29FD750DFA6A10B944D74071595B27EE88887D5E11C84590B5CC3");
        let mut transcript = unhex("E9137679E582BA7C1DB41CF725F86C6D09C8C05F297BAD9A65B552EAF524FDE4");
        transcript.extend(unhex("23ECCFD030790748C8F8D8A656FD98D717F1B62AF3712F97211D2070B499F98A"));
        let h = HashAlg::Sha256;
        let ks = KeySchedule13::with_psk(h, &psk, &dhe, &h.hash(&transcript));
        assert_eq!(hex(&ks.client_hs), "b32306c3ce9932c460a1fe6c0f060593974842036b96fa45049b7352e71c2ad2");
        assert_eq!(hex(&ks.server_hs), "22787f8ca269d34bc549ac8ba19f2040938a3aa370d7cc9d60f720882b88d01b");
        transcript.extend(unhex("C750EDA6696CD101B142BD79E00E6AC8C5F2C0ABC78DD64F4D991326659E9299"));
        let (c, s) = ks.app_secrets(&h.hash(&transcript));
        assert_eq!(hex(&c), "47d7ea08397b5871154b0fe85584bcc30a87c69e84d69b56007c5b21f76493ba");
        assert_eq!(hex(&s), "efbdb0c873c0480da57307083839a8984be25b9a8545e4fca029940fe2800565");
    }

    #[test]
    fn tls12_prf_vector() {
        // Widely used TLS 1.2 PRF (SHA-256) test vector (IETF TLS WG mailing list),
        // cross-checked against Python's hmac/hashlib.
        let secret = unhex("9bbe436ba940f017b17652849a71db35");
        let seed = unhex("a0ba9f936cda311827a6f796ffd5198c");
        let out = HashAlg::Sha256.tls12_prf(&secret, b"test label", &seed, 100);
        assert_eq!(
            hex(&out),
            "e3f229ba727be17b8d122620557cd453c2aab21d07c3d495329b52d4e61edb5a6b301791e90d35c9c9a46b4e14baf9af0fa022f7077def17abfd3797c0564bab4fbc91666e9def9b97fce34f796789baa48082d122ee42c5a72e5a5110fff70187347b66"
        );
    }
}
