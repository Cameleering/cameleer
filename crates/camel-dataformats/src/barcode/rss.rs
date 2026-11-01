//! GS1 DataBar (RSS): RSS-14 (omnidirectional, also its stacked variant by collecting the
//! pairs of several rows) and RSS Expanded, ported from ZXing's `RSS14Reader` and
//! `RSSExpandedReader` (finder patterns, data characters with odd/even count adjustment,
//! checksums, the expanded finder sequences and the general-purpose AI decoders with
//! ZXing's `FieldParser`). Like ZXing these are decode-only; the test-only encoders build
//! symbols to round-trip through the readers.

// ---------------------------------------------------------------------------------------
// Pixel row helpers (ZXing BitArray / OneDReader semantics)

fn next_set(row: &[bool], from: usize) -> usize {
    (from..row.len()).find(|i| row[*i]).unwrap_or(row.len())
}

fn next_unset(row: &[bool], from: usize) -> usize {
    (from..row.len()).find(|i| !row[*i]).unwrap_or(row.len())
}

/// ZXing `OneDReader.recordPattern`.
fn record_pattern(row: &[bool], start: usize, n: usize) -> Option<Vec<usize>> {
    let end = row.len();
    if start >= end {
        return None;
    }
    let mut c = vec![0usize; n];
    let mut is_white = !row[start];
    let mut pos = 0;
    let mut i = start;
    while i < end {
        if row[i] != is_white {
            c[pos] += 1;
        } else {
            pos += 1;
            if pos == n {
                break;
            }
            c[pos] = 1;
            is_white = !is_white;
        }
        i += 1;
    }
    (pos == n || (pos == n - 1 && i == end)).then_some(c)
}

/// ZXing `OneDReader.recordPatternInReverse`.
fn record_pattern_in_reverse(row: &[bool], start: usize, n: usize) -> Option<Vec<usize>> {
    if start >= row.len() {
        return None;
    }
    let mut left = n as isize;
    let mut last = row[start];
    let mut s = start;
    while s > 0 && left >= 0 {
        s -= 1;
        if row[s] != last {
            left -= 1;
            last = !last;
        }
    }
    if left >= 0 {
        return None;
    }
    record_pattern(row, s + 1, n)
}

fn pattern_match_variance(c: &[usize], p: &[u8], max_individual: f64) -> f64 {
    super::linear::pattern_variance(c, p, max_individual)
}

const MAX_AVG_VARIANCE: f64 = 0.2;
const MAX_INDIVIDUAL_VARIANCE: f64 = 0.45;

fn parse_finder_value(c: &[usize], patterns: &[[u8; 4]]) -> Option<usize> {
    patterns.iter().position(|p| pattern_match_variance(c, p, MAX_INDIVIDUAL_VARIANCE) < MAX_AVG_VARIANCE)
}

/// ZXing `AbstractRSSReader.isFinderPattern` (with a looser width spread, see below).
fn is_finder_pattern(c: &[usize; 4]) -> bool {
    let first_two = c[0] + c[1];
    let total = first_two + c[2] + c[3];
    let ratio = first_two as f64 / total as f64;
    if (9.5 / 12.0..=12.5 / 14.0).contains(&ratio) {
        let mn = *c.iter().min().unwrap();
        let mx = *c.iter().max().unwrap();
        // ZXing requires max < 10 * min; thresholded photographs thin the one-module light
        // elements next to the eight-module bar, so allow 15 (the finder value match follows)
        return mx < 15 * mn;
    }
    false
}

fn combins(n: i64, r: i64) -> i64 {
    let (max_d, min_d) = if n - r > r { (r, n - r) } else { (n - r, r) };
    let mut val: i64 = 1;
    let mut j = 1;
    let mut i = n;
    while i > min_d {
        val *= i;
        if j <= max_d {
            val /= j;
            j += 1;
        }
        i -= 1;
    }
    while j <= max_d {
        val /= j;
        j += 1;
    }
    val
}

/// ZXing `RSSUtils.getRSSvalue`.
pub fn rss_value(widths: &[u8], max_width: i64, no_narrow: bool) -> i64 {
    let mut n: i64 = widths.iter().map(|w| *w as i64).sum();
    let elements = widths.len() as i64;
    let mut val = 0;
    let mut narrow_mask = 0i64;
    for bar in 0..elements - 1 {
        let mut elm_width = 1;
        narrow_mask |= 1 << bar;
        while elm_width < widths[bar as usize] as i64 {
            let mut sub_val = combins(n - elm_width - 1, elements - bar - 2);
            if no_narrow && narrow_mask == 0 && n - elm_width - (elements - bar - 1) >= elements - bar - 1 {
                sub_val -= combins(n - elm_width - (elements - bar), elements - bar - 2);
            }
            if elements - bar - 1 > 1 {
                let mut less_val = 0;
                let mut mxw = n - elm_width - (elements - bar - 2);
                while mxw > max_width {
                    less_val += combins(n - elm_width - mxw - 1, elements - bar - 3);
                    mxw -= 1;
                }
                sub_val -= less_val * (elements - 1 - bar);
            } else if n - elm_width > max_width {
                sub_val -= 1;
            }
            val += sub_val;
            elm_width += 1;
            narrow_mask &= !(1 << bar);
        }
        n -= elm_width;
    }
    val
}

/// The ISO/IEC 24724 `getRSSwidths` routine: the four element widths summing to `total`
/// with RSS value `value` (the inverse of [`rss_value`]).
pub fn rss_widths(value: i64, total: u8, max_width: u8, no_narrow: bool) -> Option<[u8; 4]> {
    let mut val = value;
    let mut n = total as i64;
    let elements = 4i64;
    let max_width = max_width as i64;
    let mut widths = [0u8; 4];
    let mut narrow_mask = 0i64;
    let mut bar = 0;
    while bar < elements - 1 {
        narrow_mask |= 1 << bar;
        let mut elm_width = 1;
        let mut sub_val;
        loop {
            sub_val = combins(n - elm_width - 1, elements - bar - 2);
            if no_narrow && narrow_mask == 0 && n - elm_width - (elements - bar - 1) >= elements - bar - 1 {
                sub_val -= combins(n - elm_width - (elements - bar), elements - bar - 2);
            }
            if elements - bar - 1 > 1 {
                let mut less_val = 0;
                let mut mxw = n - elm_width - (elements - bar - 2);
                while mxw > max_width {
                    less_val += combins(n - elm_width - mxw - 1, elements - bar - 3);
                    mxw -= 1;
                }
                sub_val -= less_val * (elements - 1 - bar);
            } else if n - elm_width > max_width {
                sub_val -= 1;
            }
            val -= sub_val;
            if val < 0 {
                break;
            }
            elm_width += 1;
            narrow_mask &= !(1 << bar);
            if elm_width > n {
                return None;
            }
        }
        val += sub_val;
        n -= elm_width;
        widths[bar as usize] = elm_width as u8;
        bar += 1;
    }
    if n < 1 {
        return None;
    }
    widths[3] = n as u8;
    Some(widths)
}

