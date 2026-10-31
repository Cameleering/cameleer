//! MongoDB query language subset used by the embedded server: filters (`$eq $gt $gte
//! $lt $lte $ne $in $nin $and $or $nor $not $exists $regex $size $all $elemMatch $type`),
//! updates (`$set $unset $inc $mul $push $addToSet $pull $pop $rename $setOnInsert $min $max
//! $currentDate`), projections, sorting and aggregation stages (`$match $group $sort $limit
//! $skip $project $addFields/$set $unset $count $unwind`).

use std::cmp::Ordering;

use super::bson::{Bson, Document};

/// A query error (message, MongoDB error code).
#[derive(Clone, Debug, PartialEq)]
pub struct QueryError(pub String, pub i32);

type QResult<T> = std::result::Result<T, QueryError>;

fn bad(m: impl Into<String>) -> QueryError {
    QueryError(m.into(), 2)
}

/// Values reachable through a dotted path; arrays along the way are expanded
/// (numeric components index arrays).
pub fn path_values<'a>(doc: &'a Document, path: &str) -> Vec<&'a Bson> {
    fn walk<'a>(v: &'a Bson, parts: &[&str], out: &mut Vec<&'a Bson>) {
        let Some((first, rest)) = parts.split_first() else {
            out.push(v);
            return;
        };
        match v {
            Bson::Document(d) => {
                if let Some(x) = d.get(first) {
                    walk(x, rest, out);
                }
            }
            Bson::Array(a) => {
                if let Ok(i) = first.parse::<usize>() {
                    if let Some(x) = a.get(i) {
                        walk(x, rest, out);
                    }
                } else {
                    for x in a {
                        if matches!(x, Bson::Document(_)) {
                            walk(x, parts, out);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    let parts: Vec<&str> = path.split('.').collect();
    let mut out = Vec::new();
    if let Some(v) = doc.get(parts[0]) {
        walk(v, &parts[1..], &mut out);
    }
    out
}

/// The single value at a dotted path (no array expansion except numeric indexes).
pub fn get_path(doc: &Document, path: &str) -> Option<Bson> {
    let mut cur = Bson::Document(doc.clone());
    for p in path.split('.') {
        cur = match cur {
            Bson::Document(d) => d.get(p)?.clone(),
            Bson::Array(a) => match p.parse::<usize>() {
                Ok(i) => a.get(i)?.clone(),
                Err(_) => Bson::Array(a.iter().filter_map(|x| x.as_document().and_then(|d| d.get(p)).cloned()).collect()),
            },
            _ => return None,
        };
    }
    Some(cur)
}

/// Sets a value at a dotted path, creating intermediate documents.
pub fn set_path(doc: &mut Document, path: &str, value: Bson) -> QResult<()> {
    let (head, rest) = match path.split_once('.') {
        Some((h, r)) => (h, Some(r)),
        None => (path, None),
    };
    match rest {
        None => {
            doc.insert(head, value);
            Ok(())
        }
        Some(r) => {
            if !doc.contains_key(head) {
                doc.insert(head, Document::new());
            }
            match doc.get_mut(head).unwrap() {
                Bson::Document(d) => set_path(d, r, value),
                Bson::Array(a) => {
                    let (idx, rest2) = match r.split_once('.') {
                        Some((i, r2)) => (i, Some(r2)),
                        None => (r, None),
                    };
                    let i: usize = idx.parse().map_err(|_| QueryError(format!("Cannot create field '{}' in element {{{}: [...]}}", idx, head), 28))?;
                    while a.len() <= i {
                        a.push(Bson::Null);
                    }
                    match rest2 {
                        None => {
                            a[i] = value;
                            Ok(())
                        }
                        Some(r2) => {
                            if !matches!(a[i], Bson::Document(_)) {
                                a[i] = Bson::Document(Document::new());
                            }
                            match &mut a[i] {
                                Bson::Document(d) => set_path(d, r2, value),
                                _ => unreachable!(),
                            }
                        }
                    }
                }
                other => Err(QueryError(format!("Cannot create field '{}' in element {{{}: {:?}}}", r, head, other), 28)),
            }
        }
    }
}

/// Removes the value at a dotted path.
pub fn unset_path(doc: &mut Document, path: &str) -> bool {
    match path.split_once('.') {
        None => doc.remove(path).is_some(),
        Some((h, r)) => match doc.get_mut(h) {
            Some(Bson::Document(d)) => unset_path(d, r),
            Some(Bson::Array(a)) => match r.parse::<usize>() {
                Ok(i) if i < a.len() => {
                    a[i] = Bson::Null;
                    true
                }
                _ => false,
            },
            _ => false,
        },
    }
}

fn regex_match(pattern: &str, options: &str, text: &str) -> QResult<bool> {
    let re = camel_core::regex::Regex::with_flags(pattern, options.contains('i')).map_err(|e| QueryError(format!("Regular expression is invalid: {}", e.message), 51091))?;
    Ok(re.is_match(text))
}

fn value_matches_regex(v: &Bson, pattern: &str, options: &str) -> QResult<bool> {
    match v {
        Bson::String(s) | Bson::Symbol(s) => regex_match(pattern, options, s),
        Bson::Array(a) => {
            for x in a {
                if value_matches_regex(x, pattern, options)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Equality with array semantics: an array field matches when it equals the value or
/// one of its elements does.
fn eq_match(v: &Bson, target: &Bson) -> QResult<bool> {
    if let Bson::Regex(p, o) = target {
        return value_matches_regex(v, p, o);
    }
    if v.eq_bson(target) {
        return Ok(true);
    }
    if let Bson::Array(a) = v {
        return Ok(a.iter().any(|x| x.eq_bson(target)));
    }
    Ok(false)
}

fn cmp_match(v: &Bson, target: &Bson, ok: fn(Ordering) -> bool) -> bool {
    let test = |x: &Bson| {
        // comparisons only within the same type bracket (numbers with numbers, ...)
        let same = x.is_number() && target.is_number() || std::mem::discriminant(x) == std::mem::discriminant(target) || (x.as_str().is_some() && target.as_str().is_some());
        same && ok(x.cmp_bson(target))
    };
    match v {
        Bson::Array(a) => a.iter().any(test) || test(v),
        _ => test(v),
    }
}

fn type_matches(v: &Bson, t: &Bson) -> bool {
    match t {
        Bson::String(name) => name == v.type_name() || (name == "number" && v.is_number()),
        other => other.as_i64().is_some_and(|n| n as u8 == v.element_type()),
    }
}

/// Matches one field's operator document (or plain value) against the field values.
fn field_matches(doc: &Document, path: &str, cond: &Bson) -> QResult<bool> {
    let values = path_values(doc, path);
    let ops = match cond {
        Bson::Document(d) if d.keys().next().is_some_and(|k| k.starts_with('$')) => d,
        other => {
            // plain equality ({field: value}); null matches missing fields
            if matches!(other, Bson::Null) && values.is_empty() {
                return Ok(true);
            }
            for v in &values {
                if eq_match(v, other)? {
                    return Ok(true);
                }
            }
            // whole-array equality on a path ending in an array of documents
            if let (Some(whole), Bson::Array(_)) = (get_path(doc, path), other) {
                if whole.eq_bson(other) {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
    };
    let regex_opts = ops.get_str("$options").unwrap_or("").to_string();
    for (op, arg) in ops.iter() {
        let ok = match op.as_str() {
            "$eq" => {
                if matches!(arg, Bson::Null) && values.is_empty() {
                    true
                } else {
                    let mut any = false;
                    for v in &values {
                        if eq_match(v, arg)? {
                            any = true;
                            break;
                        }
                    }
                    any
                }
            }
            "$ne" => !field_matches(doc, path, &Bson::Document(Document::new().with("$eq", arg.clone())))?,
            "$gt" => values.iter().any(|v| cmp_match(v, arg, |o| o == Ordering::Greater)),
            "$gte" => values.iter().any(|v| cmp_match(v, arg, |o| o != Ordering::Less)),
            "$lt" => values.iter().any(|v| cmp_match(v, arg, |o| o == Ordering::Less)),
            "$lte" => values.iter().any(|v| cmp_match(v, arg, |o| o != Ordering::Greater)),
            "$in" | "$nin" => {
                let Bson::Array(list) = arg else { return Err(bad(format!("{} needs an array", op))) };
                let mut any = false;
                for t in list {
                    if matches!(t, Bson::Null) && values.is_empty() {
                        any = true;
                    }
                    for v in &values {
                        if eq_match(v, t)? {
                            any = true;
                        }
                    }
                }
                if op == "$in" { any } else { !any }
            }
            "$exists" => {
                let want = match arg {
                    Bson::Boolean(b) => *b,
                    other => other.as_f64().is_some_and(|f| f != 0.0),
                };
                want == !values.is_empty()
            }
            "$regex" => {
                let (p, o) = match arg {
                    Bson::Regex(p, o) => (p.clone(), if regex_opts.is_empty() { o.clone() } else { regex_opts.clone() }),
                    Bson::String(s) => (s.clone(), regex_opts.clone()),
                    _ => return Err(QueryError("$regex has to be a string".into(), 2)),
                };
                let mut any = false;
                for v in &values {
                    if value_matches_regex(v, &p, &o)? {
                        any = true;
                    }
                }
                any
            }
            "$options" => true,
            "$not" => match arg {
                Bson::Document(_) => !field_matches(doc, path, arg)?,
                Bson::Regex(p, o) => {
                    let mut any = false;
                    for v in &values {
                        if value_matches_regex(v, p, o)? {
                            any = true;
                        }
                    }
                    !any
                }
                _ => return Err(bad("$not needs a regex or a document")),
            },
            "$size" => {
                let n = arg.as_i64().ok_or_else(|| bad("$size needs a number"))?;
                values.iter().any(|v| matches!(v, Bson::Array(a) if a.len() as i64 == n))
            }
            "$all" => {
                let Bson::Array(list) = arg else { return Err(bad("$all needs an array")) };
                let mut all = !list.is_empty();
                for t in list {
                    let mut hit = false;
                    for v in &values {
                        if eq_match(v, t)? {
                            hit = true;
                        }
                    }
                    all &= hit;
                }
                all
            }
            "$elemMatch" => {
                let Bson::Document(sub) = arg else { return Err(bad("$elemMatch needs an Object")) };
                let mut any = false;
                for v in &values {
                    if let Bson::Array(a) = v {
                        for el in a {
                            let hit = match el {
                                Bson::Document(d) if !sub.keys().next().is_some_and(|k| k.starts_with('$')) => matches(d, sub)?,
                                other => field_matches(&Document::new().with("v", other.clone()), "v", arg)?,
                            };
                            if hit {
                                any = true;
                            }
                        }
                    }
                }
                any
            }
            "$type" => match arg {
                Bson::Array(ts) => values.iter().any(|v| ts.iter().any(|t| type_matches(v, t))),
                t => values.iter().any(|v| type_matches(v, t)),
            },
            "$mod" => {
                let Bson::Array(a) = arg else { return Err(bad("$mod needs an array")) };
                let (d, r) = (a.first().and_then(Bson::as_i64).unwrap_or(1), a.get(1).and_then(Bson::as_i64).unwrap_or(0));
                d != 0 && values.iter().any(|v| v.as_i64().is_some_and(|x| x % d == r))
            }
            other => return Err(QueryError(format!("unknown operator: {}", other), 2)),
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `doc` matches `filter`.
pub fn matches(doc: &Document, filter: &Document) -> QResult<bool> {
    for (k, v) in filter.iter() {
        let ok = match k.as_str() {
            "$and" | "$or" | "$nor" => {
                let Bson::Array(list) = v else { return Err(bad(format!("{} must be an array", k))) };
                if list.is_empty() {
                    return Err(bad(format!("{} must be a nonempty array", k)));
                }
                let mut results = Vec::new();
                for f in list {
                    let Bson::Document(f) = f else { return Err(bad(format!("{} argument's entries must be objects", k))) };
                    results.push(matches(doc, f)?);
                }
                match k.as_str() {
                    "$and" => results.iter().all(|b| *b),
                    "$or" => results.iter().any(|b| *b),
                    _ => !results.iter().any(|b| *b),
                }
            }
            "$comment" => true,
            "$expr" | "$where" | "$text" => return Err(QueryError(format!("{} is not supported by the embedded server", k), 2)),
            _ => field_matches(doc, k, v)?,
        };
        if !ok {
            return Ok(false);
        }
    }
    Ok(true)
}

fn num_op(a: Option<&Bson>, b: &Bson, mul: bool) -> QResult<Bson> {
    let a = a.cloned().unwrap_or(Bson::Int32(0));
    if !a.is_number() || !b.is_number() {
        return Err(QueryError(format!("Cannot apply {} to a value of non-numeric type", if mul { "$mul" } else { "$inc" }), 14));
    }
    Ok(match (&a, b) {
        (Bson::Double(_), _) | (_, Bson::Double(_)) | (Bson::Decimal128(_), _) | (_, Bson::Decimal128(_)) => {
            let (x, y) = (a.as_f64().unwrap(), b.as_f64().unwrap());
            Bson::Double(if mul { x * y } else { x + y })
        }
        _ => {
            let (x, y) = (a.as_i64().unwrap(), b.as_i64().unwrap());
            let r = if mul { x.checked_mul(y) } else { x.checked_add(y) }.ok_or_else(|| QueryError("integer overflow".into(), 2))?;
            if matches!(a, Bson::Int64(_)) || matches!(b, Bson::Int64(_)) { Bson::Int64(r) } else { Bson::from_i64_compact(r) }
        }
    })
}

/// Whether an update document uses operators (`$set` ...) rather than being a replacement.
pub fn is_operator_update(update: &Document) -> bool {
    update.keys().next().is_some_and(|k| k.starts_with('$'))
}

/// Applies an operator update in place. `inserting` enables `$setOnInsert`.
pub fn apply_update(doc: &mut Document, update: &Document, inserting: bool) -> QResult<()> {
    for (op, fields) in update.iter() {
        let Bson::Document(fields) = fields else { return Err(QueryError(format!("Modifiers operate on fields but we found type {} instead", fields.type_name()), 9)) };
        for (path, arg) in fields.iter() {
            if path == "_id" && op != "$setOnInsert" && !(op == "$set" && get_path(doc, "_id").is_some_and(|v| v.eq_bson(arg))) && op != "$unset" {
                return Err(QueryError("Performing an update on the path '_id' would modify the immutable field '_id'".into(), 66));
            }
            match op.as_str() {
                "$set" => set_path(doc, path, arg.clone())?,
                "$setOnInsert" => {
                    if inserting {
                        set_path(doc, path, arg.clone())?
                    }
                }
                "$unset" => {
                    unset_path(doc, path);
                }
                "$inc" | "$mul" => {
                    let cur = get_path(doc, path);
                    let v = num_op(cur.as_ref(), arg, op == "$mul")?;
                    set_path(doc, path, if cur.is_none() && op == "$mul" { Bson::Int32(0) } else { v })?
                }
                "$min" | "$max" => {
                    let cur = get_path(doc, path);
                    let replace = match &cur {
                        None => true,
                        Some(c) => {
                            let o = arg.cmp_bson(c);
                            if op == "$min" { o == Ordering::Less } else { o == Ordering::Greater }
                        }
                    };
                    if replace {
                        set_path(doc, path, arg.clone())?
                    }
                }
                "$currentDate" => set_path(doc, path, Bson::DateTime(crate::util::now_millis()))?,
                "$rename" => {
                    let Some(to) = arg.as_str() else { return Err(bad("$rename target must be a string")) };
                    if let Some(v) = get_path(doc, path) {
                        unset_path(doc, path);
                        set_path(doc, to, v)?;
                    }
                }
                "$push" | "$addToSet" => {
                    let (items, each) = match arg {
                        Bson::Document(d) if d.contains_key("$each") => (d.get_array("$each").cloned().ok_or_else(|| bad("$each must be an array"))?, true),
                        other => (vec![other.clone()], false),
                    };
                    let _ = each;
                    let mut arr = match get_path(doc, path) {
                        None => Vec::new(),
                        Some(Bson::Array(a)) => a,
                        Some(other) => return Err(QueryError(format!("The field '{}' must be an array but is of type {}", path, other.type_name()), 2)),
                    };
                    for it in items {
                        if op == "$push" || !arr.iter().any(|x| x.eq_bson(&it)) {
                            arr.push(it);
                        }
                    }
                    if let Bson::Document(d) = arg {
                        if let Some(n) = d.get_i64("$slice") {
                            if n >= 0 {
                                arr.truncate(n as usize);
                            } else {
                                let keep = (-n) as usize;
                                if arr.len() > keep {
                                    arr = arr.split_off(arr.len() - keep);
                                }
                            }
                        }
                    }
                    set_path(doc, path, Bson::Array(arr))?
                }
                "$pull" => {
                    if let Some(Bson::Array(a)) = get_path(doc, path) {
                        let mut kept = Vec::new();
                        for x in a {
                            let remove = match arg {
                                Bson::Document(cond) if cond.keys().next().is_some_and(|k| k.starts_with('$')) => field_matches(&Document::new().with("v", x.clone()), "v", arg)?,
                                Bson::Document(cond) => matches!(&x, Bson::Document(d) if matches(d, cond)?),
                                other => x.eq_bson(other),
                            };
                            if !remove {
                                kept.push(x);
                            }
                        }
                        set_path(doc, path, Bson::Array(kept))?
                    }
                }
                "$pop" => {
                    if let Some(Bson::Array(mut a)) = get_path(doc, path) {
                        if arg.as_i64() == Some(-1) {
                            if !a.is_empty() {
                                a.remove(0);
                            }
                        } else {
                            a.pop();
                        }
                        set_path(doc, path, Bson::Array(a))?
                    }
                }
                other => return Err(QueryError(format!("Unknown modifier: {}. Expected a valid update modifier or pipeline-style update specified as an array", other), 9)),
            }
        }
    }
    Ok(())
}

/// Builds the base document for an upsert from the equality conditions of a filter.
pub fn upsert_base(filter: &Document) -> Document {
    let mut d = Document::new();
    for (k, v) in filter.iter() {
        if k.starts_with('$') {
            if k == "$and" {
                if let Bson::Array(list) = v {
                    for f in list {
                        if let Bson::Document(f) = f {
                            for (k2, v2) in upsert_base(f).0 {
                                let _ = set_path(&mut d, &k2, v2);
                            }
                        }
                    }
                }
            }
            continue;
        }
        match v {
            Bson::Document(ops) if ops.keys().next().is_some_and(|k| k.starts_with('$')) => {
                if let Some(eq) = ops.get("$eq") {
                    let _ = set_path(&mut d, k, eq.clone());
                }
            }
            other => {
                let _ = set_path(&mut d, k, other.clone());
            }
        }
    }
    d
}

/// Applies a find projection (inclusion or exclusion; `_id` included unless excluded).
pub fn project(doc: &Document, projection: &Document) -> QResult<Document> {
    if projection.is_empty() {
        return Ok(doc.clone());
    }
    let truthy = |v: &Bson| match v {
        Bson::Boolean(b) => *b,
        other => other.as_f64().map(|f| f != 0.0).unwrap_or(true),
    };
    let include_id = projection.get("_id").map(truthy).unwrap_or(true);
    let fields: Vec<(&String, &Bson)> = projection.iter().filter(|(k, _)| k.as_str() != "_id").collect();
    let inclusion = fields.iter().any(|(_, v)| truthy(v) || !v.is_number() && !matches!(v, Bson::Boolean(_)));
    if inclusion && fields.iter().any(|(_, v)| !truthy(v)) {
        return Err(QueryError("Cannot do exclusion on field in inclusion projection".into(), 31254));
    }
    let mut out = Document::new();
    if inclusion {
        if include_id {
            if let Some(id) = doc.get("_id") {
                out.insert("_id", id.clone());
            }
        }
        for (k, v) in fields {
            match v {
                Bson::String(s) if s.starts_with('$') => {
                    if let Some(x) = get_path(doc, &s[1..]) {
                        set_path(&mut out, k, x)?;
                    }
                }
                Bson::Document(_) => {
                    let val = eval_expr(doc, v)?;
                    set_path(&mut out, k, val)?;
                }
                _ => {
                    if let Some(x) = get_path(doc, k) {
                        set_path(&mut out, k, x)?;
                    }
                }
            }
        }
    } else {
        out = doc.clone();
        for (k, _) in fields {
            unset_path(&mut out, k);
        }
        if !include_id {
            out.remove("_id");
        }
    }
    Ok(out)
}

/// Sorts documents by a sort specification (`{field: 1|-1}`).
pub fn sort_docs(docs: &mut [Document], sort: &Document) {
    docs.sort_by(|a, b| {
        for (k, dir) in sort.iter() {
            let desc = dir.as_f64().is_some_and(|d| d < 0.0);
            let va = get_path(a, k).unwrap_or(Bson::Null);
            let vb = get_path(b, k).unwrap_or(Bson::Null);
            let mut o = va.cmp_bson(&vb);
            if desc {
                o = o.reverse();
            }
            if o != Ordering::Equal {
                return o;
            }
        }
        Ordering::Equal
    });
}

/// Evaluates an aggregation expression: `"$path"`, literals, documents of expressions
/// and the operators `$add $subtract $multiply $divide $concat $toUpper $toLower
/// $ifNull $cond $eq $gt $lt $size $literal`.
pub fn eval_expr(doc: &Document, e: &Bson) -> QResult<Bson> {
    Ok(match e {
        Bson::String(s) if s.starts_with("$$ROOT") => Bson::Document(doc.clone()),
        Bson::String(s) if s.starts_with('$') => get_path(doc, &s[1..]).unwrap_or(Bson::Null),
        Bson::Array(a) => Bson::Array(a.iter().map(|x| eval_expr(doc, x)).collect::<QResult<_>>()?),
        Bson::Document(d) if d.len() == 1 && d.keys().next().is_some_and(|k| k.starts_with('$')) => {
            let (op, arg) = d.iter().next().unwrap();
            let args: Vec<Bson> = match arg {
                Bson::Array(a) if op != "$literal" => a.iter().map(|x| eval_expr(doc, x)).collect::<QResult<_>>()?,
                other if op != "$literal" => vec![eval_expr(doc, other)?],
                _ => vec![],
            };
            match op.as_str() {
                "$literal" => arg.clone(),
                "$add" | "$multiply" => {
                    let mut acc = Bson::Int32(if op == "$add" { 0 } else { 1 });
                    for a in &args {
                        if matches!(a, Bson::Null) {
                            return Ok(Bson::Null);
                        }
                        acc = num_op(Some(&acc), a, op == "$multiply")?;
                    }
                    acc
                }
                "$subtract" | "$divide" => {
                    let (a, b) = (args.first().and_then(Bson::as_f64), args.get(1).and_then(Bson::as_f64));
                    match (a, b) {
                        (Some(a), Some(b)) if op == "$subtract" => {
                            if args.iter().all(|x| matches!(x, Bson::Int32(_) | Bson::Int64(_))) {
                                Bson::from_i64_compact(a as i64 - b as i64)
                            } else {
                                Bson::Double(a - b)
                            }
                        }
                        (Some(_), Some(b)) if b == 0.0 => return Err(QueryError("can't $divide by zero".into(), 2)),
                        (Some(a), Some(b)) => Bson::Double(a / b),
                        _ => Bson::Null,
                    }
                }
                "$concat" => {
                    let mut s = String::new();
                    for a in &args {
                        match a.as_str() {
                            Some(x) => s.push_str(x),
                            None => return Ok(Bson::Null),
                        }
                    }
                    Bson::String(s)
                }
                "$toUpper" => Bson::String(args.first().and_then(Bson::as_str).unwrap_or("").to_uppercase()),
                "$toLower" => Bson::String(args.first().and_then(Bson::as_str).unwrap_or("").to_lowercase()),
                "$ifNull" => args.iter().find(|x| !matches!(x, Bson::Null)).cloned().unwrap_or(Bson::Null),
                "$cond" => {
                    let c = args.first().map(truthy_value).unwrap_or(false);
                    args.get(if c { 1 } else { 2 }).cloned().unwrap_or(Bson::Null)
                }
                "$eq" | "$ne" | "$gt" | "$gte" | "$lt" | "$lte" => {
                    let o = args.first().unwrap_or(&Bson::Null).cmp_bson(args.get(1).unwrap_or(&Bson::Null));
                    Bson::Boolean(match op.as_str() {
                        "$eq" => o == Ordering::Equal,
                        "$ne" => o != Ordering::Equal,
                        "$gt" => o == Ordering::Greater,
                        "$gte" => o != Ordering::Less,
                        "$lt" => o == Ordering::Less,
                        _ => o != Ordering::Greater,
                    })
                }
                "$size" => match args.first() {
                    Some(Bson::Array(a)) => Bson::Int32(a.len() as i32),
                    _ => return Err(QueryError("The argument to $size must be an array".into(), 17124)),
                },
                other => return Err(QueryError(format!("Unrecognized expression '{}'", other), 168)),
            }
        }
        Bson::Document(d) => {
            let mut out = Document::new();
            for (k, v) in d.iter() {
                out.insert(k.clone(), eval_expr(doc, v)?);
            }
            Bson::Document(out)
        }
        other => other.clone(),
    })
}

fn truthy_value(v: &Bson) -> bool {
    match v {
        Bson::Boolean(b) => *b,
        Bson::Null | Bson::Undefined => false,
        other => other.as_f64().map(|f| f != 0.0).unwrap_or(true),
    }
}

struct Acc {
    op: String,
    expr: Bson,
    values: Vec<Bson>,
}

/// Runs an aggregation pipeline over `docs`.
pub fn aggregate(mut docs: Vec<Document>, pipeline: &[Bson]) -> QResult<Vec<Document>> {
    for stage in pipeline {
        let Bson::Document(stage) = stage else { return Err(QueryError("Each element of the 'pipeline' array must be an object".into(), 14)) };
        if stage.len() != 1 {
            return Err(QueryError("A pipeline stage specification object must contain exactly one field.".into(), 40323));
        }
        let (name, spec) = stage.iter().next().unwrap();
        docs = match name.as_str() {
            "$match" => {
                let f = spec.as_document().ok_or_else(|| bad("the match filter must be an expression in an object"))?;
                let mut out = Vec::new();
                for d in docs {
                    if matches(&d, f)? {
                        out.push(d);
                    }
                }
                out
            }
            "$sort" => {
                let s = spec.as_document().ok_or_else(|| bad("the $sort key specification must be an object"))?;
                sort_docs(&mut docs, s);
                docs
            }
            "$limit" => {
                let n = spec.as_i64().filter(|n| *n > 0).ok_or_else(|| QueryError("the limit must be positive".into(), 15958))?;
                docs.truncate(n as usize);
                docs
            }
            "$skip" => {
                let n = spec.as_i64().filter(|n| *n >= 0).ok_or_else(|| bad("invalid argument to $skip stage"))?;
                docs.into_iter().skip(n as usize).collect()
            }
            "$project" => {
                let p = spec.as_document().ok_or_else(|| bad("$project specification must be an object"))?;
                docs.iter().map(|d| project(d, p)).collect::<QResult<_>>()?
            }
            "$addFields" | "$set" => {
                let p = spec.as_document().ok_or_else(|| bad("$addFields specification must be an object"))?;
                let mut out = Vec::new();
                for mut d in docs {
                    let src = d.clone();
                    for (k, e) in p.iter() {
                        set_path(&mut d, k, eval_expr(&src, e)?)?;
                    }
                    out.push(d);
                }
                out
            }
            "$unset" => {
                let fields: Vec<String> = match spec {
                    Bson::String(s) => vec![s.clone()],
                    Bson::Array(a) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
                    _ => return Err(bad("$unset specification must be a string or an array")),
                };
                for d in docs.iter_mut() {
                    for f in &fields {
                        unset_path(d, f);
                    }
                }
                docs
            }
            "$count" => {
                let field = spec.as_str().ok_or_else(|| QueryError("the count field must be a non-empty string".into(), 40156))?;
                if docs.is_empty() {
                    vec![]
                } else {
                    vec![Document::new().with(field, Bson::Int32(docs.len() as i32))]
                }
            }
            "$unwind" => {
                let path = match spec {
                    Bson::String(s) => s.clone(),
                    Bson::Document(d) => d.get_str("path").unwrap_or("").to_string(),
                    _ => return Err(bad("expected either a string or an object as specification for $unwind stage")),
                };
                let path = path.strip_prefix('$').ok_or_else(|| QueryError("path option to $unwind stage should be prefixed with a '$'".into(), 28818))?.to_string();
                let mut out = Vec::new();
                for d in docs {
                    match get_path(&d, &path) {
                        Some(Bson::Array(a)) => {
                            for x in a {
                                let mut c = d.clone();
                                set_path(&mut c, &path, x)?;
                                out.push(c);
                            }
                        }
                        Some(Bson::Null) | None => {}
                        Some(_) => out.push(d),
                    }
                }
                out
            }
            "$group" => group(docs, spec.as_document().ok_or_else(|| QueryError("a group's fields must be specified in an object".into(), 15947))?)?,
            other => return Err(QueryError(format!("Unrecognized pipeline stage name: '{}'", other), 40324)),
        };
    }
    Ok(docs)
}

fn group(docs: Vec<Document>, spec: &Document) -> QResult<Vec<Document>> {
    let id_expr = spec.get("_id").ok_or_else(|| QueryError("a group specification must include an _id".into(), 15955))?.clone();
    let acc_specs: Vec<(String, String, Bson)> = spec
        .iter()
        .filter(|(k, _)| k.as_str() != "_id")
        .map(|(k, v)| {
            let d = v.as_document().filter(|d| d.len() == 1).ok_or_else(|| QueryError(format!("The field '{}' must be an accumulator object", k), 40234))?;
            let (op, e) = d.iter().next().unwrap();
            Ok((k.clone(), op.clone(), e.clone()))
        })
        .collect::<QResult<_>>()?;
    let mut groups: Vec<(Bson, Vec<Acc>)> = Vec::new();
    for d in &docs {
        let key = eval_expr(d, &id_expr)?;
        let idx = match groups.iter().position(|(k, _)| k.eq_bson(&key) && std::mem::discriminant(k) == std::mem::discriminant(&key) || (k.is_number() && key.is_number() && k.eq_bson(&key))) {
            Some(i) => i,
            None => {
                groups.push((key, acc_specs.iter().map(|(_, op, e)| Acc { op: op.clone(), expr: e.clone(), values: Vec::new() }).collect()));
                groups.len() - 1
            }
        };
        for a in groups[idx].1.iter_mut() {
            let v = if a.op == "$count" { Bson::Int32(1) } else { eval_expr(d, &a.expr)? };
            a.values.push(v);
        }
    }
    let mut out = Vec::new();
    for (key, accs) in groups {
        let mut d = Document::new().with("_id", key);
        for ((name, _, _), a) in acc_specs.iter().zip(accs) {
            let nums: Vec<&Bson> = a.values.iter().filter(|v| v.is_number()).collect();
            let v = match a.op.as_str() {
                "$sum" | "$count" => {
                    let mut acc = Bson::Int32(0);
                    for n in &nums {
                        acc = num_op(Some(&acc), n, false)?;
                    }
                    acc
                }
                "$avg" => {
                    if nums.is_empty() {
                        Bson::Null
                    } else {
                        Bson::Double(nums.iter().filter_map(|n| n.as_f64()).sum::<f64>() / nums.len() as f64)
                    }
                }
                "$min" => a.values.iter().filter(|v| !matches!(v, Bson::Null)).min_by(|x, y| x.cmp_bson(y)).cloned().unwrap_or(Bson::Null),
                "$max" => a.values.iter().filter(|v| !matches!(v, Bson::Null)).max_by(|x, y| x.cmp_bson(y)).cloned().unwrap_or(Bson::Null),
                "$first" => a.values.first().cloned().unwrap_or(Bson::Null),
                "$last" => a.values.last().cloned().unwrap_or(Bson::Null),
                "$push" => Bson::Array(a.values.clone()),
                "$addToSet" => {
                    let mut s: Vec<Bson> = Vec::new();
                    for v in a.values {
                        if !s.iter().any(|x| x.eq_bson(&v)) {
                            s.push(v);
                        }
                    }
                    Bson::Array(s)
                }
                other => return Err(QueryError(format!("unknown group operator '{}'", other), 15952)),
            };
            d.insert(name.clone(), v);
        }
        out.push(d);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(json: &str) -> Document {
        Document::parse_json(json).unwrap()
    }

    #[test]
    fn filters() {
        let doc = d(r#"{"name":"Ada","age":36,"tags":["math","code"],"addr":{"city":"London"},"items":[{"sku":"a","qty":2},{"sku":"b","qty":5}]}"#);
        let m = |f: &str| matches(&doc, &d(f)).unwrap();
        assert!(m(r#"{"name":"Ada"}"#));
        assert!(m(r#"{"age":{"$gt":30,"$lte":36}}"#));
        assert!(!m(r#"{"age":{"$lt":30}}"#));
        assert!(m(r#"{"age":{"$ne":5}}"#));
        assert!(m(r#"{"tags":"code"}"#));
        assert!(m(r#"{"tags":{"$in":["x","math"]}}"#));
        assert!(m(r#"{"tags":{"$nin":["x"]}}"#));
        assert!(m(r#"{"addr.city":"London"}"#));
        assert!(m(r#"{"items.qty":{"$gte":5}}"#));
        assert!(m(r#"{"$or":[{"age":1},{"name":{"$regex":"^a","$options":"i"}}]}"#));
        assert!(!m(r#"{"$and":[{"age":36},{"name":"Bob"}]}"#));
        assert!(m(r#"{"missing":{"$exists":false},"age":{"$exists":true}}"#));
        assert!(m(r#"{"missing":null}"#));
        assert!(m(r#"{"age":{"$not":{"$gt":40}}}"#));
        assert!(m(r#"{"tags":{"$size":2,"$all":["code","math"]}}"#));
        assert!(m(r#"{"items":{"$elemMatch":{"sku":"b","qty":{"$gt":4}}}}"#));
        assert!(m(r#"{"age":{"$type":"int"}}"#));
        assert!(m(r#"{"name":{"$regularExpression":{"pattern":"d","options":""}}}"#));
        assert!(matches(&doc, &d(r#"{"age":{"$bogus":1}}"#)).is_err());
        assert!(!m(r#"{"name":{"$gt":5}}"#), "no cross-type comparison");
    }

    #[test]
    fn updates() {
        let mut doc = d(r#"{"_id":1,"n":1,"a":{"b":1},"arr":[1]}"#);
        apply_update(&mut doc, &d(r#"{"$set":{"a.c":2,"x":"y"},"$inc":{"n":2,"new":1},"$push":{"arr":{"$each":[2,3]}},"$unset":{"a.b":""}}"#), false).unwrap();
        assert_eq!(doc.to_json(), r#"{"_id":1,"n":3,"a":{"c":2},"arr":[1,2,3],"x":"y","new":1}"#);
        apply_update(&mut doc, &d(r#"{"$addToSet":{"arr":3},"$pull":{"arr":1},"$mul":{"n":2.5}}"#), false).unwrap();
        assert_eq!(doc.get_array("arr").unwrap().len(), 2);
        assert_eq!(doc.get("n"), Some(&Bson::Double(7.5)));
        assert_eq!(apply_update(&mut doc, &d(r#"{"$set":{"_id":2}}"#), false).unwrap_err().1, 66);
        assert!(apply_update(&mut doc, &d(r#"{"$inc":{"x":1}}"#), false).is_err());
        assert_eq!(upsert_base(&d(r#"{"k":"v","n":{"$eq":3},"m":{"$gt":1}}"#)).to_json(), r#"{"k":"v","n":3}"#);
    }

    #[test]
    fn projection_and_sort() {
        let doc = d(r#"{"_id":1,"a":1,"b":{"c":2,"d":3}}"#);
        assert_eq!(project(&doc, &d(r#"{"a":1}"#)).unwrap().to_json(), r#"{"_id":1,"a":1}"#);
        assert_eq!(project(&doc, &d(r#"{"b.c":1,"_id":0}"#)).unwrap().to_json(), r#"{"b":{"c":2}}"#);
        assert_eq!(project(&doc, &d(r#"{"b":0}"#)).unwrap().to_json(), r#"{"_id":1,"a":1}"#);
        assert!(project(&doc, &d(r#"{"a":1,"b":0}"#)).is_err());
        let mut docs = vec![d(r#"{"x":2}"#), d(r#"{"x":1}"#), d(r#"{"x":3}"#)];
        sort_docs(&mut docs, &d(r#"{"x":-1}"#));
        assert_eq!(docs.iter().map(|d| d.get_i64("x").unwrap()).collect::<Vec<_>>(), vec![3, 2, 1]);
    }

    #[test]
    fn pipeline() {
        let docs = vec![
            d(r#"{"cat":"a","qty":2,"price":1.5}"#),
            d(r#"{"cat":"b","qty":5,"price":2}"#),
            d(r#"{"cat":"a","qty":3,"price":1}"#),
        ];
        let p = |s: &str| match super::super::bson::parse_json(s).unwrap() {
            Bson::Array(a) => a,
            _ => panic!(),
        };
        let out = aggregate(docs.clone(), &p(r#"[{"$match":{"qty":{"$gt":1}}},{"$group":{"_id":"$cat","total":{"$sum":"$qty"},"avg":{"$avg":"$price"},"n":{"$count":{}}}},{"$sort":{"total":-1}}]"#)).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to_json(), r#"{"_id":"a","total":5,"avg":1.25,"n":2}"#);
        let out = aggregate(docs.clone(), &p(r#"[{"$project":{"_id":0,"cat":1,"value":{"$multiply":["$qty","$price"]}}},{"$limit":1}]"#)).unwrap();
        assert_eq!(out[0].to_json(), r#"{"cat":"a","value":3.0}"#);
        let out = aggregate(docs, &p(r#"[{"$count":"n"}]"#)).unwrap();
        assert_eq!(out[0].get_i64("n"), Some(3));
        assert!(aggregate(vec![], &p(r#"[{"$nope":1}]"#)).is_err());
    }
}
