//! Camel-style SQL parameter placeholders.
//!
//! * `:#name` — named parameter (looked up in the body map, headers, exchange properties,
//!   variables),
//! * `:#${expression}` — a Simple expression,
//! * `:#in:name` — a collection expanded to `?, ?, ...` (for `IN (...)`),
//! * `?` and the endpoint `placeholder` (default `#`) — positional parameters taken from the
//!   message body (array, or a string split by `separator`),
//! * `:?name` — the jdbc component's named parameters (from headers / `CamelJdbcParameters`).

use std::collections::BTreeMap;

use cameleer_core::error::{CamelError, Result};
use cameleer_core::message::Exchange;
use cameleer_core::value::{ObjectMap, Value};

/// One placeholder.
#[derive(Clone, Debug, PartialEq)]
pub enum Param {
    Positional,
    Named(String),
    Expression(String),
    InList(String),
}

/// SQL with placeholders replaced by markers.
#[derive(Clone, Debug, PartialEq)]
pub struct ParsedSql {
    /// SQL pieces; a parameter goes between consecutive pieces (`pieces.len() == params.len() + 1`).
    pub pieces: Vec<String>,
    pub params: Vec<Param>,
}

impl ParsedSql {
    /// Number of positional parameters.
    pub fn positional_count(&self) -> usize {
        self.params.iter().filter(|p| **p == Param::Positional).count()
    }
    /// Whether any named / expression parameter is used.
    pub fn has_named(&self) -> bool {
        self.params.iter().any(|p| *p != Param::Positional)
    }
}

/// Parse SQL text. `placeholder` is an extra positional marker (`#`; empty = none);
/// `named_prefix` is `:#` (sql) or `:?` (jdbc). Quoted strings and comments are left alone.
pub fn parse_sql(sql: &str, placeholder: &str, named_prefix: &str, allow_named: bool) -> Result<ParsedSql> {
    let chars: Vec<char> = sql.chars().collect();
    let ph: Vec<char> = placeholder.chars().collect();
    let np: Vec<char> = named_prefix.chars().collect();
    let mut pieces = Vec::new();
    let mut params = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    let starts = |i: usize, pat: &[char]| !pat.is_empty() && i + pat.len() <= chars.len() && chars[i..i + pat.len()] == *pat;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' || c == '"' {
            cur.push(c);
            i += 1;
            while i < chars.len() {
                cur.push(chars[i]);
                if chars[i] == c {
                    if chars.get(i + 1) == Some(&c) {
                        cur.push(c);
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if c == '-' && chars.get(i + 1) == Some(&'-') {
            while i < chars.len() && chars[i] != '\n' {
                cur.push(chars[i]);
                i += 1;
            }
            continue;
        }
        if allow_named && starts(i, &np) {
            let mut j = i + np.len();
            if chars.get(j) == Some(&'$') && chars.get(j + 1) == Some(&'{') {
                // :#${expr} with nested braces
                let mut depth = 0;
                let start = j;
                while j < chars.len() {
                    if chars[j] == '{' {
                        depth += 1;
                    } else if chars[j] == '}' {
                        depth -= 1;
                        if depth == 0 {
                            j += 1;
                            break;
                        }
                    }
                    j += 1;
                }
                if depth != 0 {
                    return Err(CamelError::illegal_argument(format!("Unterminated expression parameter in SQL: {}", sql)));
                }
                pieces.push(std::mem::take(&mut cur));
                params.push(Param::Expression(chars[start..j].iter().collect()));
                i = j;
                continue;
            }
            let is_in = chars[j..].starts_with(&['i', 'n', ':']);
            if is_in {
                j += 3;
            }
            let start = j;
            while j < chars.len() && (chars[j].is_alphanumeric() || matches!(chars[j], '_' | '$' | '.' | '-') && j > start) {
                j += 1;
            }
            // a trailing '.' or '-' is punctuation, not part of the name
            while j > start && matches!(chars[j - 1], '.' | '-') {
                j -= 1;
            }
            if j > start {
                let name: String = chars[start..j].iter().collect();
                pieces.push(std::mem::take(&mut cur));
                params.push(if is_in { Param::InList(name) } else { Param::Named(name) });
                i = j;
                continue;
            }
        }
        if c == '?' || starts(i, &ph) {
            pieces.push(std::mem::take(&mut cur));
            params.push(Param::Positional);
            i += if c == '?' { 1 } else { ph.len() };
            continue;
        }
        cur.push(c);
        i += 1;
    }
    pieces.push(cur);
    Ok(ParsedSql { pieces, params })
}

/// Look up a named parameter: body map, headers, exchange properties, variables.
pub fn lookup_named(ex: &Exchange, name: &str, body_map: Option<&ObjectMap>) -> Option<Value> {
    if let Some(m) = body_map {
        if let Some(v) = m.get(name).or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v)) {
            return Some(v.clone());
        }
    }
    if let Some(v) = ex.header(name) {
        return Some(v.clone());
    }
    if let Some(v) = ex.property(name) {
        return Some(v.clone());
    }
    let var = name.strip_prefix("variable.").unwrap_or(name);
    ex.property(&format!("CamelVariable.{}", var)).cloned()
}

