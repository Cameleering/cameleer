//! MTOM / XOP and SOAP with Attachments (SwA): `multipart/related` SOAP messages.
//!
//! * Sending with MTOM: base64 content of elements at least `threshold` characters long
//!   (an element with only base64 text) is moved to a binary MIME part and replaced by
//!   `<xop:Include href="cid:..."/>`; the root part is `application/xop+xml`.
//! * Sending attachments (SwA): every entry of the `CamelAttachments` exchange property
//!   (`content-id -> bytes`) becomes a further MIME part (`Content-ID: <id>`).
//! * Receiving: a `multipart/related` request / response is split; `xop:Include` elements
//!   are replaced by the base64 text of the referenced part (so POJO decoding sees
//!   `base64Binary` values) and the other parts become attachments.

use camel_core::error::{CamelError, Result};
use camel_core::value::{ObjectMap, Value};
use camel_tls::pem::{base64_decode, base64_encode};
use camel_xml::dom::{Document, NodeId, NodeType};

use crate::envelope::SoapVersion;

pub const XOP_NS: &str = "http://www.w3.org/2004/08/xop/include";
/// Exchange property with the attachments (`content-id -> bytes`).
pub const ATTACHMENTS: &str = "CamelAttachments";
const ROOT_CID: &str = "root.message@cxf.apache.org";