/// Round the eight element widths to `modules` modules (ZXing's rounding and
/// `adjustOddEvenCounts`): returns (odd counts, even counts) or None.
fn char_counts(counters: &[usize], modules: usize, rule: &dyn Fn(i32, i32) -> (bool, bool, bool, bool, bool, bool)) -> Option<([i32; 4], [i32; 4])> {
    let element = counters.iter().sum::<usize>() as f64 / modules as f64;
    let mut odd = [0i32; 4];
    let mut even = [0i32; 4];
    let mut odd_err = [0f64; 4];
    let mut even_err = [0f64; 4];
    for (i, c) in counters.iter().enumerate() {
        let v = *c as f64 / element;
        let mut count = (v + 0.5) as i32;
        if count < 1 {
            if v < 0.3 {
                return None;
            }
            count = 1;
        } else if count > 8 {
            if v > 8.7 {
                return None;
            }
            count = 8;
        }
        if i % 2 == 0 {
            odd[i / 2] = count;
            odd_err[i / 2] = v - count as f64;
        } else {
            even[i / 2] = count;
            even_err[i / 2] = v - count as f64;
        }
    }
    let odd_sum: i32 = odd.iter().sum();
    let even_sum: i32 = even.iter().sum();
    // (increment odd, decrement odd, increment even, decrement even, odd parity bad, even parity bad)
    let (mut inc_odd, mut dec_odd, mut inc_even, mut dec_even, odd_bad, even_bad) = rule(odd_sum, even_sum);
    let mismatch = odd_sum + even_sum - modules as i32;
    match mismatch {
        1 => {
            if odd_bad {
                if even_bad {
                    return None;
                }
                dec_odd = true;
            } else {
                if !even_bad {
                    return None;
                }
                dec_even = true;
            }
        }
        -1 => {
            if odd_bad {
                if even_bad {
                    return None;
                }
                inc_odd = true;
            } else {
                if !even_bad {
                    return None;
                }
                inc_even = true;
            }
        }
        0 => {
            if odd_bad {
                if !even_bad {
                    return None;
                }
                if odd_sum < even_sum {
                    inc_odd = true;
                    dec_even = true;
                } else {
                    dec_odd = true;
                    inc_even = true;
                }
            } else if even_bad {
                return None;
            }
        }
        _ => return None,
    }
    let argmax = |e: &[f64; 4]| (0..4).max_by(|a, b| e[*a].partial_cmp(&e[*b]).unwrap()).unwrap();
    let argmin = |e: &[f64; 4]| (0..4).min_by(|a, b| e[*a].partial_cmp(&e[*b]).unwrap()).unwrap();
    if inc_odd {
        if dec_odd {
            return None;
        }
        odd[argmax(&odd_err)] += 1;
    }
    if dec_odd {
        odd[argmin(&odd_err)] -= 1;
    }
    if inc_even {
        if dec_even {
            return None;
        }
        // ZXing passes the odd rounding errors here
        even[argmax(&odd_err)] += 1;
    }
    if dec_even {
        even[argmin(&even_err)] -= 1;
    }
    if odd.iter().chain(even.iter()).any(|v| *v < 1) {
        return None;
    }
    Some((odd, even))
}

fn to_u8(a: &[i32; 4]) -> [u8; 4] {
    [a[0] as u8, a[1] as u8, a[2] as u8, a[3] as u8]
}

// ---------------------------------------------------------------------------------------
// RSS-14

const OUTSIDE_EVEN_TOTAL_SUBSET: [i64; 5] = [1, 10, 34, 70, 126];
const INSIDE_ODD_TOTAL_SUBSET: [i64; 4] = [4, 20, 48, 81];
const OUTSIDE_GSUM: [i64; 5] = [0, 161, 961, 2015, 2715];
const INSIDE_GSUM: [i64; 4] = [0, 336, 1036, 1516];
const OUTSIDE_ODD_WIDEST: [i64; 5] = [8, 6, 4, 3, 1];
const INSIDE_ODD_WIDEST: [i64; 4] = [2, 4, 6, 8];
const RSS14_FINDERS: [[u8; 4]; 9] = [[3, 8, 2, 1], [3, 5, 5, 1], [3, 3, 7, 1], [3, 1, 9, 1], [2, 7, 4, 1], [2, 5, 6, 1], [2, 3, 8, 1], [1, 5, 7, 1], [1, 3, 9, 1]];

/// A decoded half of an RSS-14 symbol.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pair {
    pub value: i64,
    pub checksum: i64,
    pub finder: usize,
}

struct Finder {
    value: usize,
    start: usize,
    end: usize,
}

fn rss14_find_finder(row: &[bool], right: bool, from: usize) -> Option<(usize, usize, [usize; 4])> {
    let width = row.len();
    let mut off = from;
    while off < width {
        let is_white = !row[off];
        if right == is_white {
            break;
        }
        off += 1;
    }
    let mut c = [0usize; 4];
    let mut pos = 0;
    let mut is_white = right;
    let mut start = off;
    for x in off..width {
        if row[x] != is_white {
            c[pos] += 1;
        } else {
            if pos == 3 {
                if is_finder_pattern(&c) {
                    return Some((start, x, c));
                }
                start += c[0] + c[1];
                c = [c[2], c[3], 0, 0];
                pos -= 1;
            } else {
                pos += 1;
            }
            c[pos] = 1;
            is_white = !is_white;
        }
    }
    None
}

fn parse_found_finder(row: &[bool], start: usize, end: usize, c: [usize; 4], patterns: &[[u8; 4]]) -> Option<Finder> {
    let first_is_black = row[start];
    let mut s = start as isize - 1;
    while s >= 0 && first_is_black != row[s as usize] {
        s -= 1;
    }
    let first_start = (s + 1) as usize;
    let first = start - first_start;
    let value = parse_finder_value(&[first, c[0], c[1], c[2]], patterns)?;
    Some(Finder { value, start: first_start, end })
}

fn rss14_char(row: &[bool], f: &Finder, outside: bool) -> Option<(i64, i64)> {
    let counters = if outside {
        record_pattern_in_reverse(row, f.start, 8)?
    } else {
        let mut c = record_pattern(row, f.end, 8)?;
        c.reverse();
        c
    };
    let modules = if outside { 16 } else { 15 };
    let rule = |odd_sum: i32, even_sum: i32| {
        let (mut io, mut d_o, mut ie, mut de) = (false, false, false, false);
        if outside {
            if odd_sum > 12 {
                d_o = true;
            } else if odd_sum < 4 {
                io = true;
            }
            if even_sum > 12 {
                de = true;
            } else if even_sum < 4 {
                ie = true;
            }
        } else {
            if odd_sum > 11 {
                d_o = true;
            } else if odd_sum < 5 {
                io = true;
            }
            if even_sum > 10 {
                de = true;
            } else if even_sum < 4 {
                ie = true;
            }
        }
        (io, d_o, ie, de, (odd_sum & 1) == if outside { 1 } else { 0 }, even_sum & 1 == 1)
    };
    let (odd, even) = char_counts(&counters, modules, &rule)?;
    let mut odd_cs = 0i64;
    let mut even_cs = 0i64;
    for i in (0..4).rev() {
        odd_cs = odd_cs * 9 + odd[i] as i64;
        even_cs = even_cs * 9 + even[i] as i64;
    }
    let checksum = odd_cs + 3 * even_cs;
    let odd_sum: i32 = odd.iter().sum();
    let even_sum: i32 = even.iter().sum();
    if outside {
        if odd_sum & 1 != 0 || !(4..=12).contains(&odd_sum) {
            return None;
        }
        let g = ((12 - odd_sum) / 2) as usize;
        let ow = OUTSIDE_ODD_WIDEST[g];
        let v_odd = rss_value(&to_u8(&odd), ow, false);
        let v_even = rss_value(&to_u8(&even), 9 - ow, true);
        Some((v_odd * OUTSIDE_EVEN_TOTAL_SUBSET[g] + v_even + OUTSIDE_GSUM[g], checksum))
    } else {
        if even_sum & 1 != 0 || !(4..=10).contains(&even_sum) {
            return None;
        }
        let g = ((10 - even_sum) / 2) as usize;
        let ow = INSIDE_ODD_WIDEST[g];
        let v_odd = rss_value(&to_u8(&odd), ow, true);
        let v_even = rss_value(&to_u8(&even), 9 - ow, false);
        Some((v_even * INSIDE_ODD_TOTAL_SUBSET[g] + v_odd + INSIDE_GSUM[g], checksum))
    }
}

