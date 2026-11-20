//! Materialise: bring the working folder to the state the log describes.

use crate::blobs::{available, fetch, hash_file};
use crate::cache::{Row, mtime_ms};
use crate::entry::Id;
use crate::log::{Loser, PathState};
use crate::output::{Options, Summary};
use crate::vault::{TMP_PREFIX, Vault, unique};
use anyhow::{Context, Result, bail, ensure};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UNAVAILABLE: &str = "content not available from any source";

/// The name a losing version is saved under: `-` and the first 8 hex
/// characters of its hash go before the last `.` that is not the first character.
pub fn copy_name(path: &str, hash: &str) -> String {
    let (dir, name) = path.split_at(path.rfind('/').map_or(0, |i| i + 1));
    let short = &hash[..8];
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{dir}{}-{short}{}", &name[..dot], &name[dot..]),
        _ => format!("{dir}{name}-{short}"),
    }
}

/// Visit every path in the log. The peer is only a blob source.
pub fn materialise(
    opts: &Options,
    v: &mut Vault,
    peer: Option<&Vault>,
    sum: &mut Summary,
) -> Result<()> {
    let paths: Vec<String> = v.log.paths.keys().cloned().collect();
    for path in paths {
        // A path that fails a safety check or hits a filesystem error is
        // left as it was, and the run goes on.
        if let Err(e) = one(opts, v, peer, &path, sum) {
            v.event(opts, sum, "skipped", &path, &format!("{e:#}"));
        }
    }
    if opts.dry_run {
        return Ok(());
    }
    v.cache.save(&v.root)
}

fn one(
    opts: &Options,
    v: &mut Vault,
    peer: Option<&Vault>,
    p: &str,
    sum: &mut Summary,
) -> Result<()> {
    let (heads, winner, losers) = match v.log.resolve(p) {
        PathState::Absent => return Ok(()),
        PathState::Incomplete => {
            v.event(opts, sum, "incomplete", p, "waiting for missing history");
            return Ok(());
        }
        PathState::Resolved {
            heads,
            winner,
            losers,
            ..
        } => (heads, winner, losers),
    };
    let row = v.cache.rows.get(p).cloned();
    if v.declines(p) {
        return Ok(());
    }
    let (file, w) = (v.root.join(p), v.log.entries[&winner].clone());
    let Some(h) = w.hash else {
        if row.is_some() {
            safe(v, p)?;
            if !opts.dry_run {
                fs::remove_file(&file)?;
                prune(&v.root, &file);
            }
            v.cache.rows.remove(p);
            v.event(opts, sum, "deleted", p, "");
        }
        // No file here now reflects every head: this vault has seen them.
        v.cache.deleted.insert(p.to_string(), heads);
        return Ok(());
    };
    // The copy name the local version was renamed to, if it was.
    let mut renamed = None;
    if row.as_ref().is_none_or(|row| row.hash != h) {
        safe(v, p)?;
        // A local version that lost a conflict and has not been surfaced
        // stays visible: it is renamed to its copy name, not overwritten.
        let copy = row.as_ref().map(|row| copy_name(p, &row.hash)).filter(|c| {
            losers
                .iter()
                .any(|l| Some(&l.hash) == row.as_ref().map(|r| &r.hash))
                && !v.log.paths.contains_key(c)
        });
        if let Some(c) = &copy {
            ensure!(
                v.root.join(c).symlink_metadata().is_err(),
                "the conflict copy name {c} is taken"
            );
        }
        let (size, mtime) = if opts.dry_run {
            if !available(v, peer, &h) {
                v.event(opts, sum, "pending", p, UNAVAILABLE);
                return Ok(());
            }
            (0, w.mtime) // in memory only
        } else {
            let Some(tmp) = stage(v, peer, p, &h, w.mtime)? else {
                v.event(opts, sum, "pending", p, UNAVAILABLE);
                return Ok(());
            };
            if let (Some(c), Some(row)) = (&copy, &row) {
                if let Err(e) = fs::rename(&file, v.root.join(c)) {
                    let _ = fs::remove_file(&tmp);
                    return Err(e.into());
                }
                v.hint(&row.hash, v.root.join(c));
            }
            place(v, &tmp, p, &h)?
        };
        v.cache.deleted.remove(p);
        v.cache.rows.insert(
            p.to_string(),
            Row {
                size,
                mtime,
                hash: h.clone(),
                base: vec![],
            },
        );
        v.event(
            opts,
            sum,
            if row.is_some() { "updated" } else { "added" },
            p,
            "",
        );
        renamed = copy;
    }
    copies_and_base(opts, v, peer, p, &h, heads, &losers, renamed, sum);
    Ok(())
}

