//! `xmlsecurity-sign:` and `xmlsecurity-verify:` components (Camel `camel-xmlsecurity`):
//! enveloped XML Signatures with RSA-SHA256 or ECDSA-SHA256 (P-256) using the camel-tls
//! cryptography, exclusive (default) or inclusive canonicalization.
//!
//! `xmlsecurity-sign:name?privateKey=file:key.pem&certificate=file:cert.pem`
//! `xmlsecurity-verify:name?certificate=file:cert.pem` (or `publicKey=file:pub.pem`)
//!
//! Sign options: `privateKey` (PEM file `file:`/plain path, `ref:` bean holding PEM text, or
//! inline PEM; PKCS#8, PKCS#1 or SEC1), `certificate` (PEM, added as `ds:X509Certificate`
//! in `ds:KeyInfo`), `signatureAlgorithm` (default from the key: `...xmldsig-more#rsa-sha256`
//! or `...xmldsig-more#ecdsa-sha256`), `digestAlgorithm` (default
//! `http://www.w3.org/2001/04/xmlenc#sha256`), `canonicalizationMethod` (default exclusive
//! c14n; inclusive c14n is supported), `parentLocalName` / `parentNamespace` (element that
//! receives the signature, default the document element), `prefixForXmlSignatureNamespace`
//! (default `ds`, empty for none), `omitXmlDeclaration` (default false),
//! `contentReferenceUri` (default `""`, the whole document). The body becomes the signed
//! document.
//!
//! Verify options: `certificate` or `publicKey` (PEM), `trustKeyInfoCertificate` (default
//! false: use the `ds:X509Certificate` of the signature itself — only for tests or with
//! out-of-band trust), `removeSignature` (default true: the body becomes the document
//! without the `ds:Signature` element). Failures: `XmlSignatureFormatException` (no or
//! malformed signature), `XmlSignatureInvalidContentHashException` (a reference digest does
//! not match), `XmlSignatureInvalidValueException` (bad signature value); all are
//! `XmlSignatureInvalidException`s.
//!
//! Detached and enveloping signatures, XPath transforms, `KeyValue` and
//! `ds:Manifest` are not supported.

use std::any::Any;
use std::sync::Arc;

use camel_core::component::template_support::{load_resource, resource_path};
use camel_core::context::{CamelContext, WeakContext};
use camel_core::error::{kinds, register_kind_parent, CamelError, Result};
use camel_core::message::Exchange;
use camel_core::spi::{Component, Endpoint, Processor, Producer, Service};
use camel_core::util::Parameters;
use camel_core::value::Value;
use camel_tls::crypto::hash::HashAlg;
use camel_tls::keys::{PrivateKey, PublicKey};
use camel_tls::pem::{base64_decode, base64_encode, parse_pem};
use camel_tls::x509::Certificate;

use crate::c14n::{canonicalize, C14nOptions, EXC_C14N};
use crate::dom::{Document, NodeId, NodeType, QName};
use crate::serialize::{serialize, OutputOptions};

pub const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";
pub const RSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";
pub const ECDSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha256";
pub const SHA256: &str = "http://www.w3.org/2001/04/xmlenc#sha256";
pub const ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";

pub const XML_SIGNATURE_EXCEPTION: &str = "XmlSignatureException";
pub const XML_SIGNATURE_FORMAT_EXCEPTION: &str = "XmlSignatureFormatException";
pub const XML_SIGNATURE_INVALID_EXCEPTION: &str = "XmlSignatureInvalidException";
pub const XML_SIGNATURE_INVALID_CONTENT_HASH_EXCEPTION: &str = "XmlSignatureInvalidContentHashException";
pub const XML_SIGNATURE_INVALID_VALUE_EXCEPTION: &str = "XmlSignatureInvalidValueException";

pub(crate) fn register_kinds() {
    register_kind_parent(XML_SIGNATURE_EXCEPTION, kinds::EXCEPTION);
    register_kind_parent(XML_SIGNATURE_FORMAT_EXCEPTION, XML_SIGNATURE_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_EXCEPTION, XML_SIGNATURE_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_CONTENT_HASH_EXCEPTION, XML_SIGNATURE_INVALID_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_VALUE_EXCEPTION, XML_SIGNATURE_INVALID_EXCEPTION);
}

fn sig_err(kind: &str, m: impl Into<String>) -> CamelError {
    crate::register_kinds();
    CamelError::new(kind, m.into())
}

