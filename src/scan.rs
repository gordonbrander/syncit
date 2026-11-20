//! Scan: compare the working folder with the stat cache and write entries.

use crate::blobs::hash_file;
use crate::cache::{Row, mtime_ms};
use crate::entry::{Entry, Id};
use crate::segment;
use crate::vault::{TMP_PREFIX, Vault, write_atomic};
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Default)]
pub struct Scan {
    pub entries: Vec<Entry>,
    /// What was recorded, per path: "new", "modified" or "deleted".
    pub changes: Vec<(String, &'static str)>,
    /// Files that changed while they were being read.
    pub skipped: Vec<String>,
}

/// Scan the vault. A dry scan returns what it would write and changes nothing.
pub fn scan(v: &mut Vault, dry: bool) -> Result<Scan> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let (mut files, mut odd) = (BTreeSet::new(), 0);
    walk(v, &v.root, "", dry, &mut files, &mut odd)?;
    if odd > 0 {
        eprintln!(
            "warning: skipped {odd} symbolic links or special files in {}",
            v.root.display()
        );
    }

    let (mut cache, mut config, mut out) = (v.cache.clone(), v.config.clone(), Scan::default());
    let mut unwanted = vec![];
    for path in &files {
        let file = v.root.join(path);
        let row = cache.rows.get(path).cloned();
        if let (Some(row), Ok(meta)) = (&row, file.symlink_metadata())
            && cache.unchanged(row, meta.len(), mtime_ms(&meta))
        {
            continue;
        }
        let Ok(Some((size, mtime, hash))) = hash_stable(&file) else {
            out.skipped.push(path.clone());
            continue;
        };
        v.hint(&hash, file);
        let (parents, kind) = match row {
            Some(row) if row.hash == hash => {
                cache.rows.insert(path.clone(), Row { size, mtime, ..row });
                continue;
            }
            Some(row) => (row.base, "modified"),
            None => {
                if !config.wanted(path) {
                    unwanted.push(path.clone());
                }
                // A file this vault did not know it had: if the log knows
                // this content at this path, the vault holds that version.
                let known = v
                    .log
                    .valid(path)
                    .into_iter()
                    .filter(|(_, e)| e.hash.as_ref() == Some(&hash));
                let known: Vec<Id> = known.map(|(id, _)| id.clone()).collect();
                if !known.is_empty() {
                    cache.rows.insert(
                        path.clone(),
                        Row {
                            size,
                            mtime,
                            hash,
                            base: known,
                        },
                    );
                    continue;
                }
                (vec![], "new")
            }
        };
        let entry = Entry {
            hash: Some(hash.clone()),
            mtime,
            parents,
            path: path.clone(),
        };
        cache.rows.insert(
            path.clone(),
            Row {
                size,
                mtime,
                hash,
                base: vec![entry.id()],
            },
        );
        out.changes.push((path.clone(), kind));
        out.entries.push(entry);
    }

    let gone = |p: &&String| !files.contains(*p) && !v.skips(p);
    let gone: Vec<String> = cache.rows.keys().filter(gone).cloned().collect();
    for path in gone {
        let parents = cache
            .rows
            .remove(&path)
            .map(|row| row.base)
            .unwrap_or_default();
        out.changes.push((path.clone(), "deleted"));
        out.entries.push(Entry {
            hash: None,
            mtime: now,
            parents,
            path,
        });
    }

    // Shortest first, so one dragged-in folder yields one include.
    unwanted.sort_by_key(|p| p.len());
    unwanted.iter().for_each(|p| config.auto_include(p));

    if dry {
        return Ok(out);
    }
    // Segment, then config, then cache: stopped in between, the next scan
    // sees the same difference and writes the same entries again.
    if !out.entries.is_empty() {
        let (name, bytes) = segment::encode(&out.entries);
        write_atomic(&v.root, &v.dot().join("log").join(name), &bytes)?;
        out.entries.iter().for_each(|e| v.log.add(e.clone()));
    }
    if config != v.config {
        config.save(&v.root)?;
        v.config = config;
    }
    cache.scanned = now;
    cache.save(&v.root)?;
    v.cache = cache;
    Ok(out)
}

/// Collect the vault-relative paths of the regular files under `dir`.
/// An unreadable directory is an error: its files must not read as deleted.
fn walk(
    v: &Vault,
    dir: &Path,
    prefix: &str,
    dry: bool,
    files: &mut BTreeSet<String>,
    odd: &mut usize,
) -> Result<()> {
    for item in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
        let item = item?;
        let Some(name) = item.file_name().to_str().map(String::from) else {
            eprintln!(
                "warning: skipped a name that is not UTF-8 in {}",
                dir.display()
            );
            continue;
        };
        let path = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        let kind = item.file_type()?;
        if name.eq_ignore_ascii_case(".syncit") {
            if !prefix.is_empty() {
                eprintln!(
                    "warning: {} is a vault inside this one; its files are synced as part of {}",
                    dir.display(),
                    v.root.display()
                );
            }
            continue;
        }
        if v.ignore.matches(&path) {
            continue;
        }
        if kind.is_dir() {
            walk(v, &item.path(), &path, dry, files, odd)?;
        } else if !kind.is_file() {
            *odd += 1;
        } else if !name.starts_with(TMP_PREFIX) {
            files.insert(path);
        } else if !dry {
            fs::remove_file(item.path())?; // left by an interrupted run
        }
    }
    Ok(())
}

/// Size, mtime and hash of a file, or None if it changed while being read.
fn hash_stable(file: &Path) -> io::Result<Option<(u64, i64, String)>> {
    let before = file.symlink_metadata()?;
    let hash = hash_file(file)?;
    let after = file.symlink_metadata()?;
    let same = before.len() == after.len() && mtime_ms(&before) == mtime_ms(&after);
    Ok(same.then(|| (before.len(), mtime_ms(&before), hash)))
}
