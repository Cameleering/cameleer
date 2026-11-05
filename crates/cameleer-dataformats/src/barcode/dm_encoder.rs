//! Data Matrix high-level encoder, ported from ZXing's `datamatrix.encoder.HighLevelEncoder`
//! (ISO/IEC 16022 Annex P look-ahead): ASCII (digit pairs, upper shift), C40, Text, ANSI X12,
//! EDIFACT and Base 256 encodations with ZXing's mode switching, end-of-data handling
//! relative to the symbol capacity, macro 05/06 headers and the 253-state padding.

use super::datamatrix::{lookup, Entry, Shape};

const ASCII: usize = 0;
const C40: usize = 1;
const TEXT: usize = 2;
const X12: usize = 3;
const EDIFACT: usize = 4;
const BASE256: usize = 5;

const PAD: u8 = 129;
const UPPER_SHIFT: u8 = 235;
const LATCH_TO_C40: u8 = 230;
const LATCH_TO_BASE256: u8 = 231;
const LATCH_TO_X12: u8 = 238;
const LATCH_TO_TEXT: u8 = 239;
const LATCH_TO_EDIFACT: u8 = 240;
const MACRO_05: u8 = 236;
const MACRO_06: u8 = 237;
const UNLATCH: u8 = 254;

type R<T> = Result<T, String>;

struct Ctx<'a> {
    msg: &'a [u8],
    pos: usize,
    cw: Vec<u8>,
    skip_at_end: usize,
    new_enc: Option<usize>,
    shape: Shape,
    info: Option<Entry>,
}

impl Ctx<'_> {
    fn cur(&self) -> u8 {
        self.msg[self.pos]
    }
    fn total(&self) -> usize {
        self.msg.len() - self.skip_at_end
    }
    fn has_more(&self) -> bool {
        self.pos < self.total()
    }
    fn remaining(&self) -> isize {
        self.total() as isize - self.pos as isize
    }
    fn update(&mut self, len: usize) -> R<()> {
        if self.info.is_none_or(|i| len > i.ndata) {
            self.info = Some(lookup(len, self.shape).ok_or_else(|| format!("Can't find a symbol arrangement that matches the message. Data codewords: {}", len))?);
        }
        Ok(())
    }
    fn update_now(&mut self) -> R<()> {
        self.update(self.cw.len())
    }
    fn capacity(&self) -> isize {
        self.info.map(|i| i.ndata as isize).unwrap_or(0)
    }
    fn signal(&mut self, e: usize) {
        self.new_enc = Some(e);
    }
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}
fn is_ext(c: u8) -> bool {
    c >= 128
}
fn native_c40(c: u8) -> bool {
    c == b' ' || c.is_ascii_digit() || c.is_ascii_uppercase()
}
fn native_text(c: u8) -> bool {
    c == b' ' || c.is_ascii_digit() || c.is_ascii_lowercase()
}
fn x12_term_sep(c: u8) -> bool {
    c == b'\r' || c == b'*' || c == b'>'
}
fn native_x12(c: u8) -> bool {
    x12_term_sep(c) || c == b' ' || c.is_ascii_digit() || c.is_ascii_uppercase()
}
fn native_edifact(c: u8) -> bool {
    (b' '..=b'^').contains(&c)
}

fn find_minimums(counts: &[f32; 6], ints: &mut [i32; 6], mins: &mut [u8; 6]) -> i32 {
    let mut min = i32::MAX;
    for i in 0..6 {
        let cur = counts[i].ceil() as i32;
        ints[i] = cur;
        if min > cur {
            min = cur;
            *mins = [0; 6];
        }
        if min == cur {
            mins[i] += 1;
        }
    }
    min
}

