//! HPACK Huffman coding (RFC 7541 section 5.2 and Appendix B).

use std::sync::OnceLock;

use super::huffman_table::HUFFMAN_TABLE;
use super::HpackError;

/// Number of bytes `data` occupies once Huffman encoded.
pub fn encoded_len(data: &[u8]) -> usize {
    let bits: usize = data.iter().map(|&b| HUFFMAN_TABLE[b as usize].1 as usize).sum();
    bits.div_ceil(8)
}

/// Huffman-encodes `data`, appending to `out` (padded with the EOS prefix).
pub fn encode(data: &[u8], out: &mut Vec<u8>) {
    let mut acc: u64 = 0;
    let mut nbits: u32 = 0;
    for &b in data {
        let (code, len) = HUFFMAN_TABLE[b as usize];
        acc = (acc << len) | code as u64;
        nbits += len as u32;
        while nbits >= 8 {
            nbits -= 8;
            out.push((acc >> nbits) as u8);
        }
        acc &= (1u64 << nbits) - 1;
    }
    if nbits > 0 {
        let pad = 8 - nbits;
        out.push(((acc << pad) | ((1u64 << pad) - 1)) as u8);
    }
}

/// Decoding tree: node `i` has children `tree[i][0|1]`; values >= 0x8000 are leaves
/// (`0x8000 | symbol`), 0 means "no child".
fn tree() -> &'static Vec<[u16; 2]> {
    static TREE: OnceLock<Vec<[u16; 2]>> = OnceLock::new();
    TREE.get_or_init(|| {
        let mut t: Vec<[u16; 2]> = vec![[0, 0]];
        for (sym, &(code, len)) in HUFFMAN_TABLE.iter().enumerate() {
            let mut node = 0usize;
            for i in (0..len).rev() {
                let bit = ((code >> i) & 1) as usize;
                if i == 0 {
                    t[node][bit] = 0x8000 | sym as u16;
                } else {
                    let next = t[node][bit];
                    if next == 0 {
                        t.push([0, 0]);
                        let idx = (t.len() - 1) as u16;
                        t[node][bit] = idx;
                        node = idx as usize;
                    } else {
                        node = next as usize;
                    }
                }
            }
        }
        t
    })
}

/// Decodes a Huffman-encoded string.
pub fn decode(data: &[u8]) -> Result<Vec<u8>, HpackError> {
    let t = tree();
    let mut out = Vec::with_capacity(data.len() * 8 / 5);
    let mut node = 0usize;
    // bits consumed since the last emitted symbol, and whether they were all ones
    let mut pending_bits = 0u32;
    let mut pending_all_ones = true;
    for &byte in data {
        for i in (0..8).rev() {
            let bit = ((byte >> i) & 1) as usize;
            pending_bits += 1;
            pending_all_ones &= bit == 1;
            let next = t[node][bit];
            if next == 0 {
                return Err(HpackError::Huffman("invalid code"));
            }
            if next & 0x8000 != 0 {
                let sym = next & 0x7fff;
                if sym == 256 {
                    return Err(HpackError::Huffman("EOS symbol in string"));
                }
                out.push(sym as u8);
                node = 0;
                pending_bits = 0;
                pending_all_ones = true;
            } else {
                node = next as usize;
            }
        }
    }
    if pending_bits > 7 {
        return Err(HpackError::Huffman("padding longer than 7 bits"));
    }
    if !pending_all_ones {
        return Err(HpackError::Huffman("padding is not the EOS prefix"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }

    #[test]
    fn rfc_examples() {
        // RFC 7541 C.4.1
        let mut out = Vec::new();
        encode(b"www.example.com", &mut out);
        assert_eq!(out, hex("f1e3 c2e5 f23a 6ba0 ab90 f4ff"));
        assert_eq!(decode(&out).unwrap(), b"www.example.com");
        assert_eq!(encoded_len(b"www.example.com"), 12);
        // C.4.2
        assert_eq!(decode(&hex("a8eb 1064 9cbf")).unwrap(), b"no-cache");
        // C.4.3
        assert_eq!(decode(&hex("25a8 49e9 5ba9 7d7f")).unwrap(), b"custom-key");
        assert_eq!(decode(&hex("25a8 49e9 5bb8 e8b4 bf")).unwrap(), b"custom-value");
        // C.6.1
        assert_eq!(decode(&hex("6402")).unwrap(), b"302");
        assert_eq!(decode(&hex("aec3 771a 4b")).unwrap(), b"private");
        assert_eq!(decode(&hex("d07a be94 1054 d444 a820 0595 040b 8166 e082 a62d 1bff")).unwrap(), b"Mon, 21 Oct 2013 20:13:21 GMT");
        assert_eq!(decode(&hex("9d29 ad17 1863 c78f 0b97 c8e9 ae82 ae43 d3")).unwrap(), b"https://www.example.com");
    }

    #[test]
    fn all_bytes_round_trip() {
        let data: Vec<u8> = (0..=255u8).chain((0..=255u8).rev()).collect();
        let mut out = Vec::new();
        encode(&data, &mut out);
        assert_eq!(out.len(), encoded_len(&data));
        assert_eq!(decode(&out).unwrap(), data);
    }

    #[test]
    fn invalid_padding() {
        // 'a' = 00011 (5 bits); pad with zeros instead of ones
        assert!(decode(&[0b0001_1000]).is_err());
        assert_eq!(decode(&[0b0001_1111]).unwrap(), b"a");
        // a full byte of ones is more than 7 bits of padding
        assert!(decode(&[0b0001_1111, 0xff]).is_err());
    }
}
