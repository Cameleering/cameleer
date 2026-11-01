//! jq builtins: native functions, `@formats`, regex functions, date functions and the
//! jq-defined prelude.

use std::cmp::Ordering;

use super::eval::*;
use super::parse::Ast;
use crate::value::{ObjectMap, Value};

/// Builtins defined in jq itself (as in jq 1.7's `builtin.jq`).
pub(crate) const PRELUDE: &str = r#"
def values: select(. != null);
def nulls: select(. == null);
def booleans: select(type == "boolean");
def numbers: select(type == "number");
def strings: select(type == "string");
def arrays: select(type == "array");
def objects: select(type == "object");
def iterables: select(type|. == "array" or . == "object");
def scalars: select(type|. != "array" and . != "object");
def select(f): if f then . else empty end;
def map(f): [.[] | f];
def map_values(f): .[] |= f;
def recurse(f; cond): def r: ., (f | select(cond) | r); r;
def recurse: recurse(.[]?);
def to_entries: [keys_unsorted[] as $k | {key: $k, value: .[$k]}];
def from_entries: reduce .[] as $x ({};
    . + { ($x | if type == "object" then (if .key == null then .k // .name // .Name // .K // .Key else .key end) else null end
             | if type == "string" then . else tojson end):
          ($x | if type == "object" then (if has("value") then .value else .v end) else null end)
        });
def with_entries(f): to_entries | map(f) | from_entries;
def add(f): reduce f as $x (null; . + $x);
def any: reduce .[] as $x (false; . or $x);
def all: reduce .[] as $x (true; . and $x);
def any(f): reduce (.[] | f) as $x (false; . or $x);
def all(f): reduce (.[] | f) as $x (true; . and $x);
def any(g; cond): isempty(first(g | cond | select(.))) | not;
def all(g; cond): isempty(first(g | cond | select(. | not)));
def range($x): range(0; $x);
def in(xs): . as $x | xs | has($x);
def inside(xs): . as $x | xs | contains($x);
def combinations: if length == 0 then [] else .[0][] as $x | (.[1:] | combinations) as $w | [$x] + $w end;
def combinations(n): . as $dot | [range(n)] | map($dot) | combinations;
def walk(f): def w: if type == "object" then map_values(w) elif type == "array" then map(w) else . end | f; w;
def isvalid(f): try (f | true) catch false;
def last(f): reduce f as $x (null; $x);
def nth($n): .[$n];
def nth($n; f): if $n < 0 then error("Out of bounds negative array index") else last(limit($n + 1; f)) end;
def first: .[0];
def last: .[-1];
def paths: path(..) | select(length > 0);
def paths(node_filter): . as $dot | paths | select(. as $p | $dot | getpath($p) | node_filter);
def leaf_paths: paths(scalars);
def del(f): delpaths([path(f)]);
def toarray: if type == "array" then . else [.] end;
def todate: strftime("%Y-%m-%dT%H:%M:%SZ");
def todateiso8601: strftime("%Y-%m-%dT%H:%M:%SZ");
def fromdateiso8601: strptime("%Y-%m-%dT%H:%M:%SZ") | mktime;
def fromdate: fromdateiso8601;
def date: todate;
def dateadd(u; n): . + n;
def datesub(u; n): . - n;
def finites: select(isinfinite or isnan | not);
def normals: select(isnormal);
def env: $ENV;
def INDEX(stream; idx_expr): reduce stream as $row ({}; .[$row | idx_expr | tostring] |= $row);
def INDEX(idx_expr): INDEX(.[]; idx_expr);
def IN(s): any(s == .; .);
def IN(src; s): any(src == s; .);
def ascii: [.] | implode;
def tostream: path(def r: (.[]?|r), .; r) as $p | getpath($p) | reduce path(.[]?) as $q ([$p, .]; [$p+$q]);
def fromstream(f): { x: null, e: false } as $init
  | foreach f as $i ($init;
      if .e then $init else . end
      | if $i | length == 2
        then setpath(["e"]; $i[0] | length == 0) | setpath(["x"] + $i[0]; $i[1])
        else setpath(["e"]; $i[0] | length == 1) end;
      if .e then .x else empty end);
def truncate_stream(stream): . as $n | null | stream | . as $input | if (.[0]|length) > $n then setpath([0];.[0][$n:]) else empty end;
def pick(pathexps): . as $top | reduce path(pathexps) as $p (null; setpath($p; $top | getpath($p)));
def debug(msg): (msg | debug | empty), .;
def test($re): test($re; null);
def test($re; $flags): _match_impl($re; $flags; true);
def match($re): match($re; null);
def match($re; $flags): _match_impl($re; $flags; false) | .[];
def capture($re): capture($re; null);
def capture($re; $mods): match($re; $mods) | reduce (.captures | .[] | select(.name != null) | { key: .name, value: .string }) as $pair ({}; . + {($pair.key): $pair.value});
def scan($re): scan($re; null);
def scan($re; $flags): match($re; "g" + $flags) | if (.captures | length > 0) then [ .captures | .[] | .string ] else .string end;
def splits($re): splits($re; null);
def splits($re; $flags): split($re; $flags) | .[];
def sub($re; str): sub($re; str; "");
def gsub($re; str): sub($re; str; "g");
def gsub($re; str; $flags): sub($re; str; $flags + "g");
def transpose: if . == [] then [] else . as $in | (map(length) | max) as $max | [range(0; $max) as $j | [range(0; $in | length) as $i | $in[$i][$j]]] end;
def have_literal_numbers: true;
def have_decnum: false;
def halt_error: halt_error(5);
"#;

/// Natively implemented builtins (`name`, arity).
pub(crate) const NATIVES: &[(&str, usize)] = &[
    ("empty", 0),
    ("not", 0),
    ("error", 0),
    ("error", 1),
    ("path", 1),
    ("getpath", 1),
    ("setpath", 2),
    ("delpaths", 1),
    ("limit", 2),
    ("first", 1),
    ("isempty", 1),
    ("recurse", 1),
    ("range", 2),
    ("range", 3),
    ("until", 2),
    ("while", 2),
    ("repeat", 1),
    ("sort_by", 1),
    ("group_by", 1),
    ("unique_by", 1),
    ("min_by", 1),
    ("max_by", 1),
    ("sub", 3),
    ("_match_impl", 3),
    ("split", 1),
    ("split", 2),
    ("input", 0),
    ("inputs", 0),
    ("debug", 0),
    ("stderr", 0),
    ("input_filename", 0),
    ("input_line_number", 0),
    ("builtins", 0),
    ("halt_error", 1),
    ("tojson", 0),
    ("fromjson", 0),
    ("tostring", 0),
    ("tonumber", 0),
    ("type", 0),
    ("length", 0),
    ("utf8bytelength", 0),
    ("keys", 0),
    ("keys_unsorted", 0),
    ("has", 1),
    ("contains", 1),
    ("add", 0),
    ("flatten", 0),
    ("flatten", 1),
    ("sort", 0),
    ("unique", 0),
    ("min", 0),
    ("max", 0),
    ("reverse", 0),
    ("startswith", 1),
    ("endswith", 1),
    ("ltrimstr", 1),
    ("rtrimstr", 1),
    ("trim", 0),
    ("ltrim", 0),
    ("rtrim", 0),
    ("join", 1),
    ("ascii_downcase", 0),
    ("ascii_upcase", 0),
    ("explode", 0),
    ("implode", 0),
    ("indices", 1),
    ("index", 1),
    ("rindex", 1),
    ("infinite", 0),
    ("nan", 0),
    ("isinfinite", 0),
    ("isnan", 0),
    ("isnormal", 0),
    ("abs", 0),
    ("now", 0),
    ("mktime", 0),
    ("gmtime", 0),
    ("localtime", 0),
    ("strftime", 1),
    ("strflocaltime", 1),
    ("strptime", 1),
    ("format", 1),
    ("significand", 0),
    ("logb", 0),
    ("gamma", 0),
    ("lgamma", 0),
    ("tgamma", 0),
    ("frexp", 0),
    ("modf", 0),
    ("pow", 2),
    ("atan2", 2),
    ("fmin", 2),
    ("fmax", 2),
    ("fmod", 2),
    ("ldexp", 2),
    ("scalb", 2),
    ("scalbln", 2),
    ("hypot", 2),
    ("copysign", 2),
    ("drem", 2),
    ("nearbyint", 0),
    ("header", 1),
    ("header", 2),
    ("property", 1),
    ("property", 2),
    ("variable", 1),
    ("variable", 2),
];

const MATH0: &[&str] = &[
    "floor", "ceil", "round", "sqrt", "fabs", "trunc", "rint", "exp", "exp2", "exp10", "log", "log2", "log10", "log1p", "expm1", "sin", "cos", "tan", "asin", "acos", "atan", "sinh", "cosh", "tanh",
    "asinh", "acosh", "atanh", "cbrt",
];

/// Is `name/arity` a native builtin?
pub(crate) fn is_native(name: &str, arity: usize) -> bool {
    NATIVES.iter().any(|(n, a)| *n == name && *a == arity) || (arity == 0 && MATH0.contains(&name))
}

/// `$ENV` / `env`.
pub(crate) fn env_object() -> Value {
    let mut m = ObjectMap::new();
    for (k, v) in std::env::vars() {
        m.insert(k, Value::String(v));
    }
    Value::Object(m)
}

fn need_str<'v>(v: &'v Value, what: &str) -> Res<&'v str> {
    match v {
        Value::String(s) => Ok(s),
        other => err(format!("{} ({}) {}", tname(other), dump_trunc(other), what)),
    }
}

