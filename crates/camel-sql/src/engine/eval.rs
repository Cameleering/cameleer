//! Expression evaluation, value coercion and comparison.

use std::cmp::Ordering;

use camel_core::value::{ObjectMap, Value};

use super::ast::{is_aggregate, BinOp, DataType, Expr};
use super::exec::{run_select, Database};
use super::{SqlErr, SqlResult};

/// A column visible to expressions.
#[derive(Clone, Debug)]
pub struct ScopeCol {
    /// Table name or alias (as written).
    pub table: Option<String>,
    pub name: String,
    pub ty: Option<DataType>,
}

/// The columns of the current row.
#[derive(Clone, Debug, Default)]
pub struct Scope {
    pub cols: Vec<ScopeCol>,
}

impl Scope {
    /// Find a column; `Ok(None)` if not found, `Err` if ambiguous.
    pub fn resolve(&self, table: Option<&str>, name: &str) -> SqlResult<Option<usize>> {
        let mut found = None;
        for (i, c) in self.cols.iter().enumerate() {
            if !c.name.eq_ignore_ascii_case(name) {
                continue;
            }
            if let Some(t) = table {
                if !c.table.as_deref().is_some_and(|ct| ct.eq_ignore_ascii_case(t)) {
                    continue;
                }
            }
            if found.is_some() {
                return Err(SqlErr::new("42702", format!("column reference \"{}\" is ambiguous", name)));
            }
            found = Some(i);
        }
        Ok(found)
    }

    pub fn has_table(&self, table: &str) -> bool {
        self.cols.iter().any(|c| c.table.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(table)))
    }
}

/// Statement parameters.
pub struct Params<'a> {
    pub positional: &'a [Value],
    pub named: Option<&'a ObjectMap>,
}

/// Evaluation environment.
pub struct Env<'a> {
    pub scope: &'a Scope,
    pub row: &'a [Value],
    /// Rows of the current group (aggregate queries).
    pub group: Option<&'a [Vec<Value>]>,
    pub params: &'a Params<'a>,
    pub db: &'a Database,
    pub outer: Option<&'a Env<'a>>,
    /// Statement timestamp (`NOW()`).
    pub now: &'a str,
}

impl<'a> Env<'a> {
    fn with_row<'b>(&'b self, row: &'b [Value]) -> Env<'b>
    where
        'a: 'b,
    {
        Env { scope: self.scope, row, group: None, params: self.params, db: self.db, outer: self.outer, now: self.now }
    }
}

// ---------------------------------------------------------------------------
// value helpers
// ---------------------------------------------------------------------------

/// Text rendering used by `||`, casts to text and the wire protocol.
pub fn text_of(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Float(f) => format!("{}", f),
        Value::String(s) => s.clone(),
        Value::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        other => other.to_json(),
    }
}

fn type_label(v: &Value) -> &'static str {
    match v {
        Value::Null => "unknown",
        Value::Bool(_) => "boolean",
        Value::Int(_) => "integer",
        Value::Float(_) => "double precision",
        Value::String(_) => "text",
        Value::Bytes(_) => "bytea",
        Value::Array(_) => "array",
        Value::Object(_) => "json",
    }
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().to_ascii_lowercase().as_str() {
        "t" | "true" | "y" | "yes" | "on" | "1" => Some(true),
        "f" | "false" | "n" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

/// A number from a value (strings are parsed).
pub fn num_of(v: &Value) -> Option<Value> {
    match v {
        Value::Int(_) | Value::Float(_) => Some(v.clone()),
        Value::String(s) => {
            let t = s.trim();
            if let Ok(i) = t.parse::<i64>() {
                Some(Value::Int(i))
            } else {
                t.parse::<f64>().ok().filter(|f| f.is_finite()).map(Value::Float)
            }
        }
        _ => None,
    }
}

/// An integer from a value.
pub fn int_of(v: &Value, what: &str) -> SqlResult<i64> {
    match num_of(v) {
        Some(Value::Int(i)) => Ok(i),
        Some(Value::Float(f)) => Ok(f.round() as i64),
        _ => Err(SqlErr::new("22P02", format!("invalid input syntax for type integer in {}: \"{}\"", what, text_of(v)))),
    }
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Int(i) => *i as f64,
        Value::Float(f) => *f,
        _ => 0.0,
    }
}

