//! `xmlsecurity-sign:` and `xmlsecurity-verify:` components (Camel `camel-xmlsecurity`):
//! enveloped, enveloping and detached XML Signatures with RSA-SHA256 or ECDSA-SHA256 (P-256)
//! using the camel-tls cryptography, exclusive (default) or inclusive canonicalization,
//! XPath Filter and XPath Filter 2.0 transforms, `KeyInfo` with `X509Data`, `KeyValue`
//! (`RSAKeyValue`, `dsig11:ECKeyValue`) and `KeyName`, keys from PEM or key stores.
//!
//! `xmlsecurity-sign:name?privateKey=file:key.pem&certificate=file:cert.pem`
//! `xmlsecurity-verify:name?certificate=file:cert.pem` (or `publicKey=file:pub.pem`)
//!
//! Signature type, as in Camel: `parentLocalName` (+ `parentNamespace`) or
//! `parentXpath` → **enveloped** (the signature is appended to that element);
//! `xpathsToIdAttributes` (comma separated XPath expressions selecting `Id` attributes) →
//! **detached** (one reference per element, signature appended after the last signed
//! element, or to the parent element when given); `contentObjectId` or
//! `envelopingSignature=true` → **enveloping** (the body becomes a `ds:Object` inside the
//! `ds:Signature` document element). Without any of these the signature is enveloped into
//! the document element (rust-camel's historic default).
//!
//! Sign options: `privateKey` (PEM file `file:`/plain path, `ref:` bean holding PEM text, or
//! inline PEM; PKCS#8, PKCS#1 or SEC1), `certificate` (PEM, added as `ds:X509Certificate`
//! in `ds:KeyInfo`), `keyStore` (PKCS#12 / JKS / JCEKS file, `#bean` holding a
//! `camel_tls::keystore::KeyStore`) with `keyStorePassword`, `keyPassword`, `alias` (the key
//! entry and its certificate), `addKeyValue` (default false: `ds:KeyValue`), `keyName`
//! (`ds:KeyName`), `signatureId`, `xpathFilter` (an XPath Filter transform expression,
//! namespaces from `xpathNamespaces=p=uri,...`), `xpath2Filter`
//! (`intersect:expr;subtract:expr;union:expr`), `signatureAlgorithm` (default from the key:
//! `...xmldsig-more#rsa-sha256` or `...xmldsig-more#ecdsa-sha256`), `digestAlgorithm`
//! (default `http://www.w3.org/2001/04/xmlenc#sha256`), `canonicalizationMethod` (default
//! exclusive c14n; inclusive c14n is supported), `prefixForXmlSignatureNamespace` (default
//! `ds`, empty for none), `omitXmlDeclaration` (default false).
//!
//! Verify options: `certificate` or `publicKey` (PEM), `keyStore` (+ `keyStorePassword`,
//! `alias`: the signature's `X509Certificate` must be one of the store's certificates, or
//! its `KeyName` an alias, else every store key is tried), `trustKeyInfoCertificate`
//! (default false: use the `ds:X509Certificate` or `ds:KeyValue` of the signature itself —
//! only for tests or with out-of-band trust), `removeSignature` (default true: the body
//! becomes the document without the `ds:Signature` element; for an enveloping signature the
//! content of its `ds:Object`). Failures: `XmlSignatureFormatException` (no or malformed
//! signature), `XmlSignatureInvalidContentHashException` (a reference digest does not
//! match), `XmlSignatureInvalidValueException` (bad signature value), `XmlSignatureInvalidKeyException`
//! (no trusted key); all are `XmlSignatureInvalidException`s.
//!
//! Not supported: `ds:Manifest`, external (non same-document) references, XSLT transforms.

use std::any::Any;
use std::collections::HashSet;
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
use camel_tls::keystore::KeyStore;
use camel_tls::pem::{base64_decode, base64_encode, parse_pem};
use camel_tls::x509::Certificate;

use crate::c14n::{canonicalize, C14nOptions, EXC_C14N};
use crate::dom::{Document, NodeId, NodeType, QName};
use crate::serialize::{serialize, OutputOptions};

pub const DSIG_NS: &str = "http://www.w3.org/2000/09/xmldsig#";
pub const DSIG11_NS: &str = "http://www.w3.org/2009/xmldsig11#";
pub const RSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#rsa-sha256";
pub const ECDSA_SHA256: &str = "http://www.w3.org/2001/04/xmldsig-more#ecdsa-sha256";
pub const SHA256: &str = "http://www.w3.org/2001/04/xmlenc#sha256";
pub const ENVELOPED: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";
/// XPath Filter transform.
pub const XPATH_FILTER: &str = "http://www.w3.org/TR/1999/REC-xpath-19991116";
/// XPath Filter 2.0 transform (and namespace of its `XPath` elements).
pub const XPATH2_FILTER: &str = "http://www.w3.org/2002/06/xmldsig-filter2";

pub const XML_SIGNATURE_EXCEPTION: &str = "XmlSignatureException";
pub const XML_SIGNATURE_FORMAT_EXCEPTION: &str = "XmlSignatureFormatException";
pub const XML_SIGNATURE_INVALID_EXCEPTION: &str = "XmlSignatureInvalidException";
pub const XML_SIGNATURE_INVALID_CONTENT_HASH_EXCEPTION: &str = "XmlSignatureInvalidContentHashException";
pub const XML_SIGNATURE_INVALID_VALUE_EXCEPTION: &str = "XmlSignatureInvalidValueException";
pub const XML_SIGNATURE_INVALID_KEY_EXCEPTION: &str = "XmlSignatureInvalidKeyException";

