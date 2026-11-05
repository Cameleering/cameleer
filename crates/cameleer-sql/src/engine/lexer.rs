//! SQL tokenizer.

use super::SqlResult;
use super::SqlErr;

/// A token kind.
#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// Unquoted identifier or keyword (original spelling).
    Ident(String),
    /// `"quoted identifier"`.
    QIdent(String),
    Int(i64),
    Float(f64),
    /// `'string literal'`.
    Str(String),
    /// `?` positional parameter.
    Param,
    /// `$n` numbered parameter (1-based).
    NumParam(usize),
    /// `:#name` or `:name` named parameter.
    Named(String),
    /// Operator or punctuation.
    Op(&'static str),
    Eof,
}

/// A token with its position (1-based line and column).
#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: usize,
    pub col: usize,
}

impl Token {
    /// Text used in error messages.
    pub fn describe(&self) -> String {
        match &self.tok {
            Tok::Ident(s) => s.clone(),
            Tok::QIdent(s) => format!("\"{}\"", s),
            Tok::Int(i) => i.to_string(),
            Tok::Float(f) => f.to_string(),
            Tok::Str(s) => format!("'{}'", s),
            Tok::Param => "?".into(),
            Tok::NumParam(n) => format!("${}", n),
            Tok::Named(n) => format!(":#{}", n),
            Tok::Op(o) => o.to_string(),
            Tok::Eof => "end of input".into(),
        }
    }
}

const OPS: &[&str] = &[":=", "..", "=>", "<>", "!=", "<=", ">=", "||", "::", "=", "<", ">", "+", "-", "*", "/", "%", "(", ")", ",", ".", ";"];

