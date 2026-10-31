//! Data Matrix ECC 200 (ISO/IEC 16022), square symbols 10x10 .. 144x144: ASCII encodation
//! (digit pairs, extended ASCII via upper shift, pseudo-random padding), Reed-Solomon over
//! GF(256)/0x12D with block interleaving, the standard module placement algorithm, finder /
//! timing patterns per data region; decoding of module matrices (ASCII, C40, Text, X12,
//! EDIFACT and Base 256 encodations).

use camel_core::error::Result;

use super::rs::Gf;
use super::Matrix;
use crate::df_err;

/// (symbol size, data region size, data codewords, EC codewords, interleaved blocks)
const SIZES: [(usize, usize, usize, usize, usize); 24] = [
    (10, 8, 3, 5, 1),
    (12, 10, 5, 7, 1),
    (14, 12, 8, 10, 1),
    (16, 14, 12, 12, 1),
    (18, 16, 18, 14, 1),
    (20, 18, 22, 18, 1),
    (22, 20, 30, 20, 1),
    (24, 22, 36, 24, 1),
    (26, 24, 44, 28, 1),
    (32, 14, 62, 36, 1),
    (36, 16, 86, 42, 1),
    (40, 18, 114, 48, 1),
    (44, 20, 144, 56, 1),
    (48, 22, 174, 68, 1),
    (52, 24, 204, 84, 2),
    (64, 14, 280, 112, 2),
    (72, 16, 368, 144, 4),
    (80, 18, 456, 192, 4),
    (88, 20, 576, 224, 4),
    (96, 22, 696, 272, 4),
    (104, 24, 816, 336, 6),
    (120, 18, 1050, 408, 6),
    (132, 20, 1304, 496, 8),
    (144, 22, 1558, 620, 10),
];

/// Symbol sizes supported.
pub fn symbol_sizes() -> Vec<usize> {
    SIZES.iter().map(|s| s.0).collect()
}

/// ASCII encodation.
pub fn encode_ascii(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if c.is_ascii_digit() && i + 1 < data.len() && data[i + 1].is_ascii_digit() {
            out.push(130 + (c - b'0') * 10 + (data[i + 1] - b'0'));
            i += 2;
        } else if c < 128 {
            out.push(c + 1);
            i += 1;
        } else {
            out.push(235);
            out.push(c - 127);
            i += 1;
        }
    }
    out
}