pub(crate) fn register_kinds() {
    register_kind_parent(XML_SIGNATURE_EXCEPTION, kinds::EXCEPTION);
    register_kind_parent(XML_SIGNATURE_FORMAT_EXCEPTION, XML_SIGNATURE_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_EXCEPTION, XML_SIGNATURE_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_CONTENT_HASH_EXCEPTION, XML_SIGNATURE_INVALID_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_VALUE_EXCEPTION, XML_SIGNATURE_INVALID_EXCEPTION);
    register_kind_parent(XML_SIGNATURE_INVALID_KEY_EXCEPTION, XML_SIGNATURE_INVALID_EXCEPTION);
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

/// Load a key store: `#bean` / `ref:` holding a [`KeyStore`], or a PKCS#12 / JKS / JCEKS / PEM file.
pub fn load_keystore(ctx: Option<&CamelContext>, spec: &str, store_password: Option<&str>, key_password: Option<&str>) -> Result<Arc<KeyStore>> {
    let s = spec.trim();
    if let Some(n) = s.strip_prefix('#').or_else(|| s.strip_prefix("ref:")).or_else(|| s.strip_prefix("bean:")) {
        let c = ctx.ok_or_else(|| CamelError::illegal_state("No CamelContext to look up the key store bean"))?;
        return c.registry().lookup::<KeyStore>(n).ok_or_else(|| CamelError::new(kinds::NO_SUCH_BEAN, format!("No KeyStore bean named {}", n)));
    }
    let path = resource_path(s);
    let ks = KeyStore::load_file(&path, store_password, key_password.or(store_password)).map_err(|e| CamelError::illegal_argument(format!("Cannot load key store {}: {}", spec, e)))?;
    Ok(Arc::new(ks))
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
        #[allow(unreachable_patterns)]
        _ => Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, "Unsupported key type for XML signature (RSA or EC P-256 required)")),
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

fn key_alg(key: &PrivateKey) -> Result<&'static str> {
    match key {
        PrivateKey::Rsa(_) => Ok(RSA_SHA256),
        PrivateKey::Ec { .. } => Ok(ECDSA_SHA256),
        #[allow(unreachable_patterns)]
        _ => Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, "Unsupported key type for XML signature (RSA or EC P-256 required)")),
    }
}

// ------------------------------------------------------------------ transforms

/// Nodes of the subtree of `root` (elements, text, comments, PIs; attributes follow their element).
fn subtree(doc: &Document, root: NodeId) -> Vec<NodeId> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let n = out[i];
        if doc.node_type(n) == NodeType::Element || doc.node_type(n) == NodeType::Document {
            out.extend(doc.children(n).iter().copied());
        }
        i += 1;
    }
    out
}

fn xpath_error(e: crate::xpath::XPathError) -> CamelError {
    sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("Invalid XPath transform: {}", e.message))
}

/// The node set selected by an XPath Filter transform: nodes of `root`'s subtree for which
/// `expr` (evaluated with the node as context) is true.
pub fn xpath_filter_set(doc: &Document, root: NodeId, expr: &str, ns: &[(String, String)]) -> Result<HashSet<NodeId>> {
    let xp = crate::xpath::XPath::compile_ns(expr, ns).map_err(xpath_error)?;
    let docs = crate::xpath::Docs::new(doc.clone());
    let mut set = HashSet::new();
    for n in subtree(doc, root) {
        let ctx = crate::xpath::Ctx { docs: &docs, node: crate::xpath::XNode::new(0, n), pos: 1, size: 1, env: &crate::xpath::NoEnv };
        if xp.eval_ctx(&ctx).map_err(xpath_error)?.to_bool() {
            set.insert(n);
        }
    }
    Ok(set)
}

/// The node set of an XPath Filter 2.0 transform: `(filter, expr)` steps (`intersect`,
/// `subtract`, `union`) applied to the whole document.
pub fn xpath2_filter_set(doc: &Document, steps: &[(String, String)], ns: &[(String, String)]) -> Result<HashSet<NodeId>> {
    let docs = crate::xpath::Docs::new(doc.clone());
    let mut set: HashSet<NodeId> = subtree(doc, 0).into_iter().collect();
    for (filter, expr) in steps {
        let xp = crate::xpath::XPath::compile_ns(expr, ns).map_err(xpath_error)?;
        let v = xp.eval_docs(&docs, &crate::xpath::NoEnv).map_err(xpath_error)?;
        let crate::xpath::XValue::Nodes(nodes) = v else { return Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("XPath Filter 2.0 expression '{}' must select nodes", expr))) };
        let mut sel = HashSet::new();
        for n in nodes {
            if n.is_namespace() {
                continue;
            }
            let id = n.node_id();
            let id = if doc.node_type(id) == NodeType::Attribute { doc.parent(id).unwrap_or(id) } else { id };
            sel.extend(subtree(doc, id));
        }
        match filter.as_str() {
            "intersect" => set.retain(|n| sel.contains(n)),
            "subtract" => set.retain(|n| !sel.contains(n)),
            "union" => set.extend(sel),
            other => return Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("Unknown XPath Filter 2.0 filter '{}'", other))),
        }
    }
    Ok(set)
}

// ------------------------------------------------------------------ signing

