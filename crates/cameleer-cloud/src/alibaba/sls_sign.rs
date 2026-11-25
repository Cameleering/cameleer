//! Simple Log Service request signatures.
//!
//! **V1** (`LOG <AccessKeyId>:<Signature>`):
//!
//! ```text
//! SignString = VERB + "\n"
//!            + CONTENT-MD5 + "\n"      (upper-case hex MD5 of the body as sent, empty without a body)
//!            + CONTENT-TYPE + "\n"
//!            + DATE + "\n"             (RFC 1123, GMT)
//!            + CanonicalizedLOGHeaders (x-log-* and x-acs-* headers, lower-case names, sorted, "name:value\n")
//!            + CanonicalizedResource   (path, then "?" and the sorted query "k=v" joined by "&", not encoded)
//! Signature  = Base64(HMAC(AccessKeySecret, SignString))
//! ```
//!
//! The HMAC is the one named in `x-log-signaturemethod`: `hmac-sha1` (the classic SDKs) or
//! `hmac-sha256` (the `sls20201230` SDK that Camel uses).
//!
//! **V4** (`SLS4-HMAC-SHA256`): see [`authorization_v4`].

use cameleer_tls::crypto::hash::{hmac, Sha1, Sha256};

use crate::util::{base64_encode, hex, hmac_sha256, md5, sha256_hex, uri_encode};

/// Upper-case hex MD5 of a body (the `Content-MD5` header of SLS).
pub fn content_md5(body: &[u8]) -> String {
    hex(&md5(body)).to_ascii_uppercase()
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers.iter().find(|(k, _)| k.trim().eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).unwrap_or("")
}

/// The canonicalized `x-log-` / `x-acs-` headers of the V1 string to sign.
fn canonical_log_headers(headers: &[(String, String)]) -> String {
    let mut h: Vec<(String, &str)> = headers
        .iter()
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.as_str()))
        .filter(|(k, _)| (k.starts_with("x-log-") || k.starts_with("x-acs-")) && !k.starts_with("x-log-meta-"))
        .collect();
    h.sort();
    h.iter().map(|(k, v)| format!("{}:{}\n", k, v.trim())).collect()
}

/// The canonicalized resource of the V1 string to sign.
fn canonical_resource(path: &str, query: &[(String, String)]) -> String {
    if query.is_empty() {
        return path.to_string();
    }
    let mut q: Vec<&(String, String)> = query.iter().collect();
    q.sort();
    format!("{}?{}", path, q.iter().map(|(k, v)| format!("{}={}", k, v)).collect::<Vec<_>>().join("&"))
}

/// The V1 string to sign. `path` is the decoded resource path (`/logstores/x/shards/lb`), `query`
/// the decoded query parameters.
pub fn string_to_sign_v1(method: &str, path: &str, query: &[(String, String)], headers: &[(String, String)]) -> String {
    format!(
        "{}\n{}\n{}\n{}\n{}{}",
        method,
        header(headers, "Content-MD5"),
        header(headers, "Content-Type"),
        header(headers, "Date"),
        canonical_log_headers(headers),
        canonical_resource(path, query)
    )
}

/// The V1 signature: Base64 HMAC-SHA1, or HMAC-SHA256 when `sha256`.
pub fn signature_v1(secret: &str, string_to_sign: &str, sha256: bool) -> String {
    let mac = if sha256 { hmac::<Sha256>(secret.as_bytes(), string_to_sign.as_bytes()) } else { hmac::<Sha1>(secret.as_bytes(), string_to_sign.as_bytes()) };
    base64_encode(&mac)
}

/// The V1 `Authorization` header value; the HMAC follows the `x-log-signaturemethod` header
/// (`hmac-sha256`, else SHA-1).
pub fn authorization_v1(access_key: &str, secret: &str, method: &str, path: &str, query: &[(String, String)], headers: &[(String, String)]) -> String {
    let sha256 = header(headers, "x-log-signaturemethod").eq_ignore_ascii_case("hmac-sha256");
    format!("LOG {}:{}", access_key, signature_v1(secret, &string_to_sign_v1(method, path, query, headers), sha256))
}

/// The V4 canonical request and the signed header names.
///
/// ```text
/// METHOD \n URI \n CanonicalQuery \n CanonicalHeaders \n SignedHeaders \n x-log-content-sha256
/// ```
///
/// `uri` is the path as sent (already escaped, not encoded again). The query keys are sorted
/// and written raw, the values percent-encoded (RFC 3986); a key with an empty value is written
/// without `=`. The signed headers are `content-type`, `host` and every `x-log-*` (except
/// `x-log-meta-*`) and `x-acs-*` header, with lower-case names, sorted, as `name:trim(value)\n`.
pub fn canonical_request_v4(method: &str, uri: &str, query: &[(String, String)], headers: &[(String, String)], payload_hash: &str) -> (String, String) {
    let mut q: Vec<(&str, &str)> = query.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    q.sort();
    let query = q.iter().map(|(k, v)| if v.is_empty() { k.to_string() } else { format!("{}={}", k, uri_encode(v, false)) }).collect::<Vec<_>>().join("&");
    let mut h: Vec<(String, String)> = headers
        .iter()
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .filter(|(k, _)| k == "content-type" || k == "host" || ((k.starts_with("x-log-") || k.starts_with("x-acs-")) && !k.starts_with("x-log-meta-")))
        .collect();
    h.sort();
    let signed = h.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical_headers: String = h.iter().map(|(k, v)| format!("{}:{}\n", k, v)).collect();
    (format!("{}\n{}\n{}\n{}\n{}\n{}", method, uri, query, canonical_headers, signed, payload_hash), signed)
}

