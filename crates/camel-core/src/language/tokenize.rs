//! The `tokenize` language: splits the body (or a header) into tokens.
//!
//! Options: `token` (default `,`), `endToken` (pair mode: text between `token` and
//! `endToken`), `includeTokens` (pair/xml mode), `regex` (token is a regular expression),
//! `xml` (token is an element name like `<order>` and whole elements are extracted),
//! `group` (join every N tokens with `groupDelimiter`, default the token), `skipFirst`,
//! `headerName`/`source` (input). `inheritNamespaceTagName` is accepted and ignored.
//! The expression text, if not empty, is the name of the header to tokenize.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::error::{CamelError, Result};
use crate::message::Exchange;
use crate::regex::Regex;
use crate::spi::{Expression, Language};
use crate::value::Value;

/// Tokenizer configuration.
#[derive(Clone, Debug)]
pub struct Tokenizer {
    pub token: String,
    pub end_token: Option<String>,
    pub include_tokens: bool,
    pub regex: Option<Regex>,
    pub xml: bool,
    pub group: usize,
    pub group_delimiter: Option<String>,
    pub skip_first: bool,
}

fn opt_bool(o: &BTreeMap<String, String>, k: &str) -> bool {
    o.get(k).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n").replace("\\r", "\r").replace("\\t", "\t")
}

impl Tokenizer {
    /// Build from language options.
    pub fn from_options(o: &BTreeMap<String, String>) -> Result<Tokenizer> {
        let token = o.get("token").map(|t| unescape(t)).unwrap_or_else(|| ",".to_string());
        let is_regex = opt_bool(o, "regex");
        let xml = opt_bool(o, "xml");
        let group = match o.get("group").map(|g| g.trim()).filter(|g| !g.is_empty()) {
            Some(g) => g.parse::<usize>().map_err(|_| CamelError::illegal_argument(format!("tokenize group must be a number: {}", g)))?,
            None => 0,
        };
        if token.is_empty() {
            return Err(CamelError::illegal_argument("tokenize token must not be empty"));
        }
        Ok(Tokenizer {
            regex: if is_regex && !xml { Some(Regex::new(&token)?) } else { None },
            token,
            end_token: o.get("endToken").filter(|t| !t.is_empty()).map(|t| unescape(t)),
            include_tokens: opt_bool(o, "includeTokens"),
            xml,
            group,
            group_delimiter: o.get("groupDelimiter").map(|t| unescape(t)),
            skip_first: opt_bool(o, "skipFirst"),
        })
    }

    /// Tokenize a string.
    pub fn tokenize(&self, text: &str) -> Vec<String> {
        let mut tokens: Vec<String> = if self.xml {
            xml_tokens(text, &self.token)
        } else if let Some(end) = &self.end_token {
            pair_tokens(text, &self.token, end, self.include_tokens)
        } else if let Some(re) = &self.regex {
            let mut parts = re.split_limit(text, -1);
            if parts.last().map(|p| p.is_empty()).unwrap_or(false) {
                parts.pop();
            }
            parts
        } else {
            if text.is_empty() {
                vec![]
            } else {
                let mut parts: Vec<String> = text.split(self.token.as_str()).map(|s| s.to_string()).collect();
                if parts.last().map(|p| p.is_empty()).unwrap_or(false) {
                    parts.pop();
                }
                parts
            }
        };
        if self.skip_first && !tokens.is_empty() {
            tokens.remove(0);
        }
        if self.group > 0 {
            let delim = self.group_delimiter.clone().unwrap_or_else(|| if self.xml || self.end_token.is_some() { String::new() } else { self.token.clone() });
            tokens = tokens.chunks(self.group).map(|c| c.join(&delim)).collect();
        }
        tokens
    }

    /// Tokenize a value: arrays pass through, null gives an empty array.
    pub fn tokenize_value(&self, v: &Value) -> Value {
        match v {
            Value::Null => Value::Array(vec![]),
            Value::Array(a) => {
                let mut items = a.clone();
                if self.skip_first && !items.is_empty() {
                    items.remove(0);
                }
                Value::Array(items)
            }
            other => Value::from(self.tokenize(&other.to_string())),
        }
    }
}

