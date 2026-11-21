//! One function per command. Each prints as it goes; a command that runs a
//! sync returns a `Summary` for `main` to print last.

use crate::blobs::{available, hash_file, retain};
use crate::config::{Config, default_nickname, prefix_matches};
use crate::entry::valid_path;
use crate::log::PathState;
use crate::materialise::{copy_name, materialise, prune, write_now};
use crate::output::{Event, Holding, Message, Options, Summary, Version, print_stdout, warn};
use crate::scan::scan;
use crate::segment::{self, valid_nickname};
use crate::vault::{Vault, find_root, relative, resolve_path, write_atomic};
use anyhow::{Context, Result, bail, ensure};
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Sync,
    Push,
    Pull,
}

/// Make `dir` a vault that includes `include`, nicknamed after the folder.
fn create(dir: &Path, include: String) -> Result<Config> {
    let folder = resolve_path(dir)?;
    let folder = folder.file_name().unwrap_or_default().to_string_lossy();
    let config = Config::new(include, default_nickname(&folder));
    for sub in ["log", "blobs", "tmp"] {
        fs::create_dir_all(dir.join(".syncit").join(sub))?;
    }
    File::create(dir.join(".syncit/lock"))?;
    config.save(dir)?;
    Ok(config)
}

pub fn init(opts: &Options, dir: &Path) -> Result<()> {
    ensure!(
        !dir.join(".syncit").exists(),
        "{} is already a vault",
        dir.display()
    );
    let config = create(dir, String::new())?;
    let message = format!(
        "Vault initialized: {}\nNickname: {}",
        resolve_path(dir)?.display(),
        config.nickname
    );
    print_stdout(&Message::Info { message }, opts.json);
    Ok(())
}

pub fn clone(opts: &Options, src: &Path, dir: &Path) -> Result<Summary> {
    let src = resolve_path(src)?;
    let root = find_root(&src)?;
    let sub = relative(&root, &src)?;
    ensure!(
        !resolve_path(dir)?.starts_with(&root),
        "cannot clone a vault into itself"
    );
    let empty = !dir.exists() || dir.read_dir()?.next().is_none();
    ensure!(empty, "{} exists and is not empty", dir.display());
    create(dir, sub)?;
    run(opts, dir, &root, Mode::Pull)
}

pub fn sync(opts: &Options, cwd: &Path, peer: &Path) -> Result<Summary> {
    run(opts, cwd, peer, Mode::Sync)
}

pub fn push(opts: &Options, cwd: &Path, peer: &Path) -> Result<Summary> {
    run(opts, cwd, peer, Mode::Push)
}

pub fn pull(opts: &Options, cwd: &Path, peer: &Path) -> Result<Summary> {
    run(opts, cwd, peer, Mode::Pull)
}

/// Whether the folders are the same or one contains the other. Such vaults
/// share files: a run between them would copy the outer into the inner,
/// one level deeper each time.
fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Open another vault for a run with `v`.
fn other(opts: &Options, v: &mut Vault, path: &Path) -> Result<Vault> {
    ensure!(
        !nested(&find_root(path)?, &v.root),
        "cannot sync a vault with itself or with a vault inside it"
    );
    let mut peer = Vault::open(opts, path, false)?;
    (v.side, peer.side) = ("->", "<-");
    Ok(peer)
}

/// A run with a peer (SPEC.md section 10). `a` is the local vault.
fn run(opts: &Options, cwd: &Path, peer: &Path, mode: Mode) -> Result<Summary> {
    let mut a = Vault::open(opts, cwd, true)?;
    let mut b = other(opts, &mut a, peer)?;
    let mut sum = Summary::default();
    announce(opts);
    record(opts, &mut a, Some(&b), &mut sum)?;
    record(opts, &mut b, Some(&a), &mut sum)?;
    exchange(opts, &mut a, &mut b, mode, &mut sum)?;
    Ok(sum)
}

/// Say that this is a dry run, if it is.
fn announce(opts: &Options) {
    if opts.dry_run {
        let message = "dry run: nothing is changed".to_string();
        print_stdout(&Message::Info { message }, opts.json);
    }
}

/// The files `from` holds that `to` excludes.
pub fn excluded(from: &Vault, to: &Vault) -> Vec<String> {
    let excluded = |p: &&String| !to.config.includes(p) && !to.skips(p);
    from.cache.rows.keys().filter(excluded).cloned().collect()
}

