//! Tokenizer for the C-like languages (JavaScript, Groovy, OGNL, MVEL, SpEL).

use crate::ast::Dialect;
use crate::error::SyntaxError;

/// A raw template part: literal text or embedded source code (with its absolute offset).
#[derive(Clone, Debug, PartialEq)]
pub enum RawPart {
    Lit(String),
    Code(String, usize),
    /// JavaScript template literals: the raw text of a literal segment (appended after the
    /// cooked parts, one per segment, for tagged templates).
    Raw(String),
}

/// Token kinds.
#[derive(Clone, Debug, PartialEq)]
pub enum T {
    Int(i64),
    Num(f64),
    /// JavaScript BigInt literal (decimal digits).
    BigInt(String),
    Str(String),
    /// Template literal / GString with interpolations.
    Tpl(Vec<RawPart>),
    Regex(String, String),
    Ident(String),
    P(&'static str),
    Eof,
}

/// A token with its source offset and whether a newline precedes it.
#[derive(Clone, Debug)]
pub struct Tok {
    pub t: T,
    pub pos: usize,
    pub nl: bool,
}

const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "..<", "<=>", "==~", "&&=", "||=", "??=", ".?[", ".![", ".^[", ".$[", "?.[", "?:", "?.", "=>", "->", "::",
    "==", "!=", "<=", ">=", "&&", "||", "??", "++", "--", "+=", "-=", "*=", "/=", "%=", "**", "<<", ">>", "=~", "..", "*.", "#{", ".?", "{", "}", "(",
    ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%", "&", "|", "^", "!", "~", "?", ":", "=", ".", "@", "#",
];

fn punct_allowed(p: &str, d: Dialect) -> bool {
    match p {
        ">>>=" | ">>>" => matches!(d, Dialect::Js | Dialect::Java),
        "..." => matches!(d, Dialect::Js | Dialect::Java),
        "::" => d == Dialect::Java,
        "===" | "!==" | "=>" | "??" | "??=" | "&&=" | "||=" => d == Dialect::Js,
        "..<" | "<=>" | "==~" | "=~" | "*." => d == Dialect::Groovy,
        ".." => matches!(d, Dialect::Groovy),
        ".?[" | ".![" | ".^[" | ".$[" => d == Dialect::Spel,
        ".?" => d == Dialect::Mvel,
        "?.[" => d == Dialect::Js,
        "#{" => d == Dialect::Ognl,
        "->" => matches!(d, Dialect::Groovy | Dialect::Mvel | Dialect::Java),
        "**" | "**=" => matches!(d, Dialect::Js | Dialect::Groovy),
        _ => true,
    }
}