/// SQL comparison; `None` if either side is NULL.
pub fn compare(a: &Value, b: &Value) -> SqlResult<Option<Ordering>> {
    use Value::*;
    Ok(Some(match (a, b) {
        (Null, _) | (_, Null) => return Ok(None),
        (Int(x), Int(y)) => x.cmp(y),
        (Int(_) | Float(_), Int(_) | Float(_)) => as_f64(a).partial_cmp(&as_f64(b)).unwrap_or(Ordering::Equal),
        (Bool(x), Bool(y)) => x.cmp(y),
        (String(x), String(y)) => x.cmp(y),
        (Bytes(x), Bytes(y)) => x.cmp(y),
        (String(x), Bytes(y)) => x.as_bytes().cmp(y.as_slice()),
        (Bytes(x), String(y)) => x.as_slice().cmp(y.as_bytes()),
        (Bool(x), String(s)) => match parse_bool(s) {
            Some(y) => x.cmp(&y),
            None => return Err(SqlErr::new("22P02", format!("invalid input syntax for type boolean: \"{}\"", s))),
        },
        (String(_), Bool(_)) => return compare(b, a).map(|o| o.map(|o| o.reverse())),
        (Bool(x), Int(i)) | (Int(i), Bool(x)) => (*x as i64).cmp(i),
        (Int(_) | Float(_), String(s)) => match num_of(b) {
            Some(n) => return compare(a, &n),
            None => return Err(SqlErr::new("22P02", format!("invalid input syntax for type {}: \"{}\"", type_label(a), s))),
        },
        (String(_), Int(_) | Float(_)) => return compare(b, a).map(|o| o.map(|o| o.reverse())),
        _ => text_of(a).cmp(&text_of(b)),
    }))
}

/// Equality used for grouping / DISTINCT / UNION (NULLs are equal).
pub fn group_key(row: &[Value]) -> String {
    let mut s = String::new();
    for v in row {
        match v {
            Value::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => s.push_str(&format!("I{}", *f as i64)),
            Value::Int(i) => s.push_str(&format!("I{}", i)),
            other => s.push_str(&format!("{:?}", other)),
        }
        s.push('\u{1}');
    }
    s
}

fn arith(op: BinOp, a: &Value, b: &Value) -> SqlResult<Value> {
    if a.is_null() || b.is_null() {
        return Ok(Value::Null);
    }
    let (x, y) = match (num_of(a), num_of(b)) {
        (Some(x), Some(y)) => (x, y),
        _ => {
            let sym = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                _ => "%",
            };
            return Err(SqlErr::new("42883", format!("operator does not exist: {} {} {}", type_label(a), sym, type_label(b))));
        }
    };
    let overflow = || SqlErr::new("22003", "bigint out of range");
    match (x, y) {
        (Value::Int(x), Value::Int(y)) => Ok(Value::Int(match op {
            BinOp::Add => x.checked_add(y).ok_or_else(overflow)?,
            BinOp::Sub => x.checked_sub(y).ok_or_else(overflow)?,
            BinOp::Mul => x.checked_mul(y).ok_or_else(overflow)?,
            BinOp::Div => {
                if y == 0 {
                    return Err(SqlErr::new("22012", "division by zero"));
                }
                x.wrapping_div(y)
            }
            _ => {
                if y == 0 {
                    return Err(SqlErr::new("22012", "division by zero"));
                }
                x.wrapping_rem(y)
            }
        })),
        (x, y) => {
            let (x, y) = (as_f64(&x), as_f64(&y));
            Ok(Value::Float(match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Div => {
                    if y == 0.0 {
                        return Err(SqlErr::new("22012", "division by zero"));
                    }
                    x / y
                }
                _ => {
                    if y == 0.0 {
                        return Err(SqlErr::new("22012", "division by zero"));
                    }
                    x % y
                }
            }))
        }
    }
}

/// Three-valued truth of a value (`None` = NULL).
pub fn truth(v: &Value) -> SqlResult<Option<bool>> {
    match v {
        Value::Null => Ok(None),
        Value::Bool(b) => Ok(Some(*b)),
        Value::Int(i) => Ok(Some(*i != 0)),
        Value::String(s) => parse_bool(s).map(Some).ok_or_else(|| SqlErr::new("22P02", format!("invalid input syntax for type boolean: \"{}\"", s))),
        other => Err(SqlErr::new("42804", format!("argument must be of type boolean, not type {}", type_label(other)))),
    }
}

fn tri(b: Option<bool>) -> Value {
    b.map(Value::Bool).unwrap_or(Value::Null)
}

