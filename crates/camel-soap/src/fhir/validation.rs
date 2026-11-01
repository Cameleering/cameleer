//! FHIR resource validation (like HAPI's `FhirValidator`), without terminology services or
//! FHIRPath invariants:
//!
//! * [`validate_base`] checks a resource against the embedded type registry (unknown
//!   elements, repeating vs single elements, JSON types and lexical forms of primitives);
//! * [`validate_profile`] checks it against a `StructureDefinition` profile (snapshot, else
//!   differential elements): `min` / `max` cardinality, allowed `type`s (choice suffixes,
//!   primitive kinds, complex vs primitive, `Reference.targetProfile` resource types),
//!   `fixed[x]` and `pattern[x]` values, `maxLength`, and the profile's `type`. Slices
//!   (`sliceName`) are not evaluated.
//!
//! Results are lists of [`Issue`]s; [`outcome`] turns them into an `OperationOutcome`.

use super::json::J;
use super::model::{self, choice_type, json_kind, FhirVersion, JsonKind};

/// A validation message.
#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    /// `error`, `warning` or `information`.
    pub severity: &'static str,
    /// OperationOutcome issue type (`required`, `structure`, `value`, `invalid`...).
    pub code: &'static str,
    pub diagnostics: String,
    /// FHIRPath-like location (`Patient.name[0].family`).
    pub expression: String,
}

impl Issue {
    fn error(code: &'static str, expression: impl Into<String>, diagnostics: impl Into<String>) -> Issue {
        Issue { severity: "error", code, diagnostics: diagnostics.into(), expression: expression.into() }
    }
}

/// Whether the issues contain no errors.
pub fn is_successful(issues: &[Issue]) -> bool {
    !issues.iter().any(|i| i.severity == "error" || i.severity == "fatal")
}

/// An `OperationOutcome` for the issues (an `information` "All OK" issue when empty).
pub fn outcome(issues: &[Issue]) -> J {
    let s = |x: &str| J::Str(x.to_string());
    let list: Vec<J> = if issues.is_empty() {
        vec![J::Obj(vec![("severity".into(), s("information")), ("code".into(), s("informational")), ("diagnostics".into(), s("All OK"))])]
    } else {
        issues
            .iter()
            .map(|i| J::Obj(vec![("severity".into(), s(i.severity)), ("code".into(), s(i.code)), ("diagnostics".into(), s(&i.diagnostics)), ("expression".into(), J::Arr(vec![s(&i.expression)]))]))
            .collect()
    };
    J::Obj(vec![("resourceType".into(), s("OperationOutcome")), ("issue".into(), J::Arr(list))])
}

// ------------------------------------------------------------------ primitives

fn all(s: &str, f: impl Fn(char) -> bool) -> bool {
    !s.is_empty() && s.chars().all(f)
}

fn digits(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| b.is_ascii_digit())
}

fn valid_date(s: &str) -> bool {
    let p: Vec<&str> = s.split('-').collect();
    match p.as_slice() {
        [y] => digits(y, 4),
        [y, m] => digits(y, 4) && digits(m, 2) && (1..=12).contains(&m.parse::<u32>().unwrap_or(0)),
        [y, m, d] => digits(y, 4) && digits(m, 2) && digits(d, 2) && (1..=12).contains(&m.parse::<u32>().unwrap_or(0)) && (1..=31).contains(&d.parse::<u32>().unwrap_or(0)),
        _ => false,
    }
}

fn valid_time(s: &str) -> bool {
    let (main, _) = s.split_once('.').unwrap_or((s, ""));
    let p: Vec<&str> = main.split(':').collect();
    p.len() == 3 && p.iter().all(|x| digits(x, 2)) && p[0].parse::<u32>().unwrap_or(99) < 24 && p[1].parse::<u32>().unwrap_or(99) < 60 && p[2].parse::<u32>().unwrap_or(99) < 61
}

