//! Key Vault keys and certificates for [`AzureMock`] (bearer token for
//! `https://vault.azure.net/.default` required, `api-version` checked): EC keys (P-256, P-384)
//! created with camel-tls, `sign` / `verify` (ES256 / ES384 over the given digest, raw `r||s`
//! signatures), and certificates issued by the emulator (self-signed P-256 via camel-tls
//! `certgen`, created as a pending operation completed on the first poll) or imported from PEM.
//! RSA and symmetric (`oct` / `oct-HSM`) keys, `encrypt` / `decrypt` / `wrapkey` /
//! `unwrapkey`, RSA signatures and key import: see [`super::crypto`] (generated RSA keys share
//! one fixed key pair).

use std::collections::BTreeMap;

use camel_core::value::{ObjectMap, Value};
use camel_tls::crypto::ec::EcCurve;
use camel_tls::crypto::hash::{Digest, HashAlg, Sha1};

use super::{bearer_ok, kv_error, AzureMock, Shared, TENANT_ID};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_encode, base64url_encode, now_secs, obj, parse_json, random_hex};

struct KeyVersion {
    version: String,
    /// `EC`, `RSA` or `oct` (the `-HSM` suffix kept in `kty_name`)
    kty_name: String,
    rsa: Option<super::crypto::Rsa>,
    oct: Option<Vec<u8>>,
    curve: EcCurve,
    private: Vec<u8>,
    public: Vec<u8>,
    ops: Vec<String>,
    created: i64,
}

struct CertVersion {
    version: String,
    der: Vec<u8>,
    subject: String,
    created: i64,
}

#[derive(Default)]
pub(super) struct KeysState {
    keys: BTreeMap<String, Vec<KeyVersion>>,
    /// soft-deleted keys (recoverable / purgeable)
    deleted_keys: BTreeMap<String, Vec<KeyVersion>>,
    certs: BTreeMap<String, Vec<CertVersion>>,
    pending: BTreeMap<String, (String, bool)>,
}

impl AzureMock {
    /// Names of the Key Vault keys.
    pub fn key_names(&self) -> Vec<String> {
        self.state.lock().unwrap().keys.keys.keys().cloned().collect()
    }
    /// DER of the latest version of a Key Vault certificate.
    pub fn certificate_der(&self, name: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().keys.certs.get(name).and_then(|v| v.last()).map(|c| c.der.clone())
    }
}

fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut t: String = s.trim().replace('-', "+").replace('_', "/");
    while t.len() % 4 != 0 {
        t.push('=');
    }
    crate::util::base64_decode(&t)
}

fn crv_name(c: EcCurve) -> &'static str {
    match c {
        EcCurve::P256 => "P-256",
        EcCurve::P384 => "P-384",
    }
}

fn key_bundle(base: &str, name: &str, k: &KeyVersion) -> Value {
    let n = k.curve.byte_len();
    let mut key = ObjectMap::new();
    key.insert("kid".into(), Value::from(format!("{}/keys/{}/{}", base, name, k.version)));
    key.insert("kty".into(), Value::from(k.kty_name.clone()));
    if let Some(r) = &k.rsa {
        key.insert("n".into(), Value::from(base64url_encode(&r.n)));
        key.insert("e".into(), Value::from(base64url_encode(&r.e)));
    } else if k.oct.is_none() {
        key.insert("crv".into(), Value::from(crv_name(k.curve)));
        key.insert("x".into(), Value::from(base64url_encode(&k.public[1..1 + n])));
        key.insert("y".into(), Value::from(base64url_encode(&k.public[1 + n..])));
    }
    key.insert("key_ops".into(), Value::from(k.ops.clone()));
    obj([
        ("key", Value::Object(key)),
        ("attributes", obj([("enabled", Value::Bool(true)), ("created", Value::Int(k.created)), ("updated", Value::Int(k.created)), ("recoveryLevel", Value::from("Recoverable+Purgeable"))])),
    ])
}