/// Normalize a timestamp text (`2024-01-02T03:04:05Z` → `2024-01-02 03:04:05`).
fn normalize_timestamp(s: &str, date_only: bool) -> SqlResult<String> {
    let t = s.trim();
    let bad = || SqlErr::new("22007", format!("invalid input syntax for type {}: \"{}\"", if date_only { "date" } else { "timestamp" }, s));
    let b = t.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' || !b[..4].iter().all(|c| c.is_ascii_digit()) {
        return Err(bad());
    }
    let month: u32 = t[5..7].parse().map_err(|_| bad())?;
    let day: u32 = t[8..10].parse().map_err(|_| bad())?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return Err(SqlErr::new("22008", format!("date/time field value out of range: \"{}\"", s)));
    }
    if date_only {
        return Ok(t[..10].to_string());
    }
    if b.len() == 10 {
        return Ok(format!("{} 00:00:00", t));
    }
    let mut out = t.replacen('T', " ", 1);
    if let Some(stripped) = out.strip_suffix('Z') {
        out = stripped.to_string();
    }
    if let Some(stripped) = out.strip_suffix("+00:00").or_else(|| out.strip_suffix("+00")) {
        out = stripped.to_string();
    }
    Ok(out)
}

fn decode_hex_bytea(s: &str) -> Option<Vec<u8>> {
    let h = s.strip_prefix("\\x")?;
    if h.len() % 2 != 0 {
        return None;
    }
    (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).ok()).collect()
}

/// Coerce a value to a column type (assignment / CAST).
pub fn coerce(v: Value, ty: &DataType) -> SqlResult<Value> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    let mismatch = |v: &Value| SqlErr::new("42804", format!("value of type {} cannot be converted to type {}", type_label(v), ty.sql_name()));
    match ty {
        DataType::Integer | DataType::BigInt => {
            let i = match &v {
                Value::Int(i) => *i,
                Value::Float(f) => f.round() as i64,
                Value::Bool(b) => *b as i64,
                Value::String(s) => match num_of(&v) {
                    Some(Value::Int(i)) => i,
                    Some(Value::Float(f)) => f.round() as i64,
                    _ => return Err(SqlErr::new("22P02", format!("invalid input syntax for type {}: \"{}\"", ty.sql_name(), s))),
                },
                other => return Err(mismatch(other)),
            };
            if *ty == DataType::Integer && (i > i32::MAX as i64 || i < i32::MIN as i64) {
                return Err(SqlErr::new("22003", "integer out of range"));
            }
            Ok(Value::Int(i))
        }
        DataType::Varchar(max) => {
            let s = match &v {
                Value::Array(_) | Value::Object(_) => v.to_json(),
                other => text_of(other),
            };
            if let Some(n) = max {
                if s.chars().count() > *n {
                    return Err(SqlErr::new("22001", format!("value too long for type character varying({})", n)));
                }
            }
            Ok(Value::String(s))
        }
        DataType::Text => Ok(Value::String(match &v {
            Value::Array(_) | Value::Object(_) => v.to_json(),
            other => text_of(other),
        })),
        DataType::Boolean => match &v {
            Value::Bool(_) => Ok(v),
            Value::Int(i) => Ok(Value::Bool(*i != 0)),
            Value::String(s) => parse_bool(s).map(Value::Bool).ok_or_else(|| SqlErr::new("22P02", format!("invalid input syntax for type boolean: \"{}\"", s))),
            other => Err(mismatch(other)),
        },
        DataType::Double | DataType::Decimal(..) => {
            let f = match &v {
                Value::Int(i) => *i as f64,
                Value::Float(f) => *f,
                Value::String(s) => match num_of(&v) {
                    Some(n) => as_f64(&n),
                    None => return Err(SqlErr::new("22P02", format!("invalid input syntax for type {}: \"{}\"", ty.sql_name(), s))),
                },
                other => return Err(mismatch(other)),
            };
            if let DataType::Decimal(_, Some(scale)) = ty {
                let m = 10f64.powi(*scale as i32);
                return Ok(Value::Float((f * m).round() / m));
            }
            Ok(Value::Float(f))
        }
        DataType::Timestamp | DataType::Date => {
            let date_only = *ty == DataType::Date;
            match &v {
                Value::String(s) => Ok(Value::String(normalize_timestamp(s, date_only)?)),
                Value::Int(ms) => {
                    let pat = if date_only { "yyyy-MM-dd" } else { "yyyy-MM-dd HH:mm:ss.SSS" };
                    Ok(Value::String(camel_core::util_time::format_millis(*ms, pat)))
                }
                other => Err(mismatch(other)),
            }
        }
        DataType::Bytes => match v {
            Value::Bytes(_) => Ok(v),
            Value::String(s) => Ok(Value::Bytes(decode_hex_bytea(&s).unwrap_or_else(|| s.into_bytes()))),
            other => Ok(Value::Bytes(text_of(&other).into_bytes())),
        },
    }
}

