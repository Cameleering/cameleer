//! Aztec high-level encoder, ported from ZXing's `aztec.encoder.HighLevelEncoder`: an optimal
//! (shortest) bit sequence found by keeping every non-dominated encoder state (mode, binary
//! shift run, bit count) per input character, with latches, shifts, the four two-character
//! punctuation codes and binary shift, ties resolved in ZXing's order so the output matches
//! ZXing bit for bit.

use std::rc::Rc;

const UPPER: usize = 0;
const LOWER: usize = 1;
const DIGIT: usize = 2;
const MIXED: usize = 3;
const PUNCT: usize = 4;

/// (bits << 16) | code, per [from][to] mode.
const LATCH: [[u32; 5]; 5] = [
    [0, (5 << 16) + 28, (5 << 16) + 30, (5 << 16) + 29, (10 << 16) + (29 << 5) + 30],
    [(9 << 16) + (30 << 4) + 14, 0, (5 << 16) + 30, (5 << 16) + 29, (10 << 16) + (29 << 5) + 30],
    [(4 << 16) + 14, (9 << 16) + (14 << 5) + 28, 0, (9 << 16) + (14 << 5) + 29, (14 << 16) + (14 << 10) + (29 << 5) + 30],
    [(5 << 16) + 29, (5 << 16) + 28, (10 << 16) + (29 << 5) + 30, 0, (5 << 16) + 30],
    [(5 << 16) + 31, (10 << 16) + (31 << 5) + 28, (10 << 16) + (31 << 5) + 30, (10 << 16) + (31 << 5) + 29, 0],
];

fn shift_code(from: usize, to: usize) -> i32 {
    match (from, to) {
        (UPPER, PUNCT) | (LOWER, PUNCT) | (MIXED, PUNCT) | (DIGIT, PUNCT) => 0,
        (LOWER, UPPER) => 28,
        (DIGIT, UPPER) => 15,
        _ => -1,
    }
}

fn char_map() -> [[u8; 256]; 5] {
    let mut m = [[0u8; 256]; 5];
    m[UPPER][b' ' as usize] = 1;
    for c in b'A'..=b'Z' {
        m[UPPER][c as usize] = c - b'A' + 2;
    }
    m[LOWER][b' ' as usize] = 1;
    for c in b'a'..=b'z' {
        m[LOWER][c as usize] = c - b'a' + 2;
    }
    m[DIGIT][b' ' as usize] = 1;
    for c in b'0'..=b'9' {
        m[DIGIT][c as usize] = c - b'0' + 2;
    }
    m[DIGIT][b',' as usize] = 12;
    m[DIGIT][b'.' as usize] = 13;
    let mixed: [u8; 28] = [0, b' ', 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 27, 28, 29, 30, 31, b'@', b'\\', b'^', b'_', b'`', b'|', b'~', 127];
    for (i, c) in mixed.iter().enumerate() {
        m[MIXED][*c as usize] = i as u8;
    }
    let punct: [u8; 31] = [0, b'\r', 0, 0, 0, 0, b'!', b'"', b'#', b'$', b'%', b'&', b'\'', b'(', b')', b'*', b'+', b',', b'-', b'.', b'/', b':', b';', b'<', b'=', b'>', b'?', b'[', b']', b'{', b'}'];
    for (i, c) in punct.iter().enumerate() {
        if *c > 0 {
            m[PUNCT][*c as usize] = i as u8;
        }
    }
    m
}

enum Tok {
    Simple { value: u32, bits: u32 },
    Binary { start: usize, count: usize },
}

struct Token {
    tok: Tok,
    prev: Option<Rc<Token>>,
}

fn add(prev: &Option<Rc<Token>>, value: u32, bits: u32) -> Option<Rc<Token>> {
    Some(Rc::new(Token { tok: Tok::Simple { value, bits }, prev: prev.clone() }))
}

#[derive(Clone)]
struct State {
    token: Option<Rc<Token>>,
    mode: usize,
    bs_bytes: usize,
    bits: u32,
}

fn bs_cost(n: usize) -> i64 {
    if n > 62 {
        21
    } else if n > 31 {
        20
    } else if n > 0 {
        10
    } else {
        0
    }
}

impl State {
    fn latch_and_append(&self, mode: usize, value: u32) -> State {
        let mut bits = self.bits;
        let mut token = self.token.clone();
        if mode != self.mode {
            let l = LATCH[self.mode][mode];
            token = add(&token, l & 0xFFFF, l >> 16);
            bits += l >> 16;
        }
        let n = if mode == DIGIT { 4 } else { 5 };
        token = add(&token, value, n);
        State { token, mode, bs_bytes: 0, bits: bits + n }
    }