fn look_ahead_intern(msg: &[u8], start: usize, current: usize) -> usize {
    if start >= msg.len() {
        return current;
    }
    let mut counts: [f32; 6] = if current == ASCII {
        [0.0, 1.0, 1.0, 1.0, 1.0, 1.25]
    } else {
        let mut c = [1.0, 2.0, 2.0, 2.0, 2.0, 2.25];
        c[current] = 0.0;
        c
    };
    let mut processed = 0;
    let mut mins = [0u8; 6];
    let mut ints = [0i32; 6];
    loop {
        if start + processed == msg.len() {
            let min = find_minimums(&counts, &mut ints, &mut mins);
            let min_count: u32 = mins.iter().map(|m| *m as u32).sum();
            if ints[ASCII] == min {
                return ASCII;
            }
            if min_count == 1 {
                if mins[BASE256] > 0 {
                    return BASE256;
                }
                if mins[EDIFACT] > 0 {
                    return EDIFACT;
                }
                if mins[TEXT] > 0 {
                    return TEXT;
                }
                if mins[X12] > 0 {
                    return X12;
                }
            }
            return C40;
        }
        let c = msg[start + processed];
        processed += 1;
        if is_digit(c) {
            counts[ASCII] += 0.5;
        } else if is_ext(c) {
            counts[ASCII] = counts[ASCII].ceil() + 2.0;
        } else {
            counts[ASCII] = counts[ASCII].ceil() + 1.0;
        }
        counts[C40] += if native_c40(c) { 2.0 / 3.0 } else if is_ext(c) { 8.0 / 3.0 } else { 4.0 / 3.0 };
        counts[TEXT] += if native_text(c) { 2.0 / 3.0 } else if is_ext(c) { 8.0 / 3.0 } else { 4.0 / 3.0 };
        counts[X12] += if native_x12(c) { 2.0 / 3.0 } else if is_ext(c) { 13.0 / 3.0 } else { 10.0 / 3.0 };
        counts[EDIFACT] += if native_edifact(c) { 3.0 / 4.0 } else if is_ext(c) { 17.0 / 4.0 } else { 13.0 / 4.0 };
        // isSpecialB256 is always false in ZXing
        counts[BASE256] += 1.0;
        if processed >= 4 {
            find_minimums(&counts, &mut ints, &mut mins);
            let m = |idx: &[usize]| idx.iter().map(|i| ints[*i]).min().unwrap();
            if ints[ASCII] < m(&[BASE256, C40, TEXT, X12, EDIFACT]) {
                return ASCII;
            }
            if ints[BASE256] < ints[ASCII] || ints[BASE256] + 1 < m(&[C40, TEXT, X12, EDIFACT]) {
                return BASE256;
            }
            if ints[EDIFACT] + 1 < m(&[BASE256, C40, TEXT, X12, ASCII]) {
                return EDIFACT;
            }
            if ints[TEXT] + 1 < m(&[BASE256, C40, EDIFACT, X12, ASCII]) {
                return TEXT;
            }
            if ints[X12] + 1 < m(&[BASE256, C40, EDIFACT, TEXT, ASCII]) {
                return X12;
            }
            if ints[C40] + 1 < m(&[ASCII, BASE256, EDIFACT, TEXT]) {
                if ints[C40] < ints[X12] {
                    return C40;
                }
                if ints[C40] == ints[X12] {
                    let mut p = start + processed + 1;
                    while p < msg.len() {
                        let tc = msg[p];
                        if x12_term_sep(tc) {
                            return X12;
                        }
                        if !native_x12(tc) {
                            break;
                        }
                        p += 1;
                    }
                    return C40;
                }
            }
        }
    }
}

fn look_ahead(msg: &[u8], start: usize, current: usize) -> usize {
    let new = look_ahead_intern(msg, start, current);
    if current == X12 && new == X12 {
        let end = (start + 3).min(msg.len());
        if (start..end).any(|i| !native_x12(msg[i])) {
            return ASCII;
        }
    } else if current == EDIFACT && new == EDIFACT {
        let end = (start + 4).min(msg.len());
        if (start..end).any(|i| !native_edifact(msg[i])) {
            return ASCII;
        }
    }
    new
}

fn consecutive_digits(msg: &[u8], start: usize) -> usize {
    msg[start..].iter().take_while(|c| c.is_ascii_digit()).count()
}

fn encode_ascii(ctx: &mut Ctx) -> R<()> {
    let n = consecutive_digits(ctx.msg, ctx.pos);
    if n >= 2 {
        let (a, b) = (ctx.msg[ctx.pos], ctx.msg[ctx.pos + 1]);
        ctx.cw.push((a - b'0') * 10 + (b - b'0') + 130);
        ctx.pos += 2;
        return Ok(());
    }
    let c = ctx.cur();
    let new = look_ahead(ctx.msg, ctx.pos, ASCII);
    if new != ASCII {
        ctx.cw.push(match new {
            BASE256 => LATCH_TO_BASE256,
            C40 => LATCH_TO_C40,
            X12 => LATCH_TO_X12,
            TEXT => LATCH_TO_TEXT,
            _ => LATCH_TO_EDIFACT,
        });
        ctx.signal(new);
    } else if is_ext(c) {
        ctx.cw.push(UPPER_SHIFT);
        ctx.cw.push(c - 128 + 1);
        ctx.pos += 1;
    } else {
        ctx.cw.push(c + 1);
        ctx.pos += 1;
    }
    Ok(())
}