/// Tokenize source text (`base` is the absolute offset of `src` in the whole script, used for
/// error positions of embedded expressions).
pub fn tokenize(src: &str, base: usize, d: Dialect) -> Result<Vec<Tok>, SyntaxError> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut toks: Vec<Tok> = Vec::new();
    let mut nl = false;
    if d == Dialect::Groovy && src.starts_with("#!") {
        while i < b.len() && b[i] != b'\n' {
            i += 1;
        }
    }
    let err = |at: usize, msg: String| SyntaxError { pos: base + at, msg };
    while i < b.len() {
        let c = b[i];
        if c == b'\n' {
            nl = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let comments = matches!(d, Dialect::Js | Dialect::Groovy | Dialect::Mvel);
        if comments && c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if comments && c == b'/' && b.get(i + 1) == Some(&b'*') {
            let start = i;
            i += 2;
            loop {
                if i + 1 >= b.len() {
                    return Err(err(start, "unterminated comment".into()));
                }
                if b[i] == b'*' && b[i + 1] == b'/' {
                    i += 2;
                    break;
                }
                if b[i] == b'\n' {
                    nl = true;
                }
                i += 1;
            }
            continue;
        }
        let start = i;
        let regex_ok = match toks.last() {
            None => true,
            Some(Tok { t: T::P(p), .. }) => !matches!(*p, ")" | "]" | "}"),
            Some(Tok { t: T::Ident(w), .. }) => {
                matches!(w.as_str(), "return" | "typeof" | "case" | "do" | "else" | "in" | "of" | "new" | "delete" | "void" | "throw" | "instanceof")
            }
            _ => false,
        };
        let t;
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|x| x.is_ascii_digit()) && !matches!(toks.last(), Some(Tok { t: T::Ident(_), .. }) | Some(Tok { t: T::P(")" | "]"), .. })))
        {
            let (tok, ni) = lex_number(src, i, d).map_err(|m| err(i, m))?;
            t = tok;
            i = ni;
        } else if c == b'_' || c == b'$' && !(d == Dialect::Spel && b.get(i + 1) == Some(&b'[')) || c.is_ascii_alphabetic() || c >= 0x80 {
            let mut j = i;
            while j < b.len() && (b[j] == b'_' || b[j] == b'$' || b[j].is_ascii_alphanumeric() || b[j] >= 0x80) {
                j += 1;
            }
            // keep char boundaries for non-ascii identifiers
            while !src.is_char_boundary(j) {
                j += 1;
            }
            t = T::Ident(src[i..j].to_string());
            i = j;
        } else if c == b'#' && matches!(d, Dialect::Ognl | Dialect::Spel) && b.get(i + 1).is_some_and(|x| x.is_ascii_alphabetic() || *x == b'_') {
            let mut j = i + 1;
            while j < b.len() && (b[j] == b'_' || b[j].is_ascii_alphanumeric()) {
                j += 1;
            }
            t = T::Ident(src[i..j].to_string());
            i = j;
        } else if c == b'\'' || c == b'"' || c == b'`' {
            let triple = (d == Dialect::Groovy && c != b'`' || d == Dialect::Java && c == b'"') && b.get(i + 1) == Some(&c) && b.get(i + 2) == Some(&c);
            let interpolate = (d == Dialect::Js && c == b'`') || (d == Dialect::Groovy && c == b'"');
            let (mut tok, ni) = lex_string(src, i, c, triple, interpolate, d, base)?;
            if triple && d == Dialect::Java {
                if let T::Str(text) = &tok {
                    tok = T::Str(java_text_block(text));
                }
            }
            t = tok;
            i = ni;
        } else if c == b'/' && regex_ok && matches!(d, Dialect::Js | Dialect::Groovy) && b.get(i + 1) != Some(&b'=') {
            // regex literal (JS) or slashy string (Groovy)
            let mut j = i + 1;
            let mut s = String::new();
            let mut in_class = false;
            loop {
                if j >= b.len() || b[j] == b'\n' {
                    return Err(err(i, "unterminated regular expression".into()));
                }
                let ch = b[j];
                if ch == b'\\' && j + 1 < b.len() {
                    if d == Dialect::Groovy && b[j + 1] == b'/' {
                        s.push('/');
                    } else {
                        s.push('\\');
                        let ch2 = src[j + 1..].chars().next().unwrap();
                        s.push(ch2);
                        j += 1 + ch2.len_utf8();
                        continue;
                    }
                    j += 2;
                    continue;
                }
                if ch == b'[' {
                    in_class = true;
                } else if ch == b']' {
                    in_class = false;
                } else if ch == b'/' && !in_class {
                    break;
                }
                let chr = src[j..].chars().next().unwrap();
                s.push(chr);
                j += chr.len_utf8();
            }
            j += 1;
            if d == Dialect::Js {
                let fs = j;
                while j < b.len() && b[j].is_ascii_alphabetic() {
                    j += 1;
                }
                t = T::Regex(s, src[fs..j].to_string());
            } else {
                t = T::Str(s);
            }
            i = j;
        } else {
            let rest = &src[i..];
            let mut found = None;
            for p in PUNCTS {
                if rest.starts_with(p) && punct_allowed(p, d) {
                    // `?.5` is a ternary with a number, `1..2` handled by number lexer
                    if *p == "?." && b.get(i + 2).is_some_and(|x| x.is_ascii_digit()) {
                        continue;
                    }
                    found = Some(*p);
                    break;
                }
            }
            match found {
                Some(p) => {
                    t = T::P(p);
                    i += p.len();
                }
                None => {
                    let ch = rest.chars().next().unwrap();
                    return Err(err(i, format!("unexpected character '{}'", ch)));
                }
            }
        }
        toks.push(Tok { t, pos: base + start, nl });
        nl = false;
    }
    toks.push(Tok { t: T::Eof, pos: base + src.len(), nl: true });
    Ok(toks)
}

