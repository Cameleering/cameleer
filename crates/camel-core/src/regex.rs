//! Regular expression engine (std-only, backtracking) supporting the commonly used
//! subset of Java regular expression syntax:
//!
//! - literals, `.`, character classes `[a-z]`, `[^...]`, escapes `\d \w \s \D \W \S`,
//!   `\t \n \r \f \e \0nn \xhh \uhhhh \Q...\E`, POSIX-ish `\p{Alpha}`, `\p{L}`, ...
//! - anchors `^ $ \A \z \Z \b \B`
//! - groups: capturing `( )`, non-capturing `(?: )`, named `(?<name> )`, atomic `(?> )`,
//!   lookahead `(?= )`/`(?! )`, lookbehind `(?<= )`/`(?<! )`
//! - alternation `|`, quantifiers `* + ? {n} {n,} {n,m}` with lazy (`?`) and possessive (`+`)
//!   variants, backreferences `\1`, `\k<name>`
//! - inline flags `(?i)`, `(?s)`, `(?m)`, `(?x)`, scoped `(?i: )` and `(?-i)`
//!
//! The pattern is compiled to a small instruction program executed by a backtracking
//! virtual machine using an explicit stack (no native recursion per character), with a
//! step budget that guards against catastrophic backtracking.
//!
//! Positions in the public API are **byte offsets** into the input string.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{CamelError, Result};

/// Maximum number of VM steps per match attempt sequence before giving up.
const STEP_LIMIT: u64 = 20_000_000;
/// Maximum compiled program size (guards against huge counted repetitions).
const MAX_PROGRAM: usize = 200_000;
/// Maximum group nesting depth in the parser.
const MAX_NESTING: usize = 250;

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Copy, PartialEq)]
enum AssertKind {
    Bol,
    Eol,
    BolMulti,
    EolMulti,
    StartText,
    EndText,
    EndTextOptNl,
    WordB,
    NotWordB,
}

#[derive(Clone, Debug, PartialEq)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
    Prop(String, bool),
    Nested(Box<CharClass>),
}

#[derive(Clone, Debug, PartialEq)]
struct CharClass {
    items: Vec<ClassItem>,
    negated: bool,
    ci: bool,
    /// Intersections (`&&`): all must match as well.
    and: Vec<CharClass>,
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Char(char, bool),
    Any(bool),
    Class(CharClass),
    Assert(AssertKind),
    Group(Box<Node>, Option<usize>),
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat { node: Box<Node>, min: u32, max: Option<u32>, greedy: bool, possessive: bool },
    Look { node: Box<Node>, ahead: bool, negate: bool },
    Atomic(Box<Node>),
    Backref(usize, bool),
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Flags {
    ci: bool,
    dotall: bool,
    multiline: bool,
    comments: bool,
}

struct Parser<'a> {
    chars: Vec<char>,
    pos: usize,
    pattern: &'a str,
    groups: usize,
    names: BTreeMap<String, usize>,
    flags: Flags,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> CamelError {
        CamelError::syntax(format!("{} near index {}\n{}\n{}^", msg, self.pos, self.pattern, " ".repeat(self.pos.min(self.chars.len()))))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, off: usize) -> Option<char> {
        self.chars.get(self.pos + off).copied()
    }

    fn eat(&mut self, c: char) -> bool {
        if self.peek() == Some(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_alt(&mut self) -> Result<Node> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(self.err("Pattern nesting too deep"));
        }
        let mut alts = vec![self.parse_concat()?];
        while self.eat('|') {
            alts.push(self.parse_concat()?);
        }
        self.depth -= 1;
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn skip_comments_ws(&mut self) {
        if !self.flags.comments {
            return;
        }
        loop {
            match self.peek() {
                Some(c) if c.is_whitespace() => self.pos += 1,
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_concat(&mut self) -> Result<Node> {
        let mut items: Vec<Node> = Vec::new();
        loop {
            self.skip_comments_ws();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                _ => {}
            }
            let atom = match self.parse_atom()? {
                Some(a) => a,
                None => continue, // e.g. inline flag group
            };
            self.skip_comments_ws();
            let atom = self.parse_quantifier(atom)?;
            items.push(atom);
        }
        Ok(match items.len() {
            0 => Node::Empty,
            1 => items.pop().unwrap(),
            _ => Node::Concat(items),
        })
    }

    fn parse_number(&mut self) -> Option<u32> {
        let start = self.pos;
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        if start == self.pos {
            return None;
        }
        let s: String = self.chars[start..self.pos].iter().collect();
        s.parse().ok()
    }

    fn parse_quantifier(&mut self, atom: Node) -> Result<Node> {
        let mut atom = atom;
        loop {
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    (0, None)
                }
                Some('+') => {
                    self.pos += 1;
                    (1, None)
                }
                Some('?') => {
                    self.pos += 1;
                    (0, Some(1))
                }
                Some('{') => {
                    let save = self.pos;
                    self.pos += 1;
                    match self.parse_number() {
                        Some(n) => {
                            let max = if self.eat(',') {
                                self.parse_number().map(|m| m)
                            } else {
                                Some(n)
                            };
                            if !self.eat('}') {
                                self.pos = save;
                                return Ok(atom);
                            }
                            if let Some(m) = max {
                                if m < n {
                                    return Err(self.err("Illegal repetition range"));
                                }
                            }
                            if n > 10_000 || max.map(|m| m > 10_000).unwrap_or(false) {
                                return Err(self.err("Repetition count too large"));
                            }
                            (n, max)
                        }
                        None => {
                            self.pos = save;
                            return Ok(atom);
                        }
                    }
                }
                _ => return Ok(atom),
            };
            if matches!(atom, Node::Assert(_) | Node::Empty) && !matches!(atom, Node::Empty) {
                // quantified assertion: just keep it (Java allows e.g. `^*`)
            }
            let mut greedy = true;
            let mut possessive = false;
            if self.eat('?') {
                greedy = false;
            } else if self.eat('+') {
                possessive = true;
            }
            atom = Node::Repeat { node: Box::new(atom), min, max, greedy, possessive };
            self.skip_comments_ws();
        }
    }

    fn parse_atom(&mut self) -> Result<Option<Node>> {
        let c = self.peek().unwrap();
        self.pos += 1;
        Ok(Some(match c {
            '.' => Node::Any(self.flags.dotall),
            '^' => Node::Assert(if self.flags.multiline { AssertKind::BolMulti } else { AssertKind::Bol }),
            '$' => Node::Assert(if self.flags.multiline { AssertKind::EolMulti } else { AssertKind::Eol }),
            '[' => Node::Class(self.parse_class()?),
            '(' => return self.parse_group(),
            '\\' => return self.parse_escape(),
            '*' | '+' | '?' => {
                self.pos -= 1;
                return Err(self.err(&format!("Dangling meta character '{}'", c)));
            }
            c => Node::Char(c, self.flags.ci),
        }))
    }

    fn parse_group_name(&mut self, end: char) -> Result<String> {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == end {
                break;
            }
            if !(c.is_ascii_alphanumeric() || c == '_') {
                return Err(self.err("Invalid group name"));
            }
            self.pos += 1;
        }
        if !self.eat(end) || start + 1 > self.pos {
            return Err(self.err("Invalid group name"));
        }
        Ok(self.chars[start..self.pos - 1].iter().collect())
    }

