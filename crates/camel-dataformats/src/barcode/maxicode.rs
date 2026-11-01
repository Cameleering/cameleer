//! MaxiCode (ISO/IEC 16023), decode only like ZXing (`MaxiCodeReader`): the 30x33 hexagonal
//! module grid is read through ZXing's bit-number table, the primary message (mode, and for
//! modes 2/3 the structured carrier postcode, country and service class) and the secondary
//! message are Reed-Solomon corrected over GF(64) (even/odd interleaved), then decoded with
//! the five code sets A-E (shifts, latches, lock, numeric shift NS). ZXing reads only "pure"
//! images (the symbol is sampled over the bounding box of the dark pixels); the camera path
//! additionally locates the bull's-eye and the six orientation clusters (see [`detect`]).

use super::aztec::GfN;
use super::detect::Bin;
use super::Matrix;

pub const WIDTH: usize = 30;
pub const HEIGHT: usize = 33;

/// ZXing `maxicode.decoder.BitMatrixParser.BITNR`: the message bit carried by each module
/// (-1/-2: orientation modules light/dark, -3: unused / finder).
#[rustfmt::skip]
pub const BITNR: [[i16; 30]; 33] = [
    [121,120,127,126,133,132,139,138,145,144,151,150,157,156,163,162,169,168,175,174,181,180,187,186,193,192,199,198,-2,-2],
    [123,122,129,128,135,134,141,140,147,146,153,152,159,158,165,164,171,170,177,176,183,182,189,188,195,194,201,200,816,-3],
    [125,124,131,130,137,136,143,142,149,148,155,154,161,160,167,166,173,172,179,178,185,184,191,190,197,196,203,202,818,817],
    [283,282,277,276,271,270,265,264,259,258,253,252,247,246,241,240,235,234,229,228,223,222,217,216,211,210,205,204,819,-3],
    [285,284,279,278,273,272,267,266,261,260,255,254,249,248,243,242,237,236,231,230,225,224,219,218,213,212,207,206,821,820],
    [287,286,281,280,275,274,269,268,263,262,257,256,251,250,245,244,239,238,233,232,227,226,221,220,215,214,209,208,822,-3],
    [289,288,295,294,301,300,307,306,313,312,319,318,325,324,331,330,337,336,343,342,349,348,355,354,361,360,367,366,824,823],
    [291,290,297,296,303,302,309,308,315,314,321,320,327,326,333,332,339,338,345,344,351,350,357,356,363,362,369,368,825,-3],
    [293,292,299,298,305,304,311,310,317,316,323,322,329,328,335,334,341,340,347,346,353,352,359,358,365,364,371,370,827,826],
    [409,408,403,402,397,396,391,390,79,78,-2,-2,13,12,37,36,2,-1,44,43,109,108,385,384,379,378,373,372,828,-3],
    [411,410,405,404,399,398,393,392,81,80,40,-2,15,14,39,38,3,-1,-1,45,111,110,387,386,381,380,375,374,830,829],
    [413,412,407,406,401,400,395,394,83,82,41,-3,-3,-3,-3,-3,5,4,47,46,113,112,389,388,383,382,377,376,831,-3],
    [415,414,421,420,427,426,103,102,55,54,16,-3,-3,-3,-3,-3,-3,-3,20,19,85,84,433,432,439,438,445,444,833,832],
    [417,416,423,422,429,428,105,104,57,56,-3,-3,-3,-3,-3,-3,-3,-3,22,21,87,86,435,434,441,440,447,446,834,-3],
    [419,418,425,424,431,430,107,106,59,58,-3,-3,-3,-3,-3,-3,-3,-3,-3,23,89,88,437,436,443,442,449,448,836,835],
    [481,480,475,474,469,468,48,-2,30,-3,-3,-3,-3,-3,-3,-3,-3,-3,-3,0,53,52,463,462,457,456,451,450,837,-3],
    [483,482,477,476,471,470,49,-1,-2,-3,-3,-3,-3,-3,-3,-3,-3,-3,-3,-3,-2,-1,465,464,459,458,453,452,839,838],
    [485,484,479,478,473,472,51,50,31,-3,-3,-3,-3,-3,-3,-3,-3,-3,-3,1,-2,42,467,466,461,460,455,454,840,-3],
    [487,486,493,492,499,498,97,96,61,60,-3,-3,-3,-3,-3,-3,-3,-3,-3,26,91,90,505,504,511,510,517,516,842,841],
    [489,488,495,494,501,500,99,98,63,62,-3,-3,-3,-3,-3,-3,-3,-3,28,27,93,92,507,506,513,512,519,518,843,-3],
    [491,490,497,496,503,502,101,100,65,64,17,-3,-3,-3,-3,-3,-3,-3,18,29,95,94,509,508,515,514,521,520,845,844],
    [559,558,553,552,547,546,541,540,73,72,32,-3,-3,-3,-3,-3,-3,10,67,66,115,114,535,534,529,528,523,522,846,-3],
    [561,560,555,554,549,548,543,542,75,74,-2,-1,7,6,35,34,11,-2,69,68,117,116,537,536,531,530,525,524,848,847],
    [563,562,557,556,551,550,545,544,77,76,-2,33,9,8,25,24,-1,-2,71,70,119,118,539,538,533,532,527,526,849,-3],
    [565,564,571,570,577,576,583,582,589,588,595,594,601,600,607,606,613,612,619,618,625,624,631,630,637,636,643,642,851,850],
    [567,566,573,572,579,578,585,584,591,590,597,596,603,602,609,608,615,614,621,620,627,626,633,632,639,638,645,644,852,-3],
    [569,568,575,574,581,580,587,586,593,592,599,598,605,604,611,610,617,616,623,622,629,628,635,634,641,640,647,646,854,853],
    [727,726,721,720,715,714,709,708,703,702,697,696,691,690,685,684,679,678,673,672,667,666,661,660,655,654,649,648,855,-3],
    [729,728,723,722,717,716,711,710,705,704,699,698,693,692,687,686,681,680,675,674,669,668,663,662,657,656,651,650,857,856],
    [731,730,725,724,719,718,713,712,707,706,701,700,695,694,689,688,683,682,677,676,671,670,665,664,659,658,653,652,858,-3],
    [733,732,739,738,745,744,751,750,757,756,763,762,769,768,775,774,781,780,787,786,793,792,799,798,805,804,811,810,860,859],
    [735,734,741,740,747,746,753,752,759,758,765,764,771,770,777,776,783,782,789,788,795,794,801,800,807,806,813,812,861,-3],
    [737,736,743,742,749,748,755,754,761,760,767,766,773,772,779,778,785,784,791,790,797,796,803,802,809,808,815,814,863,862],
];

