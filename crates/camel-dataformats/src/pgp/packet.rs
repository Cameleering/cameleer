//! OpenPGP packet framing (RFC 4880 / RFC 9580 section 4), ASCII armor (section 6),
//! MPIs and the OpenPGP CFB mode.

use camel_core::error::Result;
use camel_tls::crypto::aes::Aes;
use camel_tls::crypto::des::TripleDes;
use camel_tls::crypto::aes::BlockCipher;

use super::pgp_err;

pub const TAG_PKESK: u8 = 1;
pub const TAG_SIGNATURE: u8 = 2;
pub const TAG_SKESK: u8 = 3;
pub const TAG_ONE_PASS: u8 = 4;
pub const TAG_SECRET_KEY: u8 = 5;
pub const TAG_PUBLIC_KEY: u8 = 6;
pub const TAG_SECRET_SUBKEY: u8 = 7;
pub const TAG_COMPRESSED: u8 = 8;
pub const TAG_SED: u8 = 9;
pub const TAG_MARKER: u8 = 10;
pub const TAG_LITERAL: u8 = 11;
pub const TAG_TRUST: u8 = 12;
pub const TAG_USER_ID: u8 = 13;
pub const TAG_PUBLIC_SUBKEY: u8 = 14;
pub const TAG_USER_ATTRIBUTE: u8 = 17;
pub const TAG_SEIPD: u8 = 18;
pub const TAG_MDC: u8 = 19;
pub const TAG_OCB_ENCRYPTED: u8 = 20;

/// A parsed packet.
#[derive(Clone, Debug)]
pub struct Packet {
    pub tag: u8,
    pub body: Vec<u8>,
}

/// Parse a sequence of packets (old and new formats, partial body lengths, old format
/// indeterminate length).
pub fn parse_packets(data: &[u8]) -> Result<Vec<Packet>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let h = data[i];
        i += 1;
        if h & 0x80 == 0 {
            return Err(pgp_err(format!("Invalid packet header byte 0x{:02x}", h)));
        }
        if h & 0x40 != 0 {
            let tag = h & 0x3f;
            let mut body = Vec::new();
            loop {
                let b0 = *data.get(i).ok_or_else(|| pgp_err("Truncated packet length"))? as usize;
                i += 1;
                let (len, partial) = if b0 < 192 {
                    (b0, false)
                } else if b0 < 224 {
                    let b1 = *data.get(i).ok_or_else(|| pgp_err("Truncated packet length"))? as usize;
                    i += 1;
                    (((b0 - 192) << 8) + b1 + 192, false)
                } else if b0 == 255 {
                    let l = data.get(i..i + 4).ok_or_else(|| pgp_err("Truncated packet length"))?;
                    i += 4;
                    (u32::from_be_bytes(l.try_into().unwrap()) as usize, false)
                } else {
                    (1usize << (b0 & 0x1f), true)
                };
                let chunk = data.get(i..i.checked_add(len).ok_or_else(|| pgp_err("Bad packet length"))?).ok_or_else(|| pgp_err("Truncated packet"))?;
                body.extend_from_slice(chunk);
                i += len;
                if !partial {
                    break;
                }
            }
            out.push(Packet { tag, body });
        } else {
            let tag = (h >> 2) & 0x0f;
            let len = match h & 3 {
                0 => {
                    let l = *data.get(i).ok_or_else(|| pgp_err("Truncated packet length"))? as usize;
                    i += 1;
                    l
                }
                1 => {
                    let l = data.get(i..i + 2).ok_or_else(|| pgp_err("Truncated packet length"))?;
                    i += 2;
                    u16::from_be_bytes(l.try_into().unwrap()) as usize
                }
                2 => {
                    let l = data.get(i..i + 4).ok_or_else(|| pgp_err("Truncated packet length"))?;
                    i += 4;
                    u32::from_be_bytes(l.try_into().unwrap()) as usize
                }
                _ => data.len() - i,
            };
            let body = data.get(i..i.checked_add(len).ok_or_else(|| pgp_err("Bad packet length"))?).ok_or_else(|| pgp_err("Truncated packet"))?.to_vec();
            i += len;
            out.push(Packet { tag, body });
        }
    }
    Ok(out)
}

/// Write a packet in the new format with a definite length.
pub fn write_packet(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(0xc0 | tag);
    let l = body.len();
    if l < 192 {
        out.push(l as u8);
    } else if l < 8384 {
        let x = l - 192;
        out.push(((x >> 8) + 192) as u8);
        out.push(x as u8);
    } else {
        out.push(0xff);
        out.extend_from_slice(&(l as u32).to_be_bytes());
    }
    out.extend_from_slice(body);
}

pub fn packet(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut o = Vec::new();
    write_packet(&mut o, tag, body);
    o
}

// ------------------------------------------------------------------------- MPIs