/// What a signature covers and where it goes.
#[derive(Clone, Debug)]
pub enum SignatureKind {
    /// Enveloped: appended to this element, covering the whole document.
    Enveloped(NodeId),
    /// Enveloping: the document element is wrapped into a `ds:Object` with this `Id`.
    Enveloping { object_id: String },
    /// Detached: references to these `Id`s; the signature is appended to `parent`, else
    /// inserted after the last signed element.
    Detached { ids: Vec<String>, parent: Option<NodeId> },
}

/// A signature specification for [`sign_document_with`].
#[derive(Clone, Debug)]
pub struct SignSpec {
    pub kind: SignatureKind,
    pub c14n: String,
    pub prefix: String,
    /// Certificates for `ds:X509Data` (leaf first; empty = none).
    pub certificates: Vec<Certificate>,
    pub key_value: bool,
    pub key_name: Option<String>,
    pub signature_id: Option<String>,
    /// XPath Filter transform expression.
    pub xpath_filter: Option<String>,
    /// XPath Filter 2.0 steps `(filter, expr)`.
    pub xpath2_filter: Vec<(String, String)>,
    /// Namespace bindings `(prefix, uri)` of the XPath expressions.
    pub xpath_namespaces: Vec<(String, String)>,
}

impl SignSpec {
    /// An enveloped signature into `parent` with exclusive c14n and the `ds` prefix.
    pub fn enveloped(parent: NodeId) -> SignSpec {
        SignSpec {
            kind: SignatureKind::Enveloped(parent),
            c14n: EXC_C14N.into(),
            prefix: "ds".into(),
            certificates: Vec::new(),
            key_value: false,
            key_name: None,
            signature_id: None,
            xpath_filter: None,
            xpath2_filter: Vec::new(),
            xpath_namespaces: Vec::new(),
        }
    }
}

fn find_by_id(doc: &Document, id: &str) -> Option<NodeId> {
    (0..doc.nodes.len()).find(|&i| doc.node_type(i) == NodeType::Element && ["Id", "ID", "id"].iter().any(|a| doc.attribute(i, a) == Some(id)) && attached(doc, i))
}

fn attached(doc: &Document, i: NodeId) -> bool {
    let mut cur = Some(i);
    while let Some(c) = cur {
        if c == 0 {
            return true;
        }
        cur = doc.parent(c);
    }
    false
}

fn b64_unsigned(bytes: &[u8]) -> String {
    let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len().saturating_sub(1));
    base64_encode(&bytes[start..])
}

fn write_key_info(doc: &mut Document, sig: NodeId, q: &dyn Fn(&str) -> QName, key: &PrivateKey, spec: &SignSpec) {
    if spec.certificates.is_empty() && !spec.key_value && spec.key_name.is_none() {
        return;
    }
    let ki = doc.append_element(sig, q("KeyInfo"));
    if let Some(n) = &spec.key_name {
        let kn = doc.append_element(ki, q("KeyName"));
        doc.append_text(kn, n);
    }
    if spec.key_value {
        let kv = doc.append_element(ki, q("KeyValue"));
        match key.public_key() {
            PublicKey::Rsa(r) => {
                let (n, e) = r.components();
                let rk = doc.append_element(kv, q("RSAKeyValue"));
                let m = doc.append_element(rk, q("Modulus"));
                doc.append_text(m, &b64_unsigned(&n));
                let x = doc.append_element(rk, q("Exponent"));
                doc.append_text(x, &b64_unsigned(&e));
            }
            PublicKey::Ec { curve, point } => {
                let ek = doc.append_element(kv, QName::ns("dsig11", "ECKeyValue", DSIG11_NS));
                doc.declare_namespace(ek, "dsig11", DSIG11_NS);
                let nc = doc.append_element(ek, QName::ns("dsig11", "NamedCurve", DSIG11_NS));
                doc.set_attribute(nc, QName::local("URI"), format!("urn:oid:{}", camel_tls::keys::curve_oid(curve)));
                let pk = doc.append_element(ek, QName::ns("dsig11", "PublicKey", DSIG11_NS));
                doc.append_text(pk, &base64_encode(&point));
            }
            #[allow(unreachable_patterns)]
            _ => {}
        }
    }
    if !spec.certificates.is_empty() {
        let xd = doc.append_element(ki, q("X509Data"));
        for c in &spec.certificates {
            let xc = doc.append_element(xd, q("X509Certificate"));
            doc.append_text(xc, &base64_encode(&c.der));
        }
    }
}

/// Write one `ds:Reference` (with its transforms and the digest of `target`).
fn write_reference(doc: &mut Document, si: NodeId, q: &dyn Fn(&str) -> QName, uri: &str, digest: &str, enveloped: bool, spec: &SignSpec) {
    let r = doc.append_element(si, q("Reference"));
    doc.set_attribute(r, QName::local("URI"), uri);
    let ts = doc.append_element(r, q("Transforms"));
    if let Some(x) = &spec.xpath_filter {
        let t = doc.append_element(ts, q("Transform"));
        doc.set_attribute(t, QName::local("Algorithm"), XPATH_FILTER);
        let xe = doc.append_element(t, q("XPath"));
        for (p, u) in &spec.xpath_namespaces {
            doc.declare_namespace(xe, p, u);
        }
        doc.append_text(xe, x);
    }
    if !spec.xpath2_filter.is_empty() {
        let t = doc.append_element(ts, q("Transform"));
        doc.set_attribute(t, QName::local("Algorithm"), XPATH2_FILTER);
        for (f, x) in &spec.xpath2_filter {
            let xe = doc.append_element(t, QName::ns("dsig-xpath", "XPath", XPATH2_FILTER));
            doc.declare_namespace(xe, "dsig-xpath", XPATH2_FILTER);
            for (p, u) in &spec.xpath_namespaces {
                doc.declare_namespace(xe, p, u);
            }
            doc.set_attribute(xe, QName::local("Filter"), f.as_str());
            doc.append_text(xe, x);
        }
    }
    if enveloped {
        let t = doc.append_element(ts, q("Transform"));
        doc.set_attribute(t, QName::local("Algorithm"), ENVELOPED);
    }
    let t2 = doc.append_element(ts, q("Transform"));
    doc.set_attribute(t2, QName::local("Algorithm"), spec.c14n.as_str());
    let dm = doc.append_element(r, q("DigestMethod"));
    doc.set_attribute(dm, QName::local("Algorithm"), SHA256);
    let dv = doc.append_element(r, q("DigestValue"));
    doc.append_text(dv, digest);
}