/// Placement of the codeword bits: for each mapping-matrix cell, `Some((codeword, bit))`
/// (bit 1 = MSB) or `None` for the fixed corner pattern; plus which corner cells are dark.
fn placement(nrow: usize, ncol: usize) -> (Vec<Option<(usize, u8)>>, Vec<bool>) {
    let mut cells: Vec<Option<(usize, u8)>> = vec![None; nrow * ncol];
    let mut visited = vec![false; nrow * ncol];
    let (nr, nc) = (nrow as i32, ncol as i32);
    let mut module = |cells: &mut Vec<Option<(usize, u8)>>, visited: &mut Vec<bool>, mut row: i32, mut col: i32, chr: usize, bit: u8| {
        if row < 0 {
            row += nr;
            col += 4 - ((nr + 4) % 8);
        }
        if col < 0 {
            col += nc;
            row += 4 - ((nc + 4) % 8);
        }
        let i = (row * nc + col) as usize;
        cells[i] = Some((chr, bit));
        visited[i] = true;
    };
    let utah = |cells: &mut Vec<Option<(usize, u8)>>, visited: &mut Vec<bool>, row: i32, col: i32, chr: usize, module: &mut dyn FnMut(&mut Vec<Option<(usize, u8)>>, &mut Vec<bool>, i32, i32, usize, u8)| {
        module(cells, visited, row - 2, col - 2, chr, 1);
        module(cells, visited, row - 2, col - 1, chr, 2);
        module(cells, visited, row - 1, col - 2, chr, 3);
        module(cells, visited, row - 1, col - 1, chr, 4);
        module(cells, visited, row - 1, col, chr, 5);
        module(cells, visited, row, col - 2, chr, 6);
        module(cells, visited, row, col - 1, chr, 7);
        module(cells, visited, row, col, chr, 8);
    };
    let corner = |which: u8, cells: &mut Vec<Option<(usize, u8)>>, visited: &mut Vec<bool>, chr: usize, module: &mut dyn FnMut(&mut Vec<Option<(usize, u8)>>, &mut Vec<bool>, i32, i32, usize, u8)| {
        let pts: [(i32, i32); 8] = match which {
            1 => [(nr - 1, 0), (nr - 1, 1), (nr - 1, 2), (0, nc - 2), (0, nc - 1), (1, nc - 1), (2, nc - 1), (3, nc - 1)],
            2 => [(nr - 3, 0), (nr - 2, 0), (nr - 1, 0), (0, nc - 4), (0, nc - 3), (0, nc - 2), (0, nc - 1), (1, nc - 1)],
            3 => [(nr - 3, 0), (nr - 2, 0), (nr - 1, 0), (0, nc - 2), (0, nc - 1), (1, nc - 1), (2, nc - 1), (3, nc - 1)],
            _ => [(nr - 1, 0), (nr - 1, nc - 1), (0, nc - 3), (0, nc - 2), (0, nc - 1), (1, nc - 3), (1, nc - 2), (1, nc - 1)],
        };
        for (k, (r, c)) in pts.iter().enumerate() {
            module(cells, visited, *r, *c, chr, k as u8 + 1);
        }
    };
    let mut chr = 0usize;
    let (mut row, mut col) = (4i32, 0i32);
    loop {
        if row == nr && col == 0 {
            corner(1, &mut cells, &mut visited, chr, &mut module);
            chr += 1;
        }
        if row == nr - 2 && col == 0 && nc % 4 != 0 {
            corner(2, &mut cells, &mut visited, chr, &mut module);
            chr += 1;
        }
        if row == nr - 2 && col == 0 && nc % 8 == 4 {
            corner(3, &mut cells, &mut visited, chr, &mut module);
            chr += 1;
        }
        if row == nr + 4 && col == 2 && nc % 8 == 0 {
            corner(4, &mut cells, &mut visited, chr, &mut module);
            chr += 1;
        }
        loop {
            if row < nr && col >= 0 && !visited[(row * nc + col) as usize] {
                utah(&mut cells, &mut visited, row, col, chr, &mut module);
                chr += 1;
            }
            row -= 2;
            col += 2;
            if !(row >= 0 && col < nc) {
                break;
            }
        }
        row += 1;
        col += 3;
        loop {
            if row >= 0 && col < nc && !visited[(row * nc + col) as usize] {
                utah(&mut cells, &mut visited, row, col, chr, &mut module);
                chr += 1;
            }
            row += 2;
            col -= 2;
            if !(row < nr && col >= 0) {
                break;
            }
        }
        row += 3;
        col += 1;
        if !(row < nr || col < nc) {
            break;
        }
    }
    let mut dark = vec![false; nrow * ncol];
    if !visited[nrow * ncol - 1] {
        dark[nrow * ncol - 1] = true;
        dark[nrow * ncol - ncol - 2] = true;
    }
    (cells, dark)
}

fn size_entry(size: usize) -> Option<(usize, usize, usize, usize, usize)> {
    SIZES.iter().copied().find(|s| s.0 == size)
}

/// Codewords (data + interleaved EC) for a symbol entry.
fn with_ecc(data: &[u8], entry: (usize, usize, usize, usize, usize)) -> Vec<u8> {
    let (_, _, ndata, necc, nblocks) = entry;
    let gf = Gf::datamatrix();
    let mut out = data.to_vec();
    out.resize(ndata + necc, 0);
    let ecc_per = necc / nblocks;
    for b in 0..nblocks {
        let block: Vec<u8> = (b..ndata).step_by(nblocks).map(|i| data[i]).collect();
        let ecc = gf.encode(&block, ecc_per);
        for (k, e) in ecc.iter().enumerate() {
            out[ndata + b + k * nblocks] = *e;
        }
    }
    out
}