/// The files `from` holds that `to` ignores.
pub fn ignored(from: &Vault, to: &Vault) -> Vec<String> {
    let ignored = |p: &&String| to.skips(p);
    from.cache.rows.keys().filter(ignored).cloned().collect()
}

/// The part of a run after both vaults are scanned.
fn exchange(
    opts: &Options,
    a: &mut Vault,
    b: &mut Vault,
    mode: Mode,
    sum: &mut Summary,
) -> Result<()> {
    let (to_b, to_a) = (mode != Mode::Pull, mode != Mode::Push);
    // A push says "put my files there": say so when some are not taken.
    let untaken = [("excludes", excluded(a, b)), ("ignores", ignored(a, b))];
    for (verb, files) in untaken {
        if mode != Mode::Push || files.is_empty() {
            continue;
        }
        let top = |p: &String| match p.split_once('/') {
            Some((dir, _)) => format!("{dir}/"),
            None => p.clone(),
        };
        let tops: BTreeSet<String> = files.iter().map(top).collect();
        let mut names: Vec<String> = tops.iter().take(5).cloned().collect();
        if tops.len() > 5 {
            names.push(format!("and {} more", tops.len() - 5));
        }
        warn(
            opts,
            format!(
                "{} {verb} {} files ({}); they were not copied",
                b.root.display(),
                files.len(),
                names.join(", ")
            ),
        );
    }
    if to_b {
        receive(opts, b, a, sum)?;
    }
    if to_a {
        receive(opts, a, b, sum)?;
    }
    // Both retain before either materialises, so that a version is saved
    // before it is replaced.
    if to_b {
        retain(opts, b, Some(a))?;
    }
    if to_a {
        retain(opts, a, Some(b))?;
    }
    if to_b {
        materialise(opts, b, Some(a), sum)?;
    }
    if to_a {
        materialise(opts, a, Some(b), sum)?;
    }
    // A conflict open in both vaults is reported once, for the local one.
    let mut seen = BTreeSet::new();
    if to_a {
        conflicts(opts, a, &mut seen);
    }
    if to_b {
        conflicts(opts, b, &mut seen);
    }
    sum.conflicts += seen.len();
    Ok(())
}

/// Scan, then retain blobs for what the scan recorded.
fn record(opts: &Options, v: &mut Vault, peer: Option<&Vault>, sum: &mut Summary) -> Result<()> {
    let scanned = scan(opts, v, opts.dry_run)?;
    sum.recorded += scanned.entries.len();
    for path in &scanned.skipped {
        v.event(
            opts,
            sum,
            "skipped",
            path,
            "changed while it was being read",
        );
    }
    retain(opts, v, peer)
}

/// Take every segment the other vault has and this one lacks. A segment is
/// validated before it is stored; one that fails is reported and left out.
pub fn receive(opts: &Options, v: &mut Vault, from: &Vault, sum: &mut Summary) -> Result<()> {
    let dir = v.dot().join("log");
    for file in fs::read_dir(from.dot().join("log"))? {
        let file = file?;
        let name = file.file_name().to_string_lossy().into_owned();
        if dir.join(&name).exists() {
            continue;
        }
        let read = fs::read(file.path()).map_err(anyhow::Error::from);
        match read.and_then(|bytes| Ok((segment::decode(&name, &bytes)?, bytes))) {
            Ok((segment, bytes)) => {
                if !opts.dry_run {
                    write_atomic(&v.root, &dir.join(&name), &bytes)?;
                }
                for e in segment.entries {
                    v.log.add(e, &segment.nickname);
                }
            }
            Err(e) => v.event(opts, sum, "rejected", "", &format!("segment {name}: {e:#}")),
        }
    }
    // What a dry scan of `from` recorded is in its memory, not in a segment.
    if opts.dry_run {
        absorb(v, from);
    }
    Ok(())
}

/// Add every entry of `from`'s log to `v`'s, in memory only.
fn absorb(v: &mut Vault, from: &Vault) {
    for (id, e) in &from.log.entries {
        for nickname in &from.log.authors[id] {
            v.log.add(e.clone(), nickname);
        }
    }
}