/// Every RSS-14 pair found in a row (`right` = the row is reversed and holds a right half).
pub fn rss14_pairs(row: &[bool], right: bool) -> Vec<Pair> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some((s, e, c)) = rss14_find_finder(row, right, from) {
        if let Some(f) = parse_found_finder(row, s, e, c, &RSS14_FINDERS) {
            if let (Some(o), Some(i)) = (rss14_char(row, &f, true), rss14_char(row, &f, false)) {
                let p = Pair { value: 1597 * o.0 + i.0, checksum: o.1 + 4 * i.1, finder: f.value };
                if !out.contains(&p) {
                    out.push(p);
                }
            }
        }
        from = s + c[0] + c[1];
    }
    out
}

fn rss14_check(l: &Pair, r: &Pair) -> bool {
    let check = (l.checksum + 16 * r.checksum) % 79;
    let mut target = 9 * l.finder as i64 + r.finder as i64;
    if target > 72 {
        target -= 1;
    }
    if target > 8 {
        target -= 1;
    }
    check == target
}

fn rss14_text(l: &Pair, r: &Pair) -> String {
    let v = 4537077i64 * l.value + r.value;
    let mut s = format!("{:013}", v);
    let mut check = 0;
    for (i, ch) in s.bytes().enumerate() {
        let d = (ch - b'0') as i64;
        check += if i % 2 == 0 { 3 * d } else { d };
    }
    let check = (10 - check % 10) % 10;
    s.push((b'0' + check as u8) as char);
    s
}

/// RSS-14 pairs collected over several rows (ZXing's `possibleLeftPairs`/`possibleRightPairs`),
/// so stacked symbols, whose halves are on different rows, decode too.
///
/// Like ZXing, a pair must have been seen on more than one row before it is combined
/// (`min_count` 2), which keeps chance checksum matches out.
pub struct Rss14Collector {
    left: Vec<(Pair, usize)>,
    right: Vec<(Pair, usize)>,
    min_count: usize,
}

impl Default for Rss14Collector {
    fn default() -> Rss14Collector {
        Rss14Collector { left: Vec::new(), right: Vec::new(), min_count: 2 }
    }
}

fn tally(list: &mut Vec<(Pair, usize)>, p: Pair) {
    match list.iter_mut().find(|e| e.0 == p) {
        Some(e) => e.1 += 1,
        None => list.push((p, 1)),
    }
}

impl Rss14Collector {
    /// Add a row (read left to right); returns a result once a left and a right pair match.
    pub fn add_row(&mut self, row: &[bool]) -> Option<String> {
        for p in rss14_pairs(row, false) {
            tally(&mut self.left, p);
        }
        let rev: Vec<bool> = row.iter().rev().copied().collect();
        for p in rss14_pairs(&rev, true) {
            tally(&mut self.right, p);
        }
        for (l, lc) in &self.left {
            for (r, rc) in &self.right {
                if *lc >= self.min_count && *rc >= self.min_count && rss14_check(l, r) {
                    return Some(rss14_text(l, r));
                }
            }
        }
        None
    }
}

/// Decode an RSS-14 symbol from one row.
pub fn decode_rss14(row: &[bool]) -> Option<String> {
    Rss14Collector { min_count: 1, ..Rss14Collector::default() }.add_row(row)
}

// ---------------------------------------------------------------------------------------
// RSS Expanded

const SYMBOL_WIDEST: [i64; 5] = [7, 5, 4, 3, 1];
const EVEN_TOTAL_SUBSET: [i64; 5] = [4, 20, 52, 104, 204];
const GSUM: [i64; 5] = [0, 348, 1388, 2948, 3988];
const EXP_FINDERS: [[u8; 4]; 6] = [[1, 8, 4, 1], [3, 6, 4, 1], [3, 4, 6, 1], [3, 2, 8, 1], [2, 6, 5, 1], [2, 2, 9, 1]];
const FINDER_SEQUENCES: [&[usize]; 10] = [
    &[0, 0],
    &[0, 1, 1],
    &[0, 2, 1, 3],
    &[0, 4, 1, 3, 2],
    &[0, 4, 1, 3, 3, 5],
    &[0, 4, 1, 3, 4, 5, 5],
    &[0, 0, 1, 1, 2, 2, 3, 3],
    &[0, 0, 1, 1, 2, 2, 3, 4, 4],
    &[0, 0, 1, 1, 2, 2, 3, 4, 5, 5],
    &[0, 0, 1, 1, 2, 3, 3, 4, 4, 5, 5],
];

/// The checksum weights: row `r`, column `c` is 3^(8r + c) mod 211 (ZXing's WEIGHTS table).
fn weight(row: usize, col: usize) -> i64 {
    let mut w = 1i64;
    for _ in 0..8 * row + col {
        w = w * 3 % 211;
    }
    w
}

#[derive(Clone, Copy, Debug)]
struct DataChar {
    value: i64,
    checksum: i64,
}

struct ExpPair {
    left: DataChar,
    right: Option<DataChar>,
    finder: usize,
    end: usize,
}

fn exp_find_next(row: &[bool], from: usize, even: bool) -> Option<(usize, usize, [usize; 4])> {
    let width = row.len();
    let mut off = from;
    while off < width && !row[off] {
        off += 1;
    }
    let mut c = [0usize; 4];
    let mut pos = 0;
    let mut is_white = false;
    let mut start = off;
    for x in off..width {
        if row[x] != is_white {
            c[pos] += 1;
        } else {
            if pos == 3 {
                let mut t = c;
                if even {
                    t.reverse();
                }
                if is_finder_pattern(&t) {
                    return Some((start, x, t));
                }
                start += c[0] + c[1];
                c = [c[2], c[3], 0, 0];
                pos -= 1;
            } else {
                pos += 1;
            }
            c[pos] = 1;
            is_white = !is_white;
        }
    }
    None
}

fn exp_parse_finder(row: &[bool], start: usize, end: usize, c: [usize; 4], odd: bool) -> Option<Finder> {
    let (first, s, e) = if odd {
        let mut p = start as isize - 1;
        while p >= 0 && !row[p as usize] {
            p -= 1;
        }
        let fs = (p + 1) as usize;
        (start - fs, fs, end)
    } else {
        let e = next_unset(row, end + 1);
        (e - end, start, e)
    };
    let value = parse_finder_value(&[first, c[0], c[1], c[2]], &EXP_FINDERS)?;
    Some(Finder { value, start: s, end: e })
}

