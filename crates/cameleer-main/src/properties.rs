//! Java `.properties` file parser (application.properties).

use std::collections::BTreeMap;

/// Parse Java properties text: `key=value`, `key: value`, `key value`, comments with `#`/`!`,
/// line continuations with trailing `\`, and escapes (`\n`, `\t`, `\uXXXX`, `\=`, `\:`).
pub fn parse_properties(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    let mut logical = String::new();
    let mut lines = text.lines().peekable();
    while let Some(raw) = lines.next() {
        let line = if logical.is_empty() { raw.trim_start() } else { raw.trim_start() };
        if logical.is_empty() && (line.is_empty() || line.starts_with('#') || line.starts_with('!')) {
            continue;
        }
        // count trailing backslashes
        let trailing = line.chars().rev().take_while(|c| *c == '\\').count();
        if trailing % 2 == 1 {
            logical.push_str(&line[..line.len() - 1]);
            if lines.peek().is_some() {
                continue;
            }
        } else {
            logical.push_str(line);
        }
        let entry = std::mem::take(&mut logical);
        let (k, v) = split_entry(&entry);
        map.insert(unescape(&k), unescape(&v));
    }
    map
}

fn split_entry(s: &str) -> (String, String) {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut key = String::new();
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && i + 1 < chars.len() {
            key.push(c);
            key.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '=' || c == ':' || c.is_whitespace() {
            break;
        }
        key.push(c);
        i += 1;
    }
    // skip whitespace, one separator, whitespace
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    if i < chars.len() && (chars[i] == '=' || chars[i] == ':') {
        i += 1;
    }
    while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
    }
    (key, chars[i..].iter().collect())
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('f') => out.push('\u{0c}'),
            Some('u') => {
                let hex: String = (0..4).filter_map(|_| it.next()).collect();
                if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let p = parse_properties(
            "# comment\n! also\nname=Camel\nkey2 : value two\nkey3 value3\nmulti=a,\\\n   b\nesc=a\\=b\\nc\nuni=\\u00e9\nempty=\n",
        );
        assert_eq!(p["name"], "Camel");
        assert_eq!(p["key2"], "value two");
        assert_eq!(p["key3"], "value3");
        assert_eq!(p["multi"], "a,b");
        assert_eq!(p["esc"], "a=b\nc");
        assert_eq!(p["uni"], "é");
        assert_eq!(p["empty"], "");
    }
}