fn need_num(v: &Value, fname: &str) -> Res<f64> {
    as_f64(v).ok_or_else(|| Ctl::Err(Value::String(format!("{} ({}) number required{}", tname(v), dump_trunc(v), if fname.is_empty() { String::new() } else { format!(" by {}", fname) }))))
}

fn sort_values(mut items: Vec<Value>) -> Vec<Value> {
    items.sort_by(cmp);
    items
}

fn keyed<'a>(me: &Interp<'_>, f: &'a Ast, env: &Env<'a>, items: &[Value]) -> Res<Vec<(Value, Value)>> {
    let mut out = Vec::with_capacity(items.len());
    for x in items {
        out.push((Value::Array(me.values(f, env, x)?), x.clone()));
    }
    out.sort_by(|a, b| cmp(&a.0, &b.0));
    Ok(out)
}

fn need_array<'v>(v: &'v Value, fname: &str) -> Res<&'v Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a),
        other => err(format!("Cannot index {} with number{}", tname(other), if fname.is_empty() { String::new() } else { String::new() })),
    }
}

fn contains(a: &Value, b: &Value) -> Res<bool> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for (k, v) in y {
                match x.get(k) {
                    Some(xv) => {
                        if !contains(xv, v)? {
                            return Ok(false);
                        }
                    }
                    None => return Ok(false),
                }
            }
            Ok(true)
        }
        (Value::Array(x), Value::Array(y)) => {
            for yv in y {
                let mut found = false;
                for xv in x {
                    if tname(xv) == tname(yv) && contains(xv, yv)? {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return Ok(false);
                }
            }
            Ok(true)
        }
        (Value::String(x), Value::String(y)) => Ok(x.contains(y.as_str())),
        _ if tname(a) == tname(b) => Ok(cmp(a, b) == Ordering::Equal),
        _ => err(format!("{} ({}) and {} ({}) cannot have their containment checked", tname(a), dump_trunc(a), tname(b), dump_trunc(b))),
    }
}

fn flatten(v: &[Value], depth: f64, out: &mut Vec<Value>) {
    for x in v {
        match x {
            Value::Array(a) if depth > 0.0 => flatten(a, depth - 1.0, out),
            other => out.push(other.clone()),
        }
    }
}

fn str_indices(s: &str, needle: &str) -> Vec<Value> {
    if needle.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let sb = s.as_bytes();
    let nb = needle.as_bytes();
    let mut cp = 0usize;
    let mut i = 0usize;
    while i + nb.len() <= sb.len() {
        if s.is_char_boundary(i) {
            if &sb[i..i + nb.len()] == nb {
                out.push(Value::Int(cp as i64));
            }
            cp += 1;
        }
        i += 1;
    }
    out
}

fn indices(v: &Value, i: &Value) -> Res<Value> {
    Ok(match (v, i) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::String(s), Value::String(n)) => Value::Array(str_indices(s, n)),
        (Value::Array(a), Value::Array(n)) => indices_of(a, n),
        (Value::Array(a), x) => indices_of(a, std::slice::from_ref(x)),
        _ => return err(format!("Cannot determine indices of {} in {}", tname(i), tname(v))),
    })
}

fn math1(name: &str, x: f64) -> f64 {
    match name {
        "floor" => x.floor(),
        "ceil" => x.ceil(),
        "round" => x.round(),
        "sqrt" => x.sqrt(),
        "fabs" => x.abs(),
        "trunc" => x.trunc(),
        "rint" | "nearbyint" => {
            let r = x.round();
            if (x - x.trunc()).abs() == 0.5 {
                2.0 * (x / 2.0).round()
            } else {
                r
            }
        }
        "exp" => x.exp(),
        "exp2" => x.exp2(),
        "exp10" => 10f64.powf(x),
        "log" => x.ln(),
        "log2" => x.log2(),
        "log10" => x.log10(),
        "log1p" => x.ln_1p(),
        "expm1" => x.exp_m1(),
        "sin" => x.sin(),
        "cos" => x.cos(),
        "tan" => x.tan(),
        "asin" => x.asin(),
        "acos" => x.acos(),
        "atan" => x.atan(),
        "sinh" => x.sinh(),
        "cosh" => x.cosh(),
        "tanh" => x.tanh(),
        "asinh" => x.asinh(),
        "acosh" => x.acosh(),
        "atanh" => x.atanh(),
        "cbrt" => x.cbrt(),
        "logb" => {
            if x == 0.0 {
                f64::NEG_INFINITY
            } else {
                x.abs().log2().floor()
            }
        }
        "significand" => {
            if x == 0.0 || !x.is_finite() {
                x
            } else {
                x / 2f64.powf(x.abs().log2().floor())
            }
        }
        "gamma" | "lgamma" => lgamma(x),
        "tgamma" => lgamma(x).exp() * if x < 0.0 && (x.floor() as i64) % 2 != 0 { -1.0 } else { 1.0 },
        _ => f64::NAN,
    }
}

