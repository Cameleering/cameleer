//! Small shared helpers: resource loading, XML fragments, time stamps, PEM keys.

use std::path::{Path, PathBuf};

use cameleer_core::component::template_support::{load_resource, resource_path};
use cameleer_core::context::CamelContext;
use cameleer_core::error::{CamelError, Result};
use cameleer_tls::keys::{PrivateKey, PublicKey};
use cameleer_tls::pem::parse_pem;
use cameleer_tls::x509::Certificate;
use cameleer_xml::dom::{Document, NodeId, NodeType};

/// A `(namespace, local)` expanded name.
pub type Key = (String, String);

/// Format a key as Clark notation `{ns}local` (or `local`).
pub fn clark(k: &Key) -> String {
    if k.0.is_empty() {
        k.1.clone()
    } else {
        format!("{{{}}}{}", k.0, k.1)
    }
}

/// Parse Clark notation `{ns}local` (or a bare `local`, namespace empty).
pub fn parse_clark(s: &str) -> Key {
    let s = s.trim();
    if let Some(r) = s.strip_prefix('{') {
        if let Some(i) = r.find('}') {
            return (r[..i].to_string(), r[i + 1..].to_string());
        }
    }
    (String::new(), s.to_string())
}

/// Load a text resource: inline XML/JSON (starting with `<` or `{`), `http(s)://` url,
/// `ref:`/`bean:`/`#bean` registry string, `file:`/`classpath:` or a plain path.
/// Returns the text and the base directory (for relative imports) when loaded from a file.
pub fn load_text(ctx: Option<&CamelContext>, spec: &str) -> Result<(String, Option<PathBuf>)> {
    let s = spec.trim();
    if s.starts_with('<') || s.starts_with('{') && !s.starts_with("{{") {
        return Ok((s.to_string(), None));
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        let resp = cameleer_http::HttpClient::new().get(s)?;
        if resp.status >= 300 {
            return Err(CamelError::io(format!("Cannot load {}: HTTP {}", s, resp.status)));
        }
        return Ok((resp.body_text(), None));
    }
    if let Some(n) = s.strip_prefix('#') {
        return Ok((load_resource(ctx, &format!("ref:{}", n))?, None));
    }
    if s.starts_with("ref:") || s.starts_with("bean:") {
        return Ok((load_resource(ctx, s)?, None));
    }
    let path = resource_path(s);
    let text = load_resource(ctx, s)?;
    Ok((text, path.parent().map(Path::to_path_buf)))
}

/// Serialize an element as a self-contained fragment: every namespace in scope is declared.
/// Declarations of the SOAP envelope and WSDL namespaces are dropped (they are re-declared
/// by the serializer when an element or attribute name of the fragment uses them).
pub fn fragment(doc: &Document, el: NodeId) -> String {
    const DROP: &[&str] = &[
        "http://schemas.xmlsoap.org/soap/envelope/",
        "http://www.w3.org/2003/05/soap-envelope",
        "http://schemas.xmlsoap.org/wsdl/",
        "http://schemas.xmlsoap.org/wsdl/soap/",
        "http://schemas.xmlsoap.org/wsdl/soap12/",
    ];
    let mut d = Document::new();
    if let Some(r) = d.import_node(doc, el, 0) {
        let own: Vec<(String, String)> = doc.node(el).ns_decls.clone();
        d.node_mut(r).ns_decls.retain(|(p, u)| !DROP.contains(&u.as_str()) || own.iter().any(|(op, ou)| op == p && ou == u));
    }
    d.to_xml_string()
}

/// Parse a text that may contain several top-level elements; returns the fragments
/// (self-contained) of the top-level elements.
pub fn split_fragments(text: &str) -> Result<Vec<String>> {
    let t = strip_xml_decl(text.trim());
    if t.is_empty() {
        return Ok(Vec::new());
    }
    if let Ok(doc) = Document::parse(t) {
        if let Some(r) = doc.document_element() {
            return Ok(vec![fragment(&doc, r)]);
        }
    }
    let wrapped = format!("<camel-wrapper>{}</camel-wrapper>", t);
    let doc = Document::parse(&wrapped).map_err(|e| CamelError::new(cameleer_xml::SAX_PARSE_EXCEPTION, format!("Invalid XML payload: {}", e)))?;
    let root = doc.document_element().unwrap_or(0);
    Ok(doc.element_children(root).into_iter().map(|c| fragment(&doc, c)).collect())
}

/// Remove a leading XML declaration.
pub fn strip_xml_decl(t: &str) -> &str {
    let t = t.trim_start_matches('\u{feff}').trim_start();
    if t.starts_with("<?xml") {
        if let Some(i) = t.find("?>") {
            return t[i + 2..].trim_start();
        }
    }
    t
}

/// First element child.
pub fn first_element(doc: &Document, n: NodeId) -> Option<NodeId> {
    doc.children(n).iter().copied().find(|&c| doc.node_type(c) == NodeType::Element)
}