/// Tokenize SQL text.
pub fn tokenize(sql: &str) -> SqlResult<Vec<Token>> {
    let chars: Vec<char> = sql.chars().collect();
    let mut out = Vec::new();
    let (mut i, mut line, mut col) = (0usize, 1usize, 1usize);
    macro_rules! adv {
        () => {{
            if chars[i] == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
            i += 1;
        }};
    }
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            adv!();
            continue;
        }
        // comments
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                adv!();
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let (sl, sc) = (line, col);
            adv!();
            adv!();
            loop {
                if i >= chars.len() {
                    return Err(SqlErr::syntax(format!("unterminated /* comment at line {}, column {}", sl, sc)));
                }
                if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    adv!();
                    adv!();
                    break;
                }
                adv!();
            }
            continue;
        }
        let (tl, tc) = (line, col);
        let push = |out: &mut Vec<Token>, tok: Tok| out.push(Token { tok, line: tl, col: tc });
        if c.is_ascii_alphabetic() || c == '_' {
            let mut s = String::new();
            while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$') {
                s.push(chars[i]);
                adv!();
            }
            push(&mut out, Tok::Ident(s));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let mut s = String::new();
            let mut is_float = false;
            while i < chars.len() && chars[i].is_ascii_digit() {
                s.push(chars[i]);
                adv!();
            }
            if i < chars.len() && chars[i] == '.' && chars.get(i + 1) != Some(&'.') && chars.get(i + 1).is_none_or(|d| d.is_ascii_digit() || !d.is_alphabetic()) {
                is_float = true;
                s.push('.');
                adv!();
                while i < chars.len() && chars[i].is_ascii_digit() {
                    s.push(chars[i]);
                    adv!();
                }
            }
            if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                let next = chars.get(i + 1).copied();
                let next2 = chars.get(i + 2).copied();
                if next.is_some_and(|d| d.is_ascii_digit()) || (matches!(next, Some('+') | Some('-')) && next2.is_some_and(|d| d.is_ascii_digit())) {
                    is_float = true;
                    s.push('e');
                    adv!();
                    if matches!(chars[i], '+' | '-') {
                        s.push(chars[i]);
                        adv!();
                    }
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        s.push(chars[i]);
                        adv!();
                    }
                }
            }
            if i < chars.len() && (chars[i].is_alphabetic() || chars[i] == '_') {
                return Err(SqlErr::syntax(format!("trailing junk after numeric literal at or near \"{}{}\" (line {}, column {})", s, chars[i], tl, tc)));
            }
            let tok = if is_float {
                Tok::Float(s.parse().map_err(|_| SqlErr::syntax(format!("invalid number \"{}\"", s)))?)
            } else {
                match s.parse::<i64>() {
                    Ok(n) => Tok::Int(n),
                    Err(_) => Tok::Float(s.parse().map_err(|_| SqlErr::syntax(format!("invalid number \"{}\"", s)))?),
                }
            };
            push(&mut out, tok);
            continue;
        }
        if c == '\'' || ((c == 'E' || c == 'e') && chars.get(i + 1) == Some(&'\'')) {
            let escapes = c != '\'';
            if escapes {
                adv!();
            }
            adv!();
            let mut s = String::new();
            loop {
                if i >= chars.len() {
                    return Err(SqlErr::syntax(format!("unterminated quoted string at line {}, column {}", tl, tc)));
                }
                let ch = chars[i];
                if ch == '\'' {
                    if chars.get(i + 1) == Some(&'\'') {
                        s.push('\'');
                        adv!();
                        adv!();
                        continue;
                    }
                    adv!();
                    break;
                }
                if escapes && ch == '\\' && i + 1 < chars.len() {
                    adv!();
                    let e = chars[i];
                    s.push(match e {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other,
                    });
                    adv!();
                    continue;
                }
                s.push(ch);
                adv!();
            }
            push(&mut out, Tok::Str(s));
            continue;
        }
        if c == '"' || c == '`' {
            let q = c;
            adv!();
            let mut s = String::new();
            loop {
                if i >= chars.len() {
                    return Err(SqlErr::syntax(format!("unterminated quoted identifier at line {}, column {}", tl, tc)));
                }
                if chars[i] == q {
                    if chars.get(i + 1) == Some(&q) {
                        s.push(q);
                        adv!();
                        adv!();
                        continue;
                    }
                    adv!();
                    break;
                }
                s.push(chars[i]);
                adv!();
            }
            push(&mut out, Tok::QIdent(s));
            continue;
        }
        if c == '?' {
            adv!();
            push(&mut out, Tok::Param);
            continue;
        }
        // dollar-quoted string: $$...$$ or $tag$...$tag$
        if c == '$' && chars.get(i + 1).is_some_and(|d| *d == '$' || d.is_alphabetic() || *d == '_') {
            let mut j = i + 1;
            while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            if chars.get(j) == Some(&'$') {
                let tag: String = chars[i..=j].iter().collect();
                let tag_chars: Vec<char> = tag.chars().collect();
                let body_start = j + 1;
                let mut k = body_start;
                let mut end = None;
                while k + tag_chars.len() <= chars.len() {
                    if chars[k..k + tag_chars.len()] == tag_chars[..] {
                        end = Some(k);
                        break;
                    }
                    k += 1;
                }
                let Some(end) = end else {
                    return Err(SqlErr::syntax(format!("unterminated dollar-quoted string at line {}, column {}", tl, tc)));
                };
                let body: String = chars[body_start..end].iter().collect();
                while i < end + tag_chars.len() {
                    adv!();
                }
                push(&mut out, Tok::Str(body));
                continue;
            }
        }
        if c == '$' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            adv!();
            let mut s = String::new();
            while i < chars.len() && chars[i].is_ascii_digit() {
                s.push(chars[i]);
                adv!();
            }
            let n: usize = s.parse().map_err(|_| SqlErr::syntax(format!("invalid parameter ${}", s)))?;
            if n == 0 {
                return Err(SqlErr::syntax("parameter $0 is not valid (parameters start at $1)"));
            }
            push(&mut out, Tok::NumParam(n));
            continue;
        }
        if c == ':' && chars.get(i + 1) != Some(&':') {
            let mut j = i + 1;
            if chars.get(j) == Some(&'#') {
                j += 1;
            }
            if chars.get(j).is_some_and(|d| d.is_alphanumeric() || *d == '_') {
                while i < j {
                    adv!();
                }
                let mut s = String::new();
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.' || chars[i] == '$') {
                    s.push(chars[i]);
                    adv!();
                }
                push(&mut out, Tok::Named(s));
                continue;
            }
        }
        let rest: String = chars[i..chars.len().min(i + 2)].iter().collect();
        if let Some(op) = OPS.iter().find(|o| rest.starts_with(**o)) {
            for _ in 0..op.chars().count() {
                adv!();
            }
            push(&mut out, Tok::Op(op));
            continue;
        }
        return Err(SqlErr::syntax(format!("unexpected character '{}' at line {}, column {}", c, tl, tc)));
    }
    out.push(Token { tok: Tok::Eof, line, col });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let t = tokenize("select a.b, 'it''s', 1.5, 2, ? , $2, :#id, :name, x::int -- c\n /* x */ <> != ||").unwrap();
        let kinds: Vec<Tok> = t.into_iter().map(|t| t.tok).collect();
        assert_eq!(kinds[0], Tok::Ident("select".into()));
        assert!(kinds.contains(&Tok::Str("it's".into())));
        assert!(kinds.contains(&Tok::Float(1.5)));
        assert!(kinds.contains(&Tok::NumParam(2)));
        assert!(kinds.contains(&Tok::Named("id".into())));
        assert!(kinds.contains(&Tok::Named("name".into())));
        assert!(kinds.contains(&Tok::Op("::")));
        assert!(kinds.contains(&Tok::Op("<>")));
        assert!(kinds.contains(&Tok::Op("||")));
        assert!(tokenize("select 'abc").is_err());
        assert!(tokenize("select 12abc").is_err());
    }
}