/// The V4 signature (hex):
///
/// ```text
/// StringToSign = "SLS4-HMAC-SHA256\n" + x-log-date + "\n" + scope + "\n" + hex(SHA256(CanonicalRequest))
/// scope        = yyyyMMdd/region/sls/aliyun_v4_request
/// SigningKey   = HMAC(HMAC(HMAC(HMAC("aliyun_v4" + secret, yyyyMMdd), region), "sls"), "aliyun_v4_request")
/// Signature    = hex(HMAC-SHA256(SigningKey, StringToSign))
/// ```
///
/// `headers` must already hold `x-log-date` (`yyyyMMddTHHmmssZ`) and `x-log-content-sha256`.
pub fn signature_v4(secret: &str, region: &str, method: &str, uri: &str, query: &[(String, String)], headers: &[(String, String)]) -> String {
    let date_time = header(headers, "x-log-date").to_string();
    let date = &date_time[..date_time.len().min(8)];
    let payload = header(headers, "x-log-content-sha256").to_string();
    let (canonical, _signed) = canonical_request_v4(method, uri, query, headers, &payload);
    let scope = format!("{}/{}/sls/aliyun_v4_request", date, region);
    let to_sign = format!("SLS4-HMAC-SHA256\n{}\n{}\n{}", date_time, scope, sha256_hex(canonical.as_bytes()));
    let mut key = hmac_sha256(format!("aliyun_v4{}", secret).as_bytes(), date.as_bytes());
    for part in [region, "sls", "aliyun_v4_request"] {
        key = hmac_sha256(&key, part.as_bytes());
    }
    hex(&hmac_sha256(&key, to_sign.as_bytes()))
}