const SHIFTA: char = '\u{FFF0}';
const SHIFTB: char = '\u{FFF1}';
const SHIFTC: char = '\u{FFF2}';
const SHIFTD: char = '\u{FFF3}';
const SHIFTE: char = '\u{FFF4}';
const TWOSHIFTA: char = '\u{FFF5}';
const THREESHIFTA: char = '\u{FFF6}';
const LATCHA: char = '\u{FFF7}';
const LATCHB: char = '\u{FFF8}';
const LOCK: char = '\u{FFF9}';
const ECI: char = '\u{FFFA}';
const NS: char = '\u{FFFB}';
const PAD: char = '\u{FFFC}';
const FS: char = '\u{1C}';
const GS: char = '\u{1D}';
const RS: char = '\u{1E}';

/// The five code sets (ZXing `DecodedBitStreamParser.SETS`), 64 characters each.
pub fn sets() -> [Vec<char>; 5] {
    let s = |parts: &[&str]| -> Vec<char> { parts.concat().chars().collect() };
    let c = |ch: char| ch.to_string();
    let (eci, fs, gs, rs, ns, pad) = (c(ECI), c(FS), c(GS), c(RS), c(NS), c(PAD));
    let (sa, sb, sc, sd, se) = (c(SHIFTA), c(SHIFTB), c(SHIFTC), c(SHIFTD), c(SHIFTE));
    let (la, lb, lock, two, three) = (c(LATCHA), c(LATCHB), c(LOCK), c(TWOSHIFTA), c(THREESHIFTA));
    [
        s(&["\nABCDEFGHIJKLMNOPQRSTUVWXYZ", &eci, &fs, &gs, &rs, &ns, " ", &pad, "\"#$%&'()*+,-./0123456789:", &sb, &sc, &sd, &se, &lb]),
        s(&["`abcdefghijklmnopqrstuvwxyz", &eci, &fs, &gs, &rs, &ns, "{", &pad, "}~\u{7F};<=>?[\\]^_ ,./:@!|", &pad, &two, &three, &pad, &sa, &sc, &sd, &se, &la]),
        s(&[
            "\u{C0}\u{C1}\u{C2}\u{C3}\u{C4}\u{C5}\u{C6}\u{C7}\u{C8}\u{C9}\u{CA}\u{CB}\u{CC}\u{CD}\u{CE}\u{CF}\u{D0}\u{D1}\u{D2}\u{D3}\u{D4}\u{D5}\u{D6}\u{D7}\u{D8}\u{D9}\u{DA}",
            &eci, &fs, &gs, &rs, &ns,
            "\u{DB}\u{DC}\u{DD}\u{DE}\u{DF}\u{AA}\u{AC}\u{B1}\u{B2}\u{B3}\u{B5}\u{B9}\u{BA}\u{BC}\u{BD}\u{BE}\u{80}\u{81}\u{82}\u{83}\u{84}\u{85}\u{86}\u{87}\u{88}\u{89}",
            &la, " ", &lock, &sd, &se, &lb,
        ]),
        s(&[
            "\u{E0}\u{E1}\u{E2}\u{E3}\u{E4}\u{E5}\u{E6}\u{E7}\u{E8}\u{E9}\u{EA}\u{EB}\u{EC}\u{ED}\u{EE}\u{EF}\u{F0}\u{F1}\u{F2}\u{F3}\u{F4}\u{F5}\u{F6}\u{F7}\u{F8}\u{F9}\u{FA}",
            &eci, &fs, &gs, &rs, &ns,
            "\u{FB}\u{FC}\u{FD}\u{FE}\u{FF}\u{A1}\u{A8}\u{AB}\u{AF}\u{B0}\u{B4}\u{B7}\u{B8}\u{BB}\u{BF}\u{8A}\u{8B}\u{8C}\u{8D}\u{8E}\u{8F}\u{90}\u{91}\u{92}\u{93}\u{94}",
            &la, " ", &sc, &lock, &se, &lb,
        ]),
        s(&[
            "\u{0}\u{1}\u{2}\u{3}\u{4}\u{5}\u{6}\u{7}\u{8}\u{9}\n\u{B}\u{C}\r\u{E}\u{F}\u{10}\u{11}\u{12}\u{13}\u{14}\u{15}\u{16}\u{17}\u{18}\u{19}\u{1A}",
            &eci, &pad, &pad, "\u{1B}", &ns, &fs, &gs, &rs,
            "\u{1F}\u{9F}\u{A0}\u{A2}\u{A3}\u{A4}\u{A5}\u{A6}\u{A7}\u{A9}\u{AD}\u{AE}\u{B6}\u{95}\u{96}\u{97}\u{98}\u{99}\u{9A}\u{9B}\u{9C}\u{9D}\u{9E}",
            &la, " ", &sc, &sd, &lock, &lb,
        ]),
    ]
}