    fn shift_and_append(&self, mode: usize, value: u32) -> State {
        let n = if self.mode == DIGIT { 4 } else { 5 };
        let mut token = add(&self.token, shift_code(self.mode, mode) as u32, n);
        token = add(&token, value, 5);
        State { token, mode: self.mode, bs_bytes: 0, bits: self.bits + n + 5 }
    }

    fn add_binary_shift_char(&self, index: usize) -> State {
        let mut token = self.token.clone();
        let mut mode = self.mode;
        let mut bits = self.bits;
        if mode == PUNCT || mode == DIGIT {
            let l = LATCH[mode][UPPER];
            token = add(&token, l & 0xFFFF, l >> 16);
            bits += l >> 16;
            mode = UPPER;
        }
        let delta = match self.bs_bytes {
            0 | 31 => 18,
            62 => 9,
            _ => 8,
        };
        let r = State { token, mode, bs_bytes: self.bs_bytes + 1, bits: bits + delta };
        if r.bs_bytes == 2047 + 31 {
            return r.end_binary_shift(index + 1);
        }
        r
    }

    fn end_binary_shift(&self, index: usize) -> State {
        if self.bs_bytes == 0 {
            return self.clone();
        }
        let token = Some(Rc::new(Token { tok: Tok::Binary { start: index - self.bs_bytes, count: self.bs_bytes }, prev: self.token.clone() }));
        State { token, mode: self.mode, bs_bytes: 0, bits: self.bits }
    }

    fn better_or_equal(&self, other: &State) -> bool {
        let mut n = self.bits as i64 + (LATCH[self.mode][other.mode] >> 16) as i64;
        if self.bs_bytes < other.bs_bytes {
            n += bs_cost(other.bs_bytes) - bs_cost(self.bs_bytes);
        } else if self.bs_bytes > other.bs_bytes && other.bs_bytes > 0 {
            n += 10;
        }
        n <= other.bits as i64
    }

    fn append_flg_n(&self, eci: u32) -> State {
        let r = self.shift_and_append(PUNCT, 0);
        let digits = eci.to_string();
        let mut token = add(&r.token, digits.len() as u32, 3);
        for d in digits.bytes() {
            token = add(&token, (d - b'0') as u32 + 2, 4);
        }
        State { token, mode: self.mode, bs_bytes: 0, bits: r.bits + 3 + 4 * digits.len() as u32 }
    }

    fn to_bits(&self, text: &[u8]) -> Vec<bool> {
        let end = self.end_binary_shift(text.len());
        let mut toks = Vec::new();
        let mut t = end.token.clone();
        while let Some(tk) = t {
            t = tk.prev.clone();
            toks.push(tk);
        }
        let mut out = Vec::new();
        let push = |out: &mut Vec<bool>, v: u32, n: u32| {
            for i in (0..n).rev() {
                out.push(v >> i & 1 == 1);
            }
        };
        for tk in toks.iter().rev() {
            match tk.tok {
                Tok::Simple { value, bits } => push(&mut out, value, bits),
                Tok::Binary { start, count } => {
                    for i in 0..count {
                        if i == 0 || (i == 31 && count <= 62) {
                            push(&mut out, 31, 5);
                            if count > 62 {
                                push(&mut out, (count - 31) as u32, 16);
                            } else if i == 0 {
                                push(&mut out, count.min(31) as u32, 5);
                            } else {
                                push(&mut out, (count - 31) as u32, 5);
                            }
                        }
                        push(&mut out, text[start + i] as u32, 8);
                    }
                }
            }
        }
        out
    }
}

/// ZXing `simplifyStates`: drop dominated states; survivors are prepended (deque order
/// matters for ties).
fn simplify(states: Vec<State>) -> Vec<State> {
    let mut result: std::collections::VecDeque<State> = std::collections::VecDeque::new();
    for s in states {
        let mut add = true;
        let mut i = 0;
        while i < result.len() {
            if result[i].better_or_equal(&s) {
                add = false;
                break;
            }
            if s.better_or_equal(&result[i]) {
                result.remove(i);
            } else {
                i += 1;
            }
        }
        if add {
            result.push_front(s);
        }
    }
    result.into_iter().collect()
}