fn cert_bundle(base: &str, name: &str, c: &CertVersion) -> Value {
    let mut sha1 = Sha1::new();
    sha1.update(&c.der);
    obj([
        ("id", Value::from(format!("{}/certificates/{}/{}", base, name, c.version))),
        ("kid", Value::from(format!("{}/keys/{}/{}", base, name, c.version))),
        ("sid", Value::from(format!("{}/secrets/{}/{}", base, name, c.version))),
        ("x5t", Value::from(base64url_encode(&sha1.finish()))),
        ("cer", Value::from(base64_encode(&c.der))),
        ("attributes", obj([("enabled", Value::Bool(true)), ("created", Value::Int(c.created)), ("updated", Value::Int(c.created))])),
        ("policy", obj([("x509_props", obj([("subject", Value::from(c.subject.clone()))])), ("issuer", obj([("name", Value::from("Self"))]))])),
    ])
}

fn ok(v: Value) -> Response {
    Response::json(200, v.to_json())
}

fn deleted_key_bundle(base: &str, name: &str, k: &KeyVersion) -> Value {
    let mut b = key_bundle(base, name, k).as_object().cloned().unwrap_or_default();
    b.insert("recoveryId".into(), Value::from(format!("{}/deletedkeys/{}", base, name)));
    b.insert("deletedDate".into(), Value::Int(crate::util::now_secs()));
    b.insert("scheduledPurgeDate".into(), Value::Int(crate::util::now_secs() + 90 * 86400));
    // list items (DeletedKeyItem) carry the key id at the top level
    b.insert("kid".into(), Value::from(format!("{}/keys/{}/{}", base, name, k.version)));
    Value::Object(b)
}