/// Read the 144 codewords of a 30x33 module grid.
pub fn read_codewords(m: &Matrix) -> [u8; 144] {
    let mut cw = [0u8; 144];
    for (y, row) in BITNR.iter().enumerate() {
        for (x, &bit) in row.iter().enumerate() {
            if bit >= 0 && m.get(x, y) {
                let b = bit as usize;
                cw[b / 6] |= 1 << (5 - b % 6);
            }
        }
    }
    cw
}

fn field() -> GfN {
    GfN::new(0x43, 64)
}

/// ZXing `maxicode.decoder.Decoder.correctErrors` (mode 0 = all, 1 = even, 2 = odd).
fn correct(cw: &mut [u8; 144], start: usize, data: usize, ec: usize, mode: usize) -> Option<usize> {
    let total = data + ec;
    let div = if mode == 0 { 1 } else { 2 };
    let mut block = vec![0u16; total / div];
    for i in 0..total {
        if mode == 0 || i % 2 == mode - 1 {
            block[i / div] = cw[i + start] as u16;
        }
    }
    let n = field().decode(&mut block, ec / div)?;
    for i in 0..data {
        if mode == 0 || i % 2 == mode - 1 {
            cw[i + start] = block[i / div] as u8;
        }
    }
    Some(n)
}

/// Decode the codewords: (text, mode).
pub fn decode_codewords(cw: &[u8; 144]) -> Option<(String, u8)> {
    let mut cw = *cw;
    correct(&mut cw, 0, 10, 10, 0)?;
    let mode = cw[0] & 0x0F;
    let n = match mode {
        2..=4 => {
            correct(&mut cw, 20, 84, 40, 1)?;
            correct(&mut cw, 20, 84, 40, 2)?;
            94
        }
        5 => {
            correct(&mut cw, 20, 68, 56, 1)?;
            correct(&mut cw, 20, 68, 56, 2)?;
            78
        }
        _ => return None,
    };
    let mut dw = vec![0u8; n];
    dw[..10].copy_from_slice(&cw[..10]);
    dw[10..].copy_from_slice(&cw[20..20 + n - 10]);
    Some((parse(&dw, mode)?, mode))
}

fn bit(b: &[u8], bit: usize) -> u32 {
    let bit = bit - 1;
    (b[bit / 6] >> (5 - bit % 6) & 1) as u32
}

fn int(b: &[u8], x: &[usize]) -> u32 {
    x.iter().fold(0, |a, i| (a << 1) | bit(b, *i))
}

