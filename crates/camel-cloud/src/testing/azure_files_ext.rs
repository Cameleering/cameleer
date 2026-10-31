//! The Azure Files (`file` service) endpoint of the Azurite-style emulator: shares,
//! directories, files with `Create File` + `Put Range`, download, properties and listing.
//! Requests are authorized by the parent handler (Shared Key / account SAS with service `f`).

use std::collections::{BTreeMap, BTreeSet};

use super::{error, Shared};
use crate::http::{Response, ServerRequest};
use crate::util::{http_date, now_secs, random_hex, xml_escape};

#[derive(Clone)]
pub(super) struct AzFile {
    pub data: Vec<u8>,
    pub content_type: String,
    pub etag: String,
    pub last_modified: i64,
}

#[derive(Default)]
pub(super) struct Share {
    pub created: i64,
    pub dirs: BTreeSet<String>,
    pub files: BTreeMap<String, AzFile>,
}

fn etag() -> String {
    format!("\"0x{}\"", random_hex(8).to_uppercase())
}

fn parent(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(p, _)| p)
}

fn parent_exists(s: &Share, path: &str) -> bool {
    parent(path).is_none_or(|p| s.dirs.contains(p))
}

/// (permission letters accepted, resource type) needed by a Files request (account SAS).
pub(super) fn sas_requirement(method: &str, segs: &[String], req: &ServerRequest) -> (&'static str, char) {
    let restype = req.query_param("restype");
    match (segs.len(), restype.as_deref(), method) {
        (0, _, _) => ("l", 's'),
        (_, Some("share"), "PUT") => ("c", 'c'),
        (_, Some("share"), "DELETE") => ("d", 'c'),
        (_, Some("directory"), "GET") => ("l", 'c'),
        (_, Some("directory"), "PUT") => ("cw", 'c'),
        (_, Some("directory"), "DELETE") => ("d", 'c'),
        (_, _, "PUT") => ("cw", 'o'),
        (_, _, "DELETE") => ("d", 'o'),
        _ => ("r", 'o'),
    }
}

fn list_xml(name: &str, dir: &str, entries: Vec<(bool, String, usize)>) -> Response {
    let mut xml = format!("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults ServiceEndpoint=\"\" ShareName=\"{}\" DirectoryPath=\"{}\"><Entries>", xml_escape(name), xml_escape(dir));
    for (is_dir, n, size) in entries {
        if is_dir {
            xml.push_str(&format!("<Directory><Name>{}</Name><Properties /></Directory>", xml_escape(&n)));
        } else {
            xml.push_str(&format!("<File><Name>{}</Name><Properties><Content-Length>{}</Content-Length></Properties></File>", xml_escape(&n), size));
        }
    }
    xml.push_str("</Entries><NextMarker /></EnumerationResults>");
    Response::xml(200, xml)
}