/// Load PEM text from `file:path`, a plain path, `ref:bean` or inline PEM.
fn load_pem(ctx: Option<&CamelContext>, spec: &str) -> Result<Vec<u8>> {
    let s = spec.trim();
    if s.contains("-----BEGIN") {
        return Ok(s.as_bytes().to_vec());
    }
    if s.starts_with("ref:") || s.starts_with("bean:") || s.starts_with('#') {
        let r = if let Some(n) = s.strip_prefix('#') { format!("ref:{}", n) } else { s.to_string() };
        return Ok(load_resource(ctx, &r)?.into_bytes());
    }
    std::fs::read(resource_path(s)).map_err(|e| CamelError::new(kinds::FILE_NOT_FOUND, format!("Cannot read {}: {}", spec, e)))
}

fn load_certificate(ctx: Option<&CamelContext>, spec: &str) -> Result<Certificate> {
    let pem = load_pem(ctx, spec)?;
    Certificate::from_pem_all(&pem).map_err(CamelError::illegal_argument)?.into_iter().next().ok_or_else(|| CamelError::illegal_argument(format!("No certificate in {}", spec)))
}

fn load_public_key(ctx: Option<&CamelContext>, spec: &str) -> Result<PublicKey> {
    let pem = load_pem(ctx, spec)?;
    let blocks = parse_pem(&pem).map_err(CamelError::illegal_argument)?;
    for b in blocks {
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

fn sign_bytes(key: &PrivateKey, data: &[u8]) -> Result<(Vec<u8>, &'static str)> {
    let digest = HashAlg::Sha256.hash(data);
    match key {
        PrivateKey::Rsa(k) => Ok((k.sign_pkcs1(HashAlg::Sha256, &digest).ok_or_else(|| sig_err(XML_SIGNATURE_EXCEPTION, "RSA signing failed"))?, RSA_SHA256)),
        PrivateKey::Ec { curve, d, .. } => {
            let (r, s) = curve.sign(d, &digest, HashAlg::Sha256).ok_or_else(|| sig_err(XML_SIGNATURE_EXCEPTION, "ECDSA signing failed"))?;
            let n = curve.byte_len();
            let mut v = vec![0u8; 2 * n];
            v[n - r.len()..n].copy_from_slice(&r);
            v[2 * n - s.len()..].copy_from_slice(&s);
            Ok((v, ECDSA_SHA256))
        }
    }
}

fn verify_bytes(key: &PublicKey, alg: &str, data: &[u8], sig: &[u8]) -> Result<bool> {
    let digest = HashAlg::Sha256.hash(data);
    match (key, alg) {
        (PublicKey::Rsa(k), RSA_SHA256) => Ok(k.verify_pkcs1(HashAlg::Sha256, &digest, sig)),
        (PublicKey::Ec { curve, point }, ECDSA_SHA256) => {
            let n = curve.byte_len();
            if sig.len() != 2 * n {
                return Ok(false);
            }
            Ok(curve.verify(point, &digest, &sig[..n], &sig[n..]))
        }
        (_, a) if a != RSA_SHA256 && a != ECDSA_SHA256 => Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("Unsupported signature algorithm {}", a))),
        _ => Ok(false),
    }
}

/// Sign a document (enveloped signature appended to `parent`). Returns the signature element.
pub fn sign_document(doc: &mut Document, parent: NodeId, key: &PrivateKey, cert: Option<&Certificate>, c14n_alg: &str, prefix: &str) -> Result<NodeId> {
    let opts = C14nOptions::for_algorithm(c14n_alg).ok_or_else(|| CamelError::illegal_argument(format!("Unsupported canonicalization method {}", c14n_alg)))?;
    let digest = base64_encode(&HashAlg::Sha256.hash(canonicalize(doc, 0, &opts).as_bytes()));
    let q = |l: &str| QName::ns(prefix, l, DSIG_NS);
    let sig = doc.append_element(parent, q("Signature"));
    doc.declare_namespace(sig, prefix, DSIG_NS);
    let si = doc.append_element(sig, q("SignedInfo"));
    let cm = doc.append_element(si, q("CanonicalizationMethod"));
    doc.set_attribute(cm, QName::local("Algorithm"), c14n_alg);
    let sm = doc.append_element(si, q("SignatureMethod"));
    let alg = match key {
        PrivateKey::Rsa(_) => RSA_SHA256,
        PrivateKey::Ec { .. } => ECDSA_SHA256,
    };
    doc.set_attribute(sm, QName::local("Algorithm"), alg);
    let r = doc.append_element(si, q("Reference"));
    doc.set_attribute(r, QName::local("URI"), "");
    let ts = doc.append_element(r, q("Transforms"));
    let t1 = doc.append_element(ts, q("Transform"));
    doc.set_attribute(t1, QName::local("Algorithm"), ENVELOPED);
    let t2 = doc.append_element(ts, q("Transform"));
    doc.set_attribute(t2, QName::local("Algorithm"), c14n_alg);
    let dm = doc.append_element(r, q("DigestMethod"));
    doc.set_attribute(dm, QName::local("Algorithm"), SHA256);
    let dv = doc.append_element(r, q("DigestValue"));
    doc.append_text(dv, &digest);
    let signed_info = canonicalize(doc, si, &C14nOptions { exclude: None, ..opts.clone() });
    let (value, _) = sign_bytes(key, signed_info.as_bytes())?;
    let sv = doc.append_element(sig, q("SignatureValue"));
    doc.append_text(sv, &base64_encode(&value));
    if let Some(c) = cert {
        let ki = doc.append_element(sig, q("KeyInfo"));
        let xd = doc.append_element(ki, q("X509Data"));
        let xc = doc.append_element(xd, q("X509Certificate"));
        doc.append_text(xc, &base64_encode(&c.der));
    }
    Ok(sig)
}