/// Lanczos approximation of ln|Γ(x)|.
fn lgamma(x: f64) -> f64 {
    if x < 0.5 {
        let s = (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).abs().ln();
        return s - lgamma(1.0 - x);
    }
    const G: f64 = 7.0;
    const C: [f64; 9] = [0.999_999_999_999_809_9, 676.520_368_121_885_1, -1_259.139_216_722_402_8, 771.323_428_777_653_1, -176.615_029_162_140_6, 12.507_343_278_686_905, -0.138_571_095_265_720_12, 9.984_369_578_019_572e-6, 1.505_632_735_149_311_6e-7];
    let x = x - 1.0;
    let mut a = C[0];
    let t = x + G + 0.5;
    for (i, c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

fn math2(name: &str, a: f64, b: f64) -> f64 {
    match name {
        "pow" => a.powf(b),
        "atan2" => a.atan2(b),
        "fmin" => a.min(b),
        "fmax" => a.max(b),
        "fmod" => a % b,
        "ldexp" | "scalb" | "scalbln" => a * 2f64.powf(b),
        "hypot" => a.hypot(b),
        "copysign" => a.copysign(b),
        "drem" => a - b * (a / b).round(),
        _ => f64::NAN,
    }
}

/// Apply a `@format`.
pub(crate) fn format(name: &str, v: &Value) -> Res<String> {
    Ok(match name {
        "text" => tostring(v),
        "json" => to_json(v),
        "html" => {
            let s = tostring(v);
            let mut o = String::with_capacity(s.len());
            for c in s.chars() {
                match c {
                    '<' => o.push_str("&lt;"),
                    '>' => o.push_str("&gt;"),
                    '&' => o.push_str("&amp;"),
                    '\'' => o.push_str("&#39;"),
                    '"' => o.push_str("&quot;"),
                    c => o.push(c),
                }
            }
            o
        }
        "uri" => {
            let s = tostring(v);
            let mut o = String::new();
            for b in s.bytes() {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    o.push(b as char);
                } else {
                    o.push_str(&format!("%{:02X}", b));
                }
            }
            o
        }
        "csv" | "tsv" => {
            let a = match v {
                Value::Array(a) => a,
                other => return err(format!("{} ({}) cannot be {}-formatted, only an array can be", tname(other), dump_trunc(other), name)),
            };
            let mut cells = Vec::new();
            for x in a {
                cells.push(match x {
                    Value::Null => String::new(),
                    Value::Bool(b) => b.to_string(),
                    Value::Int(_) | Value::Float(_) => to_json(x),
                    Value::String(s) => {
                        if name == "csv" {
                            format!("\"{}\"", s.replace('"', "\"\""))
                        } else {
                            s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n").replace('\r', "\\r")
                        }
                    }
                    other => return err(format!("{} ({}) is not valid in a {} row", tname(other), dump_trunc(other), name)),
                });
            }
            cells.join(if name == "csv" { "," } else { "\t" })
        }
        "sh" => {
            let quote = |x: &Value| -> Res<String> {
                match x {
                    Value::String(s) => Ok(format!("'{}'", s.replace('\'', "'\\''"))),
                    Value::Array(_) | Value::Object(_) => err(format!("{} ({}) can not be escaped for shell", tname(x), dump_trunc(x))),
                    other => Ok(to_json(other)),
                }
            };
            match v {
                Value::Array(a) => a.iter().map(quote).collect::<Res<Vec<_>>>()?.join(" "),
                other => quote(other)?,
            }
        }
        "base64" => crate::dataformat::base64::encode(tostring(v).as_bytes(), false),
        "base64d" => {
            let s = tostring(v);
            let trimmed: String = s.chars().filter(|c| !c.is_whitespace()).collect();
            let mut t = trimmed.trim_end_matches('=').to_string();
            while t.len() % 4 != 0 {
                t.push('=');
            }
            match crate::dataformat::base64::decode(&t) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(_) => return err(format!("{} ({}) is not valid base64 data", tname(v), dump_trunc(v))),
            }
        }
        "base32" => base32_encode(tostring(v).as_bytes()),
        "base32d" => match base32_decode(&tostring(v)) {
            Some(b) => String::from_utf8_lossy(&b).into_owned(),
            None => return err(format!("{} ({}) is not valid base32 data", tname(v), dump_trunc(v))),
        },
        other => return err(format!("{} is not a valid format", other)),
    })
}

const B32: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