/// Handle a Files request; `rest` is the raw path after the account.
pub(super) fn file_op(st: &Shared, req: &ServerRequest, rest: &str) -> Response {
    let mut s = st.lock().unwrap();
    let decoded = crate::util::percent_decode(rest, false);
    let (share, path) = match decoded.trim_matches('/').split_once('/') {
        Some((a, b)) => (a.to_string(), b.trim_matches('/').to_string()),
        None => (decoded.trim_matches('/').to_string(), String::new()),
    };
    let restype = req.query_param("restype");
    let comp = req.query_param("comp");
    if share.is_empty() {
        if comp.as_deref() != Some("list") {
            return error(400, "InvalidQueryParameterValue", "Unsupported service operation");
        }
        let prefix = req.query_param("prefix").unwrap_or_default();
        let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults><Shares>");
        for (n, sh) in s.shares.iter().filter(|(n, _)| n.starts_with(&prefix)) {
            xml.push_str(&format!("<Share><Name>{}</Name><Properties><Last-Modified>{}</Last-Modified><Quota>5120</Quota></Properties></Share>", xml_escape(n), http_date(sh.created)));
        }
        xml.push_str("</Shares><NextMarker /></EnumerationResults>");
        return Response::xml(200, xml);
    }
    if restype.as_deref() == Some("share") {
        return match req.method.as_str() {
            "PUT" if s.shares.contains_key(&share) => error(409, "ShareAlreadyExists", "The specified share already exists."),
            "PUT" => {
                s.shares.insert(share, Share { created: now_secs(), ..Default::default() });
                Response::new(201).with_header("ETag", etag())
            }
            "DELETE" => match s.shares.remove(&share) {
                Some(_) => Response::new(202),
                None => error(404, "ShareNotFound", "The specified share does not exist."),
            },
            _ => error(400, "InvalidQueryParameterValue", "Unsupported share operation"),
        };
    }
    let Some(sh) = s.shares.get_mut(&share) else { return error(404, "ShareNotFound", "The specified share does not exist.") };
    if restype.as_deref() == Some("directory") {
        return match (req.method.as_str(), comp.as_deref()) {
            ("GET", Some("list")) => {
                if !path.is_empty() && !sh.dirs.contains(&path) {
                    return error(404, "ResourceNotFound", "The specified resource does not exist.");
                }
                let prefix = req.query_param("prefix").unwrap_or_default();
                let base = if path.is_empty() { String::new() } else { format!("{}/", path) };
                let child = |p: &str| p.strip_prefix(base.as_str()).filter(|r| !r.is_empty() && !r.contains('/')).map(|r| r.to_string());
                let mut entries: Vec<(bool, String, usize)> = sh.dirs.iter().filter_map(|d| child(d)).map(|n| (true, n, 0)).collect();
                entries.extend(sh.files.iter().filter_map(|(p, f)| child(p).map(|n| (false, n, f.data.len()))));
                entries.retain(|(_, n, _)| n.starts_with(&prefix));
                list_xml(&share, &path, entries)
            }
            ("PUT", _) => {
                if sh.dirs.contains(&path) {
                    return error(409, "ResourceAlreadyExists", "The specified resource already exists.");
                }
                if !parent_exists(sh, &path) {
                    return error(404, "ParentNotFound", "The specified parent path does not exist.");
                }
                sh.dirs.insert(path);
                Response::new(201).with_header("ETag", etag())
            }
            ("DELETE", _) => {
                if !sh.dirs.contains(&path) {
                    return error(404, "ResourceNotFound", "The specified resource does not exist.");
                }
                let pre = format!("{}/", path);
                if sh.dirs.iter().any(|d| d.starts_with(&pre)) || sh.files.keys().any(|f| f.starts_with(&pre)) {
                    return error(409, "DirectoryNotEmpty", "The specified directory is not empty.");
                }
                sh.dirs.remove(&path);
                Response::new(202)
            }
            _ => error(400, "InvalidQueryParameterValue", "Unsupported directory operation"),
        };
    }
    match (req.method.as_str(), comp.as_deref()) {
        ("PUT", Some("range")) => {
            let Some(f) = sh.files.get_mut(&path) else { return error(404, "ResourceNotFound", "The specified resource does not exist.") };
            let range = req.header("x-ms-range").or_else(|| req.header("Range")).unwrap_or("");
            let Some((a, b)) = range.strip_prefix("bytes=").and_then(|r| r.split_once('-')).and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))) else {
                return error(400, "InvalidHeaderValue", "x-ms-range is required");
            };
            if req.header("x-ms-write") != Some("update") {
                return error(400, "InvalidHeaderValue", "x-ms-write must be update");
            }
            if b < a || b >= f.data.len() {
                return error(416, "InvalidRange", "The range specified is invalid for the current size of the resource.");
            }
            if req.body.len() != b - a + 1 {
                return error(400, "InvalidHeaderValue", "The body length does not match the range");
            }
            f.data[a..=b].copy_from_slice(&req.body);
            f.etag = etag();
            f.last_modified = now_secs();
            Response::new(201).with_header("ETag", f.etag.clone())
        }
        ("PUT", _) => {
            if req.header("x-ms-type") != Some("file") {
                return error(400, "MissingRequiredHeader", "x-ms-type: file is required");
            }
            let Some(len) = req.header("x-ms-content-length").and_then(|l| l.parse::<usize>().ok()) else { return error(400, "MissingRequiredHeader", "x-ms-content-length is required") };
            if !parent_exists(sh, &path) {
                return error(404, "ParentNotFound", "The specified parent path does not exist.");
            }
            let f = AzFile { data: vec![0; len], content_type: req.header("x-ms-content-type").unwrap_or("application/octet-stream").to_string(), etag: etag(), last_modified: now_secs() };
            let e = f.etag.clone();
            sh.files.insert(path, f);
            Response::new(201).with_header("ETag", e)
        }
        ("GET", _) | ("HEAD", _) => match sh.files.get(&path) {
            Some(f) => Response::new(200).with_header("Content-Type", f.content_type.clone()).with_header("ETag", f.etag.clone()).with_header("Last-Modified", http_date(f.last_modified)).with_header("x-ms-type", "File").with_body(f.data.clone()),
            None => error(404, "ResourceNotFound", "The specified resource does not exist."),
        },
        ("DELETE", _) => match sh.files.remove(&path) {
            Some(_) => Response::new(202),
            None => error(404, "ResourceNotFound", "The specified resource does not exist."),
        },
        _ => error(400, "InvalidQueryParameterValue", "Unsupported file operation"),
    }
}
