//! `tarFile` data format: POSIX ustar archives with pax extended headers (and GNU long
//! names when reading).
//!
//! * marshal: the body becomes a tar archive with one entry named after the `CamelFileName`
//!   header (file name only unless `preservePathElements=true`); the header gets a `.tar`
//!   suffix. An object body (`{name: content, ...}`) produces one entry per key (extension).
//! * unmarshal: without `usingIterator`, the single entry becomes the body and its name the
//!   `CamelFileName` header (more than one entry fails like Camel). With `usingIterator=true`
//!   the body is an array with the content of each file entry (directories are included as
//!   empty bodies only with `allowEmptyDirectory=true`) and the `CamelTarFileEntryNames`
//!   header lists the entry names (index aligned).

use std::sync::Arc;

use camel_core::dataformat::DataFormat;
use camel_core::error::Result;
use camel_core::message::props::FILE_NAME;
use camel_core::message::Exchange;
use camel_core::Value;

use crate::{body_bytes, df_err, Opts};

/// Header with the names of the entries when unmarshalling with `usingIterator`.
pub const ENTRY_NAMES: &str = "CamelTarFileEntryNames";

/// One tar entry.
#[derive(Clone, Debug, PartialEq)]
pub struct TarEntry {
    pub name: String,
    pub data: Vec<u8>,
    pub is_dir: bool,
    pub mode: u32,
    pub mtime: u64,
}

impl TarEntry {
    pub fn file(name: impl Into<String>, data: Vec<u8>) -> TarEntry {
        TarEntry { name: name.into(), data, is_dir: false, mode: 0o644, mtime: now_secs() }
    }
    pub fn dir(name: impl Into<String>) -> TarEntry {
        let mut n: String = name.into();
        if !n.ends_with('/') {
            n.push('/');
        }
        TarEntry { name: n, data: Vec::new(), is_dir: true, mode: 0o755, mtime: now_secs() }
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn octal(field: &mut [u8], v: u64) {
    let w = field.len() - 1;
    let s = format!("{:0w$o}", v, w = w);
    if s.len() > w {
        // base-256 (GNU) for big numbers
        let n = field.len();
        for (i, b) in field.iter_mut().enumerate() {
            let shift = (n - 1 - i) * 8;
            *b = if shift < 64 { (v >> shift) as u8 } else { 0 };
        }
        field[0] |= 0x80;
        return;
    }
    field[..w].copy_from_slice(s.as_bytes());
    field[w] = 0;
}

fn header_block(name: &[u8], size: u64, typeflag: u8, mode: u32, mtime: u64, prefix: &[u8]) -> [u8; 512] {
    let mut h = [0u8; 512];
    h[..name.len()].copy_from_slice(name);
    octal(&mut h[100..108], mode as u64);
    octal(&mut h[108..116], 0);
    octal(&mut h[116..124], 0);
    octal(&mut h[124..136], size);
    octal(&mut h[136..148], mtime);
    h[156] = typeflag;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix);
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| *b as u32).sum();
    let s = format!("{:06o}\0 ", sum);
    h[148..156].copy_from_slice(s.as_bytes());
    h
}

fn pad(out: &mut Vec<u8>) {
    while out.len() % 512 != 0 {
        out.push(0);
    }
}

/// Split a long name into ustar (prefix, name) if possible.
fn split_ustar(name: &str) -> Option<(String, String)> {
    let b = name.as_bytes();
    if b.len() <= 100 {
        return Some((String::new(), name.to_string()));
    }
    for (i, c) in name.char_indices() {
        if c == '/' && i <= 155 && b.len() - i - 1 <= 100 && b.len() - i - 1 > 0 {
            return Some((name[..i].to_string(), name[i + 1..].to_string()));
        }
    }
    None
}

fn pax_record(key: &str, value: &str) -> String {
    let body = format!(" {}={}\n", key, value);
    let mut len = body.len() + 1;
    loop {
        let candidate = len.to_string().len() + body.len();
        if candidate == len {
            break;
        }
        len = candidate;
    }
    format!("{}{}", len, body)
}