/// ZXing `maxicode.decoder.DecodedBitStreamParser.decode`.
fn parse(b: &[u8], mode: u8) -> Option<String> {
    let sets = sets();
    match mode {
        2 | 3 => {
            let postcode = if mode == 2 {
                let pc = int(b, &[33, 34, 35, 36, 25, 26, 27, 28, 29, 30, 19, 20, 21, 22, 23, 24, 13, 14, 15, 16, 17, 18, 7, 8, 9, 10, 11, 12, 1, 2]);
                let len = int(b, &[39, 40, 41, 42, 31, 32]) as usize;
                if len > 10 {
                    return None;
                }
                let s = pc.to_string();
                if s.len() < len {
                    format!("{}{}", "0".repeat(len - s.len()), s)
                } else {
                    s
                }
            } else {
                [[39, 40, 41, 42, 31, 32], [33, 34, 35, 36, 25, 26], [27, 28, 29, 30, 19, 20], [21, 22, 23, 24, 13, 14], [15, 16, 17, 18, 7, 8], [9, 10, 11, 12, 1, 2]]
                    .iter()
                    .map(|x| sets[0][int(b, x) as usize])
                    .collect()
            };
            let country = format!("{:03}", int(b, &[53, 54, 43, 44, 45, 46, 47, 48, 37, 38]));
            let service = format!("{:03}", int(b, &[55, 56, 57, 58, 59, 60, 49, 50, 51, 52]));
            let msg = message(b, 10, 84, &sets)?;
            let head = format!("{}{}{}{}{}{}", postcode, GS, country, GS, service, GS);
            let prefix = format!("[)>{}01{}", RS, GS);
            Some(if msg.starts_with(&prefix) && msg.chars().count() >= 9 {
                let at: usize = msg.char_indices().nth(9).map(|(i, _)| i).unwrap_or(msg.len());
                format!("{}{}{}", &msg[..at], head, &msg[at..])
            } else {
                format!("{}{}", head, msg)
            })
        }
        4 => message(b, 1, 93, &sets),
        5 => message(b, 1, 77, &sets),
        _ => None,
    }
}

/// ZXing `getMessage` (ECI designators are skipped; the text stays Latin-1).
fn message(b: &[u8], start: usize, len: usize, sets: &[Vec<char>; 5]) -> Option<String> {
    let mut out = String::new();
    let mut shift: i32 = -1;
    let mut set = 0usize;
    let mut last = 0usize;
    let mut i = start;
    while i < start + len {
        let c = sets[set][*b.get(i)? as usize];
        match c {
            LATCHA => {
                set = 0;
                shift = -1;
            }
            LATCHB => {
                set = 1;
                shift = -1;
            }
            SHIFTA | SHIFTB | SHIFTC | SHIFTD | SHIFTE => {
                last = set;
                set = c as usize - SHIFTA as usize;
                shift = 1;
            }
            TWOSHIFTA => {
                last = set;
                set = 0;
                shift = 2;
            }
            THREESHIFTA => {
                last = set;
                set = 0;
                shift = 3;
            }
            NS => {
                let mut v: u32 = 0;
                for _ in 0..5 {
                    i += 1;
                    v = (v << 6) | *b.get(i)? as u32;
                }
                out.push_str(&format!("{:09}", v));
            }
            LOCK => shift = -1,
            ECI => {
                // ISO/IEC 16023: 1-4 codewords, the leading bits give the length
                i += 1;
                let first = *b.get(i)?;
                let extra = if first & 0x20 == 0 {
                    0
                } else if first & 0x10 == 0 {
                    1
                } else if first & 0x08 == 0 {
                    2
                } else {
                    3
                };
                i += extra;
            }
            _ => out.push(c),
        }
        if shift == 0 {
            set = last;
        }
        shift -= 1;
        i += 1;
    }
    while out.ends_with(PAD) {
        out.pop();
    }
    Some(out)
}

/// Decode a sampled 30x33 grid.
pub fn decode_matrix(m: &Matrix) -> Option<String> {
    decode_codewords(&read_codewords(m)).map(|d| d.0)
}

