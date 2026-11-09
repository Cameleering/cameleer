//! FHIR JSON ⇄ XML conversion following the FHIR serialization rules:
//!
//! * the resource type is the XML root element (namespace `http://hl7.org/fhir`) and the
//!   JSON `resourceType` property;
//! * primitives are `<name value="..."/>` in XML; their `id` and `extension`s go to the JSON
//!   `_name` property (arrays of primitives get aligned `_name` arrays with `null`s);
//! * repeating elements (per the type registry, see [`super::model`]) are JSON arrays;
//! * `boolean` / `integer` / `decimal` / `positiveInt` / `unsignedInt` are JSON booleans and
//!   numbers (decimal lexemes are kept as written);
//! * `Element.id` and `Extension.url` are XML attributes;
//! * nested resources (`contained`, `Bundle.entry.resource`, `Parameters.parameter.resource`,
//!   `Bundle.entry.response.outcome`) are wrapped in an element named after the property;
//! * the narrative `div` is embedded XHTML in XML and a string in JSON.

use cameleer_core::error::{CamelError, Result};
use cameleer_xml::dom::{Document, NodeId, QName};
use cameleer_xml::serialize::{serialize, OutputOptions};

use super::json::{self, J};
use super::model::{self, json_kind, FhirVersion, JsonKind, FHIR_NS, XHTML_NS};
use super::DATA_FORMAT_EXCEPTION;

fn dfe(m: impl Into<String>) -> CamelError {
    super::register_kinds();
    CamelError::new(DATA_FORMAT_EXCEPTION, m.into())
}

// ------------------------------------------------------------------ XML -> JSON

/// Parse FHIR XML into a JSON tree.
pub fn xml_to_tree(xml: &str) -> Result<J> {
    xml_to_tree_v(xml, FhirVersion::R4)
}

/// [`xml_to_tree`] for a given FHIR release.
pub fn xml_to_tree_v(xml: &str, v: FhirVersion) -> Result<J> {
    let doc = Document::parse(super::strip_xml_decl(xml)).map_err(|e| dfe(format!("Invalid XML: {}", e)))?;
    let root = doc.document_element().ok_or_else(|| dfe("Empty XML document"))?;
    if doc.namespace_uri(root) != FHIR_NS {
        return Err(dfe(format!("Root element <{}> is not in the FHIR namespace {}", doc.qualified_name(root), FHIR_NS)));
    }
    Ok(resource_from_xml(&doc, root, v))
}

fn resource_from_xml(doc: &Document, el: NodeId, v: FhirVersion) -> J {
    let ty = doc.local_name(el).to_string();
    complex_from_xml(doc, el, &ty, true, v)
}

fn typed_primitive(v: &str, ty: &str) -> J {
    match json_kind(ty) {
        JsonKind::Bool => match v.trim() {
            "true" => J::Bool(true),
            "false" => J::Bool(false),
            other => J::Str(other.to_string()),
        },
        JsonKind::Number if v.trim().parse::<f64>().is_ok() => J::Num(v.trim().to_string()),
        _ => J::Str(v.to_string()),
    }
}

fn fhir_children(doc: &Document, el: NodeId) -> Vec<NodeId> {
    doc.element_children(el).into_iter().filter(|&c| doc.namespace_uri(c) == FHIR_NS).collect()
}

fn primitive_extras(doc: &Document, el: NodeId, v: FhirVersion) -> J {
    let mut m = Vec::new();
    if let Some(id) = doc.attribute(el, "id") {
        m.push(("id".to_string(), J::Str(id.to_string())));
    }
    let exts: Vec<J> = fhir_children(doc, el).into_iter().filter(|&c| doc.local_name(c) == "extension").map(|c| complex_from_xml(doc, c, "Extension", false, v)).collect();
    if !exts.is_empty() {
        m.push(("extension".to_string(), J::Arr(exts)));
    }
    if m.is_empty() {
        J::Null
    } else {
        J::Obj(m)
    }
}

