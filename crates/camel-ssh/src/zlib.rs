//! Streaming zlib for SSH packet compression (`zlib`, `zlib@openssh.com`, RFC 4253 6.2).
//!
//! One zlib stream spans the whole connection direction. The compressor emits, per packet,
//! fixed-Huffman DEFLATE blocks (LZ77 matches against the last 32 KiB of plaintext) followed
//! by a sync flush (empty stored block), so every packet is decodable on its own and ends on
//! a byte boundary. The decompressor keeps the window and any leftover bits between packets,
//! as peers using `Z_PARTIAL_FLUSH` (OpenSSH) end packets mid-byte.

const LEN_BASE: [u16; 29] = [3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258];
const LEN_EXTRA: [u8; 29] = [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0];
const DIST_BASE: [u16; 30] = [1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577];
const DIST_EXTRA: [u8; 30] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13];
const CL_ORDER: [usize; 19] = [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15];
const WINDOW: usize = 32 * 1024;
/// Upper bound for one decompressed packet (RFC 4253 limits packets to 256 KiB).
const MAX_OUT: usize = 1 << 20;

// ------------------------------------------------------------------------- compressor

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn put(&mut self, v: u32, bits: u32) {
        self.acc |= (v as u64) << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.out.push(self.acc as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// Huffman codes are sent most significant bit first.
    fn put_rev(&mut self, code: u32, bits: u32) {
        let mut r = 0;
        for i in 0..bits {
            r |= ((code >> i) & 1) << (bits - 1 - i);
        }
        self.put(r, bits);
    }
    fn align(&mut self) {
        if self.n > 0 {
            self.out.push(self.acc as u8);
            self.acc = 0;
            self.n = 0;
        }
    }
}

fn put_lit(w: &mut BitWriter, sym: u32) {
    match sym {
        0..=143 => w.put_rev(0x30 + sym, 8),
        144..=255 => w.put_rev(0x190 + sym - 144, 9),
        256..=279 => w.put_rev(sym - 256, 7),
        _ => w.put_rev(0xc0 + sym - 280, 8),
    }
}

/// The compressing side of a connection direction.
pub struct Deflater {
    started: bool,
    /// Plaintext history (last 32 KiB) for back references.
    hist: Vec<u8>,
}

impl Default for Deflater {
    fn default() -> Self {
        Self::new()
    }
}

impl Deflater {
    pub fn new() -> Deflater {
        Deflater { started: false, hist: Vec::new() }
    }

    /// Compresses one packet payload (sync flushed).
    pub fn compress(&mut self, data: &[u8]) -> Vec<u8> {
        let mut w = BitWriter { out: Vec::with_capacity(data.len() / 2 + 16), acc: 0, n: 0 };
        if !self.started {
            self.started = true;
            w.out.extend_from_slice(&[0x78, 0x01]);
        }
        if !data.is_empty() {
            let base = self.hist.len();
            let mut buf = std::mem::take(&mut self.hist);
            buf.extend_from_slice(data);
            // fixed Huffman block, not final
            w.put(0, 1);
            w.put(1, 2);
            let mut head: std::collections::HashMap<[u8; 3], Vec<usize>> = std::collections::HashMap::new();
            for i in base.saturating_sub(WINDOW)..base {
                if i + 3 <= buf.len() {
                    head.entry([buf[i], buf[i + 1], buf[i + 2]]).or_default().push(i);
                }
            }
            let mut i = base;
            while i < buf.len() {
                let mut best = (0usize, 0usize);
                if i + 3 <= buf.len() {
                    let key = [buf[i], buf[i + 1], buf[i + 2]];
                    if let Some(cands) = head.get(&key) {
                        for &c in cands.iter().rev().take(16) {
                            if i - c > WINDOW {
                                break;
                            }
                            let mut l = 0;
                            while l < 258 && i + l < buf.len() && buf[c + l] == buf[i + l] {
                                l += 1;
                            }
                            if l > best.0 {
                                best = (l, i - c);
                            }
                        }
                    }
                }
                let step = if best.0 >= 3 {
                    let (len, dist) = best;
                    let ls = LEN_BASE.iter().rposition(|b| *b as usize <= len).unwrap();
                    put_lit(&mut w, 257 + ls as u32);
                    w.put((len - LEN_BASE[ls] as usize) as u32, LEN_EXTRA[ls] as u32);
                    let ds = DIST_BASE.iter().rposition(|b| *b as usize <= dist).unwrap();
                    w.put_rev(ds as u32, 5);
                    w.put((dist - DIST_BASE[ds] as usize) as u32, DIST_EXTRA[ds] as u32);
                    len
                } else {
                    put_lit(&mut w, buf[i] as u32);
                    1
                };
                for k in i..i + step {
                    if k + 3 <= buf.len() {
                        head.entry([buf[k], buf[k + 1], buf[k + 2]]).or_default().push(k);
                    }
                }
                i += step;
            }
            put_lit(&mut w, 256);
            let keep = buf.len().saturating_sub(WINDOW);
            self.hist = buf.split_off(keep);
        }
        // sync flush: empty stored block
        w.put(0, 1);
        w.put(0, 2);
        w.align();
        w.out.extend_from_slice(&[0, 0, 0xff, 0xff]);
        w.out
    }
}

// ------------------------------------------------------------------------- decompressor

#[derive(Debug)]
enum Stop {
    /// Input ended: wait for the next packet (state rolled back to the block start).
    Eof,
    Bad(String),
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> Result<u32, Stop> {
        let byte = *self.data.get(self.pos / 8).ok_or(Stop::Eof)?;
        let v = (byte >> (self.pos % 8)) & 1;
        self.pos += 1;
        Ok(v as u32)
    }
    fn bits(&mut self, n: u32) -> Result<u32, Stop> {
        let mut v = 0;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }
}

struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, Stop> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offs = [0u16; 16];
        for len in 1..15 {
            offs[len + 1] = offs[len] + counts[len];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let o = &mut offs[l as usize];
                if *o as usize >= symbols.len() {
                    return Err(Stop::Bad("invalid Huffman code".into()));
                }
                symbols[*o as usize] = sym as u16;
                *o += 1;
            }
        }
        Ok(Huffman { counts, symbols })
    }
    fn decode(&self, b: &mut Bits) -> Result<u16, Stop> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied().ok_or_else(|| Stop::Bad("invalid Huffman code".into()));
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        Err(Stop::Bad("invalid Huffman code".into()))
    }
}

