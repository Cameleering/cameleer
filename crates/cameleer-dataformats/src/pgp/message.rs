//! OpenPGP messages: literal data, compression, one-pass signed messages, PKESK + SEIPD v1
//! (with MDC) / SED encryption, and the matching decryption and verification.

use cameleer_core::dataformat::compress::{deflate, inflate, zlib_compress, zlib_decompress};
use cameleer_core::error::Result;
use cameleer_tls::crypto::hash::{Digest, Sha1};
use cameleer_tls::crypto::rng;

use super::key::*;
use super::packet::*;
use super::pgp_err;

pub const COMP_NONE: u8 = 0;
pub const COMP_ZIP: u8 = 1;
pub const COMP_ZLIB: u8 = 2;

/// Message creation options.
#[derive(Clone, Debug)]
pub struct WriteOptions {
    pub sym: u8,
    pub compression: u8,
    pub hash: u8,
    pub integrity: bool,
    pub file_name: String,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions { sym: SYM_AES256, compression: COMP_ZIP, hash: HASH_SHA256, integrity: true, file_name: "_CONSOLE".into() }
    }
}

/// A signing key with its unlocked secret.
pub struct Signer<'a> {
    pub key: &'a Key,
    pub secret: SecretParams,
}

fn literal_packet(data: &[u8], name: &str) -> Vec<u8> {
    let name = name.as_bytes();
    let name = &name[..name.len().min(255)];
    let mut body = vec![b'b', name.len() as u8];
    body.extend_from_slice(name);
    body.extend_from_slice(&0u32.to_be_bytes());
    body.extend_from_slice(data);
    packet(TAG_LITERAL, &body)
}