fn valid_date_time(s: &str, instant: bool) -> bool {
    match s.split_once('T') {
        None => !instant && valid_date(s),
        Some((d, t)) => {
            if d.len() != 10 || !valid_date(d) {
                return false;
            }
            let (time, zone) = if let Some(t) = t.strip_suffix('Z') {
                (t, "Z")
            } else if let Some(i) = t.rfind(['+', '-']) {
                (&t[..i], &t[i..])
            } else {
                (t, "")
            };
            if zone.is_empty() {
                return false;
            }
            valid_time(time) && (zone == "Z" || (zone.len() == 6 && valid_time(&format!("{}:00", &zone[1..]))))
        }
    }
}

/// Check a primitive's JSON value against its FHIR type; `None` when valid.
pub fn check_primitive(ty: &str, v: &J) -> Option<String> {
    let kind_ok = match (json_kind(ty), v) {
        (JsonKind::Bool, J::Bool(_)) | (JsonKind::Number, J::Num(_)) | (JsonKind::String, J::Str(_)) => true,
        // values that came from a `Value` model keep decimals as strings
        (JsonKind::Number, J::Str(s)) => s.parse::<f64>().is_ok() && ty == "decimal",
        _ => false,
    };
    if !kind_ok {
        return Some(format!("Error parsing JSON: the primitive value must be a {}", match json_kind(ty) {
            JsonKind::Bool => "boolean",
            JsonKind::Number => "number",
            JsonKind::String => "string",
        }));
    }
    let text = match v {
        J::Str(s) | J::Num(s) => s.as_str(),
        _ => return None,
    };
    let ok = match ty {
        "integer" => text.parse::<i32>().is_ok(),
        "integer64" => text.parse::<i64>().is_ok(),
        "positiveInt" => text.parse::<i32>().map(|i| i > 0).unwrap_or(false),
        "unsignedInt" => text.parse::<i32>().map(|i| i >= 0).unwrap_or(false),
        "decimal" => text.parse::<f64>().is_ok(),
        "date" => valid_date(text),
        "dateTime" => valid_date_time(text, false),
        "instant" => valid_date_time(text, true),
        "time" => valid_time(text),
        "id" => text.len() <= 64 && all(text, |c| c.is_ascii_alphanumeric() || c == '-' || c == '.'),
        "code" => !text.is_empty() && text.trim() == text && !text.contains("  "),
        "uri" | "url" | "canonical" => !text.is_empty() && !text.contains(char::is_whitespace),
        "oid" => text.starts_with("urn:oid:"),
        "uuid" => text.starts_with("urn:uuid:"),
        "base64Binary" => camel_tls::pem::base64_decode(text).is_some(),
        _ => true,
    };
    if ok {
        None
    } else {
        Some(format!("The value '{}' is not a valid {}", text, ty))
    }
}

// ------------------------------------------------------------------ base validation

/// Validate a resource against the embedded type registry.
pub fn validate_base(resource: &J, v: FhirVersion) -> Vec<Issue> {
    let mut out = Vec::new();
    let Some(rt) = resource.get("resourceType").and_then(J::as_str) else {
        out.push(Issue::error("structure", "", "Missing resourceType"));
        return out;
    };
    if !model::is_resource(rt) {
        out.push(Issue { severity: "warning", code: "not-supported", diagnostics: format!("Unknown resource type '{}': only structural checks were done", rt), expression: rt.to_string() });
        return out;
    }
    base_complex(resource, rt, true, rt, v, &mut out);
    out
}