/// Read an MPI; returns its magnitude bytes (big-endian, no leading zeros).
pub fn read_mpi<'a>(data: &'a [u8], pos: &mut usize) -> Result<&'a [u8]> {
    let bits = data.get(*pos..*pos + 2).ok_or_else(|| pgp_err("Truncated MPI"))?;
    let bits = u16::from_be_bytes(bits.try_into().unwrap()) as usize;
    *pos += 2;
    let n = bits.div_ceil(8);
    let v = data.get(*pos..*pos + n).ok_or_else(|| pgp_err("Truncated MPI"))?;
    *pos += n;
    Ok(v)
}

/// Write an MPI (leading zero bytes stripped).
pub fn write_mpi(out: &mut Vec<u8>, v: &[u8]) {
    let mut i = 0;
    while i < v.len() && v[i] == 0 {
        i += 1;
    }
    let v = &v[i..];
    let bits = if v.is_empty() { 0 } else { (v.len() - 1) * 8 + (8 - v[0].leading_zeros() as usize) };
    out.extend_from_slice(&(bits as u16).to_be_bytes());
    out.extend_from_slice(v);
}

/// Left-pad big-endian bytes to `n`.
pub fn pad_left(v: &[u8], n: usize) -> Vec<u8> {
    if v.len() >= n {
        return v[v.len() - n..].to_vec();
    }
    let mut o = vec![0u8; n - v.len()];
    o.extend_from_slice(v);
    o
}

// ------------------------------------------------------------------------- armor

fn crc24(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xB704CE;
    for b in data {
        crc ^= (*b as u32) << 16;
        for _ in 0..8 {
            crc <<= 1;
            if crc & 0x1000000 != 0 {
                crc ^= 0x1864CFB;
            }
        }
    }
    crc & 0xFFFFFF
}

/// ASCII-armor binary data with a block type (`MESSAGE`, `PUBLIC KEY BLOCK`, ...).
pub fn armor(kind: &str, data: &[u8]) -> String {
    let b64 = camel_core::dataformat::base64::encode(data, false);
    let mut out = format!("-----BEGIN PGP {}-----\n\n", kind);
    for chunk in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(chunk).unwrap());
        out.push('\n');
    }
    let c = crc24(data).to_be_bytes();
    out.push('=');
    out.push_str(&camel_core::dataformat::base64::encode(&c[1..], false));
    out.push('\n');
    out.push_str(&format!("-----END PGP {}-----\n", kind));
    out
}

/// Whether data looks ASCII-armored.
pub fn is_armored(data: &[u8]) -> bool {
    let t = &data[..data.len().min(2048)];
    t.windows(15).any(|w| w == b"-----BEGIN PGP ")
}

/// Decode all armored blocks in the text (concatenated).
pub fn dearmor(data: &[u8]) -> Result<Vec<u8>> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    let mut lines = text.lines().peekable();
    let mut found = false;
    while let Some(l) = lines.next() {
        let l = l.trim();
        if !(l.starts_with("-----BEGIN PGP ") && l.ends_with("-----")) {
            continue;
        }
        if l.contains("SIGNED MESSAGE") {
            return Err(pgp_err("Cleartext signed messages are not supported"));
        }
        found = true;
        // headers until an empty line
        let mut body = String::new();
        let mut in_headers = true;
        let mut checksum: Option<String> = None;
        for l in lines.by_ref() {
            let l = l.trim();
            if l.starts_with("-----END PGP ") {
                break;
            }
            if in_headers {
                if l.is_empty() {
                    in_headers = false;
                    continue;
                }
                if l.contains(": ") {
                    continue;
                }
                in_headers = false;
            }
            if let Some(c) = l.strip_prefix('=') {
                if c.len() == 4 {
                    checksum = Some(c.to_string());
                    continue;
                }
            }
            body.push_str(l);
        }
        let bin = camel_core::dataformat::base64::decode(&body).map_err(|e| pgp_err(format!("Invalid armor: {}", e)))?;
        if let Some(c) = checksum {
            let want = camel_core::dataformat::base64::decode(&c).map_err(|_| pgp_err("Invalid armor checksum"))?;
            if want.len() != 3 || want[..] != crc24(&bin).to_be_bytes()[1..] {
                return Err(pgp_err("Armor checksum mismatch"));
            }
        }
        out.extend(bin);
    }
    if !found {
        return Err(pgp_err("No armored PGP block found"));
    }
    Ok(out)
}

/// Binary data from armored or binary input.
pub fn to_binary(data: &[u8]) -> Result<Vec<u8>> {
    if is_armored(data) {
        dearmor(data)
    } else {
        Ok(data.to_vec())
    }
}

// ------------------------------------------------------------------------- symmetric

pub const SYM_TRIPLEDES: u8 = 2;
pub const SYM_CAST5: u8 = 3;
pub const SYM_AES128: u8 = 7;
pub const SYM_AES192: u8 = 8;
pub const SYM_AES256: u8 = 9;

pub fn sym_key_len(alg: u8) -> Result<usize> {
    Ok(match alg {
        SYM_TRIPLEDES => 24,
        SYM_AES128 => 16,
        SYM_AES192 => 24,
        SYM_AES256 => 32,
        SYM_CAST5 => return Err(pgp_err("CAST5 is not supported (use AES)")),
        other => return Err(pgp_err(format!("Unsupported symmetric algorithm {}", other))),
    })
}