/// SQL LIKE matching with `%`, `_` and an escape character.
pub fn like_match(text: &str, pattern: &str, escape: Option<char>, ci: bool) -> bool {
    enum P {
        Any,
        One,
        Lit(char),
    }
    let norm = |s: &str| if ci { s.to_lowercase() } else { s.to_string() };
    let pchars: Vec<char> = norm(pattern).chars().collect();
    let mut pat = Vec::new();
    let mut i = 0;
    while i < pchars.len() {
        let c = pchars[i];
        if Some(c) == escape && i + 1 < pchars.len() {
            pat.push(P::Lit(pchars[i + 1]));
            i += 2;
            continue;
        }
        pat.push(match c {
            '%' => P::Any,
            '_' => P::One,
            c => P::Lit(c),
        });
        i += 1;
    }
    let t: Vec<char> = norm(text).chars().collect();
    let (mut ti, mut pi) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < pat.len() {
            match pat[pi] {
                P::Any => {
                    star = Some(pi);
                    mark = ti;
                    pi += 1;
                    continue;
                }
                P::One => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                P::Lit(c) if c == t[ti] => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some(s) => {
                pi = s + 1;
                mark += 1;
                ti = mark;
            }
            None => return false,
        }
    }
    while pi < pat.len() && matches!(pat[pi], P::Any) {
        pi += 1;
    }
    pi == pat.len()
}

// ---------------------------------------------------------------------------
// evaluation
// ---------------------------------------------------------------------------

fn lookup_column(env: &Env, table: Option<&str>, name: &str) -> SqlResult<Value> {
    let mut e = Some(env);
    while let Some(cur) = e {
        if let Some(i) = cur.scope.resolve(table, name)? {
            return Ok(cur.row.get(i).cloned().unwrap_or(Value::Null));
        }
        e = cur.outer;
    }
    if let Some(t) = table {
        let mut known = false;
        let mut e = Some(env);
        while let Some(cur) = e {
            known |= cur.scope.has_table(t);
            e = cur.outer;
        }
        if !known {
            return Err(SqlErr::new("42P01", format!("missing FROM-clause entry for table \"{}\"", t)));
        }
        return Err(SqlErr::new("42703", format!("column {}.{} does not exist", t, name)));
    }
    Err(SqlErr::new("42703", format!("column \"{}\" does not exist", name)))
}