/// Expanded name of an element.
pub fn key_of(doc: &Document, el: NodeId) -> Key {
    (doc.namespace_uri(el).to_string(), doc.local_name(el).to_string())
}

/// Resolve a `prefix:local` QName attribute value in the scope of `el`.
pub fn resolve_qname(doc: &Document, el: NodeId, v: &str) -> Key {
    let v = v.trim();
    match v.split_once(':') {
        Some((p, l)) => (doc.lookup_namespace(el, p).unwrap_or_default(), l.to_string()),
        None => (doc.lookup_namespace(el, "").unwrap_or_default(), v.to_string()),
    }
}

/// Current UTC time as `xsd:dateTime` with milliseconds (`2024-01-02T03:04:05.678Z`).
pub fn now_utc() -> String {
    format_utc_millis(cameleer_core::util_time::now_millis())
}

/// Format epoch milliseconds as `xsd:dateTime` (UTC).
pub fn format_utc_millis(ms: i64) -> String {
    let (y, mo, d, h, mi, s) = cameleer_core::util_time::civil_from_epoch(ms.div_euclid(1000));
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, mi, s, ms.rem_euclid(1000))
}

/// Parse an `xsd:dateTime` (`Z` or `±hh:mm` offset; none = UTC) to epoch milliseconds.
pub fn parse_datetime_millis(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, time) = s.split_once('T')?;
    let mut dp = date.split('-');
    let y: i64 = dp.next()?.parse().ok()?;
    let mo: u32 = dp.next()?.parse().ok()?;
    let d: u32 = dp.next()?.parse().ok()?;
    let (clock, offset_secs) = if let Some(c) = time.strip_suffix('Z') {
        (c, 0i64)
    } else if let Some(i) = time.rfind(['+', '-']) {
        let (c, o) = time.split_at(i);
        let sign = if o.starts_with('-') { -1 } else { 1 };
        let (oh, om) = o[1..].split_once(':')?;
        (c, sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60))
    } else {
        (time, 0)
    };
    let mut cp = clock.split(':');
    let h: u32 = cp.next()?.parse().ok()?;
    let mi: u32 = cp.next()?.parse().ok()?;
    let sec = cp.next().unwrap_or("0");
    let (si, frac) = sec.split_once('.').unwrap_or((sec, "0"));
    let s: u32 = si.parse().ok()?;
    let ms: i64 = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;
    Some((cameleer_core::util_time::epoch_from_civil(y, mo, d, h, mi, s) - offset_secs) * 1000 + ms)
}

fn load_pem(ctx: Option<&CamelContext>, spec: &str) -> Result<Vec<u8>> {
    let s = spec.trim();
    if s.contains("-----BEGIN") {
        return Ok(s.as_bytes().to_vec());
    }
    Ok(load_text(ctx, s)?.0.into_bytes())
}

/// Load a PEM private key (`file:`, path, `ref:`/`#bean` or inline PEM).
pub fn load_private_key(ctx: Option<&CamelContext>, spec: &str) -> Result<PrivateKey> {
    PrivateKey::from_pem(&load_pem(ctx, spec)?).map_err(CamelError::illegal_argument)
}

/// Load a PEM certificate.
pub fn load_certificate(ctx: Option<&CamelContext>, spec: &str) -> Result<Certificate> {
    Certificate::from_pem_all(&load_pem(ctx, spec)?)
        .map_err(CamelError::illegal_argument)?
        .into_iter()
        .next()
        .ok_or_else(|| CamelError::illegal_argument(format!("No certificate in {}", spec)))
}

/// Load a public key from a PEM certificate or `PUBLIC KEY` block.
pub fn load_public_key(ctx: Option<&CamelContext>, spec: &str) -> Result<PublicKey> {
    let pem = load_pem(ctx, spec)?;
    for b in parse_pem(&pem).map_err(CamelError::illegal_argument)? {
        match b.label.as_str() {
            "PUBLIC KEY" => return PublicKey::from_spki(&b.der).map_err(|e| CamelError::illegal_argument(e.to_string())),
            "CERTIFICATE" => {
                let c = Certificate::from_der(&b.der).map_err(|e| CamelError::illegal_argument(e.to_string()))?;
                return c.public_key.ok_or_else(|| CamelError::illegal_argument("Unsupported certificate public key"));
            }
            _ => {}
        }
    }
    Err(CamelError::illegal_argument(format!("No public key or certificate in {}", spec)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clark_and_time() {
        assert_eq!(parse_clark("{urn:a}B"), ("urn:a".to_string(), "B".to_string()));
        assert_eq!(clark(&("urn:a".into(), "B".into())), "{urn:a}B");
        let ms = parse_datetime_millis("2024-02-29T12:34:56.789Z").unwrap();
        assert_eq!(format_utc_millis(ms), "2024-02-29T12:34:56.789Z");
        assert_eq!(parse_datetime_millis("2024-02-29T14:34:56.789+02:00"), Some(ms));
        assert_eq!(split_fragments("<a/><b x='1'/>").unwrap(), vec!["<a/>".to_string(), "<b x=\"1\"/>".to_string()]);
    }
}