pub fn sym_block_len(alg: u8) -> usize {
    if alg == SYM_TRIPLEDES || alg == SYM_CAST5 {
        8
    } else {
        16
    }
}

/// A block cipher selected at runtime.
pub enum Cipher {
    Aes(Aes),
    Tdes(TripleDes),
}

impl Cipher {
    pub fn new(alg: u8, key: &[u8]) -> Result<Cipher> {
        let kl = sym_key_len(alg)?;
        if key.len() != kl {
            return Err(pgp_err("Wrong session key length"));
        }
        Ok(if alg == SYM_TRIPLEDES { Cipher::Tdes(TripleDes::new(key)) } else { Cipher::Aes(Aes::new(key)) })
    }
    fn bs(&self) -> usize {
        match self {
            Cipher::Aes(_) => 16,
            Cipher::Tdes(_) => 8,
        }
    }
    fn enc(&self, b: &mut [u8]) {
        match self {
            Cipher::Aes(c) => c.encrypt(b),
            Cipher::Tdes(c) => c.encrypt(b),
        }
    }

    /// Standard CFB encryption (used by SEIPD v1 and secret key protection).
    pub fn cfb_encrypt(&self, iv: &[u8], data: &mut [u8]) {
        let bs = self.bs();
        let mut reg = iv.to_vec();
        for chunk in data.chunks_mut(bs) {
            let mut ks = reg.clone();
            self.enc(&mut ks);
            for (b, k) in chunk.iter_mut().zip(&ks) {
                *b ^= k;
            }
            reg[..chunk.len()].copy_from_slice(chunk);
        }
    }

    /// Standard CFB decryption.
    pub fn cfb_decrypt(&self, iv: &[u8], data: &mut [u8]) {
        let bs = self.bs();
        let mut reg = iv.to_vec();
        for chunk in data.chunks_mut(bs) {
            let mut ks = reg.clone();
            self.enc(&mut ks);
            let ct = chunk.to_vec();
            for (b, k) in chunk.iter_mut().zip(&ks) {
                *b ^= k;
            }
            reg[..ct.len()].copy_from_slice(&ct);
        }
    }

    /// OpenPGP CFB with resynchronization (legacy Symmetrically Encrypted Data, tag 9).
    pub fn pgp_cfb(&self, data: &mut [u8], encrypt: bool) {
        let bs = self.bs();
        let zero = vec![0u8; bs];
        if data.len() < bs + 2 {
            if encrypt {
                self.cfb_encrypt(&zero, data)
            } else {
                self.cfb_decrypt(&zero, data)
            };
            return;
        }
        // first bs+2 bytes with a zero IV, then resync on the ciphertext bytes 2..bs+2
        let (head, rest) = data.split_at_mut(bs + 2);
        if encrypt {
            self.cfb_encrypt(&zero, head);
        } else {
            let saved = head.to_vec();
            self.cfb_decrypt(&zero, head);
            let iv = saved[2..bs + 2].to_vec();
            self.cfb_decrypt(&iv, rest);
            return;
        }
        let iv = head[2..bs + 2].to_vec();
        self.cfb_encrypt(&iv, rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc24_and_armor() {
        let a = armor("MESSAGE", b"hello world");
        assert!(a.starts_with("-----BEGIN PGP MESSAGE-----"));
        assert_eq!(dearmor(a.as_bytes()).unwrap(), b"hello world");
        let bad = a.replace("aGVsbG8", "aGVsbG9");
        assert!(dearmor(bad.as_bytes()).is_err());
    }

    #[test]
    fn packet_lengths() {
        for n in [0usize, 191, 192, 8383, 8384, 100_000] {
            let body = vec![7u8; n];
            let p = packet(TAG_LITERAL, &body);
            let back = parse_packets(&p).unwrap();
            assert_eq!(back.len(), 1);
            assert_eq!(back[0].body.len(), n);
        }
        // partial lengths: 2^9 = 512 byte chunk then a final 3 byte part
        let mut p = vec![0xc0 | TAG_LITERAL, 0xe9];
        p.extend(vec![1u8; 512]);
        p.push(3);
        p.extend([2, 2, 2]);
        assert_eq!(parse_packets(&p).unwrap()[0].body.len(), 515);
        // old format, indeterminate
        let p = [0xa3u8, 1, 2, 3];
        let back = parse_packets(&p).unwrap();
        assert_eq!(back[0].tag, TAG_COMPRESSED);
        assert_eq!(back[0].body, vec![1, 2, 3]);
    }

    #[test]
    fn mpi() {
        let mut o = Vec::new();
        write_mpi(&mut o, &[0, 0x01, 0xff]);
        assert_eq!(o, vec![0, 9, 1, 0xff]);
        let mut p = 0;
        assert_eq!(read_mpi(&o, &mut p).unwrap(), &[1, 0xff]);
    }
}