/// The canonical bytes of a reference target per the spec's transforms.
fn reference_digest(doc: &Document, target: NodeId, exclude: Option<NodeId>, spec: &SignSpec) -> Result<String> {
    let mut opts = C14nOptions::for_algorithm(&spec.c14n).ok_or_else(|| CamelError::illegal_argument(format!("Unsupported canonicalization method {}", spec.c14n)))?;
    opts.exclude = exclude;
    if let Some(x) = &spec.xpath_filter {
        opts.include = Some(xpath_filter_set(doc, target, x, &spec.xpath_namespaces)?);
    }
    if !spec.xpath2_filter.is_empty() {
        let s2 = xpath2_filter_set(doc, &spec.xpath2_filter, &spec.xpath_namespaces)?;
        opts.include = Some(match opts.include.take() {
            Some(s) => s.intersection(&s2).copied().collect(),
            None => s2,
        });
    }
    Ok(base64_encode(&HashAlg::Sha256.hash(canonicalize(doc, target, &opts).as_bytes())))
}

/// Sign a document according to `spec`; returns the signed document (a new document for
/// enveloping signatures).
pub fn sign_document_with(mut doc: Document, key: &PrivateKey, spec: &SignSpec) -> Result<Document> {
    let alg = key_alg(key)?;
    let opts = C14nOptions::for_algorithm(&spec.c14n).ok_or_else(|| CamelError::illegal_argument(format!("Unsupported canonicalization method {}", spec.c14n)))?;
    let prefix = spec.prefix.clone();
    let q = move |l: &str| QName::ns(prefix.as_str(), l, DSIG_NS);
    let (mut out, sig, refs): (Document, NodeId, Vec<(String, String, bool)>) = match &spec.kind {
        SignatureKind::Enveloped(parent) => {
            let digest = reference_digest(&doc, 0, None, spec)?;
            let sig = doc.append_element(*parent, q("Signature"));
            doc.declare_namespace(sig, &spec.prefix, DSIG_NS);
            (doc, sig, vec![(String::new(), digest, true)])
        }
        SignatureKind::Detached { ids, parent } => {
            let mut refs = Vec::new();
            let mut last = None;
            for id in ids {
                let t = find_by_id(&doc, id).ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("No element with Id '{}' to sign", id)))?;
                refs.push((format!("#{}", id), reference_digest(&doc, t, None, spec)?, false));
                last = Some(last.map_or(t, |l: NodeId| l.max(t)));
            }
            let sig = match (parent, last) {
                (Some(p), _) => doc.append_element(*p, q("Signature")),
                (None, Some(l)) => {
                    let p = doc.parent(l).unwrap_or(0);
                    let s = doc.append_element(p, q("Signature"));
                    // move right after the last signed element
                    let kids = &mut doc.node_mut(p).children;
                    kids.retain(|&c| c != s);
                    let pos = kids.iter().position(|&c| c == l).map(|i| i + 1).unwrap_or(kids.len());
                    kids.insert(pos, s);
                    s
                }
                (None, None) => return Err(CamelError::illegal_argument("Detached signature without elements to sign")),
            };
            doc.declare_namespace(sig, &spec.prefix, DSIG_NS);
            (doc, sig, refs)
        }
        SignatureKind::Enveloping { object_id } => {
            let root = doc.document_element().ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, "No document element to sign"))?;
            let mut out = Document::new();
            let sig = out.append_element(0, q("Signature"));
            out.declare_namespace(sig, &spec.prefix, DSIG_NS);
            let obj = out.append_element(sig, q("Object"));
            out.set_attribute(obj, QName::local("Id"), object_id.as_str());
            out.import_node(&doc, root, obj);
            let digest = reference_digest(&out, obj, None, spec)?;
            (out, sig, vec![(format!("#{}", object_id), digest, false)])
        }
    };
    if let Some(id) = &spec.signature_id {
        out.set_attribute(sig, QName::local("Id"), id.as_str());
    }
    let si = out.append_element(sig, q("SignedInfo"));
    let cm = out.append_element(si, q("CanonicalizationMethod"));
    out.set_attribute(cm, QName::local("Algorithm"), spec.c14n.as_str());
    let sm = out.append_element(si, q("SignatureMethod"));
    out.set_attribute(sm, QName::local("Algorithm"), alg);
    for (uri, digest, enveloped) in &refs {
        write_reference(&mut out, si, &q, uri, digest, *enveloped, spec);
    }
    let signed_info = canonicalize(&out, si, &C14nOptions { exclude: None, include: None, ..opts.clone() });
    let (value, _) = sign_bytes(key, signed_info.as_bytes())?;
    let sv = out.append_element(sig, q("SignatureValue"));
    out.append_text(sv, &base64_encode(&value));
    write_key_info(&mut out, sig, &q, key, spec);
    // ds:Object (enveloping) goes last
    if matches!(spec.kind, SignatureKind::Enveloping { .. }) {
        let kids = &mut out.node_mut(sig).children;
        let obj = kids.remove(0);
        kids.push(obj);
    }
    Ok(out)
}

