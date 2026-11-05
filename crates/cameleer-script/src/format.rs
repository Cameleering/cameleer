//! String formatting: printf-style (`%` in Python / Jsonnet, Java `String.format`, Groovy
//! `sprintf`) and Python's format-spec mini language (f-strings, `str.format`).

use crate::ast::Dialect;
use crate::interp::Interp;
use crate::value::*;

fn group_thousands(int_part: &str) -> String {
    let (sign, digits) = if let Some(d) = int_part.strip_prefix('-') { ("-", d) } else { ("", int_part) };
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    format!("{}{}", sign, out)
}

fn pad(s: String, width: usize, align: char, fill: char, zero: bool) -> String {
    let n = s.chars().count();
    if n >= width {
        return s;
    }
    let padn = width - n;
    if zero {
        let (sign, rest) = if s.starts_with('-') || s.starts_with('+') { s.split_at(1) } else { ("", s.as_str()) };
        return format!("{}{}{}", sign, "0".repeat(padn), rest);
    }
    let f = fill.to_string();
    match align {
        '<' => format!("{}{}", s, f.repeat(padn)),
        '^' => format!("{}{}{}", f.repeat(padn / 2), s, f.repeat(padn - padn / 2)),
        _ => format!("{}{}", f.repeat(padn), s),
    }
}

fn to_f64(it: &Interp, v: &V) -> Result<f64, String> {
    match v {
        V::Int(i) => Ok(*i as f64),
        V::Num(f) => Ok(*f),
        V::Bool(b) => Ok(*b as i64 as f64),
        V::Str(s) => s.trim().parse::<f64>().map_err(|_| format!("a number is required, not '{}'", s)),
        other => Err(format!("a number is required, not {}", it.display(other))),
    }
}

fn exp_str(f: f64, prec: usize, upper: bool) -> String {
    let s = format!("{:.*e}", prec, f);
    let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
    let (sign, digits) = if let Some(d) = e.strip_prefix('-') { ('-', d) } else { ('+', e) };
    let r = format!("{}e{}{:0>2}", m, sign, digits);
    if upper {
        r.to_uppercase()
    } else {
        r
    }
}

/// Apply a Python format spec (`>10`, `.2f`, `,d`, `05d`, `x`, `%`...) to a value.
pub fn format_spec(it: &Interp, v: &V, spec: &str) -> Result<String, String> {
    let chars: Vec<char> = spec.chars().collect();
    let mut i = 0;
    let mut fill = ' ';
    let mut align = '\0';
    if chars.len() >= 2 && matches!(chars[1], '<' | '>' | '^' | '=') {
        fill = chars[0];
        align = chars[1];
        i = 2;
    } else if !chars.is_empty() && matches!(chars[0], '<' | '>' | '^' | '=') {
        align = chars[0];
        i = 1;
    }
    let mut sign = '-';
    if i < chars.len() && matches!(chars[i], '+' | '-' | ' ') {
        sign = chars[i];
        i += 1;
    }
    if i < chars.len() && chars[i] == '#' {
        i += 1;
    }
    let mut zero = false;
    if i < chars.len() && chars[i] == '0' {
        zero = true;
        i += 1;
    }
    let mut width = 0usize;
    while i < chars.len() && chars[i].is_ascii_digit() {
        width = width * 10 + chars[i].to_digit(10).unwrap() as usize;
        i += 1;
    }
    let mut comma = false;
    if i < chars.len() && (chars[i] == ',' || chars[i] == '_') {
        comma = true;
        i += 1;
    }
    let mut prec: Option<usize> = None;
    if i < chars.len() && chars[i] == '.' {
        i += 1;
        let mut p = 0;
        while i < chars.len() && chars[i].is_ascii_digit() {
            p = p * 10 + chars[i].to_digit(10).unwrap() as usize;
            i += 1;
        }
        prec = Some(p);
    }
    let ty = chars.get(i).copied().unwrap_or('\0');
    if i + 1 < chars.len() {
        return Err(format!("Invalid format specifier '{}'", spec));
    }
    let is_num = v.is_number() || matches!(v, V::Bool(_));
    let mut s = match ty {
        'd' | 'n' => {
            let f = to_f64(it, v)?;
            if matches!(v, V::Num(_)) {
                return Err("Unknown format code 'd' for object of type 'float'".into());
            }
            let s = format!("{}", f as i64);
            if comma {
                group_thousands(&s)
            } else {
                s
            }
        }
        'f' | 'F' | '%' => {
            let mut f = to_f64(it, v)?;
            if ty == '%' {
                f *= 100.0;
            }
            let s = format!("{:.*}", prec.unwrap_or(6), f);
            let s = if comma {
                let (ip, fp) = s.split_once('.').map(|(a, b)| (a.to_string(), format!(".{}", b))).unwrap_or((s.clone(), String::new()));
                format!("{}{}", group_thousands(&ip), fp)
            } else {
                s
            };
            if ty == '%' {
                format!("{}%", s)
            } else {
                s
            }
        }
        'e' | 'E' => exp_str(to_f64(it, v)?, prec.unwrap_or(6), ty == 'E'),
        'g' | 'G' => {
            let f = to_f64(it, v)?;
            let p = prec.unwrap_or(6).max(1);
            let e = if f == 0.0 { 0 } else { f.abs().log10().floor() as i32 };
            if e < -4 || e >= p as i32 {
                exp_str(f, p - 1, ty == 'G')
            } else {
                let s = format!("{:.*}", (p as i32 - 1 - e).max(0) as usize, f);
                if s.contains('.') {
                    s.trim_end_matches('0').trim_end_matches('.').to_string()
                } else {
                    s
                }
            }
        }
        'x' | 'X' | 'o' | 'b' => {
            let n = to_f64(it, v)? as i64;
            match ty {
                'x' => format!("{:x}", n),
                'X' => format!("{:X}", n),
                'o' => format!("{:o}", n),
                _ => format!("{:b}", n),
            }
        }
        's' | '\0' => {
            let mut s = if is_num && ty == '\0' && prec.is_some() {
                format!("{:.*}", prec.unwrap(), to_f64(it, v)?)
            } else {
                it.display(v)
            };
            if comma && v.is_number() {
                s = group_thousands(&s);
            }
            if let (Some(p), false) = (prec, is_num) {
                s = s.chars().take(p).collect();
            }
            s
        }
        other => return Err(format!("Unknown format code '{}'", other)),
    };
    if is_num && sign != '-' && !s.starts_with('-') {
        s = format!("{}{}", sign, s);
    }
    let default_align = if is_num { '>' } else { '<' };
    let align = if align == '\0' { default_align } else { align };
    Ok(pad(s, width, if align == '=' { '>' } else { align }, fill, zero && align != '<' && align != '^'))
}