/// Evaluate an expression.
pub fn eval(expr: &Expr, env: &Env) -> SqlResult<Value> {
    match expr {
        Expr::Literal(v) => Ok(v.clone()),
        Expr::Column { table, name } => lookup_column(env, table.as_deref(), name),
        Expr::Param(i) => env
            .params
            .positional
            .get(*i)
            .cloned()
            .ok_or_else(|| SqlErr::new("07001", format!("No value specified for parameter {}", i + 1))),
        Expr::Named(n) => match env.params.named.and_then(|m| m.get(n).or_else(|| m.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v))) {
            Some(v) => Ok(v.clone()),
            None => Err(SqlErr::new("07001", format!("No value specified for parameter :{}", n))),
        },
        Expr::Neg(e) => {
            let v = eval(e, env)?;
            arith(BinOp::Sub, &Value::Int(0), &v)
        }
        Expr::Not(e) => Ok(tri(truth(&eval(e, env)?)?.map(|b| !b))),
        Expr::Binary { op, left, right } => match op {
            BinOp::And => {
                let l = truth(&eval(left, env)?)?;
                if l == Some(false) {
                    return Ok(Value::Bool(false));
                }
                let r = truth(&eval(right, env)?)?;
                Ok(tri(match (l, r) {
                    (_, Some(false)) => Some(false),
                    (Some(true), Some(true)) => Some(true),
                    _ => None,
                }))
            }
            BinOp::Or => {
                let l = truth(&eval(left, env)?)?;
                if l == Some(true) {
                    return Ok(Value::Bool(true));
                }
                let r = truth(&eval(right, env)?)?;
                Ok(tri(match (l, r) {
                    (_, Some(true)) => Some(true),
                    (Some(false), Some(false)) => Some(false),
                    _ => None,
                }))
            }
            BinOp::Concat => {
                let (l, r) = (eval(left, env)?, eval(right, env)?);
                if l.is_null() || r.is_null() {
                    return Ok(Value::Null);
                }
                Ok(Value::String(format!("{}{}", text_of(&l), text_of(&r))))
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod => arith(*op, &eval(left, env)?, &eval(right, env)?),
            cmp => {
                let o = compare(&eval(left, env)?, &eval(right, env)?)?;
                Ok(tri(o.map(|o| match cmp {
                    BinOp::Eq => o == Ordering::Equal,
                    BinOp::NotEq => o != Ordering::Equal,
                    BinOp::Lt => o == Ordering::Less,
                    BinOp::LtEq => o != Ordering::Greater,
                    BinOp::Gt => o == Ordering::Greater,
                    _ => o != Ordering::Less,
                })))
            }
        },
        Expr::IsNull { expr, negated } => Ok(Value::Bool(eval(expr, env)?.is_null() != *negated)),
        Expr::IsBool { expr, value, negated } => {
            let t = truth(&eval(expr, env)?)?;
            Ok(Value::Bool((t == Some(*value)) != *negated))
        }
        Expr::Like { expr, pattern, negated, case_insensitive, escape } => {
            let (v, p) = (eval(expr, env)?, eval(pattern, env)?);
            if v.is_null() || p.is_null() {
                return Ok(Value::Null);
            }
            Ok(Value::Bool(like_match(&text_of(&v), &text_of(&p), *escape, *case_insensitive) != *negated))
        }
        Expr::InList { expr, list, negated } => {
            let v = eval(expr, env)?;
            let mut items = Vec::with_capacity(list.len());
            for e in list {
                let x = eval(e, env)?;
                // a parameter bound to an array expands into its elements
                match x {
                    Value::Array(a) => items.extend(a),
                    x => items.push(x),
                }
            }
            in_values(&v, &items, *negated)
        }
        Expr::InSubquery { expr, query, negated } => {
            let v = eval(expr, env)?;
            let rel = run_select(env.db, query, env.params, Some(env), env.now)?;
            if rel.columns.len() != 1 {
                return Err(SqlErr::new("42601", "subquery has too many columns"));
            }
            let items: Vec<Value> = rel.rows.into_iter().map(|mut r| r.remove(0)).collect();
            in_values(&v, &items, *negated)
        }
        Expr::Between { expr, low, high, negated } => {
            let v = eval(expr, env)?;
            let (lo, hi) = (eval(low, env)?, eval(high, env)?);
            let a = compare(&v, &lo)?.map(|o| o != Ordering::Less);
            let b = compare(&v, &hi)?.map(|o| o != Ordering::Greater);
            let r = match (a, b) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            };
            Ok(tri(r.map(|x| x != *negated)))
        }
        Expr::Case { operand, whens, else_ } => {
            let op = match operand {
                Some(o) => Some(eval(o, env)?),
                None => None,
            };
            for (c, v) in whens {
                let hit = match &op {
                    Some(o) => compare(o, &eval(c, env)?)? == Some(Ordering::Equal),
                    None => truth(&eval(c, env)?)? == Some(true),
                };
                if hit {
                    return eval(v, env);
                }
            }
            match else_ {
                Some(e) => eval(e, env),
                None => Ok(Value::Null),
            }
        }
        Expr::Cast { expr, ty } => {
            let v = eval(expr, env)?;
            match (ty, &v) {
                (DataType::Integer | DataType::BigInt, Value::Bool(b)) => Ok(Value::Int(*b as i64)),
                (DataType::Integer | DataType::BigInt, Value::Float(f)) => Ok(Value::Int(f.round() as i64)),
                (DataType::Varchar(_) | DataType::Text, Value::Float(_)) => Ok(Value::String(text_of(&v))),
                _ => coerce(v, ty),
            }
        }
        Expr::Exists(q) => {
            let rel = run_select(env.db, q, env.params, Some(env), env.now)?;
            Ok(Value::Bool(!rel.rows.is_empty()))
        }
        Expr::Subquery(q) => {
            let rel = run_select(env.db, q, env.params, Some(env), env.now)?;
            if rel.columns.len() != 1 {
                return Err(SqlErr::new("42601", "subquery must return only one column"));
            }
            match rel.rows.len() {
                0 => Ok(Value::Null),
                1 => Ok(rel.rows[0][0].clone()),
                _ => Err(SqlErr::new("21000", "more than one row returned by a subquery used as an expression")),
            }
        }
        Expr::Function { name, args, distinct, star } => {
            if is_aggregate(name) {
                return eval_aggregate(name, args, *distinct, *star, env);
            }
            let mut vals = Vec::with_capacity(args.len());
            for a in args {
                vals.push(eval(a, env)?);
            }
            if let Some(r) = env.db.routines.get(&name.to_ascii_lowercase()) {
                if !r.function {
                    return Err(SqlErr::new("42809", format!("{}() is a procedure (use CALL)", r.name)));
                }
                let out = super::plsql::call_routine(&mut super::plsql::DbCtx::Ref(env.db), r, vals, false, env.now)?;
                return Ok(out.scalar);
            }
            call_function(name, vals, env)
        }
    }
}