fn exp_char(row: &[bool], f: &Finder, odd: bool, left: bool) -> Option<DataChar> {
    let counters = if left {
        record_pattern_in_reverse(row, f.start, 8)?
    } else {
        let mut c = record_pattern(row, f.end, 8)?;
        c.reverse();
        c
    };
    let element = counters.iter().sum::<usize>() as f64 / 17.0;
    let expected = (f.end - f.start) as f64 / 15.0;
    if (element - expected).abs() / expected > 0.3 {
        return None;
    }
    let rule = |odd_sum: i32, even_sum: i32| {
        let (mut io, mut d_o, mut ie, mut de) = (false, false, false, false);
        if odd_sum > 13 {
            d_o = true;
        } else if odd_sum < 4 {
            io = true;
        }
        if even_sum > 13 {
            de = true;
        } else if even_sum < 4 {
            ie = true;
        }
        (io, d_o, ie, de, odd_sum & 1 == 1, even_sum & 1 == 0)
    };
    let (oddc, evenc) = char_counts(&counters, 17, &rule)?;
    let weighted = !(f.value == 0 && odd && left);
    let wrow = (4 * f.value + if odd { 0 } else { 2 } + if left { 0 } else { 1 }) as isize - 1;
    let mut checksum = 0;
    if weighted && wrow >= 0 {
        for i in 0..4 {
            checksum += oddc[i] as i64 * weight(wrow as usize, 2 * i) + evenc[i] as i64 * weight(wrow as usize, 2 * i + 1);
        }
    }
    let odd_sum: i32 = oddc.iter().sum();
    if odd_sum & 1 != 0 || !(4..=13).contains(&odd_sum) {
        return None;
    }
    let g = ((13 - odd_sum) / 2) as usize;
    let ow = SYMBOL_WIDEST[g];
    let v_odd = rss_value(&to_u8(&oddc), ow, true);
    let v_even = rss_value(&to_u8(&evenc), 9 - ow, false);
    Some(DataChar { value: v_odd * EVEN_TOTAL_SUBSET[g] + v_even + GSUM[g], checksum })
}

fn next_second_bar(row: &[bool], p: usize) -> usize {
    if p < row.len() && row[p] {
        next_set(row, next_unset(row, p))
    } else {
        next_set(row, next_unset(row, next_set(row, p)))
    }
}

fn exp_next_pair(row: &[bool], pairs: &[ExpPair]) -> Option<ExpPair> {
    let odd = pairs.len() % 2 == 0;
    let mut from = pairs.last().map(|p| p.end).unwrap_or(0);
    loop {
        let (s, e, c) = exp_find_next(row, from, !odd)?;
        match exp_parse_finder(row, s, e, c, odd) {
            Some(f) => {
                let left = exp_char(row, &f, odd, true)?;
                if pairs.last().is_some_and(|p| p.right.is_none()) {
                    return None;
                }
                let right = exp_char(row, &f, odd, false);
                return Some(ExpPair { left, right, finder: f.value, end: f.end });
            }
            None => {
                let n = next_second_bar(row, s);
                if n <= from || n >= row.len() {
                    return None;
                }
                from = n;
            }
        }
    }
}

fn exp_checksum_ok(pairs: &[ExpPair]) -> bool {
    let first = &pairs[0];
    let Some(fc) = first.right else { return false };
    let mut checksum = fc.checksum;
    let mut s = 2;
    for p in &pairs[1..] {
        checksum += p.left.checksum;
        s += 1;
        if let Some(r) = p.right {
            checksum += r.checksum;
            s += 1;
        }
    }
    checksum %= 211;
    211 * (s - 4) + checksum == first.left.value
}

fn exp_valid_sequence(pairs: &[ExpPair]) -> bool {
    FINDER_SEQUENCES.iter().any(|seq| seq.len() == pairs.len() && pairs.iter().zip(seq.iter()).all(|(p, v)| p.finder == *v))
}

/// Decode an RSS Expanded symbol from one row.
pub fn decode_rss_expanded(row: &[bool]) -> Option<String> {
    let mut pairs: Vec<ExpPair> = Vec::new();
    while let Some(p) = exp_next_pair(row, &pairs) {
        pairs.push(p);
        if pairs.len() > 11 {
            return None;
        }
    }
    if pairs.is_empty() || !exp_checksum_ok(&pairs) || !exp_valid_sequence(&pairs) {
        return None;
    }
    let mut bits = Vec::new();
    let mut push = |v: i64| {
        for i in (0..12).rev() {
            bits.push(v >> i & 1 == 1);
        }
    };
    push(pairs[0].right?.value);
    for p in &pairs[1..] {
        push(p.left.value);
        if let Some(r) = p.right {
            push(r.value);
        }
    }
    expanded::parse(&bits)
}

/// The expanded data decoders (ZXing `com.google.zxing.oned.rss.expanded.decoders`).
pub mod expanded {
    fn num(b: &[bool], pos: usize, bits: usize) -> Option<u32> {
        if pos + bits > b.len() {
            return None;
        }
        Some(b[pos..pos + bits].iter().fold(0u32, |a, x| (a << 1) | *x as u32))
    }

    /// Decode the binary data of an expanded symbol into its GS1 element string.
    pub fn parse(b: &[bool]) -> Option<String> {
        if b.len() < 3 {
            return None;
        }
        if b[1] {
            // AI01AndOtherAIs
            let mut buf = String::from("(01)");
            let initial = buf.len();
            buf.push_str(&num(b, 4, 4)?.to_string());
            gtin_without_ai(b, &mut buf, 8, initial)?;
            return decode_all_codes(b, buf, 48);
        }
        if !b[2] {
            // AnyAIDecoder
            return decode_all_codes(b, String::new(), 5);
        }
        match num(b, 1, 4)? {
            4 => return x0x(b, |buf, w| {
                buf.push_str("(3103)");
                w
            }),
            5 => return x0x(b, |buf, w| {
                buf.push_str(if w < 10000 { "(3202)" } else { "(3203)" });
                if w < 10000 {
                    w
                } else {
                    w - 10000
                }
            }),
            _ => {}
        }
        match num(b, 1, 5)? {
            12 => return ai0139x(b, false),
            13 => return ai0139x(b, true),
            _ => {}
        }
        let (first, date) = match num(b, 1, 7)? {
            56 => ("310", "11"),
            57 => ("320", "11"),
            58 => ("310", "13"),
            59 => ("320", "13"),
            60 => ("310", "15"),
            61 => ("320", "15"),
            62 => ("310", "17"),
            63 => ("320", "17"),
            _ => return None,
        };
        // AI013x0x1xDecoder
        if b.len() != 8 + 40 + 20 + 16 {
            return None;
        }
        let mut buf = String::new();
        compressed_gtin(b, &mut buf, 8)?;
        let w = num(b, 48, 20)?;
        buf.push_str(&format!("({}{})", first, w / 100000));
        weight_digits(&mut buf, w % 100000);
        let mut d = num(b, 68, 16)?;
        if d != 38400 {
            buf.push_str(&format!("({})", date));
            let day = d % 32;
            d /= 32;
            let month = d % 12 + 1;
            d /= 12;
            buf.push_str(&format!("{:02}{:02}{:02}", d, month, day));
        }
        Some(buf)
    }

    fn weight_digits(buf: &mut String, w: u32) {
        let mut div = 100000;
        for _ in 0..5 {
            if w / div == 0 {
                buf.push('0');
            }
            div /= 10;
        }
        buf.push_str(&w.to_string());
    }

    fn x0x(b: &[bool], code: impl Fn(&mut String, u32) -> u32) -> Option<String> {
        if b.len() != 5 + 40 + 15 {
            return None;
        }
        let mut buf = String::new();
        compressed_gtin(b, &mut buf, 5)?;
        let w = num(b, 45, 15)?;
        let w = code(&mut buf, w);
        weight_digits(&mut buf, w);
        Some(buf)
    }

