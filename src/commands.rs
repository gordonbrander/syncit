//! One function per command. Each prints as it goes; a command that runs a
//! sync returns a `Summary` for `main` to print last.

use crate::blobs::{self, available, hash_file, retain};
use crate::config::{Config, default_nickname, prefix_matches};
use crate::dirstate::is_exec;
use crate::entry::valid_path;
use crate::log::PathState;
use crate::materialise::{copy_name, materialise, no_symlinked_parent, prune, write_now};
use crate::output::{
    Event, Message, Options, State, Summary, Version, print_states, print_stdout, warn,
};
use crate::scan::{deleted_heads, scan};
use crate::segment::{self, valid_nickname};
use crate::vault::{TMP_PREFIX, Vault, find_root, relative, resolve_path, unique};
use anyhow::{Context, Result, bail, ensure};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Sync,
    Push,
    Pull,
}

/// Make `dir` a vault that includes `include`, nicknamed after the folder.
fn create(dir: &Path, include: Vec<String>) -> Result<Config> {
    let folder = resolve_path(dir)?;
    let folder = folder.file_name().unwrap_or_default().to_string_lossy();
    let config = Config {
        include,
        ..Config::new(String::new(), default_nickname(&folder))
    };
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
    let config = create(dir, vec![String::new()])?;
    let message = format!(
        "Vault initialized: {}\nNickname: {}",
        resolve_path(dir)?.display(),
        config.nickname
    );
    print_stdout(&Message::Info { message }, opts);
    Ok(())
}

/// The error for a URL in a build without remote vaults.
#[cfg(not(feature = "remote"))]
fn no_remote() -> anyhow::Error {
    anyhow::anyhow!("this build has no remote support: install with the remote feature")
}

/// Make a remote vault at `url`, which must be reachable and empty. Returns
/// the phrase it printed, if it generated the key.
#[cfg(feature = "remote")]
pub fn init_remote(
    opts: &Options,
    url: &str,
    nickname: Option<String>,
    save_key: bool,
) -> Result<Option<String>> {
    use crate::remote::{Bucket, credentials};
    use crate::{crypt, keys};
    let nickname = nickname.unwrap_or_else(|| "origin".to_string());
    ensure!(
        valid_nickname(&nickname),
        "a nickname is 1 to 64 bytes, with no spaces or control characters"
    );
    let bucket = Bucket::new(url, credentials(opts, url)?)?;
    ensure!(bucket.is_empty()?, "{url} is not empty");
    let (key, generated) = match &opts.secret_key {
        Some(phrase) => (
            keys::parse_phrase(phrase).context("SYNCIT_SECRET_KEY")?,
            false,
        ),
        None => (crypt::new_key()?, true),
    };
    let config = Config {
        keep_history: None,
        autocompact: false,
        ..Config::new(String::new(), nickname)
    };
    bucket.create(&key, &config)?;
    let message = format!("Vault initialized: {url}\nNickname: {}", config.nickname);
    print_stdout(&Message::Info { message }, opts);
    if !generated {
        return Ok(None);
    }
    let words = keys::phrase(&key);
    let message = format!(
        "Secret key for {url}:\n\n  {}\n\nWrite these words down and keep them safe. They are the only way to\nread this vault from another computer, and they are not shown again.",
        *words
    );
    print_stdout(&Message::Info { message }, opts);
    keys::save(opts, url, &key, keys::Source::New, save_key)?;
    Ok(Some(words.to_string()))
}

#[cfg(not(feature = "remote"))]
pub fn init_remote(
    _opts: &Options,
    _url: &str,
    _nickname: Option<String>,
    _save_key: bool,
) -> Result<Option<String>> {
    Err(no_remote())
}

/// Paths from a vault's root, as given on the command line.
fn vault_paths(paths: &[String]) -> Result<()> {
    for path in paths {
        ensure!(valid_path(path), "{path} is not a path a vault can have");
    }
    Ok(())
}