/// A MIME part: content id (without `<>`), content type, content.
#[derive(Clone, Debug, PartialEq)]
pub struct Part {
    pub id: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

fn is_base64_text(s: &str) -> bool {
    let t: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    !t.is_empty() && t.len() % 4 == 0 && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=') && base64_decode(&t).is_some()
}

fn new_cid() -> String {
    format!("{}@camel.apache.org", camel_core::message::generate_id().replace('-', ""))
}

/// Move large base64 element content to binary parts (XOP). Returns the optimized
/// envelope and the parts.
pub fn xop_optimize(envelope: &str, threshold: usize) -> Result<(String, Vec<Part>)> {
    let mut doc = Document::parse(envelope).map_err(|e| CamelError::illegal_argument(format!("Invalid envelope: {}", e)))?;
    let Some(env) = doc.document_element() else { return Ok((envelope.to_string(), Vec::new())) };
    let Some(body) = doc.element_children(env).into_iter().find(|&c| doc.local_name(c) == "Body") else { return Ok((envelope.to_string(), Vec::new())) };
    let mut stack = vec![body];
    let mut targets: Vec<NodeId> = Vec::new();
    while let Some(n) = stack.pop() {
        let kids = doc.children(n).to_vec();
        let elems: Vec<NodeId> = kids.iter().copied().filter(|&c| doc.node_type(c) == NodeType::Element).collect();
        if elems.is_empty() && n != body {
            let text = doc.string_value(n);
            if text.len() >= threshold && is_base64_text(&text) {
                targets.push(n);
            }
        }
        stack.extend(elems);
    }
    let mut parts = Vec::new();
    for t in targets {
        let text: String = doc.string_value(t).chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let data = base64_decode(&text).unwrap_or_default();
        let cid = new_cid();
        for c in doc.children(t).to_vec() {
            doc.node_mut(c).parent = None;
        }
        doc.node_mut(t).children.clear();
        let inc = doc.append_element(t, camel_xml::dom::QName::ns("xop", "Include", XOP_NS));
        doc.declare_namespace(inc, "xop", XOP_NS);
        doc.set_attribute(inc, camel_xml::dom::QName::local("href"), format!("cid:{}", cid));
        parts.push(Part { id: cid, content_type: "application/octet-stream".into(), data });
    }
    Ok((doc.to_xml_string(), parts))
}

/// Build a `multipart/related` message; returns (Content-Type header value, body).
pub fn build_multipart(envelope: &str, version: SoapVersion, xop: bool, action: &str, parts: &[Part]) -> (String, Vec<u8>) {
    let boundary = format!("uuid:{}", camel_core::message::generate_id());
    let soap_ct = match version {
        SoapVersion::V11 => "text/xml",
        SoapVersion::V12 => "application/soap+xml",
    };
    let root_ct = if xop {
        let mut t = format!("application/xop+xml; charset=UTF-8; type=\"{}\"", soap_ct);
        if version == SoapVersion::V12 && !action.is_empty() {
            t = format!("{}; action=\"{}\"", t, action);
        }
        t
    } else {
        format!("{}; charset=UTF-8", soap_ct)
    };
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{}\r\nContent-Type: {}\r\nContent-Transfer-Encoding: binary\r\nContent-ID: <{}>\r\n\r\n", boundary, root_ct, ROOT_CID).as_bytes());
    body.extend_from_slice(envelope.as_bytes());
    for p in parts {
        body.extend_from_slice(format!("\r\n--{}\r\nContent-Type: {}\r\nContent-Transfer-Encoding: binary\r\nContent-ID: <{}>\r\n\r\n", boundary, p.content_type, p.id).as_bytes());
        body.extend_from_slice(&p.data);
    }
    body.extend_from_slice(format!("\r\n--{}--\r\n", boundary).as_bytes());
    let mut ct = format!("multipart/related; type=\"{}\"; boundary=\"{}\"; start=\"<{}>\"", if xop { "application/xop+xml" } else { soap_ct }, boundary, ROOT_CID);
    if xop {
        ct.push_str(&format!("; start-info=\"{}\"", soap_ct));
    }
    (ct, body)
}

fn param(ct: &str, name: &str) -> Option<String> {
    for p in ct.split(';').skip(1) {
        let (k, v) = p.split_once('=')?;
        if k.trim().eq_ignore_ascii_case(name) {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// Whether a content type is `multipart/related`.
pub fn is_multipart(content_type: Option<&str>) -> bool {
    content_type.map(|c| c.trim().to_ascii_lowercase().starts_with("multipart/related")).unwrap_or(false)
}

/// Split a `multipart/related` body: the root part (by `start`, else the first) and the
/// other parts.
pub fn parse_multipart(content_type: &str, body: &[u8]) -> Result<(Vec<u8>, Vec<Part>)> {
    let boundary = param(content_type, "boundary").ok_or_else(|| CamelError::illegal_argument("multipart/related without boundary"))?;
    let start = param(content_type, "start").map(|s| s.trim_matches(|c| c == '<' || c == '>').to_string());
    let delim = format!("--{}", boundary);
    let mut parts: Vec<Part> = Vec::new();
    let mut pos = find(body, delim.as_bytes(), 0).ok_or_else(|| CamelError::illegal_argument("multipart body without boundary"))?;
    loop {
        let after = pos + delim.len();
        if body[after..].starts_with(b"--") {
            break;
        }
        let hstart = find(body, b"\r\n", after).map(|i| i + 2).unwrap_or(after);
        let hend = find(body, b"\r\n\r\n", hstart).ok_or_else(|| CamelError::illegal_argument("malformed MIME part"))?;
        let headers = String::from_utf8_lossy(&body[hstart..hend]).to_string();
        let dstart = hend + 4;
        let next = find(body, format!("\r\n{}", delim).as_bytes(), dstart).ok_or_else(|| CamelError::illegal_argument("unterminated MIME part"))?;
        let mut id = String::new();
        let mut ct = "application/octet-stream".to_string();
        let mut b64 = false;
        for line in headers.split("\r\n") {
            if let Some((k, v)) = line.split_once(':') {
                match k.trim().to_ascii_lowercase().as_str() {
                    "content-id" => id = v.trim().trim_matches(|c| c == '<' || c == '>').to_string(),
                    "content-type" => ct = v.trim().to_string(),
                    "content-transfer-encoding" => b64 = v.trim().eq_ignore_ascii_case("base64"),
                    _ => {}
                }
            }
        }
        let raw = &body[dstart..next];
        let data = if b64 { base64_decode(&String::from_utf8_lossy(raw).split_whitespace().collect::<String>()).unwrap_or_default() } else { raw.to_vec() };
        parts.push(Part { id, content_type: ct, data });
        pos = next + 2;
    }
    if parts.is_empty() {
        return Err(CamelError::illegal_argument("multipart/related message without parts"));
    }
    let root_idx = start.as_ref().and_then(|s| parts.iter().position(|p| p.id == *s)).unwrap_or(0);
    let root = parts.remove(root_idx);
    Ok((root.data, parts))
}

/// Replace `xop:Include` references with the base64 content of their parts; returns the
/// envelope and the parts that were not referenced (attachments).
pub fn xop_inline(envelope: &str, parts: Vec<Part>) -> Result<(String, Vec<Part>)> {
    if !envelope.contains(XOP_NS) {
        return Ok((envelope.to_string(), parts));
    }
    let mut doc = Document::parse(envelope).map_err(|e| CamelError::new(camel_xml::SAX_PARSE_EXCEPTION, format!("Invalid XOP root part: {}", e)))?;
    let mut incs = Vec::new();
    let mut stack: Vec<NodeId> = doc.document_element().into_iter().collect();
    while let Some(n) = stack.pop() {
        for c in doc.element_children(n) {
            if doc.namespace_uri(c) == XOP_NS && doc.local_name(c) == "Include" {
                incs.push(c);
            } else {
                stack.push(c);
            }
        }
    }
    let mut used = vec![false; parts.len()];
    for inc in incs {
        let href = doc.attribute(inc, "href").unwrap_or("").to_string();
        let cid = href.strip_prefix("cid:").unwrap_or(&href);
        let cid = percent_decode(cid);
        let i = parts.iter().position(|p| p.id == cid).ok_or_else(|| CamelError::illegal_argument(format!("xop:Include references a missing MIME part {}", href)))?;
        used[i] = true;
        let parent = doc.parent(inc).unwrap_or(0);
        doc.node_mut(parent).children.retain(|&c| c != inc);
        doc.append_text(parent, &base64_encode(&parts[i].data));
    }
    let rest = parts.into_iter().zip(used).filter(|(_, u)| !u).map(|(p, _)| p).collect();
    Ok((doc.to_xml_string(), rest))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Attachments of an exchange property value.
pub fn attachments_from(v: Option<&Value>) -> Vec<Part> {
    let Some(Value::Object(m)) = v else { return Vec::new() };
    m.iter()
        .map(|(k, v)| {
            let data = match v {
                Value::Bytes(b) => b.clone(),
                other => other.to_string().into_bytes(),
            };
            let ct = if matches!(v, Value::String(_)) { "text/plain; charset=UTF-8" } else { "application/octet-stream" };
            Part { id: k.clone(), content_type: ct.into(), data }
        })
        .collect()
}

/// Parts as an attachments property value.
pub fn attachments_value(parts: Vec<Part>) -> Value {
    let mut m = ObjectMap::new();
    for p in parts {
        m.insert(p.id, Value::Bytes(p.data));
    }
    Value::Object(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::build_envelope;

    #[test]
    fn mtom_roundtrip_with_attachment() {
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let env = build_envelope(SoapVersion::V11, &[], &[format!("<m:up xmlns:m=\"urn:m\"><m:name>f</m:name><m:data>{}</m:data></m:up>", base64_encode(&data))]);
        let (opt, mut parts) = xop_optimize(&env, 1024).unwrap();
        assert!(opt.contains("xop:Include"), "{}", opt);
        assert_eq!(parts.len(), 1);
        parts.push(Part { id: "att1".into(), content_type: "text/plain".into(), data: b"hello".to_vec() });
        let (ct, body) = build_multipart(&opt, SoapVersion::V11, true, "", &parts);
        assert!(is_multipart(Some(&ct)));
        let (root, rest) = parse_multipart(&ct, &body).unwrap();
        let (inlined, atts) = xop_inline(&String::from_utf8(root).unwrap(), rest).unwrap();
        assert!(inlined.contains(&base64_encode(&data)));
        assert_eq!(atts.len(), 1);
        assert_eq!(atts[0].data, b"hello");
    }
}
