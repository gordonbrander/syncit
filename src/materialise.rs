//! Materialise: bring the working folder to the state the log describes.

use crate::blobs::{available, fetch, hash_file, read};
use crate::dirstate::{Row, TICK_MARGIN_MS, mtime_ms};
use crate::entry::{Entry, Id, sha256};
use crate::log::{Loser, PathState};
use crate::output::{Context, Progress, Summary};
use crate::scan::moves;
use crate::segment;
use crate::vault::{TMP_PREFIX, Vault, unique};
use anyhow::{Context as _, Result, bail, ensure};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::PermissionsExt;
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

/// Files whose conflicting edits are merged line by line. Only plain prose:
/// a line merge of structured text can be clean and still broken.
const MERGE_EXTENSIONS: [&str; 3] = ["markdown", "md", "txt"];

/// Larger versions are not merged: they are read into memory and diffed.
const MERGE_MAX_BYTES: u64 = 1 << 20;

fn mergeable(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(stem, ext)| {
        !stem.is_empty() && MERGE_EXTENSIONS.iter().any(|x| ext.eq_ignore_ascii_case(x))
    })
}

/// The clean three-way merge of a text file's two conflicting versions, and
/// the entry that records it: a child of both, so they stay in the log. None
/// unless there are exactly two heads, both files, with one common ancestor,
/// all three contents can be read, and the edits do not overlap. The winner
/// is "ours" and the mtime is fixed, so every vault writes the same entry.
fn merged(
    v: &Vault,
    peer: Option<&Vault>,
    p: &str,
    heads: &[Id],
    winner: &Id,
) -> Option<(String, Entry)> {
    let [a, b] = heads else { return None };
    if !mergeable(p) {
        return None;
    }
    let (w, l) = (&v.log.entries[winner], if a == winner { b } else { a });
    let l = &v.log.entries[l];
    if w.hash == l.hash {
        return None; // the same content twice is not a conflict
    }
    let base = &v.log.entries[&v.log.merge_base(p, a, b)?];
    let text =
        |e: &Entry| String::from_utf8(read(v, peer, e.hash.as_ref()?, MERGE_MAX_BYTES)?).ok();
    let text = diffy::merge(&text(base)?, &text(w)?, &text(l)?).ok()?;
    let entry = Entry {
        // The bit is merged as the text is: a change on either side is kept.
        exec: if w.exec == base.exec { l.exec } else { w.exec },
        hash: Some(sha256(text.as_bytes())),
        // Later than both, by enough that any filesystem shows the change.
        mtime: w.mtime.checked_add(TICK_MARGIN_MS)?,
        parents: heads.to_vec(),
        path: p.to_string(),
    };
    Some((text, entry))
}