/// Write a tar archive (ustar; pax `path` records for names that do not fit).
pub fn tar_create(entries: &[TarEntry]) -> Vec<u8> {
    let mut out = Vec::new();
    for e in entries {
        let size = if e.is_dir { 0 } else { e.data.len() as u64 };
        let typeflag = if e.is_dir { b'5' } else { b'0' };
        let (prefix, name) = match split_ustar(&e.name).filter(|_| e.name.is_ascii()) {
            Some(pn) => pn,
            None => {
                let rec = pax_record("path", &e.name);
                let short: String = format!("PaxHeaders/{}", e.name.rsplit('/').find(|s| !s.is_empty()).unwrap_or("x"));
                let short: String = short.chars().filter(|c| c.is_ascii()).take(99).collect();
                out.extend_from_slice(&header_block(short.as_bytes(), rec.len() as u64, b'x', 0o644, e.mtime, b""));
                out.extend_from_slice(rec.as_bytes());
                pad(&mut out);
                let fallback: String = e.name.chars().filter(|c| c.is_ascii()).collect();
                let fb = fallback.as_bytes();
                let fb = &fb[fb.len().saturating_sub(100)..];
                (String::new(), String::from_utf8_lossy(fb).to_string())
            }
        };
        out.extend_from_slice(&header_block(name.as_bytes(), size, typeflag, e.mode, e.mtime, prefix.as_bytes()));
        if !e.is_dir {
            out.extend_from_slice(&e.data);
            pad(&mut out);
        }
    }
    out.extend_from_slice(&[0u8; 1024]);
    out
}

fn cstr(b: &[u8]) -> String {
    let end = b.iter().position(|c| *c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).to_string()
}

fn parse_num(b: &[u8]) -> Result<u64> {
    if !b.is_empty() && b[0] & 0x80 != 0 {
        let mut v: u64 = (b[0] & 0x7f) as u64;
        for x in &b[1..] {
            v = (v << 8) | *x as u64;
        }
        return Ok(v);
    }
    let s = cstr(b);
    let s = s.trim_matches(|c: char| c == ' ' || c == '\0');
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| df_err(format!("Invalid tar header number: {:?}", s)))
}

fn parse_pax(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let sp = match data[i..].iter().position(|c| *c == b' ') {
            Some(p) => i + p,
            None => break,
        };
        let len: usize = match std::str::from_utf8(&data[i..sp]).ok().and_then(|s| s.parse().ok()) {
            Some(l) if l > 0 && i + l <= data.len() => l,
            _ => break,
        };
        let rec = &data[sp + 1..i + len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(eq) = rec.iter().position(|c| *c == b'=') {
            out.push((String::from_utf8_lossy(&rec[..eq]).to_string(), String::from_utf8_lossy(&rec[eq + 1..]).to_string()));
        }
        i += len;
    }
    out
}

/// Read a tar archive (ustar, pax `x` headers, GNU `L` long names). Links, devices and
/// FIFOs are skipped.
pub fn tar_read(data: &[u8]) -> Result<Vec<TarEntry>> {
    let mut entries = Vec::new();
    let mut i = 0;
    let mut pending_name: Option<String> = None;
    let mut pending_size: Option<u64> = None;
    while i + 512 <= data.len() {
        let h = &data[i..i + 512];
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let stored: u64 = parse_num(&h[148..156])?;
        let mut sum: u64 = h.iter().map(|b| *b as u64).sum();
        sum = sum - h[148..156].iter().map(|b| *b as u64).sum::<u64>() + 8 * 32;
        if sum != stored {
            return Err(df_err("Invalid tar header checksum"));
        }
        let typeflag = h[156];
        let mut size = parse_num(&h[124..136])?;
        if let Some(s) = pending_size.take() {
            size = s;
        }
        let start = i + 512;
        let end = start.checked_add(size as usize).filter(|e| *e <= data.len()).ok_or_else(|| df_err("Truncated tar archive"))?;
        let content = &data[start..end];
        i = start + (size as usize).div_ceil(512) * 512;
        match typeflag {
            b'x' => {
                for (k, v) in parse_pax(content) {
                    match k.as_str() {
                        "path" => pending_name = Some(v),
                        "size" => pending_size = v.parse().ok(),
                        _ => {}
                    }
                }
                continue;
            }
            b'g' => continue,
            b'L' => {
                pending_name = Some(cstr(content));
                continue;
            }
            _ => {}
        }
        let name = match pending_name.take() {
            Some(n) => n,
            None => {
                let n = cstr(&h[0..100]);
                let magic = &h[257..263];
                let prefix = if magic == b"ustar\0" { cstr(&h[345..500]) } else { String::new() };
                if prefix.is_empty() {
                    n
                } else {
                    format!("{}/{}", prefix, n)
                }
            }
        };
        let mode = parse_num(&h[100..108]).unwrap_or(0o644) as u32;
        let mtime = parse_num(&h[136..148]).unwrap_or(0);
        match typeflag {
            b'0' | 0 | b'7' => {
                if name.ends_with('/') && content.is_empty() {
                    entries.push(TarEntry { name, data: Vec::new(), is_dir: true, mode, mtime });
                } else {
                    entries.push(TarEntry { name, data: content.to_vec(), is_dir: false, mode, mtime });
                }
            }
            b'5' => entries.push(TarEntry { name, data: Vec::new(), is_dir: true, mode, mtime }),
            _ => {}
        }
    }
    Ok(entries)
}