fn complex_from_xml(doc: &Document, el: NodeId, ty: &str, resource: bool, v: FhirVersion) -> J {
    let mut out: Vec<(String, J)> = Vec::new();
    if resource {
        out.push(("resourceType".into(), J::Str(ty.to_string())));
    } else if let Some(id) = doc.attribute(el, "id") {
        out.push(("id".into(), J::Str(id.to_string())));
    }
    if ty == "Extension" {
        if let Some(u) = doc.attribute(el, "url") {
            out.push(("url".into(), J::Str(u.to_string())));
        }
    }
    // group children by name in order of first appearance
    let mut groups: Vec<(String, Vec<NodeId>)> = Vec::new();
    for c in doc.element_children(el) {
        let name = doc.local_name(c).to_string();
        if doc.namespace_uri(c) == XHTML_NS && name == "div" {
            groups.push((name, vec![c]));
            continue;
        }
        if doc.namespace_uri(c) != FHIR_NS {
            continue;
        }
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some(g) => g.1.push(c),
            None => groups.push((name, vec![c])),
        }
    }
    for (name, nodes) in groups {
        let first = nodes[0];
        let known = model::element_v(v, ty, &name, resource);
        if doc.namespace_uri(first) == XHTML_NS {
            out.push((name, J::Str(super::xml_fragment(doc, first))));
            continue;
        }
        let (cty, many) = match &known {
            Some((t, m)) => (t.clone(), *m || nodes.len() > 1),
            None => {
                let t = if doc.attribute(first, "value").is_some() { "string".to_string() } else { String::new() };
                (t, nodes.len() > 1 || model::usually_repeats(&name))
            }
        };
        if cty == "Resource" {
            let vals: Vec<J> = nodes.iter().filter_map(|&n| fhir_children(doc, n).first().map(|&r| resource_from_xml(doc, r, v))).collect();
            push(&mut out, name, vals, many);
        } else if cty == "xhtml" {
            let div = doc.element_children(first).into_iter().next();
            if let Some(d) = div {
                out.push((name, J::Str(super::xml_fragment(doc, d))));
            }
        } else if model::is_primitive(&cty) {
            let values: Vec<J> = nodes.iter().map(|&n| doc.attribute(n, "value").map(|x| typed_primitive(x, &cty)).unwrap_or(J::Null)).collect();
            let extras: Vec<J> = nodes.iter().map(|&n| primitive_extras(doc, n, v)).collect();
            let has_values = values.iter().any(|x| *x != J::Null);
            let has_extras = extras.iter().any(|x| *x != J::Null);
            if many {
                if has_values {
                    out.push((name.clone(), J::Arr(values)));
                }
                if has_extras {
                    out.push((format!("_{}", name), J::Arr(extras)));
                }
            } else {
                let (val, e) = (values.into_iter().next().unwrap_or(J::Null), extras.into_iter().next().unwrap_or(J::Null));
                if val != J::Null {
                    out.push((name.clone(), val));
                }
                if e != J::Null {
                    out.push((format!("_{}", name), e));
                }
            }
        } else {
            let vals: Vec<J> = nodes.iter().map(|&n| complex_from_xml(doc, n, &cty, false, v)).collect();
            push(&mut out, name, vals, many);
        }
    }
    J::Obj(out)
}

fn push(out: &mut Vec<(String, J)>, name: String, mut vals: Vec<J>, many: bool) {
    if vals.is_empty() {
        return;
    }
    if many {
        out.push((name, J::Arr(vals)));
    } else {
        out.push((name, vals.remove(0)));
    }
}

// ------------------------------------------------------------------ JSON -> XML

/// Write a JSON resource tree as FHIR XML (`pretty` indents).
pub fn tree_to_xml(j: &J, pretty: bool) -> Result<String> {
    tree_to_xml_v(j, pretty, FhirVersion::R4)
}