/// ZXing `MaxiCodeReader.extractPureBits`: sample the grid over the bounding box of the dark
/// pixels (odd rows shifted by half a module).
pub fn extract_pure(b: &Bin) -> Option<Matrix> {
    let (mut l, mut t, mut r, mut bt) = (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..b.height {
        for x in 0..b.width {
            if b.get(x, y) {
                l = l.min(x);
                r = r.max(x);
                t = t.min(y);
                bt = bt.max(y);
            }
        }
    }
    if l == usize::MAX {
        return None;
    }
    let (w, h) = (r - l + 1, bt - t + 1);
    if w < WIDTH || h < HEIGHT {
        return None;
    }
    let mut m = Matrix::new(WIDTH, HEIGHT);
    for y in 0..HEIGHT {
        let iy = t + ((y * h + h / 2) / HEIGHT).min(h - 1);
        for x in 0..WIDTH {
            let ix = l + ((x * w + w / 2 + (y & 1) * w / 2) / WIDTH).min(w - 1);
            if b.get(ix, iy) {
                m.set(x, y, true);
            }
        }
    }
    Some(m)
}

/// The pure-image reader (ZXing behaviour).
pub fn find_and_decode(b: &Bin) -> Option<String> {
    decode_matrix(&extract_pure(b)?)
}

/// Camera images: locate the bull's-eye (six concentric dark/light rings), estimate the
/// module pitch from the ring spacing and the orientation from the orientation clusters,
/// then sample the hexagonal grid through an affine fit refined by search.
pub mod detect {
    use super::super::detect::Bin;
    use super::super::Matrix;
    use super::{decode_matrix, BITNR, HEIGHT, WIDTH};

    /// Candidate bull's-eye centres: rows whose runs look like the ring pattern (11
    /// alternating, roughly symmetric runs, the centre one light), confirmed along the
    /// vertical and both diagonals (five transitions each way, extents agreeing); the three
    /// most regular candidates.
    fn centres(b: &Bin) -> Vec<(f64, f64, f64)> {
        let mut out: Vec<(f64, f64, f64, f64)> = Vec::new();
        let step = (b.height / 200).max(1);
        for y in (0..b.height).step_by(step) {
            let row = b.row(y);
            let mut runs: Vec<(bool, usize, usize)> = Vec::new();
            for (x, &v) in row.iter().enumerate() {
                match runs.last_mut() {
                    Some(r) if r.0 == v => r.2 += 1,
                    _ => runs.push((v, x, 1)),
                }
            }
            if runs.len() < 11 {
                continue;
            }
            for i in 0..runs.len() - 10 {
                let w = &runs[i..i + 11];
                if !w[0].0 || w[5].0 {
                    continue;
                }
                let widths: Vec<f64> = w.iter().map(|r| r.2 as f64).collect();
                let mean = widths.iter().sum::<f64>() / 11.0;
                if mean < 1.5 || widths.iter().any(|v| (*v - mean).abs() > mean * 0.6) {
                    continue;
                }
                if (0..5).any(|k| (widths[k] - widths[10 - k]).abs() > mean * 0.6) {
                    continue;
                }
                let cx = w[5].1 as f64 + w[5].2 as f64 / 2.0;
                let cy = y as f64 + 0.5;
                let half = widths.iter().sum::<f64>() / 2.0;
                if out.iter().any(|c| (c.0 - cx).abs() < half && (c.1 - cy).abs() < half) {
                    continue;
                }
                if let Some(score) = confirm(b, cx, cy, half, mean) {
                    out.push((cx, cy, 2.0 * half, score));
                }
            }
        }
        out.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap());
        out.truncate(3);
        out.into_iter().map(|c| (c.0, c.1, c.2)).collect()
    }

    /// Distance along a ray from the centre to the outer edge of the fifth ring transition,
    /// if every run on the way is plausible.
    fn ray(b: &Bin, cx: f64, cy: f64, dx: f64, dy: f64, unit: f64) -> Option<f64> {
        let mut t = 0.0;
        let mut colour = false;
        let mut transitions = 0;
        let mut run = 0.0;
        let limit = unit * 12.0;
        while t < limit {
            let (x, y) = (cx + dx * t, cy + dy * t);
            if x < 0.0 || y < 0.0 || x as usize >= b.width || y as usize >= b.height {
                return None;
            }
            let v = b.get(x as usize, y as usize);
            if v != colour {
                if run < unit * 0.25 && transitions > 0 {
                    return None;
                }
                transitions += 1;
                if transitions == 6 {
                    return Some(t);
                }
                colour = v;
                run = 0.0;
            }
            t += 0.5;
            run += 0.5;
        }
        None
    }

    /// Check the ring pattern on eight rays; returns the spread of their extents (lower is
    /// more regular).
    fn confirm(b: &Bin, cx: f64, cy: f64, half: f64, unit: f64) -> Option<f64> {
        let r = std::f64::consts::FRAC_1_SQRT_2;
        let dirs = [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (r, r), (-r, -r), (r, -r), (-r, r)];
        let mut ext = Vec::with_capacity(8);
        for (dx, dy) in dirs {
            let e = ray(b, cx, cy, dx, dy, unit)?;
            if e < half * 0.55 || e > half * 1.6 {
                return None;
            }
            ext.push(e);
        }
        // opposite rays agree (the centre is central)
        for k in 0..4 {
            if (ext[2 * k] - ext[2 * k + 1]).abs() > half * 0.35 {
                return None;
            }
        }
        let m = ext.iter().sum::<f64>() / 8.0;
        Some(ext.iter().map(|e| (e - m).abs()).sum::<f64>() / m)
    }

    /// Module centre (in grid units, x along a row, y down) relative to the symbol centre.
    pub fn grid_pos(x: usize, y: usize) -> (f64, f64) {
        let gx = x as f64 + 0.5 + if y % 2 == 1 { 0.5 } else { 0.0 } - 15.0;
        // rows are sqrt(3)/2 module pitches apart in the ideal hexagonal grid
        let gy = (y as f64 - 16.0) * 0.866;
        (gx, gy)
    }

    /// Sample the grid through an affine map around the centre `c` (axes `ax`, `ay` per grid
    /// unit) with a perspective term `(g, h)`: p = c + (gx * ax + gy * ay) / (1 + g gx + h gy).
    fn sample(b: &Bin, c: (f64, f64), ax: (f64, f64), ay: (f64, f64), persp: (f64, f64)) -> Matrix {
        let mut m = Matrix::new(WIDTH, HEIGHT);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let (gx, gy) = grid_pos(x, y);
                let w = 1.0 + persp.0 * gx + persp.1 * gy;
                if w <= 0.1 {
                    continue;
                }
                let px = c.0 + (gx * ax.0 + gy * ay.0) / w;
                let py = c.1 + (gx * ax.1 + gy * ay.1) / w;
                if px >= 0.0 && py >= 0.0 && (px as usize) < b.width && (py as usize) < b.height && b.get(px as usize, py as usize) {
                    m.set(x, y, true);
                }
            }
        }
        m
    }

    /// Agreement of a sampled grid with the fixed orientation modules (-2 dark, -1 light).
    fn orientation_score(m: &Matrix) -> usize {
        let mut s = 0;
        for (y, row) in BITNR.iter().enumerate() {
            for (x, v) in row.iter().enumerate() {
                if (*v == -2 && m.get(x, y)) || (*v == -1 && !m.get(x, y)) {
                    s += 1;
                }
            }
        }
        s
    }

    type Pose = ((f64, f64), (f64, f64));

    /// Parameters of the sampling map: centre, axes and perspective terms.
    #[derive(Clone, Copy)]
    struct Map([f64; 8]);

    impl Map {
        fn point(&self, gx: f64, gy: f64) -> (f64, f64) {
            let p = &self.0;
            let w = (1.0 + p[6] * gx + p[7] * gy).max(0.1);
            (p[0] + (gx * p[2] + gy * p[4]) / w, p[1] + (gx * p[3] + gy * p[5]) / w)
        }
    }

    fn dark(b: &Bin, p: (f64, f64)) -> Option<bool> {
        (p.0 >= 0.0 && p.1 >= 0.0 && (p.0 as usize) < b.width && (p.1 as usize) < b.height).then(|| b.get(p.0 as usize, p.1 as usize))
    }

    /// How well the map hits module centres: modules whose centre and four points a quarter
    /// pitch away all agree (misplaced samples straddle module edges).
    fn fit(b: &Bin, m: &Map) -> usize {
        let mut s = 0;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                if BITNR[y][x] == -3 {
                    continue;
                }
                let (gx, gy) = grid_pos(x, y);
                let Some(c) = dark(b, m.point(gx, gy)) else { continue };
                if [(0.22, 0.0), (-0.22, 0.0), (0.0, 0.2), (0.0, -0.2)].iter().all(|d| dark(b, m.point(gx + d.0, gy + d.1)) == Some(c)) {
                    s += 1;
                }
            }
        }
        s
    }

    fn sample_map(b: &Bin, m: &Map) -> Matrix {
        let mut out = Matrix::new(WIDTH, HEIGHT);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let (gx, gy) = grid_pos(x, y);
                if dark(b, m.point(gx, gy)) == Some(true) {
                    out.set(x, y, true);
                }
            }
        }
        out
    }

    /// Coordinate ascent of [`fit`] over the eight map parameters.
    fn refine(b: &Bin, start: Map, pitch: f64) -> Map {
        let mut m = start;
        let mut best = fit(b, &m);
        let mut steps = [pitch * 0.25, pitch * 0.25, pitch * 0.03, pitch * 0.03, pitch * 0.03, pitch * 0.03, 0.002, 0.002];
        for _ in 0..6 {
            let mut improved = true;
            let mut rounds = 0;
            while improved && rounds < 12 {
                improved = false;
                rounds += 1;
                for i in 0..8 {
                    for dir in [1.0, -1.0] {
                        let mut t = m;
                        t.0[i] += dir * steps[i];
                        let f = fit(b, &t);
                        if f > best {
                            best = f;
                            m = t;
                            improved = true;
                        }
                    }
                }
            }
            for s in steps.iter_mut() {
                *s /= 2.0;
            }
        }
        m
    }

    /// Detect and decode a MaxiCode in a binarized photograph.
    pub fn detect_and_decode(b: &Bin) -> Option<String> {
        for (cx, cy, rings) in centres(b) {
            // the bull's-eye outer diameter is about 6.5 module pitches (ISO/IEC 16023)
            let base = rings / 6.6;
            let mut poses: Vec<(usize, Pose, f64)> = Vec::new();
            for si in 0..7 {
                let pitch = base * (0.88 + 0.04 * si as f64);
                for k in 0..60 {
                    let a = (k as f64 * 6.0).to_radians();
                    for mirror in [false, true] {
                        let ax = (pitch * a.cos(), pitch * a.sin());
                        let ay = if mirror { (pitch * a.sin(), -pitch * a.cos()) } else { (-pitch * a.sin(), pitch * a.cos()) };
                        let m = sample(b, (cx, cy), ax, ay, (0.0, 0.0));
                        let s = orientation_score(&m);
                        if s + 1 >= 18 {
                            if let Some(t) = decode_matrix(&m) {
                                return Some(t);
                            }
                        }
                        poses.push((s, (ax, ay), pitch));
                    }
                }
            }
            // perspective and fine pose: refine the best poses by the module fit
            // among the poses matching the orientation modules best, rank by the module fit
            let top = poses.iter().map(|p| p.0).max().unwrap_or(0);
            let mut ranked: Vec<(usize, Map, f64)> = poses
                .iter()
                .filter(|p| p.0 + 1 >= top)
                .map(|(_, (ax, ay), pitch)| {
                    let m = Map([cx, cy, ax.0, ax.1, ay.0, ay.1, 0.0, 0.0]);
                    (fit(b, &m), m, *pitch)
                })
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, start, pitch) in ranked.iter().take(2) {
                let m = refine(b, *start, *pitch);
                if let Some(t) = decode_matrix(&sample_map(b, &m)) {
                    return Some(t);
                }
                // the fit can stall on a perspective plateau: try the projective terms around it
                for gi in -8..=8 {
                    for hi in -8..=8 {
                        let mut t = m;
                        t.0[6] += gi as f64 * 0.0016;
                        t.0[7] += hi as f64 * 0.0016;
                        if let Some(s) = decode_matrix(&sample_map(b, &t)) {
                            return Some(s);
                        }
                    }
                }
            }
        }
        None
    }
}