    fn ai0139x(b: &[bool], with_currency: bool) -> Option<String> {
        if b.len() < 8 + 40 {
            return None;
        }
        let mut buf = String::new();
        compressed_gtin(b, &mut buf, 8)?;
        let last = num(b, 48, 2)?;
        let mut pos = 50;
        if with_currency {
            buf.push_str(&format!("(393{})", last));
            let first3 = num(b, 50, 10)?;
            buf.push_str(&format!("{:03}", first3));
            pos = 60;
        } else {
            buf.push_str(&format!("(392{})", last));
        }
        let mut st = State::default();
        let info = general_field(b, &mut st, pos, None)?;
        buf.push_str(&info.text);
        Some(buf)
    }

    fn compressed_gtin(b: &[bool], buf: &mut String, pos: usize) -> Option<()> {
        buf.push_str("(01)");
        let initial = buf.len();
        buf.push('9');
        gtin_without_ai(b, buf, pos, initial)
    }

    fn gtin_without_ai(b: &[bool], buf: &mut String, pos: usize, initial: usize) -> Option<()> {
        for i in 0..4 {
            let block = num(b, pos + 10 * i, 10)?;
            buf.push_str(&format!("{:03}", block));
        }
        let digits: Vec<u32> = buf[initial..initial + 13].bytes().map(|c| (c - b'0') as u32).collect();
        let s: u32 = digits.iter().enumerate().map(|(i, d)| if i % 2 == 0 { 3 * d } else { *d }).sum();
        buf.push((b'0' + ((10 - s % 10) % 10) as u8) as char);
        Some(())
    }

    #[derive(Clone, Copy, Default, PartialEq, Eq)]
    enum Mode {
        #[default]
        Numeric,
        Alpha,
        Iso,
    }

    #[derive(Default)]
    struct State {
        pos: usize,
        mode: Mode,
    }

    struct Info {
        pos: usize,
        text: String,
        remaining: Option<u32>,
    }

    const FNC1: u32 = 10;

    fn decode_all_codes(b: &[bool], mut buf: String, initial: usize) -> Option<String> {
        let mut pos = initial;
        let mut remaining: Option<String> = None;
        let mut st = State::default();
        loop {
            let info = general_field(b, &mut st, pos, remaining.take())?;
            if let Some(parsed) = super::field_parser::parse(&info.text)? {
                buf.push_str(&parsed);
            }
            remaining = info.remaining.map(|r| r.to_string());
            if pos == info.pos {
                break;
            }
            pos = info.pos;
        }
        Some(buf)
    }

    fn general_field(b: &[bool], st: &mut State, pos: usize, remaining: Option<String>) -> Option<Info> {
        let mut buf = remaining.unwrap_or_default();
        st.pos = pos;
        let last = parse_blocks(b, st, &mut buf)?;
        Some(match last {
            Some((_, Some(r))) => Info { pos: st.pos, text: buf, remaining: Some(r) },
            _ => Info { pos: st.pos, text: buf, remaining: None },
        })
    }

    /// Returns the final block's decoded information (finished flag, remaining digit).
    #[allow(clippy::type_complexity)]
    fn parse_blocks(b: &[bool], st: &mut State, buf: &mut String) -> Option<Option<(bool, Option<u32>)>> {
        loop {
            let initial = st.pos;
            let (finished, info) = match st.mode {
                Mode::Alpha => alpha_block(b, st, buf)?,
                Mode::Iso => iso_block(b, st, buf)?,
                Mode::Numeric => numeric_block(b, st, buf)?,
            };
            let changed = initial != st.pos;
            if finished {
                return Some(info.map(|r| (true, r)));
            }
            if !changed {
                return Some(None);
            }
        }
    }

    fn still_numeric(b: &[bool], pos: usize) -> bool {
        if pos + 7 > b.len() {
            return pos + 4 <= b.len();
        }
        b[pos..pos + 3].iter().any(|x| *x) || b[pos + 3]
    }

    /// (new position, digit 1, digit 2) with 10 = FNC1.
    fn decode_numeric(b: &[bool], pos: usize) -> Option<(usize, u32, u32)> {
        if pos + 7 > b.len() {
            let n = num(b, pos, 4)?;
            if n == 0 {
                return Some((b.len(), FNC1, FNC1));
            }
            return Some((b.len(), n - 1, FNC1));
        }
        let n = num(b, pos, 7)?;
        if n < 8 {
            return None;
        }
        Some((pos + 7, (n - 8) / 11, (n - 8) % 11))
    }

    // (finished, Some(remaining digit) for a finished block with a pending digit)
    type Block = (bool, Option<Option<u32>>);

    fn numeric_block(b: &[bool], st: &mut State, buf: &mut String) -> Option<Block> {
        while still_numeric(b, st.pos) {
            let (np, d1, d2) = decode_numeric(b, st.pos)?;
            st.pos = np;
            if d1 == FNC1 {
                let rem = if d2 == FNC1 { None } else { Some(d2) };
                return Some((true, Some(rem)));
            }
            buf.push((b'0' + d1 as u8) as char);
            if d2 == FNC1 {
                return Some((true, Some(None)));
            }
            buf.push((b'0' + d2 as u8) as char);
        }
        if numeric_to_alpha_latch(b, st.pos) {
            st.mode = Mode::Alpha;
            st.pos += 4;
        }
        Some((false, None))
    }

    fn iso_block(b: &[bool], st: &mut State, buf: &mut String) -> Option<Block> {
        while still_iso(b, st.pos) {
            let (np, c) = decode_iso(b, st.pos)?;
            st.pos = np;
            match c {
                None => return Some((true, Some(None))),
                Some(c) => buf.push(c),
            }
        }
        if alpha_or_iso_to_numeric_latch(b, st.pos) {
            st.pos += 3;
            st.mode = Mode::Numeric;
        } else if alpha_to_iso_to_alpha_latch(b, st.pos) {
            st.pos = if st.pos + 5 < b.len() { st.pos + 5 } else { b.len() };
            st.mode = Mode::Alpha;
        }
        Some((false, None))
    }

    fn alpha_block(b: &[bool], st: &mut State, buf: &mut String) -> Option<Block> {
        while still_alpha(b, st.pos) {
            let (np, c) = decode_alpha(b, st.pos)?;
            st.pos = np;
            match c {
                None => return Some((true, Some(None))),
                Some(c) => buf.push(c),
            }
        }
        if alpha_or_iso_to_numeric_latch(b, st.pos) {
            st.pos += 3;
            st.mode = Mode::Numeric;
        } else if alpha_to_iso_to_alpha_latch(b, st.pos) {
            st.pos = if st.pos + 5 < b.len() { st.pos + 5 } else { b.len() };
            st.mode = Mode::Iso;
        }
        Some((false, None))
    }

    fn still_iso(b: &[bool], pos: usize) -> bool {
        let Some(v5) = num(b, pos, 5) else { return false };
        if (5..16).contains(&v5) {
            return true;
        }
        let Some(v7) = num(b, pos, 7) else { return false };
        if (64..116).contains(&v7) {
            return true;
        }
        num(b, pos, 8).is_some_and(|v8| (232..253).contains(&v8))
    }