/// The winner's content is the file at `p`: write the conflict copies and
/// set the row's base to the heads now visible in this vault.
#[allow(clippy::too_many_arguments)]
fn copies_and_base(
    opts: &Options,
    v: &mut Vault,
    peer: Option<&Vault>,
    p: &str,
    h: &str,
    heads: Vec<Id>,
    losers: &[Loser],
    renamed: Option<String>,
    sum: &mut Summary,
) {
    let mut visible = vec![h];
    for l in losers {
        let c = copy_name(p, &l.hash);
        let file = v.root.join(&c);
        let surfaced = if v.log.paths.contains_key(&c) || renamed.as_ref() == Some(&c) {
            true // surfaced before; a copy the user deleted stays deleted
        } else if file.symlink_metadata().is_ok() {
            let same = hash_file(&file).is_ok_and(|x| x == l.hash);
            if !same {
                v.event(opts, sum, "skipped", &c, "the conflict copy name is taken");
            }
            same
        } else {
            match write_copy(opts, v, peer, &c, l) {
                Ok(true) => v.event(opts, sum, "copied", &c, ""),
                Ok(false) => v.event(opts, sum, "pending", &c, UNAVAILABLE),
                Err(e) => v.event(opts, sum, "skipped", &c, &format!("{e:#}")),
            }
            opts.dry_run || file.symlink_metadata().is_ok()
        };
        if surfaced {
            visible.push(&l.hash);
        }
    }
    let shown = |id: &Id| {
        v.log.entries[id]
            .hash
            .as_deref()
            .is_none_or(|x| visible.contains(&x))
    };
    let base: Vec<Id> = heads.into_iter().filter(shown).collect();
    if let Some(row) = v.cache.rows.get_mut(p) {
        row.base = base;
    }
}

/// A conflict copy is an ordinary file with no cache row: the next scan
/// records it.
fn write_copy(
    opts: &Options,
    v: &mut Vault,
    peer: Option<&Vault>,
    c: &str,
    l: &Loser,
) -> Result<bool> {
    ensure!(!v.skips(c), "the conflict copy name is ignored");
    no_symlinked_parent(v, c)?;
    if opts.dry_run {
        return Ok(available(v, peer, &l.hash));
    }
    let Some(tmp) = stage(v, peer, c, &l.hash, l.mtime)? else {
        return Ok(false);
    };
    place(v, &tmp, c, &l.hash)?;
    Ok(true)
}

/// Segments come from other vaults and must not write outside this one.
fn no_symlinked_parent(v: &Vault, p: &str) -> Result<()> {
    let mut dir = v.root.join(p);
    while dir.pop() && dir != v.root {
        if dir.symlink_metadata().is_ok_and(|m| m.is_symlink()) {
            bail!("a parent folder is a symbolic link");
        }
    }
    Ok(())
}

/// The safety checks before a write, rename or delete at `p`.
fn safe(v: &Vault, p: &str) -> Result<()> {
    no_symlinked_parent(v, p)?;
    let file = v.root.join(p);
    match v.cache.rows.get(p) {
        Some(row) => ensure!(v.cache.matches(row, &file), "changed since it was scanned"),
        None => ensure!(
            file.symlink_metadata().is_err(),
            "an untracked file is in the way"
        ),
    }
    Ok(())
}