fn in_values(v: &Value, items: &[Value], negated: bool) -> SqlResult<Value> {
    if v.is_null() {
        return Ok(Value::Null);
    }
    let mut saw_null = false;
    for x in items {
        match compare(v, x)? {
            Some(Ordering::Equal) => return Ok(Value::Bool(!negated)),
            None => saw_null = true,
            _ => {}
        }
    }
    if saw_null {
        Ok(Value::Null)
    } else {
        Ok(Value::Bool(negated))
    }
}

fn eval_aggregate(name: &str, args: &[Expr], distinct: bool, star: bool, env: &Env) -> SqlResult<Value> {
    let group = env.group.ok_or_else(|| SqlErr::new("42803", format!("aggregate function {} is not allowed here", name.to_lowercase())))?;
    if star {
        return Ok(Value::Int(group.len() as i64));
    }
    let arg = args.first().ok_or_else(|| SqlErr::new("42883", format!("function {}() requires an argument", name.to_lowercase())))?;
    let mut vals = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for row in group {
        let v = eval(arg, &env.with_row(row))?;
        if v.is_null() {
            continue;
        }
        if distinct && !seen.insert(group_key(std::slice::from_ref(&v))) {
            continue;
        }
        vals.push(v);
    }
    match name {
        "COUNT" => Ok(Value::Int(vals.len() as i64)),
        "SUM" | "AVG" => {
            if vals.is_empty() {
                return Ok(Value::Null);
            }
            let mut acc = Value::Int(0);
            for v in &vals {
                let n = num_of(v).ok_or_else(|| SqlErr::new("42883", format!("function {}({}) does not exist", name.to_lowercase(), type_label(v))))?;
                acc = arith(BinOp::Add, &acc, &n)?;
            }
            if name == "AVG" {
                Ok(Value::Float(as_f64(&acc) / vals.len() as f64))
            } else {
                Ok(acc)
            }
        }
        "MIN" | "MAX" => {
            let mut best: Option<Value> = None;
            for v in vals {
                best = Some(match best {
                    None => v,
                    Some(b) => {
                        let o = compare(&v, &b)?.unwrap_or(Ordering::Equal);
                        if (name == "MIN" && o == Ordering::Less) || (name == "MAX" && o == Ordering::Greater) {
                            v
                        } else {
                            b
                        }
                    }
                });
            }
            Ok(best.unwrap_or(Value::Null))
        }
        "BOOL_AND" | "EVERY" | "BOOL_OR" => {
            if vals.is_empty() {
                return Ok(Value::Null);
            }
            let mut r = name != "BOOL_OR";
            for v in &vals {
                let b = truth(v)?.unwrap_or(false);
                if name == "BOOL_OR" {
                    r |= b;
                } else {
                    r &= b;
                }
            }
            Ok(Value::Bool(r))
        }
        _ => {
            // STRING_AGG(expr, sep) / GROUP_CONCAT(expr)
            if vals.is_empty() {
                return Ok(Value::Null);
            }
            let sep = match args.get(1) {
                Some(e) => text_of(&eval(e, &env.with_row(group.first().map(|r| r.as_slice()).unwrap_or(&[])))?),
                None => ",".to_string(),
            };
            Ok(Value::String(vals.iter().map(text_of).collect::<Vec<_>>().join(&sep)))
        }
    }
}

fn arity(name: &str, vals: &[Value], min: usize, max: usize) -> SqlResult<()> {
    if vals.len() < min || vals.len() > max {
        return Err(SqlErr::new("42883", format!("function {} does not accept {} argument(s)", name.to_lowercase(), vals.len())));
    }
    Ok(())
}