/// Build the (optionally signed, optionally compressed) inner message.
pub fn build_inner(data: &[u8], signers: &[Signer<'_>], opts: &WriteOptions) -> Result<Vec<u8>> {
    let mut inner = Vec::new();
    for (i, s) in signers.iter().enumerate() {
        let mut ops = vec![3, 0x00, opts.hash, s.key.algorithm];
        ops.extend_from_slice(&s.key.key_id);
        ops.push(if i + 1 == signers.len() { 1 } else { 0 });
        write_packet(&mut inner, TAG_ONE_PASS, &ops);
    }
    inner.extend(literal_packet(data, &opts.file_name));
    for s in signers.iter().rev() {
        let hp = signature_hashed_part(0x00, s.key, opts.hash, &[]);
        let mut h = Hasher::new(hash_alg(opts.hash)?);
        h.update(data);
        let sig = make_signature(s.key, &s.secret, opts.hash, hp, h)?;
        write_packet(&mut inner, TAG_SIGNATURE, &sig);
    }
    Ok(match opts.compression {
        COMP_NONE => inner,
        COMP_ZIP | COMP_ZLIB => {
            let mut body = vec![opts.compression];
            body.extend(if opts.compression == COMP_ZIP { deflate(&inner, 6) } else { zlib_compress(&inner, 6) });
            packet(TAG_COMPRESSED, &body)
        }
        other => return Err(pgp_err(format!("Unsupported compression algorithm {}", other))),
    })
}

/// Encrypt (and optionally sign) data for the recipients; returns binary OpenPGP data.
pub fn encrypt(data: &[u8], recipients: &[&Key], signers: &[Signer<'_>], opts: &WriteOptions) -> Result<Vec<u8>> {
    if recipients.is_empty() {
        return Err(pgp_err("No encryption key"));
    }
    let inner = build_inner(data, signers, opts)?;
    let kl = sym_key_len(opts.sym)?;
    let bs = sym_block_len(opts.sym);
    let session_key = rng::random_bytes(kl);
    let mut out = Vec::new();
    for r in recipients {
        let pkesk = encrypt_session_key(r, opts.sym, &session_key)?;
        write_packet(&mut out, TAG_PKESK, &pkesk);
    }
    let mut prefix = rng::random_bytes(bs);
    prefix.push(prefix[bs - 2]);
    prefix.push(prefix[bs - 1]);
    let cipher = Cipher::new(opts.sym, &session_key)?;
    if opts.integrity {
        let mut plain = prefix;
        plain.extend_from_slice(&inner);
        plain.extend_from_slice(&[0xd3, 0x14]);
        let mdc = Sha1::digest(&plain);
        plain.extend(mdc);
        cipher.cfb_encrypt(&vec![0u8; bs], &mut plain);
        let mut body = vec![1];
        body.extend(plain);
        write_packet(&mut out, TAG_SEIPD, &body);
    } else {
        let mut plain = prefix;
        plain.extend_from_slice(&inner);
        cipher.pgp_cfb(&mut plain, true);
        write_packet(&mut out, TAG_SED, &plain);
    }
    Ok(out)
}

/// Sign only (no encryption): returns binary OpenPGP data (compressed signed message).
pub fn sign(data: &[u8], signers: &[Signer<'_>], opts: &WriteOptions) -> Result<Vec<u8>> {
    build_inner(data, signers, opts)
}

/// A signature found in a message.
#[derive(Clone, Debug)]
pub struct SignatureCheck {
    pub issuer: Option<[u8; 8]>,
    /// The verified key's user IDs (None when no key was found).
    pub user_ids: Option<Vec<String>>,
    pub valid: bool,
}

/// A decrypted message.
#[derive(Clone, Debug, Default)]
pub struct Decrypted {
    pub data: Vec<u8>,
    pub file_name: String,
    pub encrypted: bool,
    pub integrity_protected: bool,
    pub signatures: Vec<SignatureCheck>,
}

/// Decrypt a message (armored or binary). `secret` provides the decryption keys with
/// their passwords; `verify` the public keys for signature verification.
pub fn decrypt(msg: &[u8], secret: &[(Cert, Option<String>)], verify: &[Cert]) -> Result<Decrypted> {
    let bin = to_binary(msg)?;
    let packets = parse_packets(&bin)?;
    let mut out = Decrypted::default();
    let mut session: Option<(u8, Vec<u8>)> = None;
    let mut last_err: Option<cameleer_core::CamelError> = None;
    let mut saw_pkesk = false;
    let mut rest = Vec::new();
    for p in packets {
        match p.tag {
            TAG_PKESK => {
                saw_pkesk = true;
                if session.is_some() || p.body.len() < 10 || p.body[0] != 3 {
                    continue;
                }
                let kid: [u8; 8] = p.body[1..9].try_into().unwrap();
                let wildcard = kid == [0u8; 8];
                for (cert, pw) in secret {
                    for k in cert.keys().filter(|k| k.has_secret() && (k.key_id == kid || (wildcard && k.can_encrypt()))) {
                        let r = k.unlock(pw.as_deref()).and_then(|s| decrypt_session_key(k, &s, &p.body));
                        match r {
                            Ok(sk) => {
                                session = Some(sk);
                                break;
                            }
                            Err(e) => last_err = Some(e),
                        }
                    }
                    if session.is_some() {
                        break;
                    }
                }
            }
            TAG_SKESK => return Err(pgp_err("Password (symmetric key) encrypted messages are not supported")),
            TAG_OCB_ENCRYPTED | TAG_SEIPD | TAG_SED => {
                let (sym, key) = match &session {
                    Some(s) => s.clone(),
                    None if saw_pkesk => {
                        return Err(last_err.unwrap_or_else(|| pgp_err("Provided input is encrypted with a key that is not in the secret keyring (no matching private key)")))
                    }
                    None => return Err(pgp_err("Encrypted data without a session key packet")),
                };
                let bs = sym_block_len(sym);
                let cipher = Cipher::new(sym, &key)?;
                let inner = if p.tag == TAG_OCB_ENCRYPTED {
                    out.integrity_protected = true;
                    decrypt_ocb_packet(&p.body, sym, &key)?
                } else if p.tag == TAG_SEIPD {
                    if p.body.first() != Some(&1) {
                        return Err(pgp_err(format!("Unsupported SEIPD version {}", p.body.first().copied().unwrap_or(0))));
                    }
                    let mut d = p.body[1..].to_vec();
                    cipher.cfb_decrypt(&vec![0u8; bs], &mut d);
                    if d.len() < bs + 2 + 22 || d[bs - 2..bs] != d[bs..bs + 2] {
                        return Err(pgp_err("Session key decryption failed (quick check)"));
                    }
                    let n = d.len();
                    if d[n - 22..n - 20] != [0xd3, 0x14] || Sha1::digest(&d[..n - 20]) != d[n - 20..] {
                        return Err(pgp_err("Integrity check failed: modification detected (MDC)"));
                    }
                    out.integrity_protected = true;
                    d[bs + 2..n - 22].to_vec()
                } else {
                    let mut d = p.body.clone();
                    cipher.pgp_cfb(&mut d, false);
                    if d.len() < bs + 2 || d[bs - 2..bs] != d[bs..bs + 2] {
                        return Err(pgp_err("Session key decryption failed (quick check)"));
                    }
                    d[bs + 2..].to_vec()
                };
                out.encrypted = true;
                rest.extend(parse_packets(&inner)?);
            }
            TAG_MARKER => {}
            _ => rest.push(p),
        }
    }
    process(rest, verify, &mut out, 0)?;
    Ok(out)
}

fn process(packets: Vec<Packet>, verify: &[Cert], out: &mut Decrypted, depth: usize) -> Result<()> {
    if depth > 8 {
        return Err(pgp_err("Too deeply nested OpenPGP message"));
    }
    let mut sigs = Vec::new();
    let mut literal: Option<(u8, Vec<u8>)> = None;
    for p in packets {
        match p.tag {
            TAG_COMPRESSED => {
                let alg = *p.body.first().ok_or_else(|| pgp_err("Empty compressed packet"))?;
                let data = match alg {
                    COMP_NONE => p.body[1..].to_vec(),
                    COMP_ZIP => inflate(&p.body[1..])?,
                    COMP_ZLIB => zlib_decompress(&p.body[1..])?,
                    3 => return Err(pgp_err("BZip2 compression is not supported")),
                    o => return Err(pgp_err(format!("Unsupported compression algorithm {}", o))),
                };
                process(parse_packets(&data)?, verify, out, depth + 1)?;
            }
            TAG_LITERAL => {
                let b = &p.body;
                if b.len() < 6 {
                    return Err(pgp_err("Truncated literal data"));
                }
                let nl = b[1] as usize;
                let start = 2 + nl + 4;
                if b.len() < start {
                    return Err(pgp_err("Truncated literal data"));
                }
                out.file_name = String::from_utf8_lossy(&b[2..2 + nl]).to_string();
                literal = Some((b[0], b[start..].to_vec()));
            }
            TAG_SIGNATURE => sigs.push(p.body),
            TAG_ONE_PASS | TAG_MARKER => {}
            TAG_PKESK | TAG_SEIPD | TAG_SED | TAG_OCB_ENCRYPTED => return Err(pgp_err("Nested encrypted data is not supported")),
            _ => {}
        }
    }
    if let Some((_fmt, data)) = literal {
        for s in sigs {
            out.signatures.push(check_signature(&s, &data, verify));
        }
        out.data = data;
    } else if !sigs.is_empty() && out.data.is_empty() {
        return Err(pgp_err("Signature without literal data (detached signatures are not supported)"));
    }
    Ok(())
}

fn canonical_text(d: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(d.len());
    let mut prev = 0u8;
    for &b in d {
        if b == b'\n' && prev != b'\r' {
            o.push(b'\r');
        }
        o.push(b);
        prev = b;
    }
    o
}

fn check_signature(body: &[u8], data: &[u8], verify: &[Cert]) -> SignatureCheck {
    let sig = match Signature::parse(body) {
        Ok(s) => s,
        Err(_) => return SignatureCheck { issuer: None, user_ids: None, valid: false },
    };
    let issuer = sig.issuer;
    let digest = match hash_alg(sig.hash_alg) {
        Ok(h) => {
            let mut hs = Hasher::new(h);
            if sig.sig_type == 0x01 {
                hs.update(&canonical_text(data));
            } else {
                hs.update(data);
            }
            sig.digest(hs)
        }
        Err(_) => return SignatureCheck { issuer, user_ids: None, valid: false },
    };
    for c in verify {
        for k in c.keys() {
            let matches = match (&sig.issuer_fpr, issuer) {
                (Some(f), _) => *f == k.fingerprint,
                (None, Some(i)) => i == k.key_id,
                _ => true,
            };
            if !matches {
                continue;
            }
            if let Ok(true) = sig.verify_digest(k, &digest) {
                return SignatureCheck { issuer, user_ids: Some(c.user_ids.clone()), valid: true };
            }
            return SignatureCheck { issuer, user_ids: Some(c.user_ids.clone()), valid: false };
        }
    }
    SignatureCheck { issuer, user_ids: None, valid: false }
}

/// Decrypt a LibrePGP / RFC 4880bis "OCB Encrypted Data" packet (tag 20, version 1).
fn decrypt_ocb_packet(body: &[u8], sym: u8, key: &[u8]) -> Result<Vec<u8>> {
    if body.len() < 4 || body[0] != 1 {
        return Err(pgp_err("Unsupported AEAD encrypted data packet version"));
    }
    let (cipher, aead, cbyte) = (body[1], body[2], body[3]);
    if aead != 2 {
        return Err(pgp_err(format!("Unsupported AEAD algorithm {} (only OCB)", aead)));
    }
    if cipher != sym && !matches!(cipher, SYM_AES128 | SYM_AES192 | SYM_AES256) {
        return Err(pgp_err("Unsupported AEAD cipher"));
    }
    if key.len() != sym_key_len(cipher)? || cbyte > 56 {
        return Err(pgp_err("Invalid AEAD parameters"));
    }
    let chunk = 1usize << (cbyte as u32 + 6);
    let iv = body.get(4..19).ok_or_else(|| pgp_err("Truncated AEAD packet"))?;
    let data = &body[19..];
    if data.len() < 16 {
        return Err(pgp_err("Truncated AEAD packet"));
    }
    let (chunks_data, final_tag) = data.split_at(data.len() - 16);
    let ocb = super::ocb::Ocb::new(key);
    let header = [0xc0 | TAG_OCB_ENCRYPTED, 1, cipher, aead, cbyte];
    let nonce = |idx: u64| -> Vec<u8> {
        let mut n = iv.to_vec();
        for (i, b) in idx.to_be_bytes().iter().enumerate() {
            n[7 + i] ^= b;
        }
        n
    };
    let mut out = Vec::new();
    let mut idx = 0u64;
    for c in chunks_data.chunks(chunk + 16) {
        let mut aad = header.to_vec();
        aad.extend_from_slice(&idx.to_be_bytes());
        let p = ocb.open(&nonce(idx), &aad, c).ok_or_else(|| pgp_err("AEAD authentication failed: modification detected"))?;
        out.extend(p);
        idx += 1;
    }
    let mut aad = header.to_vec();
    aad.extend_from_slice(&idx.to_be_bytes());
    aad.extend_from_slice(&(out.len() as u64).to_be_bytes());
    ocb.open(&nonce(idx), &aad, final_tag).ok_or_else(|| pgp_err("AEAD final authentication tag mismatch"))?;
    Ok(out)
}
