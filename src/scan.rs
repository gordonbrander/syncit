//! Scan: compare the working folder with the stat dirstate and write entries.

use crate::backend::Backend;
use crate::blobs::hash_file;
use crate::dirstate::{Dirstate, Row, TICK_MARGIN_MS, is_exec, mtime_ms};
use crate::entry::{Entry, Id};
use crate::log::{Log, PathState};
use crate::output::{Context, Progress, warn};
use crate::segment;
use crate::vault::{Dir, TMP_PREFIX, Vault};
use anyhow::{Context as _, Result};
use std::collections::{BTreeSet, HashMap};
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
    /// Deletions and new files that are one file moved: `(from, to)`.
    pub moves: Vec<(String, String)>,
    /// From a dry scan that learned something about files it recorded
    /// nothing for: the dirstate as it was before the scan, with the size and
    /// mtime of each row whose file was hashed and found the same, with no
    /// size in each row whose file looks the same and is not, and with
    /// `scanned` moved on. Nothing in it is a record of a change. For
    /// `status` to save, if the dirstate the scan began with is the one on
    /// disk.
    pub cache: Option<Dirstate>,
}

/// Pair deleted and added files, each `(path, hash)`, into moves `(from,
/// to)`, as git does: by content. Only a hash found once on each side pairs,
/// so copies, empty files and a move with an edit stay a delete and an add.
pub fn moves(deleted: &[(String, String)], added: &[(String, String)]) -> Vec<(String, String)> {
    let once = |side: &[(String, String)]| {
        let mut seen: HashMap<String, Option<String>> = HashMap::new();
        for (path, hash) in side {
            seen.entry(hash.clone())
                .and_modify(|p| *p = None)
                .or_insert_with(|| Some(path.clone()));
        }
        seen
    };
    let (gone, new) = (once(deleted), once(added));
    let mut pairs: Vec<_> = gone
        .into_iter()
        .filter_map(|(hash, from)| Some((from?, new.get(&hash)?.clone()?)))
        .collect();
    pairs.sort();
    pairs
}

