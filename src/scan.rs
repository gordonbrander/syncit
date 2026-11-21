//! Scan: compare the working folder with the stat dirstate and write entries.

use crate::blobs::hash_file;
use crate::dirstate::{Row, is_exec, mtime_ms};
use crate::entry::{Entry, Id};
use crate::log::{Log, PathState};
use crate::output::{Options, warn};
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

/// Scan the vault. A dry scan writes nothing: the vault value is brought up
/// to date in memory only.
pub fn scan(opts: &Options, v: &mut Vault, dry: bool) -> Result<Scan> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let (mut files, mut odd) = (BTreeSet::new(), 0);
    walk(opts, v, &v.root, "", dry, &mut files, &mut odd)?;
    if odd > 0 {
        let root = v.root.display();
        warn(
            opts,
            format!("skipped {odd} symbolic links or special files in {root}"),
        );
    }

    let (mut dirstate, mut config, mut out) =
        (v.dirstate.clone(), v.config.clone(), Scan::default());
    let mut excluded = vec![];
    for path in &files {
        // A file on disk is included, whether or not the vault knew it.
        if !config.includes(path) {
            excluded.push(path.clone());
        }
        let file = v.root.join(path);
        let row = dirstate.rows.get(path).cloned();
        if let (Some(row), Ok(meta)) = (&row, file.symlink_metadata())
            && dirstate.unchanged(row, &meta)
        {
            continue;
        }
        let Ok(Some((size, mtime, exec, hash))) = hash_stable(&file) else {
            out.skipped.push(path.clone());
            continue;
        };
        v.hint(&hash, file);
        let (parents, kind) = match row {
            Some(row) if row.hash == hash && row.exec == exec => {
                dirstate
                    .rows
                    .insert(path.clone(), Row { size, mtime, ..row });
                continue;
            }
            Some(row) => (row.base, "modified"),
            None => {
                if let Some(base) = dirstate.deleted.remove(path) {
                    // Re-created where this vault saw a deletion: the new
                    // version replaces the tombstones it had seen.
                    (base, "new")
                } else if let Some(tombstones) = deleted_heads(&v.log, path) {
                    // No record of what this vault saw, and nothing live at
                    // the path: an add. Tombstones have no content to lose.
                    (tombstones, "new")
                } else {
                    // A file this vault did not know it had: if the log knows
                    // this content at this path, the vault has that version.
                    // With the other executable bit, it has changed that version.
                    let known = v.log.valid(path).into_iter();
                    let known: Vec<_> = known
                        .filter(|(_, e)| e.hash.as_ref() == Some(&hash))
                        .collect();
                    let ids = |exec: Option<bool>| -> Vec<Id> {
                        let same = known
                            .iter()
                            .filter(|(_, e)| exec.is_none_or(|x| x == e.exec));
                        same.map(|(id, _)| (*id).clone()).collect()
                    };
                    let same = ids(Some(exec));
                    if !same.is_empty() {
                        dirstate.rows.insert(
                            path.clone(),
                            Row {
                                size,
                                mtime,
                                exec,
                                hash,
                                base: same,
                            },
                        );
                        continue;
                    }
                    let kind = if known.is_empty() { "new" } else { "modified" };
                    (ids(None), kind)
                }
            }
        };
        let entry = Entry {
            exec,
            hash: Some(hash.clone()),
            mtime,
            parents,
            path: path.clone(),
        };
        dirstate.rows.insert(
            path.clone(),
            Row {
                size,
                mtime,
                exec,
                hash,
                base: vec![entry.id()],
            },
        );
        out.changes.push((path.clone(), kind));
        out.entries.push(entry);
    }

    let gone = |p: &&String| !files.contains(*p) && !v.skips(p);
    let gone: Vec<String> = dirstate.rows.keys().filter(gone).cloned().collect();
    for path in gone {
        let parents = dirstate
            .rows
            .remove(&path)
            .map(|row| row.base)
            .unwrap_or_default();
        out.changes.push((path.clone(), "deleted"));
        let entry = Entry {
            exec: false,
            hash: None,
            mtime: now,
            parents,
            path,
        };
        dirstate
            .deleted
            .insert(entry.path.clone(), vec![entry.id()]);
        out.entries.push(entry);
    }

    // Shortest first, so one dragged-in folder yields one include.
    excluded.sort_by_key(|p| p.len());
    excluded.iter().for_each(|p| config.auto_include(p));

    // Segment, then config, then dirstate: stopped in between, the next scan
    // sees the same difference and writes the same entries again.
    if !out.entries.is_empty() && !dry {
        let (name, bytes) = segment::encode(&v.config.nickname, &out.entries);
        write_atomic(&v.root, &v.dot().join("log").join(name), &bytes)?;
    }
    for e in &out.entries {
        v.log.add(e.clone(), &v.config.nickname);
    }
    if config != v.config && !dry {
        config.save(&v.root)?;
    }
    v.config = config;
    dirstate.scanned = now;
    if !dry {
        dirstate.save(&v.root)?;
    }
    v.dirstate = dirstate;
    Ok(out)
}

/// The heads of a path whose every head is a tombstone; None for any other
/// path. A file beats a tombstone, so a tombstone winner means all of them.
fn deleted_heads(log: &Log, path: &str) -> Option<Vec<Id>> {
    match log.resolve(path) {
        PathState::Resolved { heads, winner, .. } if log.entries[&winner].hash.is_none() => {
            Some(heads)
        }
        _ => None,
    }
}

/// Collect the vault-relative paths of the regular files under `dir`.
/// An unreadable directory is an error: its files must not read as deleted.
fn walk(
    opts: &Options,
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
            warn(
                opts,
                format!("skipped a name that is not UTF-8 in {}", dir.display()),
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
                let (inner, outer) = (dir.display(), v.root.display());
                warn(
                    opts,
                    format!(
                        "{inner} is a vault inside this one; its files are synced as part of {outer}"
                    ),
                );
            }
            continue;
        }
        if v.ignore.matches(&path) {
            continue;
        }
        if kind.is_dir() {
            walk(opts, v, &item.path(), &path, dry, files, odd)?;
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

/// Size, mtime, executable bit and hash of a file, or None if it changed
/// while being read.
fn hash_stable(file: &Path) -> io::Result<Option<(u64, i64, bool, String)>> {
    let before = file.symlink_metadata()?;
    let hash = hash_file(file)?;
    let after = file.symlink_metadata()?;
    let same = before.len() == after.len() && mtime_ms(&before) == mtime_ms(&after);
    Ok(same.then(|| (before.len(), mtime_ms(&before), is_exec(&after), hash)))
}