fn c40_char(c: u8, sb: &mut Vec<u8>) -> usize {
    match c {
        b' ' => {
            sb.push(3);
            1
        }
        b'0'..=b'9' => {
            sb.push(c - 48 + 4);
            1
        }
        b'A'..=b'Z' => {
            sb.push(c - 65 + 14);
            1
        }
        0..=31 => {
            sb.extend([0, c]);
            2
        }
        33..=47 => {
            sb.extend([1, c - 33]);
            2
        }
        58..=64 => {
            sb.extend([1, c - 58 + 15]);
            2
        }
        91..=95 => {
            sb.extend([1, c - 91 + 22]);
            2
        }
        96..=127 => {
            sb.extend([2, c - 96]);
            2
        }
        _ => {
            sb.extend([1, 0x1e]);
            2 + c40_char(c - 128, sb)
        }
    }
}

fn text_char(c: u8, sb: &mut Vec<u8>) -> usize {
    match c {
        b' ' => {
            sb.push(3);
            1
        }
        b'0'..=b'9' => {
            sb.push(c - 48 + 4);
            1
        }
        b'a'..=b'z' => {
            sb.push(c - 97 + 14);
            1
        }
        0..=31 => {
            sb.extend([0, c]);
            2
        }
        33..=47 => {
            sb.extend([1, c - 33]);
            2
        }
        58..=64 => {
            sb.extend([1, c - 58 + 15]);
            2
        }
        91..=95 => {
            sb.extend([1, c - 91 + 22]);
            2
        }
        b'`' => {
            sb.extend([2, 0]);
            2
        }
        65..=90 => {
            sb.extend([2, c - 65 + 1]);
            2
        }
        123..=127 => {
            sb.extend([2, c - 123 + 27]);
            2
        }
        _ => {
            sb.extend([1, 0x1e]);
            2 + text_char(c - 128, sb)
        }
    }
}

fn write_triplet(ctx: &mut Ctx, buf: &mut Vec<u8>) {
    let v = 1600 * buf[0] as u32 + 40 * buf[1] as u32 + buf[2] as u32 + 1;
    ctx.cw.push((v / 256) as u8);
    ctx.cw.push((v % 256) as u8);
    buf.drain(..3);
}

/// ZXing `C40Encoder.encode` (also `TextEncoder` with the Text character table).
fn encode_c40(ctx: &mut Ctx, mode: usize) -> R<()> {
    let enc = |c: u8, sb: &mut Vec<u8>| if mode == TEXT { text_char(c, sb) } else { c40_char(c, sb) };
    let mut buf: Vec<u8> = Vec::new();
    while ctx.has_more() {
        let c = ctx.cur();
        ctx.pos += 1;
        let mut last = enc(c, &mut buf);
        let unwritten = buf.len() / 3 * 2;
        let cur = ctx.cw.len() + unwritten;
        ctx.update(cur)?;
        let available = ctx.capacity() - cur as isize;
        if !ctx.has_more() {
            let mut removed = Vec::new();
            let mut backtrack = |ctx: &mut Ctx, buf: &mut Vec<u8>, last: usize| -> usize {
                let n = buf.len();
                buf.truncate(n - last);
                ctx.pos -= 1;
                let c = ctx.cur();
                let l = enc(c, &mut removed);
                ctx.info = None;
                l
            };
            if buf.len() % 3 == 2 && available != 2 {
                last = backtrack(ctx, &mut buf, last);
            }
            while buf.len() % 3 == 1 && (last > 3 || available != 1) {
                last = backtrack(ctx, &mut buf, last);
            }
            break;
        }
        if buf.len() % 3 == 0 {
            let new = look_ahead(ctx.msg, ctx.pos, mode);
            if new != mode {
                ctx.signal(ASCII);
                break;
            }
        }
    }
    c40_eod(ctx, &mut buf)
}