fn dsig_child(doc: &Document, el: NodeId, local: &str) -> Option<NodeId> {
    doc.child_elements_named(el, DSIG_NS, local).next()
}

fn find_signature(doc: &Document) -> Option<NodeId> {
    (0..doc.nodes.len()).find(|&i| {
        let n = doc.node(i);
        n.ty == NodeType::Element && n.name.uri == DSIG_NS && n.name.local == "Signature" && {
            // attached to the tree
            let mut cur = Some(i);
            while let Some(c) = cur {
                if c == 0 {
                    break;
                }
                cur = doc.parent(c);
            }
            cur == Some(0)
        }
    })
}

/// Verify the (first) enveloped signature of a document. Returns the signature element.
pub fn verify_document(doc: &Document, key: Option<&PublicKey>, trust_key_info: bool) -> Result<NodeId> {
    let fmt = |m: &str| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, m);
    let sig = find_signature(doc).ok_or_else(|| fmt("No XML signature found in the document"))?;
    let si = dsig_child(doc, sig, "SignedInfo").ok_or_else(|| fmt("ds:SignedInfo is missing"))?;
    let cm = dsig_child(doc, si, "CanonicalizationMethod").and_then(|c| doc.attribute(c, "Algorithm")).ok_or_else(|| fmt("ds:CanonicalizationMethod is missing"))?;
    let c14n = C14nOptions::for_algorithm(cm).ok_or_else(|| fmt(&format!("Unsupported canonicalization method {}", cm)))?;
    let alg = dsig_child(doc, si, "SignatureMethod").and_then(|c| doc.attribute(c, "Algorithm")).ok_or_else(|| fmt("ds:SignatureMethod is missing"))?;
    let refs: Vec<NodeId> = doc.child_elements_named(si, DSIG_NS, "Reference").collect();
    if refs.is_empty() {
        return Err(fmt("ds:Reference is missing"));
    }
    for r in refs {
        let uri = doc.attribute(r, "URI").unwrap_or("");
        let target = if uri.is_empty() {
            0
        } else if let Some(id) = uri.strip_prefix('#') {
            (0..doc.nodes.len())
                .find(|&i| doc.node_type(i) == NodeType::Element && ["Id", "ID", "id"].iter().any(|a| doc.attribute(i, a) == Some(id)))
                .ok_or_else(|| fmt(&format!("Reference target #{} not found", id)))?
        } else {
            return Err(fmt(&format!("Unsupported reference URI {} (only same-document references)", uri)));
        };
        let mut opts = C14nOptions { exclusive: false, with_comments: false, inclusive_prefixes: Vec::new(), exclude: None };
        if let Some(ts) = dsig_child(doc, r, "Transforms") {
            for t in doc.child_elements_named(ts, DSIG_NS, "Transform") {
                let a = doc.attribute(t, "Algorithm").unwrap_or("");
                if a == ENVELOPED {
                    opts.exclude = Some(sig);
                } else if let Some(o) = C14nOptions::for_algorithm(a) {
                    opts.exclusive = o.exclusive;
                    opts.with_comments = o.with_comments && !uri.is_empty();
                    if let Some(inc) = doc.element_children(t).into_iter().find(|&c| doc.local_name(c) == "InclusiveNamespaces") {
                        opts.inclusive_prefixes = doc.attribute(inc, "PrefixList").unwrap_or("").split_whitespace().map(String::from).collect();
                    }
                } else {
                    return Err(fmt(&format!("Unsupported transform {}", a)));
                }
            }
        }
        let dm = dsig_child(doc, r, "DigestMethod").and_then(|c| doc.attribute(c, "Algorithm")).unwrap_or("");
        let h = match dm {
            SHA256 => HashAlg::Sha256,
            "http://www.w3.org/2001/04/xmlenc#sha512" => HashAlg::Sha512,
            "http://www.w3.org/2000/09/xmldsig#sha1" => HashAlg::Sha1,
            other => return Err(fmt(&format!("Unsupported digest method {}", other))),
        };
        let expected = dsig_child(doc, r, "DigestValue").map(|d| doc.string_value(d)).ok_or_else(|| fmt("ds:DigestValue is missing"))?;
        let actual = base64_encode(&h.hash(canonicalize(doc, target, &opts).as_bytes()));
        let expected_clean: String = expected.chars().filter(|c| !c.is_whitespace()).collect();
        if actual != expected_clean {
            return Err(sig_err(XML_SIGNATURE_INVALID_CONTENT_HASH_EXCEPTION, format!("The digest of reference '{}' does not match: the signed content was changed", uri)));
        }
    }
    let sv: String = dsig_child(doc, sig, "SignatureValue").map(|s| doc.string_value(s)).ok_or_else(|| fmt("ds:SignatureValue is missing"))?.chars().filter(|c| !c.is_whitespace()).collect();
    let sig_bytes = base64_decode(&sv).ok_or_else(|| fmt("ds:SignatureValue is not base64"))?;
    let embedded;
    let key = match key {
        Some(k) => k,
        None if trust_key_info => {
            let cert_b64: String = dsig_child(doc, sig, "KeyInfo")
                .and_then(|ki| dsig_child(doc, ki, "X509Data"))
                .and_then(|xd| dsig_child(doc, xd, "X509Certificate"))
                .map(|c| doc.string_value(c))
                .ok_or_else(|| fmt("No ds:X509Certificate in ds:KeyInfo"))?
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            let der = base64_decode(&cert_b64).ok_or_else(|| fmt("Invalid ds:X509Certificate"))?;
            embedded = Certificate::from_der(&der).map_err(|e| fmt(&e.to_string()))?.public_key.ok_or_else(|| fmt("Unsupported certificate key"))?;
            &embedded
        }
        None => return Err(CamelError::illegal_argument("xmlsecurity-verify: no verification key (certificate or publicKey option)")),
    };
    let signed_info = canonicalize(doc, si, &c14n);
    if !verify_bytes(key, alg, signed_info.as_bytes(), &sig_bytes)? {
        return Err(sig_err(XML_SIGNATURE_INVALID_VALUE_EXCEPTION, "The XML signature value is invalid"));
    }
    Ok(sig)
}

