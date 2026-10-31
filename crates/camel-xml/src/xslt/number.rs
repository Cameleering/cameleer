//! Number formatting for `xsl:number` (format tokens) and `format-number()`
//! (a `java.text.DecimalFormat` pattern subset with `xsl:decimal-format` symbols).

/// Symbols of an `xsl:decimal-format`.
#[derive(Clone, Debug)]
pub struct DecimalFormat {
    pub decimal_separator: char,
    pub grouping_separator: char,
    pub infinity: String,
    pub minus_sign: char,
    pub nan: String,
    pub percent: char,
    pub per_mille: char,
    pub zero_digit: char,
    pub digit: char,
    pub pattern_separator: char,
}

impl Default for DecimalFormat {
    fn default() -> Self {
        DecimalFormat {
            decimal_separator: '.',
            grouping_separator: ',',
            infinity: "Infinity".into(),
            minus_sign: '-',
            nan: "NaN".into(),
            percent: '%',
            per_mille: '\u{2030}',
            zero_digit: '0',
            digit: '#',
            pattern_separator: ';',
        }
    }
}

struct SubPattern {
    prefix: String,
    suffix: String,
    min_int: usize,
    min_frac: usize,
    max_frac: usize,
    grouping: usize,
    multiplier: f64,
}

fn parse_sub(p: &str, df: &DecimalFormat) -> Result<SubPattern, String> {
    let chars: Vec<char> = p.chars().collect();
    let is_num = |c: char| c == df.digit || c == df.zero_digit || c == df.grouping_separator || c == df.decimal_separator;
    let start = chars.iter().position(|&c| is_num(c)).unwrap_or(chars.len());
    let mut end = start;
    while end < chars.len() && is_num(chars[end]) {
        end += 1;
    }
    let mut prefix: String = chars[..start].iter().collect();
    let mut suffix: String = chars[end..].iter().collect();
    let mut multiplier = 1.0;
    for s in [&mut prefix, &mut suffix] {
        if s.contains(df.percent) {
            multiplier = 100.0;
        } else if s.contains(df.per_mille) {
            multiplier = 1000.0;
        }
        *s = s.replace('\'', "");
    }
    let num: Vec<char> = chars[start..end].to_vec();
    let dec = num.iter().position(|&c| c == df.decimal_separator);
    let (int_part, frac_part) = match dec {
        Some(i) => (&num[..i], &num[i + 1..]),
        None => (&num[..], &[][..]),
    };
    let min_int = int_part.iter().filter(|&&c| c == df.zero_digit).count();
    let grouping = match int_part.iter().rposition(|&c| c == df.grouping_separator) {
        Some(g) => int_part.len() - g - 1,
        None => 0,
    };
    let min_frac = frac_part.iter().filter(|&&c| c == df.zero_digit).count();
    let max_frac = frac_part.iter().filter(|&&c| c == df.zero_digit || c == df.digit).count();
    Ok(SubPattern { prefix, suffix, min_int, min_frac, max_frac, grouping, multiplier })
}

/// Round half-even to `digits` fraction digits and return the decimal digits.
fn round_digits(x: f64, digits: usize) -> String {
    let s = format!("{:.*}", digits + 3, x);
    // use exact decimal representation for half-even decisions
    let factor = 10f64.powi(digits as i32);
    let scaled = x * factor;
    let fl = scaled.floor();
    let diff = scaled - fl;
    let r = if (diff - 0.5).abs() < 1e-9 {
        if (fl as i64) % 2 == 0 {
            fl
        } else {
            fl + 1.0
        }
    } else {
        scaled.round()
    };
    let _ = s;
    let v = r / factor;
    format!("{:.*}", digits, v)
}

/// XSLT `format-number(number, pattern)`.
pub fn format_number(x: f64, pattern: &str, df: &DecimalFormat) -> Result<String, String> {
    if x.is_nan() {
        return Ok(df.nan.clone());
    }
    let (pos_p, neg_p) = match pattern.split_once(df.pattern_separator) {
        Some((a, b)) => (a, Some(b)),
        None => (pattern, None),
    };
    let pos = parse_sub(pos_p, df)?;
    let negative = x < 0.0 || (x == 0.0 && x.is_sign_negative() && false);
    let (prefix, suffix) = if negative {
        match neg_p {
            Some(n) => {
                let np = parse_sub(n, df)?;
                (np.prefix, np.suffix)
            }
            None => (format!("{}{}", df.minus_sign, pos.prefix), pos.suffix.clone()),
        }
    } else {
        (pos.prefix.clone(), pos.suffix.clone())
    };
    let v = x.abs() * pos.multiplier;
    if v.is_infinite() {
        return Ok(format!("{}{}{}", prefix, df.infinity, suffix));
    }
    let s = round_digits(v, pos.max_frac);
    let (ip, fp) = match s.split_once('.') {
        Some((a, b)) => (a.to_string(), b.to_string()),
        None => (s.clone(), String::new()),
    };
    let mut ip = ip.trim_start_matches('0').to_string();
    while ip.len() < pos.min_int {
        ip.insert(0, '0');
    }
    let mut fp = fp;
    while fp.len() > pos.min_frac && fp.ends_with('0') {
        fp.pop();
    }
    // grouping
    let mut int_out = String::new();
    let ic: Vec<char> = ip.chars().collect();
    for (i, c) in ic.iter().enumerate() {
        if pos.grouping > 0 && i > 0 && (ic.len() - i) % pos.grouping == 0 {
            int_out.push(df.grouping_separator);
        }
        int_out.push(*c);
    }
    let map_digit = |c: char| -> char {
        if c.is_ascii_digit() && df.zero_digit != '0' {
            char::from_u32(df.zero_digit as u32 + (c as u32 - '0' as u32)).unwrap_or(c)
        } else {
            c
        }
    };
    let mut out = prefix;
    out.extend(int_out.chars().map(map_digit));
    if !fp.is_empty() {
        out.push(df.decimal_separator);
        out.extend(fp.chars().map(map_digit));
    }
    if int_out.is_empty() && fp.is_empty() {
        out.push(df.zero_digit);
    }
    out.push_str(&suffix);
    Ok(out)
}