fn c40_eod(ctx: &mut Ctx, buf: &mut Vec<u8>) -> R<()> {
    let unwritten = buf.len() / 3 * 2;
    let rest = buf.len() % 3;
    let cur = ctx.cw.len() + unwritten;
    ctx.update(cur)?;
    let available = ctx.capacity() - cur as isize;
    if rest == 2 {
        buf.push(0);
        while buf.len() >= 3 {
            write_triplet(ctx, buf);
        }
        if ctx.has_more() {
            ctx.cw.push(UNLATCH);
        }
    } else if available == 1 && rest == 1 {
        while buf.len() >= 3 {
            write_triplet(ctx, buf);
        }
        if ctx.has_more() {
            ctx.cw.push(UNLATCH);
        }
        ctx.pos -= 1;
    } else if rest == 0 {
        while buf.len() >= 3 {
            write_triplet(ctx, buf);
        }
        if available > 0 || ctx.has_more() {
            ctx.cw.push(UNLATCH);
        }
    } else {
        return Err("Unexpected case in C40 end of data".into());
    }
    ctx.signal(ASCII);
    Ok(())
}

fn x12_char(c: u8, sb: &mut Vec<u8>) -> R<()> {
    sb.push(match c {
        b'\r' => 0,
        b'*' => 1,
        b'>' => 2,
        b' ' => 3,
        b'0'..=b'9' => c - 48 + 4,
        b'A'..=b'Z' => c - 65 + 14,
        _ => return Err(format!("Illegal character: {}", c as char)),
    });
    Ok(())
}

fn encode_x12(ctx: &mut Ctx) -> R<()> {
    let mut buf = Vec::new();
    while ctx.has_more() {
        let c = ctx.cur();
        ctx.pos += 1;
        x12_char(c, &mut buf)?;
        if buf.len() % 3 == 0 {
            write_triplet(ctx, &mut buf);
            let new = look_ahead(ctx.msg, ctx.pos, X12);
            if new != X12 {
                ctx.signal(ASCII);
                break;
            }
        }
    }
    // handleEOD
    ctx.update_now()?;
    let available = ctx.capacity() - ctx.cw.len() as isize;
    ctx.pos -= buf.len();
    if ctx.remaining() > 1 || available > 1 || ctx.remaining() != available {
        ctx.cw.push(UNLATCH);
    }
    if ctx.new_enc.is_none() {
        ctx.signal(ASCII);
    }
    Ok(())
}

fn edifact_codewords(sb: &[u8]) -> Vec<u8> {
    let g = |i: usize| *sb.get(i).unwrap_or(&0) as u32;
    let v = (g(0) << 18) + (g(1) << 12) + (g(2) << 6) + g(3);
    let all = [(v >> 16 & 255) as u8, (v >> 8 & 255) as u8, (v & 255) as u8];
    all[..sb.len().min(3)].to_vec()
}

fn encode_edifact(ctx: &mut Ctx) -> R<()> {
    let mut buf: Vec<u8> = Vec::new();
    while ctx.has_more() {
        let c = ctx.cur();
        match c {
            b' '..=b'?' => buf.push(c),
            b'@'..=b'^' => buf.push(c - 64),
            _ => return Err(format!("Illegal character: {}", c as char)),
        }
        ctx.pos += 1;
        if buf.len() >= 4 {
            let cw = edifact_codewords(&buf[..4]);
            ctx.cw.extend(cw);
            buf.drain(..4);
            let new = look_ahead(ctx.msg, ctx.pos, EDIFACT);
            if new != EDIFACT {
                ctx.signal(ASCII);
                break;
            }
        }
    }
    buf.push(31);
    let r = edifact_eod(ctx, &buf);
    ctx.signal(ASCII);
    r
}

fn edifact_eod(ctx: &mut Ctx, buf: &[u8]) -> R<()> {
    let count = buf.len();
    if count == 0 {
        return Ok(());
    }
    if count == 1 {
        ctx.update_now()?;
        let mut available = ctx.capacity() - ctx.cw.len() as isize;
        let remaining = ctx.remaining();
        if remaining > available {
            ctx.update(ctx.cw.len() + 1)?;
            available = ctx.capacity() - ctx.cw.len() as isize;
        }
        if remaining <= available && available <= 2 {
            return Ok(());
        }
    }
    if count > 4 {
        return Err("Count must not exceed 4".into());
    }
    let rest = count - 1;
    let encoded = edifact_codewords(buf);
    let end = !ctx.has_more();
    let mut rest_in_ascii = end && rest <= 2;
    if rest <= 2 {
        ctx.update(ctx.cw.len() + rest)?;
        let available = ctx.capacity() - ctx.cw.len() as isize;
        if available >= 3 {
            rest_in_ascii = false;
            ctx.update(ctx.cw.len() + encoded.len())?;
        }
    }
    if rest_in_ascii {
        ctx.info = None;
        ctx.pos -= rest;
    } else {
        ctx.cw.extend(encoded);
    }
    Ok(())
}