/// Map a mapping-matrix cell to symbol coordinates.
fn to_symbol(r: usize, c: usize, region: usize) -> (usize, usize) {
    (c / region * (region + 2) + 1 + c % region, r / region * (region + 2) + 1 + r % region)
}

/// Encode bytes as a Data Matrix symbol (smallest square size, or `size` when given).
pub fn encode(data: &[u8], size: Option<usize>) -> Result<Matrix> {
    let mut cw = encode_ascii(data);
    let entry = match size {
        Some(s) => size_entry(s).filter(|e| e.2 >= cw.len()).ok_or_else(|| df_err(format!("Data does not fit a {}x{} Data Matrix (or invalid size)", s, s)))?,
        None => SIZES.iter().copied().find(|e| e.2 >= cw.len()).ok_or_else(|| df_err("Data too big for a Data Matrix symbol"))?,
    };
    let (sz, region, ndata, _, _) = entry;
    if cw.len() < ndata {
        cw.push(129);
        while cw.len() < ndata {
            let pos = cw.len() + 1;
            let mut pad = 129 + ((149 * pos) % 253) + 1;
            if pad > 254 {
                pad -= 254;
            }
            cw.push(pad as u8);
        }
    }
    let all = with_ecc(&cw, entry);
    let regions = sz / (region + 2);
    let n = region * regions;
    let (cells, dark) = placement(n, n);
    let mut m = Matrix::new(sz, sz);
    // finder and timing patterns of each region
    for ry in 0..regions {
        for rx in 0..regions {
            let (x0, y0) = (rx * (region + 2), ry * (region + 2));
            let b = region + 2;
            for i in 0..b {
                m.set(x0, y0 + i, true); // left solid
                m.set(x0 + i, y0 + b - 1, true); // bottom solid
                m.set(x0 + i, y0, i % 2 == 0); // top timing
                m.set(x0 + b - 1, y0 + i, i % 2 == 1); // right timing
            }
        }
    }
    for r in 0..n {
        for c in 0..n {
            let v = match cells[r * n + c] {
                Some((chr, bit)) => (all[chr] >> (8 - bit)) & 1 == 1,
                None => dark[r * n + c],
            };
            let (x, y) = to_symbol(r, c, region);
            m.set(x, y, v);
        }
    }
    Ok(m)
}

/// Decode a Data Matrix module matrix; returns (bytes, corrected codewords).
pub fn decode_matrix(m: &Matrix) -> Result<(Vec<u8>, usize)> {
    let entry = size_entry(m.width).filter(|_| m.height == m.width).ok_or_else(|| df_err(format!("Invalid Data Matrix size {}x{}", m.width, m.height)))?;
    let (sz, region, ndata, necc, nblocks) = entry;
    let regions = sz / (region + 2);
    let n = region * regions;
    let (cells, _) = placement(n, n);
    let mut cw = vec![0u8; ndata + necc];
    for r in 0..n {
        for c in 0..n {
            if let Some((chr, bit)) = cells[r * n + c] {
                let (x, y) = to_symbol(r, c, region);
                if m.get(x, y) {
                    cw[chr] |= 1 << (8 - bit);
                }
            }
        }
    }
    let gf = Gf::datamatrix();
    let ecc_per = necc / nblocks;
    let mut corrected = 0;
    for b in 0..nblocks {
        let idx: Vec<usize> = (b..ndata).step_by(nblocks).chain((0..ecc_per).map(|k| ndata + b + k * nblocks)).collect();
        let mut block: Vec<u8> = idx.iter().map(|i| cw[*i]).collect();
        corrected += gf.decode(&mut block, ecc_per).ok_or_else(|| df_err("Data Matrix has too many errors (Reed-Solomon decoding failed)"))?;
        for (k, i) in idx.iter().enumerate() {
            cw[*i] = block[k];
        }
    }
    Ok((decode_codewords(&cw[..ndata])?, corrected))
}

