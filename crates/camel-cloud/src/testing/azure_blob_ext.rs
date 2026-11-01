//! Blob-level operations of the Azurite emulator (block, append and page blobs, leases) and
//! the ADLS Gen2 DFS endpoint sharing the blob containers (file systems).

use super::{error, new_blob, Blob, Container, Shared};
use crate::http::{Response, ServerRequest};
use crate::util::{base64_encode, http_date, md5, now_secs, random_hex, uuid};

/// An active lease.
#[derive(Clone, Debug)]
pub(super) struct Lease {
    pub id: String,
    pub expires: Option<i64>,
}

fn active_lease(b: &Blob) -> Option<&Lease> {
    b.lease.as_ref().filter(|l| l.expires.is_none_or(|e| e > now_secs()))
}

/// Check the `x-ms-lease-id` of a write / delete against the blob's lease.
fn check_lease(b: &Blob, req: &ServerRequest) -> Result<(), Response> {
    let given = req.header("x-ms-lease-id");
    match (active_lease(b), given) {
        (Some(_), None) => Err(error(412, "LeaseIdMissing", "There is currently a lease on the blob and no lease ID was specified in the request.")),
        (Some(l), Some(g)) if l.id != g => Err(error(412, "LeaseIdMismatchWithBlobOperation", "The lease ID specified did not match the lease ID for the blob.")),
        (None, Some(_)) => Err(error(412, "LeaseNotPresentWithBlobOperation", "There is currently no lease on the blob.")),
        _ => Ok(()),
    }
}

fn blob_headers(r: Response, b: &Blob) -> Response {
    r.with_header("ETag", b.etag.clone()).with_header("Last-Modified", http_date(b.last_modified))
}

fn touch(b: &mut Blob) {
    b.etag = format!("\"0x{}\"", random_hex(8).to_uppercase());
    b.last_modified = now_secs();
}