/// Sign a document (enveloped signature appended to `parent`). Returns the signature element.
pub fn sign_document(doc: &mut Document, parent: NodeId, key: &PrivateKey, cert: Option<&Certificate>, c14n_alg: &str, prefix: &str) -> Result<NodeId> {
    let spec = SignSpec { c14n: c14n_alg.to_string(), prefix: prefix.to_string(), certificates: cert.into_iter().cloned().collect(), ..SignSpec::enveloped(parent) };
    let signed = sign_document_with(std::mem::take(doc), key, &spec)?;
    *doc = signed;
    find_signature(doc).ok_or_else(|| sig_err(XML_SIGNATURE_EXCEPTION, "Signature not created"))
}

// ------------------------------------------------------------------ verification

fn dsig_child(doc: &Document, el: NodeId, local: &str) -> Option<NodeId> {
    doc.child_elements_named(el, DSIG_NS, local).next()
}

fn find_signature(doc: &Document) -> Option<NodeId> {
    (0..doc.nodes.len()).find(|&i| {
        let n = doc.node(i);
        n.ty == NodeType::Element && n.name.uri == DSIG_NS && n.name.local == "Signature" && attached(doc, i)
    })
}

fn clean_b64(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// The keys a signature's `KeyInfo` carries: certificates (DER) and a `KeyValue` key.
pub struct KeyInfoData {
    pub certificates: Vec<Vec<u8>>,
    pub key_value: Option<PublicKey>,
    pub key_name: Option<String>,
}

/// Read the `ds:KeyInfo` of a signature.
pub fn key_info(doc: &Document, sig: NodeId) -> KeyInfoData {
    let mut out = KeyInfoData { certificates: Vec::new(), key_value: None, key_name: None };
    let Some(ki) = dsig_child(doc, sig, "KeyInfo") else { return out };
    out.key_name = dsig_child(doc, ki, "KeyName").map(|k| doc.string_value(k).trim().to_string());
    for xd in doc.child_elements_named(ki, DSIG_NS, "X509Data") {
        for c in doc.child_elements_named(xd, DSIG_NS, "X509Certificate") {
            if let Some(der) = base64_decode(&clean_b64(&doc.string_value(c))) {
                out.certificates.push(der);
            }
        }
    }
    if let Some(kv) = dsig_child(doc, ki, "KeyValue") {
        if let Some(rk) = dsig_child(doc, kv, "RSAKeyValue") {
            let m = dsig_child(doc, rk, "Modulus").and_then(|m| base64_decode(&clean_b64(&doc.string_value(m))));
            let e = dsig_child(doc, rk, "Exponent").and_then(|e| base64_decode(&clean_b64(&doc.string_value(e))));
            if let (Some(m), Some(e)) = (m, e) {
                out.key_value = camel_tls::crypto::rsa::RsaPublicKey::new(&m, &e).map(PublicKey::Rsa);
            }
        } else if let Some(ek) = doc.child_elements_named(kv, DSIG11_NS, "ECKeyValue").next() {
            let curve = doc.child_elements_named(ek, DSIG11_NS, "NamedCurve").next().and_then(|n| doc.attribute(n, "URI")).and_then(|u| camel_tls::keys::curve_from_oid(u.trim_start_matches("urn:oid:")));
            let point = doc.child_elements_named(ek, DSIG11_NS, "PublicKey").next().and_then(|p| base64_decode(&clean_b64(&doc.string_value(p))));
            if let (Some(curve), Some(point)) = (curve, point) {
                out.key_value = Some(PublicKey::Ec { curve, point });
            }
        }
    }
    out
}

/// How a signature is verified.
#[derive(Default)]
pub struct VerifySpec<'a> {
    /// The key (certificate / public key option).
    pub key: Option<&'a PublicKey>,
    /// Trust the certificate / key value of the signature's own `KeyInfo`.
    pub trust_key_info: bool,
    /// A key store: certificates and key entries usable as verification keys.
    pub keystore: Option<&'a KeyStore>,
    /// Restrict the key store to this alias.
    pub alias: Option<&'a str>,
}

/// The result of a verification.
pub struct Verified {
    /// The `ds:Signature` element.
    pub signature: NodeId,
    /// For an enveloping signature: the signed `ds:Object`.
    pub object: Option<NodeId>,
}

fn keystore_keys(ks: &KeyStore, alias: Option<&str>) -> Vec<(String, PublicKey, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    for e in &ks.entries {
        match e {
            camel_tls::keystore::KeyStoreEntry::Key { alias: a, key, chain } if alias.is_none_or(|x| x.eq_ignore_ascii_case(a)) => out.push((a.clone(), key.public_key(), chain.first().cloned())),
            camel_tls::keystore::KeyStoreEntry::Cert { alias: a, der } if alias.is_none_or(|x| x.eq_ignore_ascii_case(a)) => {
                if let Some(pk) = Certificate::from_der(der).ok().and_then(|c| c.public_key) {
                    out.push((a.clone(), pk, Some(der.clone())));
                }
            }
            _ => {}
        }
    }
    out
}