const C40_SHIFT2: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_";

/// Decode the data codewords (ASCII, C40, Text, X12, EDIFACT, Base 256).
fn decode_codewords(cw: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut upper = false;
    while i < cw.len() {
        let c = cw[i];
        i += 1;
        match c {
            1..=128 => {
                let v = c - 1;
                out.push(if upper { v + 128 } else { v });
                upper = false;
            }
            129 => break,
            130..=229 => {
                let v = c - 130;
                out.push(b'0' + v / 10);
                out.push(b'0' + v % 10);
            }
            230 | 239 => {
                // C40 / Text
                let text = c == 239;
                let mut shift = 0u8;
                let mut up = false;
                while i + 1 < cw.len() && cw[i] != 254 {
                    let v = (cw[i] as u32) * 256 + cw[i + 1] as u32 - 1;
                    i += 2;
                    for val in [v / 1600, (v / 40) % 40, v % 40] {
                        let val = val as u8;
                        let emit = |b: u8, out: &mut Vec<u8>, up: &mut bool| {
                            out.push(if *up { b + 128 } else { b });
                            *up = false;
                        };
                        match shift {
                            0 => match val {
                                0..=2 => shift = val + 1,
                                3 => emit(b' ', &mut out, &mut up),
                                4..=13 => emit(b'0' + val - 4, &mut out, &mut up),
                                14..=39 => emit(if text { b'a' } else { b'A' } + val - 14, &mut out, &mut up),
                                _ => {}
                            },
                            1 => {
                                emit(val, &mut out, &mut up);
                                shift = 0;
                            }
                            2 => {
                                match val {
                                    0..=26 => emit(C40_SHIFT2[val as usize], &mut out, &mut up),
                                    30 => up = true,
                                    _ => {}
                                }
                                shift = 0;
                            }
                            _ => {
                                let b = match (text, val) {
                                    (false, 0) | (true, 0) => b'`',
                                    (false, 1..=26) => b'a' + val - 1,
                                    (true, 1..=26) => b'A' + val - 1,
                                    (_, 27) => b'{',
                                    (_, 28) => b'|',
                                    (_, 29) => b'}',
                                    (_, 30) => b'~',
                                    _ => 127,
                                };
                                emit(b, &mut out, &mut up);
                                shift = 0;
                            }
                        }
                    }
                }
                if i < cw.len() && cw[i] == 254 {
                    i += 1;
                }
            }
            238 => {
                // X12
                while i + 1 < cw.len() && cw[i] != 254 {
                    let v = (cw[i] as u32) * 256 + cw[i + 1] as u32 - 1;
                    i += 2;
                    for val in [v / 1600, (v / 40) % 40, v % 40] {
                        out.push(match val {
                            0 => b'\r',
                            1 => b'*',
                            2 => b'>',
                            3 => b' ',
                            4..=13 => b'0' + val as u8 - 4,
                            _ => b'A' + val as u8 - 14,
                        });
                    }
                }
                if i < cw.len() && cw[i] == 254 {
                    i += 1;
                }
            }
            240 => {
                // EDIFACT: 4 x 6 bits in 3 bytes until the unlatch value 31
                'edi: while i < cw.len() {
                    let n = (cw.len() - i).min(3);
                    let mut bits = 0u32;
                    for k in 0..3 {
                        bits = (bits << 8) | if k < n { cw[i + k] as u32 } else { 0 };
                    }
                    i += n;
                    for k in 0..4 {
                        let v = ((bits >> (18 - 6 * k)) & 0x3f) as u8;
                        if v == 31 {
                            break 'edi;
                        }
                        out.push(if v & 0x20 == 0 { v | 0x40 } else { v });
                    }
                }
            }
            231 => {
                // Base 256 with the 255-state randomisation
                let unrand = |v: u8, pos: usize| -> u8 {
                    let r = ((149 * pos) % 255) + 1;
                    ((v as i32 - r as i32).rem_euclid(256)) as u8
                };
                let d1 = unrand(cw.get(i).copied().unwrap_or(0), i + 1) as usize;
                i += 1;
                let len = if d1 == 0 {
                    cw.len() - i
                } else if d1 < 250 {
                    d1
                } else {
                    let d2 = unrand(cw.get(i).copied().unwrap_or(0), i + 1) as usize;
                    i += 1;
                    250 * (d1 - 249) + d2
                };
                for _ in 0..len {
                    let v = cw.get(i).copied().ok_or_else(|| df_err("Truncated Data Matrix Base 256 field"))?;
                    out.push(unrand(v, i + 1));
                    i += 1;
                }
            }
            232 | 233 | 234 | 236 | 237 | 241 => {
                // FNC1, structured append, reader programming, macros, ECI: not mapped
                if c == 241 {
                    i += 1;
                }
                if c == 233 {
                    i += 3;
                }
            }
            235 => upper = true,
            _ => return Err(df_err(format!("Invalid Data Matrix codeword {}", c))),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_encodation_and_padding() {
        assert_eq!(encode_ascii(b"123456"), vec![142, 164, 186]);
        assert_eq!(encode_ascii(b"A1"), vec![66, 50]);
        assert_eq!(encode_ascii(&[0xE9]), vec![235, 0xE9 - 127]);
        // ISO/IEC 16022 example: "123456" in a 10x10 symbol -> data 142 164 186, EC 114 25 5 88 102
        let entry = size_entry(10).unwrap();
        assert_eq!(with_ecc(&[142, 164, 186], entry), vec![142, 164, 186, 114, 25, 5, 88, 102]);
    }

    #[test]
    fn placement_covers_every_codeword_bit() {
        for e in SIZES {
            let regions = e.0 / (e.1 + 2);
            let n = e.1 * regions;
            let (cells, _) = placement(n, n);
            let mut seen = vec![0u8; e.2 + e.3];
            for c in cells.iter().flatten() {
                seen[c.0] |= 1 << (8 - c.1);
            }
            assert!(seen.iter().all(|s| *s == 0xff), "size {}", e.0);
        }
    }

    #[test]
    fn round_trip_all_sizes_and_errors() {
        for e in SIZES {
            let cap = e.2;
            let text: Vec<u8> = (0..cap.saturating_sub(1)).map(|i| b'A' + (i % 26) as u8).collect();
            let m = encode(&text, Some(e.0)).unwrap();
            assert_eq!(m.width, e.0);
            let (d, _) = decode_matrix(&m).unwrap();
            assert_eq!(d, text, "size {}", e.0);
        }
        let m = encode("Hello Data Matrix 2026 é".as_bytes(), None).unwrap();
        let mut damaged = m.clone();
        for x in 3..7 {
            damaged.set(x, 5, !damaged.get(x, 5));
        }
        let (d, corrected) = decode_matrix(&damaged).unwrap();
        assert_eq!(d, "Hello Data Matrix 2026 é".as_bytes());
        assert!(corrected > 0);
    }

    #[test]
    fn decodes_other_encodations() {
        // C40 "AIM" = 1600*C1 + 40*C2 + C3 + 1 with A=14, I=22, M=26 -> 23307 -> 91, 11
        assert_eq!(decode_codewords(&[230, 91, 11, 254]).unwrap(), b"AIM");
        // Base 256: latch, length 2 ("ab"), randomised
        let r = |v: u8, pos: usize| ((v as usize + (149 * pos) % 255 + 1) % 256) as u8;
        assert_eq!(decode_codewords(&[231, r(2, 2), r(b'a', 3), r(b'b', 4)]).unwrap(), b"ab");
    }
}