/// Positional values from a body: arrays give their elements, strings are split by
/// `separator`, null gives none, other values are one parameter.
pub fn positional_values(body: &Value, separator: char) -> Vec<Value> {
    match body {
        Value::Null => Vec::new(),
        Value::Array(a) => a.clone(),
        Value::String(s) => {
            if s.is_empty() {
                Vec::new()
            } else {
                s.split(separator).map(|p| Value::String(p.trim().to_string())).collect()
            }
        }
        Value::Object(m) => m.values().cloned().collect(),
        other => vec![other.clone()],
    }
}

/// Evaluate a Simple expression parameter.
fn eval_expression(ex: &Exchange, text: &str) -> Result<Value> {
    let ctx = ex.context.as_ref().ok_or_else(|| CamelError::illegal_state(format!("Cannot evaluate {} without a CamelContext", text)))?;
    let lang = ctx.resolve_language("simple")?;
    lang.create_expression(text, &BTreeMap::new())?.evaluate(ex)
}

/// Bind parameters: returns the SQL with `?` markers and the values.
///
/// `body_map` is used for named lookups; `positional` supplies positional values;
/// `expected` (if > 0) overrides the number of positional values required.
pub fn bind(parsed: &ParsedSql, ex: &Exchange, body_map: Option<&ObjectMap>, positional: Vec<Value>, expected: usize) -> Result<(String, Vec<Value>)> {
    let needed = if expected > 0 { expected } else { parsed.positional_count() };
    if parsed.positional_count() > 0 && positional.len() != needed && !(positional.len() > needed && parsed.has_named()) {
        return Err(CamelError::new(
            crate::datasource::kinds::SQL_EXCEPTION,
            format!("Number of parameters mismatch or not enough values. Expected: {}, was: {}", needed, positional.len()),
        ));
    }
    let mut pos = positional.into_iter();
    let mut sql = String::new();
    let mut values = Vec::new();
    for (k, piece) in parsed.pieces.iter().enumerate() {
        sql.push_str(piece);
        let Some(p) = parsed.params.get(k) else { break };
        match p {
            Param::Positional => {
                sql.push('?');
                values.push(pos.next().unwrap_or(Value::Null));
            }
            Param::Named(n) => {
                sql.push('?');
                values.push(lookup_named(ex, n, body_map).unwrap_or(Value::Null));
            }
            Param::Expression(e) => {
                sql.push('?');
                values.push(eval_expression(ex, e)?);
            }
            Param::InList(n) => {
                let items = match lookup_named(ex, n, body_map) {
                    Some(Value::Array(a)) => a,
                    Some(Value::String(s)) => s.split(',').map(|x| Value::String(x.trim().to_string())).collect(),
                    Some(Value::Null) | None => Vec::new(),
                    Some(v) => vec![v],
                };
                if items.is_empty() {
                    sql.push_str("NULL");
                } else {
                    sql.push_str(&vec!["?"; items.len()].join(", "));
                    values.extend(items);
                }
            }
        }
    }
    Ok((sql, values))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_placeholders() {
        let p = parse_sql("select * from t where a = :#a and b = # and c = ? and d = ':#no' and e in (:#in:ids) and f = :#${header.x}", "#", ":#", true).unwrap();
        assert_eq!(p.params, vec![Param::Named("a".into()), Param::Positional, Param::Positional, Param::InList("ids".into()), Param::Expression("${header.x}".into())]);
        assert_eq!(p.pieces[3], " and d = ':#no' and e in (");
        let p = parse_sql("update t set x = :#x.", "#", ":#", true).unwrap();
        assert_eq!(p.params, vec![Param::Named("x".into())]);
        let p = parse_sql("select :?id, ? from t", "", ":?", true).unwrap();
        assert_eq!(p.params, vec![Param::Named("id".into()), Param::Positional]);
    }

    #[test]
    fn binding() {
        let mut ex = Exchange::new(None);
        ex.set_header("a", 1);
        ex.set_header("ids", Value::Array(vec![Value::Int(1), Value::Int(2)]));
        let p = parse_sql("select * from t where a = :#a and b = # and id in (:#in:ids)", "#", ":#", true).unwrap();
        let (sql, vals) = bind(&p, &ex, None, vec![Value::from("x")], 0).unwrap();
        assert_eq!(sql, "select * from t where a = ? and b = ? and id in (?, ?)");
        assert_eq!(vals, vec![Value::Int(1), Value::from("x"), Value::Int(1), Value::Int(2)]);
        assert!(bind(&p, &ex, None, vec![], 0).is_err());
        assert_eq!(positional_values(&Value::from("a, b"), ','), vec![Value::from("a"), Value::from("b")]);
    }
}