/// [`tree_to_xml`] for a given FHIR release. Elements are written in the specification's
/// order for the types of the registry (unknown elements follow, in JSON order).
pub fn tree_to_xml_v(j: &J, pretty: bool, v: FhirVersion) -> Result<String> {
    let J::Obj(m) = j else { return Err(dfe("A FHIR resource must be a JSON object")) };
    let ty = j.get("resourceType").and_then(J::as_str).ok_or_else(|| dfe("Missing resourceType"))?;
    let mut doc = Document::new();
    let root = doc.append_element(0, fq(ty));
    doc.declare_namespace(root, "", FHIR_NS);
    complex_to_xml(&mut doc, root, m, ty, true, v)?;
    let opts = OutputOptions { method: Some(cameleer_xml::serialize::Method::Xml), omit_xml_declaration: true, indent: pretty, indent_amount: 2, ..Default::default() };
    Ok(serialize(&doc, 0, &opts))
}

fn fq(local: &str) -> QName {
    QName::ns("", local, FHIR_NS)
}

fn scalar_text(j: &J) -> Option<String> {
    match j {
        J::Bool(b) => Some(b.to_string()),
        J::Num(n) => Some(n.clone()),
        J::Str(s) => Some(s.clone()),
        _ => None,
    }
}

fn write_extras(doc: &mut Document, el: NodeId, extra: Option<&J>, v: FhirVersion) -> Result<()> {
    let Some(J::Obj(m)) = extra else { return Ok(()) };
    if let Some(id) = m.iter().find(|(k, _)| k == "id").and_then(|(_, v)| scalar_text(v)) {
        doc.set_attribute(el, QName::local("id"), id);
    }
    if let Some((_, J::Arr(exts))) = m.iter().find(|(k, _)| k == "extension") {
        for e in exts {
            if let J::Obj(em) = e {
                let x = doc.append_element(el, fq("extension"));
                complex_to_xml(doc, x, em, "Extension", false, v)?;
            }
        }
    }
    Ok(())
}

fn embed_xhtml(doc: &mut Document, parent: NodeId, s: &str) -> Result<()> {
    let text = if s.trim_start().starts_with("<div") && !s.contains(XHTML_NS) { s.replacen("<div", &format!("<div xmlns=\"{}\"", XHTML_NS), 1) } else { s.to_string() };
    let x = Document::parse(&text).map_err(|e| dfe(format!("Invalid narrative XHTML: {}", e)))?;
    let r = x.document_element().ok_or_else(|| dfe("Empty narrative"))?;
    doc.import_node(&x, r, parent);
    Ok(())
}

fn complex_to_xml(doc: &mut Document, el: NodeId, m: &[(String, J)], ty: &str, resource: bool, ver: FhirVersion) -> Result<()> {
    let has = |k: &str| m.iter().any(|(x, _)| x == k);
    let get = |k: &str| m.iter().find(|(x, _)| x == k).map(|(_, v)| v);
    // spec order: known elements by their definition index, unknown ones after (stable)
    let mut ordered: Vec<&(String, J)> = m.iter().collect();
    ordered.sort_by_key(|(k, _)| model::order_key(ver, ty, k, resource).unwrap_or(usize::MAX));
    for (k, v) in ordered {
        if k == "resourceType" || k == "fhir_comments" {
            continue;
        }
        if let Some(base) = k.strip_prefix('_') {
            if has(base) {
                continue;
            }
            // extensions on primitives without values
            match v {
                J::Arr(items) => {
                    for it in items {
                        let c = doc.append_element(el, fq(base));
                        write_extras(doc, c, Some(it), ver)?;
                    }
                }
                other => {
                    let c = doc.append_element(el, fq(base));
                    write_extras(doc, c, Some(other), ver)?;
                }
            }
            continue;
        }
        if !resource && k == "id" {
            if let Some(t) = scalar_text(v) {
                doc.set_attribute(el, QName::local("id"), t);
            }
            continue;
        }
        if ty == "Extension" && k == "url" {
            if let Some(t) = scalar_text(v) {
                doc.set_attribute(el, QName::local("url"), t);
            }
            continue;
        }
        let cty = model::element_v(ver, ty, k, resource).map(|(t, _)| t).unwrap_or_default();
        let extra = get(&format!("_{}", k));
        match v {
            J::Arr(items) => {
                for (i, it) in items.iter().enumerate() {
                    let ex = match extra {
                        Some(J::Arr(a)) => a.get(i),
                        _ => None,
                    };
                    emit(doc, el, k, it, ex, &cty, ver)?;
                }
            }
            other => emit(doc, el, k, other, extra, &cty, ver)?,
        }
    }
    Ok(())
}

