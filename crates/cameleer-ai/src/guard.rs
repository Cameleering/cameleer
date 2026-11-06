//! Result guardrails: redaction of personal data and secrets in tool results (`redact`
//! option of `ai-tool`), before the result reaches a model, an MCP client or the audit log.

use cameleer_core::error::{CamelError, Result};
use cameleer_core::regex::Regex;
use cameleer_core::value::Value;

/// One kind of data to redact.
pub struct Redaction {
    pub name: String,
    regex: Regex,
    luhn: bool,
}

/// Applied in this order: the specific kinds before the broad phone pattern.
const BUILTIN: &[(&str, &str)] = &[
    (
        "secret",
        r"sk-[A-Za-z0-9_\-]{16,}|sk-ant-[A-Za-z0-9_\-]{16,}|AKIA[0-9A-Z]{16}|gh[pousr]_[A-Za-z0-9]{36,}|xox[baprs]-[A-Za-z0-9\-]{10,}|eyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}|-----BEGIN [A-Z ]*PRIVATE KEY-----",
    ),
    ("email", r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
    ("creditCard", r"\b\d(?:[ -]?\d){12,18}\b"),
    ("iban", r"\b[A-Z]{2}\d{2}[A-Z0-9]{11,30}\b"),
    ("ssn", r"\b\d{3}-\d{2}-\d{4}\b"),
    ("ipv4", r"\b(?:\d{1,3}\.){3}\d{1,3}\b"),
    ("phone", r"\+?\d[\d \-().]{7,}\d"),
];

/// Parse the `redact` option: built-in names (`email`, `phone`, `creditCard`, `iban`, `ssn`,
/// `ipv4`, `secret`, `all`) and `regex:NAME=PATTERN` entries, comma separated.
pub fn parse(option: &str) -> Result<Vec<Redaction>> {
    let mut out = Vec::new();
    for item in option.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(rest) = item.strip_prefix("regex:") {
            let (name, pattern) = rest.split_once('=').unwrap_or(("custom", rest));
            let regex = Regex::new(pattern).map_err(|e| CamelError::illegal_argument(format!("redact: invalid pattern {}: {}", pattern, e.message)))?;
            out.push(Redaction { name: name.to_string(), regex, luhn: false });
            continue;
        }
        let names: Vec<&str> = if item == "all" { BUILTIN.iter().map(|(n, _)| *n).collect() } else { vec![item] };
        for n in names {
            let (name, pattern) = BUILTIN.iter().find(|(b, _)| b.eq_ignore_ascii_case(n)).ok_or_else(|| {
                CamelError::illegal_argument(format!("redact: unknown kind {} (email, phone, creditCard, iban, ssn, ipv4, secret, all, regex:NAME=PATTERN)", n))
            })?;
            let regex = Regex::new(pattern).map_err(|e| CamelError::illegal_state(format!("redact pattern {}: {}", name, e.message)))?;
            out.push(Redaction { name: name.to_string(), regex, luhn: *name == "creditCard" });
        }
    }
    // custom patterns first, then the built-in kinds in their fixed order
    out.sort_by_key(|r| BUILTIN.iter().position(|(n, _)| *n == r.name).map(|i| i + 1).unwrap_or(0));
    Ok(out)
}

fn luhn_ok(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if digits.len() < 13 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, d)| if i % 2 == 1 { if d * 2 > 9 { d * 2 - 9 } else { d * 2 } } else { *d })
        .sum();
    sum % 10 == 0
}

/// Redact a text; returns the text and how many matches were replaced.
pub fn redact_text(rules: &[Redaction], text: &str) -> (String, usize) {
    let mut out = text.to_string();
    let mut count = 0;
    for r in rules {
        let mut result = String::with_capacity(out.len());
        let mut last = 0;
        for (start, end) in r.regex.find_iter(&out) {
            if r.luhn && !luhn_ok(&out[start..end]) {
                continue;
            }
            result.push_str(&out[last..start]);
            result.push_str(&format!("[REDACTED:{}]", r.name));
            last = end;
            count += 1;
        }
        result.push_str(&out[last..]);
        out = result;
    }
    (out, count)
}

/// Redact every string in a value.
pub fn redact_value(rules: &[Redaction], v: &Value) -> (Value, usize) {
    match v {
        Value::String(s) => {
            let (t, n) = redact_text(rules, s);
            (Value::String(t), n)
        }
        Value::Array(a) => {
            let mut n = 0;
            let items = a
                .iter()
                .map(|x| {
                    let (y, k) = redact_value(rules, x);
                    n += k;
                    y
                })
                .collect();
            (Value::Array(items), n)
        }
        Value::Object(m) => {
            let mut n = 0;
            let mut o = cameleer_core::value::ObjectMap::new();
            for (k, x) in m.iter() {
                let (y, c) = redact_value(rules, x);
                n += c;
                o.insert(k.clone(), y);
            }
            (Value::Object(o), n)
        }
        other => (other.clone(), 0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_the_builtin_kinds() {
        let rules = parse("all").unwrap();
        let (t, n) = redact_text(&rules, "Mail ann@example.com, card 4111 1111 1111 1111, key sk-abcdefghijklmnopqrstu, ip 10.0.0.1, ssn 123-45-6789");
        assert_eq!(t, "Mail [REDACTED:email], card [REDACTED:creditCard], key [REDACTED:secret], ip [REDACTED:ipv4], ssn [REDACTED:ssn]", "{}", t);
        assert_eq!(n, 5);
        // a number that fails the Luhn check is not a card
        let (t, _) = redact_text(&parse("creditCard").unwrap(), "order 1234 5678 9012 3456");
        assert_eq!(t, "order 1234 5678 9012 3456");
        let (t, _) = redact_text(&parse("regex:ticket=TCK-\\d+").unwrap(), "see TCK-42");
        assert_eq!(t, "see [REDACTED:ticket]");
        assert!(parse("passport").is_err());
    }
}