fn base_complex(j: &J, ty: &str, resource: bool, path: &str, v: FhirVersion, out: &mut Vec<Issue>) {
    let J::Obj(m) = j else {
        out.push(Issue::error("structure", path, format!("{} must be a JSON object", path)));
        return;
    };
    for (k, val) in m {
        if k == "resourceType" || k == "fhir_comments" || (k == "id" && !resource) || (ty == "Extension" && k == "url") {
            continue;
        }
        let name = k.trim_start_matches('_');
        let p = format!("{}.{}", path, k);
        let Some((cty, many)) = model::element_v(v, ty, name, resource) else {
            out.push(Issue::error("structure", &p, format!("Unrecognized element '{}' in {}", k, ty)));
            continue;
        };
        let items: Vec<&J> = match val {
            J::Arr(a) => {
                if !many {
                    out.push(Issue::error("structure", &p, format!("This property must be an Object, not an array (at {})", p)));
                }
                a.iter().collect()
            }
            other => {
                if many {
                    out.push(Issue::error("structure", &p, format!("This property must be an Array, not an object (at {})", p)));
                }
                vec![other]
            }
        };
        for (i, it) in items.iter().enumerate() {
            let ip = if many { format!("{}[{}]", p, i) } else { p.clone() };
            if k.starts_with('_') {
                if !matches!(it, J::Null) {
                    base_complex(it, "Element", false, &ip, v, out);
                }
                continue;
            }
            if matches!(it, J::Null) {
                continue;
            }
            if cty == "Resource" {
                if let Some(rt) = it.get("resourceType").and_then(J::as_str) {
                    if model::is_resource(rt) {
                        base_complex(it, rt, true, &ip, v, out);
                    }
                } else {
                    out.push(Issue::error("structure", &ip, "Contained resource has no resourceType"));
                }
            } else if cty == "xhtml" {
                if !matches!(it, J::Str(s) if s.trim_start().starts_with("<div")) {
                    out.push(Issue::error("value", &ip, "The narrative div must be an XHTML <div>"));
                }
            } else if model::is_primitive(&cty) {
                if let Some(msg) = check_primitive(&cty, it) {
                    out.push(Issue::error("value", &ip, msg));
                }
            } else if matches!(it, J::Obj(_)) {
                // registry-less complex types (e.g. Element) are skipped
                if model::element_names(v, &cty).is_empty() && cty != "Extension" {
                    continue;
                }
                base_complex(it, &cty, false, &ip, v, out);
            } else {
                out.push(Issue::error("structure", &ip, format!("{} must be an object of type {}", ip, cty)));
            }
        }
    }
}

// ------------------------------------------------------------------ profiles

/// Validate a resource against a `StructureDefinition` profile (JSON tree).
pub fn validate_profile(resource: &J, profile: &J) -> Vec<Issue> {
    let mut out = Vec::new();
    let url = profile.get("url").and_then(J::as_str).unwrap_or("profile").to_string();
    let rt = resource.get("resourceType").and_then(J::as_str).unwrap_or("");
    if let Some(t) = profile.get("type").and_then(J::as_str) {
        let t = t.rsplit('/').next().unwrap_or(t);
        if t != rt {
            out.push(Issue::error("invalid", rt, format!("Profile {} applies to {} resources, not {}", url, t, rt)));
            return out;
        }
    }
    let elements = ["snapshot", "differential"].iter().find_map(|s| match profile.get(s).and_then(|x| x.get("element")) {
        Some(J::Arr(a)) if !a.is_empty() => Some(a.clone()),
        _ => None,
    });
    let Some(elements) = elements else {
        out.push(Issue::error("invalid", rt, format!("Profile {} has no snapshot or differential elements", url)));
        return out;
    };
    for ed in &elements {
        let Some(path) = ed.get("path").and_then(J::as_str) else { continue };
        if ed.get("sliceName").is_some() {
            continue;
        }
        let segs: Vec<&str> = path.split('.').collect();
        if segs.len() < 2 || segs[0] != rt {
            continue;
        }
        let parents = select(resource, rt, &segs[1..segs.len() - 1]);
        let last = segs[segs.len() - 1];
        for (parent, ppath) in parents {
            check_element(ed, parent, &ppath, last, &url, &mut out);
        }
    }
    out
}

/// Values of `name` (`value[x]` matches any `valueXxx`) in an object: (key, path, value).
fn children<'a>(j: &'a J, base: &str, name: &str) -> Vec<(String, String, &'a J)> {
    let J::Obj(m) = j else { return Vec::new() };
    let mut out = Vec::new();
    for (k, v) in m {
        let hit = match name.strip_suffix("[x]") {
            Some(b) => k.strip_prefix(b).map(|r| r.chars().next().map(char::is_uppercase).unwrap_or(false)).unwrap_or(false),
            None => k == name,
        };
        if !hit {
            continue;
        }
        match v {
            J::Arr(a) => {
                for (i, x) in a.iter().enumerate() {
                    if !matches!(x, J::Null) {
                        out.push((k.clone(), format!("{}.{}[{}]", base, k, i), x));
                    }
                }
            }
            J::Null => {}
            other => out.push((k.clone(), format!("{}.{}", base, k), other)),
        }
    }
    out
}

