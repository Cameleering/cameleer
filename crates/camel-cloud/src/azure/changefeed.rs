//! Azure Blob Storage **change feed** reader (used by `azure-storage-blob`'s `getChangeFeed`
//! operation and its change feed consumer).
//!
//! The change feed lives in the `$blobchangefeed` container: `meta/segments.json` gives
//! `lastConsumable` (the end of the readable feed), `idx/segments/YYYY/MM/DD/hhmm/meta.json`
//! describe hourly segments whose `chunkFilePaths` point at `log/<shard>/YYYY/MM/DD/hhmm/`
//! prefixes of Avro object container files with one `BlobChangeEvent` record per change
//! (`topic`, `subject` = `/blobServices/default/containers/<c>/blobs/<b>`, `eventType`
//! `BlobCreated` / `BlobDeleted` / ..., `eventTime`, `id`, `data` with `api`, `requestId`,
//! `etag`, `contentType`, `contentLength`, `blobType`, `url`, `sequencer`...). Records are
//! decoded with the writer schema stored in each file (camel-dataformats' Avro reader).

use camel_core::error::{CamelError, Result};
use camel_core::value::Value;

use super::AzureConfig;
use crate::http::Request;
use crate::util::{kinds, parse_iso8601, parse_json, uri_encode, Xml};

/// The change feed container.
pub const CONTAINER: &str = "$blobchangefeed";

/// Epoch millis of an ISO-8601 timestamp (fractional seconds kept to the millisecond).
pub fn iso_millis(s: &str) -> Option<i64> {
    let secs = parse_iso8601(s)?;
    let frac = s.split_once('.').map(|(_, f)| f.chars().take_while(|c| c.is_ascii_digit()).collect::<String>()).unwrap_or_default();
    let ms = if frac.is_empty() { 0 } else { format!("{:0<3}", &frac[..frac.len().min(3)]).parse::<i64>().unwrap_or(0) };
    Some(secs * 1000 + ms)
}

/// A time option / header value: epoch millis or ISO-8601 text.
pub fn time_value(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Null => None,
        other => {
            let s = other.to_string();
            s.trim().parse::<i64>().ok().or_else(|| iso_millis(s.trim()))
        }
    }
}

fn list(azure: &AzureConfig, prefix: &str) -> Result<Vec<String>> {
    let mut names = Vec::new();
    let mut marker = String::new();
    loop {
        let mut q = format!("restype=container&comp=list&prefix={}", uri_encode(prefix, false));
        if !marker.is_empty() {
            q.push_str(&format!("&marker={}", uri_encode(&marker, false)));
        }
        let resp = azure.send(Request::new("GET", azure.url("blob", &uri_encode(CONTAINER, false), &q)?), kinds::BLOB_STORAGE)?;
        let x = Xml::parse(&resp.text())?;
        for b in x.descendants("Blob") {
            if let Some(n) = x.child_text(b, "Name") {
                names.push(n);
            }
        }
        marker = x.find_text("NextMarker").unwrap_or_default();
        if marker.is_empty() {
            return Ok(names);
        }
    }
}

fn get(azure: &AzureConfig, name: &str) -> Result<Vec<u8>> {
    let path = format!("{}/{}", uri_encode(CONTAINER, false), uri_encode(name, true));
    Ok(azure.send(Request::new("GET", azure.url("blob", &path, "")?), kinds::BLOB_STORAGE)?.body)
}

/// Segment start (epoch millis) from `idx/segments/YYYY/MM/DD/hhmm/meta.json`.
fn segment_time(name: &str) -> Option<i64> {
    let p: Vec<&str> = name.trim_start_matches("idx/segments/").split('/').collect();
    if p.len() < 4 {
        return None;
    }
    let hhmm = p[3];
    let text = format!("{}-{}-{}T{}:{}:00Z", p[0], p[1], p[2], hhmm.get(0..2)?, hhmm.get(2..4)?);
    parse_iso8601(&text).map(|s| s * 1000)
}

/// Read the change feed events with `start <= eventTime < end` (millis), oldest first.
pub fn read(azure: &AzureConfig, start: Option<i64>, end: Option<i64>) -> Result<Vec<Value>> {
    let meta = parse_json(&get(azure, "meta/segments.json").map_err(|e| {
        CamelError::new(e.kind.clone(), format!("The change feed is not enabled for account {} (no $blobchangefeed/meta/segments.json): {}", azure.account, e.message))
    })?)?;
    let last_consumable = meta.get("lastConsumable").and_then(|l| iso_millis(&l.to_string())).unwrap_or(i64::MAX);
    let end = end.unwrap_or(i64::MAX).min(last_consumable);
    let mut segments: Vec<(i64, String)> = list(azure, "idx/segments/")?.into_iter().filter(|n| n.ends_with("meta.json")).filter_map(|n| segment_time(&n).map(|t| (t, n))).collect();
    segments.sort();
    let mut events = Vec::new();
    for (t, seg) in segments {
        // segments cover one hour
        if t >= end || start.is_some_and(|s| t + 3_600_000 <= s) {
            continue;
        }
        let sm = parse_json(&get(azure, &seg)?)?;
        for chunk in sm.get("chunkFilePaths").and_then(|c| c.as_array()).into_iter().flatten() {
            let prefix = chunk.to_string();
            let prefix = prefix.trim_start_matches('/').trim_start_matches(CONTAINER).trim_start_matches('/').to_string();
            let mut files = list(azure, &prefix)?;
            files.sort();
            for f in files {
                let data = get(azure, &f)?;
                let c = camel_dataformats::avro::read_container(&data, camel_dataformats::avro::Conv::default())?;
                for r in c.records {
                    let et = r.get("eventTime").and_then(|t| iso_millis(&t.to_string())).unwrap_or(0);
                    if et < end && start.is_none_or(|s| et >= s) {
                        events.push((et, r));
                    }
                }
            }
        }
    }
    events.sort_by_key(|(t, _)| *t);
    Ok(events.into_iter().map(|(_, e)| e).collect())
}

/// (container, blob) of an event subject.
pub fn subject_parts(subject: &str) -> (String, String) {
    let rest = subject.trim_start_matches("/blobServices/default/containers/");
    match rest.split_once("/blobs/") {
        Some((c, b)) => (c.to_string(), b.to_string()),
        None => (rest.to_string(), String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_and_subjects() {
        assert_eq!(iso_millis("1970-01-01T00:00:01.5Z"), Some(1500));
        assert_eq!(iso_millis("1970-01-01T00:00:02.1234567Z"), Some(2123));
        assert_eq!(time_value(&Value::from("1970-01-01T00:01:00Z")), Some(60_000));
        assert_eq!(time_value(&Value::Int(5)), Some(5));
        assert_eq!(segment_time("idx/segments/1970/01/01/0100/meta.json"), Some(3_600_000));
        assert_eq!(subject_parts("/blobServices/default/containers/c1/blobs/dir/a.txt"), ("c1".into(), "dir/a.txt".into()));
    }
}