/// Visit every path in the log. The peer is only a blob source. `kept` says
/// that the versions a merge replaces are retained in this run; without it
/// nothing is merged.
pub fn materialise(
    ctx: &Context,
    v: &mut Vault,
    peer: Option<&Vault>,
    kept: bool,
    sum: &mut Summary,
) -> Result<()> {
    if v.bare {
        return Ok(()); // no working folder
    }
    let states: Vec<(String, PathState)> = (v.log.paths.keys())
        .map(|p| (p.clone(), v.log.resolve(p)))
        .collect();
    let mut pass = Pass::default();
    for (p, state) in &states {
        if let PathState::Resolved { winner, .. } = state
            && let Some(h) = &v.log.entries[winner].hash
            && v.dirstate.rows.get(p).is_none_or(|row| &row.hash != h)
            && v.takes(p)
        {
            pass.needed.insert(h.clone());
        }
    }
    // A path that fails a safety check or hits a filesystem error is left as
    // it was, and the run goes on.
    let progress = Progress::new(&ctx.bar, "writing", states.len());
    for (path, state) in states {
        progress.tick();
        if let Err(e) = one(ctx, v, peer, kept, &path, state, &mut pass, sum) {
            v.event(ctx, sum, "skipped", &path, &format!("{e:#}"));
        }
    }
    drop(progress);
    for (path, heads) in std::mem::take(&mut pass.deferred) {
        if let Err(e) = delete(ctx, v, &path, heads, &mut pass) {
            v.event(ctx, sum, "skipped", &path, &format!("{e:#}"));
        }
    }
    // A deletion and an addition of the same content are one file moved.
    let pairs = moves(&pass.deleted, &pass.added);
    let paired: HashSet<&String> = pairs.iter().flat_map(|(a, b)| [a, b]).collect();
    let mut lines: Vec<(&String, &'static str, Option<&String>)> = vec![];
    lines.extend(pairs.iter().map(|(from, to)| (to, "renamed", Some(from))));
    for (kind, side) in [("deleted", &pass.deleted), ("added", &pass.added)] {
        let unpaired = side.iter().filter(|(p, _)| !paired.contains(p));
        lines.extend(unpaired.map(|(p, _)| (p, kind, None)));
    }
    lines.sort();
    for (path, kind, from) in lines {
        match from {
            Some(from) => v.renamed(ctx, sum, from, path),
            None => v.event(ctx, sum, kind, path, ""),
        }
    }
    if ctx.dry_run {
        return Ok(());
    }
    v.dirstate.save(&v.root)
}

/// What one materialise collects as it goes over the paths.
#[derive(Default)]
struct Pass {
    /// The content written at some path in this run.
    needed: HashSet<String>,
    /// Deletions put off to the end, with the heads for their records.
    deferred: Vec<(String, Vec<Id>)>,
    /// Files removed and files added, as `(path, hash)`, to be printed as
    /// moves where they pair.
    deleted: Vec<(String, String)>,
    added: Vec<(String, String)>,
}

/// Remove the file at `p`, whose winner is a tombstone.
fn delete(ctx: &Context, v: &mut Vault, p: &str, heads: Vec<Id>, pass: &mut Pass) -> Result<()> {
    if let Some(row) = v.dirstate.rows.get(p) {
        let hash = row.hash.clone();
        safe(v, p)?;
        if !ctx.dry_run {
            let file = v.root.join(p);
            fs::remove_file(&file)?;
            prune(&v.root, &file);
        }
        v.dirstate.rows.remove(p);
        pass.deleted.push((p.to_string(), hash));
    }
    // No file here now reflects every head: this vault has seen them.
    v.dirstate.deleted.insert(p.to_string(), heads);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn one(
    ctx: &Context,
    v: &mut Vault,
    peer: Option<&Vault>,
    kept: bool,
    p: &str,
    state: PathState,
    pass: &mut Pass,
    sum: &mut Summary,
) -> Result<()> {
    let (heads, winner, losers) = match state {
        PathState::Absent => return Ok(()),
        PathState::Incomplete => {
            v.event(ctx, sum, "incomplete", p, "waiting for missing history");
            return Ok(());
        }
        PathState::Resolved {
            heads,
            winner,
            losers,
            ..
        } => (heads, winner, losers),
    };
    let row = v.dirstate.rows.get(p).cloned();
    if !v.takes(p) {
        return Ok(());
    }
    let (file, w) = (v.root.join(p), v.log.entries[&winner].clone());
    let Some(h) = w.hash else {
        // A file whose content another path is about to take is removed last:
        // until then it is a source for it, and nothing is fetched.
        // ponytail: a file moved to below its own path (`x` to `x/y`) then
        // blocks the write, which is skipped until the next run.
        if row.is_some_and(|row| pass.needed.contains(&row.hash)) {
            pass.deferred.push((p.to_string(), heads));
            return Ok(());
        }
        return delete(ctx, v, p, heads, pass);
    };
    let merge = kept.then(|| merged(v, peer, p, &heads, &winner)).flatten();
    if let Some((text, entry)) = merge {
        safe(v, p)?;
        let hash = sha256(text.as_bytes());
        let (size, mtime) = if ctx.dry_run {
            (0, entry.mtime) // in memory only
        } else {
            let fill = |tmp: &Path| fs::write(tmp, &text).map(|_| true);
            let tmp = stage(v, p, entry.mtime, entry.exec, fill)?.context("nothing staged")?;
            let placed = place(v, &tmp, p, &hash)?;
            // The file, then its entry: an entry is never published for
            // content that is nowhere. Stopped in between, the next scan
            // records the file as an ordinary edit.
            let (name, bytes) = segment::encode(&v.config.nickname, std::slice::from_ref(&entry));
            v.storage.write_segment(&name, &bytes)?;
            placed
        };
        v.dirstate.deleted.remove(p);
        v.dirstate.rows.insert(
            p.to_string(),
            Row {
                size,
                mtime,
                exec: entry.exec,
                hash,
                base: vec![entry.id()],
            },
        );
        let nickname = v.config.nickname.clone();
        v.log.add(entry, &nickname);
        let kind = if row.is_some() { "updated" } else { "added" };
        v.event(ctx, sum, kind, p, "merged");
        return Ok(());
    }
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
        let (size, mtime) = if ctx.dry_run {
            if !available(v, peer, &h) {
                v.event(ctx, sum, "pending", p, UNAVAILABLE);
                return Ok(());
            }
            (0, w.mtime) // in memory only
        } else {
            let Some(tmp) = stage(v, p, w.mtime, w.exec, |tmp| fetch(v, peer, &h, tmp))? else {
                v.event(ctx, sum, "pending", p, UNAVAILABLE);
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
        v.dirstate.deleted.remove(p);
        v.dirstate.rows.insert(
            p.to_string(),
            Row {
                size,
                mtime,
                exec: w.exec,
                hash: h.clone(),
                base: vec![],
            },
        );
        if row.is_some() {
            v.event(ctx, sum, "updated", p, "");
        } else {
            pass.added.push((p.to_string(), h.clone()));
        }
        renamed = copy;
    } else if row.is_some_and(|row| row.exec != w.exec) {
        // Same content, other executable bit: nothing to fetch.
        safe(v, p)?;
        if !ctx.dry_run {
            set_exec(&file, w.exec)?;
        }
        if let Some(row) = v.dirstate.rows.get_mut(p) {
            row.exec = w.exec;
        }
        v.event(ctx, sum, "updated", p, "");
    }
    copies_and_base(ctx, v, peer, p, &h, heads, &losers, renamed, sum);
    Ok(())
}

/// The winner's content is the file at `p`: write the conflict copies and
/// set the row's base to the heads now visible in this vault.
#[allow(clippy::too_many_arguments)]
fn copies_and_base(
    ctx: &Context,
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
                v.event(ctx, sum, "skipped", &c, "the conflict copy name is taken");
            }
            same
        } else {
            match write_copy(ctx, v, peer, &c, l) {
                Ok(true) => v.event(ctx, sum, "copied", &c, ""),
                Ok(false) => v.event(ctx, sum, "pending", &c, UNAVAILABLE),
                Err(e) => v.event(ctx, sum, "skipped", &c, &format!("{e:#}")),
            }
            ctx.dry_run || file.symlink_metadata().is_ok()
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
    if let Some(row) = v.dirstate.rows.get_mut(p) {
        row.base = base;
    }
}

/// A conflict copy is an ordinary file with no dirstate row: the next scan
/// records it.
fn write_copy(
    ctx: &Context,
    v: &mut Vault,
    peer: Option<&Vault>,
    c: &str,
    l: &Loser,
) -> Result<bool> {
    ensure!(!v.skips(c), "the conflict copy name is ignored");
    no_symlinked_parent(v, c)?;
    if ctx.dry_run {
        return Ok(available(v, peer, &l.hash));
    }
    let Some(tmp) = stage(v, c, l.mtime, l.exec, |tmp| fetch(v, peer, &l.hash, tmp))? else {
        return Ok(false);
    };
    place(v, &tmp, c, &l.hash)?;
    Ok(true)
}

/// Segments come from other vaults and must not write outside this one.
pub fn no_symlinked_parent(v: &Vault, p: &str) -> Result<()> {
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
    match v.dirstate.rows.get(p) {
        Some(row) => ensure!(
            v.dirstate.matches(row, &file),
            "changed since it was scanned"
        ),
        None => ensure!(
            file.symlink_metadata().is_err(),
            "an untracked file is in the way"
        ),
    }
    Ok(())
}

/// Give or take the executable bit as `chmod +x` and `chmod -x` do: execute
/// goes wherever read is allowed, and always to the owner.
fn set_exec(file: &Path, exec: bool) -> io::Result<()> {
    let mut perms = file.symlink_metadata()?.permissions();
    let mode = perms.mode();
    perms.set_mode(if exec {
        mode | 0o100 | (mode & 0o444) >> 2
    } else {
        mode & !0o111
    });
    fs::set_permissions(file, perms)
}

/// Fill a scratch file beside `p` with content, usually a verified blob, and
/// set its mtime and executable bit. None if `fill` says there is no content
/// (no source has the blob); then nothing is left behind.
fn stage(
    v: &Vault,
    p: &str,
    mtime: i64,
    exec: bool,
    fill: impl FnOnce(&Path) -> io::Result<bool>,
) -> Result<Option<PathBuf>> {
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
        if !fill(&tmp)? {
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
        if exec {
            set_exec(&tmp, true)?; // a new file is not executable
        }
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

/// Write content `h` at `p`, dated now, for `restore`, executable or not. A file already there
/// must be unchanged since it was scanned, as for any write; a row whose
/// file is gone is no obstacle. False if no source has the content.
pub fn write_now(v: &mut Vault, p: &str, h: &str, exec: bool) -> Result<bool> {
    if v.root.join(p).symlink_metadata().is_ok() {
        safe(v, p)?;
    } else {
        no_symlinked_parent(v, p)?;
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis() as i64;
    let Some(tmp) = stage(v, p, now, exec, |tmp| fetch(v, None, h, tmp))? else {
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
    use super::{copy_name, mergeable};

    #[test]
    fn mergeable_names() {
        for yes in ["notes.md", "a/b/Read.Me.TXT", "x.markdown", ".hidden.md"] {
            assert!(mergeable(yes), "{yes}");
        }
        for no in [
            "data.json",
            "Makefile",
            ".md",
            "md",
            "a.md/photo.jpg",
            "x.mdx",
        ] {
            assert!(!mergeable(no), "{no}");
        }
    }

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