fn pair_tokens(text: &str, start: &str, end: &str, include: bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(s) = rest.find(start) {
        let after = &rest[s + start.len()..];
        match after.find(end) {
            Some(e) => {
                if include {
                    out.push(format!("{}{}{}", start, &after[..e], end));
                } else {
                    out.push(after[..e].to_string());
                }
                rest = &after[e + end.len()..];
            }
            None => break,
        }
    }
    out
}

fn xml_tokens(text: &str, tag: &str) -> Vec<String> {
    let name = tag.trim().trim_start_matches('<').trim_end_matches('>').trim_end_matches('/').trim();
    let name = name.split_whitespace().next().unwrap_or(name).to_string();
    let open = format!("<{}", name);
    let close = format!("</{}>", name);
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find(&open) {
        let s = i + off;
        let after = &text[s + open.len()..];
        // must be followed by whitespace, '>' or '/'
        if !after.starts_with(|c: char| c.is_whitespace() || c == '>' || c == '/') {
            i = s + open.len();
            continue;
        }
        let gt = match after.find('>') {
            Some(g) => g,
            None => break,
        };
        if after[..gt].ends_with('/') {
            out.push(text[s..s + open.len() + gt + 1].to_string());
            i = s + open.len() + gt + 1;
            continue;
        }
        // find matching close considering nesting of the same tag
        let mut depth = 1;
        let mut j = s + open.len() + gt + 1;
        let mut end = None;
        while j < text.len() {
            let next_open = text[j..].find(&open).map(|k| j + k);
            let next_close = text[j..].find(&close).map(|k| j + k);
            match (next_open, next_close) {
                (Some(o), Some(c)) if o < c => {
                    let a = &text[o + open.len()..];
                    if a.starts_with(|ch: char| ch.is_whitespace() || ch == '>') {
                        depth += 1;
                    }
                    j = o + open.len();
                }
                (_, Some(c)) => {
                    depth -= 1;
                    j = c + close.len();
                    if depth == 0 {
                        end = Some(j);
                        break;
                    }
                }
                _ => break,
            }
        }
        match end {
            Some(e) => {
                out.push(text[s..e].to_string());
                i = e;
            }
            None => break,
        }
    }
    out
}

struct TokenizeExpression {
    header: Option<String>,
    tokenizer: Tokenizer,
    options: BTreeMap<String, String>,
}

impl Expression for TokenizeExpression {
    fn evaluate(&self, ex: &Exchange) -> Result<Value> {
        let input = match &self.header {
            Some(h) => ex.message.headers.get(h).cloned().unwrap_or(Value::Null),
            None => super::jsonpath::source_value(ex, &self.options),
        };
        Ok(self.tokenizer.tokenize_value(&input))
    }
}

/// The `tokenize` language.
#[derive(Default)]
pub struct TokenizeLanguage;

impl Language for TokenizeLanguage {
    fn name(&self) -> &str {
        "tokenize"
    }
    fn create_expression(&self, text: &str, options: &BTreeMap<String, String>) -> Result<Arc<dyn Expression>> {
        let t = text.trim();
        Ok(Arc::new(TokenizeExpression {
            header: if t.is_empty() { None } else { Some(t.to_string()) },
            tokenizer: Tokenizer::from_options(options)?,
            options: options.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tk(opts: &[(&str, &str)], text: &str) -> Vec<String> {
        let o: BTreeMap<String, String> = opts.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Tokenizer::from_options(&o).unwrap().tokenize(text)
    }

    #[test]
    fn modes() {
        assert_eq!(tk(&[], "a,b,c"), vec!["a", "b", "c"]);
        assert_eq!(tk(&[("token", ";")], "a;b;"), vec!["a", "b"]);
        assert_eq!(tk(&[("token", "\\d+"), ("regex", "true")], "a1b22c"), vec!["a", "b", "c"]);
        assert_eq!(tk(&[("group", "2")], "a,b,c"), vec!["a,b", "c"]);
        assert_eq!(tk(&[("skipFirst", "true")], "h,a,b"), vec!["a", "b"]);
        assert_eq!(tk(&[("token", "["), ("endToken", "]")], "[a] x [b]"), vec!["a", "b"]);
        assert_eq!(tk(&[("token", "<o>"), ("xml", "true")], "<r><o id='1'>x</o><o/></r>"), vec!["<o id='1'>x</o>", "<o/>"]);
    }
}