fn call_function(name: &str, vals: Vec<Value>, env: &Env) -> SqlResult<Value> {
    let null_in = vals.iter().any(|v| v.is_null());
    let s0 = || vals.first().map(text_of).unwrap_or_default();
    match name {
        "UPPER" | "UCASE" => {
            arity(name, &vals, 1, 1)?;
            Ok(if null_in { Value::Null } else { Value::String(s0().to_uppercase()) })
        }
        "LOWER" | "LCASE" => {
            arity(name, &vals, 1, 1)?;
            Ok(if null_in { Value::Null } else { Value::String(s0().to_lowercase()) })
        }
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" | "LEN" => {
            arity(name, &vals, 1, 1)?;
            Ok(match &vals[0] {
                Value::Null => Value::Null,
                Value::Bytes(b) => Value::Int(b.len() as i64),
                v => Value::Int(text_of(v).chars().count() as i64),
            })
        }
        "COALESCE" | "IFNULL" | "NVL" => Ok(vals.into_iter().find(|v| !v.is_null()).unwrap_or(Value::Null)),
        "NULLIF" => {
            arity(name, &vals, 2, 2)?;
            if compare(&vals[0], &vals[1])? == Some(Ordering::Equal) {
                Ok(Value::Null)
            } else {
                Ok(vals[0].clone())
            }
        }
        "NOW" | "CURRENT_TIMESTAMP" | "LOCALTIMESTAMP" | "SYSDATE" | "GETDATE" | "STATEMENT_TIMESTAMP" | "TRANSACTION_TIMESTAMP" | "CLOCK_TIMESTAMP" => {
            Ok(Value::String(env.now.to_string()))
        }
        "CURRENT_DATE" => Ok(Value::String(env.now[..10.min(env.now.len())].to_string())),
        "ABS" => {
            arity(name, &vals, 1, 1)?;
            Ok(match num_of(&vals[0]) {
                Some(Value::Int(i)) => Value::Int(i.abs()),
                Some(Value::Float(f)) => Value::Float(f.abs()),
                _ => Value::Null,
            })
        }
        "ROUND" | "TRUNC" | "TRUNCATE" | "FLOOR" | "CEIL" | "CEILING" => {
            arity(name, &vals, 1, 2)?;
            if null_in {
                return Ok(Value::Null);
            }
            let n = num_of(&vals[0]).ok_or_else(|| SqlErr::new("42883", format!("function {}({}) does not exist", name.to_lowercase(), type_label(&vals[0]))))?;
            if let Value::Int(i) = n {
                return Ok(Value::Int(i));
            }
            let f = as_f64(&n);
            let digits = match vals.get(1) {
                Some(d) => int_of(d, name)?,
                None => 0,
            };
            let m = 10f64.powi(digits as i32);
            let r = match name {
                "ROUND" => (f * m).round() / m,
                "FLOOR" => f.floor(),
                "CEIL" | "CEILING" => f.ceil(),
                _ => (f * m).trunc() / m,
            };
            Ok(Value::Float(r))
        }
        "MOD" => {
            arity(name, &vals, 2, 2)?;
            arith(BinOp::Mod, &vals[0], &vals[1])
        }
        "POWER" | "POW" => {
            arity(name, &vals, 2, 2)?;
            if null_in {
                return Ok(Value::Null);
            }
            Ok(Value::Float(as_f64(&num_of(&vals[0]).unwrap_or(Value::Float(0.0))).powf(as_f64(&num_of(&vals[1]).unwrap_or(Value::Float(0.0))))))
        }
        "SQRT" => {
            arity(name, &vals, 1, 1)?;
            Ok(if null_in { Value::Null } else { Value::Float(as_f64(&num_of(&vals[0]).unwrap_or(Value::Float(0.0))).sqrt()) })
        }
        "CONCAT" => Ok(Value::String(vals.iter().map(text_of).collect())),
        "CONCAT_WS" => {
            if vals.is_empty() {
                return Ok(Value::Null);
            }
            let sep = text_of(&vals[0]);
            Ok(Value::String(vals[1..].iter().filter(|v| !v.is_null()).map(text_of).collect::<Vec<_>>().join(&sep)))
        }
        "SUBSTRING" | "SUBSTR" => {
            arity(name, &vals, 2, 3)?;
            if null_in {
                return Ok(Value::Null);
            }
            let chars: Vec<char> = s0().chars().collect();
            let start = int_of(&vals[1], name)?;
            let len = match vals.get(2) {
                Some(l) => int_of(l, name)?,
                None => i64::MAX / 4,
            };
            if len < 0 {
                return Err(SqlErr::new("22011", "negative substring length not allowed"));
            }
            let from = start.max(1);
            let to = start.saturating_add(len).min(chars.len() as i64 + 1);
            if to <= from {
                return Ok(Value::String(String::new()));
            }
            Ok(Value::String(chars[(from - 1) as usize..(to - 1) as usize].iter().collect()))
        }
        "LEFT" | "RIGHT" => {
            arity(name, &vals, 2, 2)?;
            if null_in {
                return Ok(Value::Null);
            }
            let chars: Vec<char> = s0().chars().collect();
            let n = (int_of(&vals[1], name)?.max(0) as usize).min(chars.len());
            Ok(Value::String(if name == "LEFT" { chars[..n].iter().collect() } else { chars[chars.len() - n..].iter().collect() }))
        }
        "TRIM" | "LTRIM" | "RTRIM" | "BTRIM" => {
            arity(name, &vals, 1, 2)?;
            if null_in {
                return Ok(Value::Null);
            }
            let s = s0();
            let set: Vec<char> = vals.get(1).map(|v| text_of(v).chars().collect()).unwrap_or_else(|| vec![' ']);
            let f = |c: char| set.contains(&c);
            Ok(Value::String(match name {
                "LTRIM" => s.trim_start_matches(f).to_string(),
                "RTRIM" => s.trim_end_matches(f).to_string(),
                _ => s.trim_matches(f).to_string(),
            }))
        }
        "REPLACE" => {
            arity(name, &vals, 3, 3)?;
            if null_in {
                return Ok(Value::Null);
            }
            Ok(Value::String(s0().replace(&text_of(&vals[1]), &text_of(&vals[2]))))
        }
        "SPLIT_PART" => {
            arity(name, &vals, 3, 3)?;
            if null_in {
                return Ok(Value::Null);
            }
            let n = int_of(&vals[2], "field position")?;
            if n == 0 {
                return Err(SqlErr::new("22023", "field position must not be zero"));
            }
            let s = s0();
            let parts: Vec<&str> = s.split(text_of(&vals[1]).as_str()).collect();
            let idx = if n > 0 { n - 1 } else { parts.len() as i64 + n };
            Ok(Value::String(if idx >= 0 { parts.get(idx as usize).map(|p| p.to_string()).unwrap_or_default() } else { String::new() }))
        }
        "POSITION" | "STRPOS" | "INSTR" => {
            arity(name, &vals, 2, 2)?;
            if null_in {
                return Ok(Value::Null);
            }
            // POSITION(sub IN s) / STRPOS(s, sub) / INSTR(s, sub)
            let (sub, s) = if name == "POSITION" { (text_of(&vals[0]), text_of(&vals[1])) } else { (text_of(&vals[1]), text_of(&vals[0])) };
            Ok(Value::Int(s.find(&sub).map(|i| s[..i].chars().count() as i64 + 1).unwrap_or(0)))
        }
        "GREATEST" | "LEAST" => {
            let mut best: Option<Value> = None;
            for v in vals.into_iter().filter(|v| !v.is_null()) {
                best = Some(match best {
                    None => v,
                    Some(b) => {
                        let o = compare(&v, &b)?.unwrap_or(Ordering::Equal);
                        if (name == "GREATEST" && o == Ordering::Greater) || (name == "LEAST" && o == Ordering::Less) {
                            v
                        } else {
                            b
                        }
                    }
                });
            }
            Ok(best.unwrap_or(Value::Null))
        }
        "__NOT_DISTINCT" => {
            let (a, b) = (&vals[0], &vals[1]);
            Ok(Value::Bool(match (a.is_null(), b.is_null()) {
                (true, true) => true,
                (false, false) => compare(a, b)? == Some(Ordering::Equal),
                _ => false,
            }))
        }
        "DEFAULT" => Err(SqlErr::new("42601", "DEFAULT is not allowed in this context")),
        "VERSION" => Ok(Value::String("rustsql 0.1 (PostgreSQL-compatible subset)".into())),
        "RANDOM" | "RAND" => {
            use std::hash::{BuildHasher, Hasher};
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            Ok(Value::Float((h.finish() >> 11) as f64 / (1u64 << 53) as f64))
        }
        _ => Err(SqlErr::new("42883", format!("function {}() does not exist", name.to_lowercase()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like() {
        assert!(like_match("hello", "h%o", Some('\\'), false));
        assert!(like_match("hello", "h_llo", Some('\\'), false));
        assert!(!like_match("hello", "h_lo", Some('\\'), false));
        assert!(like_match("50%", "50\\%", Some('\\'), false));
        assert!(!like_match("500", "50\\%", Some('\\'), false));
        assert!(like_match("HeLLo", "hello", None, true));
        assert!(like_match("", "%", None, false));
        assert!(like_match("abcabc", "%bc", None, false));
    }

    #[test]
    fn coercion() {
        assert_eq!(coerce(Value::from("42"), &DataType::Integer).unwrap(), Value::Int(42));
        assert!(coerce(Value::from("x"), &DataType::Integer).is_err());
        assert!(coerce(Value::from("abcdef"), &DataType::Varchar(Some(3))).is_err());
        assert_eq!(coerce(Value::from("t"), &DataType::Boolean).unwrap(), Value::Bool(true));
        assert_eq!(coerce(Value::from("2024-01-02T03:04:05Z"), &DataType::Timestamp).unwrap(), Value::from("2024-01-02 03:04:05"));
        assert_eq!(coerce(Value::Float(1.234), &DataType::Decimal(Some(5), Some(2))).unwrap(), Value::Float(1.23));
        assert!(coerce(Value::Int(1 << 40), &DataType::Integer).is_err());
    }
}
