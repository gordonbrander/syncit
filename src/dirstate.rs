//! `.syncit/dirstate`: this vault's position in the log, which the log cannot
//! record because it is the same in every vault. It says which version of
//! each file this vault has on disk, and which deletions it has seen. It is
//! not a copy of anything: only the size and mtime in a row are a cache.

use crate::blobs::hash_file;
use crate::entry::Id;
use crate::vault::write_atomic;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// A row whose mtime is this close to the scan that recorded it may hide an
/// edit made in the same timestamp tick, so the file is hashed again.
pub const TICK_MARGIN_MS: i64 = 2000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Row {
    pub size: u64,
    pub mtime: i64,
    pub hash: String,
    pub base: Vec<Id>, // the entries the file was derived from; sorted
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Dirstate {
    pub scanned: i64, // when the most recent scan started, in milliseconds
    pub rows: BTreeMap<String, Row>,
    /// Paths this vault has seen deleted, each with its base: the heads, all
    /// tombstones, that the absence of a file here reflects. A path is in at
    /// most one of `rows` and `deleted`.
    #[serde(default)]
    pub deleted: BTreeMap<String, Vec<Id>>,
}

/// A file's mtime in milliseconds since the epoch, rounded down.
pub fn mtime_ms(meta: &Metadata) -> i64 {
    meta.mtime() * 1000 + meta.mtime_nsec() / 1_000_000
}

impl Dirstate {
    /// A missing file is an empty dirstate.
    pub fn load(root: &Path) -> Result<Dirstate> {
        let file = root.join(".syncit/dirstate");
        match fs::read_to_string(&file) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("corrupt dirstate {}", file.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Dirstate::default()),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
        }
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        write_atomic(
            root,
            &root.join(".syncit/dirstate"),
            serde_json::to_string(self)?.as_bytes(),
        )
    }

    /// The unchanged test: may a file with this size and mtime be taken as
    /// the row's content without hashing it?
    pub fn unchanged(&self, row: &Row, size: u64, mtime: i64) -> bool {
        size == row.size && mtime == row.mtime && row.mtime < self.scanned - TICK_MARGIN_MS
    }

    /// Whether the file on disk still has the row's content.
    pub fn matches(&self, row: &Row, file: &Path) -> bool {
        let Ok(meta) = file.symlink_metadata() else {
            return false;
        };
        meta.is_file()
            && (self.unchanged(row, meta.len(), mtime_ms(&meta))
                || hash_file(file).is_ok_and(|h| h == row.hash))
    }
}