fn alpha(mut n: u64, upper: bool) -> String {
    let base = if upper { b'A' } else { b'a' };
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(base + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

fn roman(mut n: u64, upper: bool) -> String {
    if n == 0 || n >= 4000 {
        return n.to_string();
    }
    let table = [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
    let mut s = String::new();
    for (v, r) in table {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    if upper {
        s.to_uppercase()
    } else {
        s
    }
}

fn format_one(n: u64, token: &str, grouping: Option<(char, usize)>) -> String {
    let s = match token {
        "a" => alpha(n, false),
        "A" => alpha(n, true),
        "i" => roman(n, false),
        "I" => roman(n, true),
        t if !t.is_empty() && t.chars().all(|c| c.is_ascii_digit()) => {
            let mut s = n.to_string();
            while s.len() < t.len() {
                s.insert(0, '0');
            }
            s
        }
        _ => n.to_string(),
    };
    match grouping {
        Some((sep, size)) if size > 0 && s.chars().all(|c| c.is_ascii_digit()) => {
            let c: Vec<char> = s.chars().collect();
            let mut out = String::new();
            for (i, ch) in c.iter().enumerate() {
                if i > 0 && (c.len() - i) % size == 0 {
                    out.push(sep);
                }
                out.push(*ch);
            }
            out
        }
        _ => s,
    }
}

/// Format a list of numbers with an `xsl:number` format string.
pub fn format_number_list(nums: &[u64], format: &str, grouping: Option<(char, usize)>) -> String {
    // split into alternating separator / alphanumeric tokens
    let mut parts: Vec<(bool, String)> = Vec::new();
    for c in format.chars() {
        let alnum = c.is_alphanumeric();
        match parts.last_mut() {
            Some((a, s)) if *a == alnum => s.push(c),
            _ => parts.push((alnum, c.to_string())),
        }
    }
    let mut prefix = String::new();
    let mut suffix = String::new();
    if parts.first().map(|(a, _)| !a).unwrap_or(false) {
        prefix = parts.remove(0).1;
    }
    if parts.last().map(|(a, _)| !a).unwrap_or(false) {
        suffix = parts.pop().unwrap().1;
    }
    let tokens: Vec<String> = parts.iter().filter(|(a, _)| *a).map(|(_, s)| s.clone()).collect();
    let seps: Vec<String> = parts.iter().filter(|(a, _)| !*a).map(|(_, s)| s.clone()).collect();
    let tokens = if tokens.is_empty() { vec!["1".to_string()] } else { tokens };
    let mut out = prefix;
    for (i, n) in nums.iter().enumerate() {
        if i > 0 {
            out.push_str(seps.get(i - 1).or(seps.last()).map(|s| s.as_str()).unwrap_or("."));
        }
        let tok = tokens.get(i).or(tokens.last()).unwrap();
        out.push_str(&format_one(*n, tok, grouping));
    }
    out.push_str(&suffix);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_format() {
        let df = DecimalFormat::default();
        assert_eq!(format_number(1234567.891, "#,##0.00", &df).unwrap(), "1,234,567.89");
        assert_eq!(format_number(0.5, "0.00", &df).unwrap(), "0.50");
        assert_eq!(format_number(-3.0, "#", &df).unwrap(), "-3");
        assert_eq!(format_number(0.256, "0.0%", &df).unwrap(), "25.6%");
        assert_eq!(format_number(7.0, "000", &df).unwrap(), "007");
        assert_eq!(format_number(-7.0, "0;(0)", &df).unwrap(), "(7)");
        assert_eq!(format_number(2.5, "0", &df).unwrap(), "2");
        assert_eq!(format_number(f64::NAN, "0", &df).unwrap(), "NaN");
    }

    #[test]
    fn number_tokens() {
        assert_eq!(format_number_list(&[3], "1", None), "3");
        assert_eq!(format_number_list(&[1, 2, 3], "1.1.", None), "1.2.3.");
        assert_eq!(format_number_list(&[28], "a", None), "ab");
        assert_eq!(format_number_list(&[14], "I", None), "XIV");
        assert_eq!(format_number_list(&[5], "(01)", None), "(05)");
        assert_eq!(format_number_list(&[1234], "1", Some((',', 3))), "1,234");
    }
}
