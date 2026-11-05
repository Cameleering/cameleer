//! JSON parsing / serialization of runtime values (keeps object key order).

use crate::value::*;

/// Parse JSON text into a runtime value.
pub fn parse(text: &str) -> Result<V, String> {
    let mut p = P { s: text.as_bytes(), src: text, i: 0 };
    p.ws();
    let v = p.value(0)?;
    p.ws();
    if p.i < p.s.len() {
        return Err(format!("Unexpected token '{}' in JSON at position {}", p.src[p.i..].chars().next().unwrap_or(' '), p.i));
    }
    Ok(v)
}

struct P<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn err<T>(&self, what: &str) -> Result<T, String> {
        if self.i >= self.s.len() {
            return Err(format!("Unexpected end of JSON input (expected {})", what));
        }
        Err(format!("Unexpected token '{}' in JSON at position {} (expected {})", self.src[self.i..].chars().next().unwrap_or(' '), self.i, what))
    }
    fn value(&mut self, depth: usize) -> Result<V, String> {
        if depth > 500 {
            return Err("JSON nested too deeply".into());
        }
        self.ws();
        match self.s.get(self.i) {
            None => self.err("a value"),
            Some(b'{') => {
                self.i += 1;
                let mut pm = PropMap::default();
                self.ws();
                if self.s.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(V::obj(pm));
                }
                loop {
                    self.ws();
                    if self.s.get(self.i) != Some(&b'"') {
                        return self.err("a property name");
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.s.get(self.i) != Some(&b':') {
                        return self.err("':'");
                    }
                    self.i += 1;
                    let v = self.value(depth + 1)?;
                    pm.set(&k, v);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(V::obj(pm));
                        }
                        _ => return self.err("',' or '}'"),
                    }
                }
            }
            Some(b'[') => {
                self.i += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(V::arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.s.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(V::arr(items));
                        }
                        _ => return self.err("',' or ']'"),
                    }
                }
            }
            Some(b'"') => Ok(V::string(self.string()?)),
            Some(b't') if self.src[self.i..].starts_with("true") => {
                self.i += 4;
                Ok(V::Bool(true))
            }
            Some(b'f') if self.src[self.i..].starts_with("false") => {
                self.i += 5;
                Ok(V::Bool(false))
            }
            Some(b'n') if self.src[self.i..].starts_with("null") => {
                self.i += 4;
                Ok(V::Null)
            }
            Some(c) if *c == b'-' || c.is_ascii_digit() => {
                let st = self.i;
                self.i += 1;
                let mut float = false;
                while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || matches!(self.s[self.i], b'.' | b'e' | b'E' | b'+' | b'-')) {
                    if matches!(self.s[self.i], b'.' | b'e' | b'E') {
                        float = true;
                    }
                    self.i += 1;
                }
                let t = &self.src[st..self.i];
                if !float {
                    if let Ok(i) = t.parse::<i64>() {
                        return Ok(V::Int(i));
                    }
                }
                t.parse::<f64>().map(V::Num).map_err(|_| format!("Invalid number '{}' in JSON at position {}", t, st))
            }
            _ => self.err("a value"),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out = String::new();
        loop {
            let c = match self.s.get(self.i) {
                None => return Err("Unterminated string in JSON".into()),
                Some(c) => *c,
            };
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    let (s, ni) = crate::lexer::escape(self.src, self.i + 1)?;
                    out.push_str(&s);
                    self.i = ni;
                }
                _ => {
                    let ch = self.src[self.i..].chars().next().unwrap();
                    out.push(ch);
                    self.i += ch.len_utf8();
                }
            }
        }
    }
}

/// JSON serialization options.
pub struct JsonOpts {
    /// Indentation (None = compact).
    pub indent: Option<String>,
    /// Separator after `,` in compact mode (`""` for JS, `" "` for Python).
    pub item_sep: &'static str,
    /// Separator after `:` (`""` for JS, `" "` for Python).
    pub key_sep: &'static str,
    pub sort_keys: bool,
    /// JS semantics: skip undefined / functions in objects.
    pub js: bool,
    pub newline: &'static str,
}

impl JsonOpts {
    pub fn compact() -> JsonOpts {
        JsonOpts { indent: None, item_sep: "", key_sep: "", sort_keys: false, js: true, newline: "\n" }
    }
}

/// Quote a string as JSON.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    cameleer_core::value::json_escape(s, &mut out);
    out
}

/// Serialize a runtime value; returns `None` for values that have no JSON form (undefined, functions).
pub fn stringify(v: &V, o: &JsonOpts) -> Option<String> {
    let mut out = String::new();
    if write(v, o, 0, &mut out) {
        Some(out)
    } else {
        None
    }
}

fn num(f: f64) -> String {
    if !f.is_finite() {
        return "null".into();
    }
    crate::interp::js_num_str(f)
}

fn write(v: &V, o: &JsonOpts, level: usize, out: &mut String) -> bool {
    if level > 200 {
        out.push_str("null");
        return true;
    }
    match v {
        V::Undef | V::Func(_) | V::Method(_) => return false,
        V::Host(_) => out.push_str("null"),
        V::Null => out.push_str("null"),
        V::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        V::Int(i) => out.push_str(&i.to_string()),
        V::Num(f) => out.push_str(&num(*f)),
        V::Str(s) => out.push_str(&quote(s)),
        V::Bytes(b) => out.push_str(&quote(&String::from_utf8_lossy(b))),
        V::Regex(_) => out.push_str("{}"),
        V::Arr(a) => {
            let items = a.borrow();
            if items.is_empty() {
                out.push_str("[]");
                return true;
            }
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if o.indent.is_none() {
                        out.push_str(o.item_sep);
                    }
                }
                newline(o, level + 1, out);
                if !write(x, o, level + 1, out) {
                    out.push_str("null");
                }
            }
            newline(o, level, out);
            out.push(']');
        }
        V::Obj(ob) => {
            let ob = ob.borrow();
            match &ob.kind {
                ObjKind::Date(ms) => {
                    out.push_str(&quote(&crate::builtins::iso_date(*ms)));
                    return true;
                }
                ObjKind::Set => {
                    let items: Vec<V> = ob.props.values().cloned().collect();
                    return write(&V::arr(items), o, level, out);
                }
                _ => {}
            }
            let mut keys: Vec<&std::rc::Rc<str>> = ob.props.keys().collect();
            if o.sort_keys {
                keys.sort();
            }
            let mut first = true;
            out.push('{');
            for k in keys {
                let x = ob.props.get(k).unwrap();
                if o.js && matches!(x, V::Undef | V::Func(_) | V::Method(_)) {
                    continue;
                }
                if !first {
                    out.push(',');
                    if o.indent.is_none() {
                        out.push_str(o.item_sep);
                    }
                }
                first = false;
                newline(o, level + 1, out);
                out.push_str(&quote(k));
                out.push(':');
                if o.indent.is_some() {
                    out.push(' ');
                } else {
                    out.push_str(o.key_sep);
                }
                if !write(x, o, level + 1, out) {
                    out.push_str("null");
                }
            }
            if !first {
                newline(o, level, out);
            }
            out.push('}');
        }
    }
    true
}

fn newline(o: &JsonOpts, level: usize, out: &mut String) {
    if let Some(ind) = &o.indent {
        out.push_str(o.newline);
        for _ in 0..level {
            out.push_str(ind);
        }
    }
}