/// Test-only encoder (ZXing has no MaxiCode writer).
#[cfg(test)]
pub mod encode {
    use super::*;

    /// Message codewords for `text` in modes 4/5 (sets A/B with latches, single shifts to C-E,
    /// NS for runs of 9 digits), padded to `len`.
    pub fn message_codewords(text: &str, len: usize) -> Vec<u8> {
        let sets = sets();
        let pos = |set: usize, c: char| sets[set].iter().position(|x| *x == c).map(|p| p as u8);
        let chars: Vec<char> = text.chars().collect();
        let mut out = Vec::new();
        let mut set = 0usize;
        let mut i = 0;
        while i < chars.len() {
            if i + 9 <= chars.len() && chars[i..i + 9].iter().all(|c| c.is_ascii_digit()) {
                let v: u32 = chars[i..i + 9].iter().collect::<String>().parse().unwrap();
                out.push(pos(set, NS).unwrap());
                for k in (0..5).rev() {
                    out.push((v >> (6 * k) & 0x3F) as u8);
                }
                i += 9;
                continue;
            }
            let c = chars[i];
            if let Some(p) = pos(set, c) {
                out.push(p);
            } else if set == 0 && pos(1, c).is_some() {
                out.push(pos(0, LATCHB).unwrap());
                set = 1;
                out.push(pos(1, c).unwrap());
            } else if set == 1 && pos(0, c).is_some() {
                out.push(pos(1, LATCHA).unwrap());
                set = 0;
                out.push(pos(0, c).unwrap());
            } else {
                let (target, shift) = (2..5).find_map(|s| pos(s, c).map(|_| (s, [SHIFTC, SHIFTD, SHIFTE][s - 2]))).expect("encodable");
                out.push(pos(set, shift).unwrap());
                out.push(pos(target, c).unwrap());
            }
            i += 1;
        }
        let pad = pos(set, PAD).unwrap();
        assert!(out.len() <= len, "message too long");
        out.resize(len, pad);
        out
    }