/// printf-style formatting. `whole` is the right operand of `%` (a map for `%(name)s`).
pub fn printf(it: &Interp, fmt: &str, args: &[V], whole: Option<&V>) -> Result<String, String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut ai = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '%' {
            out.push(c);
            i += 1;
            continue;
        }
        i += 1;
        if i >= chars.len() {
            return Err("incomplete format".into());
        }
        if chars[i] == '%' {
            out.push('%');
            i += 1;
            continue;
        }
        if chars[i] == 'n' && it.d != Dialect::Python {
            out.push('\n');
            i += 1;
            continue;
        }
        // %(name)s
        let mut named: Option<V> = None;
        if chars[i] == '(' {
            let end = chars[i..].iter().position(|c| *c == ')').ok_or("incomplete format key")? + i;
            let key: String = chars[i + 1..end].iter().collect();
            i = end + 1;
            named = Some(match whole {
                Some(V::Obj(o)) => o.borrow().props.get(&key).cloned().ok_or_else(|| format!("format key '{}' not found", key))?,
                _ => return Err("format requires a mapping".into()),
            });
        }
        // positional index `%1$s` (Java)
        let mut spec = String::new();
        let mut j = i;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        let mut explicit: Option<usize> = None;
        if j < chars.len() && chars[j] == '$' && j > i {
            explicit = Some(chars[i..j].iter().collect::<String>().parse::<usize>().unwrap_or(1) - 1);
            i = j + 1;
        }
        while i < chars.len() && matches!(chars[i], '-' | '+' | ' ' | '0' | ',' | '#' | '.' | '*') || (i < chars.len() && chars[i].is_ascii_digit()) {
            spec.push(chars[i]);
            i += 1;
        }
        if i >= chars.len() {
            return Err("incomplete format".into());
        }
        let conv = chars[i];
        i += 1;
        let arg = match named {
            Some(v) => v,
            None => {
                let idx = explicit.unwrap_or_else(|| {
                    ai += 1;
                    ai - 1
                });
                match args.get(idx) {
                    Some(v) => v.clone(),
                    None => return Err("not enough arguments for format string".into()),
                }
            }
        };
        let left = spec.contains('-');
        let plus = spec.contains('+');
        let comma = spec.contains(',');
        let zero = spec.trim_start_matches(['-', '+', ' ', ',', '#']).starts_with('0');
        let digits: String = spec.chars().filter(|c| *c != '-' && *c != '+' && *c != ' ' && *c != ',' && *c != '#').collect();
        let (w, p) = match digits.split_once('.') {
            Some((w, p)) => (w.parse::<usize>().unwrap_or(0), Some(p.parse::<usize>().unwrap_or(0))),
            None => (digits.parse::<usize>().unwrap_or(0), None),
        };
        let mut s = match conv {
            's' | 'S' => {
                let mut s = it.display(&arg);
                if let Some(p) = p {
                    s = s.chars().take(p).collect();
                }
                if conv == 'S' {
                    s.to_uppercase()
                } else {
                    s
                }
            }
            'r' => crate::builtins::py_repr(it, &arg),
            'd' | 'i' | 'u' => {
                let s = format!("{}", to_f64(it, &arg)?.trunc() as i64);
                if comma {
                    group_thousands(&s)
                } else {
                    s
                }
            }
            'f' | 'F' => {
                let s = format!("{:.*}", p.unwrap_or(6), to_f64(it, &arg)?);
                if comma {
                    let (ip, fp) = s.split_once('.').map(|(a, b)| (a.to_string(), format!(".{}", b))).unwrap_or((s.clone(), String::new()));
                    format!("{}{}", group_thousands(&ip), fp)
                } else {
                    s
                }
            }
            'e' | 'E' => exp_str(to_f64(it, &arg)?, p.unwrap_or(6), conv == 'E'),
            'g' | 'G' => format_spec(it, &arg, &format!(".{}{}", p.unwrap_or(6), conv))?,
            'x' => format!("{:x}", to_f64(it, &arg)? as i64),
            'X' => format!("{:X}", to_f64(it, &arg)? as i64),
            'o' => format!("{:o}", to_f64(it, &arg)? as i64),
            'c' => match &arg {
                V::Int(n) => char::from_u32(*n as u32).map(|c| c.to_string()).unwrap_or_default(),
                other => it.display(other).chars().next().map(|c| c.to_string()).unwrap_or_default(),
            },
            'b' | 'B' => it.truthy(&arg).to_string(),
            other => return Err(format!("unsupported format character '{}'", other)),
        };
        if plus && matches!(conv, 'd' | 'i' | 'f' | 'F' | 'e' | 'E') && !s.starts_with('-') {
            s = format!("+{}", s);
        }
        let numeric = matches!(conv, 'd' | 'i' | 'u' | 'f' | 'F' | 'e' | 'E' | 'g' | 'G' | 'x' | 'X' | 'o');
        out.push_str(&pad(s, w, if left { '<' } else { '>' }, ' ', zero && !left && numeric));
    }
    if whole.is_some() && ai < args.len() && args.len() > 1 && it.d == Dialect::Python {
        return Err("not all arguments converted during string formatting".into());
    }
    Ok(out)
}