fn base32_encode(data: &[u8]) -> String {
    let mut out = String::new();
    for chunk in data.chunks(5) {
        let mut buf = [0u8; 5];
        buf[..chunk.len()].copy_from_slice(chunk);
        let n = u64::from_be_bytes([0, 0, 0, buf[0], buf[1], buf[2], buf[3], buf[4]]);
        let chars = match chunk.len() {
            1 => 2,
            2 => 4,
            3 => 5,
            4 => 7,
            _ => 8,
        };
        for i in 0..8 {
            if i < chars {
                out.push(B32[((n >> (35 - i * 5)) & 31) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut bits = 0u64;
    let mut nbits = 0;
    let mut out = Vec::new();
    for c in s.trim_end_matches('=').bytes() {
        let v = B32.iter().position(|&b| b == c.to_ascii_uppercase())? as u64;
        bits = (bits << 5) | v;
        nbits += 5;
        if nbits >= 8 {
            nbits -= 8;
            out.push((bits >> nbits) as u8);
            bits &= (1 << nbits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// Regex
// ---------------------------------------------------------------------------

struct Rx {
    re: crate::regex::Regex,
    global: bool,
    no_empty: bool,
}

fn compile_rx(re: &Value, flags: &Value) -> Res<Rx> {
    let (re, flags) = match (re, flags) {
        (Value::Array(a), Value::Null) if !a.is_empty() => (a[0].clone(), a.get(1).cloned().unwrap_or(Value::Null)),
        _ => (re.clone(), flags.clone()),
    };
    let re = match re {
        Value::String(s) => s,
        other => return err(format!("{} ({}) cannot be matched, as it is not a string", tname(&other), dump_trunc(&other))),
    };
    let flags = match flags {
        Value::Null => String::new(),
        Value::String(s) => s,
        other => return err(format!("{} ({}) is not a string", tname(&other), dump_trunc(&other))),
    };
    let mut prefix = String::new();
    let (mut global, mut no_empty, mut ci) = (false, false, false);
    for c in flags.chars() {
        match c {
            'g' => global = true,
            'i' => ci = true,
            'x' => prefix.push_str("(?x)"),
            'n' => no_empty = true,
            's' => prefix.push_str("(?s)"),
            'p' => prefix.push_str("(?s)(?m)"),
            'l' => {}
            _ => return err(format!("{} is not a valid modifier string", flags)),
        }
    }
    let pattern = format!("{}{}", prefix, re);
    let re = crate::regex::Regex::with_flags(&pattern, ci).map_err(|e| Ctl::Err(Value::String(format!("{} (at offset 0) is not a valid regex: {}", re, e.message))))?;
    Ok(Rx { re, global, no_empty })
}

fn cp_offset(s: &str, byte: usize) -> i64 {
    s[..byte].chars().count() as i64
}

fn matches(s: &str, rx: &Rx) -> Vec<Value> {
    let names: Vec<(usize, String)> = rx.re.group_names().iter().map(|(n, i)| (*i, n.clone())).collect();
    let name_of = |i: usize| names.iter().find(|(j, _)| *j == i).map(|(_, n)| Value::String(n.clone())).unwrap_or(Value::Null);
    let caps = if rx.global { rx.re.captures_iter(s) } else { rx.re.captures_match(s).into_iter().collect() };
    let mut out = Vec::new();
    for c in caps {
        let (ms, me) = c.span(0).unwrap();
        if rx.no_empty && ms == me {
            continue;
        }
        let mut groups = Vec::new();
        for g in 1..c.len() {
            let entry = match c.span(g) {
                Some((gs, ge)) => Value::object([
                    ("offset", Value::Int(cp_offset(s, gs))),
                    ("length", Value::Int(s[gs..ge].chars().count() as i64)),
                    ("string", Value::String(s[gs..ge].to_string())),
                    ("name", name_of(g)),
                ]),
                None => Value::object([("offset", Value::Int(-1)), ("length", Value::Int(0)), ("string", Value::Null), ("name", name_of(g))]),
            };
            groups.push(entry);
        }
        out.push(Value::object([
            ("offset", Value::Int(cp_offset(s, ms))),
            ("length", Value::Int(s[ms..me].chars().count() as i64)),
            ("string", Value::String(s[ms..me].to_string())),
            ("captures", Value::Array(groups)),
        ]));
    }
    out
}

fn match_spans(s: &str, rx: &Rx) -> Vec<crate::regex::Captures> {
    let caps = if rx.global { rx.re.captures_iter(s) } else { rx.re.captures_match(s).into_iter().collect() };
    caps.into_iter().filter(|c| !(rx.no_empty && c.span(0).map(|(a, b)| a == b).unwrap_or(false))).collect()
}

fn capture_object(rx: &Rx, c: &crate::regex::Captures) -> Value {
    let mut m = ObjectMap::new();
    let mut names: Vec<(&String, &usize)> = rx.re.group_names().iter().collect();
    names.sort_by_key(|(_, i)| **i);
    for (n, i) in names {
        m.insert(n.clone(), c.get(*i).map(|s| Value::String(s.to_string())).unwrap_or(Value::Null));
    }
    Value::Object(m)
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

const WDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    crate::util_time::epoch_from_civil(y, m, d, 0, 0, 0).div_euclid(86_400)
}

/// Broken-down time `[year, month0, mday, hours, minutes, seconds, wday, yday]`.
fn tm_from_secs(secs: f64, offset: i64) -> Value {
    let whole = secs.floor();
    let frac = secs - whole;
    let local = whole as i64 + offset;
    let (y, m, d, h, mi, s) = crate::util_time::civil_from_epoch(local);
    let days = local.div_euclid(86_400);
    let wday = (days + 4).rem_euclid(7);
    let yday = days - days_from_civil(y, 1, 1);
    let sec = if frac == 0.0 { Value::Int(s as i64) } else { Value::Float(s as f64 + frac) };
    Value::Array(vec![Value::Int(y), Value::Int(m as i64 - 1), Value::Int(d as i64), Value::Int(h as i64), Value::Int(mi as i64), sec, Value::Int(wday), Value::Int(yday)])
}

fn tm_fields(v: &Value, fname: &str) -> Res<Vec<f64>> {
    let a = match v {
        Value::Array(a) => a,
        _ => return err(format!("{} requires parsed datetime inputs", fname)),
    };
    if a.len() < 6 {
        return err(format!("{} requires parsed datetime inputs", fname));
    }
    let mut out = Vec::new();
    for x in a {
        out.push(as_f64(x).ok_or_else(|| Ctl::Err(Value::String(format!("{} requires parsed datetime inputs", fname))))?);
    }
    Ok(out)
}

fn secs_from_tm(tm: &[f64]) -> f64 {
    let mon0 = tm[1] as i64;
    let y = tm[0] as i64 + mon0.div_euclid(12);
    let mon = mon0.rem_euclid(12) as u32 + 1;
    let base = crate::util_time::epoch_from_civil(y, mon, 1, 0, 0, 0) as f64;
    base + (tm[2] - 1.0) * 86_400.0 + tm[3] * 3600.0 + tm[4] * 60.0 + tm[5]
}

fn iso_week(y: i64, yday: i64, wday: i64) -> (i64, i64) {
    // ISO 8601 week number and week-based year.
    let wd = (wday + 6) % 7; // Monday = 0
    let mut week = (yday - wd + 10) / 7;
    let mut year = y;
    let leap = |y: i64| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    if week < 1 {
        year -= 1;
        let prev_days = if leap(year) { 366 } else { 365 };
        week = (yday + prev_days - wd + 10) / 7;
    } else {
        let days = if leap(y) { 366 } else { 365 };
        if week == 53 && (yday - wd + 10) - days >= 7 {
            week = 1;
            year += 1;
        } else if week == 53 {
            // Week 53 only exists if Dec 31 falls on Thursday (or Friday in leap years).
            let dec31_wd = (wd + (days - 1 - yday)) % 7;
            if dec31_wd < 3 {
                week = 1;
                year += 1;
            }
        }
    }
    (year, week)
}

fn strftime(tm: &[f64], fmt: &str, zone: &str, offset: i64) -> String {
    let y = tm[0] as i64;
    let mon = tm[1] as i64;
    let mday = tm[2] as i64;
    let h = tm[3] as i64;
    let mi = tm[4] as i64;
    let s = tm[5].floor() as i64;
    let days = days_from_civil(y + mon.div_euclid(12), mon.rem_euclid(12) as u32 + 1, 1) + mday - 1;
    let wday = tm.get(6).map(|w| *w as i64).filter(|_| tm.len() >= 8).unwrap_or((days + 4).rem_euclid(7));
    let yday = tm.get(7).map(|w| *w as i64).unwrap_or(days - days_from_civil(y, 1, 1));
    let mut o = String::new();
    let mut it = fmt.chars().peekable();
    while let Some(c) = it.next() {
        if c != '%' {
            o.push(c);
            continue;
        }
        let Some(d) = it.next() else {
            o.push('%');
            break;
        };
        let h12 = if h % 12 == 0 { 12 } else { h % 12 };
        let m_idx = mon.rem_euclid(12) as usize;
        match d {
            'Y' => o.push_str(&y.to_string()),
            'C' => o.push_str(&format!("{:02}", y.div_euclid(100))),
            'y' => o.push_str(&format!("{:02}", y.rem_euclid(100))),
            'm' => o.push_str(&format!("{:02}", mon + 1)),
            'd' => o.push_str(&format!("{:02}", mday)),
            'e' => o.push_str(&format!("{:>2}", mday)),
            'H' => o.push_str(&format!("{:02}", h)),
            'k' => o.push_str(&format!("{:>2}", h)),
            'I' => o.push_str(&format!("{:02}", h12)),
            'l' => o.push_str(&format!("{:>2}", h12)),
            'M' => o.push_str(&format!("{:02}", mi)),
            'S' => o.push_str(&format!("{:02}", s)),
            'j' => o.push_str(&format!("{:03}", yday + 1)),
            'a' => o.push_str(&WDAYS[wday.rem_euclid(7) as usize][..3]),
            'A' => o.push_str(WDAYS[wday.rem_euclid(7) as usize]),
            'b' | 'h' => o.push_str(&MONTHS[m_idx][..3]),
            'B' => o.push_str(MONTHS[m_idx]),
            'p' => o.push_str(if h < 12 { "AM" } else { "PM" }),
            'P' => o.push_str(if h < 12 { "am" } else { "pm" }),
            'c' => o.push_str(&format!("{} {} {:>2} {:02}:{:02}:{:02} {}", &WDAYS[wday.rem_euclid(7) as usize][..3], &MONTHS[m_idx][..3], mday, h, mi, s, y)),
            'D' | 'x' => o.push_str(&format!("{:02}/{:02}/{:02}", mon + 1, mday, y.rem_euclid(100))),
            'F' => o.push_str(&format!("{}-{:02}-{:02}", y, mon + 1, mday)),
            'T' | 'X' => o.push_str(&format!("{:02}:{:02}:{:02}", h, mi, s)),
            'R' => o.push_str(&format!("{:02}:{:02}", h, mi)),
            'r' => o.push_str(&format!("{:02}:{:02}:{:02} {}", h12, mi, s, if h < 12 { "AM" } else { "PM" })),
            's' => o.push_str(&((secs_from_tm(tm) as i64) - offset).to_string()),
            'u' => o.push_str(&(if wday == 0 { 7 } else { wday }).to_string()),
            'w' => o.push_str(&wday.to_string()),
            'U' => o.push_str(&format!("{:02}", (yday + 7 - wday) / 7)),
            'W' => o.push_str(&format!("{:02}", (yday + 7 - ((wday + 6) % 7)) / 7)),
            'V' => o.push_str(&format!("{:02}", iso_week(y, yday, wday).1)),
            'G' => o.push_str(&iso_week(y, yday, wday).0.to_string()),
            'g' => o.push_str(&format!("{:02}", iso_week(y, yday, wday).0.rem_euclid(100))),
            'z' => {
                let sign = if offset < 0 { '-' } else { '+' };
                let a = offset.abs();
                o.push_str(&format!("{}{:02}{:02}", sign, a / 3600, (a % 3600) / 60));
            }
            'Z' => o.push_str(zone),
            'n' => o.push('\n'),
            't' => o.push('\t'),
            '%' => o.push('%'),
            other => {
                o.push('%');
                o.push(other);
            }
        }
    }
    o
}

fn strptime(s: &str, fmt: &str) -> Option<Value> {
    let sc: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let (mut y, mut mon, mut d, mut h, mut mi, mut sec) = (1900i64, 0i64, 1i64, 0i64, 0i64, 0i64);
    let mut yday: Option<i64> = None;
    let mut pm: Option<bool> = None;
    let mut epoch: Option<i64> = None;
    let mut century: Option<i64> = None;
    let mut yy: Option<i64> = None;
    fn num(sc: &[char], i: &mut usize, max: usize, allow_sign: bool) -> Option<i64> {
        while *i < sc.len() && sc[*i] == ' ' {
            *i += 1;
        }
        let st = *i;
        if allow_sign && *i < sc.len() && (sc[*i] == '-' || sc[*i] == '+') {
            *i += 1;
        }
        while *i < sc.len() && sc[*i].is_ascii_digit() && *i - st < max {
            *i += 1;
        }
        let t: String = sc[st..*i].iter().collect();
        t.parse().ok()
    }
    fn name(sc: &[char], i: &mut usize, names: &[&str]) -> Option<usize> {
        let rest: String = sc[*i..].iter().collect::<String>().to_lowercase();
        for (k, n) in names.iter().enumerate() {
            let full = n.to_lowercase();
            if rest.starts_with(&full) {
                *i += full.chars().count();
                return Some(k);
            }
        }
        for (k, n) in names.iter().enumerate() {
            let short: String = n.to_lowercase().chars().take(3).collect();
            if rest.starts_with(&short) {
                *i += 3;
                return Some(k);
            }
        }
        None
    }
    let expanded = fmt.replace("%T", "%H:%M:%S").replace("%D", "%m/%d/%y").replace("%F", "%Y-%m-%d").replace("%R", "%H:%M").replace("%r", "%I:%M:%S %p").replace("%c", "%a %b %e %H:%M:%S %Y");
    let fc: Vec<char> = expanded.chars().collect();
    let mut k = 0;
    while k < fc.len() {
        let c = fc[k];
        k += 1;
        if c.is_whitespace() {
            while i < sc.len() && sc[i].is_whitespace() {
                i += 1;
            }
            continue;
        }
        if c != '%' {
            if i < sc.len() && sc[i] == c {
                i += 1;
                continue;
            }
            return None;
        }
        let d2 = *fc.get(k)?;
        k += 1;
        match d2 {
            'Y' => y = num(&sc, &mut i, 4, true)?,
            'C' => century = Some(num(&sc, &mut i, 2, false)?),
            'y' => yy = Some(num(&sc, &mut i, 2, false)?),
            'm' => mon = num(&sc, &mut i, 2, false)? - 1,
            'd' | 'e' => d = num(&sc, &mut i, 2, false)?,
            'H' | 'k' => h = num(&sc, &mut i, 2, false)?,
            'I' | 'l' => h = num(&sc, &mut i, 2, false)?,
            'M' => mi = num(&sc, &mut i, 2, false)?,
            'S' => sec = num(&sc, &mut i, 2, false)?,
            'j' => yday = Some(num(&sc, &mut i, 3, false)? - 1),
            's' => epoch = Some(num(&sc, &mut i, 20, true)?),
            'a' | 'A' => {
                name(&sc, &mut i, &WDAYS)?;
            }
            'b' | 'B' | 'h' => mon = name(&sc, &mut i, &MONTHS)? as i64,
            'p' | 'P' => {
                let rest: String = sc[i..].iter().take(2).collect::<String>().to_uppercase();
                pm = Some(match rest.as_str() {
                    "AM" => false,
                    "PM" => true,
                    _ => return None,
                });
                i += 2;
            }
            'z' => {
                if i < sc.len() && (sc[i] == 'Z' || sc[i] == 'z') {
                    i += 1;
                } else {
                    if i < sc.len() && (sc[i] == '+' || sc[i] == '-') {
                        i += 1;
                    } else {
                        return None;
                    }
                    let st = i;
                    while i < sc.len() && (sc[i].is_ascii_digit() || sc[i] == ':') && i - st < 5 {
                        i += 1;
                    }
                }
            }
            'Z' => {
                while i < sc.len() && (sc[i].is_ascii_alphabetic()) {
                    i += 1;
                }
            }
            'n' | 't' => {
                while i < sc.len() && sc[i].is_whitespace() {
                    i += 1;
                }
            }
            '%' => {
                if i < sc.len() && sc[i] == '%' {
                    i += 1;
                } else {
                    return None;
                }
            }
            _ => return None,
        }
    }
    if i != sc.len() {
        return None;
    }
    if let Some(e) = epoch {
        return Some(tm_from_secs(e as f64, 0));
    }
    if let Some(v) = yy {
        y = century.map(|c| c * 100).unwrap_or(if v < 69 { 2000 } else { 1900 }) + v;
    } else if let Some(c) = century {
        y = c * 100 + y.rem_euclid(100);
    }
    if let Some(p) = pm {
        h %= 12;
        if p {
            h += 12;
        }
    }
    if let Some(yd) = yday {
        let days = days_from_civil(y, 1, 1) + yd;
        let (yy2, m2, d2, _, _, _) = crate::util_time::civil_from_epoch(days * 86_400);
        y = yy2;
        mon = m2 as i64 - 1;
        d = d2 as i64;
    }
    let secs = secs_from_tm(&[y as f64, mon as f64, d as f64, h as f64, mi as f64, sec as f64]);
    Some(tm_from_secs(secs, 0))
}

fn local_offset(secs: f64) -> (i64, String) {
    let tz = crate::tz::TimeZone::local();
    let s = secs.floor() as i64;
    (tz.offset_at(s) as i64, tz.abbreviation_at(s))
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

fn emit(cb: Cb<'_>, v: Value) -> R {
    cb(Pv::val(v))
}

/// Run a native builtin.
pub(crate) fn native<'a>(me: &Interp<'_>, name: &str, args: &'a [Ast], env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
    let v = &inp.v;
    if args.is_empty() && MATH0.contains(&name) {
        let x = need_num(v, name)?;
        return emit(cb, num(math1(name, x)));
    }
    match (name, args.len()) {
        ("empty", 0) => Ok(()),
        ("not", 0) => emit(cb, Value::Bool(!truthy(v))),
        ("error", 0) => Err(Ctl::Err(v.clone())),
        ("error", 1) => me.with_args(args, env, v, &mut |a| Err(Ctl::Err(a[0].clone()))),
        ("halt_error", 1) => Err(Ctl::Err(v.clone())),
        ("input" | "inputs", 0) => err("No more inputs"),
        ("debug", 0) => {
            crate::camel_debug!("jq", "[\"DEBUG:\",{}]", to_json(v));
            cb(inp)
        }
        ("stderr", 0) => cb(inp),
        ("input_filename", 0) => emit(cb, Value::Null),
        ("input_line_number", 0) => emit(cb, Value::Int(0)),
        ("builtins", 0) => {
            let mut names: Vec<Value> = NATIVES.iter().map(|(n, a)| Value::String(format!("{}/{}", n, a))).collect();
            names.extend(MATH0.iter().map(|n| Value::String(format!("{}/0", n))));
            for d in super::prelude_defs() {
                names.push(Value::String(format!("{}/{}", d.name, d.params.len())));
            }
            emit(cb, Value::Array(names))
        }
        ("path", 1) => {
            for p in me.paths(&args[0], env, v)? {
                emit(cb, Value::Array(p))?;
            }
            Ok(())
        }
        ("getpath", 1) => me.with_args(args, env, v, &mut |a| {
            let p = match &a[0] {
                Value::Array(p) => p,
                _ => return err("Path must be specified as an array"),
            };
            let got = match getpath(v, p) {
                Ok(x) => x,
                Err(_) if inp.p.is_some() => Value::Null,
                Err(e) => return Err(e),
            };
            let path = inp.p.as_ref().map(|base| {
                let mut b = base.clone();
                b.extend(p.iter().cloned());
                b
            });
            cb(Pv { v: got, p: path })
        }),
        ("setpath", 2) => me.with_args(args, env, v, &mut |a| match &a[0] {
            Value::Array(p) => emit(cb, setpath(v.clone(), p, a[1].clone())?),
            _ => err("Path must be specified as an array"),
        }),
        ("delpaths", 1) => me.with_args(args, env, v, &mut |a| match &a[0] {
            Value::Array(ps) => emit(cb, delpaths(v.clone(), ps)?),
            _ => err("Paths must be specified as an array"),
        }),
        ("limit", 2) => me.with_args(&args[..1], env, v, &mut |a| {
            let n = need_num(&a[0], "limit")?;
            if n <= 0.0 {
                return Ok(());
            }
            let id = next_id();
            let mut count = 0f64;
            let r = me.eval(&args[1], env, inp.clone(), &mut |x| {
                count += 1.0;
                cb(x)?;
                if count >= n {
                    Err(Ctl::Break(id))
                } else {
                    Ok(())
                }
            });
            match r {
                Err(Ctl::Break(i)) if i == id => Ok(()),
                r => r,
            }
        }),
        ("first", 1) => {
            let id = next_id();
            let r = me.eval(&args[0], env, inp, &mut |x| {
                cb(x)?;
                Err(Ctl::Break(id))
            });
            match r {
                Err(Ctl::Break(i)) if i == id => Ok(()),
                r => r,
            }
        }
        ("isempty", 1) => {
            let id = next_id();
            let mut empty = true;
            let r = me.eval(&args[0], env, Pv::val(v.clone()), &mut |_| {
                empty = false;
                Err(Ctl::Break(id))
            });
            match r {
                Ok(()) => {}
                Err(Ctl::Break(i)) if i == id => {}
                Err(e) => return Err(e),
            }
            emit(cb, Value::Bool(empty))
        }
        ("recurse", 1) => recurse(me, &args[0], env, inp, cb),
        ("range", 2) => me.with_args(args, env, v, &mut |a| range(&a[0], &a[1], &Value::Int(1), cb)),
        ("range", 3) => me.with_args(args, env, v, &mut |a| range(&a[0], &a[1], &a[2], cb)),
        ("until", 2) => until(me, &args[0], &args[1], env, inp, cb),
        ("while", 2) => while_(me, &args[0], &args[1], env, inp, cb),
        ("repeat", 1) => repeat(me, &args[0], env, inp, cb),
        ("sort_by", 1) => {
            let items = iter_items(v, "sort")?;
            emit(cb, Value::Array(keyed(me, &args[0], env, &items)?.into_iter().map(|(_, x)| x).collect()))
        }
        ("group_by", 1) => {
            let items = iter_items(v, "group")?;
            let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
            for (k, x) in keyed(me, &args[0], env, &items)? {
                match groups.last_mut() {
                    Some((gk, g)) if cmp(gk, &k) == Ordering::Equal => g.push(x),
                    _ => groups.push((k, vec![x])),
                }
            }
            emit(cb, Value::Array(groups.into_iter().map(|(_, g)| Value::Array(g)).collect()))
        }
        ("unique_by", 1) => {
            let items = iter_items(v, "unique")?;
            let mut out: Vec<(Value, Value)> = Vec::new();
            for (k, x) in keyed(me, &args[0], env, &items)? {
                if !matches!(out.last(), Some((gk, _)) if cmp(gk, &k) == Ordering::Equal) {
                    out.push((k, x));
                }
            }
            emit(cb, Value::Array(out.into_iter().map(|(_, x)| x).collect()))
        }
        ("min_by" | "max_by", 1) => {
            let items = iter_items(v, name)?;
            let ks = keyed(me, &args[0], env, &items)?;
            let r = if name == "min_by" { ks.into_iter().next() } else { ks.into_iter().last() };
            emit(cb, r.map(|(_, x)| x).unwrap_or(Value::Null))
        }
        ("sub", 3) => sub(me, args, env, v, cb),
        ("_match_impl", 3) => me.with_args(args, env, v, &mut |a| {
            let s = match v {
                Value::String(s) => s,
                other => return err(format!("{} ({}) cannot be matched, as it is not a string", tname(other), dump_trunc(other))),
            };
            let rx = compile_rx(&a[0], &a[1])?;
            if truthy(&a[2]) {
                emit(cb, Value::Bool(rx.re.find(s).is_some()))
            } else {
                emit(cb, Value::Array(matches(s, &rx)))
            }
        }),
        ("split", 1) => me.with_args(args, env, v, &mut |a| match (v, &a[0]) {
            (Value::String(s), Value::String(sep)) => emit(cb, split_str(s, sep)),
            _ => err("split input and separator must be strings"),
        }),
        ("split", 2) => me.with_args(args, env, v, &mut |a| {
            let s = need_str(v, "cannot be matched, as it is not a string")?;
            let mut flags = match &a[1] {
                Value::Null => String::new(),
                other => tostring(other),
            };
            flags.push('g');
            let rx = compile_rx(&a[0], &Value::String(flags))?;
            let mut out = Vec::new();
            let mut prev = 0;
            for c in match_spans(s, &rx) {
                let (ms, me2) = c.span(0).unwrap();
                out.push(Value::String(s[prev..ms].to_string()));
                prev = me2;
            }
            out.push(Value::String(s[prev..].to_string()));
            emit(cb, Value::Array(out))
        }),
        ("tojson", 0) => emit(cb, Value::String(to_json(v))),
        ("fromjson", 0) => {
            let s = need_str(v, "cannot be parsed as JSON")?;
            match s.trim() {
                "nan" | "NaN" => emit(cb, Value::Float(f64::NAN)),
                t => match crate::json::parse(t) {
                    Ok(x) => emit(cb, normalize(x)),
                    Err(e) => err(format!("{} (while parsing '{}')", e.message, s)),
                },
            }
        }
        ("tostring", 0) => emit(cb, Value::String(tostring(v))),
        ("tonumber", 0) => match v {
            Value::Int(_) | Value::Float(_) => cb(inp),
            Value::String(s) => match s.trim().parse::<f64>() {
                Ok(f) if !s.trim().is_empty() && !s.trim().starts_with('+') && f.is_finite() || s.trim() == "nan" => {
                    if let Ok(i) = s.trim().parse::<i64>() {
                        emit(cb, Value::Int(i))
                    } else {
                        emit(cb, num(f))
                    }
                }
                _ => err(format!("Cannot parse '{}' as JSON", s)),
            },
            other => err(format!("{} ({}) cannot be parsed as a number", tname(other), dump_trunc(other))),
        },
        ("type", 0) => emit(cb, Value::String(tname(v).to_string())),
        ("length", 0) => emit(
            cb,
            match v {
                Value::Null => Value::Int(0),
                Value::Bool(_) => return err(format!("boolean ({}) has no length", to_json(v))),
                Value::Int(i) => Value::Int(i.abs()),
                Value::Float(f) => num(f.abs()),
                Value::String(s) => Value::Int(s.chars().count() as i64),
                Value::Bytes(b) => Value::Int(b.len() as i64),
                Value::Array(a) => Value::Int(a.len() as i64),
                Value::Object(o) => Value::Int(o.len() as i64),
            },
        ),
        ("utf8bytelength", 0) => match v {
            Value::String(s) => emit(cb, Value::Int(s.len() as i64)),
            other => err(format!("{} ({}) only strings have UTF-8 byte length", tname(other), dump_trunc(other))),
        },
        ("keys" | "keys_unsorted", 0) => match v {
            Value::Object(o) => {
                let ks: Vec<String> = if name == "keys" { sorted_keys(o) } else { o.keys().cloned().collect() };
                emit(cb, Value::Array(ks.into_iter().map(Value::String).collect()))
            }
            Value::Array(a) => emit(cb, Value::Array((0..a.len()).map(|i| Value::Int(i as i64)).collect())),
            other => err(format!("{} ({}) has no keys", tname(other), dump_trunc(other))),
        },
        ("has", 1) => me.with_args(args, env, v, &mut |a| match (v, &a[0]) {
            (Value::Object(o), Value::String(k)) => emit(cb, Value::Bool(o.contains_key(k))),
            (Value::Array(arr), k @ (Value::Int(_) | Value::Float(_))) => {
                let f = as_f64(k).unwrap();
                emit(cb, Value::Bool(f >= 0.0 && (f as usize) < arr.len()))
            }
            (x, k) => err(format!("Cannot check whether {} has a {} key", tname(x), tname(k))),
        }),
        ("contains", 1) => me.with_args(args, env, v, &mut |a| emit(cb, Value::Bool(contains(v, &a[0])?))),
        ("add", 0) => {
            let items: Vec<Value> = match v {
                Value::Array(a) => a.clone(),
                Value::Object(o) => o.values().cloned().collect(),
                Value::Null => vec![],
                other => return err(format!("Cannot iterate over {} ({})", tname(other), dump_trunc(other))),
            };
            let mut acc = Value::Null;
            for x in &items {
                acc = binop(super::parse::BinOp::Add, &acc, x)?;
            }
            emit(cb, acc)
        }
        ("flatten", 0) => {
            let a = iter_items(v, "flatten")?;
            let mut out = Vec::new();
            flatten(&a, 1e9, &mut out);
            emit(cb, Value::Array(out))
        }
        ("flatten", 1) => me.with_args(args, env, v, &mut |d| {
            let depth = need_num(&d[0], "flatten")?;
            if depth < 0.0 {
                return err("flatten depth must not be negative");
            }
            let a = iter_items(v, "flatten")?;
            let mut out = Vec::new();
            flatten(&a, depth, &mut out);
            emit(cb, Value::Array(out))
        }),
        ("sort", 0) => match v {
            Value::Array(a) => emit(cb, Value::Array(sort_values(a.clone()))),
            other => err(format!("{} ({}) cannot be sorted, as it is not an array", tname(other), dump_trunc(other))),
        },
        ("unique", 0) => match v {
            Value::Array(a) => {
                let mut s = sort_values(a.clone());
                s.dedup_by(|x, y| cmp(x, y) == Ordering::Equal);
                emit(cb, Value::Array(s))
            }
            other => err(format!("{} ({}) cannot be sorted, as it is not an array", tname(other), dump_trunc(other))),
        },
        ("min" | "max", 0) => {
            let a = need_array(v, name)?;
            let r = if name == "min" {
                a.iter().fold(None::<&Value>, |m, x| match m {
                    Some(m) if cmp(x, m) != Ordering::Less => Some(m),
                    _ => Some(x),
                })
            } else {
                a.iter().fold(None::<&Value>, |m, x| match m {
                    Some(m) if cmp(x, m) == Ordering::Less => Some(m),
                    _ => Some(x),
                })
            };
            emit(cb, r.cloned().unwrap_or(Value::Null))
        }
        ("reverse", 0) => match v {
            Value::Array(a) => emit(cb, Value::Array(a.iter().rev().cloned().collect())),
            Value::String(s) => emit(cb, Value::String(s.chars().rev().collect())),
            Value::Null => emit(cb, Value::Array(vec![])),
            other => err(format!("Cannot reverse {} ({})", tname(other), dump_trunc(other))),
        },
        ("startswith" | "endswith", 1) => me.with_args(args, env, v, &mut |a| match (v, &a[0]) {
            (Value::String(s), Value::String(p)) => emit(cb, Value::Bool(if name == "startswith" { s.starts_with(p.as_str()) } else { s.ends_with(p.as_str()) })),
            _ => err(format!("{}() requires string inputs", name)),
        }),
        ("ltrimstr" | "rtrimstr", 1) => me.with_args(args, env, v, &mut |a| match (v, &a[0]) {
            (Value::String(s), Value::String(p)) => {
                let r = if name == "ltrimstr" { s.strip_prefix(p.as_str()) } else { s.strip_suffix(p.as_str()) };
                emit(cb, Value::String(r.unwrap_or(s).to_string()))
            }
            _ => emit(cb, v.clone()),
        }),
        ("trim" | "ltrim" | "rtrim", 0) => match v {
            Value::String(s) => emit(
                cb,
                Value::String(match name {
                    "trim" => s.trim(),
                    "ltrim" => s.trim_start(),
                    _ => s.trim_end(),
                }
                .to_string()),
            ),
            other => err(format!("{} ({}) cannot be trimmed", tname(other), dump_trunc(other))),
        },
        ("join", 1) => me.with_args(args, env, v, &mut |a| {
            let sep = match &a[0] {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => return err(format!("{} ({}) cannot be used as a separator", tname(other), dump_trunc(other))),
            };
            let items = iter_items(v, "join")?;
            if items.is_empty() {
                return emit(cb, Value::String(String::new()));
            }
            let mut parts = Vec::new();
            for x in &items {
                parts.push(match x {
                    Value::Null => String::new(),
                    Value::String(s) => s.clone(),
                    Value::Bool(_) | Value::Int(_) | Value::Float(_) => to_json(x),
                    other => return err(format!("Cannot join with {}", tname(other))),
                });
            }
            emit(cb, Value::String(parts.join(&sep)))
        }),
        ("ascii_downcase" | "ascii_upcase", 0) => match v {
            Value::String(s) => emit(cb, Value::String(if name == "ascii_downcase" { s.to_ascii_lowercase() } else { s.to_ascii_uppercase() })),
            _ => err(format!("{} input must be a string", name)),
        },
        ("explode", 0) => match v {
            Value::String(s) => emit(cb, Value::Array(s.chars().map(|c| Value::Int(c as i64)).collect())),
            _ => err("explode input must be a string"),
        },
        ("implode", 0) => match v {
            Value::Array(a) => {
                let mut s = String::new();
                for x in a {
                    let n = as_f64(x).ok_or_else(|| Ctl::Err(Value::from("Unicode codepoint must be numeric")))?;
                    s.push(char::from_u32(n as u32).unwrap_or('\u{FFFD}'));
                }
                emit(cb, Value::String(s))
            }
            _ => err("implode input must be an array"),
        },
        ("indices", 1) => me.with_args(args, env, v, &mut |a| emit(cb, indices(v, &a[0])?)),
        ("index" | "rindex", 1) => me.with_args(args, env, v, &mut |a| {
            let r = indices(v, &a[0])?;
            emit(
                cb,
                match r {
                    Value::Array(items) => {
                        if name == "index" {
                            items.first().cloned().unwrap_or(Value::Null)
                        } else {
                            items.last().cloned().unwrap_or(Value::Null)
                        }
                    }
                    other => other,
                },
            )
        }),
        ("infinite", 0) => emit(cb, Value::Float(f64::INFINITY)),
        ("nan", 0) => emit(cb, Value::Float(f64::NAN)),
        ("isinfinite", 0) => emit(cb, Value::Bool(need_num(v, "")?.is_infinite())),
        ("isnan", 0) => emit(cb, Value::Bool(need_num(v, "")?.is_nan())),
        ("isnormal", 0) => emit(cb, Value::Bool(need_num(v, "")?.is_normal())),
        ("abs", 0) => match v {
            Value::Int(i) => emit(cb, i.checked_abs().map(Value::Int).unwrap_or_else(|| num((*i as f64).abs()))),
            Value::Float(f) => emit(cb, num(f.abs())),
            other => err(format!("{} ({}) has no absolute value", tname(other), dump_trunc(other))),
        },
        ("frexp", 0) => {
            let x = need_num(v, "frexp")?;
            let (m, e) = if x == 0.0 || !x.is_finite() {
                (x, 0)
            } else {
                let e = x.abs().log2().floor() as i64 + 1;
                (x / 2f64.powi(e as i32), e)
            };
            emit(cb, Value::Array(vec![num(m), Value::Int(e)]))
        }
        ("modf", 0) => {
            let x = need_num(v, "modf")?;
            emit(cb, Value::Array(vec![num(x.fract()), num(x.trunc())]))
        }
        ("significand" | "logb" | "gamma" | "lgamma" | "tgamma" | "nearbyint", 0) => emit(cb, num(math1(name, need_num(v, name)?))),
        ("pow" | "atan2" | "fmin" | "fmax" | "fmod" | "ldexp" | "scalb" | "scalbln" | "hypot" | "copysign" | "drem", 2) => {
            me.with_args(args, env, v, &mut |a| emit(cb, num(math2(name, need_num(&a[0], name)?, need_num(&a[1], name)?))))
        }
        ("now", 0) => {
            let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
            emit(cb, Value::Float(d))
        }
        ("mktime", 0) => {
            if !matches!(v, Value::Array(_)) {
                return err("mktime requires array of 6 numbers");
            }
            let tm = tm_fields(v, "mktime")?;
            emit(cb, Value::Int(secs_from_tm(&tm).floor() as i64))
        }
        ("gmtime", 0) => {
            let s = as_f64(v).ok_or_else(|| Ctl::Err(Value::from("gmtime() requires a number")))?;
            emit(cb, tm_from_secs(s, 0))
        }
        ("localtime", 0) => {
            let s = as_f64(v).ok_or_else(|| Ctl::Err(Value::from("localtime() requires a number")))?;
            emit(cb, tm_from_secs(s, local_offset(s).0))
        }
        ("strftime" | "strflocaltime", 1) => me.with_args(args, env, v, &mut |a| {
            let fmt = match &a[0] {
                Value::String(s) => s.clone(),
                _ => return err(format!("{}/1 requires a string format", name)),
            };
            let local = name == "strflocaltime";
            let (tm, zone, off) = match v {
                Value::Int(_) | Value::Float(_) => {
                    let s = as_f64(v).unwrap();
                    if local {
                        let (off, abbr) = local_offset(s);
                        (tm_fields(&tm_from_secs(s, off), name)?, abbr, off)
                    } else {
                        (tm_fields(&tm_from_secs(s, 0), name)?, "UTC".to_string(), 0)
                    }
                }
                Value::Array(_) => {
                    let tm = tm_fields(v, &format!("{}/1", name))?;
                    if local {
                        let (off, abbr) = local_offset(secs_from_tm(&tm));
                        (tm, abbr, off)
                    } else {
                        (tm, "UTC".to_string(), 0)
                    }
                }
                _ => return err(format!("{}/1 requires parsed datetime inputs", name)),
            };
            emit(cb, Value::String(strftime(&tm, &fmt, &zone, off)))
        }),
        ("strptime", 1) => me.with_args(args, env, v, &mut |a| match (v, &a[0]) {
            (Value::String(s), Value::String(f)) => match strptime(s, f) {
                Some(tm) => emit(cb, tm),
                None => err(format!("date \"{}\" does not match format \"{}\"", s, f)),
            },
            _ => err("strptime/1 requires string inputs and arguments"),
        }),
        ("format", 1) => me.with_args(args, env, v, &mut |a| match &a[0] {
            Value::String(f) => emit(cb, Value::String(format(f, v)?)),
            other => err(format!("{} ({}) is not a valid format", tname(other), dump_trunc(other))),
        }),
        ("header" | "property" | "variable", 1 | 2) => me.with_args(args, env, v, &mut |a| {
            let key = tostring(&a[0]);
            let found = me.ex.and_then(|ex| match name {
                "header" => ex.header(&key).cloned(),
                "property" => ex.property(&key).cloned(),
                _ => ex.property(&format!("CamelVariable.{}", key)).cloned(),
            });
            let found = found.map(|x| normalize(x));
            emit(cb, found.filter(|x| !x.is_null()).unwrap_or_else(|| a.get(1).cloned().unwrap_or(Value::Null)))
        }),
        (other, n) => err(format!("{}/{} is not defined", other, n)),
    }
}

/// Items of an array (or object values) for the sorting functions.
fn iter_items(v: &Value, what: &str) -> Res<Vec<Value>> {
    match v {
        Value::Array(a) => Ok(a.clone()),
        other => err(format!("{} ({}) cannot be {}ed, as it is not an array", tname(other), dump_trunc(other), what.trim_end_matches('e'))),
    }
}

/// Convert parser output (which may carry floats for integers) into jq numbers.
pub(crate) fn normalize(v: Value) -> Value {
    match v {
        Value::Float(f) => num(f),
        Value::Bytes(b) => Value::String(String::from_utf8_lossy(&b).into_owned()),
        Value::Array(a) => Value::Array(a.into_iter().map(normalize).collect()),
        Value::Object(o) => {
            let mut m = ObjectMap::with_capacity(o.len());
            for (k, x) in o {
                m.insert(k, normalize(x));
            }
            Value::Object(m)
        }
        other => other,
    }
}

fn range(from: &Value, upto: &Value, by: &Value, cb: Cb<'_>) -> R {
    let (f, u, b) = (need_num(from, "range")?, need_num(upto, "range")?, need_num(by, "range")?);
    let all_int = matches!((from, by), (Value::Int(_), Value::Int(_)));
    if b > 0.0 {
        let mut x = f;
        let mut k = 0i64;
        while x < u {
            emit(cb, if all_int { Value::Int(x as i64) } else { num(x) })?;
            k += 1;
            x = f + b * k as f64;
        }
    } else if b < 0.0 {
        let mut x = f;
        let mut k = 0i64;
        while x > u {
            emit(cb, if all_int { Value::Int(x as i64) } else { num(x) })?;
            k += 1;
            x = f + b * k as f64;
        }
    } else if f < u {
        loop {
            emit(cb, num(f))?;
        }
    }
    Ok(())
}

fn recurse<'a>(me: &Interp<'_>, f: &'a Ast, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
    cb(inp.clone())?;
    me.eval(f, env, inp, &mut |x| recurse(me, f, env, x, cb))
}

fn outputs<'a>(me: &Interp<'_>, f: &'a Ast, env: &Env<'a>, inp: Pv) -> Res<Vec<Pv>> {
    let mut out = Vec::new();
    me.eval(f, env, inp, &mut |x| {
        out.push(x);
        Ok(())
    })?;
    Ok(out)
}

fn until<'a>(me: &Interp<'_>, cond: &'a Ast, upd: &'a Ast, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
    let mut state = inp;
    loop {
        let cs = outputs(me, cond, env, Pv::val(state.v.clone()))?;
        if cs.len() != 1 {
            for c in cs {
                if truthy(&c.v) {
                    cb(state.clone())?;
                } else {
                    for o in outputs(me, upd, env, state.clone())? {
                        until(me, cond, upd, env, o, cb)?;
                    }
                }
            }
            return Ok(());
        }
        if truthy(&cs[0].v) {
            return cb(state);
        }
        let mut os = outputs(me, upd, env, state)?;
        if os.len() == 1 {
            state = os.pop().unwrap();
            continue;
        }
        for o in os {
            until(me, cond, upd, env, o, cb)?;
        }
        return Ok(());
    }
}

fn while_<'a>(me: &Interp<'_>, cond: &'a Ast, upd: &'a Ast, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
    let mut state = inp;
    loop {
        let cs = outputs(me, cond, env, Pv::val(state.v.clone()))?;
        let mut next = None;
        for c in cs {
            if !truthy(&c.v) {
                continue;
            }
            cb(state.clone())?;
            let mut os = outputs(me, upd, env, state.clone())?;
            if os.len() == 1 && next.is_none() {
                next = os.pop();
            } else {
                for o in os {
                    while_(me, cond, upd, env, o, cb)?;
                }
            }
        }
        match next {
            Some(n) => state = n,
            None => return Ok(()),
        }
    }
}

fn repeat<'a>(me: &Interp<'_>, f: &'a Ast, env: &Env<'a>, inp: Pv, cb: Cb<'_>) -> R {
    let mut state = inp;
    loop {
        cb(state.clone())?;
        let mut os = outputs(me, f, env, state)?;
        if os.len() == 1 {
            state = os.pop().unwrap();
            continue;
        }
        for o in os {
            repeat(me, f, env, o, cb)?;
        }
        return Ok(());
    }
}

fn sub<'a>(me: &Interp<'_>, args: &'a [Ast], env: &Env<'a>, v: &Value, cb: Cb<'_>) -> R {
    let s = match v {
        Value::String(s) => s.clone(),
        other => return err(format!("{} ({}) cannot be matched, as it is not a string", tname(other), dump_trunc(other))),
    };
    let res = me.values(&args[0], env, v)?;
    let flags = me.values(&args[2], env, v)?;
    for re in &res {
        for fl in &flags {
            let rx = compile_rx(re, fl)?;
            let caps = match_spans(&s, &rx);
            let mut results = vec![String::new()];
            let mut prev = 0usize;
            for c in &caps {
                let (ms, me2) = c.span(0).unwrap();
                let seg = &s[prev..ms];
                let reps = me.values(&args[1], env, &capture_object(&rx, c))?;
                let mut next = Vec::new();
                for r in &results {
                    for rep in &reps {
                        let rep = match rep {
                            Value::String(x) => x.clone(),
                            other => return err(format!("string ({}) and {} ({}) cannot be added", dump_trunc(&Value::String(seg.to_string())), tname(other), dump_trunc(other))),
                        };
                        next.push(format!("{}{}{}", r, seg, rep));
                    }
                }
                results = next;
                prev = me2;
            }
            for r in results {
                emit(cb, Value::String(format!("{}{}", r, &s[prev..])))?;
            }
        }
    }
    Ok(())
}
