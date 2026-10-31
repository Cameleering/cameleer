//! `.proto` file parser (proto3 and the common proto2 subset).
//!
//! Supported: `syntax`, `edition` (treated like proto2 presence rules), `package`,
//! `import` (incl. `public`/`weak`), `option` (ignored), messages (nested messages and
//! enums, `optional`/`required`/`repeated` labels, `map<K,V>`, `oneof`, `reserved`,
//! `extensions`, field options `packed`, `default`, `json_name`; other options ignored),
//! enums, services with unary / streaming rpcs, and top-level/nested `extend` blocks
//! (skipped). proto2 `group` fields are rejected.

use super::{EnumDescriptor, FieldDescriptor, FieldType, Label, MessageDescriptor, MethodDescriptor, ProtoError, ServiceDescriptor, Syntax};

/// A parsed but not yet resolved message.
#[derive(Clone, Debug)]
pub struct ParsedMessage {
    pub desc: MessageDescriptor,
    /// Resolution scope (the message's full name).
    pub scope: String,
    /// Per field: the unresolved type reference (None for scalars) and the line.
    pub raw_types: Vec<(Option<String>, usize)>,
}

/// A parsed service (types unresolved).
#[derive(Clone, Debug)]
pub struct ParsedService {
    pub desc: ServiceDescriptor,
    pub scope: String,
    pub line: usize,
}

/// A parsed file.
#[derive(Clone, Debug)]
pub struct ParsedFile {
    pub name: String,
    pub syntax: Syntax,
    pub package: String,
    pub imports: Vec<String>,
    /// All messages, nested ones flattened (plus synthesized map entries).
    pub messages: Vec<ParsedMessage>,
    pub enums: Vec<EnumDescriptor>,
    pub services: Vec<ParsedService>,
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Ident(String),
    Int(String),
    Float(String),
    Str(String),
    Sym(char),
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: usize,
}

impl<'a> Lexer<'a> {
    fn tokens(src: &'a str, file: &str) -> Result<Vec<(Tok, usize)>, ProtoError> {
        let mut lx = Lexer { src: src.as_bytes(), pos: 0, line: 1 };
        let mut out = Vec::new();
        loop {
            lx.skip_ws(file)?;
            if lx.pos >= lx.src.len() {
                return Ok(out);
            }
            let line = lx.line;
            let t = lx.next(file)?;
            out.push((t, line));
        }
    }

    fn err(&self, file: &str, msg: impl Into<String>) -> ProtoError {
        ProtoError { file: file.to_string(), line: self.line, message: msg.into() }
    }