/// Python `str.format(*args, **kwargs)`.
pub fn py_format(it: &mut Interp, fmt: &str, args: &[V], kwargs: Option<&ObjRef>) -> Result<String, String> {
    let chars: Vec<char> = fmt.chars().collect();
    let mut out = String::new();
    let mut auto = 0;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '{' {
            if chars.get(i + 1) == Some(&'{') {
                out.push('{');
                i += 2;
                continue;
            }
            let end = chars[i..].iter().position(|c| *c == '}').ok_or("Single '{' encountered in format string")? + i;
            let field: String = chars[i + 1..end].iter().collect();
            i = end + 1;
            let (name_conv, spec) = match field.split_once(':') {
                Some((a, b)) => (a.to_string(), Some(b.to_string())),
                None => (field.clone(), None),
            };
            let (name, conv) = match name_conv.split_once('!') {
                Some((a, b)) => (a.to_string(), b.chars().next()),
                None => (name_conv, None),
            };
            // field name with optional .attr / [key]
            let base_end = name.find(['.', '[']).unwrap_or(name.len());
            let base = &name[..base_end];
            let mut v = if base.is_empty() {
                auto += 1;
                args.get(auto - 1).cloned().ok_or("Replacement index out of range for positional args tuple")?
            } else if let Ok(n) = base.parse::<usize>() {
                args.get(n).cloned().ok_or_else(|| format!("Replacement index {} out of range for positional args tuple", n))?
            } else {
                kwargs.and_then(|k| k.borrow().props.get(base).cloned()).ok_or_else(|| format!("KeyError: '{}'", base))?
            };
            let mut rest = &name[base_end..];
            while !rest.is_empty() {
                if let Some(r) = rest.strip_prefix('.') {
                    let e = r.find(['.', '[']).unwrap_or(r.len());
                    v = it.get_member(&v, &r[..e], 0).map_err(|_| format!("no attribute '{}'", &r[..e]))?;
                    rest = &r[e..];
                } else if let Some(r) = rest.strip_prefix('[') {
                    let e = r.find(']').ok_or("missing ']' in format string")?;
                    let k = &r[..e];
                    let key = match k.parse::<i64>() {
                        Ok(n) => V::Int(n),
                        Err(_) => V::str(k),
                    };
                    v = it.get_index(&v, &key, 0).map_err(|_| format!("KeyError: '{}'", k))?;
                    rest = &r[e + 1..];
                } else {
                    break;
                }
            }
            if conv == Some('r') {
                v = V::string(crate::builtins::py_repr(it, &v));
            }
            match spec {
                Some(s) => out.push_str(&format_spec(it, &v, &s)?),
                None => out.push_str(&it.display(&v)),
            }
            continue;
        }
        if c == '}' && chars.get(i + 1) == Some(&'}') {
            out.push('}');
            i += 2;
            continue;
        }
        out.push(c);
        i += 1;
    }
    Ok(out)
}