/// The `xmlsecurity-sign` / `xmlsecurity-verify` component.
pub struct XmlSecurityComponent {
    sign: bool,
}

impl XmlSecurityComponent {
    pub fn sign() -> XmlSecurityComponent {
        XmlSecurityComponent { sign: true }
    }
    pub fn verify() -> XmlSecurityComponent {
        XmlSecurityComponent { sign: false }
    }
}

#[derive(Clone, Debug)]
struct Config {
    private_key: Option<String>,
    certificate: Option<String>,
    public_key: Option<String>,
    c14n: String,
    parent_local: Option<String>,
    parent_ns: Option<String>,
    prefix: String,
    omit_decl: bool,
    trust_key_info: bool,
    remove_signature: bool,
}

impl Component for XmlSecurityComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, _remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        let take_key = |params: &mut Parameters, names: &[&str]| names.iter().find_map(|n| params.take(n)).filter(|s| !s.is_empty());
        let cfg = Config {
            private_key: take_key(params, &["privateKey", "keyAccessor"]),
            certificate: take_key(params, &["certificate"]),
            public_key: take_key(params, &["publicKey", "keySelector"]),
            c14n: params.take("canonicalizationMethod").unwrap_or_else(|| EXC_C14N.into()),
            parent_local: params.take("parentLocalName"),
            parent_ns: params.take("parentNamespace"),
            prefix: params.take("prefixForXmlSignatureNamespace").unwrap_or_else(|| "ds".into()),
            omit_decl: params.take_bool("omitXmlDeclaration", false)?,
            trust_key_info: params.take_bool("trustKeyInfoCertificate", false)?,
            remove_signature: params.take_bool("removeSignature", true)?,
        };
        if let Some(a) = params.take("signatureAlgorithm") {
            if a != RSA_SHA256 && a != ECDSA_SHA256 {
                return Err(CamelError::illegal_argument(format!("Unsupported signatureAlgorithm {} (rsa-sha256 or ecdsa-sha256)", a)));
            }
        }
        if let Some(d) = params.take("digestAlgorithm") {
            if d != SHA256 && d != "http://www.w3.org/2001/04/xmlenc#sha256" {
                return Err(CamelError::illegal_argument(format!("Unsupported digestAlgorithm {} (sha256)", d)));
            }
        }
        if C14nOptions::for_algorithm(&cfg.c14n).is_none() {
            return Err(CamelError::illegal_argument(format!("Unsupported canonicalizationMethod {}", cfg.c14n)));
        }
        for k in ["transformMethods", "contentReferenceUri", "contentReferenceType", "addKeyInfoReference", "clearHeaders", "outputXmlEncoding", "baseUri", "cryptoContextProperties", "disallowDoctypeDecl", "schemaResourceUri", "lazyStartProducer", "uriDereferencer", "validationFailedHandler", "xmlSignature2Message", "outputNodeSearch", "outputNodeSearchType", "secureValidation"] {
            params.take(k);
        }
        if self.sign && cfg.private_key.is_none() {
            return Err(CamelError::illegal_argument("xmlsecurity-sign requires the privateKey option"));
        }
        Ok(Arc::new(XmlSecurityEndpoint { uri: uri.to_string(), sign: self.sign, cfg, ctx: ctx.downgrade() }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// An xmlsecurity endpoint (producer only).
pub struct XmlSecurityEndpoint {
    uri: String,
    sign: bool,
    cfg: Config,
    ctx: WeakContext,
}

impl Endpoint for XmlSecurityEndpoint {
    fn uri(&self) -> &str {
        &self.uri
    }
    fn create_producer(&self) -> Result<Arc<dyn Producer>> {
        let ctx = self.ctx.upgrade();
        let c = ctx.as_ref();
        let key = match &self.cfg.private_key {
            Some(p) => Some(PrivateKey::from_pem(&load_pem(c, p)?).map_err(CamelError::illegal_argument)?),
            None => None,
        };
        let cert = match &self.cfg.certificate {
            Some(p) if self.sign => Some(load_certificate(c, p)?),
            _ => None,
        };
        let public = match (&self.cfg.public_key, &self.cfg.certificate) {
            (Some(p), _) => Some(load_public_key(c, p)?),
            (None, Some(p)) if !self.sign => Some(load_public_key(c, p)?),
            _ => None,
        };
        Ok(Arc::new(XmlSecurityProducer { sign: self.sign, cfg: self.cfg.clone(), key, cert, public }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct XmlSecurityProducer {
    sign: bool,
    cfg: Config,
    key: Option<PrivateKey>,
    cert: Option<Certificate>,
    public: Option<PublicKey>,
}

impl Service for XmlSecurityProducer {}

impl Processor for XmlSecurityProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut doc = crate::xslt_component::body_document(&ex.message.body)?;
        let out_opts = OutputOptions { method: Some(crate::serialize::Method::Xml), omit_xml_declaration: self.cfg.omit_decl, ..Default::default() };
        if self.sign {
            let root = doc.document_element().ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, "No document element"))?;
            let parent = match &self.cfg.parent_local {
                Some(l) => (0..doc.nodes.len())
                    .find(|&i| doc.node_type(i) == NodeType::Element && doc.local_name(i) == l && self.cfg.parent_ns.as_ref().map(|n| doc.namespace_uri(i) == n).unwrap_or(true))
                    .ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("Parent element '{}' for the signature not found", l)))?,
                None => root,
            };
            sign_document(&mut doc, parent, self.key.as_ref().unwrap(), self.cert.as_ref(), &self.cfg.c14n, &self.cfg.prefix)?;
            ex.message.body = Value::String(serialize(&doc, 0, &out_opts));
        } else {
            let sig = verify_document(&doc, self.public.as_ref(), self.cfg.trust_key_info)?;
            if self.cfg.remove_signature {
                if let Some(p) = doc.parent(sig) {
                    doc.node_mut(p).children.retain(|&c| c != sig);
                }
                ex.message.body = Value::String(serialize(&doc, 0, &out_opts));
            }
        }
        Ok(())
    }
}