fn emit(doc: &mut Document, parent: NodeId, k: &str, v: &J, extra: Option<&J>, cty: &str, ver: FhirVersion) -> Result<()> {
    match v {
        J::Obj(o) => {
            if let Some(rt) = v.get("resourceType").and_then(J::as_str) {
                let wrapper = doc.append_element(parent, fq(k));
                let r = doc.append_element(wrapper, fq(rt));
                complex_to_xml(doc, r, o, rt, true, ver)?;
            } else {
                let c = doc.append_element(parent, fq(k));
                complex_to_xml(doc, c, o, cty, false, ver)?;
            }
        }
        J::Str(s) if cty == "xhtml" || (k == "div" && s.trim_start().starts_with('<')) => embed_xhtml(doc, parent, s)?,
        J::Null => {
            if extra.map(|e| *e != J::Null).unwrap_or(false) {
                let c = doc.append_element(parent, fq(k));
                write_extras(doc, c, extra, ver)?;
            }
        }
        J::Arr(_) => return Err(dfe(format!("Nested arrays are not allowed in FHIR JSON (element {})", k))),
        scalar => {
            let c = doc.append_element(parent, fq(k));
            write_extras(doc, c, extra, ver)?;
            if let Some(t) = scalar_text(scalar) {
                doc.set_attribute(c, QName::local("value"), t);
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ text level

/// Convert FHIR JSON text to FHIR XML text.
pub fn json_to_xml(text: &str, pretty: bool) -> Result<String> {
    tree_to_xml(&parse_json(text)?, pretty)
}

/// Convert FHIR XML text to FHIR JSON text.
pub fn xml_to_json(text: &str, pretty: bool) -> Result<String> {
    Ok(json::write(&xml_to_tree(text)?, pretty))
}

/// Retype a resource tree built from a [`cameleer_core::value::Value`]: strings holding numbers
/// or booleans in `integer` / `decimal` / `positiveInt` / `unsignedInt` / `boolean` elements
/// become JSON numbers / booleans (decimals whose lexeme an `f64` would not keep, such as
/// `1.50`, are unmarshalled as strings), and numbers in string-typed elements become strings.
pub fn retype(j: &mut J, v: FhirVersion) {
    let ty = j.get("resourceType").and_then(J::as_str).unwrap_or("").to_string();
    retype_complex(j, &ty, true, v);
}

fn retype_complex(j: &mut J, ty: &str, resource: bool, v: FhirVersion) {
    let J::Obj(m) = j else { return };
    for (k, val) in m.iter_mut() {
        if k == "resourceType" {
            continue;
        }
        let name = k.trim_start_matches('_');
        let Some((cty, _)) = model::element_v(v, ty, name, resource) else {
            // nested resources of unknown positions still get retyped
            retype_any(val, v);
            continue;
        };
        let primitive_ext = k.starts_with('_');
        let fix = |x: &mut J| {
            if primitive_ext {
                retype_complex(x, "Element", false, v);
            } else if cty == "Resource" {
                if let Some(rt) = x.get("resourceType").and_then(J::as_str).map(str::to_string) {
                    retype_complex(x, &rt, true, v);
                }
            } else if model::is_primitive(&cty) {
                let new = match (json_kind(&cty), &*x) {
                    (JsonKind::Number, J::Str(s)) if s.trim().parse::<f64>().is_ok() => Some(J::Num(s.trim().to_string())),
                    (JsonKind::Bool, J::Str(s)) if s == "true" || s == "false" => Some(J::Bool(s == "true")),
                    (JsonKind::String, J::Num(n)) => Some(J::Str(n.clone())),
                    (JsonKind::String, J::Bool(b)) => Some(J::Str(b.to_string())),
                    _ => None,
                };
                if let Some(n) = new {
                    *x = n;
                }
            } else {
                retype_complex(x, &cty, false, v);
            }
        };
        match val {
            J::Arr(a) => a.iter_mut().for_each(fix),
            other => fix(other),
        }
    }
}

fn retype_any(j: &mut J, v: FhirVersion) {
    match j {
        J::Obj(_) => {
            if let Some(rt) = j.get("resourceType").and_then(J::as_str).map(str::to_string) {
                retype_complex(j, &rt, true, v);
            }
        }
        J::Arr(a) => a.iter_mut().for_each(|x| retype_any(x, v)),
        _ => {}
    }
}

/// Parse and check FHIR JSON (an object with a `resourceType`).

pub fn parse_json(text: &str) -> Result<J> {
    let j = json::parse(text.trim_start_matches('\u{feff}'))?;
    match j.get("resourceType") {
        Some(J::Str(_)) => Ok(j),
        _ => Err(dfe("Invalid JSON content detected, missing required element: 'resourceType'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATIENT_JSON: &str = r#"{"resourceType":"Patient","id":"example","text":{"status":"generated","div":"<div xmlns=\"http://www.w3.org/1999/xhtml\">John <b>Doe</b></div>"},"identifier":[{"use":"usual","system":"urn:oid:1.2.36.146.595.217.0.1","value":"12345"}],"active":true,"name":[{"use":"official","family":"Chalmers","given":["Peter","James"],"_given":[null,{"extension":[{"url":"http://example.org/nick","valueString":"Jim"}]}]}],"gender":"male","birthDate":"1974-12-25","_birthDate":{"extension":[{"url":"http://hl7.org/fhir/StructureDefinition/patient-birthTime","valueDateTime":"1974-12-25T14:35:45-05:00"}]},"deceasedBoolean":false,"multipleBirthInteger":2}"#;

    #[test]
    fn patient_round_trip() {
        let xml = json_to_xml(PATIENT_JSON, false).unwrap();
        assert!(xml.starts_with("<Patient xmlns=\"http://hl7.org/fhir\"><id value=\"example\"/><text><status value=\"generated\"/><div xmlns=\"http://www.w3.org/1999/xhtml\">John <b>Doe</b></div></text>"), "{}", xml);
        assert!(xml.contains("<given value=\"Peter\"/><given value=\"James\"><extension url=\"http://example.org/nick\"><valueString value=\"Jim\"/></extension></given>"), "{}", xml);
        assert!(xml.contains("<birthDate value=\"1974-12-25\"><extension url=\"http://hl7.org/fhir/StructureDefinition/patient-birthTime\">"));
        let back = xml_to_json(&xml, false).unwrap();
        assert_eq!(json::parse(&back).unwrap(), json::parse(PATIENT_JSON).unwrap());
    }

    #[test]
    fn bundle_with_resources() {
        let j = r#"{"resourceType":"Bundle","type":"searchset","total":1,"entry":[{"fullUrl":"http://x/Observation/1","resource":{"resourceType":"Observation","id":"1","status":"final","code":{"coding":[{"system":"http://loinc.org","code":"29463-7"}]},"valueQuantity":{"value":67.50,"unit":"kg"}},"search":{"mode":"match"}}]}"#;
        let xml = json_to_xml(j, true).unwrap();
        assert!(xml.contains("<resource>"));
        assert!(xml.contains("<value value=\"67.50\"/>"));
        let back = xml_to_json(&xml, false).unwrap();
        assert_eq!(json::parse(&back).unwrap(), json::parse(j).unwrap());
        assert!(parse_json("{\"a\":1}").is_err());
        assert!(xml_to_tree("<Patient/>").is_err());
    }
}