/// Print each open conflict among the paths this vault includes that
/// is not in `seen`, and add it there.
fn conflicts(opts: &Options, v: &Vault, seen: &mut BTreeSet<String>) {
    for path in v.log.paths.keys() {
        let PathState::Resolved {
            losers,
            lost_delete,
            ..
        } = v.log.resolve(path)
        else {
            continue;
        };
        if (losers.is_empty() && !lost_delete)
            || !v.config.includes(path)
            || !seen.insert(path.clone())
        {
            continue;
        }
        let mut what: Vec<String> = losers.iter().map(|l| copy_name(path, &l.hash)).collect();
        if lost_delete {
            what.push("deleted in another vault".to_string());
        }
        let conflict = Message::Conflict {
            vault: v.root.display().to_string(),
            path: path.clone(),
            message: what.join(", "),
            side: v.side,
        };
        print_stdout(&conflict, opts.json);
    }
}

pub fn snapshot(opts: &Options, cwd: &Path) -> Result<Summary> {
    let mut v = Vault::open(opts, cwd, true)?;
    let mut sum = Summary::default();
    record(opts, &mut v, None, &mut sum)?;
    Ok(sum)
}

/// What a scan would record and what a sync would do. Writes nothing.
/// Returns what it printed, as (kind, path).
pub fn status(opts: &Options, cwd: &Path) -> Result<Vec<(&'static str, String)>> {
    let mut v = Vault::open(opts, cwd, true)?;
    let scanned = scan(opts, &mut v, true)?;
    let mut found: Vec<(&'static str, String)> = vec![];
    for (path, kind) in &scanned.changes {
        found.push((kind, path.clone()));
    }
    let changed: BTreeSet<&String> = scanned.changes.iter().map(|(path, _)| path).collect();
    for path in v.log.paths.keys() {
        let row = v.cache.rows.get(path);
        match v.log.resolve(path) {
            PathState::Absent => {}
            PathState::Incomplete => found.push(("incomplete", path.clone())),
            PathState::Resolved { winner, .. } => {
                let on_disk = row.map(|row| &row.hash);
                let concerns = v.takes(path) && !changed.contains(path);
                if concerns && v.log.entries[&winner].hash.as_ref() != on_disk {
                    found.push(("behind", path.clone()));
                }
            }
        }
    }
    for (kind, path) in &found {
        v.event(opts, &mut Summary::default(), kind, path, "");
    }
    let mut seen = BTreeSet::new();
    conflicts(opts, &v, &mut seen);
    found.extend(seen.into_iter().map(|path| ("conflict", path)));
    Ok(found)
}

/// A vault's state for a path whose current version has this hash, or is a
/// deletion.
fn holding(v: &Vault, path: &str, hash: Option<&String>) -> &'static str {
    match v.cache.rows.get(path) {
        _ if v.skips(path) => "ignored",
        Some(row) if Some(&row.hash) == hash => "held",
        Some(_) => "behind",
        None if !v.config.includes(path) => "excluded",
        None if hash.is_none() => "deleted",
        None => "missing",
    }
}

/// The lines of `ls` for the files under the folder `dir`, which are sorted
/// by path. A folder below the top whose files, more than one, are all in the
/// same state is one line; any other is listed child by child.
fn collapse(dir: &str, files: &[Holding], top: bool, out: &mut Vec<Holding>) {
    let state = |f: &Holding| (f.ours, f.theirs, f.deleted);
    let same = files.iter().all(|f| state(f) == state(&files[0]));
    if !top && same && files.len() > 1 {
        out.push(Holding {
            path: format!("{dir}/"),
            files: files.len(),
            ..files[0].clone()
        });
        return;
    }
    // A child is the next name in the path, with its `/` if it is a folder.
    let skip = if dir.is_empty() { 0 } else { dir.len() + 1 };
    let child = |f: &Holding| {
        let rest = &f.path[skip..];
        rest.find('/').map_or(rest.len(), |i| i + 1) + skip
    };
    for group in files.chunk_by(|a, b| a.path[..child(a)] == b.path[..child(b)]) {
        let name = &group[0].path[..child(&group[0])];
        match name.strip_suffix('/') {
            Some(folder) => collapse(folder, group, false, out),
            None => out.extend(group.iter().cloned()),
        }
    }
}