fn parse_range(v: Option<&str>) -> Option<(usize, usize)> {
    let r = v?.trim().strip_prefix("bytes=")?;
    let (a, b) = r.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Put Block / Put Block List / Get Block List; None for other requests.
pub(super) fn block_list_op(c: &mut Container, req: &ServerRequest, blob: &str) -> Option<Response> {
    let comp = req.query_param("comp").unwrap_or_default();
    match (req.method.as_str(), comp.as_str()) {
        ("PUT", "block") => {
            let id = req.query_param("blockid").unwrap_or_default();
            if id.is_empty() || crate::util::base64_decode(&id).is_none() {
                return Some(error(400, "InvalidQueryParameterValue", "Value for one of the query parameters specified in the request URI is invalid (blockid)."));
            }
            if let Some(b) = c.blobs.get(blob) {
                if let Err(r) = check_lease(b, req) {
                    return Some(r);
                }
            }
            let staged = c.staged.entry(blob.to_string()).or_default();
            staged.retain(|(i, _)| *i != id);
            staged.push((id, req.body.clone()));
            Some(Response::new(201).with_header("Content-MD5", base64_encode(&md5(&req.body))))
        }
        ("PUT", "blocklist") => {
            let text = req.text();
            let Ok(x) = crate::util::Xml::parse(&text) else { return Some(error(400, "InvalidXmlDocument", "XML specified is not syntactically valid.")) };
            if let Some(b) = c.blobs.get(blob) {
                if let Err(r) = check_lease(b, req) {
                    return Some(r);
                }
            }
            let committed: Vec<(String, usize)> = c.blobs.get(blob).map(|b| b.block_list.clone()).unwrap_or_default();
            let staged = c.staged.get(blob).cloned().unwrap_or_default();
            let mut data = Vec::new();
            let mut list = Vec::new();
            let old_data = c.blobs.get(blob).map(|b| b.data.clone()).unwrap_or_default();
            for i in x.doc.element_children(x.root()) {
                let kind = x.name(i).to_string();
                let id = x.text(i);
                let from_staged = staged.iter().find(|(s, _)| *s == id).map(|(_, d)| d.clone());
                let from_committed = || {
                    let mut off = 0;
                    for (cid, size) in &committed {
                        if *cid == id {
                            return old_data.get(off..off + size).map(|d| d.to_vec());
                        }
                        off += size;
                    }
                    None
                };
                let block = match kind.as_str() {
                    "Uncommitted" => from_staged,
                    "Committed" => from_committed(),
                    _ => from_staged.or_else(from_committed),
                };
                let Some(block) = block else { return Some(error(400, "InvalidBlockList", "The specified block list is invalid.")) };
                list.push((id, block.len()));
                data.extend_from_slice(&block);
            }
            let ct = req.header("x-ms-blob-content-type").unwrap_or("application/octet-stream").to_string();
            let meta = req.headers.iter().filter_map(|(k, v)| k.to_ascii_lowercase().strip_prefix("x-ms-meta-").map(|m| (m.to_string(), v.clone()))).collect();
            let lease = c.blobs.get(blob).and_then(|b| b.lease.clone());
            let mut b = new_blob(data, ct, meta);
            b.block_list = list;
            b.lease = lease;
            let r = blob_headers(Response::new(201), &b);
            c.blobs.insert(blob.to_string(), b);
            c.staged.remove(blob);
            Some(r)
        }
        ("GET", "blocklist") => {
            let kind = req.query_param("blocklisttype").unwrap_or_else(|| "committed".into()).to_ascii_lowercase();
            let committed = c.blobs.get(blob).map(|b| b.block_list.clone());
            let staged = c.staged.get(blob).cloned();
            if committed.is_none() && staged.is_none() {
                return Some(error(404, "BlobNotFound", "The specified blob does not exist."));
            }
            let block = |id: &str, size: usize| format!("<Block><Name>{}</Name><Size>{}</Size></Block>", id, size);
            let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><BlockList>");
            if kind != "uncommitted" {
                xml.push_str("<CommittedBlocks>");
                for (id, size) in committed.unwrap_or_default() {
                    xml.push_str(&block(&id, size));
                }
                xml.push_str("</CommittedBlocks>");
            }
            if kind != "committed" {
                xml.push_str("<UncommittedBlocks>");
                for (id, d) in staged.unwrap_or_default() {
                    xml.push_str(&block(&id, d.len()));
                }
                xml.push_str("</UncommittedBlocks>");
            }
            xml.push_str("</BlockList>");
            Some(Response::xml(200, xml))
        }
        _ => None,
    }
}

/// Operations on one blob of an existing container.
pub(super) fn blob_item_op(c: &mut Container, req: &ServerRequest, blob: String) -> Response {
    let comp = req.query_param("comp").unwrap_or_default();
    let not_found = || error(404, "BlobNotFound", "The specified blob does not exist.");
    // conditional headers (If-Match / If-None-Match) of writes and deletes
    if matches!(req.method.as_str(), "PUT" | "DELETE") && comp.is_empty() {
        let current = c.blobs.get(&blob).map(|b| b.etag.clone());
        if req.header("If-None-Match").is_some_and(|v| v.trim() == "*") && current.is_some() {
            return error(409, "BlobAlreadyExists", "The specified blob already exists.");
        }
        if let Some(m) = req.header("If-Match").map(|v| v.trim().to_string()) {
            match &current {
                None => return not_found(),
                Some(e) if m != "*" && *e != m => return error(412, "ConditionNotMet", "The condition specified using HTTP conditional header(s) is not met."),
                _ => {}
            }
        }
    }
    match (req.method.as_str(), comp.as_str()) {
        ("PUT", "lease") => {
            let Some(b) = c.blobs.get_mut(&blob) else { return not_found() };
            let action = req.header("x-ms-lease-action").unwrap_or("").to_ascii_lowercase();
            let given = req.header("x-ms-lease-id").map(|s| s.to_string());
            let current = active_lease(b).cloned();
            match action.as_str() {
                "acquire" => {
                    let d: i64 = req.header("x-ms-lease-duration").and_then(|d| d.trim().parse().ok()).unwrap_or(-2);
                    if d != -1 && !(15..=60).contains(&d) {
                        return error(400, "InvalidHeaderValue", "The value for one of the HTTP headers is not in the correct format (x-ms-lease-duration).");
                    }
                    let proposed = req.header("x-ms-proposed-lease-id").map(|s| s.to_string()).unwrap_or_else(uuid);
                    if let Some(l) = current {
                        if l.id != proposed {
                            return error(409, "LeaseAlreadyPresent", "There is already a lease present.");
                        }
                    }
                    b.lease = Some(Lease { id: proposed.clone(), expires: if d == -1 { None } else { Some(now_secs() + d) } });
                    blob_headers(Response::new(201), b).with_header("x-ms-lease-id", proposed)
                }
                "renew" | "release" | "change" => {
                    let Some(l) = b.lease.clone() else { return error(409, "LeaseNotPresentWithLeaseOperation", "There is currently no lease on the blob.") };
                    if given.as_deref() != Some(l.id.as_str()) {
                        return error(409, "LeaseIdMismatchWithLeaseOperation", "The lease ID specified did not match the lease ID for the blob.");
                    }
                    match action.as_str() {
                        "renew" => {
                            b.lease = Some(Lease { id: l.id.clone(), expires: l.expires.map(|_| now_secs() + 60) });
                            blob_headers(Response::new(200), b).with_header("x-ms-lease-id", l.id)
                        }
                        "release" => {
                            b.lease = None;
                            blob_headers(Response::new(200), b)
                        }
                        _ => {
                            let new_id = req.header("x-ms-proposed-lease-id").unwrap_or("").to_string();
                            b.lease = Some(Lease { id: new_id.clone(), expires: l.expires });
                            blob_headers(Response::new(200), b).with_header("x-ms-lease-id", new_id)
                        }
                    }
                }
                "break" => {
                    if current.is_none() {
                        return error(409, "LeaseNotPresentWithLeaseOperation", "There is currently no lease on the blob.");
                    }
                    b.lease = None;
                    blob_headers(Response::new(202), b).with_header("x-ms-lease-time", "0")
                }
                _ => error(400, "InvalidHeaderValue", "Unknown x-ms-lease-action"),
            }
        }
        ("PUT", "appendblock") => {
            let Some(b) = c.blobs.get_mut(&blob) else { return not_found() };
            if b.blob_type != "AppendBlob" {
                return error(409, "InvalidBlobType", "The blob type is invalid for this operation.");
            }
            if let Err(r) = check_lease(b, req) {
                return r;
            }
            let offset = b.data.len();
            b.data.extend_from_slice(&req.body);
            b.committed_blocks += 1;
            touch(b);
            blob_headers(Response::new(201), b)
                .with_header("x-ms-blob-append-offset", offset.to_string())
                .with_header("x-ms-blob-committed-block-count", b.committed_blocks.to_string())
        }
        ("PUT", "page") => {
            let Some(b) = c.blobs.get_mut(&blob) else { return not_found() };
            if b.blob_type != "PageBlob" {
                return error(409, "InvalidBlobType", "The blob type is invalid for this operation.");
            }
            if let Err(r) = check_lease(b, req) {
                return r;
            }
            let Some((start, end)) = parse_range(req.header("x-ms-range").or_else(|| req.header("Range"))) else { return error(400, "InvalidHeaderValue", "Missing or invalid x-ms-range") };
            if start % 512 != 0 || (end + 1) % 512 != 0 || end < start || end >= b.data.len() {
                return error(416, "InvalidPageRange", "The page range specified is invalid.");
            }
            match req.header("x-ms-page-write").unwrap_or("").to_ascii_lowercase().as_str() {
                "update" => {
                    if req.body.len() != end - start + 1 {
                        return error(400, "InvalidHeaderValue", "The body length does not match the range.");
                    }
                    b.data[start..=end].copy_from_slice(&req.body);
                    for p in start / 512..=end / 512 {
                        b.pages.insert(p);
                    }
                }
                "clear" => {
                    b.data[start..=end].iter_mut().for_each(|x| *x = 0);
                    for p in start / 512..=end / 512 {
                        b.pages.remove(&p);
                    }
                }
                _ => return error(400, "InvalidHeaderValue", "x-ms-page-write must be update or clear"),
            }
            touch(b);
            blob_headers(Response::new(201), b)
        }
        ("PUT", "properties") => {
            let Some(b) = c.blobs.get_mut(&blob) else { return not_found() };
            if let Err(r) = check_lease(b, req) {
                return r;
            }
            if let Some(size) = req.header("x-ms-blob-content-length").and_then(|s| s.parse::<usize>().ok()) {
                if b.blob_type != "PageBlob" || size % 512 != 0 {
                    return error(400, "InvalidHeaderValue", "Invalid page blob size");
                }
                b.data.resize(size, 0);
                b.pages.retain(|p| (p + 1) * 512 <= size);
            }
            touch(b);
            blob_headers(Response::new(200), b)
        }
        ("GET", "pagelist") => {
            let Some(b) = c.blobs.get(&blob) else { return not_found() };
            let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><PageList>");
            let pages: Vec<usize> = b.pages.iter().copied().collect();
            let mut i = 0;
            while i < pages.len() {
                let start = pages[i];
                let mut end = start;
                while i + 1 < pages.len() && pages[i + 1] == end + 1 {
                    i += 1;
                    end = pages[i];
                }
                xml.push_str(&format!("<PageRange><Start>{}</Start><End>{}</End></PageRange>", start * 512, end * 512 + 511));
                i += 1;
            }
            xml.push_str("</PageList>");
            blob_headers(Response::xml(200, xml), b).with_header("x-ms-blob-content-length", b.data.len().to_string())
        }
        ("PUT", "") => {
            if let Some(existing) = c.blobs.get(&blob) {
                if let Err(r) = check_lease(existing, req) {
                    return r;
                }
            }
            let lease = c.blobs.get(&blob).and_then(|b| b.lease.clone());
            let btype = req.header("x-ms-blob-type").unwrap_or("").to_string();
            let meta = req.headers.iter().filter_map(|(k, v)| k.to_ascii_lowercase().strip_prefix("x-ms-meta-").map(|m| (m.to_string(), v.clone()))).collect();
            let ct = req.header("Content-Type").unwrap_or("application/octet-stream").to_string();
            let mut b = match btype.as_str() {
                "BlockBlob" => new_blob(req.body.clone(), ct, meta),
                "AppendBlob" => new_blob(Vec::new(), ct, meta),
                "PageBlob" => {
                    let Some(size) = req.header("x-ms-blob-content-length").and_then(|s| s.parse::<usize>().ok()).filter(|s| s % 512 == 0) else {
                        return error(400, "InvalidHeaderValue", "x-ms-blob-content-length must be a multiple of 512");
                    };
                    new_blob(vec![0; size], ct, meta)
                }
                _ => return error(400, "MissingRequiredHeader", "An HTTP header that's mandatory for this request is not specified (x-ms-blob-type)."),
            };
            b.blob_type = btype;
            b.lease = lease;
            let r = blob_headers(Response::new(201), &b).with_header("Content-MD5", base64_encode(&md5(&b.data)));
            c.blobs.insert(blob, b);
            r
        }
        ("GET" | "HEAD", "") => match c.blobs.get(&blob) {
            None => error(404, "BlobNotFound", "The specified blob does not exist."),
            Some(b) => {
                let mut r = blob_headers(Response::new(200), b)
                    .with_header("Content-Type", b.content_type.clone())
                    .with_header("Content-MD5", base64_encode(&md5(&b.data)))
                    .with_header("x-ms-blob-type", b.blob_type.clone())
                    .with_header("Content-Length", b.data.len().to_string())
                    .with_header("x-ms-lease-state", if active_lease(b).is_some() { "leased" } else { "available" });
                for (k, v) in &b.metadata {
                    r = r.with_header(&format!("x-ms-meta-{}", k), v.clone());
                }
                r.with_body(b.data.clone())
            }
        },
        ("DELETE", "") => {
            let Some(b) = c.blobs.get(&blob) else { return not_found() };
            if let Err(r) = check_lease(b, req) {
                return r;
            }
            c.blobs.remove(&blob);
            Response::new(202)
        }
        _ => error(405, "UnsupportedHttpVerb", "The resource doesn't support the specified HTTP verb."),
    }
}

// ---------------------------------------------------------------------------
// ADLS Gen2 (dfs)
// ---------------------------------------------------------------------------

fn dfs_error(status: u16, code: &str, message: &str) -> Response {
    Response::json(status, format!("{{\"error\":{{\"code\":\"{}\",\"message\":\"{}\"}}}}", code, message.replace('"', "'")))
        .with_header("x-ms-error-code", code)
        .with_header("x-ms-request-id", uuid())
}

fn is_dir(b: &Blob) -> bool {
    b.metadata.iter().any(|(k, v)| k == "hdi_isfolder" && v == "true")
}

fn folder() -> Blob {
    let mut b = new_blob(Vec::new(), "application/octet-stream".into(), vec![("hdi_isfolder".into(), "true".into())]);
    b.blob_type = "BlockBlob".into();
    b
}

fn ensure_parents(c: &mut Container, path: &str) {
    let mut acc = String::new();
    let parts: Vec<&str> = path.split('/').collect();
    for p in &parts[..parts.len().saturating_sub(1)] {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(p);
        c.blobs.entry(acc.clone()).or_insert_with(folder);
    }
}

/// DFS endpoint operations (`fs` = file system / container, `path` within it).
pub(super) fn dfs_op(st: &Shared, req: &ServerRequest, fs: Option<String>, path: Option<String>) -> Response {
    let mut s = st.lock().unwrap();
    let resource = req.query_param("resource").unwrap_or_default();
    let Some(fs) = fs else {
        if req.method == "GET" && resource == "account" {
            let items: Vec<String> = s.containers.iter().map(|(n, c)| format!("{{\"name\":\"{}\",\"lastModified\":\"{}\",\"etag\":\"0x1\"}}", n, http_date(c.created))).collect();
            return Response::json(200, format!("{{\"filesystems\":[{}]}}", items.join(",")));
        }
        return dfs_error(400, "InvalidQueryParameterValue", "Unsupported account operation");
    };
    let Some(path) = path else {
        return match (req.method.as_str(), resource.as_str()) {
            ("PUT", "filesystem") => {
                if s.containers.contains_key(&fs) {
                    return dfs_error(409, "FilesystemAlreadyExists", "The specified filesystem already exists.");
                }
                s.containers.insert(fs, Container { created: now_secs(), ..Default::default() });
                Response::new(201).with_header("ETag", "\"0x1\"")
            }
            ("DELETE", "filesystem") => match s.containers.remove(&fs) {
                Some(_) => Response::new(202),
                None => dfs_error(404, "FilesystemNotFound", "The specified filesystem does not exist."),
            },
            ("GET", "filesystem") => {
                let Some(c) = s.containers.get(&fs) else { return dfs_error(404, "FilesystemNotFound", "The specified filesystem does not exist.") };
                let recursive = req.query_param("recursive").as_deref() == Some("true");
                let dir = req.query_param("directory").unwrap_or_default();
                let prefix = if dir.is_empty() { String::new() } else { format!("{}/", dir.trim_end_matches('/')) };
                if !dir.is_empty() && !c.blobs.get(dir.trim_end_matches('/')).is_some_and(is_dir) {
                    return dfs_error(404, "PathNotFound", "The specified path does not exist.");
                }
                let items: Vec<String> = c
                    .blobs
                    .iter()
                    .filter(|(n, _)| n.starts_with(&prefix) && (recursive || !n[prefix.len()..].contains('/')))
                    .map(|(n, b)| {
                        format!(
                            "{{\"name\":\"{}\",\"isDirectory\":\"{}\",\"contentLength\":\"{}\",\"lastModified\":\"{}\",\"etag\":\"{}\"}}",
                            n,
                            is_dir(b),
                            b.data.len(),
                            http_date(b.last_modified),
                            b.etag.trim_matches('"')
                        )
                    })
                    .collect();
                Response::json(200, format!("{{\"paths\":[{}]}}", items.join(",")))
            }
            _ => dfs_error(400, "InvalidQueryParameterValue", "Unsupported filesystem operation"),
        };
    };
    let pending_key = (fs.clone(), path.clone());
    let pending_len = s.dfs_pending.get(&pending_key).map(|p| p.len()).unwrap_or(0);
    let Some(c) = s.containers.get_mut(&fs) else { return dfs_error(404, "FilesystemNotFound", "The specified filesystem does not exist.") };
    let action = req.query_param("action").unwrap_or_default();
    match (req.method.as_str(), resource.as_str(), action.as_str()) {
        ("PUT", "file", _) | ("PUT", "directory", _) => {
            ensure_parents(c, &path);
            let b = if resource == "directory" {
                folder()
            } else {
                let mut b = new_blob(Vec::new(), "application/octet-stream".into(), vec![]);
                b.blob_type = "BlockBlob".into();
                b
            };
            let r = Response::new(201).with_header("ETag", b.etag.clone()).with_header("Last-Modified", http_date(b.last_modified));
            c.blobs.insert(path, b);
            s.dfs_pending.remove(&pending_key);
            r
        }
        ("PATCH", _, "append") => {
            let Some(b) = c.blobs.get(&path) else { return dfs_error(404, "PathNotFound", "The specified path does not exist.") };
            let pos: usize = req.query_param("position").and_then(|p| p.parse().ok()).unwrap_or(usize::MAX);
            if pos != b.data.len() + pending_len {
                return dfs_error(400, "InvalidAppendPosition", "The position does not match the length of the file plus the uncommitted data.");
            }
            s.dfs_pending.entry(pending_key).or_default().extend_from_slice(&req.body);
            Response::new(202)
        }
        ("PATCH", _, "flush") => {
            let Some(b) = c.blobs.get_mut(&path) else { return dfs_error(404, "PathNotFound", "The specified path does not exist.") };
            let pos: usize = req.query_param("position").and_then(|p| p.parse().ok()).unwrap_or(usize::MAX);
            if pos != b.data.len() + pending_len {
                return dfs_error(400, "InvalidFlushPosition", "The uploaded data is not contiguous or the position query parameter value is not equal to the length of the file after appending the uploaded data.");
            }
            let pending = s.dfs_pending.remove(&pending_key).unwrap_or_default();
            let c = s.containers.get_mut(&fs).expect("checked");
            let b = c.blobs.get_mut(&path).expect("checked");
            b.data.extend_from_slice(&pending);
            touch(b);
            Response::new(200).with_header("ETag", b.etag.clone()).with_header("Last-Modified", http_date(b.last_modified))
        }
        ("GET" | "HEAD", _, _) => match c.blobs.get(&path) {
            None => dfs_error(404, "PathNotFound", "The specified path does not exist."),
            Some(b) => Response::new(200)
                .with_header("ETag", b.etag.clone())
                .with_header("Last-Modified", http_date(b.last_modified))
                .with_header("Content-Length", b.data.len().to_string())
                .with_header("x-ms-resource-type", if is_dir(b) { "directory" } else { "file" })
                .with_body(b.data.clone()),
        },
        ("DELETE", _, _) => {
            let Some(b) = c.blobs.get(&path) else { return dfs_error(404, "PathNotFound", "The specified path does not exist.") };
            let prefix = format!("{}/", path);
            if is_dir(b) {
                let has_children = c.blobs.keys().any(|k| k.starts_with(&prefix));
                if has_children && req.query_param("recursive").as_deref() != Some("true") {
                    return dfs_error(409, "DirectoryNotEmpty", "The recursive query parameter value must be true to delete a non-empty directory.");
                }
                c.blobs.retain(|k, _| !k.starts_with(&prefix));
            }
            c.blobs.remove(&path);
            Response::new(200)
        }
        _ => dfs_error(400, "InvalidQueryParameterValue", "Unsupported path operation"),
    }
}
