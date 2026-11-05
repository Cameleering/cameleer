//! `zipFile` data format with multi-entry support (replaces cameleer-core's single-entry one
//! when [`crate::register`] is called). Same options and behaviour as [`crate::tar`]:
//! `usingIterator`, `allowEmptyDirectory`, `preservePathElements`, `compressionLevel`;
//! entry names of an iterated archive are in the `CamelZipFileEntryNames` header.

use std::sync::Arc;

use cameleer_core::dataformat::compress::{zip_create, zip_read, ZipEntry};
use cameleer_core::dataformat::DataFormat;
use cameleer_core::error::Result;
use cameleer_core::message::Exchange;
use cameleer_core::message::props::FILE_NAME;
use cameleer_core::Value;

use crate::tar::{marshal_entries, unmarshal_entries, ArchiveOptions};
use crate::{body_bytes, Opts};

/// Header with the names of the entries when unmarshalling with `usingIterator`.
pub const ENTRY_NAMES: &str = "CamelZipFileEntryNames";

/// The `zipFile` data format.
pub struct ZipFileDataFormat {
    pub options: ArchiveOptions,
    pub level: u32,
}

impl DataFormat for ZipFileDataFormat {
    fn name(&self) -> &str {
        "zipFile"
    }
    fn marshal(&self, ex: &mut Exchange) -> Result<()> {
        let (entries, name) = marshal_entries(ex, &self.options);
        let z: Vec<ZipEntry> = entries.into_iter().map(|(n, d)| ZipEntry { name: n, data: d }).collect();
        ex.message.body = Value::Bytes(zip_create(&z, self.level));
        if !name.ends_with(".zip") {
            ex.message.headers.set(FILE_NAME, format!("{}.zip", name));
        }
        Ok(())
    }
    fn unmarshal(&self, ex: &mut Exchange) -> Result<()> {
        let entries = zip_read(&body_bytes(&ex.message.body))?;
        let triples = entries
            .into_iter()
            .map(|e| {
                let dir = e.name.ends_with('/') && e.data.is_empty();
                (e.name, e.data, dir)
            })
            .collect();
        unmarshal_entries(ex, triples, &self.options, ENTRY_NAMES, "Zip")
    }
}

pub(crate) fn create(o: &Opts) -> Result<Arc<dyn DataFormat>> {
    let l = o.num(&["compressionLevel", "level"], 6)?;
    Ok(Arc::new(ZipFileDataFormat { options: ArchiveOptions::from(o)?, level: if l < 0 { 6 } else { l.min(9) as u32 } }))
}