    /// (new position, char) with None = FNC1.
    fn decode_iso(b: &[bool], pos: usize) -> Option<(usize, Option<char>)> {
        let v5 = num(b, pos, 5)?;
        if v5 == 15 {
            return Some((pos + 5, None));
        }
        if (5..15).contains(&v5) {
            return Some((pos + 5, Some((b'0' + v5 as u8 - 5) as char)));
        }
        let v7 = num(b, pos, 7)?;
        if (64..90).contains(&v7) {
            return Some((pos + 7, Some((v7 as u8 + 1) as char)));
        }
        if (90..116).contains(&v7) {
            return Some((pos + 7, Some((v7 as u8 + 7) as char)));
        }
        let c = match num(b, pos, 8)? {
            232 => '!',
            233 => '"',
            234 => '%',
            235 => '&',
            236 => '\'',
            237 => '(',
            238 => ')',
            239 => '*',
            240 => '+',
            241 => ',',
            242 => '-',
            243 => '.',
            244 => '/',
            245 => ':',
            246 => ';',
            247 => '<',
            248 => '=',
            249 => '>',
            250 => '?',
            251 => '_',
            252 => ' ',
            _ => return None,
        };
        Some((pos + 8, Some(c)))
    }

    fn still_alpha(b: &[bool], pos: usize) -> bool {
        let Some(v5) = num(b, pos, 5) else { return false };
        if (5..16).contains(&v5) {
            return true;
        }
        num(b, pos, 6).is_some_and(|v6| (16..63).contains(&v6))
    }

    fn decode_alpha(b: &[bool], pos: usize) -> Option<(usize, Option<char>)> {
        let v5 = num(b, pos, 5)?;
        if v5 == 15 {
            return Some((pos + 5, None));
        }
        if (5..15).contains(&v5) {
            return Some((pos + 5, Some((b'0' + v5 as u8 - 5) as char)));
        }
        let v6 = num(b, pos, 6)?;
        if (32..58).contains(&v6) {
            return Some((pos + 6, Some((v6 as u8 + 33) as char)));
        }
        let c = match v6 {
            58 => '*',
            59 => ',',
            60 => '-',
            61 => '.',
            62 => '/',
            _ => return None,
        };
        Some((pos + 6, Some(c)))
    }

    fn alpha_to_iso_to_alpha_latch(b: &[bool], pos: usize) -> bool {
        if pos + 1 > b.len() {
            return false;
        }
        for i in 0..5 {
            if i + pos >= b.len() {
                break;
            }
            if i == 2 {
                if !b[pos + 2] {
                    return false;
                }
            } else if b[pos + i] {
                return false;
            }
        }
        true
    }

    fn alpha_or_iso_to_numeric_latch(b: &[bool], pos: usize) -> bool {
        pos + 3 <= b.len() && !b[pos..pos + 3].iter().any(|x| *x)
    }

    fn numeric_to_alpha_latch(b: &[bool], pos: usize) -> bool {
        if pos + 1 > b.len() {
            return false;
        }
        (0..4).take_while(|i| i + pos < b.len()).all(|i| !b[pos + i])
    }
}

/// ZXing `FieldParser`: parenthesise the application identifiers of an element string.
pub mod field_parser {
    /// (AI, variable length?, data length)
    const TWO: &[(&str, bool, usize)] = &[
        ("00", false, 18),
        ("01", false, 14),
        ("02", false, 14),
        ("10", true, 20),
        ("11", false, 6),
        ("12", false, 6),
        ("13", false, 6),
        ("15", false, 6),
        ("17", false, 6),
        ("20", false, 2),
        ("21", true, 20),
        ("22", true, 29),
        ("30", true, 8),
        ("37", true, 8),
        ("90", true, 30),
        ("91", true, 30),
        ("92", true, 30),
        ("93", true, 30),
        ("94", true, 30),
        ("95", true, 30),
        ("96", true, 30),
        ("97", true, 30),
        ("98", true, 30),
        ("99", true, 30),
    ];
    const THREE: &[(&str, bool, usize)] = &[
        ("240", true, 30),
        ("241", true, 30),
        ("242", true, 6),
        ("250", true, 30),
        ("251", true, 30),
        ("253", true, 17),
        ("254", true, 20),
        ("400", true, 30),
        ("401", true, 30),
        ("402", false, 17),
        ("403", true, 30),
        ("410", false, 13),
        ("411", false, 13),
        ("412", false, 13),
        ("413", false, 13),
        ("414", false, 13),
        ("420", true, 20),
        ("421", true, 15),
        ("422", false, 3),
        ("423", true, 15),
        ("424", false, 3),
        ("425", false, 3),
        ("426", false, 3),
    ];
    const THREE_PLUS: &[(&str, bool, usize)] = &[
        ("310", false, 6),
        ("311", false, 6),
        ("312", false, 6),
        ("313", false, 6),
        ("314", false, 6),
        ("315", false, 6),
        ("316", false, 6),
        ("320", false, 6),
        ("321", false, 6),
        ("322", false, 6),
        ("323", false, 6),
        ("324", false, 6),
        ("325", false, 6),
        ("326", false, 6),
        ("327", false, 6),
        ("328", false, 6),
        ("329", false, 6),
        ("330", false, 6),
        ("331", false, 6),
        ("332", false, 6),
        ("333", false, 6),
        ("334", false, 6),
        ("335", false, 6),
        ("336", false, 6),
        ("340", false, 6),
        ("341", false, 6),
        ("342", false, 6),
        ("343", false, 6),
        ("344", false, 6),
        ("345", false, 6),
        ("346", false, 6),
        ("347", false, 6),
        ("348", false, 6),
        ("349", false, 6),
        ("350", false, 6),
        ("351", false, 6),
        ("352", false, 6),
        ("353", false, 6),
        ("354", false, 6),
        ("355", false, 6),
        ("356", false, 6),
        ("357", false, 6),
        ("360", false, 6),
        ("361", false, 6),
        ("362", false, 6),
        ("363", false, 6),
        ("364", false, 6),
        ("365", false, 6),
        ("366", false, 6),
        ("367", false, 6),
        ("368", false, 6),
        ("369", false, 6),
        ("390", true, 15),
        ("391", true, 18),
        ("392", true, 15),
        ("393", true, 18),
        ("703", true, 30),
    ];
    const FOUR: &[(&str, bool, usize)] = &[
        ("7001", false, 13),
        ("7002", true, 30),
        ("7003", false, 10),
        ("8001", false, 14),
        ("8002", true, 20),
        ("8003", true, 30),
        ("8004", true, 30),
        ("8005", false, 6),
        ("8006", false, 18),
        ("8007", true, 30),
        ("8008", true, 12),
        ("8018", false, 18),
        ("8020", true, 25),
        ("8100", false, 6),
        ("8101", false, 10),
        ("8102", false, 2),
        ("8110", true, 70),
        ("8200", true, 70),
    ];

    fn find(table: &[(&str, bool, usize)], key: &str) -> Option<(bool, usize)> {
        table.iter().find(|e| e.0 == key).map(|e| (e.1, e.2))
    }

    /// `Ok(None)` for an empty string, `None` when the string does not parse.
    pub fn parse(raw: &str) -> Option<Option<String>> {
        if raw.is_empty() {
            return Some(None);
        }
        if raw.len() < 2 || !raw.is_char_boundary(2) {
            return None;
        }
        if let Some((var, len)) = find(TWO, &raw[..2]) {
            return process(2, var, len, raw).map(Some);
        }
        if raw.len() < 3 || !raw.is_char_boundary(3) {
            return None;
        }
        if let Some((var, len)) = find(THREE, &raw[..3]) {
            return process(3, var, len, raw).map(Some);
        }
        if raw.len() < 4 || !raw.is_char_boundary(4) {
            return None;
        }
        if let Some((var, len)) = find(THREE_PLUS, &raw[..3]) {
            return process(4, var, len, raw).map(Some);
        }
        if let Some((var, len)) = find(FOUR, &raw[..4]) {
            return process(4, var, len, raw).map(Some);
        }
        None
    }