/// What this vault holds of every file the log knows under `paths`, and with
/// `peer` what that vault holds too. Folders in one state are one line. Writes nothing.
/// Returns what it printed.
pub fn ls(
    opts: &Options,
    cwd: &Path,
    paths: &[PathBuf],
    peer: Option<&Path>,
) -> Result<Vec<Holding>> {
    let opts = &Options {
        json: opts.json,
        dry_run: true,
    };
    let mut v = Vault::open(opts, cwd, true)?;
    let mut scopes = prefixes(&v.root, cwd, paths)
        .context("paths are in this vault; to compare with another, add `--vs <vault>`")?;
    if scopes.is_empty() {
        scopes.push(String::new());
    }
    let mut peer = peer.map(|path| other(opts, &mut v, path)).transpose()?;
    // Dry scans, so that changes no scan has recorded count; then one log
    // for both vaults, so that each is compared with the same winner.
    scan(opts, &mut v, true)?;
    if let Some(peer) = &mut peer {
        scan(opts, peer, true)?;
        absorb(&mut v, peer);
    }
    let mut files: Vec<Holding> = vec![];
    for path in v.log.paths.keys() {
        let PathState::Resolved { winner, .. } = v.log.resolve(path) else {
            continue;
        };
        let hash = v.log.entries[&winner].hash.as_ref();
        let ours = holding(&v, path, hash);
        let theirs = peer.as_ref().map(|peer| holding(peer, path, hash));
        // A deleted path is listed only while a vault still has the file.
        if hash.is_none() && ours != "behind" && theirs != Some("behind") {
            continue;
        }
        files.push(Holding {
            path: path.clone(),
            files: 1,
            ours,
            theirs,
            deleted: hash.is_none(),
        });
    }
    let mut found = vec![];
    for prefix in &scopes {
        // The prefix itself, if it is a file; then what lies under it.
        let (file, under): (Vec<Holding>, Vec<Holding>) = files
            .iter()
            .filter(|f| prefix_matches(prefix, &f.path))
            .cloned()
            .partition(|f| f.path == *prefix);
        found.extend(file);
        if !under.is_empty() {
            collapse(prefix, &under, true, &mut found);
        }
    }
    for line in &found {
        print_stdout(&Message::Holding(line.clone()), opts.json);
    }
    Ok(found)
}

