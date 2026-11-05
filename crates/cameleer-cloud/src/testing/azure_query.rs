//! Query acceleration (`POST ...?comp=query`, Query Blob Contents) for [`super::AzuriteMock`]:
//! a SQL subset (`SELECT * | col, ... | COUNT(*) FROM BlobStorage [WHERE col op literal [AND
//! ...]] [LIMIT n]`, columns `_1`, `_2`... or header names, JSON fields by name) over delimited
//! text or line-delimited JSON, answered as the Avro stream of the service (`resultData`,
//! `progress`, `error` and `end` records).

use cameleer_core::value::Value;
use cameleer_dataformats::avro::{write_container, AvroSchema, Codec};

use crate::http::Response;
use crate::util::{obj, parse_json, Xml};

const SCHEMA: &str = r#"[
 {"type":"record","name":"resultData","namespace":"com.microsoft.azure.storage.queryBlobContents","fields":[{"name":"data","type":"bytes"}]},
 {"type":"record","name":"error","namespace":"com.microsoft.azure.storage.queryBlobContents","fields":[{"name":"fatal","type":"boolean"},{"name":"name","type":"string"},{"name":"description","type":"string"},{"name":"position","type":"long"}]},
 {"type":"record","name":"progress","namespace":"com.microsoft.azure.storage.queryBlobContents","fields":[{"name":"bytesScanned","type":"long"},{"name":"totalBytes","type":"long"}]},
 {"type":"record","name":"end","namespace":"com.microsoft.azure.storage.queryBlobContents","fields":[{"name":"totalBytes","type":"long"}]}
]"#;

#[derive(Debug, Clone)]
enum Lit {
    Num(f64),
    Str(String),
}

struct Query {
    columns: Option<Vec<String>>,
    count: bool,
    conds: Vec<(String, String, Lit)>,
    limit: Option<usize>,
}