    fn process(ai_size: usize, variable: bool, len: usize, raw: &str) -> Option<String> {
        let ai = &raw[..ai_size];
        let end = if variable {
            raw.len().min(ai_size + len)
        } else {
            if raw.len() < ai_size + len {
                return None;
            }
            ai_size + len
        };
        if !raw.is_char_boundary(end) {
            return None;
        }
        let field = &raw[ai_size..end];
        let rest = &raw[end..];
        let mut out = format!("({}){}", ai, field);
        if let Some(p) = parse(rest)? {
            out.push_str(&p);
        }
        Some(out)
    }
}

// ---------------------------------------------------------------------------------------
// Test-only encoders (ZXing has no RSS writers)

#[cfg(test)]
pub mod encode {
    use super::*;

    fn outside_counters(v: i64) -> ([u8; 4], [u8; 4]) {
        let g = (0..5).rev().find(|g| OUTSIDE_GSUM[*g] <= v).unwrap();
        let rem = v - OUTSIDE_GSUM[g];
        let t_even = OUTSIDE_EVEN_TOTAL_SUBSET[g];
        let (v_odd, v_even) = (rem / t_even, rem % t_even);
        let odd_sum = 12 - 2 * g as u8;
        let ow = OUTSIDE_ODD_WIDEST[g] as u8;
        (rss_widths(v_odd, odd_sum, ow, false).unwrap(), rss_widths(v_even, 16 - odd_sum, 9 - ow, true).unwrap())
    }

    fn inside_counters(v: i64) -> ([u8; 4], [u8; 4]) {
        let g = (0..4).rev().find(|g| INSIDE_GSUM[*g] <= v).unwrap();
        let rem = v - INSIDE_GSUM[g];
        let t_odd = INSIDE_ODD_TOTAL_SUBSET[g];
        let (v_even, v_odd) = (rem / t_odd, rem % t_odd);
        let even_sum = 10 - 2 * g as u8;
        let ow = INSIDE_ODD_WIDEST[g] as u8;
        (rss_widths(v_odd, 15 - even_sum, ow, true).unwrap(), rss_widths(v_even, even_sum, 9 - ow, false).unwrap())
    }

    fn interleave(odd: [u8; 4], even: [u8; 4]) -> Vec<u8> {
        (0..8).map(|i| if i % 2 == 0 { odd[i / 2] } else { even[i / 2] }).collect()
    }

    fn rss14_cs(odd: [u8; 4], even: [u8; 4]) -> i64 {
        let mut o = 0i64;
        let mut e = 0i64;
        for i in (0..4).rev() {
            o = o * 9 + odd[i] as i64;
            e = e * 9 + even[i] as i64;
        }
        o + 3 * e
    }

    fn elements_to_modules(el: &[u8]) -> Vec<bool> {
        let mut out = Vec::new();
        for (i, w) in el.iter().enumerate() {
            out.extend(std::iter::repeat_n(i % 2 == 1, *w as usize));
        }
        out
    }

    /// RSS-14 modules (starting with the light module of the left guard) for 13 or 14 digits.
    pub fn rss14(text: &str) -> Vec<bool> {
        let v: i64 = text[..13].parse().unwrap();
        let halves = [v / 4537077, v % 4537077];
        let mut half_el = Vec::new();
        let mut cs = Vec::new();
        for h in halves {
            let (oo, oe) = outside_counters(h / 1597);
            let (io, ie) = inside_counters(h % 1597);
            cs.push(rss14_cs(oo, oe) + 4 * rss14_cs(io, ie));
            half_el.push((interleave(oo, oe), interleave(io, ie)));
        }
        let check = (cs[0] + 16 * cs[1]) % 79;
        let (f1, f2) = (0..9)
            .flat_map(|a| (0..9).map(move |b| (a, b)))
            .find(|(a, b)| {
                let mut t = 9 * a + b;
                if t > 72 {
                    t -= 1;
                }
                if t > 8 {
                    t -= 1;
                }
                t == check
            })
            .unwrap();
        let build = |(outer, inner): &(Vec<u8>, Vec<u8>), f: usize| -> Vec<u8> {
            let mut e = vec![1, 1];
            e.extend(outer);
            e.extend(RSS14_FINDERS[f as usize]);
            e.push(1);
            e.extend(inner.iter().rev());
            e
        };
        let mut el = build(&half_el[0], f1 as usize);
        let mut right = build(&half_el[1], f2 as usize);
        right.reverse();
        el.extend(right);
        elements_to_modules(&el)
    }

    fn exp_counters(v: i64) -> ([u8; 4], [u8; 4]) {
        let g = (0..5).rev().find(|g| GSUM[*g] <= v).unwrap();
        let rem = v - GSUM[g];
        let t_even = EVEN_TOTAL_SUBSET[g];
        let (v_odd, v_even) = (rem / t_even, rem % t_even);
        let odd_sum = 12 - 2 * g as u8;
        let ow = SYMBOL_WIDEST[g] as u8;
        (rss_widths(v_odd, odd_sum, ow, true).unwrap(), rss_widths(v_even, 17 - odd_sum, 9 - ow, false).unwrap())
    }

    /// RSS Expanded modules for already encoded binary data (12-bit symbol characters).
    pub fn rss_expanded_bits(bits: &[bool]) -> Vec<bool> {
        let mut bits = bits.to_vec();
        let n_data = bits.len().div_ceil(12).max(3);
        bits.resize(n_data * 12, false);
        let values: Vec<i64> = bits.chunks(12).map(|c| c.iter().fold(0i64, |a, b| (a << 1) | *b as i64)).collect();
        let total = n_data + 1; // with the check character
        let n_pairs = total.div_ceil(2);
        let seq = FINDER_SEQUENCES[n_pairs - 2];
        // characters in order: check, d0, d1, ...; character k sits in pair k/2, left if k even
        let mut chars: Vec<([u8; 4], [u8; 4])> = vec![([0; 4], [0; 4])];
        let mut checksum = 0i64;
        for (k, v) in values.iter().enumerate() {
            let ci = k + 1;
            let (o, e) = exp_counters(*v);
            let pair = ci / 2;
            let odd = pair % 2 == 0;
            let left = ci % 2 == 0;
            let row = 4 * seq[pair] + if odd { 0 } else { 2 } + if left { 0 } else { 1 } - 1;
            for i in 0..4 {
                checksum += o[i] as i64 * weight(row, 2 * i) + e[i] as i64 * weight(row, 2 * i + 1);
            }
            chars.push((o, e));
        }
        let check_value = 211 * (total as i64 - 4) + checksum % 211;
        chars[0] = exp_counters(check_value);
        let mut el: Vec<u8> = vec![1, 1];
        for p in 0..n_pairs {
            let odd = p % 2 == 0;
            let (lo, le) = chars[2 * p];
            el.extend(interleave(lo, le));
            let f = EXP_FINDERS[seq[p]];
            if odd {
                el.extend(f);
                el.push(1);
            } else {
                el.push(1);
                el.extend(f.iter().rev());
            }
            if let Some((ro, re)) = chars.get(2 * p + 1) {
                el.extend(interleave(*ro, *re).iter().rev());
            }
        }
        // right guard: end on a dark module
        if el.len() % 2 == 1 {
            el.push(1);
        } else {
            el.extend([1, 1]);
        }
        elements_to_modules(&el)
    }