    fn rs(data: &[u8], ec: usize) -> Vec<u8> {
        let d: Vec<u16> = data.iter().map(|x| *x as u16).collect();
        field().encode(&d, ec).iter().map(|x| *x as u8).collect()
    }

    /// The 144 codewords of a symbol from its 94 (modes 2-4) or 78 (mode 5) datawords.
    pub fn codewords(dw: &[u8]) -> [u8; 144] {
        let mode5 = dw.len() == 78;
        let (sd, se) = if mode5 { (68, 56) } else { (84, 40) };
        let mut cw = [0u8; 144];
        cw[..10].copy_from_slice(&dw[..10]);
        let pec = rs(&dw[..10], 10);
        cw[10..20].copy_from_slice(&pec[pec.len() - 10..]);
        let sec = &dw[10..];
        for parity in 0..2 {
            let data: Vec<u8> = (0..sd).filter(|i| i % 2 == parity).map(|i| sec[i]).collect();
            let ec = rs(&data, se / 2);
            let ec = &ec[ec.len() - se / 2..];
            for (k, i) in (0..sd).filter(|i| i % 2 == parity).enumerate() {
                cw[20 + i] = data[k];
            }
            for (k, v) in ec.iter().enumerate() {
                cw[20 + sd + 2 * k + parity] = *v;
            }
        }
        cw
    }

    /// Mode 4 (standard) or 5 (enhanced EC) datawords for a text.
    pub fn datawords(text: &str, mode: u8) -> Vec<u8> {
        let n = if mode == 5 { 78 } else { 94 };
        let mut dw = vec![mode];
        dw.extend(message_codewords(text, n - 1));
        dw
    }

