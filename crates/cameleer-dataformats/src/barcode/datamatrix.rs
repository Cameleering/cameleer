//! Data Matrix ECC 200 (ISO/IEC 16022), square symbols 10x10 .. 144x144: ASCII encodation
//! (digit pairs, extended ASCII via upper shift, pseudo-random padding), Reed-Solomon over
//! GF(256)/0x12D with block interleaving, the standard module placement algorithm, finder /
//! timing patterns per data region; decoding of module matrices (ASCII, C40, Text, X12,
//! EDIFACT and Base 256 encodations).

use cameleer_core::error::Result;

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

/// Rectangular symbols: (rows, columns, data region rows, data region columns, data
/// codewords, EC codewords); one block each, regions side by side.
const RECT: [(usize, usize, usize, usize, usize, usize); 6] = [(8, 18, 6, 16, 5, 7), (8, 32, 6, 14, 10, 11), (12, 26, 10, 24, 16, 14), (12, 36, 10, 16, 22, 18), (16, 36, 14, 16, 32, 24), (16, 48, 14, 22, 49, 28)];

/// A symbol size (square or rectangular).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub rows: usize,
    pub cols: usize,
    /// Data region rows / columns.
    pub rh: usize,
    pub rw: usize,
    pub ndata: usize,
    pub necc: usize,
    pub nblocks: usize,
}

impl Entry {
    fn square(e: (usize, usize, usize, usize, usize)) -> Entry {
        Entry { rows: e.0, cols: e.0, rh: e.1, rw: e.1, ndata: e.2, necc: e.3, nblocks: e.4 }
    }
    fn regions(&self) -> (usize, usize) {
        (self.rows / (self.rh + 2), self.cols / (self.rw + 2))
    }
    /// Mapping matrix size (rows, columns).
    fn mapping(&self) -> (usize, usize) {
        let (ry, rx) = self.regions();
        (self.rh * ry, self.rw * rx)
    }
}

/// Symbol shape preference (ZXing `SymbolShapeHint`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Shape {
    /// Smallest symbol of either shape, in ZXing's table order.
    #[default]
    Any,
    Square,
    Rectangle,
}

impl Shape {
    pub fn parse(s: &str) -> Option<Shape> {
        match s.trim().to_ascii_uppercase().trim_start_matches("FORCE_") {
            "NONE" | "ANY" => Some(Shape::Any),
            "SQUARE" => Some(Shape::Square),
            "RECTANGLE" | "RECT" => Some(Shape::Rectangle),
            _ => None,
        }
    }
}

/// Every symbol size in ZXing's lookup order (rectangles interleaved by capacity).
pub fn entries() -> Vec<Entry> {
    let mut v: Vec<Entry> = SIZES.iter().map(|e| Entry::square(*e)).collect();
    v.extend(RECT.iter().map(|r| Entry { rows: r.0, cols: r.1, rh: r.2, rw: r.3, ndata: r.4, necc: r.5, nblocks: 1 }));
    v.sort_by_key(|e| (e.ndata, e.rows != e.cols));
    v
}

fn entry_for(rows: usize, cols: usize) -> Option<Entry> {
    entries().into_iter().find(|e| e.rows == rows && e.cols == cols)
}

/// Symbol sizes supported.
pub fn symbol_sizes() -> Vec<usize> {
    SIZES.iter().map(|s| s.0).collect()
}