    /// General-purpose encodation of an element string without parentheses; `|` is FNC1
    /// (after variable-length fields). Numeric pairs, alphanumeric and ISO/IEC 646 runs.
    pub fn general(raw: &str, bits: &mut Vec<bool>) {
        let put = |bits: &mut Vec<bool>, v: u32, n: usize| {
            for i in (0..n).rev() {
                bits.push(v >> i & 1 == 1);
            }
        };
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        let mut mode = 'n';
        let is_alpha = |c: char| c.is_ascii_digit() || c.is_ascii_uppercase() || "*,-./|".contains(c);
        while i < chars.len() {
            let c = chars[i];
            let digitish = |c: char| c.is_ascii_digit() || c == '|';
            if digitish(c) && chars.get(i + 1).is_some_and(|d| digitish(*d)) && !(c == '|' && chars[i + 1] == '|') {
                if mode != 'n' {
                    put(bits, 0, 3);
                    mode = 'n';
                }
                let v = |c: char| if c == '|' { 10 } else { c.to_digit(10).unwrap() };
                put(bits, 11 * v(c) + v(chars[i + 1]) + 8, 7);
                i += 2;
                continue;
            }
            if mode == 'n' && c.is_ascii_digit() && i + 1 == chars.len() {
                put(bits, 11 * c.to_digit(10).unwrap() + 10 + 8, 7);
                i += 1;
                continue;
            }
            if mode == 'n' {
                put(bits, 0, 4);
                mode = 'a';
            }
            if mode == 'a' && !is_alpha(c) {
                put(bits, 0b00100, 5);
                mode = 'i';
            }
            if c == '|' {
                put(bits, 15, 5);
            } else if c.is_ascii_digit() {
                put(bits, c.to_digit(10).unwrap() + 5, 5);
            } else if mode == 'a' {
                match c {
                    'A'..='Z' => put(bits, c as u32 - 33, 6),
                    '*' => put(bits, 58, 6),
                    ',' => put(bits, 59, 6),
                    '-' => put(bits, 60, 6),
                    '.' => put(bits, 61, 6),
                    _ => put(bits, 62, 6),
                }
            } else {
                match c {
                    'A'..='Z' => put(bits, c as u32 - 1, 7),
                    'a'..='z' => put(bits, c as u32 - 7, 7),
                    _ => {
                        let v = "!\"%&'()*+,-./:;<=>?_ ".find(c).unwrap() as u32 + 232;
                        put(bits, v, 8);
                    }
                }
            }
            i += 1;
        }
    }

    /// RSS Expanded with the AnyAI method (`00`): an element string without parentheses.
    pub fn rss_expanded_any(raw: &str) -> Vec<bool> {
        let mut bits = vec![false, false, false, false, false];
        general(raw, &mut bits);
        rss_expanded_bits(&bits)
    }

    /// RSS Expanded with method `0100`: (01) GTIN (13 digits, no check) and (3103) weight.
    pub fn rss_expanded_3103(gtin12: &str, weight: u32) -> Vec<bool> {
        let mut bits = vec![false, false, true, false, false];
        let d: Vec<u32> = gtin12.bytes().map(|c| (c - b'0') as u32).collect();
        for k in 0..4 {
            let v = d[3 * k] * 100 + d[3 * k + 1] * 10 + d[3 * k + 2];
            for i in (0..10).rev() {
                bits.push(v >> i & 1 == 1);
            }
        }
        for i in (0..15).rev() {
            bits.push(weight >> i & 1 == 1);
        }
        rss_expanded_bits(&bits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every character value of every group maps to widths of the group's sums and back
    /// (checks the subset/widest/gsum tables against the width routines).
    #[test]
    fn rss_value_tables() {
        let check = |t: i64, total: u8, max: u8, no_narrow: bool| {
            for v in 0..t {
                let w = rss_widths(v, total, max, no_narrow).unwrap_or_else(|| panic!("{} {} {} {}", v, total, max, no_narrow));
                assert_eq!(w.iter().map(|x| *x as u32).sum::<u32>(), total as u32);
                assert!(w.iter().all(|x| *x >= 1 && *x <= max));
                assert_eq!(rss_value(&w, max as i64, no_narrow), v);
            }
        };
        for g in 0..5 {
            let ow = OUTSIDE_ODD_WIDEST[g] as u8;
            let odd = 12 - 2 * g as u8;
            let t = if g < 4 { OUTSIDE_GSUM[g + 1] } else { 2841 } - OUTSIDE_GSUM[g];
            let te = OUTSIDE_EVEN_TOTAL_SUBSET[g];
            assert_eq!(t % te, 0);
            check(t / te, odd, ow, false);
            check(te, 16 - odd, 9 - ow, true);
        }
        for g in 0..4 {
            let ow = INSIDE_ODD_WIDEST[g] as u8;
            let even = 10 - 2 * g as u8;
            let t = if g < 3 { INSIDE_GSUM[g + 1] } else { 1597 } - INSIDE_GSUM[g];
            let to = INSIDE_ODD_TOTAL_SUBSET[g];
            assert_eq!(t % to, 0);
            check(to, 15 - even, ow, true);
            check(t / to, even, 9 - ow, false);
        }
        for g in 0..4 {
            let ow = SYMBOL_WIDEST[g] as u8;
            let odd = 12 - 2 * g as u8;
            let t = GSUM[g + 1] - GSUM[g];
            let te = EVEN_TOTAL_SUBSET[g];
            assert_eq!(t % te, 0);
            check(t / te, odd, ow, true);
            check(te, 17 - odd, 9 - ow, false);
        }
        assert_eq!(weight(0, 5), 32);
        assert_eq!(weight(22, 7), 89);
    }

    fn scaled(m: &[bool], k: usize) -> Vec<bool> {
        let mut row = vec![false; 12 * k];
        row.extend(m.iter().flat_map(|b| std::iter::repeat_n(*b, k)));
        row.extend(vec![false; 12 * k]);
        row
    }

    #[test]
    fn rss14_round_trip() {
        for t in ["0441234567890", "0001234567890", "9999999999999", "0000000000000", "2001234567890"] {
            let m = encode::rss14(t);
            assert_eq!(m.len(), 96);
            let d = decode_rss14(&scaled(&m, 3)).unwrap();
            assert_eq!(&d[..13], t);
            assert_eq!(d.len(), 14);
        }
        assert_eq!(decode_rss14(&scaled(&encode::rss14("0441234567890"), 2)).unwrap(), "04412345678909");
    }

    #[test]
    fn rss_expanded_round_trip() {
        for raw in ["1012A", "10ABC-123|21XYZ", "11100224171102243102000100", "90abc!xyz", "0198898765432106"] {
            let m = encode::rss_expanded_any(raw);
            let d = decode_rss_expanded(&scaled(&m, 3)).unwrap_or_else(|| panic!("{}", raw));
            let want = field_parser_expect(raw);
            assert_eq!(d, want, "{}", raw);
        }
        let m = encode::rss_expanded_3103("001234567890", 1750);
        assert_eq!(decode_rss_expanded(&scaled(&m, 3)).unwrap(), "(01)90012345678908(3103)001750");
    }

    fn field_parser_expect(raw: &str) -> String {
        raw.split('|').map(|p| field_parser::parse(p).unwrap().unwrap()).collect()
    }
}