/// The decompressing side of a connection direction.
pub struct Inflater {
    header_done: bool,
    pending: Vec<u8>,
    /// Bits of `pending[0]` already consumed.
    bit_off: usize,
    hist: Vec<u8>,
    finished: bool,
}

impl Default for Inflater {
    fn default() -> Self {
        Self::new()
    }
}

impl Inflater {
    pub fn new() -> Inflater {
        Inflater { header_done: false, pending: Vec::new(), bit_off: 0, hist: Vec::new(), finished: false }
    }

    /// Decompresses one packet payload.
    pub fn decompress(&mut self, data: &[u8]) -> Result<Vec<u8>, String> {
        self.pending.extend_from_slice(data);
        if !self.header_done {
            if self.pending.len() < 2 {
                return Ok(Vec::new());
            }
            let (cmf, flg) = (self.pending[0], self.pending[1]);
            if cmf & 0x0f != 8 || ((cmf as u16) << 8 | flg as u16) % 31 != 0 || flg & 0x20 != 0 {
                return Err("invalid zlib stream header".into());
            }
            self.pending.drain(..2);
            self.header_done = true;
        }
        let start_len = self.hist.len();
        let mut b = Bits { data: &self.pending, pos: self.bit_off };
        while !self.finished {
            let snap_pos = b.pos;
            let snap_len = self.hist.len();
            match decode_block(&mut b, &mut self.hist, start_len) {
                Ok(last) => self.finished = last,
                Err(Stop::Eof) => {
                    b.pos = snap_pos;
                    self.hist.truncate(snap_len);
                    break;
                }
                Err(Stop::Bad(e)) => return Err(e),
            }
        }
        let consumed = b.pos;
        let out = self.hist[start_len..].to_vec();
        self.pending.drain(..consumed / 8);
        self.bit_off = consumed % 8;
        let keep = self.hist.len().saturating_sub(WINDOW);
        self.hist.drain(..keep);
        Ok(out)
    }
}