    fn skip_ws(&mut self, file: &str) -> Result<(), ProtoError> {
        while self.pos < self.src.len() {
            let c = self.src[self.pos];
            if c == b'\n' {
                self.line += 1;
                self.pos += 1;
            } else if c.is_ascii_whitespace() {
                self.pos += 1;
            } else if self.src[self.pos..].starts_with(b"//") {
                while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
                    self.pos += 1;
                }
            } else if self.src[self.pos..].starts_with(b"/*") {
                self.pos += 2;
                loop {
                    if self.pos + 1 >= self.src.len() {
                        return Err(self.err(file, "unterminated comment"));
                    }
                    if self.src[self.pos] == b'\n' {
                        self.line += 1;
                    }
                    if self.src[self.pos..].starts_with(b"*/") {
                        self.pos += 2;
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    fn next(&mut self, file: &str) -> Result<Tok, ProtoError> {
        let c = self.src[self.pos];
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = self.pos;
            while self.pos < self.src.len() && (self.src[self.pos].is_ascii_alphanumeric() || self.src[self.pos] == b'_') {
                self.pos += 1;
            }
            return Ok(Tok::Ident(String::from_utf8_lossy(&self.src[start..self.pos]).into_owned()));
        }
        if c.is_ascii_digit() || (c == b'.' && self.src.get(self.pos + 1).is_some_and(|d| d.is_ascii_digit())) {
            let start = self.pos;
            let mut float = false;
            if self.src[self.pos..].starts_with(b"0x") || self.src[self.pos..].starts_with(b"0X") {
                self.pos += 2;
                while self.pos < self.src.len() && self.src[self.pos].is_ascii_hexdigit() {
                    self.pos += 1;
                }
            } else {
                while self.pos < self.src.len() {
                    let d = self.src[self.pos];
                    if d.is_ascii_digit() {
                        self.pos += 1;
                    } else if d == b'.' {
                        float = true;
                        self.pos += 1;
                    } else if d == b'e' || d == b'E' {
                        float = true;
                        self.pos += 1;
                        if self.pos < self.src.len() && (self.src[self.pos] == b'+' || self.src[self.pos] == b'-') {
                            self.pos += 1;
                        }
                    } else {
                        break;
                    }
                }
            }
            let text = String::from_utf8_lossy(&self.src[start..self.pos]).into_owned();
            return Ok(if float { Tok::Float(text) } else { Tok::Int(text) });
        }
        if c == b'"' || c == b'\'' {
            self.pos += 1;
            let mut s = Vec::new();
            loop {
                let Some(&b) = self.src.get(self.pos) else {
                    return Err(self.err(file, "unterminated string"));
                };
                self.pos += 1;
                if b == c {
                    break;
                }
                if b == b'\n' {
                    return Err(self.err(file, "newline in string"));
                }
                if b == b'\\' {
                    let e = *self.src.get(self.pos).ok_or_else(|| self.err(file, "bad escape"))?;
                    self.pos += 1;
                    match e {
                        b'n' => s.push(b'\n'),
                        b't' => s.push(b'\t'),
                        b'r' => s.push(b'\r'),
                        b'0'..=b'7' => {
                            let mut v = (e - b'0') as u32;
                            for _ in 0..2 {
                                match self.src.get(self.pos) {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u32;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            s.push(v as u8);
                        }
                        b'x' | b'X' => {
                            let mut v = 0u32;
                            for _ in 0..2 {
                                match self.src.get(self.pos) {
                                    Some(d) if d.is_ascii_hexdigit() => {
                                        v = v * 16 + (*d as char).to_digit(16).unwrap();
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            s.push(v as u8);
                        }
                        other => s.push(other),
                    }
                } else {
                    s.push(b);
                }
            }
            return Ok(Tok::Str(String::from_utf8_lossy(&s).into_owned()));
        }
        self.pos += 1;
        if b"{}[]()<>;=,.:-+/".contains(&c) {
            return Ok(Tok::Sym(c as char));
        }
        Err(self.err(file, format!("unexpected character '{}'", c as char)))
    }
}

struct Parser<'a> {
    file: &'a str,
    toks: Vec<(Tok, usize)>,
    pos: usize,
    syntax: Syntax,
    package: String,
    messages: Vec<ParsedMessage>,
    enums: Vec<EnumDescriptor>,
    services: Vec<ParsedService>,
    imports: Vec<String>,
}

type PResult<T> = Result<T, ProtoError>;

/// Parses a `.proto` file's text.
pub fn parse(file: &str, src: &str) -> Result<ParsedFile, ProtoError> {
    let toks = Lexer::tokens(src, file)?;
    let mut p = Parser {
        file,
        toks,
        pos: 0,
        syntax: Syntax::Proto2,
        package: String::new(),
        messages: Vec::new(),
        enums: Vec::new(),
        services: Vec::new(),
        imports: Vec::new(),
    };
    p.parse_file()?;
    Ok(ParsedFile { name: file.to_string(), syntax: p.syntax, package: p.package, imports: p.imports, messages: p.messages, enums: p.enums, services: p.services })
}

impl<'a> Parser<'a> {
    fn line(&self) -> usize {
        self.toks.get(self.pos).or(self.toks.last()).map(|t| t.1).unwrap_or(0)
    }
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(ProtoError { file: self.file.to_string(), line: self.line(), message: msg.into() })
    }
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.0)
    }
    fn next(&mut self) -> PResult<Tok> {
        match self.toks.get(self.pos) {
            Some((t, _)) => {
                self.pos += 1;
                Ok(t.clone())
            }
            None => self.err("unexpected end of file"),
        }
    }
    fn is_sym(&self, c: char) -> bool {
        self.peek() == Some(&Tok::Sym(c))
    }
    fn eat_sym(&mut self, c: char) -> bool {
        if self.is_sym(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_sym(&mut self, c: char) -> PResult<()> {
        if self.eat_sym(c) {
            Ok(())
        } else {
            let found = self.peek().cloned();
            self.err(format!("expected '{}' but found {:?}", c, found))
        }
    }
    fn is_ident(&self, s: &str) -> bool {
        matches!(self.peek(), Some(Tok::Ident(i)) if i == s)
    }
    fn eat_ident(&mut self, s: &str) -> bool {
        if self.is_ident(s) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn ident(&mut self) -> PResult<String> {
        match self.next()? {
            Tok::Ident(s) => Ok(s),
            other => {
                self.pos -= 1;
                self.err(format!("expected identifier but found {:?}", other))
            }
        }
    }
    /// A possibly dotted (and possibly fully-qualified) name.
    fn full_ident(&mut self) -> PResult<String> {
        let mut s = String::new();
        if self.eat_sym('.') {
            s.push('.');
        }
        s.push_str(&self.ident()?);
        while self.eat_sym('.') {
            s.push('.');
            s.push_str(&self.ident()?);
        }
        Ok(s)
    }
    fn string(&mut self) -> PResult<String> {
        let mut s = match self.next()? {
            Tok::Str(s) => s,
            other => {
                self.pos -= 1;
                return self.err(format!("expected string but found {:?}", other));
            }
        };
        // adjacent string literals concatenate
        while let Some(Tok::Str(more)) = self.peek().cloned() {
            self.pos += 1;
            s.push_str(&more);
        }
        Ok(s)
    }
    fn int(&mut self) -> PResult<i64> {
        let neg = self.eat_sym('-');
        let t = self.next()?;
        let Tok::Int(text) = t else {
            self.pos -= 1;
            return self.err(format!("expected integer but found {:?}", t));
        };
        let v = if let Some(h) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
            i64::from_str_radix(h, 16)
        } else if text.len() > 1 && text.starts_with('0') {
            i64::from_str_radix(&text[1..], 8)
        } else {
            text.parse::<i64>()
        };
        match v {
            Ok(v) => Ok(if neg { -v } else { v }),
            Err(_) => self.err(format!("invalid integer {}", text)),
        }
    }

    /// Skips a constant (scalar, identifier, or `{ ... }` aggregate) and returns its text.
    fn constant(&mut self) -> PResult<String> {
        if self.is_sym('{') {
            self.skip_braces()?;
            return Ok(String::new());
        }
        let mut prefix = String::new();
        if self.eat_sym('-') {
            prefix.push('-');
        } else {
            self.eat_sym('+');
        }
        Ok(match self.next()? {
            Tok::Ident(s) => {
                let mut s = prefix + &s;
                while self.eat_sym('.') {
                    s.push('.');
                    s.push_str(&self.ident()?);
                }
                s
            }
            Tok::Int(s) | Tok::Float(s) => prefix + &s,
            Tok::Str(s) => {
                let mut s = s;
                while let Some(Tok::Str(more)) = self.peek().cloned() {
                    self.pos += 1;
                    s.push_str(&more);
                }
                s
            }
            other => return self.err(format!("unexpected {:?} in constant", other)),
        })
    }

    fn skip_braces(&mut self) -> PResult<()> {
        self.expect_sym('{')?;
        let mut depth = 1;
        while depth > 0 {
            match self.next()? {
                Tok::Sym('{') => depth += 1,
                Tok::Sym('}') => depth -= 1,
                _ => {}
            }
        }
        Ok(())
    }

    /// `option name = constant ;` (the `option` keyword already consumed).
    fn option_stmt(&mut self) -> PResult<()> {
        self.option_name()?;
        self.expect_sym('=')?;
        self.constant()?;
        self.expect_sym(';')
    }

    fn option_name(&mut self) -> PResult<String> {
        let mut name = String::new();
        loop {
            if self.eat_sym('(') {
                name.push('(');
                name.push_str(&self.full_ident()?);
                self.expect_sym(')')?;
                name.push(')');
            } else {
                name.push_str(&self.ident()?);
            }
            if !self.eat_sym('.') {
                break;
            }
            name.push('.');
        }
        Ok(name)
    }

    /// `[ name = value, ... ]` returning the simple options.
    fn field_options(&mut self) -> PResult<Vec<(String, String)>> {
        let mut out = Vec::new();
        if !self.eat_sym('[') {
            return Ok(out);
        }
        loop {
            let name = self.option_name()?;
            self.expect_sym('=')?;
            let v = self.constant()?;
            out.push((name, v));
            if self.eat_sym(']') {
                break;
            }
            self.expect_sym(',')?;
        }
        Ok(out)
    }

    fn parse_file(&mut self) -> PResult<()> {
        while self.peek().is_some() {
            if self.eat_sym(';') {
                continue;
            }
            let kw = self.ident()?;
            match kw.as_str() {
                "syntax" => {
                    self.expect_sym('=')?;
                    let s = self.string()?;
                    self.syntax = match s.as_str() {
                        "proto3" => Syntax::Proto3,
                        "proto2" => Syntax::Proto2,
                        other => return self.err(format!("unsupported syntax \"{}\"", other)),
                    };
                    self.expect_sym(';')?;
                }
                "edition" => {
                    self.expect_sym('=')?;
                    self.string()?;
                    self.syntax = Syntax::Proto2;
                    self.expect_sym(';')?;
                }
                "package" => {
                    self.package = self.full_ident()?;
                    self.expect_sym(';')?;
                }
                "import" => {
                    let _ = self.eat_ident("public") || self.eat_ident("weak");
                    let s = self.string()?;
                    self.imports.push(s);
                    self.expect_sym(';')?;
                }
                "option" => self.option_stmt()?,
                "message" => {
                    let scope = self.package.clone();
                    self.message(&scope)?;
                }
                "enum" => {
                    let scope = self.package.clone();
                    self.enum_def(&scope)?;
                }
                "service" => self.service()?,
                "extend" => {
                    self.full_ident()?;
                    self.skip_braces()?;
                }
                other => return self.err(format!("unexpected \"{}\" at top level", other)),
            }
        }
        Ok(())
    }

    fn qualify(scope: &str, name: &str) -> String {
        if scope.is_empty() {
            name.to_string()
        } else {
            format!("{}.{}", scope, name)
        }
    }

    fn message(&mut self, scope: &str) -> PResult<()> {
        let name = self.ident()?;
        let full = Self::qualify(scope, &name);
        let mut msg = ParsedMessage {
            desc: MessageDescriptor { name, full_name: full.clone(), syntax: self.syntax, fields: Vec::new(), oneofs: Vec::new(), is_map_entry: false },
            scope: full.clone(),
            raw_types: Vec::new(),
        };
        self.expect_sym('{')?;
        self.message_body(&mut msg, None)?;
        self.messages.push(msg);
        Ok(())
    }

    fn message_body(&mut self, msg: &mut ParsedMessage, oneof: Option<usize>) -> PResult<()> {
        let full = msg.desc.full_name.clone();
        loop {
            if self.eat_sym('}') {
                return Ok(());
            }
            if self.eat_sym(';') {
                continue;
            }
            let line = self.line();
            match self.peek().cloned() {
                Some(Tok::Ident(kw)) => match kw.as_str() {
                    "message" if oneof.is_none() => {
                        self.pos += 1;
                        self.message(&full)?;
                    }
                    "enum" if oneof.is_none() => {
                        self.pos += 1;
                        self.enum_def(&full)?;
                    }
                    "option" => {
                        self.pos += 1;
                        self.option_stmt()?;
                    }
                    "oneof" if oneof.is_none() => {
                        self.pos += 1;
                        let oname = self.ident()?;
                        msg.desc.oneofs.push(oname);
                        let idx = msg.desc.oneofs.len() - 1;
                        self.expect_sym('{')?;
                        self.message_body(msg, Some(idx))?;
                    }
                    "reserved" | "extensions" if oneof.is_none() => {
                        self.pos += 1;
                        while !self.eat_sym(';') {
                            self.next()?;
                        }
                    }
                    "extend" if oneof.is_none() => {
                        self.pos += 1;
                        self.full_ident()?;
                        self.skip_braces()?;
                    }
                    "map" if matches!(self.toks.get(self.pos + 1), Some((Tok::Sym('<'), _))) => {
                        self.pos += 2;
                        self.map_field(msg, line)?;
                    }
                    _ => self.field(msg, oneof, line)?,
                },
                Some(Tok::Sym('.')) => self.field(msg, oneof, line)?,
                other => return self.err(format!("unexpected {:?} in message {}", other, full)),
            }
        }
    }

    fn field(&mut self, msg: &mut ParsedMessage, oneof: Option<usize>, line: usize) -> PResult<()> {
        let mut label = if self.syntax == Syntax::Proto3 { Label::Implicit } else { Label::Optional };
        if oneof.is_none() {
            if self.eat_ident("optional") {
                label = Label::Optional;
            } else if self.eat_ident("required") {
                label = Label::Required;
            } else if self.eat_ident("repeated") {
                label = Label::Repeated;
            }
        }
        if self.is_ident("group") {
            return self.err("proto2 groups are not supported");
        }
        let type_ref = self.full_ident()?;
        let name = self.ident()?;
        self.expect_sym('=')?;
        let number = self.int()?;
        if !(1..=536_870_911).contains(&number) {
            return self.err(format!("invalid field number {}", number));
        }
        let opts = self.field_options()?;
        self.expect_sym(';')?;
        let scalar = FieldType::scalar(&type_ref);
        let mut f = FieldDescriptor {
            json_name: super::to_json_name(&name),
            name,
            number: number as u32,
            label,
            field_type: scalar.unwrap_or(FieldType::Message),
            type_name: String::new(),
            oneof_index: oneof,
            packed: None,
            default_value: None,
            is_map: false,
        };
        for (k, v) in opts {
            match k.as_str() {
                "packed" => f.packed = Some(v == "true"),
                "default" => f.default_value = Some(v),
                "json_name" => f.json_name = v,
                _ => {}
            }
        }
        if f.packed.is_none() && f.is_repeated() && self.syntax == Syntax::Proto3 && scalar.map(|s| s.is_packable()).unwrap_or(false) {
            f.packed = Some(true);
        }
        msg.desc.fields.push(f);
        msg.raw_types.push((if scalar.is_some() { None } else { Some(type_ref) }, line));
        Ok(())
    }

    fn map_field(&mut self, msg: &mut ParsedMessage, line: usize) -> PResult<()> {
        let key = self.ident()?;
        let key_type = match FieldType::scalar(&key) {
            Some(t) if !matches!(t, FieldType::Double | FieldType::Float | FieldType::Bytes) => t,
            _ => return self.err(format!("invalid map key type {}", key)),
        };
        self.expect_sym(',')?;
        let value_ref = self.full_ident()?;
        self.expect_sym('>')?;
        let name = self.ident()?;
        self.expect_sym('=')?;
        let number = self.int()?;
        let opts = self.field_options()?;
        self.expect_sym(';')?;
        // synthesized entry message: <Name>Entry { key = 1; value = 2; }
        let entry_name = {
            let camel = super::to_json_name(&name);
            let mut c = camel.chars();
            format!("{}{}Entry", c.next().map(|f| f.to_ascii_uppercase().to_string()).unwrap_or_default(), c.as_str())
        };
        let entry_full = format!("{}.{}", msg.desc.full_name, entry_name);
        let value_scalar = FieldType::scalar(&value_ref);
        let mk = |n: &str, num: u32, t: FieldType| FieldDescriptor {
            name: n.to_string(),
            json_name: n.to_string(),
            number: num,
            label: Label::Optional,
            field_type: t,
            type_name: String::new(),
            oneof_index: None,
            packed: None,
            default_value: None,
            is_map: false,
        };
        self.messages.push(ParsedMessage {
            desc: MessageDescriptor {
                name: entry_name,
                full_name: entry_full.clone(),
                syntax: self.syntax,
                fields: vec![mk("key", 1, key_type), mk("value", 2, value_scalar.unwrap_or(FieldType::Message))],
                oneofs: Vec::new(),
                is_map_entry: true,
            },
            scope: msg.desc.full_name.clone(),
            raw_types: vec![(None, line), (if value_scalar.is_some() { None } else { Some(value_ref) }, line)],
        });
        let mut f = FieldDescriptor {
            json_name: super::to_json_name(&name),
            name,
            number: number as u32,
            label: Label::Repeated,
            field_type: FieldType::Message,
            type_name: entry_full.clone(),
            oneof_index: None,
            packed: None,
            default_value: None,
            is_map: true,
        };
        for (k, v) in opts {
            if k == "json_name" {
                f.json_name = v;
            }
        }
        msg.desc.fields.push(f);
        msg.raw_types.push((Some(format!(".{}", entry_full)), line));
        Ok(())
    }

    fn enum_def(&mut self, scope: &str) -> PResult<()> {
        let name = self.ident()?;
        let full = Self::qualify(scope, &name);
        let mut e = EnumDescriptor { name, full_name: full, values: Vec::new() };
        self.expect_sym('{')?;
        loop {
            if self.eat_sym('}') {
                break;
            }
            if self.eat_sym(';') {
                continue;
            }
            if self.eat_ident("option") {
                self.option_stmt()?;
                continue;
            }
            if self.eat_ident("reserved") {
                while !self.eat_sym(';') {
                    self.next()?;
                }
                continue;
            }
            let vname = self.ident()?;
            self.expect_sym('=')?;
            let n = self.int()?;
            self.field_options()?;
            self.expect_sym(';')?;
            e.values.push((vname, n as i32));
        }
        if e.values.is_empty() {
            return self.err(format!("enum {} has no values", e.full_name));
        }
        self.enums.push(e);
        Ok(())
    }

    fn service(&mut self) -> PResult<()> {
        let line = self.line();
        let name = self.ident()?;
        let full = Self::qualify(&self.package, &name);
        let mut svc = ServiceDescriptor { name, full_name: full, methods: Vec::new() };
        self.expect_sym('{')?;
        loop {
            if self.eat_sym('}') {
                break;
            }
            if self.eat_sym(';') {
                continue;
            }
            if self.eat_ident("option") {
                self.option_stmt()?;
                continue;
            }
            if !self.eat_ident("rpc") {
                let t = self.peek().cloned();
                return self.err(format!("expected rpc but found {:?}", t));
            }
            let mname = self.ident()?;
            self.expect_sym('(')?;
            let cs = self.is_ident("stream") && !matches!(self.toks.get(self.pos + 1), Some((Tok::Sym(')'), _)));
            if cs {
                self.pos += 1;
            }
            let input = self.full_ident()?;
            self.expect_sym(')')?;
            if !self.eat_ident("returns") {
                return self.err("expected 'returns'");
            }
            self.expect_sym('(')?;
            let ss = self.is_ident("stream") && !matches!(self.toks.get(self.pos + 1), Some((Tok::Sym(')'), _)));
            if ss {
                self.pos += 1;
            }
            let output = self.full_ident()?;
            self.expect_sym(')')?;
            if self.is_sym('{') {
                self.skip_braces()?;
                self.eat_sym(';');
            } else {
                self.expect_sym(';')?;
            }
            svc.methods.push(MethodDescriptor { name: mname, input_type: input, output_type: output, client_streaming: cs, server_streaming: ss });
        }
        self.services.push(ParsedService { desc: svc, scope: self.package.clone(), line });
        Ok(())
    }
}