/// Scan the vault. A dry scan writes nothing: the vault value is brought up
/// to date in memory only.
pub fn scan(ctx: &Context, v: &mut Vault<impl Backend, Dir>, dry: bool) -> Result<Scan> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let (mut files, mut odd) = (BTreeSet::new(), 0);
    walk(ctx, v, &v.worktree.root, "", dry, &mut files, &mut odd)?;
    if odd > 0 {
        let root = v.worktree.root.display();
        warn(
            ctx,
            format!("skipped {odd} symbolic links or special files in {root}"),
        );
    }

    let mut dirstate = v.worktree.dirstate.clone();
    let mut out = Scan::default();
    // Files on disk that the config does not include yet.
    let mut excluded = vec![];
    // New files, `(path, hash)`, to pair with deletions into moves.
    let mut added = vec![];
    // For `Scan::cache`: rows to refresh, rows to smudge, and whether a
    // refresh changes what the next scan hashes.
    let mut refreshed = vec![];
    let mut smudged = vec![];
    let mut learned = false;
    let progress = Progress::new(&ctx.bar, "scanning", files.len());
    for path in &files {
        progress.tick();
        // A file on disk is included, whether or not the vault knew it.
        if !v.config.includes(path) {
            excluded.push(path);
        }
        let file = v.worktree.root.join(path);
        let row = dirstate.rows.get(path).cloned();
        let meta = file.symlink_metadata().ok();
        if let (Some(row), Some(meta)) = (&row, &meta)
            && dirstate.unchanged(row, meta)
        {
            continue;
        }
        // The file has its row's size and mtime, and is hashed only because
        // it may have been edited in the tick it was last hashed in.
        let looks_clean = matches!((&row, &meta), (Some(row), Some(meta))
            if Some(meta.len()) == row.size && mtime_ms(meta) == row.mtime);
        let Ok(Some((size, mtime, exec, hash))) = hash_stable(&file) else {
            if looks_clean {
                smudged.push(path.clone());
            }
            out.skipped.push(path.clone());
            continue;
        };
        v.worktree.hint(&hash, file);
        let size = Some(size);
        let (parents, kind) = match row {
            Some(row) if row.hash == hash && row.exec == exec => {
                learned |= (size, mtime) != (row.size, row.mtime) || mtime < now - TICK_MARGIN_MS;
                refreshed.push((path.clone(), size, mtime));
                dirstate
                    .rows
                    .insert(path.clone(), Row { size, mtime, ..row });
                continue;
            }
            Some(row) => {
                if looks_clean {
                    smudged.push(path.clone());
                }
                (row.base, "modified")
            }
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
        if kind == "new" {
            added.push((path.clone(), hash.clone()));
        }
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
    drop(progress);

    let gone = |p: &&String| !files.contains(*p) && !v.skips(p);
    let gone: Vec<String> = dirstate.rows.keys().filter(gone).cloned().collect();
    let mut deleted = vec![];
    for path in gone {
        let mut parents = vec![];
        if let Some(row) = dirstate.rows.remove(&path) {
            deleted.push((path.clone(), row.hash));
            parents = row.base;
        }
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

    out.moves = moves(&deleted, &added);

    // Shortest first, so one dragged-in folder yields one include.
    let mut config = v.config.clone();
    excluded.sort_by_key(|p| p.len());
    excluded.iter().for_each(|p| config.auto_include(p));

    // Segment, then config, then dirstate: stopped in between, the next scan
    // sees the same difference and writes the same entries again.
    if !out.entries.is_empty() && !dry {
        let (name, bytes) = segment::encode(&v.config.nickname, &out.entries);
        v.store.write_segment(&name, &bytes)?;
    }
    for e in &out.entries {
        v.log.add(e.clone(), &v.config.nickname);
    }
    if config != v.config && !dry {
        v.store.save_config(&config)?;
    }
    v.config = config;
    dirstate.scanned = now;
    if !dry {
        // A scan that found the folder as the dirstate has it writes
        // nothing; the file keeps its older `scanned`, which is only more
        // cautious. `learned`: some row is the same but newly trusted.
        let same = dirstate.rows == v.worktree.dirstate.rows
            && dirstate.deleted == v.worktree.dirstate.deleted;
        if !same || learned {
            dirstate.save(&v.worktree.root)?;
        }
    } else if learned || !smudged.is_empty() {
        // `v.worktree.dirstate` is still the one the scan began with.
        let mut cache = std::mem::take(&mut v.worktree.dirstate);
        for (path, size, mtime) in refreshed {
            if let Some(row) = cache.rows.get_mut(&path) {
                (row.size, row.mtime) = (size, mtime);
            }
        }
        for path in smudged {
            if let Some(row) = cache.rows.get_mut(&path) {
                row.size = None;
            }
        }
        cache.scanned = now;
        out.cache = Some(cache);
    }
    v.worktree.dirstate = dirstate;
    Ok(out)
}

/// The heads of a path whose every head is a tombstone; None for any other
/// path. A file beats a tombstone, so a tombstone winner means all of them.
pub fn deleted_heads(log: &Log, path: &str) -> Option<Vec<Id>> {
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
    ctx: &Context,
    v: &Vault<impl Backend, Dir>,
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
                ctx,
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
                let (inner, outer) = (dir.display(), v.worktree.root.display());
                warn(
                    ctx,
                    format!(
                        "{inner} is a vault inside this one; its files are synced as part of {outer}"
                    ),
                );
            }
            continue;
        }
        if v.worktree.ignore.matches(&path) {
            continue;
        }
        if kind.is_dir() {
            walk(ctx, v, &item.path(), &path, dry, files, odd)?;
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

#[cfg(test)]
mod tests {
    use super::moves;

    #[test]
    fn moves_pair_by_unique_hash() {
        let side = |items: &[(&str, &str)]| -> Vec<(String, String)> {
            items
                .iter()
                .map(|(p, h)| (p.to_string(), h.to_string()))
                .collect()
        };
        let deleted = side(&[("a", "1"), ("b", "2"), ("c", "3"), ("d", "3")]);
        let added = side(&[("x", "1"), ("y", "2"), ("z", "2"), ("w", "3"), ("v", "9")]);
        // 1 pairs; 2 is added twice and 3 deleted twice, so neither pairs.
        assert_eq!(moves(&deleted, &added), vec![("a".into(), "x".into())]);
    }
}