/// Verify the (first) signature of a document: enveloped, enveloping or detached.
pub fn verify_document_with(doc: &Document, spec: &VerifySpec) -> Result<Verified> {
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
    let mut object = None;
    for r in refs {
        let uri = doc.attribute(r, "URI").unwrap_or("");
        let target = if uri.is_empty() {
            0
        } else if let Some(id) = uri.strip_prefix('#') {
            let t = find_by_id(doc, id).ok_or_else(|| fmt(&format!("Reference target #{} not found", id)))?;
            if doc.namespace_uri(t) == DSIG_NS && doc.local_name(t) == "Object" && doc.parent(t) == Some(sig) {
                object = Some(t);
            }
            t
        } else {
            return Err(fmt(&format!("Unsupported reference URI {} (only same-document references)", uri)));
        };
        let mut opts = C14nOptions::default();
        if let Some(ts) = dsig_child(doc, r, "Transforms") {
            for t in doc.child_elements_named(ts, DSIG_NS, "Transform") {
                let a = doc.attribute(t, "Algorithm").unwrap_or("");
                if a == ENVELOPED {
                    opts.exclude = Some(sig);
                } else if a == XPATH_FILTER {
                    let xe = dsig_child(doc, t, "XPath").ok_or_else(|| fmt("XPath transform without ds:XPath"))?;
                    let ns: Vec<(String, String)> = doc.in_scope_namespaces(xe);
                    let set = xpath_filter_set(doc, target, &doc.string_value(xe), &ns)?;
                    opts.include = Some(match opts.include.take() {
                        Some(s) => s.intersection(&set).copied().collect(),
                        None => set,
                    });
                } else if a == XPATH2_FILTER {
                    let mut steps = Vec::new();
                    let mut ns = Vec::new();
                    for xe in doc.child_elements_named(t, XPATH2_FILTER, "XPath") {
                        steps.push((doc.attribute(xe, "Filter").unwrap_or("intersect").to_string(), doc.string_value(xe)));
                        ns.extend(doc.in_scope_namespaces(xe));
                    }
                    let set = xpath2_filter_set(doc, &steps, &ns)?;
                    opts.include = Some(match opts.include.take() {
                        Some(s) => s.intersection(&set).copied().collect(),
                        None => set,
                    });
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
        if actual != clean_b64(&expected) {
            return Err(sig_err(XML_SIGNATURE_INVALID_CONTENT_HASH_EXCEPTION, format!("The digest of reference '{}' does not match: the signed content was changed", uri)));
        }
    }
    let sv = clean_b64(&dsig_child(doc, sig, "SignatureValue").map(|s| doc.string_value(s)).ok_or_else(|| fmt("ds:SignatureValue is missing"))?);
    let sig_bytes = base64_decode(&sv).ok_or_else(|| fmt("ds:SignatureValue is not base64"))?;
    let signed_info = canonicalize(doc, si, &c14n);
    let check = |k: &PublicKey| verify_bytes(k, alg, signed_info.as_bytes(), &sig_bytes);
    let ok = if let Some(k) = spec.key {
        check(k)?
    } else if let Some(ks) = spec.keystore {
        let info = key_info(doc, sig);
        let candidates = keystore_keys(ks, spec.alias);
        if candidates.is_empty() {
            return Err(sig_err(XML_SIGNATURE_INVALID_KEY_EXCEPTION, "The key store holds no usable key"));
        }
        // prefer the KeyInfo certificate / KeyName, then try every key
        let preferred: Vec<&(String, PublicKey, Option<Vec<u8>>)> = candidates
            .iter()
            .filter(|(a, _, der)| der.as_ref().map(|d| info.certificates.contains(d)).unwrap_or(false) || info.key_name.as_deref().map(|n| n.eq_ignore_ascii_case(a)).unwrap_or(false))
            .collect();
        if !info.certificates.is_empty() && preferred.is_empty() && info.key_name.is_none() && candidates.iter().all(|(_, _, d)| d.is_some()) {
            return Err(sig_err(XML_SIGNATURE_INVALID_KEY_EXCEPTION, "The certificate of the signature is not in the key store"));
        }
        let mut any = false;
        for (_, k, _) in preferred.into_iter().chain(candidates.iter()) {
            if check(k)? {
                any = true;
                break;
            }
        }
        any
    } else if spec.trust_key_info {
        let info = key_info(doc, sig);
        let key = match info.certificates.first() {
            Some(der) => Certificate::from_der(der).map_err(|e| fmt(&e.to_string()))?.public_key.ok_or_else(|| fmt("Unsupported certificate key"))?,
            None => info.key_value.ok_or_else(|| fmt("No ds:X509Certificate or ds:KeyValue in ds:KeyInfo"))?,
        };
        check(&key)?
    } else {
        return Err(CamelError::illegal_argument("xmlsecurity-verify: no verification key (certificate, publicKey or keyStore option)"));
    };
    if !ok {
        return Err(sig_err(XML_SIGNATURE_INVALID_VALUE_EXCEPTION, "The XML signature value is invalid"));
    }
    Ok(Verified { signature: sig, object })
}

/// Verify the (first) signature of a document. Returns the signature element.
pub fn verify_document(doc: &Document, key: Option<&PublicKey>, trust_key_info: bool) -> Result<NodeId> {
    verify_document_with(doc, &VerifySpec { key, trust_key_info, ..Default::default() }).map(|v| v.signature)
}

// ------------------------------------------------------------------ component

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
    keystore: Option<String>,
    keystore_password: Option<String>,
    key_password: Option<String>,
    alias: Option<String>,
    c14n: String,
    parent_local: Option<String>,
    parent_ns: Option<String>,
    parent_xpath: Option<String>,
    id_xpaths: Vec<String>,
    object_id: Option<String>,
    enveloping: bool,
    key_value: bool,
    key_name: Option<String>,
    signature_id: Option<String>,
    xpath_filter: Option<String>,
    xpath2_filter: Vec<(String, String)>,
    xpath_ns: Vec<(String, String)>,
    prefix: String,
    omit_decl: bool,
    trust_key_info: bool,
    remove_signature: bool,
}

fn pairs(s: &str) -> Vec<(String, String)> {
    s.split(',').filter_map(|p| p.split_once('=')).map(|(a, b)| (a.trim().to_string(), b.trim().to_string())).collect()
}

impl Component for XmlSecurityComponent {
    fn create_endpoint(&self, ctx: &CamelContext, uri: &str, _remaining: &str, params: &mut Parameters) -> Result<Arc<dyn Endpoint>> {
        crate::register_kinds();
        let take_key = |params: &mut Parameters, names: &[&str]| names.iter().find_map(|n| params.take(n)).filter(|s| !s.is_empty());
        let cfg = Config {
            private_key: take_key(params, &["privateKey"]),
            certificate: take_key(params, &["certificate"]),
            public_key: take_key(params, &["publicKey"]),
            keystore: take_key(params, &["keyStore", "keyAccessor", "keySelector"]),
            keystore_password: params.take("keyStorePassword"),
            key_password: params.take("keyPassword"),
            alias: params.take("alias"),
            c14n: params.take("canonicalizationMethod").unwrap_or_else(|| EXC_C14N.into()),
            parent_local: params.take("parentLocalName"),
            parent_ns: params.take("parentNamespace"),
            parent_xpath: params.take("parentXpath"),
            id_xpaths: params.take("xpathsToIdAttributes").map(|s| s.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()).unwrap_or_default(),
            object_id: params.take("contentObjectId"),
            enveloping: params.take_bool("envelopingSignature", false)?,
            key_value: params.take_bool("addKeyValue", false)?,
            key_name: params.take("keyName"),
            signature_id: params.take("signatureId").filter(|s| !s.is_empty()),
            xpath_filter: params.take("xpathFilter"),
            xpath2_filter: params
                .take("xpath2Filter")
                .map(|s| s.split(';').filter_map(|p| p.split_once(':')).map(|(f, x)| (f.trim().to_string(), x.trim().to_string())).collect())
                .unwrap_or_default(),
            xpath_ns: params.take("xpathNamespaces").map(|s| pairs(&s)).unwrap_or_default(),
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
            if d != SHA256 {
                return Err(CamelError::illegal_argument(format!("Unsupported digestAlgorithm {} (sha256)", d)));
            }
        }
        if C14nOptions::for_algorithm(&cfg.c14n).is_none() {
            return Err(CamelError::illegal_argument(format!("Unsupported canonicalizationMethod {}", cfg.c14n)));
        }
        for k in ["transformMethods", "contentReferenceUri", "contentReferenceType", "addKeyInfoReference", "clearHeaders", "outputXmlEncoding", "baseUri", "cryptoContextProperties", "disallowDoctypeDecl", "schemaResourceUri", "lazyStartProducer", "uriDereferencer", "validationFailedHandler", "xmlSignature2Message", "outputNodeSearch", "outputNodeSearchType", "secureValidation", "plainText", "plainTextEncoding", "properties"] {
            params.take(k);
        }
        if self.sign && cfg.private_key.is_none() && cfg.keystore.is_none() {
            return Err(CamelError::illegal_argument("xmlsecurity-sign requires the privateKey (or keyStore) option"));
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
        let keystore = match &self.cfg.keystore {
            Some(k) => Some(load_keystore(c, k, self.cfg.keystore_password.as_deref(), self.cfg.key_password.as_deref())?),
            None => None,
        };
        let mut key = match &self.cfg.private_key {
            Some(p) => Some(PrivateKey::from_pem(&load_pem(c, p)?).map_err(CamelError::illegal_argument)?),
            None => None,
        };
        let mut certs: Vec<Certificate> = match &self.cfg.certificate {
            Some(p) if self.sign => vec![load_certificate(c, p)?],
            _ => Vec::new(),
        };
        if self.sign && key.is_none() {
            let ks = keystore.as_ref().ok_or_else(|| CamelError::illegal_argument("xmlsecurity-sign requires a private key"))?;
            let (k, chain) = ks.key_entry(self.cfg.alias.as_deref()).ok_or_else(|| CamelError::illegal_argument(format!("No key entry{} in the key store", self.cfg.alias.as_ref().map(|a| format!(" '{}'", a)).unwrap_or_default())))?;
            key = Some(k.clone());
            if certs.is_empty() {
                if let Some(leaf) = chain.first() {
                    certs.push(Certificate::from_der(leaf).map_err(|e| CamelError::illegal_argument(e.to_string()))?);
                }
            }
        }
        let public = match (&self.cfg.public_key, &self.cfg.certificate) {
            (Some(p), _) => Some(load_public_key(c, p)?),
            (None, Some(p)) if !self.sign => Some(load_public_key(c, p)?),
            _ => None,
        };
        Ok(Arc::new(XmlSecurityProducer { sign: self.sign, cfg: self.cfg.clone(), key, certs, public, keystore }))
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}

struct XmlSecurityProducer {
    sign: bool,
    cfg: Config,
    key: Option<PrivateKey>,
    certs: Vec<Certificate>,
    public: Option<PublicKey>,
    keystore: Option<Arc<KeyStore>>,
}

impl Service for XmlSecurityProducer {}

impl XmlSecurityProducer {
    fn xpath_nodes(&self, doc: &Document, expr: &str) -> Result<Vec<NodeId>> {
        let xp = crate::xpath::XPath::compile_ns(expr, &self.cfg.xpath_ns).map_err(xpath_error)?;
        let docs = crate::xpath::Docs::new(doc.clone());
        match xp.eval_docs(&docs, &crate::xpath::NoEnv).map_err(xpath_error)? {
            crate::xpath::XValue::Nodes(ns) => Ok(ns.into_iter().filter(|n| !n.is_namespace()).map(|n| n.node_id()).collect()),
            _ => Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("XPath '{}' must select nodes", expr))),
        }
    }

    fn kind(&self, doc: &Document) -> Result<SignatureKind> {
        let root = doc.document_element().ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, "No document element"))?;
        let parent = if let Some(l) = &self.cfg.parent_local {
            Some(
                (0..doc.nodes.len())
                    .find(|&i| doc.node_type(i) == NodeType::Element && doc.local_name(i) == l && self.cfg.parent_ns.as_ref().map(|n| doc.namespace_uri(i) == n).unwrap_or(true))
                    .ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("Parent element '{}' for the signature not found", l)))?,
            )
        } else if let Some(x) = &self.cfg.parent_xpath {
            Some(*self.xpath_nodes(doc, x)?.first().ok_or_else(|| sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, format!("Parent XPath '{}' selects no element", x)))?)
        } else {
            None
        };
        if !self.cfg.id_xpaths.is_empty() {
            let mut ids = Vec::new();
            for x in &self.cfg.id_xpaths {
                for n in self.xpath_nodes(doc, x)? {
                    let v = if doc.node_type(n) == NodeType::Attribute { doc.value(n).to_string() } else { doc.string_value(n) };
                    if !ids.contains(&v) {
                        ids.push(v);
                    }
                }
            }
            if ids.is_empty() {
                return Err(sig_err(XML_SIGNATURE_FORMAT_EXCEPTION, "xpathsToIdAttributes selected no Id attribute"));
            }
            return Ok(SignatureKind::Detached { ids, parent });
        }
        if let Some(p) = parent {
            return Ok(SignatureKind::Enveloped(p));
        }
        if self.cfg.enveloping || self.cfg.object_id.is_some() {
            return Ok(SignatureKind::Enveloping { object_id: self.cfg.object_id.clone().unwrap_or_else(|| format!("_{}", camel_core::message::generate_id())) });
        }
        Ok(SignatureKind::Enveloped(root))
    }
}

