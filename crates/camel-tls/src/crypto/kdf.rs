//! Password-based key derivation: PBKDF2 (RFC 8018) and the PKCS#12 KDF
//! (RFC 7292 appendix B.2).

use super::hash::HashAlg;

/// PBKDF2 with HMAC-`h`.
pub fn pbkdf2(h: HashAlg, password: &[u8], salt: &[u8], iterations: u32, len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut block = 1u32;
    while out.len() < len {
        let mut u = h.hmac_parts(password, &[salt, &block.to_be_bytes()]);
        let mut t = u.clone();
        for _ in 1..iterations {
            u = h.hmac(password, &u);
            for (a, b) in t.iter_mut().zip(&u) {
                *a ^= b;
            }
        }
        let take = (len - out.len()).min(t.len());
        out.extend_from_slice(&t[..take]);
        block += 1;
    }
    out
}

/// PKCS#12 key derivation identifiers.
pub const PKCS12_KEY: u8 = 1;
pub const PKCS12_IV: u8 = 2;
pub const PKCS12_MAC: u8 = 3;

fn block_len(h: HashAlg) -> usize {
    match h {
        HashAlg::Sha384 | HashAlg::Sha512 => 128,
        _ => 64,
    }
}

/// The password as a PKCS#12 BMPString: UTF-16BE with a terminating NUL (empty
/// for an empty password).
pub fn bmp_password(password: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for u in password.encode_utf16() {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out.extend_from_slice(&[0, 0]);
    out
}

/// The PKCS#12 KDF; `password` is already BMP encoded (see [`bmp_password`]).
pub fn pkcs12_kdf(h: HashAlg, password: &[u8], salt: &[u8], id: u8, iterations: u32, len: usize) -> Vec<u8> {
    let v = block_len(h);
    let u = h.output_len();
    let d = vec![id; v];
    let fill = |src: &[u8]| -> Vec<u8> {
        if src.is_empty() {
            return Vec::new();
        }
        let n = v * src.len().div_ceil(v);
        (0..n).map(|i| src[i % src.len()]).collect()
    };
    let mut i_buf = fill(salt);
    i_buf.extend(fill(password));
    let mut out = Vec::with_capacity(len);
    loop {
        let mut input = d.clone();
        input.extend_from_slice(&i_buf);
        let mut a = h.hash(&input);
        for _ in 1..iterations {
            a = h.hash(&a);
        }
        let take = (len - out.len()).min(u);
        out.extend_from_slice(&a[..take]);
        if out.len() >= len {
            return out;
        }
        let b: Vec<u8> = (0..v).map(|i| a[i % u]).collect();
        for chunk in i_buf.chunks_mut(v) {
            // chunk = (chunk + b + 1) mod 2^(8v)
            let mut carry = 1u16;
            for k in (0..v).rev() {
                let s = chunk[k] as u16 + b[k] as u16 + carry;
                chunk[k] = s as u8;
                carry = s >> 8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::hex;

    #[test]
    fn pbkdf2_rfc6070_and_sha256() {
        assert_eq!(hex(&pbkdf2(HashAlg::Sha1, b"password", b"salt", 2, 20)), "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957");
        assert_eq!(hex(&pbkdf2(HashAlg::Sha1, b"password", b"salt", 4096, 20)), "4b007901b765489abead49d926f721d065a429c1");
        assert_eq!(
            hex(&pbkdf2(HashAlg::Sha256, b"password", b"salt", 2, 32)),
            "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
        );
    }

    #[test]
    fn pkcs12_kdf_vectors() {
        // vectors from the BouncyCastle / OpenSSL PKCS#12 KDF tests ("smeg", salt 0A58CF64530D823F)
        let pw = bmp_password("smeg");
        let salt = crate::util::unhex("0A58CF64530D823F");
        assert_eq!(hex(&pkcs12_kdf(HashAlg::Sha1, &pw, &salt, PKCS12_KEY, 1, 24)), "8aaae6297b6cb04642ab5b077851284eb7128f1a2a7fbca3");
        assert_eq!(hex(&pkcs12_kdf(HashAlg::Sha1, &pw, &salt, PKCS12_IV, 1, 8)), "79993dfe048d3b76");
    }
}