/// Command-line paths, relative to `cwd`, as prefixes relative to `root`.
fn prefixes(root: &Path, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<String>> {
    let prefix = |path: &PathBuf| {
        let prefix = relative(root, &resolve_path(&cwd.join(path))?)?;
        ensure!(
            prefix.is_empty() || valid_path(&prefix),
            "{} is not a path a vault can hold",
            path.display()
        );
        Ok(prefix)
    };
    paths.iter().map(prefix).collect()
}

/// Start holding paths of another vault. Each path lies inside that vault,
/// whose root is found by walking up, as `clone` does. The paths become
/// included here, and a pull from that vault fetches them.
pub fn get(opts: &Options, cwd: &Path, paths: &[PathBuf]) -> Result<Summary> {
    let first = paths.first().context("no path given")?;
    let source = find_root(&cwd.join(first))?;
    for path in paths {
        ensure!(
            find_root(&cwd.join(path))? == source,
            "{} and {} are in different vaults",
            first.display(),
            path.display()
        );
    }
    {
        let mut v = Vault::open(opts, cwd, true)?;
        ensure!(
            !nested(&source, &v.root),
            "{} is in this vault: give a path in the vault to get it from",
            first.display()
        );
        for prefix in prefixes(&source, cwd, paths)? {
            v.config.exclude.retain(|x| !prefix_matches(&prefix, x));
            v.config.include.retain(|x| !prefix_matches(&prefix, x));
            v.config.include.push(prefix);
        }
        v.config.save(&v.root)?;
    }
    run(opts, cwd, &source, Mode::Pull)
}

/// Move files to another vault: push to it, confirm each file arrived, then
/// stop holding them here. Nothing is removed unless every file is confirmed.
pub fn move_to(opts: &Options, cwd: &Path, paths: &[PathBuf], target: &Path) -> Result<Summary> {
    let mut v = Vault::open(opts, cwd, true)?;
    let prefixes = prefixes(&v.root, cwd, paths)?;
    let mut t = other(opts, &mut v, target)?;
    let mut sum = Summary::default();
    announce(opts);
    record(opts, &mut v, Some(&t), &mut sum)?;
    record(opts, &mut t, Some(&v), &mut sum)?;

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let held: Vec<String> = v.cache.rows.keys().filter(under).cloned().collect();
    ensure!(
        !held.is_empty(),
        "nothing to move: no files here under the given paths"
    );
    let paths = || held.iter().map(String::as_str);
    let ignored: Vec<&str> = paths().filter(|p| t.skips(p)).collect();
    let excluded: Vec<&str> = paths().filter(|p| !t.config.includes(p)).collect();
    for (verb, refused) in [("ignores", ignored), ("excludes", excluded)] {
        ensure!(
            refused.is_empty(),
            "nothing moved: {} {verb} {}",
            t.root.display(),
            refused.join(", ")
        );
    }

    exchange(opts, &mut v, &mut t, Mode::Push, &mut sum)?;
    let arrived =
        |p: &&String| t.cache.rows.get(*p).map(|row| &row.hash) == Some(&v.cache.rows[*p].hash);
    let target = t.root.display().to_string();
    let missing: Vec<String> = held
        .iter()
        .filter(|p| !arrived(p))
        .map(|p| {
            let said = |e: &&Event| e.vault == target && &e.path == p;
            match sum.events.iter().find(said) {
                Some(e) => format!("{p} ({}: {})", e.kind, e.note),
                None => format!("{p} (it holds a different version; sync first)"),
            }
        })
        .collect();
    ensure!(
        missing.is_empty(),
        "nothing moved: {target} does not hold {}",
        missing.join(", ")
    );

    for prefix in prefixes {
        v.config.include.retain(|x| !prefix_matches(&prefix, x));
        v.config.exclude.retain(|x| !prefix_matches(&prefix, x));
        v.config.exclude.push(prefix);
    }
    if !opts.dry_run {
        v.config.save(&v.root)?;
    }
    // Rows go before files: a file with no row is held again after the next
    // scan, but a row with no file would be recorded as a deletion.
    let rows: Vec<_> = held
        .into_iter()
        .filter_map(|path| v.cache.rows.remove_entry(&path))
        .collect();
    if !opts.dry_run {
        v.cache.save(&v.root)?;
    }
    for (path, row) in rows {
        let file = v.root.join(&path);
        if !v.cache.matches(&row, &file) {
            v.event(
                opts,
                &mut sum,
                "skipped",
                &path,
                "changed since it was scanned",
            );
            continue;
        }
        if !opts.dry_run {
            fs::remove_file(&file)?;
            prune(&v.root, &file);
        }
        v.event(opts, &mut sum, "moved", &path, "");
    }
    Ok(sum)
}

/// Which vaults recorded the current version of each file under `paths`, or
/// of every file if there are none. Reads the log only: a change no scan has
/// recorded is nobody's yet. Returns what it printed, as (path, nicknames).
pub fn blame(opts: &Options, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<(String, Vec<String>)>> {
    let v = Vault::open(opts, cwd, true)?;
    let mut prefixes = prefixes(&v.root, cwd, paths)?;
    if prefixes.is_empty() {
        prefixes.push(String::new());
    }
    let mut found = vec![];
    for path in v.log.paths.keys() {
        if !prefixes.iter().any(|prefix| prefix_matches(prefix, path)) {
            continue;
        }
        let PathState::Resolved { winner, .. } = v.log.resolve(path) else {
            continue;
        };
        if v.log.entries[&winner].hash.is_none() {
            continue;
        }
        let nicknames: Vec<String> = v.log.authors[&winner].iter().cloned().collect();
        let blame = Message::Blame {
            path: path.clone(),
            nicknames: nicknames.clone(),
        };
        print_stdout(&blame, opts.json);
        found.push((path.clone(), nicknames));
    }
    Ok(found)
}

/// The recorded history of each file under `paths`, or of every file if
/// there are none: per path, newest first by mtime. Reads the log only.
/// Returns what it printed.
pub fn log(opts: &Options, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<Version>> {
    let v = Vault::open(opts, cwd, true)?;
    let mut prefixes = prefixes(&v.root, cwd, paths)?;
    if prefixes.is_empty() {
        prefixes.push(String::new());
    }
    let mut found = vec![];
    for path in v.log.paths.keys() {
        if !prefixes.iter().any(|prefix| prefix_matches(prefix, path)) {
            continue;
        }
        let winner = match v.log.resolve(path) {
            PathState::Resolved { winner, .. } => Some(winner),
            _ => None,
        };
        let mut entries = v.log.valid(path);
        entries.sort_by_key(|(id, e)| Reverse((e.mtime, *id)));
        for (id, e) in entries {
            let version = Version {
                path: path.clone(),
                hash: e.hash.clone(),
                mtime: e.mtime,
                nicknames: v.log.authors[id].iter().cloned().collect(),
                current: winner.as_ref() == Some(id),
                available: e.hash.as_ref().is_some_and(|h| available(&v, None, h)),
            };
            print_stdout(&Message::Version(version.clone()), opts.json);
            found.push(version);
        }
    }
    Ok(found)
}

/// Bring back the version of `path` whose hash starts with `hash`: as a copy
/// beside the file, named as a conflict copy is, or in place of it. Returns
/// the vault-relative path that holds the version afterwards.
///
/// A copy is an ordinary new file, recorded by the next scan. In place, the
/// folder is scanned first so that no unrecorded edit is lost, and again
/// afterwards so that the restored version is recorded like any other edit.
pub fn restore(
    opts: &Options,
    cwd: &Path,
    path: &Path,
    hash: &str,
    in_place: bool,
) -> Result<String> {
    let mut v = Vault::open(opts, cwd, true)?;
    let p = relative(&v.root, &resolve_path(&cwd.join(path))?)?;
    let mut sum = Summary::default();
    if in_place {
        record(opts, &mut v, None, &mut sum)?;
    }
    let versions = v.log.valid(&p).into_iter();
    let hashes: BTreeSet<String> = versions
        .filter_map(|(_, e)| e.hash.clone())
        .filter(|h| !hash.is_empty() && h.starts_with(hash))
        .collect();
    let mut hashes = hashes.into_iter();
    let h = match (hashes.next(), hashes.next()) {
        (Some(h), None) => h,
        (None, _) => bail!("no version of {p} has a hash starting with {hash}; see `syncit log`"),
        (Some(_), Some(_)) => bail!("several versions of {p} have a hash starting with {hash}"),
    };
    let held = v.cache.rows.get(&p).map(|row| row.hash.clone());
    let dest = if in_place {
        p.clone()
    } else {
        copy_name(&p, &h)
    };
    let already = |what: &str| {
        let message = format!("{dest} {what}");
        print_stdout(&Message::Info { message }, opts.json);
    };
    if in_place {
        if held.as_ref() == Some(&h) {
            already("is already that version");
            return Ok(dest);
        }
        // With no file here, a scan would take the restored bytes for an old
        // copy and the next sync would bring it forward again, unless the
        // path is deleted.
        let deleted = matches!(v.log.resolve(&p), PathState::Resolved { winner, .. }
            if v.log.entries[&winner].hash.is_none());
        ensure!(
            held.is_some() || deleted,
            "{p} is not on disk in this vault: sync or `syncit get` it first, or restore a copy"
        );
    } else {
        ensure!(!v.skips(&dest), "the copy name {dest} is ignored");
        let file = v.root.join(&dest);
        if file.symlink_metadata().is_ok() {
            ensure!(
                hash_file(&file).is_ok_and(|x| x == h),
                "the copy name {dest} is taken"
            );
            already("is already there");
            return Ok(dest);
        }
    }
    ensure!(
        write_now(&mut v, &dest, &h)?,
        "the content of that version is not in this vault; old versions are kept only while local-history is on"
    );
    if in_place {
        record(opts, &mut v, None, &mut sum)?;
    }
    let kind = match (in_place, held) {
        (false, _) => "copied",
        (true, Some(_)) => "updated",
        (true, None) => "added",
    };
    v.event(opts, &mut sum, kind, &dest, "");
    Ok(dest)
}

/// A change `config` can make.
pub enum Setting {
    LocalHistory(bool),
    Nickname(String),
}

/// Print this vault's settings, after changing one if asked. Returns the
/// config as it now is.
pub fn config(opts: &Options, cwd: &Path, change: Option<Setting>) -> Result<Config> {
    let mut v = Vault::open(opts, cwd, true)?;
    let was = v.config.keep_local_history;
    match change {
        Some(Setting::LocalHistory(on)) => v.config.keep_local_history = on,
        Some(Setting::Nickname(name)) => {
            ensure!(
                valid_nickname(&name),
                "a nickname is 1 to 64 bytes, with no spaces or control characters"
            );
            v.config.nickname = name;
        }
        None => {}
    }
    v.config.save(&v.root)?;
    let on = v.config.keep_local_history;
    let settings = [
        ("local-history", if on { "on" } else { "off" }.to_string()),
        ("nickname", v.config.nickname.clone()),
    ];
    for (name, value) in settings {
        print_stdout(&Message::Setting { name, value }, opts.json);
    }
    if was && !on {
        let message = "versions already in .syncit/blobs/ are left there".to_string();
        print_stdout(&Message::Info { message }, opts.json);
    }
    Ok(v.config)
}