impl Processor for XmlSecurityProducer {
    fn process(&self, ex: &mut Exchange) -> Result<()> {
        let mut doc = crate::xslt_component::body_document(&ex.message.body)?;
        let out_opts = OutputOptions { method: Some(crate::serialize::Method::Xml), omit_xml_declaration: self.cfg.omit_decl, ..Default::default() };
        if self.sign {
            let spec = SignSpec {
                kind: self.kind(&doc)?,
                c14n: self.cfg.c14n.clone(),
                prefix: self.cfg.prefix.clone(),
                certificates: self.certs.clone(),
                key_value: self.cfg.key_value,
                key_name: self.cfg.key_name.clone(),
                signature_id: self.cfg.signature_id.clone(),
                xpath_filter: self.cfg.xpath_filter.clone(),
                xpath2_filter: self.cfg.xpath2_filter.clone(),
                xpath_namespaces: self.cfg.xpath_ns.clone(),
            };
            let signed = sign_document_with(doc, self.key.as_ref().unwrap(), &spec)?;
            ex.message.body = Value::String(serialize(&signed, 0, &out_opts));
        } else {
            let v = verify_document_with(
                &doc,
                &VerifySpec { key: self.public.as_ref(), trust_key_info: self.cfg.trust_key_info, keystore: self.keystore.as_deref(), alias: self.cfg.alias.as_deref() },
            )?;
            if self.cfg.remove_signature {
                if let Some(obj) = v.object {
                    // enveloping: the object's content
                    let mut out = Document::new();
                    for &c in doc.children(obj).to_vec().iter() {
                        if doc.node_type(c) == NodeType::Element {
                            if let Some(el) = out.import_node(&doc, c, 0) {
                                // drop the inherited signature namespace when the content does not use it
                                let uses_dsig = subtree(&out, el).iter().any(|&n| out.node_type(n) == NodeType::Element && (out.namespace_uri(n) == DSIG_NS || out.attributes(n).iter().any(|&a| out.namespace_uri(a) == DSIG_NS)));
                                if !uses_dsig {
                                    out.node_mut(el).ns_decls.retain(|(_, u)| u != DSIG_NS);
                                }
                            }
                        }
                    }
                    ex.message.body = Value::String(serialize(&out, 0, &out_opts));
                } else {
                    if let Some(p) = doc.parent(v.signature) {
                        doc.node_mut(p).children.retain(|&c| c != v.signature);
                    }
                    ex.message.body = Value::String(serialize(&doc, 0, &out_opts));
                }
            }
        }
        Ok(())
    }
}