fn lex_number(src: &str, mut i: usize, d: Dialect) -> Result<(T, usize), String> {
    let b = src.as_bytes();
    let start = i;
    if b[i] == b'0' && i + 1 < b.len() && matches!(b[i + 1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O') {
        let radix = match b[i + 1] {
            b'x' | b'X' => 16,
            b'b' | b'B' => 2,
            _ => 8,
        };
        i += 2;
        let s0 = i;
        while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
            i += 1;
        }
        let digits: String = src[s0..i].chars().filter(|c| *c != '_').collect();
        if d == Dialect::Js && digits.ends_with('n') {
            let n = crate::bigint::BigInt::parse_radix(&digits[..digits.len() - 1], radix).ok_or_else(|| format!("invalid number '{}'", &src[start..i]))?;
            return Ok((T::BigInt(n.to_string()), i));
        }
        let digits = digits.trim_end_matches(['L', 'l']);
        let v = i64::from_str_radix(digits, radix).map_err(|_| format!("invalid number '{}'", &src[start..i]))?;
        return Ok((T::Int(v), i));
    }
    let mut is_float = false;
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' && b.get(i + 1).is_some_and(|x| x.is_ascii_digit()) {
        is_float = true;
        i += 1;
        while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'_') {
            i += 1;
        }
    } else if i < b.len() && b[i] == b'.' && d == Dialect::Js && !b.get(i + 1).is_some_and(|x| *x == b'.' || x.is_ascii_alphabetic() || *x == b'_') {
        // `1.` in JS
        is_float = true;
        i += 1;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;
        if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            is_float = true;
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    let text: String = src[start..i].chars().filter(|c| *c != '_').collect();
    if d == Dialect::Js && !is_float && b.get(i) == Some(&b'n') && !b.get(i + 1).is_some_and(|x| x.is_ascii_alphanumeric() || *x == b'_') {
        return Ok((T::BigInt(text), i + 1));
    }
    // Java / Groovy suffixes
    if d != Dialect::Js && i < b.len() && matches!(b[i], b'L' | b'l' | b'G' | b'g' | b'D' | b'd' | b'F' | b'f' | b'I' | b'i') {
        let suf = b[i];
        if !b.get(i + 1).is_some_and(|x| x.is_ascii_alphanumeric()) {
            i += 1;
            if matches!(suf, b'D' | b'd' | b'F' | b'f') {
                is_float = true;
            }
            if matches!(suf, b'G' | b'g') && text.contains(['.', 'e', 'E']) {
                is_float = true;
            }
        }
    }
    if is_float {
        Ok((T::Num(text.parse::<f64>().map_err(|_| format!("invalid number '{}'", text))?), i))
    } else {
        match text.parse::<i64>() {
            Ok(v) => Ok((T::Int(v), i)),
            Err(_) => Ok((T::Num(text.parse::<f64>().map_err(|_| format!("invalid number '{}'", text))?), i)),
        }
    }
}

/// Cook the raw text of a template literal segment (None for an invalid escape).
pub fn unescape_template(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            let (s, ni) = escape(raw, i + 1).ok()?;
            out.push_str(&s);
            i = ni;
        } else {
            let ch = raw[i..].chars().next()?;
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    Some(out)
}

/// Decode one escape sequence starting after the backslash at `i`; returns the char(s) and the new index.
pub fn escape(src: &str, i: usize) -> Result<(String, usize), String> {
    let b = src.as_bytes();
    let c = src[i..].chars().next().ok_or("unterminated escape")?;
    let simple = |s: &str| Ok((s.to_string(), i + 1));
    match c {
        'n' => simple("\n"),
        't' => simple("\t"),
        'r' => simple("\r"),
        'b' => simple("\u{8}"),
        'f' => simple("\u{c}"),
        'v' => simple("\u{b}"),
        '0' if !b.get(i + 1).is_some_and(|x| x.is_ascii_digit()) => simple("\0"),
        '\n' => Ok((String::new(), i + 1)),
        'x' => {
            let h = src.get(i + 1..i + 3).ok_or("invalid \\x escape")?;
            let v = u32::from_str_radix(h, 16).map_err(|_| "invalid \\x escape")?;
            Ok((char::from_u32(v).unwrap_or('?').to_string(), i + 3))
        }
        'u' => {
            if b.get(i + 1) == Some(&b'{') {
                let end = src[i..].find('}').ok_or("invalid \\u escape")? + i;
                let v = u32::from_str_radix(&src[i + 2..end], 16).map_err(|_| "invalid \\u escape")?;
                return Ok((char::from_u32(v).unwrap_or('?').to_string(), end + 1));
            }
            let h = src.get(i + 1..i + 5).ok_or("invalid \\u escape")?;
            let v = u32::from_str_radix(h, 16).map_err(|_| "invalid \\u escape")?;
            // surrogate pairs
            if (0xD800..0xDC00).contains(&v) && src.get(i + 5..i + 7) == Some("\\u") {
                if let Some(lo) = src.get(i + 7..i + 11).and_then(|h| u32::from_str_radix(h, 16).ok()) {
                    let cp = 0x10000 + ((v - 0xD800) << 10) + (lo - 0xDC00);
                    return Ok((char::from_u32(cp).unwrap_or('?').to_string(), i + 11));
                }
            }
            Ok((char::from_u32(v).unwrap_or('?').to_string(), i + 5))
        }
        other => Ok((other.to_string(), i + other.len_utf8())),
    }
}