/// Encode bytes (ISO-8859-1 text, or other bytes behind an ECI) to the Aztec data bits.
pub fn encode(text: &[u8], eci: Option<u32>) -> Vec<bool> {
    let cm = char_map();
    let mut initial = State { token: None, mode: UPPER, bs_bytes: 0, bits: 0 };
    if let Some(e) = eci {
        initial = initial.append_flg_n(e);
    }
    let mut states = vec![initial];
    let mut index = 0;
    while index < text.len() {
        let next = text.get(index + 1).copied().unwrap_or(0);
        let pair = match text[index] {
            b'\r' if next == b'\n' => 2,
            b'.' if next == b' ' => 3,
            b',' if next == b' ' => 4,
            b':' if next == b' ' => 5,
            _ => 0,
        };
        let mut result = Vec::new();
        if pair > 0 {
            for st in &states {
                let nb = st.end_binary_shift(index);
                result.push(nb.latch_and_append(PUNCT, pair));
                if st.mode != PUNCT {
                    result.push(nb.shift_and_append(PUNCT, pair));
                }
                if pair == 3 || pair == 4 {
                    result.push(nb.latch_and_append(DIGIT, 16 - pair).latch_and_append(DIGIT, 1));
                }
                if st.bs_bytes > 0 {
                    result.push(st.add_binary_shift_char(index).add_binary_shift_char(index + 1));
                }
            }
            states = simplify(result);
            index += 2;
            continue;
        }
        let ch = text[index] as usize;
        for st in &states {
            let in_current = cm[st.mode][ch] > 0;
            let mut no_binary: Option<State> = None;
            for mode in 0..=PUNCT {
                let v = cm[mode][ch];
                if v > 0 {
                    let nb = no_binary.get_or_insert_with(|| st.end_binary_shift(index)).clone();
                    if !in_current || mode == st.mode || mode == DIGIT {
                        result.push(nb.latch_and_append(mode, v as u32));
                    }
                    if !in_current && shift_code(st.mode, mode) >= 0 {
                        result.push(nb.shift_and_append(mode, v as u32));
                    }
                }
            }
            if st.bs_bytes > 0 || cm[st.mode][ch] == 0 {
                result.push(st.add_binary_shift_char(index));
            }
        }
        states = simplify(result);
        index += 1;
    }
    // Collections.min: the first state with the fewest bits
    let mut best = &states[0];
    for s in &states[1..] {
        if s.bits < best.bits {
            best = s;
        }
    }
    best.to_bits(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_str(b: &[bool]) -> String {
        b.iter().map(|x| if *x { 'X' } else { '.' }).collect()
    }

    fn latin1(s: &str) -> Vec<u8> {
        s.chars().map(|c| c as u32 as u8).collect()
    }

    /// ZXing `EncoderTest.testEncode1`: the complete compact 3-layer symbol, module for module.
    #[test]
    fn zxing_symbol_vector() {
        let expected = [
            "X     X X       X     X X     X     X         ",
            "X         X     X X     X   X X   X X       X ",
            "X X   X X X X X   X X X                 X     ",
            "X X                 X X   X       X X X X X X ",
            "    X X X   X   X     X X X X         X X     ",
            "  X X X   X X X X   X     X   X     X X   X   ",
            "        X X X X X     X X X X   X   X     X   ",
            "X       X   X X X X X X X X X X X     X   X X ",
            "X   X     X X X               X X X X   X X   ",
            "X     X X   X X   X X X X X   X X   X   X X X ",
            "X   X         X   X       X   X X X X       X ",
            "X       X     X   X   X   X   X   X X   X     ",
            "      X   X X X   X       X   X     X X X     ",
            "    X X X X X X   X X X X X   X X X X X X   X ",
            "  X X   X   X X               X X X   X X X X ",
            "  X   X       X X X X X X X X X X X X   X X   ",
            "  X X   X       X X X   X X X       X X       ",
            "  X               X   X X     X     X X X     ",
            "  X   X X X   X X   X   X X X X   X   X X X X ",
            "    X   X   X X X   X   X   X X X X     X     ",
            "        X               X                 X   ",
            "        X X     X   X X   X   X   X       X X ",
            "  X   X   X X       X   X         X X X     X ",
        ];
        let m = super::super::aztec::encode("This is an example Aztec symbol for Wikipedia.", 33).unwrap();
        assert_eq!((m.width, m.height), (23, 23));
        for (y, row) in expected.iter().enumerate() {
            let got: String = (0..23).map(|x| if m.get(x, y) { "X " } else { "  " }).collect();
            assert_eq!(&got, row, "row {}", y);
        }
    }

    /// ZXing `EncoderTest.testHighLevelEncode*` vectors.
    #[test]
    fn zxing_vectors() {
        let cases: &[(&str, &str)] = &[
            ("A. b.", "...X. ..... ...XX XXX.. ...XX XXXX. XX.X"),
            ("Lorem ipsum.", ".XX.X XXX.. X.... X..XX ..XX. .XXX. ....X .X.X. X...X X.X.. X.XX. .XXX. XXXX. XX.X"),
            ("Lo. Test 123.", ".XX.X XXX.. X.... ..... ...XX XXX.. X.X.X ..XX. X.X.. X.X.X  XXXX. ...X ..XX .X.. .X.X XX.X"),
            ("Lo...x", ".XX.X XXX.. X.... XXXX. XX.X XX.X XX.X XXX. XXX.. XX..X"),
            (". x://abc/.", "..... ...XX XXX.. XX..X ..... X.X.X ..... X.X.. ..... X.X.. ...X. ...XX ..X.. ..... X.X.. XXXX. XX.X"),
            ("ABCdEFG", "...X. ...XX ..X.. XXXXX ....X .XX..X.. ..XX. ..XXX .X..."),
            ("N\u{0}N", ".XXXX XXXXX ....X ........ .XXXX"),
            ("N\u{0}n", ".XXXX XXXXX ...X. ........ .XX.XXX."),
            ("N\u{0}\u{80} A", ".XXXX XXXXX ...X. ........ X....... ....X ...X."),
            ("\u{0}a\u{ff}\u{80} A", "XXXXX ..X.. ........ .XX....X XXXXXXXX X....... ....X ...X."),
            ("1234\u{0}", "XXXX. ..XX .X.. .X.X .XX. XXX. XXXXX ....X ........"),
            ("ABC. DEF\r\n", "...X. ...XX ..X.. ..... ...XX ..X.X ..XX. ..XXX ..... ...X."),
            ("A. : , \r\n", "...X. XXX.X XXXX. ...XX ..X.X  ..X.. ...X."),
            ("A. 1234", "...X. XXXX. XX.X ...X ..XX .X.. .X.X .X X."),
            ("A\u{80}. \u{80}", "...X. XXXXX ..X.. X....... ..X.XXX. ..X..... X......."),
        ];
        let mut failed = Vec::new();
        for (text, want) in cases {
            let got = bits_str(&encode(&latin1(text), None));
            let want: String = want.chars().filter(|c| *c != ' ').collect();
            if got != want {
                failed.push(format!("{:?}: got {} want {}", text, got, want));
            }
        }
        assert!(failed.is_empty(), "{:#?}", failed);
        let long = "09  UAG    ^160MEUCIQC0sYS/HpKxnBELR1uB85R20OoqqwFGa0q2uEiYgh6utAIgLl1aBVM4EOTQtMQQYH9M2Z3Dp4qnA/fwWuQ+M8L3V8U=";
        assert_eq!(encode(long.as_bytes(), None).len(), 823);
        // binary shift lengths
        let sb: Vec<u8> = (0..3000).map(|i| 128 + (i % 30) as u8).collect();
        for i in [1usize, 2, 3, 10, 29, 30, 31, 32, 33, 60, 61, 62, 63, 64, 2076, 2077, 2078, 2079, 2080, 2100] {
            let expected = 8 * i + if i <= 31 { 10 } else if i <= 62 { 20 } else if i <= 2078 { 21 } else { 31 };
            assert_eq!(encode(&sb[..i], None).len(), expected, "{}", i);
            if i != 1 && i != 32 && i != 2079 {
                let mut a = vec![b'a'];
                a.extend_from_slice(&sb[..i - 1]);
                assert_eq!(encode(&a, None).len(), expected, "a+{}", i);
                let mut b = sb[..i - 1].to_vec();
                b.push(b'a');
                assert_eq!(encode(&b, None).len(), expected, "{}+a", i);
            }
            let mut c = vec![b'a'];
            c.extend_from_slice(&sb[..i]);
            c.push(b'b');
            assert_eq!(encode(&c, None).len(), expected + 15, "a+{}+b", i);
        }
        let sect = |n: usize, at: usize| -> Vec<u8> {
            let mut v = vec![0xa7u8; n];
            v[at] = b'A';
            v
        };
        assert_eq!(encode(&sect(32, 1), None).len(), 5 + 20 + 31 * 8);
        assert_eq!(encode(&sect(31, 1), None).len(), 10 + 31 * 8);
        assert_eq!(encode(&sect(34, 1), None).len(), 20 + 34 * 8);
        assert_eq!(encode(&sect(64, 30), None).len(), 21 + 64 * 8);
    }
}