/// Make `dir` a vault with the files of the vault `src`: a URL, a path in a
/// vault, or a nickname in the vault at `cwd`. Without `only`, it includes
/// the sub-folder the path names, or everything. With it, `src` is a URL or
/// a vault's root, and it includes exactly those paths. The source is
/// remembered in `remotes`, then pulled.
pub fn clone(
    opts: &Options,
    cwd: &Path,
    src: &Path,
    dir: &Path,
    only: &[String],
    save_key: bool,
) -> Result<Summary> {
    vault_paths(only)?;
    let local = Vault::open(opts, cwd, true).ok();
    let src = &resolve(local.as_ref(), src);
    std::mem::drop(local); // it may be the source, which is locked
    let (source, include) = match url(src) {
        Some(_) if only.is_empty() => (src.to_path_buf(), vec![String::new()]),
        Some(_) => (src.to_path_buf(), only.to_vec()),
        None => {
            let src = resolve_path(src)?;
            let root = find_root(&src)?;
            let sub = relative(&root, &src)?;
            ensure!(
                !resolve_path(dir)?.starts_with(&root),
                "cannot clone a vault into itself"
            );
            ensure!(
                only.is_empty() || sub.is_empty(),
                "{} is inside the vault {}: with --only, give the vault's root",
                src.display(),
                root.display()
            );
            match only.is_empty() {
                true => (root, vec![sub]),
                false => (root, only.to_vec()),
            }
        }
    };
    let empty = !dir.exists() || dir.read_dir()?.next().is_none();
    ensure!(empty, "{} exists and is not empty", dir.display());
    create(dir, include)?;
    let location = source.to_str().context("the source is not UTF-8")?;
    let added =
        Vault::open(opts, dir, true).and_then(|mut v| add(opts, &mut v, location, None, save_key));
    if let Err(e) = added {
        let _ = fs::remove_dir_all(dir.join(".syncit"));
        return Err(e);
    }
    run(opts, dir, &source, Mode::Pull)
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

/// A target names another vault: a URL, or a path.
fn url(target: &Path) -> Option<&str> {
    target.to_str().filter(|t| t.contains("://"))
}

/// A target after the local vault's `remotes`: a nickname there is replaced
/// by its location.
fn resolve(local: Option<&Vault>, target: &Path) -> PathBuf {
    let location = target.to_str().and_then(|t| local?.config.remotes.get(t));
    location.map_or_else(|| target.to_path_buf(), PathBuf::from)
}

/// Open the vault a target names, for a command run in `local`.
fn open_target(opts: &Options, local: Option<&Vault>, target: &Path) -> Result<Vault> {
    match url(target) {
        #[cfg(feature = "remote")]
        Some(url) => Vault::open_url(opts, url, local),
        #[cfg(not(feature = "remote"))]
        Some(_) => {
            let _ = local;
            Err(no_remote())
        }
        None => Vault::open(opts, target, false),
    }
}

/// Open another vault for a run with `v`.
fn other(opts: &Options, v: &mut Vault, target: &Path) -> Result<Vault> {
    let target = &resolve(Some(v), target);
    if url(target).is_none() {
        ensure!(
            target.exists(),
            "no vault at {}: not a folder, a URL or the name of a remote",
            target.display()
        );
        ensure!(
            !nested(&find_root(target)?, &v.root),
            "cannot sync a vault with itself or with a vault inside it"
        );
    }
    let mut peer = open_target(opts, Some(v), target)?;
    (v.side, peer.side) = ("->", "<-");
    Ok(peer)
}

/// After a run, one line for each vault that is remote: what was sent to
/// it and received from it.
fn report(opts: &Options, vaults: [&Vault; 2]) {
    for v in vaults.into_iter().filter(|v| v.bare) {
        let t = v.traffic.borrow();
        let lines = [
            ("send", "sent", "to", &t.sent_files, &t.sent_segments),
            (
                "receive",
                "received",
                "from",
                &t.received_files,
                &t.received_segments,
            ),
        ];
        for (verb, done, to, files, segments) in lines {
            if files.is_empty() && segments.is_empty() {
                continue;
            }
            let verb = if opts.dry_run {
                format!("would {verb}")
            } else {
                done.into()
            };
            let (f, s) = (files.len(), segments.len());
            let message = format!("{verb} {f} files, {s} segments {to} {}", v.name);
            print_stdout(&Message::Info { message }, opts);
        }
    }
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
    autocompact(opts, &a)?;
    autocompact(opts, &b)?;
    report(opts, [&a, &b]);
    Ok(sum)
}

/// At the end of a run, drop the stored versions the vault's `keep_history`
/// no longer covers, if it compacts by itself. After materialise and never
/// before, so that a run cannot drop a version it is about to read.
fn autocompact(opts: &Options, v: &Vault) -> Result<()> {
    if v.config.autocompact {
        blobs::compact(opts, v)?;
    }
    Ok(())
}

/// Say that this is a dry run, if it is.
fn announce(opts: &Options) {
    if opts.dry_run {
        let message = "dry run: nothing is changed".to_string();
        print_stdout(&Message::Info { message }, opts);
    }
}

/// The files `from` has that `to` excludes.
pub fn excluded(from: &Vault, to: &Vault) -> Vec<String> {
    let excluded = |p: &&String| !to.config.includes(p) && !to.skips(p);
    from.dirstate
        .rows
        .keys()
        .filter(excluded)
        .cloned()
        .collect()
}

/// The files `from` has that `to` ignores.
pub fn ignored(from: &Vault, to: &Vault) -> Vec<String> {
    let ignored = |p: &&String| to.skips(p);
    from.dirstate.rows.keys().filter(ignored).cloned().collect()
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
                b.name,
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
    if opts.no_remove {
        if to_b {
            keep(opts, b, a, sum)?;
        }
        if to_a {
            keep(opts, a, b, sum)?;
        }
        // Each receiver takes the other's re-creations, so one run converges.
        // ponytail: a corrupt segment is reported as rejected a second time
        // here; have `receive` remember what it rejected if that matters.
        if to_b {
            receive(opts, b, a, sum)?;
        }
        if to_a {
            receive(opts, a, b, sum)?;
        }
    }
    // Both retain before either materialises, so that a version is saved
    // before it is replaced.
    if to_b {
        retain(opts, b, Some(&*a))?;
    }
    if to_a {
        retain(opts, a, Some(&*b))?;
    }
    // A merge replaces two versions. It is made only if a vault receiving in
    // this run keeps history: its `retain` above then has them, so they can
    // be restored. The setting, not the blobs, so a dry run decides alike.
    let keeps = |v: &Vault| v.config.keep_history != Some(0);
    let kept = (to_a && keeps(a)) || (to_b && keeps(b));
    if to_b {
        materialise(opts, b, Some(a), kept, sum)?;
    }
    if to_a {
        materialise(opts, a, Some(b), kept, sum)?;
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

/// `--no-remove`: a file here whose path the log says is deleted is kept, and
/// recorded as re-created, as if the user had put it back. Its row becomes a
/// deletion record of the tombstones, and the scan writes the entry.
fn keep(opts: &Options, v: &mut Vault, peer: &Vault, sum: &mut Summary) -> Result<()> {
    let rows: Vec<String> = v.dirstate.rows.keys().cloned().collect();
    let mut kept = false;
    for path in rows {
        if !v.takes(&path) {
            continue;
        }
        let Some(heads) = deleted_heads(&v.log, &path) else {
            continue;
        };
        v.dirstate.rows.remove(&path);
        v.dirstate.deleted.insert(path.clone(), heads);
        v.event(opts, sum, "added", &path, "kept");
        kept = true;
    }
    if kept {
        record(opts, v, Some(peer), sum)?;
    }
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
    let have = v.storage.segment_names()?;
    for name in from.storage.segment_names()? {
        if have.contains(&name) {
            continue;
        }
        let read = from.storage.read_segment(&name);
        match read.and_then(|bytes| Ok((segment::decode(&name, &bytes)?, bytes))) {
            Ok((segment, bytes)) => {
                if !opts.dry_run {
                    v.storage.write_segment(&name, &bytes)?;
                }
                if v.bare {
                    v.traffic.borrow_mut().sent_segments.insert(name.clone());
                }
                if from.bare {
                    from.traffic
                        .borrow_mut()
                        .received_segments
                        .insert(name.clone());
                }
                for (id, e) in segment.entries {
                    v.log.insert(id, e, &segment.nickname);
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
            v.log.insert(id.clone(), e.clone(), nickname);
        }
    }
}

/// The copies of the open conflict at `path`, as a run names them; None if
/// the path has no conflict.
fn conflict_copies(path: &str, state: &PathState) -> Option<String> {
    let PathState::Resolved {
        losers,
        lost_delete,
        ..
    } = state
    else {
        return None;
    };
    if losers.is_empty() && !lost_delete {
        return None;
    }
    let mut what: Vec<String> = losers.iter().map(|l| copy_name(path, &l.hash)).collect();
    if *lost_delete {
        what.push("deleted in another vault".to_string());
    }
    Some(what.join(", "))
}

/// Print each open conflict among the paths this vault includes that
/// is not in `seen`, and add it there.
fn conflicts(opts: &Options, v: &Vault, seen: &mut BTreeSet<String>) {
    for path in v.log.paths.keys() {
        if !v.config.includes(path) {
            continue;
        }
        let Some(message) = conflict_copies(path, &v.log.resolve(path)) else {
            continue;
        };
        if !seen.insert(path.clone()) {
            continue;
        }
        let conflict = Message::Conflict {
            vault: v.name.clone(),
            path: path.clone(),
            message,
            side: v.side,
        };
        print_stdout(&conflict, opts);
    }
}

pub fn snapshot(opts: &Options, cwd: &Path) -> Result<Summary> {
    let mut v = Vault::open(opts, cwd, true)?;
    let mut sum = Summary::default();
    record(opts, &mut v, None, &mut sum)?;
    autocompact(opts, &v)?;
    Ok(sum)
}

/// Bytes, for people.
fn size(bytes: u64) -> String {
    match bytes {
        0..1_000 => format!("{bytes} B"),
        1_000..1_000_000 => format!("{:.1} kB", bytes as f64 / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1} MB", bytes as f64 / 1e6),
        _ => format!("{:.1} GB", bytes as f64 / 1e9),
    }
}

fn versions(n: usize) -> String {
    format!("{n} version{}", if n == 1 { "" } else { "s" })
}

/// Drop from `.syncit/blobs/` the versions older than the vault's
/// `keep_history` keeps. The log stays whole: only content goes. Returns how
/// many were dropped, or with `--dry-run` would be, and their bytes.
pub fn compact(opts: &Options, cwd: &Path, vault: Option<&Path>) -> Result<(usize, u64)> {
    let v = open_vault(opts, cwd, vault)?;
    announce(opts);
    let (n, bytes) = blobs::compact(opts, &v)?;
    let message = match n {
        0 => "nothing to drop".to_string(),
        _ => format!("dropped {} ({})", versions(n), size(bytes)),
    };
    print_stdout(&Message::Info { message }, opts);
    Ok((n, bytes))
}

/// A vault's state for a path (SPEC.md section 11, `status`); the first rule
/// that applies decides. `deleted` is a recorded removal, which `status`
/// prints as `removed` too, and `leftover` a file whose removal is known,
/// which it prints as `behind` or `incomplete`. `winner` is the hash of the path's current version,
/// None in it for a deletion, and None if the path's history is incomplete.
/// `changes` is what a dry scan of the vault found.
fn state(
    v: &Vault,
    path: &str,
    winner: Option<Option<&String>>,
    conflict: bool,
    changes: &BTreeMap<String, &'static str>,
) -> &'static str {
    let (row, change) = (v.dirstate.rows.get(path), changes.get(path));
    if v.skips(path) {
        return "ignored";
    }
    if row.is_none() && change.is_none() && !v.config.includes(path) {
        return "excluded";
    }
    let Some(winner) = winner else {
        return "incomplete";
    };
    if conflict {
        return "conflict";
    }
    match (change, row) {
        (Some(&"deleted"), _) => "removed",
        (Some(kind), _) => kind,
        (None, Some(row)) if Some(&row.hash) == winner => "current",
        (None, Some(_)) if winner.is_none() => "leftover",
        (None, Some(_)) => "behind",
        (None, None) if winner.is_none() => "deleted",
        (None, None) => "missing",
    }
}

/// Whether a line of `status` needs nothing: no sync and no scan would
/// change what either vault has of it.
fn settled(line: &State) -> bool {
    let settled = |state| matches!(state, "current" | "excluded" | "ignored" | "deleted");
    settled(line.ours) && line.theirs.is_none_or(settled)
}

/// The lines of `status` for the files under the folder `dir`, which are
/// sorted by path. A folder below the top whose files, more than one, are all
/// in the same state is one line; any other is listed child by child. A
/// conflict is always a line of its own: it names its copies.
fn collapse(dir: &str, files: &[State], top: bool, out: &mut Vec<State>) {
    let skip = if dir.is_empty() { 0 } else { dir.len() + 1 };
    // A folder of files renamed from one folder, keeping their names below
    // it, is that folder renamed.
    fn from_dir(f: &State, skip: usize) -> Option<Option<&str>> {
        let from = f.from.as_deref()?;
        Some(
            from.strip_suffix(&f.path[skip..])
                .filter(|d| d.ends_with('/')),
        )
    }
    let keys: Vec<_> = (files.iter())
        .map(|f| (f.ours, f.theirs, f.conflict.is_some(), from_dir(f, skip)))
        .collect();
    let first = keys[0];
    let same = !first.2 && first.3 != Some(None) && keys.iter().all(|k| *k == first);
    if !top && same && files.len() > 1 {
        out.push(State {
            path: format!("{dir}/"),
            files: files.len(),
            from: first.3.flatten().map(String::from),
            ..files[0].clone()
        });
        return;
    }
    // A child is the next name in the path, with its `/` if it is a folder.
    let child = |f: &State| {
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

/// The state of every file the log knows under `paths` in this vault, and
/// with `peer` in that vault too: what a scan would record, what a sync
/// would bring or remove, and what is waiting for the user. Only the lines
/// that need something, unless `all`. Folders in one state are one line.
/// Writes nothing. Returns what it printed.
pub fn status(
    opts: &Options,
    cwd: &Path,
    paths: &[PathBuf],
    peer: Option<&Path>,
    all: bool,
) -> Result<Vec<State>> {
    let opts = &Options {
        dry_run: true,
        no_remove: false,
        ..opts.clone()
    };
    let mut v = Vault::open(opts, cwd, true)?;
    let mut scopes = prefixes(&v.root, cwd, paths)
        .context("paths are in this vault; to compare with another, add `--vs <vault>`")?;
    if scopes.is_empty() {
        scopes.push(String::new());
    }
    let mut peer = peer.map(|path| other(opts, &mut v, path)).transpose()?;
    // Dry scans, which say what no scan has recorded; then one log for both
    // vaults, so that each is compared with the same winner.
    let scanned = scan(opts, &mut v, true)?;
    let ours: BTreeMap<_, _> = scanned.changes.into_iter().collect();
    let mut theirs = BTreeMap::new();
    if let Some(peer) = &mut peer {
        theirs = scan(opts, peer, true)?.changes.into_iter().collect();
        absorb(&mut v, peer);
    }
    let mut files: Vec<State> = vec![];
    for path in v.log.paths.keys() {
        let resolved = v.log.resolve(path);
        let winner = match &resolved {
            PathState::Absent => continue,
            PathState::Incomplete => None,
            PathState::Resolved { winner, .. } => Some(v.log.entries[winner].hash.as_ref()),
        };
        let copies = conflict_copies(path, &resolved);
        let of = |v: &Vault, changes| state(v, path, winner, copies.is_some(), changes);
        let (a, b) = (of(&v, &ours), peer.as_ref().map(|peer| of(peer, &theirs)));
        // A file still here whose removal is known is behind a vault on the
        // line that removed it. With none, a sync did not finish.
        let leftover = |state, other| match (state, other) {
            ("leftover", Some("removed" | "deleted")) => "behind",
            ("leftover", _) => "incomplete",
            _ => state,
        };
        let mut line = State {
            path: path.clone(),
            files: 1,
            ours: leftover(a, b),
            theirs: b.map(|b| leftover(b, Some(a))),
            conflict: None,
            from: None,
        };
        // A deleted path is listed only while a vault has something to do.
        if winner == Some(None) && settled(&line) {
            continue;
        }
        if line.ours == "conflict" || line.theirs == Some("conflict") {
            line.conflict = copies;
        }
        files.push(line);
    }
    // A file removed here and its content new here is one file renamed, if
    // the other vault sees the two alike and the paths asked about take both.
    for (from, to) in &scanned.moves {
        let at = |files: &[State], p: &str| files.binary_search_by(|f| f.path.as_str().cmp(p));
        let (Ok(i), Ok(j)) = (at(&files, from), at(&files, to)) else {
            continue;
        };
        let (gone, new) = (&files[i], &files[j]);
        let scoped = |s: &String| prefix_matches(s, from) == prefix_matches(s, to);
        if (gone.ours, new.ours) == ("removed", "new")
            && gone.theirs == new.theirs
            && scopes.iter().all(scoped)
        {
            files[j].ours = "renamed";
            files[j].from = Some(from.clone());
            files.remove(i);
        }
    }
    let mut found = vec![];
    for prefix in &scopes {
        // The prefix itself, if it is a file; then what lies under it.
        let (file, under): (Vec<State>, Vec<State>) = files
            .iter()
            .filter(|f| prefix_matches(prefix, &f.path))
            .cloned()
            .partition(|f| f.path == *prefix);
        found.extend(file);
        if !under.is_empty() {
            collapse(prefix, &under, true, &mut found);
        }
    }
    // Folders are made of every file, and only then are the quiet lines
    // dropped: a folder's count is of all its files.
    found.retain(|line| all || !settled(line));
    // One word for a file gone from a vault, whether that is recorded or not.
    let word = |state| if state == "deleted" { "removed" } else { state };
    for line in &mut found {
        (line.ours, line.theirs) = (word(line.ours), line.theirs.map(word));
    }
    print_states(&found, opts);
    Ok(found)
}

/// Command-line paths, relative to `cwd`, as prefixes relative to `root`.
fn prefixes(root: &Path, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<String>> {
    let prefix = |path: &PathBuf| {
        let prefix = relative(root, &resolve_path(&cwd.join(path))?)?;
        ensure!(
            prefix.is_empty() || valid_path(&prefix),
            "{} is not a path a vault can have",
            path.display()
        );
        Ok(prefix)
    };
    paths.iter().map(prefix).collect()
}

/// Add paths of another vault to this one. Each path lies inside that vault,
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
        include(&mut v, prefixes(&source, cwd, paths)?)?;
    }
    run(opts, cwd, &source, Mode::Pull)
}

/// Include `prefixes` in the vault, and save its config.
fn include(v: &mut Vault, prefixes: Vec<String>) -> Result<()> {
    for prefix in prefixes {
        v.config.exclude.retain(|x| !prefix_matches(&prefix, x));
        v.config.include.retain(|x| !prefix_matches(&prefix, x));
        v.config.include.push(prefix);
    }
    v.storage.save_config(&v.config)
}

/// `get --from`: add paths of the vault `from`, a target, each given from
/// that vault's root. This is the form that reaches a remote vault.
pub fn get_from(opts: &Options, cwd: &Path, paths: &[String], from: &Path) -> Result<Summary> {
    ensure!(!paths.is_empty(), "no path given");
    vault_paths(paths)?;
    {
        let mut v = Vault::open(opts, cwd, true)?;
        let target = resolve(Some(&v), from);
        if url(&target).is_none() {
            ensure!(
                !nested(&find_root(&target)?, &v.root),
                "cannot get files from this vault or a vault inside it"
            );
        }
        include(&mut v, paths.to_vec())?;
    }
    run(opts, cwd, from, Mode::Pull)
}

/// The vault a command acts on: the one `vault` names if given (`--vault`),
/// otherwise the one here. A remote needs no local vault.
fn open_vault(opts: &Options, cwd: &Path, vault: Option<&Path>) -> Result<Vault> {
    let Some(target) = vault else {
        return Vault::open(opts, cwd, true);
    };
    let local = Vault::open(opts, cwd, true).ok();
    let target = resolve(local.as_ref(), target);
    match url(&target) {
        Some(_) => open_target(opts, local.as_ref(), &target),
        None => {
            std::mem::drop(local); // it may be the target, which is locked
            Vault::open(opts, &target, true)
        }
    }
}

/// List the remotes this vault remembers. Returns them, as printed.
pub fn remotes(opts: &Options, cwd: &Path) -> Result<Vec<(String, String)>> {
    let v = Vault::open(opts, cwd, true)?;
    for (nickname, location) in &v.config.remotes {
        let message = Message::Remote {
            nickname: nickname.clone(),
            location: location.clone(),
        };
        print_stdout(&message, opts);
    }
    Ok(v.config.remotes.into_iter().collect())
}

/// Remember the vault at `location`, a URL or a path, under `nickname` or its
/// own nickname. Returns the nickname.
pub fn remote_add(
    opts: &Options,
    cwd: &Path,
    location: &str,
    nickname: Option<String>,
    save_key: bool,
) -> Result<String> {
    let mut v = Vault::open(opts, cwd, true)?;
    add(opts, &mut v, location, nickname, save_key)
}

fn add(
    opts: &Options,
    v: &mut Vault,
    location: &str,
    nickname: Option<String>,
    save_key: bool,
) -> Result<String> {
    let (location, config, found) = if location.contains("://") {
        let (config, found) = find_remote(opts, location)?;
        (location.to_string(), config, Some(found))
    } else {
        let root = resolve_path(Path::new(location))?;
        ensure!(
            root.join(".syncit").is_dir(),
            "{} is not a vault",
            root.display()
        );
        ensure!(!nested(&root, &v.root), "a vault cannot be its own remote");
        (root.display().to_string(), Config::load(&root)?, None)
    };
    let nickname = nickname.unwrap_or(config.nickname);
    ensure!(
        valid_nickname(&nickname) && !nickname.contains('/'),
        "{nickname} cannot be a remote nickname: pass --nickname <nickname>"
    );
    ensure!(
        !v.config.remotes.contains_key(&nickname),
        "{nickname} is already a remote here: pass --nickname <nickname>"
    );
    if let Some(found) = found {
        save_found(opts, &location, found, save_key)?;
    }
    v.config.remotes.insert(nickname.clone(), location.clone());
    v.storage.save_config(&v.config)?;
    let message = format!("remote {nickname}: {location}");
    print_stdout(&Message::Info { message }, opts);
    Ok(nickname)
}

/// Forget a remote. Its keychain item and `secrets.toml` entry stay, since
/// other vaults on this machine may use them.
pub fn remote_remove(opts: &Options, cwd: &Path, nickname: &str) -> Result<()> {
    let mut v = Vault::open(opts, cwd, true)?;
    let location = v.config.remotes.remove(nickname);
    let location = location.with_context(|| format!("no remote nicknamed {nickname}"))?;
    v.storage.save_config(&v.config)?;
    let message = format!("removed remote {nickname}: {location}");
    print_stdout(&Message::Info { message }, opts);
    Ok(())
}

/// A remote's key, found or asked for, and where it came from.
#[cfg(feature = "remote")]
type Found = (crate::crypt::Key, crate::keys::Source);
#[cfg(not(feature = "remote"))]
type Found = ();

/// The config of the remote at `url`, and its key: found (store.md,
/// section 8), or else asked for on a terminal.
#[cfg(feature = "remote")]
fn find_remote(opts: &Options, url: &str) -> Result<(Config, Found)> {
    use crate::keys::{self, Source};
    use std::io::IsTerminal;
    if let Some(phrase) = &opts.secret_key {
        keys::parse_phrase(phrase).context("SYNCIT_SECRET_KEY")?;
    }
    let settings = crate::remote::credentials(opts, url)?;
    let bucket = crate::remote::Bucket::new(url, settings)?;
    let check = bucket.check()?;
    let found = match keys::find(opts, url, &check) {
        Ok(found) => found,
        Err(e) if !std::io::stdin().is_terminal() => return Err(e),
        Err(_) => (keys::prompt(url, &check)?, Source::New),
    };
    Ok((crate::remote::read_config(&bucket, &found.0)?, found))
}

#[cfg(not(feature = "remote"))]
fn find_remote(_opts: &Options, _url: &str) -> Result<(Config, Found)> {
    Err(no_remote())
}

#[cfg(feature = "remote")]
fn save_found(opts: &Options, url: &str, found: Found, save: bool) -> Result<()> {
    crate::keys::save(opts, url, &found.0, found.1, save)
}

#[cfg(not(feature = "remote"))]
fn save_found(_: &Options, _: &str, _: Found, _: bool) -> Result<()> {
    Ok(())
}

/// Move files to another vault: push to it, confirm each file arrived, then
/// exclude them here. Nothing is removed unless every file is confirmed.
pub fn move_to(opts: &Options, cwd: &Path, paths: &[PathBuf], target: &Path) -> Result<Summary> {
    let mut v = Vault::open(opts, cwd, true)?;
    let prefixes = prefixes(&v.root, cwd, paths)?;
    let mut t = other(opts, &mut v, target)?;
    let mut sum = Summary::default();
    announce(opts);
    record(opts, &mut v, Some(&t), &mut sum)?;
    record(opts, &mut t, Some(&v), &mut sum)?;

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let present: Vec<String> = v.dirstate.rows.keys().filter(under).cloned().collect();
    ensure!(
        !present.is_empty(),
        "nothing to move: no files here under the given paths"
    );
    let paths = || present.iter().map(String::as_str);
    let ignored: Vec<&str> = paths().filter(|p| t.skips(p)).collect();
    let excluded: Vec<&str> = paths().filter(|p| !t.config.includes(p)).collect();
    for (verb, refused) in [("ignores", ignored), ("excludes", excluded)] {
        ensure!(
            refused.is_empty(),
            "nothing moved: {} {verb} {}",
            t.name,
            refused.join(", ")
        );
    }

    exchange(opts, &mut v, &mut t, Mode::Push, &mut sum)?;
    let arrived = |p: &&String| {
        t.dirstate.rows.get(*p).map(|row| &row.hash) == Some(&v.dirstate.rows[*p].hash)
    };
    let target = t.name.clone();
    let missing: Vec<String> = present
        .iter()
        .filter(|p| !arrived(p))
        .map(|p| {
            let said = |e: &&Event| e.vault == target && &e.path == p;
            match sum.events.iter().find(said) {
                Some(e) => format!("{p} ({}: {})", e.kind, e.note),
                None => format!("{p} (it has a different version; sync first)"),
            }
        })
        .collect();
    ensure!(
        missing.is_empty(),
        "nothing moved: {target} does not have {}",
        missing.join(", ")
    );

    remove_here(opts, &mut v, prefixes, present, "moved", &mut sum)?;
    report(opts, [&v, &t]);
    Ok(sum)
}

/// Stop having files here without sending them anywhere: exclude the paths
/// and remove their files from this vault only. Unless `force`, every file's
/// content must be in this vault's history or in the vault named by `check`.
pub fn drop(
    opts: &Options,
    cwd: &Path,
    paths: &[PathBuf],
    check: Option<&Path>,
    force: bool,
) -> Result<Summary> {
    let mut v = Vault::open(opts, cwd, true)?;
    let prefixes = prefixes(&v.root, cwd, paths)?;
    let mut check = check.map(|path| other(opts, &mut v, path)).transpose()?;
    let mut sum = Summary::default();
    announce(opts);
    // Unrecorded changes are recorded before anything is removed, and the
    // checked vault is scanned so that its rows say what it has now.
    record(opts, &mut v, check.as_ref(), &mut sum)?;
    if let Some(c) = &mut check {
        record(opts, c, Some(&v), &mut sum)?;
    }

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let present: Vec<String> = v.dirstate.rows.keys().filter(under).cloned().collect();
    if !force {
        let elsewhere = |hash: &String| {
            v.storage.has_blob(hash)
                || check.as_ref().is_some_and(|c| {
                    c.dirstate.rows.values().any(|row| &row.hash == hash)
                        || c.storage.has_blob(hash)
                })
        };
        let sole: Vec<&str> = present
            .iter()
            .filter(|p| !elsewhere(&v.dirstate.rows[*p].hash))
            .map(String::as_str)
            .collect();
        let hint = match check {
            Some(_) => "Sync with it first",
            None => "Name a vault that has them with --check <vault>",
        };
        ensure!(
            sole.is_empty(),
            "nothing dropped: no other copy is known of {}. {hint}, or use --force",
            sole.join(", ")
        );
    }
    remove_here(opts, &mut v, prefixes, present, "dropped", &mut sum)?;
    Ok(sum)
}

/// Exclude `prefixes` and remove the `present` files under them from this
/// vault only, reporting each as `kind`. No deletion is recorded.
fn remove_here(
    opts: &Options,
    v: &mut Vault,
    prefixes: Vec<String>,
    present: Vec<String>,
    kind: &'static str,
    sum: &mut Summary,
) -> Result<()> {
    for prefix in prefixes {
        v.config.include.retain(|x| !prefix_matches(&prefix, x));
        v.config.exclude.retain(|x| !prefix_matches(&prefix, x));
        v.config.exclude.push(prefix);
    }
    if !opts.dry_run {
        v.storage.save_config(&v.config)?;
    }
    // Rows go before files: a file with no row gets one again at the next
    // scan, but a row with no file would be recorded as a deletion.
    let rows: Vec<_> = present
        .into_iter()
        .filter_map(|path| v.dirstate.rows.remove_entry(&path))
        .collect();
    if !opts.dry_run {
        v.dirstate.save(&v.root)?;
    }
    for (path, row) in rows {
        let file = v.root.join(&path);
        if !v.dirstate.matches(&row, &file) {
            v.event(opts, sum, "skipped", &path, "changed since it was scanned");
            continue;
        }
        if !opts.dry_run {
            fs::remove_file(&file)?;
            prune(&v.root, &file);
        }
        v.event(opts, sum, kind, &path, "");
    }
    Ok(())
}

/// Copy files from anywhere into the vault that has the folder `dest`. Each
/// source lands in `dest` under its own name, a folder with all its files; a
/// source with no name of its own (`.`) lands as its contents. A file the
/// vault lacks is added, one that differs is replaced, and nothing is
/// deleted. The copies are not recorded: they are edits for the next scan.
pub fn cp(opts: &Options, cwd: &Path, sources: &[PathBuf], dest: &Path) -> Result<Summary> {
    let dest = resolve_path(&cwd.join(dest))?;
    let mut v = Vault::open(opts, &dest, true)?;
    let into = relative(&v.root, &dest)?;
    let mut sum = Summary::default();
    announce(opts);
    // What is about to be replaced is recorded first, as `restore` does.
    record(opts, &mut v, None, &mut sum)?;

    let (mut files, mut odd, mut ignored) = (vec![], 0, 0);
    for src in sources {
        let name = match src.file_name() {
            Some(name) => name.to_str().context("a source name is not UTF-8")?,
            None => "",
        };
        collect(&cwd.join(src), under(&into, name), &mut files, &mut odd)?;
    }
    for (src, path) in files {
        if v.skips(&path) {
            ignored += 1;
            continue;
        }
        let cannot = || format!("cannot read {}", src.display());
        let ours = (
            hash_file(&src).with_context(cannot)?,
            is_exec(&src.metadata().with_context(cannot)?),
        );
        let file = v.root.join(&path);
        let theirs = file
            .symlink_metadata()
            .ok()
            .map(|meta| (hash_file(&file).unwrap_or_default(), is_exec(&meta)));
        let kind = match theirs {
            None => "added",
            Some(theirs) if theirs == ours => continue,
            Some(_) => "updated",
        };
        if !opts.dry_run {
            no_symlinked_parent(&v, &path)?;
            let dir = file.parent().context("no parent folder")?;
            fs::create_dir_all(dir)?;
            // By way of a scratch file, which a scan removes if this stops.
            let tmp = dir.join(format!("{TMP_PREFIX}{}", unique()));
            fs::copy(&src, &tmp).with_context(cannot)?;
            // A copy is an edit made now, whatever mtime the copy carried.
            File::open(&tmp)?.set_modified(SystemTime::now())?;
            fs::rename(&tmp, &file).with_context(|| format!("cannot write {}", file.display()))?;
        }
        v.event(opts, &mut sum, kind, &path, "");
    }
    if odd > 0 {
        warn(
            opts,
            format!("skipped {odd} symbolic links or special files"),
        );
    }
    if ignored > 0 {
        let root = v.root.display();
        warn(
            opts,
            format!("{root} ignores {ignored} files; they were not copied"),
        );
    }
    Ok(sum)
}

/// `name` under the vault folder `dir`; either may be empty.
fn under(dir: &str, name: &str) -> String {
    match (dir.is_empty(), name.is_empty()) {
        (false, false) => format!("{dir}/{name}"),
        _ => format!("{dir}{name}"),
    }
}

/// The regular files at or under `src`, each with the vault path it is
/// copied to, `to` being that of `src` itself. A `.syncit` folder is left
/// out: a vault's own records are not files to copy.
fn collect(
    src: &Path,
    to: String,
    files: &mut Vec<(PathBuf, String)>,
    odd: &mut usize,
) -> Result<()> {
    let cannot = || format!("cannot read {}", src.display());
    let meta = src.symlink_metadata().with_context(cannot)?;
    if meta.is_file() {
        files.push((src.to_path_buf(), to));
    } else if !meta.is_dir() {
        *odd += 1;
    } else {
        let mut items = fs::read_dir(src)
            .with_context(cannot)?
            .collect::<Result<Vec<_>, _>>()?;
        items.sort_by_key(|item| item.file_name());
        for item in items {
            let name = item.file_name();
            let name = name
                .to_str()
                .with_context(|| format!("a name in {} is not UTF-8", src.display()))?;
            if !name.eq_ignore_ascii_case(".syncit") {
                collect(&item.path(), under(&to, name), files, odd)?;
            }
        }
    }
    Ok(())
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
        print_stdout(&blame, opts);
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
                exec: e.exec,
                mtime: e.mtime,
                nicknames: v.log.authors[id].iter().cloned().collect(),
                current: winner.as_ref() == Some(id),
                available: e.hash.as_ref().is_some_and(|h| available(&v, None, h)),
            };
            print_stdout(&Message::Version(version.clone()), opts);
            found.push(version);
        }
    }
    Ok(found)
}

/// Bring back the version of `path` whose hash starts with `hash`: as a copy
/// beside the file, named as a conflict copy is, or in place of it. Returns
/// the vault-relative path that has the version afterwards.
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
    let present = v.dirstate.rows.get(&p).map(|row| row.hash.clone());
    let dest = if in_place {
        p.clone()
    } else {
        copy_name(&p, &h)
    };
    let already = |what: &str| {
        let message = format!("{dest} {what}");
        print_stdout(&Message::Info { message }, opts);
    };
    if in_place {
        if present.as_ref() == Some(&h) {
            already("is already that version");
            return Ok(dest);
        }
        // With no file here, a scan would take the restored bytes for an old
        // copy and the next sync would bring it forward again, unless the
        // path is deleted.
        let deleted = matches!(v.log.resolve(&p), PathState::Resolved { winner, .. }
            if v.log.entries[&winner].hash.is_none());
        ensure!(
            present.is_some() || deleted,
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
    // Executable if the newest entry with this content was.
    let newest = v.log.valid(&p).into_iter();
    let newest = newest
        .filter(|(_, e)| e.hash.as_ref() == Some(&h))
        .max_by_key(|(id, e)| (e.mtime, *id));
    let exec = newest.is_some_and(|(_, e)| e.exec);
    ensure!(
        write_now(&mut v, &dest, &h, exec)?,
        "the content of that version is not in this vault; see `syncit config keep-history`"
    );
    if in_place {
        record(opts, &mut v, None, &mut sum)?;
    }
    let kind = match (in_place, present) {
        (false, _) => "copied",
        (true, Some(_)) => "updated",
        (true, None) => "added",
    };
    v.event(opts, &mut sum, kind, &dest, "");
    Ok(dest)
}

/// A change `config` can make.
pub enum Setting {
    /// How many versions back to keep content for; `None` is all of them.
    KeepHistory(Option<u32>),
    Autocompact(bool),
    Nickname(String),
}

/// Print this vault's settings, after changing one if asked. Returns the
/// config as it now is.
pub fn config(
    opts: &Options,
    cwd: &Path,
    vault: Option<&Path>,
    change: Option<Setting>,
) -> Result<Config> {
    let mut v = open_vault(opts, cwd, vault)?;
    let changed = change.is_some();
    match change {
        Some(Setting::KeepHistory(depth)) => v.config.keep_history = depth,
        Some(Setting::Autocompact(on)) => v.config.autocompact = on,
        Some(Setting::Nickname(nickname)) => {
            ensure!(
                valid_nickname(&nickname),
                "a nickname is 1 to 64 bytes, with no spaces or control characters"
            );
            v.config.nickname = nickname;
        }
        None => {}
    }
    v.storage.save_config(&v.config)?;
    let depth = v.config.keep_history;
    let auto = v.config.autocompact;
    let settings = [
        (
            "keep-history",
            depth.map_or("all".to_string(), |n| n.to_string()),
        ),
        ("autocompact", if auto { "on" } else { "off" }.to_string()),
        ("nickname", v.config.nickname.clone()),
    ];
    for (name, value) in settings {
        print_stdout(&Message::Setting { name, value }, opts);
    }
    // Say what a change is about to cost, at the moment it is made.
    let keep = v.log.keep(v.config.keep_history, |_| true);
    let beyond = v.storage.drop_blobs_except(&keep, true)?.0;
    if changed && beyond > 0 {
        let by = match auto {
            true => "the next sync or snapshot drops",
            false => "`syncit compact` drops",
        };
        let message = format!(
            "keep-history no longer covers {} stored here; {by} what it does not cover",
            versions(beyond)
        );
        print_stdout(&Message::Info { message }, opts);
    }
    Ok(v.config)
}