fn select<'a>(j: &'a J, base: &str, segs: &[&str]) -> Vec<(&'a J, String)> {
    let mut cur = vec![(j, base.to_string())];
    for s in segs {
        let mut next = Vec::new();
        for (c, p) in cur {
            for (_, cp, v) in children(c, &p, s) {
                next.push((v, cp));
            }
        }
        cur = next;
    }
    cur
}

fn types_of(ed: &J) -> Vec<(String, Vec<String>)> {
    match ed.get("type") {
        Some(J::Arr(a)) => a
            .iter()
            .filter_map(|t| {
                let code = t.get("code").and_then(J::as_str)?;
                let code = code.strip_prefix("http://hl7.org/fhirpath/System.").map(|c| c.to_ascii_lowercase()).unwrap_or_else(|| code.to_string());
                let targets = match t.get("targetProfile") {
                    Some(J::Arr(tp)) => tp.iter().filter_map(J::as_str).map(|u| u.rsplit('/').next().unwrap_or(u).to_string()).collect(),
                    Some(J::Str(u)) => vec![u.rsplit('/').next().unwrap_or(u).to_string()],
                    _ => Vec::new(),
                };
                Some((code, targets))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn prefixed<'a>(ed: &'a J, prefix: &str) -> Option<(&'a str, &'a J)> {
    let J::Obj(m) = ed else { return None };
    m.iter().find(|(k, _)| k.strip_prefix(prefix).map(|r| r.chars().next().map(char::is_uppercase).unwrap_or(false)).unwrap_or(false)).map(|(k, v)| (&k[prefix.len()..], v))
}

fn num_eq(a: &str, b: &str) -> bool {
    a == b || matches!((a.parse::<f64>(), b.parse::<f64>()), (Ok(x), Ok(y)) if x == y)
}

fn same(a: &J, b: &J) -> bool {
    match (a, b) {
        (J::Num(x) | J::Str(x), J::Num(y) | J::Str(y)) if matches!(a, J::Num(_)) || matches!(b, J::Num(_)) => num_eq(x, y),
        (J::Arr(x), J::Arr(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
        (J::Obj(x), J::Obj(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.iter().any(|(k2, v2)| k == k2 && same(v, v2))),
        _ => a == b,
    }
}

/// Whether `v` matches pattern `p` (every element of the pattern present in the value).
fn pattern_match(p: &J, v: &J) -> bool {
    match (p, v) {
        (J::Obj(pm), J::Obj(_)) => pm.iter().all(|(k, pv)| match v.get(k) {
            Some(vv) => pattern_match(pv, vv),
            None => false,
        }),
        (J::Arr(pa), J::Arr(va)) => pa.iter().all(|pi| va.iter().any(|vi| pattern_match(pi, vi))),
        (J::Arr(pa), other) => pa.len() == 1 && pattern_match(&pa[0], other),
        (pp, J::Arr(va)) => va.iter().any(|vi| pattern_match(pp, vi)),
        _ => same(p, v),
    }
}

fn check_element(ed: &J, parent: &J, ppath: &str, last: &str, url: &str, out: &mut Vec<Issue>) {
    let path = ed.get("path").and_then(J::as_str).unwrap_or("");
    let values = children(parent, ppath, last);
    let n = values.len();
    let min = ed.get("min").and_then(|m| match m {
        J::Num(x) | J::Str(x) => x.parse::<usize>().ok(),
        _ => None,
    });
    if let Some(min) = min {
        if n < min {
            out.push(Issue::error("required", format!("{}.{}", ppath, last), format!("{}: minimum required = {}, but only found {} (from {})", path, min, n, url)));
        }
    }
    if let Some(max) = ed.get("max").and_then(J::as_str) {
        if max != "*" {
            if let Ok(mx) = max.parse::<usize>() {
                if n > mx {
                    out.push(Issue::error("structure", format!("{}.{}", ppath, last), format!("{}: max allowed = {}, but found {} (from {})", path, mx, n, url)));
                }
            }
        }
    }
    let types = types_of(ed);
    for (key, vpath, v) in &values {
        if !types.is_empty() {
            let actual = if let Some(base) = last.strip_suffix("[x]") { Some(choice_type(&key[base.len()..])) } else { None };
            match actual {
                Some(t) => {
                    if !types.iter().any(|(c, _)| choice_type(c) == t || *c == t || (c == "Quantity" && matches!(t.as_str(), "Age" | "Duration" | "SimpleQuantity"))) {
                        out.push(Issue::error("structure", vpath.as_str(), format!("The type '{}' is not valid - must be one of {} (at {})", t, types.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>().join(", "), vpath)));
                        continue;
                    }
                    check_value(&t, &types, v, vpath, out);
                }
                None => {
                    if types.len() == 1 {
                        let t = types[0].0.clone();
                        check_value(&t, &types, v, vpath, out);
                    }
                }
            }
        }
        if let Some((_, fixed)) = prefixed(ed, "fixed") {
            if !same(fixed, v) {
                out.push(Issue::error("value", vpath.as_str(), format!("Value is '{}' but must be '{}' (fixed value of {})", super::json::write(v, false), super::json::write(fixed, false), path)));
            }
        }
        if let Some((_, pat)) = prefixed(ed, "pattern") {
            if !pattern_match(pat, v) {
                out.push(Issue::error("value", vpath.as_str(), format!("The value {} does not match the required pattern {} (from {})", super::json::write(v, false), super::json::write(pat, false), path)));
            }
        }
        if let Some(J::Num(ml)) = ed.get("maxLength") {
            if let (Ok(ml), J::Str(s)) = (ml.parse::<usize>(), v) {
                if s.chars().count() > ml {
                    out.push(Issue::error("value", vpath.as_str(), format!("value is longer than permitted maximum length of {}", ml)));
                }
            }
        }
    }
}

fn check_value(t: &str, types: &[(String, Vec<String>)], v: &J, vpath: &str, out: &mut Vec<Issue>) {
    if model::is_primitive(t) {
        if let Some(msg) = check_primitive(t, v) {
            out.push(Issue::error("value", vpath, format!("{} (at {})", msg, vpath)));
        }
    } else if !matches!(v, J::Obj(_)) {
        out.push(Issue::error("structure", vpath, format!("{} must be an object of type {}", vpath, t)));
    } else if t == "Reference" {
        let targets: Vec<&String> = types.iter().filter(|(c, _)| c == "Reference").flat_map(|(_, t)| t.iter()).collect();
        if targets.is_empty() || targets.iter().any(|t| *t == "Resource") {
            return;
        }
        if let Some(r) = v.get("reference").and_then(J::as_str) {
            let parts: Vec<&str> = r.split('/').collect();
            let ty = parts.iter().rev().find(|p| p.chars().next().map(char::is_uppercase).unwrap_or(false) && **p != "_history").copied().unwrap_or("");
            if !r.starts_with('#') && !r.starts_with("urn:") && !targets.iter().any(|t| *t == ty) {
                out.push(Issue::error("structure", vpath, format!("Invalid Resource target type. Found {}, but expected one of ({})", ty, targets.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "))));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::json;
    use super::*;

    const PROFILE: &str = r#"{"resourceType":"StructureDefinition","url":"http://example.org/StructureDefinition/my-patient","type":"Patient","differential":{"element":[
        {"path":"Patient"},
        {"path":"Patient.identifier","min":1},
        {"path":"Patient.identifier.system","min":1,"fixedUri":"urn:oid:1.2.3"},
        {"path":"Patient.name","min":1,"max":"1"},
        {"path":"Patient.name.family","min":1,"type":[{"code":"string"}],"maxLength":10},
        {"path":"Patient.gender","min":1,"patternCode":"female"},
        {"path":"Patient.deceased[x]","type":[{"code":"boolean"}]},
        {"path":"Patient.managingOrganization","type":[{"code":"Reference","targetProfile":["http://hl7.org/fhir/StructureDefinition/Organization"]}]},
        {"path":"Patient.maritalStatus","patternCodeableConcept":{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/v3-MaritalStatus","code":"M"}]}}
    ]}}"#;

    #[test]
    fn profile_checks() {
        let profile = json::parse(PROFILE).unwrap();
        let ok = json::parse(r#"{"resourceType":"Patient","identifier":[{"system":"urn:oid:1.2.3","value":"1"}],"name":[{"family":"Doe"}],"gender":"female","deceasedBoolean":false,"managingOrganization":{"reference":"Organization/1"},"maritalStatus":{"coding":[{"system":"http://terminology.hl7.org/CodeSystem/v3-MaritalStatus","code":"M","display":"Married"}],"text":"x"}}"#).unwrap();
        assert_eq!(validate_profile(&ok, &profile), vec![]);
        let bad = json::parse(r#"{"resourceType":"Patient","identifier":[{"system":"urn:oid:9","value":"1"}],"name":[{"family":"Very-long-family"},{"given":["x"]}],"gender":"male","deceasedDateTime":"2020-01-01","managingOrganization":{"reference":"Patient/1"},"maritalStatus":{"coding":[{"code":"S"}]}}"#).unwrap();
        let issues = validate_profile(&bad, &profile);
        let msgs: Vec<&str> = issues.iter().map(|i| i.diagnostics.as_str()).collect();
        assert!(msgs.iter().any(|m| m.contains("fixed value of Patient.identifier.system")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.starts_with("Patient.name: max allowed = 1, but found 2")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.starts_with("Patient.name.family: minimum required = 1, but only found 0")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.contains("maximum length of 10")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.contains("required pattern \"female\"")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.contains("The type 'dateTime' is not valid")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.contains("Invalid Resource target type. Found Patient")), "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.contains("does not match the required pattern {\"coding\"")), "{:?}", msgs);
        assert!(!is_successful(&issues));
        let missing = json::parse(r#"{"resourceType":"Patient","gender":"female"}"#).unwrap();
        let issues = validate_profile(&missing, &profile);
        assert_eq!(issues.len(), 2, "{:?}", issues);
        assert_eq!(issues[0].expression, "Patient.identifier");
        let wrong = json::parse(r#"{"resourceType":"Observation","status":"final"}"#).unwrap();
        assert!(validate_profile(&wrong, &profile)[0].diagnostics.contains("applies to Patient"));
        let oo = outcome(&[]);
        assert!(json::write(&oo, false).contains("All OK"));
    }

    #[test]
    fn base_checks() {
        let ok = json::parse(r#"{"resourceType":"Patient","id":"a1","active":true,"birthDate":"1970-01-01","name":[{"family":"X","given":["A"]}],"_birthDate":{"extension":[{"url":"u","valueString":"x"}]}}"#).unwrap();
        assert_eq!(validate_base(&ok, FhirVersion::R4), vec![]);
        let bad = json::parse(r#"{"resourceType":"Patient","active":"yes","birthDate":"1970-13-01","name":{"family":"X"},"foo":1,"multipleBirthInteger":1.5}"#).unwrap();
        let issues = validate_base(&bad, FhirVersion::R4);
        let msgs: Vec<String> = issues.iter().map(|i| format!("{} {}", i.expression, i.diagnostics)).collect();
        assert_eq!(issues.len(), 5, "{:?}", msgs);
        assert!(msgs.iter().any(|m| m.contains("Unrecognized element 'foo'")));
        assert!(msgs.iter().any(|m| m.contains("must be an Array")));
        assert!(msgs.iter().any(|m| m.contains("'1970-13-01' is not a valid date")));
        assert!(msgs.iter().any(|m| m.contains("'1.5' is not a valid integer")));
        assert!(check_primitive("instant", &J::Str("2020-01-01T10:00:00Z".into())).is_none());
        assert!(check_primitive("instant", &J::Str("2020-01-01".into())).is_some());
        assert!(check_primitive("dateTime", &J::Str("2020-01-01T10:00:00+01:00".into())).is_none());
    }
}