/// Find the end of an embedded `${...}` expression starting at `i` (just after `{`); returns the
/// index of the matching `}`.
pub fn matching_brace(src: &str, mut i: usize) -> Option<usize> {
    let b = src.as_bytes();
    let mut depth = 0;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            q @ (b'\'' | b'"' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[allow(clippy::too_many_arguments)]
/// Java text block content: drop the opening line break, strip the common indentation and
/// trailing spaces.
fn java_text_block(raw: &str) -> String {
    let body = match raw.find('\n') {
        Some(i) if raw[..i].trim().is_empty() => &raw[i + 1..],
        _ => raw,
    };
    let lines: Vec<&str> = body.split('\n').collect();
    let indent = lines
        .iter()
        .enumerate()
        .filter(|(i, l)| !l.trim().is_empty() || *i == lines.len() - 1)
        .map(|(_, l)| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    let out: Vec<String> = lines.iter().map(|l| if l.len() >= indent { l[indent..].trim_end().to_string() } else { l.trim().to_string() }).collect();
    out.join("\n")
}

fn lex_string(src: &str, start: usize, q: u8, triple: bool, interpolate: bool, d: Dialect, base: usize) -> Result<(T, usize), SyntaxError> {
    let b = src.as_bytes();
    let mut i = start + if triple { 3 } else { 1 };
    let mut parts: Vec<RawPart> = Vec::new();
    let mut cur = String::new();
    let mut raws: Vec<String> = Vec::new();
    let mut seg_start = i;
    let err = |at: usize, msg: &str| SyntaxError { pos: base + at, msg: msg.to_string() };
    loop {
        if i >= b.len() {
            return Err(err(start, "unterminated string literal"));
        }
        let c = b[i];
        if c == q {
            if triple {
                if b.get(i + 1) == Some(&q) && b.get(i + 2) == Some(&q) {
                    i += 3;
                    break;
                }
            } else if d == Dialect::Spel && q == b'\'' && b.get(i + 1) == Some(&b'\'') {
                cur.push('\'');
                i += 2;
                continue;
            } else {
                raws.push(src[seg_start..i].to_string());
                i += 1;
                break;
            }
        }
        if c == b'\n' && !triple && q != b'`' {
            return Err(err(start, "unterminated string literal"));
        }
        if c == b'\\' && d != Dialect::Spel {
            let (s, ni) = match escape(src, i + 1) {
                Ok(x) => x,
                // template literals keep invalid escapes for tagged templates (cooked: undefined)
                Err(_) if q == b'`' => ("\\".to_string(), i + 1),
                Err(m) => return Err(err(i, &m)),
            };
            cur.push_str(&s);
            i = ni;
            continue;
        }
        if interpolate && c == b'$' {
            if b.get(i + 1) == Some(&b'{') {
                let end = matching_brace(src, i + 2).ok_or_else(|| err(i, "unterminated ${ in string"))?;
                if !cur.is_empty() {
                    parts.push(RawPart::Lit(std::mem::take(&mut cur)));
                }
                raws.push(src[seg_start..i].to_string());
                parts.push(RawPart::Code(src[i + 2..end].to_string(), base + i + 2));
                i = end + 1;
                seg_start = i;
                continue;
            }
            if d == Dialect::Groovy && b.get(i + 1).is_some_and(|x| x.is_ascii_alphabetic() || *x == b'_') {
                let mut j = i + 1;
                loop {
                    while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                        j += 1;
                    }
                    if j + 1 < b.len() && b[j] == b'.' && (b[j + 1].is_ascii_alphabetic() || b[j + 1] == b'_') {
                        j += 1;
                        continue;
                    }
                    break;
                }
                if !cur.is_empty() {
                    parts.push(RawPart::Lit(std::mem::take(&mut cur)));
                }
                parts.push(RawPart::Code(src[i + 1..j].to_string(), base + i + 1));
                i = j;
                continue;
            }
        }
        let ch = src[i..].chars().next().unwrap();
        cur.push(ch);
        i += ch.len_utf8();
    }
    if parts.is_empty() && q != b'`' {
        return Ok((T::Str(cur), i));
    }
    if !cur.is_empty() {
        parts.push(RawPart::Lit(cur));
    }
    if q == b'`' {
        parts.extend(raws.into_iter().map(RawPart::Raw));
    }
    Ok((T::Tpl(parts), i))
}