/// Fetch content `h` into a scratch file beside `p`, verified and with its
/// mtime set. None if no source has the blob; then nothing is left behind.
fn stage(v: &Vault, peer: Option<&Vault>, p: &str, h: &str, mtime: i64) -> Result<Option<PathBuf>> {
    let file = v.root.join(p);
    let dir = file.parent().context("no parent folder")?;
    let (mut top, mut d) = (None, dir); // the topmost folder this call creates
    while !d.exists() {
        top = Some(d);
        d = d.parent().context("no parent folder")?;
    }
    let tmp = dir.join(format!("{TMP_PREFIX}{}", unique()));
    let staged = (|| -> io::Result<bool> {
        fs::create_dir_all(dir)?;
        if !fetch(v, peer, h, &tmp)? {
            return Ok(false);
        }
        let span = Duration::from_millis(mtime.unsigned_abs());
        let time = if mtime < 0 {
            UNIX_EPOCH.checked_sub(span)
        } else {
            UNIX_EPOCH.checked_add(span)
        };
        let time = time.ok_or_else(|| io::Error::other("mtime out of range"))?;
        File::options().write(true).open(&tmp)?.set_modified(time)?;
        Ok(true)
    })();
    if !matches!(staged, Ok(true)) {
        let _ = fs::remove_file(&tmp);
        let mut d = dir;
        while let Some(top) = top {
            let _ = fs::remove_dir(d);
            match d.parent() {
                Some(up) if d != top => d = up,
                _ => break,
            }
        }
    }
    Ok(staged?.then_some(tmp))
}

/// Rename a staged file onto `p`. Returns its size and mtime as stat reports.
fn place(v: &mut Vault, tmp: &Path, p: &str, h: &str) -> Result<(u64, i64)> {
    let file = v.root.join(p);
    if let Err(e) = fs::rename(tmp, &file) {
        let _ = fs::remove_file(tmp);
        return Err(e.into());
    }
    let meta = file.symlink_metadata()?;
    v.hint(h, file);
    Ok((meta.len(), mtime_ms(&meta)))
}

/// Write content `h` at `p`, dated now, for `restore`. A file already there
/// must be unchanged since it was scanned, as for any write; a row whose
/// file is gone is no obstacle. False if no source has the content.
pub fn write_now(v: &mut Vault, p: &str, h: &str) -> Result<bool> {
    if v.root.join(p).symlink_metadata().is_ok() {
        safe(v, p)?;
    } else {
        no_symlinked_parent(v, p)?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let Some(tmp) = stage(v, None, p, h, now)? else {
        return Ok(false);
    };
    place(v, &tmp, p, h)?;
    Ok(true)
}

/// Remove the folders a deletion left empty, up to the vault root.
pub fn prune(root: &Path, file: &Path) {
    let mut dir = file.parent();
    while let Some(d) = dir
        && d != root
        && fs::remove_dir(d).is_ok()
    {
        dir = d.parent();
    }
}

#[cfg(test)]
mod tests {
    use super::copy_name;

    #[test]
    fn copy_name_cases() {
        let hash = "3f9a1c2e".repeat(8);
        assert_eq!(copy_name("a/notes.txt", &hash), "a/notes-3f9a1c2e.txt");
        assert_eq!(copy_name("Makefile", &hash), "Makefile-3f9a1c2e");
        assert_eq!(copy_name(".bashrc", &hash), ".bashrc-3f9a1c2e");
        assert_eq!(copy_name("x.tar.gz", &hash), "x.tar-3f9a1c2e.gz");
        assert_eq!(copy_name("a.b/c", &hash), "a.b/c-3f9a1c2e");
        assert_eq!(copy_name("a/.env.local", &hash), "a/.env-3f9a1c2e.local");
    }
}