/// Valid (rows, columns) of every symbol.
pub fn symbol_dims() -> Vec<(usize, usize)> {
    entries().iter().map(|e| (e.rows, e.cols)).collect()
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

#[cfg(test)]
fn size_entry(size: usize) -> Option<Entry> {
    entry_for(size, size)
}

/// Codewords (data + interleaved EC) for a symbol entry.
fn with_ecc(data: &[u8], entry: Entry) -> Vec<u8> {
    let (ndata, necc, nblocks) = (entry.ndata, entry.necc, entry.nblocks);
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

/// Map a mapping-matrix cell to symbol coordinates (x, y).
fn to_symbol(r: usize, c: usize, e: &Entry) -> (usize, usize) {
    (c / e.rw * (e.rw + 2) + 1 + c % e.rw, r / e.rh * (e.rh + 2) + 1 + r % e.rh)
}

/// Encode bytes as a square Data Matrix symbol (smallest size, or `size` when given).
pub fn encode(data: &[u8], size: Option<usize>) -> Result<Matrix> {
    match size {
        Some(s) => {
            let e = entry_for(s, s).ok_or_else(|| df_err(format!("Invalid Data Matrix size {}x{}", s, s)))?;
            encode_entry(data, e)
        }
        None => encode_shape(data, Shape::Square),
    }
}

/// Encode with a shape preference: ZXing's high-level encoder (all encodations with its
/// look-ahead) into the smallest fitting symbol.
pub fn encode_shape(data: &[u8], shape: Shape) -> Result<Matrix> {
    let (cw, e) = super::dm_encoder::encode_high_level(data, shape).map_err(df_err)?;
    Ok(encode_codewords(&cw, e))
}

/// The smallest symbol of a shape holding `n` data codewords (ZXing `SymbolInfo.lookup`).
pub fn lookup(n: usize, shape: Shape) -> Option<Entry> {
    entries()
        .into_iter()
        .filter(|e| match shape {
            Shape::Any => true,
            Shape::Square => e.rows == e.cols,
            Shape::Rectangle => e.rows != e.cols,
        })
        .find(|e| e.ndata >= n)
}

/// Encode into a given symbol size.
pub fn encode_entry(data: &[u8], entry: Entry) -> Result<Matrix> {
    let mut cw = encode_ascii(data);
    if cw.len() > entry.ndata {
        return Err(df_err(format!("Data does not fit a {}x{} Data Matrix", entry.rows, entry.cols)));
    }
    let ndata = entry.ndata;
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
    Ok(encode_codewords(&cw, entry))
}

/// Build the symbol for complete data codewords (EC added here).
pub fn encode_codewords(cw: &[u8], entry: Entry) -> Matrix {
    let all = with_ecc(cw, entry);
    let (regions_y, regions_x) = entry.regions();
    let (nr, nc) = entry.mapping();
    let (cells, dark) = placement(nr, nc);
    let mut m = Matrix::new(entry.cols, entry.rows);
    // finder and timing patterns of each region
    let (bh, bw) = (entry.rh + 2, entry.rw + 2);
    for ry in 0..regions_y {
        for rx in 0..regions_x {
            let (x0, y0) = (rx * bw, ry * bh);
            for i in 0..bh {
                m.set(x0, y0 + i, true); // left solid
                m.set(x0 + bw - 1, y0 + i, i % 2 == 1); // right timing
            }
            for i in 0..bw {
                m.set(x0 + i, y0 + bh - 1, true); // bottom solid
                m.set(x0 + i, y0, i % 2 == 0); // top timing
            }
        }
    }
    for r in 0..nr {
        for c in 0..nc {
            let v = match cells[r * nc + c] {
                Some((chr, bit)) => (all[chr] >> (8 - bit)) & 1 == 1,
                None => dark[r * nc + c],
            };
            let (x, y) = to_symbol(r, c, &entry);
            m.set(x, y, v);
        }
    }
    m
}

/// Decode a Data Matrix module matrix; returns (bytes, corrected codewords).
pub fn decode_matrix(m: &Matrix) -> Result<(Vec<u8>, usize)> {
    let (cw, corrected) = corrected_codewords(m)?;
    let entry = entry_for(m.height, m.width).ok_or_else(|| df_err("Invalid Data Matrix size"))?;
    Ok((decode_codewords(&cw[..entry.ndata])?, corrected))
}

/// The error-corrected codewords (data then EC) of a module matrix and the number of
/// corrected codewords.
pub fn corrected_codewords(m: &Matrix) -> Result<(Vec<u8>, usize)> {
    let entry = entry_for(m.height, m.width).ok_or_else(|| df_err(format!("Invalid Data Matrix size {}x{}", m.height, m.width)))?;
    let (ndata, necc, nblocks) = (entry.ndata, entry.necc, entry.nblocks);
    let (nr, nc) = entry.mapping();
    let (cells, _) = placement(nr, nc);
    let mut cw = vec![0u8; ndata + necc];
    for r in 0..nr {
        for c in 0..nc {
            if let Some((chr, bit)) = cells[r * nc + c] {
                let (x, y) = to_symbol(r, c, &entry);
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
    Ok((cw, corrected))
}

const C40_SHIFT2: &[u8] = b"!\"#$%&'()*+,-./:;<=>?@[\\]^_";

/// Decode the data codewords (ASCII, C40, Text, X12, EDIFACT, Base 256).
fn decode_codewords(cw: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    let mut upper = false;
    let mut trailer = false;
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
                // (like ZXing: with 2 or fewer codewords left the symbol returns to ASCII
                // without an unlatch; after an unlatch ASCII resumes at the next codeword)
                'edi: while cw.len() - i > 2 {
                    let n = (cw.len() - i).min(3);
                    let mut bits = 0u32;
                    for k in 0..3 {
                        bits = (bits << 8) | if k < n { cw[i + k] as u32 } else { 0 };
                    }
                    for k in 0..4 {
                        let v = ((bits >> (18 - 6 * k)) & 0x3f) as u8;
                        if v == 31 {
                            i += (6 * (k + 1usize)).div_ceil(8);
                            break 'edi;
                        }
                        out.push(if v & 0x20 == 0 { v | 0x40 } else { v });
                    }
                    i += n;
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
            232 | 233 | 234 | 241 => {
                // FNC1, structured append, reader programming, macros, ECI: not mapped
                if c == 241 {
                    i += 1;
                }
                if c == 233 {
                    i += 3;
                }
            }
            235 => upper = true,
            // macro 05 / 06: header now, trailer at the end (ZXing)
            236 | 237 => {
                out.extend_from_slice(if c == 236 { b"[)>\x1e05\x1d" } else { b"[)>\x1e06\x1d" });
                trailer = true;
            }
            _ => return Err(df_err(format!("Invalid Data Matrix codeword {}", c))),
        }
    }
    if trailer {
        out.extend_from_slice(b"\x1e\x04");
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

    /// ZXing `SymbolInfoTestCase` (symbol choice by data codeword count and shape).
    #[test]
    fn zxing_symbol_info_vectors() {
        let info = |n: usize, s: Shape| lookup(n, s).map(|e| (e.necc, e.mapping().1, e.mapping().0, e.cols, e.rows));
        // (error codewords, matrix width, matrix height, symbol width, symbol height)
        assert_eq!(info(3, Shape::Any), Some((5, 8, 8, 10, 10)));
        assert_eq!(info(3, Shape::Rectangle), Some((7, 16, 6, 18, 8)));
        assert_eq!(info(9, Shape::Any), Some((11, 28, 6, 32, 8)));
        assert_eq!(info(9, Shape::Square), Some((12, 14, 14, 16, 16)));
        assert_eq!(info(1559, Shape::Any), None);
        assert_eq!(info(50, Shape::Rectangle), None);
        assert_eq!(info(35, Shape::Any).map(|i| (i.3, i.4)), Some((24, 24)));
    }

    #[test]
    fn placement_covers_every_codeword_bit() {
        for e in entries() {
            let (nr, nc) = e.mapping();
            let (cells, _) = placement(nr, nc);
            let mut seen = vec![0u8; e.ndata + e.necc];
            for c in cells.iter().flatten() {
                seen[c.0] |= 1 << (8 - c.1);
            }
            assert!(seen.iter().all(|s| *s == 0xff), "size {:?}", e);
        }
    }

    #[test]
    fn round_trip_all_sizes_and_errors() {
        for e in entries() {
            let cap = e.ndata;
            let text: Vec<u8> = (0..cap.saturating_sub(1)).map(|i| b'A' + (i % 26) as u8).collect();
            let m = encode_entry(&text, e).unwrap();
            assert_eq!((m.width, m.height), (e.cols, e.rows));
            let (d, _) = decode_matrix(&m).unwrap();
            assert_eq!(d, text, "size {:?}", e);
        }
        // shapes: 4 ASCII codewords fit 12x12 (5) but the rectangle 8x18 (5) when forced
        let m = encode_shape(b"ABCD", Shape::Rectangle).unwrap();
        assert_eq!((m.height, m.width), (8, 18));
        assert_eq!(decode_matrix(&m).unwrap().0, b"ABCD");
        let m = encode_shape(b"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklm", Shape::Rectangle).unwrap();
        assert_eq!((m.height, m.width), (16, 48));
        assert!(encode_shape(&[b'x'; 80], Shape::Rectangle).is_err());
        // C40: latch + two triplets fit the 12x12 symbol (five codewords)
        assert_eq!(encode_shape(b"ABCDEF", Shape::Any).unwrap().height, 12);
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