/// The V4 `Authorization` header value, in the SDKs' form:
/// `SLS4-HMAC-SHA256 Credential=AccessKeyId/scope,Signature=...`.
pub fn authorization_v4(access_key: &str, secret: &str, region: &str, method: &str, uri: &str, query: &[(String, String)], headers: &[(String, String)]) -> String {
    let date_time = header(headers, "x-log-date");
    let date = &date_time[..date_time.len().min(8)];
    format!("SLS4-HMAC-SHA256 Credential={}/{}/{}/sls/aliyun_v4_request,Signature={}", access_key, date, region, signature_v4(secret, region, method, uri, query, headers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hs(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The vectors of the official Go SDK (`aliyun-log-go-sdk`, `signature_test.go`).
    #[test]
    fn v1_vectors_of_the_go_sdk() {
        let h = hs(&[("x-log-apiversion", "0.6.0"), ("x-log-signaturemethod", "hmac-sha1"), ("x-log-bodyrawsize", "0"), ("Date", "Mon, 3 Jan 2010 08:33:47 GMT")]);
        assert_eq!(string_to_sign_v1("GET", "/logstores", &[], &h), "GET\n\n\nMon, 3 Jan 2010 08:33:47 GMT\nx-log-apiversion:0.6.0\nx-log-bodyrawsize:0\nx-log-signaturemethod:hmac-sha1\n/logstores");
        assert_eq!(authorization_v1("mockAccessKeyID", "mockAccessKeySecret", "GET", "/logstores", &[], &h), "LOG mockAccessKeyID:Rwm6cTKzoti4HWoe+GKcb6Kv07E=");

        let body = [10u8, 50, 10, 30, 8, 248, 178, 147, 158, 5, 18, 22, 10, 7, 84, 101, 115, 116, 75, 101, 121, 18, 11, 84, 101, 115, 116, 67, 111, 110, 116, 101, 110, 116, 26, 0, 34, 14, 49, 48, 46, 50, 51, 48, 46, 50, 48, 49, 46, 49, 49, 55];
        assert_eq!(content_md5(&body), "8D6881250FF5559F59BBDF04100F6E59");
        let h = hs(&[
            ("x-log-apiversion", "0.6.0"),
            ("x-log-signaturemethod", "hmac-sha1"),
            ("x-log-bodyrawsize", "50"),
            ("Content-MD5", "8D6881250FF5559F59BBDF04100F6E59"),
            ("Content-Type", "application/x-protobuf"),
            ("Content-Length", "50"),
            ("Date", "Mon, 3 Jan 2010 08:33:47 GMT"),
        ]);
        assert_eq!(signature_v1("mockAccessKeySecret", &string_to_sign_v1("GET", "/logstores/app_log", &[], &h), false), "87xQWqFaOSewqRIma8kPjGYlXHc=");
    }

    #[test]
    fn v1_canonical_resource_and_sha256() {
        // the GET example of the SLS signature documentation (its signature is masked there)
        let h = hs(&[("x-log-apiversion", "0.6.0"), ("x-log-bodyrawsize", "0"), ("x-log-signaturemethod", "hmac-sha1"), ("Date", "Mon, 09 Nov 2015 06:11:16 GMT")]);
        let q = hs(&[("offset", "0"), ("size", "1000"), ("logstoreName", "")]);
        assert_eq!(
            string_to_sign_v1("GET", "/logstores", &q, &h),
            "GET\n\n\nMon, 09 Nov 2015 06:11:16 GMT\nx-log-apiversion:0.6.0\nx-log-bodyrawsize:0\nx-log-signaturemethod:hmac-sha1\n/logstores?logstoreName=&offset=0&size=1000"
        );
        let s = string_to_sign_v1("GET", "/logstores", &q, &h);
        assert_eq!(signature_v1("k", &s, true), base64_encode(&hmac::<Sha256>(b"k", s.as_bytes())));
    }

    /// The vectors of the official Python and Go SDKs (`tests/unit/test_auth.py`,
    /// `signature_v4_test.go`).
    #[test]
    fn v4_vectors_of_the_sdks() {
        let body = b"adasd= -asd zcas";
        let sha = sha256_hex(body);
        let base = |sha: &str| hs(&[("hello", "world"), ("hello-Text", "a12X- "), (" Ko ", ""), ("x-log-test", "het123"), ("x-acs-ppp", "dds"), ("x-log-date", "20220808T032330Z"), ("x-log-content-sha256", sha)]);
        let q = hs(&[(" abc", "efg"), (" agc ", ""), ("", "efg"), ("A-bc", "eFg")]);
        let (cr, _) = canonical_request_v4("POST", "/logstores", &q, &base(&sha), &sha);
        assert_eq!(
            cr,
            "POST\n/logstores\n=efg& abc=efg& agc &A-bc=eFg\nx-acs-ppp:dds\nx-log-content-sha256:c7a4fd95f1d70933caf91641e8000768cea86100dc5649fd4f16fead718a62e4\nx-log-date:20220808T032330Z\nx-log-test:het123\n\nx-acs-ppp;x-log-content-sha256;x-log-date;x-log-test\nc7a4fd95f1d70933caf91641e8000768cea86100dc5649fd4f16fead718a62e4"
        );
        let sig = |method: &str, uri: &str, q: &[(String, String)], sha: &str, region: &str| signature_v4("zxasdasdasw2", region, method, uri, q, &base(sha));
        assert_eq!(sig("POST", "/logstores", &q, &sha, "cn-hangzhou"), "a98f5632e93836e63839cd836a54055f480020a9364ca944e2d34f2eb9bf1bed");
        let bare = hs(&[("x-log-date", "20220808T032330Z"), ("x-log-content-sha256", &sha)]);
        assert_eq!(signature_v4("zxasdasdasw2", "cn-shanghai", "POST", "/logstores", &[], &bare), "8a10a5e723cb2e75964816de660b2c16a58af8bc0261f7f0722d832468c76ce8");
        let empty = sha256_hex(b"");
        assert_eq!(sig("POST", "/logstores", &q, &empty, "cn-hangzhou"), "5a66d8f8051983e0e9d08e0f960ef9252ef971eead5bb5c7acec8617a2eb2701");
        assert_eq!(sig("GET", "/logstores", &q, &empty, "cn-hangzhou"), "d92741852500791d662a8d469ff61627c0559ecd86c3f59b7bf6772b6c62666a");
        let mut q5 = q.clone();
        q5.extend(hs(&[("abs-ij*asd/vc", "a~js+d ada"), ("a abAas123/vc", "a~jdad a2ADFs+d ada")]));
        assert_eq!(sig("POST", "/logstores/hello/a+*~bb/cc", &q5, &sha, "cn-hangzhou"), "2c204068e961a8813a6bcf7ac422f7fa6e9bf9a5da493e0165dfe100854d18ff");
        assert_eq!(
            authorization_v4("acsddda21dsd", "zxasdasdasw2", "cn-hangzhou", "POST", "/logstores", &q, &base(&sha)),
            "SLS4-HMAC-SHA256 Credential=acsddda21dsd/20220808/cn-hangzhou/sls/aliyun_v4_request,Signature=a98f5632e93836e63839cd836a54055f480020a9364ca944e2d34f2eb9bf1bed"
        );
    }

    #[test]
    fn v4_percent_encoding_vectors() {
        assert_eq!(uri_encode("123abc!@#$%^&*()-=_+ ~|\\/", false), "123abc%21%40%23%24%25%5E%26%2A%28%29-%3D_%2B%20~%7C%5C%2F");
        assert_eq!(uri_encode("!@#$%^&*()=-+ ~./_[()]%20你好\0đ❤😓", false), "%21%40%23%24%25%5E%26%2A%28%29%3D-%2B%20~.%2F_%5B%28%29%5D%2520%E4%BD%A0%E5%A5%BD%00%C4%91%E2%9D%A4%F0%9F%98%93");
    }
}