    fn parse_group(&mut self) -> Result<Option<Node>> {
        let saved_flags = self.flags;
        let node;
        if self.eat('?') {
            match self.peek() {
                Some(':') => {
                    self.pos += 1;
                    let inner = self.parse_alt()?;
                    node = Node::Group(Box::new(inner), None);
                }
                Some('>') => {
                    self.pos += 1;
                    let inner = self.parse_alt()?;
                    node = Node::Atomic(Box::new(inner));
                }
                Some('=') | Some('!') => {
                    let negate = self.peek() == Some('!');
                    self.pos += 1;
                    let inner = self.parse_alt()?;
                    node = Node::Look { node: Box::new(inner), ahead: true, negate };
                }
                Some('<') if matches!(self.peek_at(1), Some('=') | Some('!')) => {
                    let negate = self.peek_at(1) == Some('!');
                    self.pos += 2;
                    let inner = self.parse_alt()?;
                    node = Node::Look { node: Box::new(inner), ahead: false, negate };
                }
                Some('<') | Some('P') | Some('\'') => {
                    let end = if self.peek() == Some('\'') { '\'' } else { '>' };
                    if self.peek() == Some('P') {
                        self.pos += 1;
                        if !self.eat('<') {
                            return Err(self.err("Unknown group type"));
                        }
                    } else {
                        self.pos += 1;
                    }
                    let name = self.parse_group_name(end)?;
                    self.groups += 1;
                    let idx = self.groups;
                    if self.names.insert(name.clone(), idx).is_some() {
                        return Err(self.err(&format!("Named capturing group <{}> is already defined", name)));
                    }
                    let inner = self.parse_alt()?;
                    node = Node::Group(Box::new(inner), Some(idx));
                }
                _ => {
                    // inline flags: (?imsx-imsx) or (?imsx-imsx: ... )
                    let mut on = true;
                    let mut f = self.flags;
                    loop {
                        match self.peek() {
                            Some('i') => f.ci = on,
                            Some('s') => f.dotall = on,
                            Some('m') => f.multiline = on,
                            Some('x') => f.comments = on,
                            Some('u') | Some('d') | Some('U') => {}
                            Some('-') => on = false,
                            Some(')') => {
                                self.pos += 1;
                                // applies to the rest of the enclosing group
                                self.flags = f;
                                return Ok(None);
                            }
                            Some(':') => {
                                self.pos += 1;
                                self.flags = f;
                                let inner = self.parse_alt()?;
                                if !self.eat(')') {
                                    return Err(self.err("Unclosed group"));
                                }
                                self.flags = saved_flags;
                                return Ok(Some(Node::Group(Box::new(inner), None)));
                            }
                            _ => return Err(self.err("Unknown inline modifier")),
                        }
                        self.pos += 1;
                    }
                }
            }
        } else {
            self.groups += 1;
            let idx = self.groups;
            let inner = self.parse_alt()?;
            node = Node::Group(Box::new(inner), Some(idx));
        }
        if !self.eat(')') {
            return Err(self.err("Unclosed group"));
        }
        self.flags = saved_flags;
        Ok(Some(node))
    }

    fn parse_hex(&mut self, n: usize) -> Result<char> {
        let mut v: u32 = 0;
        for _ in 0..n {
            let c = self.peek().ok_or_else(|| self.err("Illegal hexadecimal escape sequence"))?;
            let d = c.to_digit(16).ok_or_else(|| self.err("Illegal hexadecimal escape sequence"))?;
            v = v * 16 + d;
            self.pos += 1;
        }
        char::from_u32(v).ok_or_else(|| self.err("Illegal hexadecimal escape sequence"))
    }