fn randomize255(ch: u32, pos: usize) -> u8 {
    let pseudo = (149 * pos as u32) % 255 + 1;
    let t = ch + pseudo;
    if t <= 255 {
        t as u8
    } else {
        (t - 256) as u8
    }
}

fn encode_base256(ctx: &mut Ctx) -> R<()> {
    let mut buf: Vec<u32> = vec![0];
    while ctx.has_more() {
        let c = ctx.cur();
        buf.push(c as u32);
        ctx.pos += 1;
        let new = look_ahead(ctx.msg, ctx.pos, BASE256);
        if new != BASE256 {
            ctx.signal(ASCII);
            break;
        }
    }
    let data = buf.len() - 1;
    let cur = ctx.cw.len() + data + 1;
    ctx.update(cur)?;
    let must_pad = ctx.capacity() - cur as isize > 0;
    if ctx.has_more() || must_pad {
        if data <= 249 {
            buf[0] = data as u32;
        } else if data <= 1555 {
            buf[0] = (data / 250 + 249) as u32;
            buf.insert(1, (data % 250) as u32);
        } else {
            return Err(format!("Message length not in valid ranges: {}", data));
        }
    }
    for v in buf {
        let p = ctx.cw.len() + 1;
        ctx.cw.push(randomize255(v, p));
    }
    Ok(())
}

/// The 253-state pad codeword for 1-based position `pos`.
pub fn randomize253(pos: usize) -> u8 {
    let pseudo = (149 * pos) % 253 + 1;
    let t = PAD as usize + pseudo;
    (if t <= 254 { t } else { t - 254 }) as u8
}