pub(super) fn handle(st: &Shared, req: &ServerRequest, segs: &[String]) -> Option<Response> {
    if !matches!(segs.first().map(|s| s.as_str()), Some("keys") | Some("certificates") | Some("deletedkeys")) {
        return None;
    }
    let mut s = st.lock().unwrap();
    if !bearer_ok(&s, req, "https://vault.azure.net/.default") {
        s.auth_failures += 1;
        return Some(
            kv_error(401, "Unauthorized", "AKV10000: Request is missing a Bearer or PoP token.")
                .with_header("WWW-Authenticate", format!("Bearer authorization=\"https://login.microsoftonline.com/{}\", resource=\"https://vault.azure.net\"", TENANT_ID)),
        );
    }
    if req.query_param("api-version").is_none_or(|v| v.is_empty()) {
        return Some(kv_error(400, "BadParameter", "The 'api-version' query parameter is missing."));
    }
    let base = format!("{}://{}", if s.tls { "https" } else { "http" }, req.header("Host").unwrap_or("localhost"));
    let body = parse_json(&req.body).unwrap_or_default();
    let st_ = |k: &str| body.get(k).map(|v| v.to_string()).unwrap_or_default();
    let segs: Vec<&str> = segs.iter().map(|x| x.as_str()).collect();
    let ks = &mut s.keys;
    let key_nf = |n: &str| kv_error(404, "KeyNotFound", &format!("A key with (name/id) {} was not found in this key vault.", n));
    let cert_nf = |n: &str| kv_error(404, "CertificateNotFound", &format!("A certificate with (name/id) {} was not found in this key vault.", n));
    let r = match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["keys"]) => ok(obj([("value", Value::Array(ks.keys.keys().map(|n| obj([("kid", Value::from(format!("{}/keys/{}", base, n)))])).collect()))])),
        ("POST", ["keys", name, "create"]) if !st_("kty").starts_with("EC") => {
            let kty = st_("kty");
            let user_ops = body.get("key_ops").and_then(|o| o.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect::<Vec<_>>());
            let k = if kty.starts_with("RSA") {
                let size = body.get("key_size").and_then(|v| v.as_i64()).unwrap_or(2048);
                if size != 2048 {
                    return Some(kv_error(400, "BadParameter", "The emulator creates 2048-bit RSA keys only"));
                }
                KeyVersion { version: random_hex(16), kty_name: kty.clone(), rsa: Some(super::crypto::Rsa::fixed()), oct: None, curve: EcCurve::P256, private: vec![], public: vec![], ops: user_ops.unwrap_or_else(|| ["encrypt", "decrypt", "wrapKey", "unwrapKey", "sign", "verify"].map(String::from).to_vec()), created: now_secs() }
            } else if kty.starts_with("oct") {
                let bits = body.get("key_size").and_then(|v| v.as_i64()).unwrap_or(256);
                if ![128, 192, 256].contains(&bits) {
                    return Some(kv_error(400, "BadParameter", "Key size must be 128, 192 or 256 bits"));
                }
                KeyVersion { version: random_hex(16), kty_name: kty.clone(), rsa: None, oct: Some(camel_tls::crypto::rng::random_bytes(bits as usize / 8)), curve: EcCurve::P256, private: vec![], public: vec![], ops: user_ops.unwrap_or_else(|| ["encrypt", "decrypt", "wrapKey", "unwrapKey"].map(String::from).to_vec()), created: now_secs() }
            } else {
                return Some(kv_error(400, "BadParameter", &format!("Unknown key type {}", kty)));
            };
            let b = key_bundle(&base, name, &k);
            ks.keys.entry(name.to_string()).or_default().push(k);
            ok(b)
        }
        ("PUT", ["keys", name]) => {
            // import a JWK (RSA with n / e / d, or oct with k)
            let jwk = body.get("key").cloned().unwrap_or_default();
            let g = |k: &str| jwk.get(k).map(|v| v.to_string()).and_then(|v| b64url_decode(&v));
            let kty = jwk.get("kty").map(|v| v.to_string()).unwrap_or_default();
            let ops = jwk.get("key_ops").and_then(|o| o.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect::<Vec<_>>());
            let k = if kty.starts_with("RSA") {
                let (Some(n), Some(e), Some(d)) = (g("n"), g("e"), g("d")) else { return Some(kv_error(400, "BadParameter", "An RSA key import needs n, e and d")) };
                KeyVersion { version: random_hex(16), kty_name: kty, rsa: Some(super::crypto::Rsa { n, e, d }), oct: None, curve: EcCurve::P256, private: vec![], public: vec![], ops: ops.unwrap_or_else(|| ["encrypt", "decrypt", "wrapKey", "unwrapKey", "sign", "verify"].map(String::from).to_vec()), created: now_secs() }
            } else if kty.starts_with("oct") {
                let Some(key) = g("k").filter(|k| [16, 24, 32].contains(&k.len())) else { return Some(kv_error(400, "BadParameter", "An oct key import needs a 128/192/256-bit k")) };
                KeyVersion { version: random_hex(16), kty_name: kty, rsa: None, oct: Some(key), curve: EcCurve::P256, private: vec![], public: vec![], ops: ops.unwrap_or_else(|| ["encrypt", "decrypt", "wrapKey", "unwrapKey"].map(String::from).to_vec()), created: now_secs() }
            } else {
                return Some(kv_error(400, "BadParameter", &format!("The emulator imports RSA and oct keys only, not {}", kty)));
            };
            let b = key_bundle(&base, name, &k);
            ks.keys.entry(name.to_string()).or_default().push(k);
            ok(b)
        }
        ("POST", ["keys", name, rest @ ..]) if matches!(rest.last(), Some(&"encrypt") | Some(&"decrypt") | Some(&"wrapkey") | Some(&"unwrapkey")) => {
            let action = *rest.last().unwrap();
            let Some(versions) = ks.keys.get(*name) else { return Some(key_nf(name)) };
            let k = if rest.len() == 1 { versions.last() } else { versions.iter().find(|k| k.version == rest[0]) };
            let Some(k) = k else { return Some(key_nf(name)) };
            let op_name = match action {
                "wrapkey" => "wrapKey",
                "unwrapkey" => "unwrapKey",
                a => a,
            };
            if !k.ops.iter().any(|o| o == op_name) {
                return Some(kv_error(403, "Forbidden", &format!("Operation {} is not permitted on this key.", op_name)));
            }
            let alg = st_("alg");
            let Some(value) = b64url_decode(&st_("value")) else { return Some(kv_error(400, "BadParameter", "Invalid value")) };
            let kid = format!("{}/keys/{}/{}", base, name, k.version);
            let bad = |m: &str| kv_error(400, "BadParameter", m);
            if let Some(r) = &k.rsa {
                let res = if matches!(action, "encrypt" | "wrapkey") { r.encrypt(&alg, &value) } else { r.decrypt(&alg, &value) };
                match res {
                    Ok(out) => ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&out)))])),
                    Err(e) => bad(&e),
                }
            } else if let Some(key) = &k.oct {
                if super::crypto::oct_key_len(&alg) != Some(key.len()) {
                    return Some(bad(&format!("Algorithm {} is not valid for this key.", alg)));
                }
                let aad = body.get("aad").map(|v| v.to_string()).and_then(|v| b64url_decode(&v)).unwrap_or_default();
                match (action, alg.ends_with("GCM")) {
                    ("encrypt", true) => {
                        let (c, iv, tag) = super::crypto::gcm_encrypt(key, &aad, &value);
                        ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&c))), ("iv", Value::from(base64url_encode(&iv))), ("tag", Value::from(base64url_encode(&tag)))]))
                    }
                    ("decrypt", true) => {
                        let iv = body.get("iv").map(|v| v.to_string()).and_then(|v| b64url_decode(&v)).unwrap_or_default();
                        let tag = body.get("tag").map(|v| v.to_string()).and_then(|v| b64url_decode(&v)).unwrap_or_default();
                        match super::crypto::gcm_decrypt(key, &iv, &aad, &value, &tag) {
                            Some(p) => ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&p)))])),
                            None => bad("Decryption failed: the authentication tag does not match."),
                        }
                    }
                    ("wrapkey", false) => match super::crypto::aes_kw_wrap(key, &value) {
                        Some(w) => ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&w)))])),
                        None => bad("The key to wrap must be a multiple of 8 bytes (at least 16)."),
                    },
                    ("unwrapkey", false) => match super::crypto::aes_kw_unwrap(key, &value) {
                        Some(u) => ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&u)))])),
                        None => bad("Unwrap failed: integrity check failed."),
                    },
                    _ => bad(&format!("Algorithm {} is not valid for {}.", alg, op_name)),
                }
            } else {
                bad("EC keys cannot encrypt or wrap keys.")
            }
        }
        ("POST", ["keys", name, "create"]) => {
            let kty = st_("kty");
            let curve = match body.get("crv").map(|c| c.to_string()).as_deref().unwrap_or("P-256") {
                "P-256" => EcCurve::P256,
                "P-384" => EcCurve::P384,
                other => return Some(kv_error(400, "BadParameter", &format!("Unsupported curve {}", other))),
            };
            let private = curve.generate_private();
            let public = curve.public_key(&private).unwrap_or_default();
            let ops = body.get("key_ops").and_then(|o| o.as_array()).map(|a| a.iter().map(|x| x.to_string()).collect()).unwrap_or_else(|| vec!["sign".to_string(), "verify".to_string()]);
            let k = KeyVersion { version: random_hex(16), kty_name: kty.clone(), rsa: None, oct: None, curve, private, public, ops, created: now_secs() };
            let b = key_bundle(&base, name, &k);
            ks.keys.entry(name.to_string()).or_default().push(k);
            ok(b)
        }
        ("GET", ["keys", name]) => match ks.keys.get(*name).and_then(|v| v.last()) {
            Some(k) => ok(key_bundle(&base, name, k)),
            None => key_nf(name),
        },
        ("GET", ["keys", name, version]) => match ks.keys.get(*name).and_then(|v| v.iter().find(|k| k.version == *version)) {
            Some(k) => ok(key_bundle(&base, name, k)),
            None => key_nf(name),
        },
        ("DELETE", ["keys", name]) => match ks.keys.remove(*name) {
            Some(v) => {
                let b = deleted_key_bundle(&base, name, v.last().unwrap());
                ks.deleted_keys.insert(name.to_string(), v);
                ok(b)
            }
            None => key_nf(name),
        },
        ("GET", ["deletedkeys"]) => ok(obj([
            ("value", Value::Array(ks.deleted_keys.iter().filter_map(|(n, v)| v.last().map(|k| deleted_key_bundle(&base, n, k))).collect())),
            ("nextLink", Value::Null),
        ])),
        ("GET", ["deletedkeys", name]) => match ks.deleted_keys.get(*name).and_then(|v| v.last()) {
            Some(k) => ok(deleted_key_bundle(&base, name, k)),
            None => kv_error(404, "KeyNotFound", &format!("Deleted Key not found: {}", name)),
        },
        ("DELETE", ["deletedkeys", name]) => match ks.deleted_keys.remove(*name) {
            Some(_) => Response::new(204),
            None => kv_error(404, "KeyNotFound", &format!("Deleted Key not found: {}", name)),
        },
        ("POST", ["deletedkeys", name, "recover"]) => match ks.deleted_keys.remove(*name) {
            Some(v) => {
                let b = key_bundle(&base, name, v.last().unwrap());
                ks.keys.insert(name.to_string(), v);
                ok(b)
            }
            None => kv_error(404, "KeyNotFound", &format!("Deleted Key not found: {}", name)),
        },
        ("POST", ["keys", name, rest @ ..]) if matches!(rest.last(), Some(&"sign") | Some(&"verify")) => {
            let action = *rest.last().unwrap();
            let versions = match ks.keys.get(*name) {
                Some(v) => v,
                None => return Some(key_nf(name)),
            };
            let k = match rest.len() {
                1 => versions.last(),
                _ => versions.iter().find(|k| k.version == rest[0]),
            };
            let Some(k) = k else { return Some(key_nf(name)) };
            if !k.ops.iter().any(|o| o == action) {
                return Some(kv_error(403, "Forbidden", &format!("Operation {} is not permitted on this key.", action)));
            }
            let alg = st_("alg");
            if let Some(r) = &k.rsa {
                let hash = match alg.get(2..).unwrap_or("") {
                    "256" => HashAlg::Sha256,
                    "384" => HashAlg::Sha384,
                    "512" => HashAlg::Sha512,
                    _ => return Some(kv_error(400, "BadParameter", &format!("Algorithm {} is not valid for this key.", alg))),
                };
                let pss = alg.starts_with("PS");
                if !alg.starts_with("RS") && !pss {
                    return Some(kv_error(400, "BadParameter", &format!("Algorithm {} is not valid for this key.", alg)));
                }
                let field = if action == "sign" { "value" } else { "digest" };
                let Some(digest) = b64url_decode(&st_(field)).filter(|d| d.len() == hash.output_len()) else {
                    return Some(kv_error(400, "BadParameter", "Invalid digest length for the algorithm."));
                };
                let kid = format!("{}/keys/{}/{}", base, name, k.version);
                return Some(if action == "sign" {
                    let p = r.private().expect("RSA key");
                    let sig = if pss { p.sign_pss(hash, &digest) } else { p.sign_pkcs1(hash, &digest) }.unwrap_or_default();
                    ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&sig)))]))
                } else {
                    let sig = b64url_decode(&st_("value")).unwrap_or_default();
                    let pk = r.public().expect("RSA key");
                    let valid = if pss { pk.verify_pss(hash, &digest, &sig, None) } else { pk.verify_pkcs1(hash, &digest, &sig) };
                    ok(obj([("value", Value::Bool(valid))]))
                });
            }
            let (want_curve, hash) = match alg.as_str() {
                "ES256" => (EcCurve::P256, HashAlg::Sha256),
                "ES384" => (EcCurve::P384, HashAlg::Sha384),
                other => return Some(kv_error(400, "BadParameter", &format!("Algorithm {} is not valid for this key.", other))),
            };
            if want_curve != k.curve {
                return Some(kv_error(400, "BadParameter", &format!("Algorithm {} does not match the key curve.", alg)));
            }
            let field = if action == "sign" { "value" } else { "digest" };
            let Some(digest) = b64url_decode(&st_(field)).filter(|d| d.len() == hash.output_len()) else {
                return Some(kv_error(400, "BadParameter", "Invalid digest length for the algorithm."));
            };
            let kid = format!("{}/keys/{}/{}", base, name, k.version);
            if action == "sign" {
                let (r, s_) = k.curve.sign(&k.private, &digest, hash).unwrap_or_default();
                ok(obj([("kid", Value::from(kid)), ("value", Value::from(base64url_encode(&[r, s_].concat())))]))
            } else {
                let sig = b64url_decode(&st_("value")).unwrap_or_default();
                let n = k.curve.byte_len();
                let valid = sig.len() == 2 * n && k.curve.verify(&k.public, &digest, &sig[..n], &sig[n..]);
                ok(obj([("value", Value::Bool(valid))]))
            }
        }
        ("GET", ["certificates"]) => ok(obj([("value", Value::Array(ks.certs.keys().map(|n| obj([("id", Value::from(format!("{}/certificates/{}", base, n)))])).collect()))])),
        ("POST", ["certificates", name, "create"]) => {
            let subject = body.get("policy").and_then(|p| p.get("x509_props")).and_then(|x| x.get("subject")).map(|s| s.to_string()).unwrap_or_else(|| format!("CN={}", name));
            if body.get("policy").and_then(|p| p.get("issuer")).and_then(|i| i.get("name")).map(|n| n.to_string()).is_some_and(|n| n != "Self") {
                return Some(kv_error(400, "BadParameter", "Only self-issued certificates are supported by the emulator"));
            }
            ks.pending.insert(name.to_string(), (subject, false));
            Response::json(202, obj([("id", Value::from(format!("{}/certificates/{}/pending", base, name))), ("status", Value::from("inProgress")), ("request_id", Value::from(random_hex(16)))]).to_json())
        }
        ("GET", ["certificates", name, "pending"]) => match ks.pending.get(*name).cloned() {
            Some((subject, _)) => {
                let cn = subject.split(',').find_map(|p| p.trim().strip_prefix("CN=")).unwrap_or(name).to_string();
                let ss = camel_tls::certgen::self_signed(&cn);
                ks.certs.entry(name.to_string()).or_default().push(CertVersion { version: random_hex(16), der: ss.cert_der, subject, created: now_secs() });
                ks.pending.remove(*name);
                ok(obj([("id", Value::from(format!("{}/certificates/{}/pending", base, name))), ("status", Value::from("completed")), ("target", Value::from(format!("{}/certificates/{}", base, name)))]))
            }
            None => kv_error(404, "PendingCertificateNotFound", &format!("Pending certificate not found: {}", name)),
        },
        ("POST", ["certificates", name, "import"]) => {
            let value = st_("value");
            let der = camel_tls::pem::parse_pem(value.as_bytes()).ok().and_then(|blocks| blocks.into_iter().find(|b| b.label == "CERTIFICATE").map(|b| b.der));
            let Some(der) = der else { return Some(kv_error(400, "BadParameter", "The emulator imports PEM certificates only")) };
            let c = CertVersion { version: random_hex(16), der, subject: String::new(), created: now_secs() };
            let b = cert_bundle(&base, name, &c);
            ks.certs.entry(name.to_string()).or_default().push(c);
            ok(b)
        }
        ("GET", ["certificates", name]) => match ks.certs.get(*name).and_then(|v| v.last()) {
            Some(c) => ok(cert_bundle(&base, name, c)),
            None => cert_nf(name),
        },
        ("GET", ["certificates", name, version]) => match ks.certs.get(*name).and_then(|v| v.iter().find(|c| c.version == *version)) {
            Some(c) => ok(cert_bundle(&base, name, c)),
            None => cert_nf(name),
        },
        ("DELETE", ["certificates", name]) => match ks.certs.remove(*name) {
            Some(v) => {
                let mut b: ObjectMap = cert_bundle(&base, name, v.last().unwrap()).as_object().cloned().unwrap_or_default();
                b.insert("recoveryId".into(), Value::from(format!("{}/deletedcertificates/{}", base, name)));
                ok(Value::Object(b))
            }
            None => cert_nf(name),
        },
        _ => kv_error(400, "BadParameter", "Unsupported Key Vault request"),
    };
    Some(r)
}