    /// Parse an escape that denotes a single char or a class item (used inside and outside classes).
    /// Returns Err for unknown escapes. `None` is returned for non-char escapes handled by callers.
    fn parse_escape_char(&mut self, c: char) -> Result<Option<EscapeResult>> {
        Ok(Some(match c {
            'd' => EscapeResult::Item(ClassItem::Digit(false)),
            'D' => EscapeResult::Item(ClassItem::Digit(true)),
            'w' => EscapeResult::Item(ClassItem::Word(false)),
            'W' => EscapeResult::Item(ClassItem::Word(true)),
            's' => EscapeResult::Item(ClassItem::Space(false)),
            'S' => EscapeResult::Item(ClassItem::Space(true)),
            'h' => EscapeResult::Item(ClassItem::Prop("Blank".into(), false)),
            'H' => EscapeResult::Item(ClassItem::Prop("Blank".into(), true)),
            'v' => EscapeResult::Item(ClassItem::Prop("VSpace".into(), false)),
            'V' => EscapeResult::Item(ClassItem::Prop("VSpace".into(), true)),
            'p' | 'P' => {
                let neg = c == 'P';
                let name = if self.eat('{') {
                    let start = self.pos;
                    while matches!(self.peek(), Some(ch) if ch != '}') {
                        self.pos += 1;
                    }
                    if !self.eat('}') {
                        return Err(self.err("Unclosed character family"));
                    }
                    self.chars[start..self.pos - 1].iter().collect::<String>()
                } else {
                    let ch = self.peek().ok_or_else(|| self.err("Illegal character family"))?;
                    self.pos += 1;
                    ch.to_string()
                };
                let name = name.trim_start_matches("Is").trim_start_matches("In").to_string();
                if !known_prop(&name) {
                    return Err(self.err(&format!("Unknown character property name {{{}}}", name)));
                }
                EscapeResult::Item(ClassItem::Prop(name, neg))
            }
            't' => EscapeResult::Char('\t'),
            'n' => EscapeResult::Char('\n'),
            'r' => EscapeResult::Char('\r'),
            'f' => EscapeResult::Char('\u{0c}'),
            'a' => EscapeResult::Char('\u{07}'),
            'e' => EscapeResult::Char('\u{1b}'),
            '0' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 3 {
                    match self.peek().and_then(|c| c.to_digit(8)) {
                        Some(d) if v * 8 + d <= 0o377 => {
                            v = v * 8 + d;
                            self.pos += 1;
                            n += 1;
                        }
                        _ => break,
                    }
                }
                if n == 0 {
                    return Err(self.err("Illegal octal escape sequence"));
                }
                EscapeResult::Char(char::from_u32(v).unwrap())
            }
            'x' => {
                if self.eat('{') {
                    let start = self.pos;
                    while matches!(self.peek(), Some(ch) if ch != '}') {
                        self.pos += 1;
                    }
                    let s: String = self.chars[start..self.pos].iter().collect();
                    if !self.eat('}') {
                        return Err(self.err("Unclosed hexadecimal escape sequence"));
                    }
                    let v = u32::from_str_radix(&s, 16).map_err(|_| self.err("Illegal hexadecimal escape sequence"))?;
                    EscapeResult::Char(char::from_u32(v).ok_or_else(|| self.err("Illegal hexadecimal escape sequence"))?)
                } else {
                    EscapeResult::Char(self.parse_hex(2)?)
                }
            }
            'u' => EscapeResult::Char(self.parse_hex(4)?),
            'c' => {
                let ch = self.peek().ok_or_else(|| self.err("Illegal control escape sequence"))?;
                self.pos += 1;
                EscapeResult::Char(char::from_u32((ch as u32) ^ 64).unwrap_or('?'))
            }
            c if c.is_ascii_alphanumeric() => return Ok(None),
            c => EscapeResult::Char(c),
        }))
    }

    fn parse_escape(&mut self) -> Result<Option<Node>> {
        let c = self.peek().ok_or_else(|| self.err("Unexpected internal error (trailing backslash)"))?;
        self.pos += 1;
        let node = match c {
            'b' => Node::Assert(AssertKind::WordB),
            'B' => Node::Assert(AssertKind::NotWordB),
            'A' => Node::Assert(AssertKind::StartText),
            'z' => Node::Assert(AssertKind::EndText),
            'Z' => Node::Assert(AssertKind::EndTextOptNl),
            'G' => Node::Assert(AssertKind::StartText),
            'Q' => {
                let mut items = Vec::new();
                while self.pos < self.chars.len() {
                    if self.peek() == Some('\\') && self.peek_at(1) == Some('E') {
                        self.pos += 2;
                        break;
                    }
                    items.push(Node::Char(self.chars[self.pos], self.flags.ci));
                    self.pos += 1;
                }
                return Ok(Some(Node::Group(Box::new(Node::Concat(items)), None)));
            }
            'k' => {
                if !self.eat('<') {
                    return Err(self.err("\\k is not followed by '<' for named capturing group"));
                }
                let name = self.parse_group_name('>')?;
                let idx = *self.names.get(&name).ok_or_else(|| self.err(&format!("named capturing group <{}> does not exist", name)))?;
                Node::Backref(idx, self.flags.ci)
            }
            '1'..='9' => {
                let mut n = c.to_digit(10).unwrap() as usize;
                // Java: take further digits while the resulting group number exists
                while let Some(d) = self.peek().and_then(|c| c.to_digit(10)) {
                    let nn = n * 10 + d as usize;
                    if nn <= self.groups {
                        n = nn;
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
                Node::Backref(n, self.flags.ci)
            }
            _ => match self.parse_escape_char(c)? {
                Some(EscapeResult::Char(ch)) => Node::Char(ch, self.flags.ci),
                Some(EscapeResult::Item(it)) => Node::Class(CharClass { items: vec![it], negated: false, ci: self.flags.ci, and: vec![] }),
                None => {
                    self.pos -= 1;
                    return Err(self.err("Illegal/unsupported escape sequence"));
                }
            },
        };
        Ok(Some(node))
    }

    fn parse_class(&mut self) -> Result<CharClass> {
        let mut cls = CharClass { items: Vec::new(), negated: false, ci: self.flags.ci, and: Vec::new() };
        if self.eat('^') {
            cls.negated = true;
        }
        let mut first = true;
        loop {
            let c = match self.peek() {
                None => return Err(self.err("Unclosed character class")),
                Some(c) => c,
            };
            if c == ']' && !first {
                self.pos += 1;
                break;
            }
            first = false;
            if c == '[' {
                self.pos += 1;
                let nested = self.parse_class()?;
                cls.items.push(ClassItem::Nested(Box::new(nested)));
                continue;
            }
            if c == '&' && self.peek_at(1) == Some('&') {
                self.pos += 2;
                // intersection with the rest of the class
                let rest = if self.peek() == Some('[') {
                    self.pos += 1;
                    let n = self.parse_class()?;
                    // consume remaining until ']' as part of this class
                    n
                } else {
                    // parse until ']' (not consuming it) as a class
                    let mut sub = CharClass { items: Vec::new(), negated: false, ci: self.flags.ci, and: Vec::new() };
                    while let Some(ch) = self.peek() {
                        if ch == ']' {
                            break;
                        }
                        let item = self.parse_class_item()?;
                        sub.items.push(item);
                    }
                    sub
                };
                cls.and.push(rest);
                continue;
            }
            let item = self.parse_class_item()?;
            cls.items.push(item);
        }
        Ok(cls)
    }

    fn parse_class_char(&mut self) -> Result<EscapeResult> {
        let c = self.peek().ok_or_else(|| self.err("Unclosed character class"))?;
        self.pos += 1;
        if c == '\\' {
            let e = self.peek().ok_or_else(|| self.err("Unclosed character class"))?;
            self.pos += 1;
            if e == 'Q' {
                // quoted chars inside class: treat each as single char item - simplified: only first
                let mut chars = Vec::new();
                while self.pos < self.chars.len() {
                    if self.peek() == Some('\\') && self.peek_at(1) == Some('E') {
                        self.pos += 2;
                        break;
                    }
                    chars.push(self.chars[self.pos]);
                    self.pos += 1;
                }
                let items = chars.into_iter().map(|c| ClassItem::Range(c, c)).collect();
                return Ok(EscapeResult::Item(ClassItem::Nested(Box::new(CharClass {
                    items,
                    negated: false,
                    ci: self.flags.ci,
                    and: vec![],
                }))));
            }
            match self.parse_escape_char(e)? {
                Some(r) => Ok(r),
                None => {
                    if e == 'b' {
                        return Ok(EscapeResult::Char('\u{08}'));
                    }
                    self.pos -= 1;
                    Err(self.err("Illegal/unsupported escape sequence"))
                }
            }
        } else {
            Ok(EscapeResult::Char(c))
        }
    }

    fn parse_class_item(&mut self) -> Result<ClassItem> {
        let lo = self.parse_class_char()?;
        let lo = match lo {
            EscapeResult::Item(it) => return Ok(it),
            EscapeResult::Char(c) => c,
        };
        if self.peek() == Some('-') && self.peek_at(1).is_some() && self.peek_at(1) != Some(']') && self.peek_at(1) != Some('[') {
            self.pos += 1;
            let hi = match self.parse_class_char()? {
                EscapeResult::Char(c) => c,
                EscapeResult::Item(_) => return Err(self.err("Illegal character range")),
            };
            if hi < lo {
                return Err(self.err("Illegal character range"));
            }
            return Ok(ClassItem::Range(lo, hi));
        }
        Ok(ClassItem::Range(lo, lo))
    }
}

enum EscapeResult {
    Char(char),
    Item(ClassItem),
}

fn known_prop(name: &str) -> bool {
    matches!(
        name,
        "Lower"
            | "Upper"
            | "ASCII"
            | "Alpha"
            | "Digit"
            | "Alnum"
            | "Punct"
            | "Graph"
            | "Print"
            | "Blank"
            | "Cntrl"
            | "XDigit"
            | "Space"
            | "VSpace"
            | "L"
            | "Lu"
            | "Ll"
            | "N"
            | "Nd"
            | "P"
            | "Z"
            | "Zs"
            | "S"
            | "Letter"
            | "Alphabetic"
            | "javaLowerCase"
            | "javaUpperCase"
            | "javaWhitespace"
            | "Latin"
    )
}

fn prop_matches(name: &str, c: char) -> bool {
    match name {
        "Lower" => c.is_ascii_lowercase(),
        "Upper" => c.is_ascii_uppercase(),
        "ASCII" => c.is_ascii(),
        "Alpha" => c.is_ascii_alphabetic(),
        "Digit" => c.is_ascii_digit(),
        "Alnum" => c.is_ascii_alphanumeric(),
        "Punct" => c.is_ascii_punctuation(),
        "Graph" => c.is_ascii_graphic(),
        "Print" => c.is_ascii_graphic() || c == ' ',
        "Blank" => c == ' ' || c == '\t',
        "Cntrl" => c.is_ascii_control(),
        "XDigit" => c.is_ascii_hexdigit(),
        "Space" => matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}'),
        "VSpace" => matches!(c, '\n' | '\r' | '\u{0b}' | '\u{0c}' | '\u{85}' | '\u{2028}' | '\u{2029}'),
        "L" | "Letter" | "Alphabetic" => c.is_alphabetic(),
        "Lu" | "javaUpperCase" => c.is_uppercase(),
        "Ll" | "javaLowerCase" => c.is_lowercase(),
        "N" | "Nd" => c.is_numeric(),
        "P" => c.is_ascii_punctuation() || (!c.is_alphanumeric() && !c.is_whitespace() && !c.is_control() && (c as u32) > 127),
        "Z" | "Zs" | "javaWhitespace" => c.is_whitespace(),
        "S" => matches!(c, '$' | '+' | '<' | '=' | '>' | '^' | '`' | '|' | '~'),
        "Latin" => c.is_ascii_alphabetic() || ('\u{00C0}'..='\u{024F}').contains(&c),
        _ => false,
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

impl CharClass {
    fn matches_raw(&self, c: char) -> bool {
        let mut m = self.items.iter().any(|it| match it {
            ClassItem::Range(lo, hi) => *lo <= c && c <= *hi,
            ClassItem::Digit(neg) => c.is_ascii_digit() != *neg,
            ClassItem::Word(neg) => is_word(c) != *neg,
            ClassItem::Space(neg) => matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}') != *neg,
            ClassItem::Prop(name, neg) => prop_matches(name, c) != *neg,
            ClassItem::Nested(n) => n.matches(c),
        });
        if m && !self.and.is_empty() {
            m = self.and.iter().all(|a| a.matches(c));
        }
        m
    }

    fn matches(&self, c: char) -> bool {
        let mut m = self.matches_raw(c);
        if !m && self.ci {
            let lc = c.to_lowercase().next().unwrap_or(c);
            let uc = c.to_uppercase().next().unwrap_or(c);
            m = (lc != c && self.matches_raw(lc)) || (uc != c && self.matches_raw(uc));
        }
        m != self.negated
    }
}

// ---------------------------------------------------------------------------
// Compiler
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Inst {
    Char(char),
    CharCI(char),
    Any,
    AnyNoNl,
    Class(usize),
    Split(usize, usize),
    Jmp(usize),
    Save(usize),
    Assert(AssertKind),
    /// Sub-program at `start` (ending with Match) used as a look-around; continue at `next`.
    Look { start: usize, next: usize, ahead: bool, negate: bool },
    /// Atomic group: run sub-program at `start` once, commit to its first match.
    Atomic { start: usize, next: usize },
    Backref(usize, bool),
    /// Record the current position in a progress slot.
    ProgressSet(usize),
    /// Fail if the position did not advance since the matching ProgressSet.
    ProgressCheck(usize),
    Match,
}

struct Compiler {
    prog: Vec<Inst>,
    classes: Vec<CharClass>,
    progress_slots: usize,
    base_slots: usize,
}

impl Compiler {
    fn emit(&mut self, i: Inst) -> Result<usize> {
        if self.prog.len() >= MAX_PROGRAM {
            return Err(CamelError::syntax("Regular expression too large"));
        }
        self.prog.push(i);
        Ok(self.prog.len() - 1)
    }

    fn can_be_empty(node: &Node) -> bool {
        match node {
            Node::Empty | Node::Assert(_) | Node::Look { .. } => true,
            Node::Char(..) | Node::Any(_) | Node::Class(_) => false,
            Node::Backref(..) => true,
            Node::Group(n, _) | Node::Atomic(n) => Self::can_be_empty(n),
            Node::Concat(v) => v.iter().all(Self::can_be_empty),
            Node::Alt(v) => v.iter().any(Self::can_be_empty),
            Node::Repeat { node, min, .. } => *min == 0 || Self::can_be_empty(node),
        }
    }

    /// Compile a sub-program terminated by Match, placed out of line. Returns its start.
    fn sub_program(&mut self, node: &Node) -> Result<(usize, usize)> {
        // layout: Jmp over | body | Match
        let jmp = self.emit(Inst::Jmp(0))?;
        let start = self.prog.len();
        self.compile(node)?;
        self.emit(Inst::Match)?;
        let next = self.prog.len();
        self.prog[jmp] = Inst::Jmp(next);
        Ok((start, next))
    }

    fn compile(&mut self, node: &Node) -> Result<()> {
        match node {
            Node::Empty => {}
            Node::Char(c, ci) => {
                if *ci && (c.is_lowercase() || c.is_uppercase()) {
                    self.emit(Inst::CharCI(fold(*c)))?;
                } else {
                    self.emit(Inst::Char(*c))?;
                }
            }
            Node::Any(dotall) => {
                self.emit(if *dotall { Inst::Any } else { Inst::AnyNoNl })?;
            }
            Node::Class(cls) => {
                self.classes.push(cls.clone());
                let idx = self.classes.len() - 1;
                self.emit(Inst::Class(idx))?;
            }
            Node::Assert(a) => {
                self.emit(Inst::Assert(*a))?;
            }
            Node::Group(inner, idx) => match idx {
                Some(i) => {
                    self.emit(Inst::Save(2 * i))?;
                    self.compile(inner)?;
                    self.emit(Inst::Save(2 * i + 1))?;
                }
                None => self.compile(inner)?,
            },
            Node::Concat(items) => {
                for it in items {
                    self.compile(it)?;
                }
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (i, alt) in alts.iter().enumerate() {
                    if i + 1 < alts.len() {
                        let split = self.emit(Inst::Split(0, 0))?;
                        self.compile(alt)?;
                        jumps.push(self.emit(Inst::Jmp(0))?);
                        let next = self.prog.len();
                        self.prog[split] = Inst::Split(split + 1, next);
                    } else {
                        self.compile(alt)?;
                    }
                }
                let end = self.prog.len();
                for j in jumps {
                    self.prog[j] = Inst::Jmp(end);
                }
            }
            Node::Repeat { node: inner, min, max, greedy, possessive } => {
                if *possessive {
                    let plain = Node::Repeat { node: inner.clone(), min: *min, max: *max, greedy: true, possessive: false };
                    return self.compile(&Node::Atomic(Box::new(plain)));
                }
                for _ in 0..*min {
                    self.compile(inner)?;
                }
                let empty = Self::can_be_empty(inner);
                match max {
                    None => {
                        // L: Split(B, out); B: [ProgressSet] body [ProgressCheck]; Jmp L
                        let l = self.emit(Inst::Split(0, 0))?;
                        let slot = if empty {
                            let s = self.base_slots + self.progress_slots;
                            self.progress_slots += 1;
                            self.emit(Inst::ProgressSet(s))?;
                            Some(s)
                        } else {
                            None
                        };
                        self.compile(inner)?;
                        if let Some(s) = slot {
                            self.emit(Inst::ProgressCheck(s))?;
                        }
                        self.emit(Inst::Jmp(l))?;
                        let out = self.prog.len();
                        self.prog[l] = if *greedy { Inst::Split(l + 1, out) } else { Inst::Split(out, l + 1) };
                    }
                    Some(m) => {
                        let extra = m - min;
                        let mut splits = Vec::new();
                        for _ in 0..extra {
                            let s = self.emit(Inst::Split(0, 0))?;
                            splits.push(s);
                            self.compile(inner)?;
                        }
                        let out = self.prog.len();
                        for s in splits {
                            self.prog[s] = if *greedy { Inst::Split(s + 1, out) } else { Inst::Split(out, s + 1) };
                        }
                    }
                }
            }
            Node::Look { node: inner, ahead, negate } => {
                let at = self.emit(Inst::Jmp(0))?; // placeholder replaced below
                let (start, next) = self.sub_program(inner)?;
                self.prog[at] = Inst::Look { start, next, ahead: *ahead, negate: *negate };
            }
            Node::Atomic(inner) => {
                let at = self.emit(Inst::Jmp(0))?;
                let (start, next) = self.sub_program(inner)?;
                self.prog[at] = Inst::Atomic { start, next };
            }
            Node::Backref(i, ci) => {
                self.emit(Inst::Backref(*i, *ci))?;
            }
        }
        Ok(())
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn eq_ci(a: char, b: char) -> bool {
    a == b || fold(a) == fold(b) || a.to_uppercase().next() == b.to_uppercase().next()
}

// ---------------------------------------------------------------------------
// VM
// ---------------------------------------------------------------------------

enum Frame {
    Branch(usize, usize),
    Restore(usize, usize),
}

struct Vm<'a> {
    prog: &'a [Inst],
    classes: &'a [CharClass],
    text: &'a [char],
    steps: u64,
    exhausted: bool,
}

const NONE: usize = usize::MAX;

impl<'a> Vm<'a> {
    fn assert(&self, a: AssertKind, pos: usize) -> bool {
        let t = self.text;
        let n = t.len();
        match a {
            AssertKind::Bol | AssertKind::StartText => pos == 0,
            AssertKind::BolMulti => pos == 0 || (t[pos - 1] == '\n' && pos < n),
            AssertKind::Eol => pos == n || (pos + 1 == n && t[pos] == '\n') || (pos + 2 == n && t[pos] == '\r' && t[pos + 1] == '\n'),
            AssertKind::EolMulti => pos == n || t[pos] == '\n' || t[pos] == '\r',
            AssertKind::EndText => pos == n,
            AssertKind::EndTextOptNl => pos == n || (pos + 1 == n && t[pos] == '\n'),
            AssertKind::WordB | AssertKind::NotWordB => {
                let before = pos > 0 && is_word(t[pos - 1]);
                let after = pos < n && is_word(t[pos]);
                (before != after) == (a == AssertKind::WordB)
            }
        }
    }

    /// Run from `pc` at `pos`. On success returns the end position; `slots` hold captures.
    /// `required_end`: if set, Match only succeeds at that position.
    fn run(&mut self, pc: usize, pos: usize, slots: &mut Vec<usize>, required_end: Option<usize>) -> Option<usize> {
        let mut stack: Vec<Frame> = Vec::new();
        let mut pc = pc;
        let mut pos = pos;
        let text = self.text;
        loop {
            self.steps += 1;
            if self.steps > STEP_LIMIT {
                self.exhausted = true;
                return None;
            }
            let ok = match &self.prog[pc] {
                Inst::Char(c) => {
                    if pos < text.len() && text[pos] == *c {
                        pos += 1;
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::CharCI(c) => {
                    if pos < text.len() && eq_ci(text[pos], *c) {
                        pos += 1;
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::Any => {
                    if pos < text.len() {
                        pos += 1;
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::AnyNoNl => {
                    if pos < text.len() && !matches!(text[pos], '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}') {
                        pos += 1;
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::Class(i) => {
                    if pos < text.len() && self.classes[*i].matches(text[pos]) {
                        pos += 1;
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::Split(a, b) => {
                    stack.push(Frame::Branch(*b, pos));
                    pc = *a;
                    true
                }
                Inst::Jmp(a) => {
                    pc = *a;
                    true
                }
                Inst::Save(s) | Inst::ProgressSet(s) => {
                    stack.push(Frame::Restore(*s, slots[*s]));
                    slots[*s] = pos;
                    pc += 1;
                    true
                }
                Inst::ProgressCheck(s) => {
                    if slots[*s] == pos {
                        false
                    } else {
                        pc += 1;
                        true
                    }
                }
                Inst::Assert(a) => {
                    if self.assert(*a, pos) {
                        pc += 1;
                        true
                    } else {
                        false
                    }
                }
                Inst::Backref(g, ci) => {
                    let (s, e) = (slots.get(2 * g).copied().unwrap_or(NONE), slots.get(2 * g + 1).copied().unwrap_or(NONE));
                    if s == NONE || e == NONE {
                        false
                    } else {
                        let len = e - s;
                        if pos + len <= text.len()
                            && (0..len).all(|k| if *ci { eq_ci(text[s + k], text[pos + k]) } else { text[s + k] == text[pos + k] })
                        {
                            pos += len;
                            pc += 1;
                            true
                        } else {
                            false
                        }
                    }
                }
                Inst::Look { start, next, ahead, negate } => {
                    let (start, next, ahead, negate) = (*start, *next, *ahead, *negate);
                    let before = slots.clone();
                    let found = if ahead {
                        self.run(start, pos, slots, None).is_some()
                    } else {
                        let mut f = false;
                        let mut k = pos as isize;
                        while k >= 0 {
                            if self.run(start, k as usize, slots, Some(pos)).is_some() {
                                f = true;
                                break;
                            }
                            k -= 1;
                        }
                        f
                    };
                    if self.exhausted {
                        return None;
                    }
                    if found != negate {
                        if found {
                            // keep captures, but make them undoable
                            for (i, (old, new)) in before.iter().zip(slots.iter()).enumerate() {
                                if old != new {
                                    stack.push(Frame::Restore(i, *old));
                                }
                            }
                        } else {
                            *slots = before;
                        }
                        pc = next;
                        true
                    } else {
                        *slots = before;
                        false
                    }
                }
                Inst::Atomic { start, next } => {
                    let (start, next) = (*start, *next);
                    let before = slots.clone();
                    match self.run(start, pos, slots, None) {
                        Some(end) => {
                            for (i, (old, new)) in before.iter().zip(slots.iter()).enumerate() {
                                if old != new {
                                    stack.push(Frame::Restore(i, *old));
                                }
                            }
                            pos = end;
                            pc = next;
                            true
                        }
                        None => {
                            *slots = before;
                            if self.exhausted {
                                return None;
                            }
                            false
                        }
                    }
                }
                Inst::Match => {
                    if required_end.map(|r| r == pos).unwrap_or(true) {
                        return Some(pos);
                    }
                    false
                }
            };
            if !ok {
                // backtrack
                loop {
                    match stack.pop() {
                        None => return None,
                        Some(Frame::Restore(s, v)) => slots[s] = v,
                        Some(Frame::Branch(p, ps)) => {
                            pc = p;
                            pos = ps;
                            break;
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct Program {
    insts: Vec<Inst>,
    classes: Vec<CharClass>,
    groups: usize,
    names: BTreeMap<String, usize>,
    nslots: usize,
}

/// A compiled regular expression (Java regex syntax subset).
#[derive(Clone, Debug)]
pub struct Regex {
    pattern: String,
    prog: Arc<Program>,
}

/// The result of a successful match: group spans as byte offsets.
#[derive(Clone, Debug, PartialEq)]
pub struct Captures {
    text: String,
    spans: Vec<Option<(usize, usize)>>,
    names: BTreeMap<String, usize>,
}

impl Captures {
    /// Number of groups including group 0.
    pub fn len(&self) -> usize {
        self.spans.len()
    }
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
    /// Byte span of group `i`.
    pub fn span(&self, i: usize) -> Option<(usize, usize)> {
        self.spans.get(i).copied().flatten()
    }
    /// Text of group `i`.
    pub fn get(&self, i: usize) -> Option<&str> {
        self.span(i).map(|(s, e)| &self.text[s..e])
    }
    /// Text of a named group.
    pub fn name(&self, name: &str) -> Option<&str> {
        self.names.get(name).and_then(|i| self.get(*i))
    }
    /// All groups as optional strings (index 0 = whole match).
    pub fn to_vec(&self) -> Vec<Option<String>> {
        (0..self.spans.len()).map(|i| self.get(i).map(|s| s.to_string())).collect()
    }
}

struct Prepared {
    chars: Vec<char>,
    offsets: Vec<usize>,
}

fn prepare(text: &str) -> Prepared {
    let mut chars = Vec::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len() + 1);
    for (i, c) in text.char_indices() {
        chars.push(c);
        offsets.push(i);
    }
    offsets.push(text.len());
    Prepared { chars, offsets }
}

impl Regex {
    /// Compile a pattern. Returns an `ExpressionIllegalSyntaxException` error on invalid syntax.
    pub fn new(pattern: &str) -> Result<Regex> {
        Self::with_flags(pattern, false)
    }

    /// Compile a pattern, optionally case-insensitive.
    pub fn with_flags(pattern: &str, case_insensitive: bool) -> Result<Regex> {
        let mut p = Parser {
            chars: pattern.chars().collect(),
            pos: 0,
            pattern,
            groups: 0,
            names: BTreeMap::new(),
            flags: Flags { ci: case_insensitive, dotall: false, multiline: false, comments: false },
            depth: 0,
        };
        let ast = p.parse_alt()?;
        if p.pos < p.chars.len() {
            return Err(p.err("Unmatched closing ')'"));
        }
        let base = 2 * (p.groups + 1);
        let mut c = Compiler { prog: Vec::new(), classes: Vec::new(), progress_slots: 0, base_slots: base };
        c.emit(Inst::Save(0))?;
        c.compile(&ast)?;
        c.emit(Inst::Save(1))?;
        c.emit(Inst::Match)?;
        let nslots = base + c.progress_slots;
        Ok(Regex {
            pattern: pattern.to_string(),
            prog: Arc::new(Program { insts: c.prog, classes: c.classes, groups: p.groups, names: p.names, nslots }),
        })
    }

    /// Quote a literal string so it matches itself (Java `Pattern.quote`-like, but escaping each char).
    pub fn quote(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            if "\\^$.|?*+()[]{}".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }

    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Number of capturing groups (excluding group 0).
    pub fn group_count(&self) -> usize {
        self.prog.groups
    }

    /// Named groups and their indexes.
    pub fn group_names(&self) -> &BTreeMap<String, usize> {
        &self.prog.names
    }

    /// Index of a named group.
    pub fn group_index(&self, name: &str) -> Option<usize> {
        self.prog.names.get(name).copied()
    }

    fn vm<'a>(&'a self, chars: &'a [char]) -> Vm<'a> {
        Vm { prog: &self.prog.insts, classes: &self.prog.classes, text: chars, steps: 0, exhausted: false }
    }

    /// Find the first match starting at char index >= `start`. Returns slots (char indexes).
    fn search(&self, chars: &[char], start: usize, full: bool) -> Option<Vec<usize>> {
        let mut vm = self.vm(chars);
        let mut slots = vec![NONE; self.prog.nslots];
        if full {
            return vm.run(0, start, &mut slots, Some(chars.len())).map(|_| slots);
        }
        let mut s = start;
        while s <= chars.len() {
            for x in slots.iter_mut() {
                *x = NONE;
            }
            if vm.run(0, s, &mut slots, None).is_some() {
                return Some(slots);
            }
            if vm.exhausted {
                return None;
            }
            s += 1;
        }
        None
    }

    fn to_captures(&self, text: &str, prep: &Prepared, slots: &[usize]) -> Captures {
        let spans = (0..=self.prog.groups)
            .map(|g| {
                let (s, e) = (slots[2 * g], slots[2 * g + 1]);
                if s == NONE || e == NONE {
                    None
                } else {
                    Some((prep.offsets[s], prep.offsets[e]))
                }
            })
            .collect();
        Captures { text: text.to_string(), spans, names: self.prog.names.clone() }
    }

    /// True if the regex matches anywhere in `text` (Java `Matcher.find`).
    pub fn is_match(&self, text: &str) -> bool {
        let p = prepare(text);
        self.search(&p.chars, 0, false).is_some()
    }

    /// True if the regex matches the entire `text` (Java `String.matches`).
    pub fn is_full_match(&self, text: &str) -> bool {
        let p = prepare(text);
        self.search(&p.chars, 0, true).is_some()
    }

    /// Byte span of the first match.
    pub fn find(&self, text: &str) -> Option<(usize, usize)> {
        self.find_at(text, 0)
    }

    /// Byte span of the first match at or after byte offset `start`.
    pub fn find_at(&self, text: &str, start: usize) -> Option<(usize, usize)> {
        let p = prepare(text);
        let cstart = p.offsets.iter().position(|&o| o >= start)?;
        self.search(&p.chars, cstart, false).map(|s| (p.offsets[s[0]], p.offsets[s[1]]))
    }

    /// All non-overlapping match spans (byte offsets).
    pub fn find_iter(&self, text: &str) -> std::vec::IntoIter<(usize, usize)> {
        self.captures_iter(text).into_iter().map(|c| c.span(0).unwrap()).collect::<Vec<_>>().into_iter()
    }

    /// All non-overlapping matches as strings.
    pub fn find_all(&self, text: &str) -> Vec<String> {
        self.find_iter(text).map(|(s, e)| text[s..e].to_string()).collect()
    }

    /// Groups of the first match (index 0 = whole match).
    pub fn captures(&self, text: &str) -> Option<Vec<Option<String>>> {
        self.captures_match(text).map(|c| c.to_vec())
    }

    /// Groups of a full match (the whole text must match).
    pub fn full_captures(&self, text: &str) -> Option<Captures> {
        let p = prepare(text);
        self.search(&p.chars, 0, true).map(|s| self.to_captures(text, &p, &s))
    }

    /// First match with spans and named-group access.
    pub fn captures_match(&self, text: &str) -> Option<Captures> {
        let p = prepare(text);
        self.search(&p.chars, 0, false).map(|s| self.to_captures(text, &p, &s))
    }

    /// Value of a named group in the first match.
    pub fn named_capture(&self, text: &str, name: &str) -> Option<String> {
        self.captures_match(text).and_then(|c| c.name(name).map(|s| s.to_string()))
    }

    /// All non-overlapping matches with their groups.
    pub fn captures_iter(&self, text: &str) -> Vec<Captures> {
        let p = prepare(text);
        let mut out = Vec::new();
        let mut start = 0;
        while start <= p.chars.len() {
            match self.search(&p.chars, start, false) {
                None => break,
                Some(s) => {
                    let (ms, me) = (s[0], s[1]);
                    out.push(self.to_captures(text, &p, &s));
                    start = if me == ms { me + 1 } else { me };
                }
            }
        }
        out
    }

    fn expand(&self, caps: &Captures, replacement: &str, out: &mut String) {
        let r: Vec<char> = replacement.chars().collect();
        let mut i = 0;
        while i < r.len() {
            let c = r[i];
            if c == '\\' && i + 1 < r.len() {
                out.push(r[i + 1]);
                i += 2;
                continue;
            }
            if c == '$' && i + 1 < r.len() {
                if r[i + 1] == '{' {
                    if let Some(end) = r[i + 2..].iter().position(|&x| x == '}') {
                        let name: String = r[i + 2..i + 2 + end].iter().collect();
                        let v = match name.parse::<usize>() {
                            Ok(n) => caps.get(n),
                            Err(_) => caps.name(&name),
                        };
                        out.push_str(v.unwrap_or(""));
                        i += end + 3;
                        continue;
                    }
                } else if r[i + 1].is_ascii_digit() {
                    let mut n = r[i + 1].to_digit(10).unwrap() as usize;
                    let mut j = i + 2;
                    while j < r.len() && r[j].is_ascii_digit() {
                        let nn = n * 10 + r[j].to_digit(10).unwrap() as usize;
                        if nn <= self.prog.groups {
                            n = nn;
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    out.push_str(caps.get(n).unwrap_or(""));
                    i = j;
                    continue;
                }
            }
            out.push(c);
            i += 1;
        }
    }

    /// Replace all matches with `replacement` (supports `$1`, `${name}` and `\$` escapes).
    pub fn replace_all(&self, text: &str, replacement: &str) -> String {
        self.replace_n(text, replacement, usize::MAX)
    }

    /// Replace the first match.
    pub fn replace_first(&self, text: &str, replacement: &str) -> String {
        self.replace_n(text, replacement, 1)
    }

    fn replace_n(&self, text: &str, replacement: &str, n: usize) -> String {
        let mut out = String::new();
        let mut last = 0;
        for (k, caps) in self.captures_iter(text).into_iter().enumerate() {
            if k >= n {
                break;
            }
            let (s, e) = caps.span(0).unwrap();
            out.push_str(&text[last..s]);
            self.expand(&caps, replacement, &mut out);
            last = e;
        }
        out.push_str(&text[last..]);
        out
    }

    /// Split around matches with Java `String.split` semantics (trailing empty strings removed,
    /// no leading empty string for a zero-width match at the start).
    pub fn split(&self, text: &str) -> Vec<String> {
        self.split_limit(text, 0)
    }

    /// Split with a Java-style limit (`0` = remove trailing empties, negative = keep all,
    /// positive = at most `limit` parts).
    pub fn split_limit(&self, text: &str, limit: i32) -> Vec<String> {
        if text.is_empty() {
            return vec![String::new()];
        }
        let mut parts = Vec::new();
        let mut last = 0;
        for (s, e) in self.find_iter(text) {
            if limit > 0 && parts.len() as i32 >= limit - 1 {
                break;
            }
            if e == 0 && s == 0 {
                continue; // zero-width match at the beginning
            }
            if s == e && s >= text.len() {
                break;
            }
            parts.push(text[last..s].to_string());
            last = e;
        }
        parts.push(text[last..].to_string());
        if limit == 0 {
            while parts.len() > 1 && parts.last().map(|p| p.is_empty()).unwrap_or(false) {
                parts.pop();
            }
            if parts.len() == 1 && parts[0].is_empty() {
                return parts;
            }
        }
        parts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Regex::new(p).unwrap().is_match(t)
    }
    fn fm(p: &str, t: &str) -> bool {
        Regex::new(p).unwrap().is_full_match(t)
    }

    #[test]
    fn literals_and_classes() {
        assert!(m("abc", "xxabcxx"));
        assert!(!m("abd", "xxabcxx"));
        assert!(fm("a.c", "abc"));
        assert!(!fm("a.c", "a\nc"));
        assert!(fm("(?s)a.c", "a\nc"));
        assert!(fm("[a-z]+", "hello"));
        assert!(!fm("[a-z]+", "Hello"));
        assert!(fm("[^0-9]+", "abc"));
        assert!(!fm("[^0-9]+", "ab1"));
        assert!(fm("\\d{3}-\\d{4}", "555-1234"));
        assert!(fm("\\w+\\s\\w+", "foo bar"));
        assert!(fm("\\D\\W\\S", "a b"));
        assert!(fm("a\\.b\\\\c", "a.b\\c"));
        assert!(fm("[\\d.]+", "1.5"));
        assert!(fm("[a-c&&[b-d]]", "b"));
        assert!(!fm("[a-c&&[b-d]]", "a"));
        assert!(fm("\\p{Alpha}+\\p{Digit}", "abc1"));
        assert!(fm("[-a]+", "-a-"));
        assert!(fm("[a-]+", "-a-"));
        assert!(fm("[]a]+", "]a"));
        assert!(fm("\\x41\\u0042\\t", "AB\t"));
        assert!(fm("\\Q.*\\E+", ".*.*"));
    }

    #[test]
    fn anchors() {
        assert!(m("^abc", "abcdef"));
        assert!(!m("^bcd", "abcdef"));
        assert!(m("def$", "abcdef"));
        assert!(m("def$", "abcdef\n"));
        assert!(m("(?m)^b$", "a\nb\nc"));
        assert!(!m("^b$", "a\nb\nc"));
        assert!(m("\\bfoo\\b", "a foo b"));
        assert!(!m("\\bfoo\\b", "afoob"));
        assert!(m("\\Bo\\B", "foo"));
        assert!(m("\\Aab\\z", "ab"));
    }

    #[test]
    fn quantifiers() {
        assert!(fm("a*", ""));
        assert!(fm("a+b?", "aaab"));
        assert!(fm("a{2}", "aa"));
        assert!(!fm("a{2}", "aaa"));
        assert!(fm("a{2,}", "aaaa"));
        assert!(fm("a{2,3}", "aaa"));
        assert!(!fm("a{2,3}", "aaaa"));
        let re = Regex::new("<.+?>").unwrap();
        assert_eq!(re.find("<a><b>"), Some((0, 3)));
        let re = Regex::new("<.+>").unwrap();
        assert_eq!(re.find("<a><b>"), Some((0, 6)));
        assert!(!fm("a++a", "aaa"));
        assert!(fm("(?>a+)b", "aab"));
        assert!(fm("x{", "x{"));
        assert!(fm("(a*)*b", "aaab"));
        assert!(fm("(a|)*b", "aab"));
    }

    #[test]
    fn groups_and_alternation() {
        let re = Regex::new("(\\w+)@(\\w+)\\.com").unwrap();
        let c = re.captures("mail: john@example.com!").unwrap();
        assert_eq!(c[0].as_deref(), Some("john@example.com"));
        assert_eq!(c[1].as_deref(), Some("john"));
        assert_eq!(c[2].as_deref(), Some("example"));
        let re = Regex::new("(?<year>\\d{4})-(?<month>\\d{2})").unwrap();
        assert_eq!(re.named_capture("on 2024-03", "month").as_deref(), Some("03"));
        assert_eq!(re.group_index("year"), Some(1));
        assert!(fm("(?:ab)+", "ababab"));
        assert!(fm("cat|dog", "dog"));
        assert!(fm("gr(a|e)y", "grey"));
        let re = Regex::new("(a)|(b)").unwrap();
        assert_eq!(re.captures("b").unwrap(), vec![Some("b".into()), None, Some("b".into())]);
        assert!(fm("(\\w)\\1", "aa"));
        assert!(!fm("(\\w)\\1", "ab"));
        assert!(fm("(?<x>a)\\k<x>", "aa"));
    }

    #[test]
    fn lookaround_and_flags() {
        assert!(fm("(?i)hello", "HeLLo"));
        assert!(fm("(?i:a)b", "Ab"));
        assert!(!fm("(?i:a)b", "AB"));
        assert!(fm("(?i)[a-c]+", "ABC"));
        assert!(m("foo(?=bar)", "foobar"));
        assert!(!m("foo(?=bar)", "foobaz"));
        assert!(m("foo(?!bar)", "foobaz"));
        assert!(m("(?<=\\$)\\d+", "cost $42"));
        assert_eq!(Regex::new("(?<=\\$)\\d+").unwrap().find("cost $42"), Some((6, 8)));
        assert!(!m("(?<!\\$)\\b\\d+", "$42"));
        assert!(Regex::with_flags("abc", true).unwrap().is_full_match("ABC"));
    }

    #[test]
    fn replace_split_iter() {
        let re = Regex::new("(\\w+) (\\w+)").unwrap();
        assert_eq!(re.replace_all("hello world", "$2 $1"), "world hello");
        let re = Regex::new("(?<first>\\w+)-(?<second>\\w+)").unwrap();
        assert_eq!(re.replace_all("a-b c-d", "${second}+${first}"), "b+a d+c");
        let re = Regex::new("\\d").unwrap();
        assert_eq!(re.replace_all("a1b22", "#"), "a#b##");
        assert_eq!(re.replace_first("a1b22", "#"), "a#b22");
        assert_eq!(re.replace_all("a1", "\\$"), "a$");
        let re = Regex::new("\\s*,\\s*").unwrap();
        assert_eq!(re.split("a , b,c,,"), vec!["a", "b", "c"]);
        assert_eq!(re.split_limit("a,b,c", 2), vec!["a", "b,c"]);
        assert_eq!(re.split_limit("a,b,", -1), vec!["a", "b", ""]);
        assert_eq!(Regex::new("").unwrap().split("abc"), vec!["a", "b", "c"]);
        assert_eq!(Regex::new("x").unwrap().split(""), vec![""]);
        let re = Regex::new("\\d+").unwrap();
        assert_eq!(re.find_all("a12b3c456"), vec!["12", "3", "456"]);
        assert_eq!(re.find_iter("é12").collect::<Vec<_>>(), vec![(2, 4)]);
        let re = Regex::new("a*").unwrap();
        assert_eq!(re.find_iter("baa").count(), 3);
    }

    #[test]
    fn errors_and_guards() {
        assert!(Regex::new("(abc").is_err());
        assert!(Regex::new("abc)").is_err());
        assert!(Regex::new("[abc").is_err());
        assert!(Regex::new("*a").is_err());
        assert!(Regex::new("a{3,1}").is_err());
        assert!(Regex::new("\\k<nope>").is_err());
        let e = Regex::new("(abc").unwrap_err();
        assert_eq!(e.kind, crate::error::kinds::EXPRESSION_ILLEGAL_SYNTAX);
        // deep nesting
        let deep = "(".repeat(1000) + &")".repeat(1000);
        assert!(Regex::new(&deep).is_err());
        // catastrophic pattern terminates (step budget)
        let re = Regex::new("(a+)+b").unwrap();
        let text = "a".repeat(40);
        assert!(!re.is_match(&text));
        // long input does not overflow the stack
        let re = Regex::new("(?:a|b)*c").unwrap();
        let text = "ab".repeat(50_000) + "c";
        assert!(re.is_full_match(&text));
    }
}