fn tokens(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                let mut s = String::from("'");
                for d in chars.by_ref() {
                    if d == '\'' {
                        break;
                    }
                    s.push(d);
                }
                out.push(s);
            }
            ',' | '(' | ')' | '*' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            '<' | '>' | '=' | '!' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                let mut op = c.to_string();
                if let Some(&n) = chars.peek() {
                    if n == '=' || (c == '<' && n == '>') {
                        op.push(n);
                        chars.next();
                    }
                }
                out.push(op);
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse(sql: &str) -> Result<Query, String> {
    let t = tokens(sql);
    let up = |i: usize| t.get(i).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
    if up(0) != "SELECT" {
        return Err("Expected SELECT".into());
    }
    let mut i = 1;
    let mut columns = Vec::new();
    let mut count = false;
    let mut star = false;
    while i < t.len() && up(i) != "FROM" {
        match t[i].as_str() {
            "," => {}
            "*" => star = true,
            _ if up(i) == "COUNT" => {
                count = true;
                i += 3; // ( * )
            }
            c => columns.push(c.to_string()),
        }
        i += 1;
    }
    if up(i) != "FROM" || !t.get(i + 1).is_some_and(|s| s.eq_ignore_ascii_case("BlobStorage")) {
        return Err("Expected FROM BlobStorage".into());
    }
    i += 2;
    let mut conds = Vec::new();
    let mut limit = None;
    if up(i) == "WHERE" {
        i += 1;
        loop {
            let (Some(col), Some(op), Some(lit)) = (t.get(i), t.get(i + 1), t.get(i + 2)) else { return Err("Incomplete WHERE clause".into()) };
            if !matches!(op.as_str(), "=" | "!=" | "<>" | "<" | ">" | "<=" | ">=") {
                return Err(format!("Unsupported operator {}", op));
            }
            let lit = match lit.strip_prefix('\'') {
                Some(s) => Lit::Str(s.to_string()),
                None => Lit::Num(lit.parse().map_err(|_| format!("Invalid literal {}", lit))?),
            };
            conds.push((col.clone(), op.clone(), lit));
            i += 3;
            if up(i) == "AND" {
                i += 1;
            } else {
                break;
            }
        }
    }
    if up(i) == "LIMIT" {
        limit = Some(t.get(i + 1).and_then(|n| n.parse().ok()).ok_or("Invalid LIMIT")?);
        i += 2;
    }
    if i < t.len() {
        return Err(format!("Unexpected token {}", t[i]));
    }
    Ok(Query { columns: if star || count { None } else { Some(columns) }, count, conds, limit })
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn compare(v: &str, op: &str, lit: &Lit) -> bool {
    let ord = match lit {
        Lit::Num(n) => match v.trim().parse::<f64>() {
            Ok(x) => x.partial_cmp(n),
            Err(_) => return false,
        },
        Lit::Str(s) => Some(v.cmp(s.as_str())),
    };
    let Some(o) = ord else { return false };
    match op {
        "=" => o.is_eq(),
        "!=" | "<>" => o.is_ne(),
        "<" => o.is_lt(),
        ">" => o.is_gt(),
        "<=" => o.is_le(),
        _ => o.is_ge(),
    }
}

fn run(data: &[u8], request: &str) -> Result<String, (String, String)> {
    let x = Xml::parse(request).map_err(|_| ("InvalidXmlDocument".to_string(), "Invalid query request".to_string()))?;
    let expr = x.find_text("Expression").unwrap_or_default();
    let json = x.find_text("Type").is_some_and(|t| t.eq_ignore_ascii_case("json"));
    let headers = x.find_text("HasHeaders").is_some_and(|h| h == "true");
    let q = parse(&expr).map_err(|e| ("ParseError".to_string(), e))?;
    let text = String::from_utf8_lossy(data).to_string();
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let names: Vec<String> = if headers && !json { lines.next().map(split_csv).unwrap_or_default() } else { Vec::new() };
    let mut out = String::new();
    let mut n = 0usize;
    for line in lines {
        let mut record = Value::Null;
        let (get, whole): (Box<dyn Fn(&str) -> Option<String>>, String) = if json {
            let v = parse_json(line.as_bytes()).map_err(|_| ("InvalidJsonRecord".to_string(), format!("Invalid JSON record: {}", line)))?;
            let v2 = v.clone();
            record = v.clone();
            (Box::new(move |c: &str| v2.get(c).map(|x| x.to_string())), v.to_json())
        } else {
            let fields = split_csv(line);
            let names = names.clone();
            let f2 = fields.clone();
            (
                Box::new(move |c: &str| match c.strip_prefix('_').and_then(|i| i.parse::<usize>().ok()) {
                    Some(i) if i >= 1 => f2.get(i - 1).cloned(),
                    _ => names.iter().position(|nm| nm == c).and_then(|i| f2.get(i).cloned()),
                }),
                line.to_string(),
            )
        };
        let mut keep = true;
        for (col, op, lit) in &q.conds {
            match get(col) {
                Some(v) => keep &= compare(&v, op, lit),
                None if json => keep = false,
                None => return Err(("InvalidColumnOrdinal".to_string(), format!("Unknown column {}", col))),
            }
        }
        if !keep {
            continue;
        }
        n += 1;
        if !q.count {
            match &q.columns {
                None => out.push_str(&whole),
                Some(cols) if json => {
                    let mut m = cameleer_core::value::ObjectMap::new();
                    for c in cols {
                        m.insert(c.clone(), record.get(c).cloned().unwrap_or(Value::Null));
                    }
                    out.push_str(&Value::Object(m).to_json());
                }
                Some(cols) => {
                    let mut vals = Vec::new();
                    for c in cols {
                        vals.push(get(c).ok_or_else(|| ("InvalidColumnOrdinal".to_string(), format!("Unknown column {}", c)))?);
                    }
                    out.push_str(&vals.join(","));
                }
            }
            out.push('\n');
        }
        if q.limit.is_some_and(|l| n >= l) {
            break;
        }
    }
    if q.count {
        out = format!("{}\n", n);
    }
    Ok(out)
}

/// Answer a Query Blob Contents request over `data`.
pub(super) fn query_blob(data: &[u8], request: &str) -> Response {
    let schema = AvroSchema::parse(SCHEMA).expect("query schema");
    let total = data.len() as i64;
    let ns = "com.microsoft.azure.storage.queryBlobContents";
    let mut records = Vec::new();
    match run(data, request) {
        Ok(text) => {
            records.push(obj([(&format!("{}.resultData", ns), obj([("data", Value::Bytes(text.into_bytes()))]))]));
        }
        Err((name, desc)) => {
            if name == "InvalidXmlDocument" {
                return super::error(400, &name, &desc);
            }
            records.push(obj([(&format!("{}.error", ns), obj([("fatal", Value::Bool(true)), ("name", Value::from(name)), ("description", Value::from(desc)), ("position", Value::Int(0))]))]));
        }
    }
    records.push(obj([(&format!("{}.progress", ns), obj([("bytesScanned", Value::Int(total)), ("totalBytes", Value::Int(total))]))]));
    records.push(obj([(&format!("{}.end", ns), obj([("totalBytes", Value::Int(total))]))]));
    match write_container(&schema, &records, Codec::Null, 16_000) {
        Ok(bytes) => Response::new(200).with_header("Content-Type", "avro/binary").with_body(bytes),
        Err(e) => super::error(500, "InternalError", &e.message),
    }
}