    /// Mode 2 datawords: numeric postcode, country, service class and a message.
    pub fn datawords_mode2(postcode: &str, country: u32, service: u32, msg: &str) -> Vec<u8> {
        let mut dw = vec![0u8; 94];
        let mut set_bits = |positions: &[usize], v: u32| {
            for (k, p) in positions.iter().enumerate() {
                if v >> (positions.len() - 1 - k) & 1 == 1 {
                    let b = p - 1;
                    dw[b / 6] |= 1 << (5 - b % 6);
                }
            }
        };
        set_bits(&[33, 34, 35, 36, 25, 26, 27, 28, 29, 30, 19, 20, 21, 22, 23, 24, 13, 14, 15, 16, 17, 18, 7, 8, 9, 10, 11, 12, 1, 2], postcode.parse().unwrap());
        set_bits(&[39, 40, 41, 42, 31, 32], postcode.len() as u32);
        set_bits(&[53, 54, 43, 44, 45, 46, 47, 48, 37, 38], country);
        set_bits(&[55, 56, 57, 58, 59, 60, 49, 50, 51, 52], service);
        dw[0] |= 2;
        let m = message_codewords(msg, 84);
        dw[10..].copy_from_slice(&m);
        dw
    }

    /// Place codewords into the 30x33 grid (fixed orientation modules set).
    pub fn matrix(cw: &[u8; 144]) -> Matrix {
        let mut m = Matrix::new(WIDTH, HEIGHT);
        for (y, row) in BITNR.iter().enumerate() {
            for (x, &bit) in row.iter().enumerate() {
                let dark = match bit {
                    -2 => true,
                    b if b >= 0 => cw[b as usize / 6] >> (5 - b as usize % 6) & 1 == 1,
                    _ => false,
                };
                m.set(x, y, dark);
            }
        }
        m
    }

    /// Render like a printed MaxiCode: hexagon-ish dots on the offset grid (pitch `p` px,
    /// row height 0.866 p) and the bull's-eye rings, with a quiet zone of `q` pixels.
    pub fn render(m: &Matrix, p: usize, q: usize) -> super::super::png::Gray {
        let pf = p as f64;
        let rh = pf * 0.866;
        let w = (30.0 * pf) as usize + 2 * q;
        let h = (33.0 * rh) as usize + 2 * q;
        let mut g = super::super::png::Gray { width: w, height: h, pixels: vec![255; w * h] };
        let (cx, cy) = (q as f64 + 15.0 * pf, q as f64 + 16.5 * rh);
        for py in 0..h {
            for px in 0..w {
                let (fx, fy) = (px as f64 + 0.5, py as f64 + 0.5);
                // bull's-eye: three dark rings and a dark centre ring (radii in pitches)
                let r = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / pf;
                if r < 3.3 {
                    let ring = (r / 0.55) as usize;
                    if ring % 2 == 1 {
                        g.pixels[py * w + px] = 0;
                    }
                    continue;
                }
                let gy = ((fy - q as f64) / rh).floor();
                if gy < 0.0 || gy >= 33.0 {
                    continue;
                }
                let y = gy as usize;
                let off = if y % 2 == 1 { pf / 2.0 } else { 0.0 };
                let gx = ((fx - q as f64 - off) / pf).floor();
                if gx < 0.0 || gx >= 30.0 {
                    continue;
                }
                let x = gx as usize;
                // dot of 0.8 pitch
                let lx = (fx - q as f64 - off) / pf - gx;
                let ly = (fy - q as f64) / rh - gy;
                if m.get(x, y) && (0.1..0.9).contains(&lx) && (0.05..0.95).contains(&ly) {
                    g.pixels[py * w + px] = 0;
                }
            }
        }
        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables() {
        let mut seen = vec![0; 864];
        for v in BITNR.iter().flatten() {
            if *v >= 0 {
                seen[*v as usize] += 1;
            }
        }
        assert!(seen.iter().all(|c| *c == 1), "every bit exactly once");
        for s in sets() {
            assert_eq!(s.len(), 64);
        }
    }

    #[test]
    fn round_trip_modes() {
        for (text, mode) in [("Hello MaxiCode 123456789 abc", 4u8), ("ENHANCED ERROR CORRECTION", 5), ("Latin-1: \u{e0}\u{e9}\u{c7} \u{a3}5", 4)] {
            let cw = encode::codewords(&encode::datawords(text, mode));
            let m = encode::matrix(&cw);
            assert_eq!(decode_matrix(&m).as_deref(), Some(text));
            // with errors
            let mut damaged = cw;
            for i in [3usize, 30, 31, 77, 140] {
                damaged[i] ^= 0x15;
            }
            assert_eq!(decode_codewords(&damaged).map(|d| d.0).as_deref(), Some(text));
            let img = encode::render(&m, 8, 10);
            let bin = super::super::detect::binarize(&img).unwrap();
            assert_eq!(find_and_decode(&bin).as_deref(), Some(text), "pure");
        }
        let dw = encode::datawords_mode2("123450000", 222, 111, "[)>\u{1e}01\u{1d}96MODE2");
        let m = encode::matrix(&encode::codewords(&dw));
        assert_eq!(decode_matrix(&m).as_deref(), Some("[)>\u{1e}01\u{1d}96123450000\u{1d}222\u{1d}111\u{1d}MODE2"));
    }
}