fn decode_block(b: &mut Bits, out: &mut Vec<u8>, start: usize) -> Result<bool, Stop> {
    let last = b.bit()? == 1;
    match b.bits(2)? {
        0 => {
            b.pos = b.pos.div_ceil(8) * 8;
            let len = b.bits(16)?;
            let nlen = b.bits(16)?;
            if len != !nlen & 0xffff {
                return Err(Stop::Bad("invalid stored block length".into()));
            }
            let at = b.pos / 8;
            let chunk = b.data.get(at..at + len as usize).ok_or(Stop::Eof)?;
            out.extend_from_slice(chunk);
            b.pos += len as usize * 8;
        }
        1 => {
            let mut l = [0u8; 288];
            for (i, x) in l.iter_mut().enumerate() {
                *x = match i {
                    0..=143 => 8,
                    144..=255 => 9,
                    256..=279 => 7,
                    _ => 8,
                };
            }
            codes(b, out, &Huffman::new(&l)?, &Huffman::new(&[5u8; 30])?, start)?;
        }
        2 => {
            let hlit = b.bits(5)? as usize + 257;
            let hdist = b.bits(5)? as usize + 1;
            let hclen = b.bits(4)? as usize + 4;
            let mut cl = [0u8; 19];
            for &i in CL_ORDER.iter().take(hclen) {
                cl[i] = b.bits(3)? as u8;
            }
            let clh = Huffman::new(&cl)?;
            let mut lens = vec![0u8; hlit + hdist];
            let mut i = 0;
            while i < lens.len() {
                let sym = clh.decode(b)?;
                let (val, n) = match sym {
                    0..=15 => (sym as u8, 1),
                    16 => (*lens.get(i.wrapping_sub(1)).ok_or_else(|| Stop::Bad("repeat without length".into()))?, 3 + b.bits(2)? as usize),
                    17 => (0, 3 + b.bits(3)? as usize),
                    18 => (0, 11 + b.bits(7)? as usize),
                    _ => return Err(Stop::Bad("invalid code length symbol".into())),
                };
                if i + n > lens.len() {
                    return Err(Stop::Bad("too many code lengths".into()));
                }
                lens[i..i + n].fill(val);
                i += n;
            }
            codes(b, out, &Huffman::new(&lens[..hlit])?, &Huffman::new(&lens[hlit..])?, start)?;
        }
        _ => return Err(Stop::Bad("invalid block type".into())),
    }
    Ok(last)
}

fn codes(b: &mut Bits, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman, start: usize) -> Result<(), Stop> {
    loop {
        let sym = lit.decode(b)? as usize;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Ok(()),
            257..=285 => {
                let s = sym - 257;
                let len = LEN_BASE[s] as usize + b.bits(LEN_EXTRA[s] as u32)? as usize;
                let ds = dist.decode(b)? as usize;
                if ds >= 30 {
                    return Err(Stop::Bad("invalid distance symbol".into()));
                }
                let d = DIST_BASE[ds] as usize + b.bits(DIST_EXTRA[ds] as u32)? as usize;
                if d > out.len() {
                    return Err(Stop::Bad("distance too far back".into()));
                }
                let from = out.len() - d;
                for k in 0..len {
                    let v = out[from + k];
                    out.push(v);
                }
            }
            _ => return Err(Stop::Bad("invalid literal/length symbol".into())),
        }
        if out.len() - start > MAX_OUT {
            return Err(Stop::Bad("decompressed packet too large".into()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_round_trip_across_packets() {
        let mut d = Deflater::new();
        let mut i = Inflater::new();
        let packets: Vec<Vec<u8>> = vec![b"hello hello hello hello".to_vec(), vec![], (0..5000u32).map(|x| (x % 7) as u8).collect(), b"hello again, hello hello".to_vec(), (0..70000u32).map(|x| (x * 31 % 251) as u8).collect()];
        let mut total_in = 0;
        let mut total_out = 0;
        for p in &packets {
            let c = d.compress(p);
            total_in += p.len();
            total_out += c.len();
            assert_eq!(&i.decompress(&c).unwrap(), p);
        }
        assert!(total_out < total_in, "compresses: {} -> {}", total_in, total_out);
        // the concatenation of all packets is one valid stream
        let mut d = Deflater::new();
        let mut all = Vec::new();
        for p in &packets {
            all.extend(d.compress(p));
        }
        let mut i = Inflater::new();
        assert_eq!(i.decompress(&all).unwrap(), packets.concat());
    }

    #[test]
    fn partial_input_and_leftover_bits() {
        // camel-core's zlib (one final dynamic block) fed in tiny pieces
        let data: Vec<u8> = (0..20000u32).map(|x| ((x / 3) % 200) as u8).collect();
        let z = camel_core::dataformat::compress::zlib_compress(&data, 6);
        let mut i = Inflater::new();
        let mut out = Vec::new();
        for chunk in z.chunks(7) {
            out.extend(i.decompress(chunk).unwrap());
        }
        assert_eq!(out, data);
        assert!(Inflater::new().decompress(&[0x12, 0x34]).is_err());
    }
}