/// Encode a message (bytes as ISO-8859-1 characters) into data codewords, padded to the
/// capacity of the smallest symbol of `shape` that holds them; returns the codewords and
/// that symbol.
pub fn encode_high_level(msg: &[u8], shape: Shape) -> R<(Vec<u8>, Entry)> {
    let mut ctx = Ctx { msg, pos: 0, cw: Vec::new(), skip_at_end: 0, new_enc: None, shape, info: None };
    const H05: &[u8] = b"[)>\x1e05\x1d";
    const H06: &[u8] = b"[)>\x1e06\x1d";
    const TRAILER: &[u8] = b"\x1e\x04";
    if msg.starts_with(H05) && msg.ends_with(TRAILER) && msg.len() >= H05.len() + TRAILER.len() {
        ctx.cw.push(MACRO_05);
        ctx.skip_at_end = 2;
        ctx.pos += H05.len();
    } else if msg.starts_with(H06) && msg.ends_with(TRAILER) && msg.len() >= H06.len() + TRAILER.len() {
        ctx.cw.push(MACRO_06);
        ctx.skip_at_end = 2;
        ctx.pos += H06.len();
    }
    let mut mode = ASCII;
    while ctx.has_more() {
        match mode {
            ASCII => encode_ascii(&mut ctx)?,
            C40 | TEXT => encode_c40(&mut ctx, mode)?,
            X12 => encode_x12(&mut ctx)?,
            EDIFACT => encode_edifact(&mut ctx)?,
            _ => encode_base256(&mut ctx)?,
        }
        if let Some(n) = ctx.new_enc.take() {
            mode = n;
        }
    }
    let len = ctx.cw.len();
    ctx.update_now()?;
    let capacity = ctx.capacity() as usize;
    if len < capacity && mode != ASCII && mode != BASE256 && mode != EDIFACT {
        ctx.cw.push(UNLATCH);
    }
    if ctx.cw.len() < capacity {
        ctx.cw.push(PAD);
    }
    while ctx.cw.len() < capacity {
        let p = ctx.cw.len() + 1;
        ctx.cw.push(randomize253(p));
    }
    Ok((ctx.cw, ctx.info.unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vis(msg: &str) -> String {
        let bytes: Vec<u8> = msg.chars().map(|c| c as u32 as u8).collect();
        let (cw, _) = encode_high_level(&bytes, Shape::Any).unwrap();
        cw.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(" ")
    }

    fn binary_message(len: usize) -> String {
        let mut s = String::from("\u{ab}\u{e4}\u{f6}\u{fc}\u{e9}\u{e0}\u{e1}-");
        for _ in 0..len - 9 {
            s.push('\u{b7}');
        }
        s.push('\u{bb}');
        s
    }

    /// Every encodation through the symbol builder and our decoder.
    #[test]
    fn encodations_round_trip() {
        let msgs: Vec<Vec<u8>> = vec![
            b"AIMAIMAIMAIMAIMAIMAIM".to_vec(),
            b"aimaimaimaimaim text mode 123".to_vec(),
            b"ABC>ABC123>ABCDEF*X12\r".to_vec(),
            b".A.C1.3.DATA.123DATA.123DATA".to_vec(),
            (0u8..=255).collect(),
            "Mixed: AIMAIM aimaim 0123456789 \u{e4}\u{f6}\u{fc} .X.X.X.X >*>".chars().map(|c| c as u32 as u8).collect(),
            b"[)>\x1e05\x1d5555\x1c6666\x1e\x04".to_vec(),
        ];
        for (k, m) in msgs.iter().enumerate() {
            for shape in [Shape::Any, Shape::Square] {
                let sym = super::super::datamatrix::encode_shape(m, shape).unwrap();
                let (d, _) = super::super::datamatrix::decode_matrix(&sym).unwrap();
                let want: Vec<u8> = if k == 6 { b"[)>\x1e05\x1d5555\x1c6666\x1e\x04".to_vec() } else { m.clone() };
                assert_eq!(d, want, "message {}", k);
            }
        }
    }

    /// ZXing `HighLevelEncodeTestCase` vectors (codewords incl. padding).
    #[test]
    fn zxing_high_level_vectors() {
        let cases: &[(&str, &str)] = &[
            ("123456", "142 164 186"),
            ("123456\u{a3}", "142 164 186 235 36"),
            ("30Q324343430794<OQQ", "160 82 162 173 173 173 137 224 61 80 82 82"),
            ("AIMAIMAIM", "230 91 11 91 11 91 11 254"),
            ("AIMAIAB", "230 91 11 90 255 254 67 129"),
            ("AIMAIAb", "66 74 78 66 74 66 99 129"),
            ("AIMAIMAIM\u{cb}", "230 91 11 91 11 91 11 254 235 76"),
            ("AIMAIMAIM\u{eb}", "230 91 11 91 11 91 11 254 235 108"),
            ("A1B2C3D4E5F6G7H8I9J0K1L2", "230 88 88 40 8 107 147 59 67 126 206 78 126 144 121 35 47 254"),
            // (testC40EncodationSpecialCases1 checks its first three cases against a custom test
            // symbol table; its production-table cases follow)
            ("AIMAIMAIMAIMAIMAI", "230 91 11 91 11 91 11 91 11 91 11 254 66 74 129 237"),
            ("AIMAIMAIMA", "230 91 11 91 11 91 11 66"),
            ("AIMAIMAIMAIMAIMAIMAI", "230 91 11 91 11 91 11 91 11 91 11 91 11 254 66 74"),
            ("aimaimaim", "239 91 11 91 11 91 11 254"),
            ("aimaimaim'", "239 91 11 91 11 91 11 254 40 129"),
            ("aimaimaIm", "239 91 11 91 11 87 218 110"),
            ("aimaimaimB", "239 91 11 91 11 91 11 254 67 129"),
            ("aimaimaim{txt}\u{4}", "239 91 11 91 11 91 11 254 124 117 121 117 126 5 129 237"),
            ("ABC>ABC123>AB", "238 89 233 14 192 100 207 44 31 67"),
            ("ABC>ABC123>ABC", "238 89 233 14 192 100 207 44 31 254 67 68"),
            ("ABC>ABC123>ABCD", "238 89 233 14 192 100 207 44 31 96 82 254"),
            ("ABC>ABC123>ABCDE", "238 89 233 14 192 100 207 44 31 96 82 70"),
            ("ABC>ABC123>ABCDEF", "238 89 233 14 192 100 207 44 31 96 82 254 70 71 129 237"),
            (".A.C1.3.DATA.123DATA.123DATA", "240 184 27 131 198 236 238 16 21 1 187 28 179 16 21 1 187 28 179 16 21 1"),
            (".A.C1.3.X.X2..", "240 184 27 131 198 236 238 98 230 50 47 47"),
            (".A.C1.3.X.X2.", "240 184 27 131 198 236 238 98 230 50 47 129"),
            (".A.C1.3.X.X2", "240 184 27 131 198 236 238 98 230 50"),
            (".A.C1.3.X.X", "240 184 27 131 198 236 238 98 230 31"),
            (".A.C1.3.X.", "240 184 27 131 198 236 238 98 231 192"),
            (".A.C1.3.X", "240 184 27 131 198 236 238 89"),
            (
                ".XXX.XXX.XXX.XXX.XXX.XXX.\u{fc}XX.XXX.XXX.XXX.XXX.XXX.XXX",
                "240 185 134 24 185 134 24 185 134 24 185 134 24 185 134 24 185 134 24 124 47 235 125 240 97 139 152 97 139 152 97 139 152 97 139 152 97 139 152 97 139 152 89 89",
            ),
            ("\u{ab}\u{e4}\u{f6}\u{fc}\u{e9}\u{bb}", "231 44 108 59 226 126 1 104"),
            ("\u{ab}\u{e4}\u{f6}\u{fc}\u{e9}\u{e0}\u{bb}", "231 51 108 59 226 126 1 141 254 129"),
            ("\u{ab}\u{e4}\u{f6}\u{fc}\u{e9}\u{e0}\u{e1}\u{bb}", "231 44 108 59 226 126 1 141 36 147"),
            (" 23\u{a3}", "33 153 235 36 129"),
            ("\u{ab}\u{e4}\u{f6}\u{fc}\u{e9}\u{bb} 234", "231 50 108 59 226 126 1 104 33 153 53 129"),
            (
                "\u{ab}\u{e4}\u{f6}\u{fc}\u{e9}\u{bb} 23\u{a3} 1234567890123456789",
                "231 54 108 59 226 126 1 104 99 10 161 167 33 142 164 186 208 220 142 164 186 208 58 129 59 209 104 254 150 45",
            ),
            ("AIMAIMAIMAIMaimaimaim", "230 91 11 91 11 91 11 254 66 74 78 239 91 11 91 11 91 11"),
            ("aimaimaimaim12345678", "239 91 11 91 11 91 11 91 11 254 142 164 186 208 129 237"),
            ("Hello World!", "73 239 116 130 175 123 148 64 158 233 254 34"),
            ("CREX-TAN:h", "68 83 70 89 46 85 66 79 59 105"),
            ("CREX-TAN:hh", "68 83 70 89 46 85 66 79 59 105 105 129"),
            ("CREX-TAN:hhh", "68 83 70 89 46 85 66 79 59 105 105 105"),
            ("*DTCP01", "43 69 85 68 81 131 129 56"),
            ("*DTCP0", "238 9 10 104 141"),
            ("fiykmj*Rh2`,e6", "103 106 122 108 110 107 43 83 105 51 97 45 102 55 129 237"),
            ("[)>\u{1e}05\u{1d}5555\u{1c}6666\u{1e}\u{4}", "236 185 185 29 196 196 129 56"),
            ("*MEMANT-1F-MESTECH", "240 168 209 77 4 229 45 196 107 77 21 53 5 12 135 192"),
        ];
        let mut failed = Vec::new();
        for (msg, want) in cases {
            let got = vis(msg);
            if got != *want {
                failed.push(format!("{:?}: got {} want {}", msg, got, want));
            }
        }
        assert_eq!(vis(&binary_message(20)), "231 44 108 59 226 126 1 141 36 5 37 187 80 230 123 17 166 60 210 103 253 150");
        assert_eq!(vis(&binary_message(19)), "231 63 108 59 226 126 1 141 36 5 37 187 80 230 123 17 166 60 210 103 1 129");
        let l276 = vis(&binary_message(276));
        assert!(l276.starts_with("231 38 219 2 208 120 20 150 35") && l276.ends_with("146 40 194 129"), "{}", l276);
        let l277 = vis(&binary_message(277));
        assert!(l277.starts_with("231 38 220 2 208 120 20 150 35") && l277.ends_with("146 40 190 87"), "{}", l277);
        assert!(failed.is_empty(), "{:#?}", failed);
    }
}