/// Shared archive data format options (also used by [`crate::zip`]).
#[derive(Clone, Debug, Default)]
pub struct ArchiveOptions {
    pub using_iterator: bool,
    pub allow_empty_directory: bool,
    pub preserve_path_elements: bool,
}

impl ArchiveOptions {
    pub fn from(o: &Opts) -> Result<ArchiveOptions> {
        Ok(ArchiveOptions {
            using_iterator: o.bool(&["usingIterator"], false)?,
            allow_empty_directory: o.bool(&["allowEmptyDirectory"], false)?,
            preserve_path_elements: o.bool(&["preservePathElements"], false)?,
        })
    }
}

/// Entries to marshal: (name, data) pairs, and the base file name.
pub(crate) fn marshal_entries(ex: &Exchange, opts: &ArchiveOptions) -> (Vec<(String, Vec<u8>)>, String) {
    let name = ex.message.headers.get(FILE_NAME).map(|v| v.to_string()).unwrap_or_else(|| ex.exchange_id.clone());
    let entry_name = |n: &str| -> String {
        if opts.preserve_path_elements {
            n.trim_start_matches('/').to_string()
        } else {
            n.rsplit(['/', '\\']).next().unwrap_or(n).to_string()
        }
    };
    let entries = match &ex.message.body {
        Value::Object(m) => m.iter().map(|(k, v)| (entry_name(k), body_bytes(v))).collect(),
        other => vec![(entry_name(&name), body_bytes(other))],
    };
    (entries, name)
}

/// Unmarshal result handling shared by tar and zip: `(name, data, is_dir)` triples.
pub(crate) fn unmarshal_entries(ex: &mut Exchange, entries: Vec<(String, Vec<u8>, bool)>, opts: &ArchiveOptions, names_header: &str, kind: &str) -> Result<()> {
    if opts.using_iterator {
        let mut names = Vec::new();
        let mut bodies = Vec::new();
        for (n, d, is_dir) in entries {
            if is_dir && !opts.allow_empty_directory {
                continue;
            }
            names.push(Value::String(n));
            bodies.push(Value::Bytes(d));
        }
        ex.message.headers.set(names_header, Value::Array(names));
        ex.message.body = Value::Array(bodies);
        return Ok(());
    }
    let files: Vec<_> = entries.into_iter().filter(|e| !e.2).collect();
    if files.len() > 1 {
        return Err(camel_core::CamelError::new("IllegalStateException", format!("{} file has more than 1 entry.", kind)));
    }
    match files.into_iter().next() {
        Some((n, d, _)) => {
            ex.message.headers.set(FILE_NAME, n);
            ex.message.body = Value::Bytes(d);
        }
        None => ex.message.body = Value::Null,
    }
    Ok(())
}

/// The `tarFile` data format.
pub struct TarFileDataFormat {
    pub options: ArchiveOptions,
}

impl DataFormat for TarFileDataFormat {
    fn name(&self) -> &str {
        "tarFile"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let (entries, name) = marshal_entries(ex, &self.options);
        let tar: Vec<TarEntry> = entries.into_iter().map(|(n, d)| TarEntry::file(n, d)).collect();
        ex.message.body = Value::Bytes(tar_create(&tar));
        if !name.ends_with(".tar") {
            ex.message.headers.set(FILE_NAME, format!("{}.tar", name));
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let entries = tar_read(&body_bytes(&ex.message.body))?;
        let triples = entries.into_iter().map(|e| (e.name, e.data, e.is_dir)).collect();
        unmarshal_entries(ex, triples, &self.options, ENTRY_NAMES, "Tar")
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    Ok(Arc::new(TarFileDataFormat { options: ArchiveOptions::from(o)? }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_long_names() {
        let long = format!("{}/file.txt", "d".repeat(150));
        let very_long = format!("{}.txt", "x".repeat(200));
        let entries = vec![
            TarEntry::file("a.txt", b"hello".to_vec()),
            TarEntry::dir("dir"),
            TarEntry::file(long.clone(), vec![7; 1000]),
            TarEntry::file(very_long.clone(), b"v".to_vec()),
            TarEntry::file("ünïcode.txt", b"u".to_vec()),
        ];
        let t = tar_create(&entries);
        assert_eq!(t.len() % 512, 0);
        let back = tar_read(&t).unwrap();
        let names: Vec<_> = back.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["a.txt".to_string(), "dir/".into(), long, very_long, "ünïcode.txt".into()]);
        assert_eq!(back[2].data, vec![7; 1000]);
        assert!(back[1].is_dir);
    }

    #[test]
    fn pax_record_length_is_self_consistent() {
        let r = pax_record("path", "abc");
        assert_eq!(r, "12 path=abc\n");
        let r = pax_record("path", &"y".repeat(95));
        assert_eq!(r.len(), r.split(' ').next().unwrap().parse::<usize>().unwrap());
    }
}
