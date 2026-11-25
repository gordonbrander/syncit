//! One function per command. Each prints as it goes; a command that runs a
//! sync returns a `Summary` for `main` to print last.

use crate::backend::{Backend, Fs};
use crate::blobs::{self, NO_PEER, available, hash_file, retain};
use crate::config::{Config, default_nickname, prefix_matches};
use crate::dirstate::is_exec;
use crate::entry::valid_path;
use crate::log::PathState;
use crate::materialise::{copy_name, no_symlinked_parent, prune, write_now};
use crate::migrate::{self, VERSION};
use crate::output::{
    Context, Event, Message, State, Summary, Traffic, Version, print_states, print_stdout, warn,
};
use crate::scan::Scan;
use crate::segment::{self, valid_nickname};
use crate::storage::Store;
use crate::vault::{
    Bare, Dir, TMP_PREFIX, Target, Vault, Worktree, find_root, relative, resolve_path, unique,
};
use anyhow::{Context as _, Result, bail, ensure};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The vault a command runs in.
type Local = Vault<Fs, Dir>;

/// A remote vault.
#[cfg(feature = "remote")]
type Remote = Vault<crate::sealed::Sealed<crate::remote::Bucket>, Bare>;
/// Never made: without the `remote` feature, opening one is an error.
#[cfg(not(feature = "remote"))]
type Remote = Vault<Fs, Bare>;

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
    Store::create(&dir.display().to_string(), Fs::at(dir), &config)?;
    Ok(config)
}

pub fn init(ctx: &Context, dir: &Path) -> Result<()> {
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
    print_stdout(ctx, &Message::Info { message });
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
    ctx: &Context,
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
    let bucket = Bucket::new(url, credentials(ctx, url)?)?;
    ensure!(bucket.is_empty()?, "{url} is not empty");
    let (key, generated) = match &ctx.secret_key {
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
    let format = crate::format::Format {
        check: Some(crypt::check(&key)),
        syncit: VERSION,
    };
    format.write(&bucket)?;
    let sealed = crate::sealed::Sealed::new(bucket, key.clone());
    Store::create(url, sealed, &config)?;
    let message = format!("Vault initialized: {url}\nNickname: {}", config.nickname);
    print_stdout(ctx, &Message::Info { message });
    if !generated {
        return Ok(None);
    }
    let words = keys::phrase(&key);
    let message = format!(
        "Secret key for {url}:\n\n  {}\n\nWrite these words down and keep them safe. They are the only way to\nread this vault from another computer, and they are not shown again.",
        *words
    );
    print_stdout(ctx, &Message::Info { message });
    keys::save(ctx, url, &key, keys::Source::New, save_key)?;
    Ok(Some(words.to_string()))
}

#[cfg(not(feature = "remote"))]
pub fn init_remote(
    _opts: &Context,
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
    ctx: &Context,
    cwd: &Path,
    src: &Path,
    dir: &Path,
    only: &[String],
    save_key: bool,
) -> Result<Summary> {
    vault_paths(only)?;
    let local = Vault::open(ctx, cwd, true).ok();
    let src = Target::parse(src, local.as_ref().map(|v| &v.config.remotes));
    std::mem::drop(local); // it may be the source, which is locked
    let (source, include) = match src {
        Target::Url(url) if only.is_empty() => (PathBuf::from(url), vec![String::new()]),
        Target::Url(url) => (PathBuf::from(url), only.to_vec()),
        Target::Dir(src) => {
            let src = resolve_path(&src)?;
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
        Vault::open(ctx, dir, true).and_then(|mut v| add(ctx, &mut v, location, None, save_key));
    if let Err(e) = added {
        let _ = fs::remove_dir_all(dir.join(".syncit"));
        return Err(e);
    }
    run(ctx, dir, &source, Mode::Pull)
}

pub fn sync(ctx: &Context, cwd: &Path, peer: &Path) -> Result<Summary> {
    run(ctx, cwd, peer, Mode::Sync)
}

pub fn push(ctx: &Context, cwd: &Path, peer: &Path) -> Result<Summary> {
    run(ctx, cwd, peer, Mode::Push)
}

pub fn pull(ctx: &Context, cwd: &Path, peer: &Path) -> Result<Summary> {
    run(ctx, cwd, peer, Mode::Pull)
}

/// Whether the folders are the same or one contains the other. Such vaults
/// share files: a run between them would copy the outer into the inner,
/// one level deeper each time.
fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Open the remote vault at `url`, for a command run in `local` if there is
/// one. A remote is not locked: object storage has no lock.
#[cfg(feature = "remote")]
fn open_url(ctx: &Context, local: Option<&Local>, url: &str) -> Result<Remote> {
    let backend = crate::remote::backend(ctx, url)?;
    Vault::bare(ctx, url, backend, local.map(|v| &v.store))
}

#[cfg(not(feature = "remote"))]
fn open_url(_: &Context, _: Option<&Local>, _: &str) -> Result<Remote> {
    Err(no_remote())
}

/// Open the vault that has `dir`, for a run with `v`.
fn open_dir(ctx: &Context, v: &Local, dir: &Path) -> Result<Local> {
    ensure!(
        dir.exists(),
        "no vault at {}: not a folder, a URL or the name of a remote",
        dir.display()
    );
    ensure!(
        !nested(&find_root(dir)?, &v.worktree.root),
        "cannot sync a vault with itself or with a vault inside it"
    );
    Vault::open(ctx, dir, false)
}

/// The context of a run with two vaults, `ours` being the one it runs in.
fn sided(ctx: &Context, ours: &Local) -> Context {
    Context {
        ours: Some(ours.name.clone()),
        ..ctx.clone()
    }
}

/// After a run, for a vault that is bare: what was sent to it and received
/// from it.
fn report<W: Worktree>(ctx: &Context, sum: &Summary, v: &Vault<impl Backend, W>) {
    if W::BARE {
        let t = &sum.traffic;
        let lines = [
            ("send", "sent", "to", &t.sent_files, t.sent_segments),
            (
                "receive",
                "received",
                "from",
                &t.received_files,
                t.received_segments,
            ),
        ];
        for (verb, done, to, files, segments) in lines {
            if files.is_empty() && segments == 0 {
                continue;
            }
            let verb = if ctx.dry_run {
                format!("would {verb}")
            } else {
                done.into()
            };
            let (f, s) = (files.len(), segments);
            let message = format!("{verb} {f} files, {s} segments {to} {}", v.name);
            print_stdout(ctx, &Message::Info { message });
        }
    }
}

/// A run of the vault at `cwd` with the vault `peer` names.
fn run(ctx: &Context, cwd: &Path, peer: &Path, mode: Mode) -> Result<Summary> {
    let mut a = Vault::open(ctx, cwd, true)?;
    match Target::parse(peer, Some(&a.config.remotes)) {
        Target::Dir(dir) => {
            let b = open_dir(ctx, &a, &dir)?;
            run_with(ctx, &mut a, b, mode)
        }
        Target::Url(url) => {
            let b = open_url(ctx, Some(&a), &url)?;
            run_with(ctx, &mut a, b, mode)
        }
    }
}

/// A run with a peer (SPEC.md section 10). `a` is the local vault.
fn run_with(
    ctx: &Context,
    a: &mut Local,
    mut b: Vault<impl Backend, impl Worktree>,
    mode: Mode,
) -> Result<Summary> {
    let ctx = &sided(ctx, a);
    let mut sum = Summary::default();
    announce(ctx);
    record(ctx, a, Some(&b), &mut sum)?;
    record(ctx, &mut b, Some(&*a), &mut sum)?;
    exchange(ctx, a, &mut b, mode, &mut sum)?;
    autocompact(ctx, a)?;
    autocompact(ctx, &b)?;
    report(ctx, &sum, a);
    report(ctx, &sum, &b);
    Ok(sum)
}

/// At the end of a run, drop the stored versions the vault's `keep_history`
/// no longer covers, if it compacts by itself. After materialise and never
/// before, so that a run cannot drop a version it is about to read.
fn autocompact(ctx: &Context, v: &Vault<impl Backend, impl Worktree>) -> Result<()> {
    if v.config.autocompact {
        blobs::compact(ctx, v)?;
    }
    Ok(())
}

/// Say that this is a dry run, if it is.
fn announce(ctx: &Context) {
    if ctx.dry_run {
        let message = "dry run: nothing is changed".to_string();
        print_stdout(ctx, &Message::Info { message });
    }
}

/// The files `from` has that `to` excludes.
pub fn excluded(
    from: &Vault<impl Backend, impl Worktree>,
    to: &Vault<impl Backend, impl Worktree>,
) -> Vec<String> {
    let excluded = |p: &&String| !to.config.includes(p) && !to.skips(p);
    from.worktree
        .rows()
        .keys()
        .filter(excluded)
        .cloned()
        .collect()
}

/// The files `from` has that `to` ignores.
pub fn ignored(
    from: &Vault<impl Backend, impl Worktree>,
    to: &Vault<impl Backend, impl Worktree>,
) -> Vec<String> {
    let ignored = |p: &&String| to.skips(p);
    from.worktree
        .rows()
        .keys()
        .filter(ignored)
        .cloned()
        .collect()
}

/// The part of a run after both vaults are scanned.
fn exchange<WA: Worktree, WB: Worktree>(
    ctx: &Context,
    a: &mut Vault<impl Backend, WA>,
    b: &mut Vault<impl Backend, WB>,
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
            ctx,
            format!(
                "{} {verb} {} files ({}); they were not copied",
                b.name,
                files.len(),
                names.join(", ")
            ),
        );
    }
    // The size of each log when its vault last retained, which was in
    // `record`. A log only grows, so the size says whether it has changed.
    let (mut retained_a, mut retained_b) = (a.log.entries.len(), b.log.entries.len());
    if to_b {
        receive(ctx, b, a, sum)?;
    }
    if to_a {
        receive(ctx, a, b, sum)?;
    }
    if ctx.no_remove {
        if to_b && keep(ctx, b, a, sum)? {
            retained_b = b.log.entries.len();
        }
        if to_a && keep(ctx, a, b, sum)? {
            retained_a = a.log.entries.len();
        }
        // Each receiver takes the other's re-creations, so one run converges.
        // ponytail: a corrupt segment is reported as rejected a second time
        // here; have `receive` remember what it rejected if that matters.
        if to_b {
            receive(ctx, b, a, sum)?;
        }
        if to_a {
            receive(ctx, a, b, sum)?;
        }
    }
    // Both retain before either materialises, so that a version is saved
    // before it is replaced. A vault whose log is as it was when it last
    // retained has nothing new to keep.
    if to_b && b.log.entries.len() != retained_b {
        retain(ctx, b, Some(&*a), sum)?;
    }
    if to_a && a.log.entries.len() != retained_a {
        retain(ctx, a, Some(&*b), sum)?;
    }
    // A merge replaces two versions. It is made only if a vault receiving in
    // this run keeps history: its `retain` above then has them, so they can
    // be restored. The setting, not the blobs, so a dry run decides alike.
    let kept =
        (to_a && a.config.keep_history != Some(0)) || (to_b && b.config.keep_history != Some(0));
    if to_b {
        WB::materialise(ctx, b, Some(&*a), kept, sum)?;
    }
    if to_a {
        WA::materialise(ctx, a, Some(&*b), kept, sum)?;
    }
    // A conflict open in both vaults is reported once, for the local one.
    let mut seen = BTreeSet::new();
    if to_a {
        conflicts(ctx, a, &mut seen);
    }
    if to_b {
        conflicts(ctx, b, &mut seen);
    }
    sum.conflicts += seen.len();
    Ok(())
}

/// `--no-remove`: keep the files here whose paths the log says are deleted,
/// and record them as re-created. Returns whether it kept any, and so
/// recorded and retained.
fn keep<W: Worktree>(
    ctx: &Context,
    v: &mut Vault<impl Backend, W>,
    peer: &Vault<impl Backend, impl Worktree>,
    sum: &mut Summary,
) -> Result<bool> {
    let kept = W::unremove(ctx, v, sum);
    if kept {
        record(ctx, v, Some(peer), sum)?;
    }
    Ok(kept)
}

/// Scan, then retain blobs for what the scan recorded.
fn record<W: Worktree>(
    ctx: &Context,
    v: &mut Vault<impl Backend, W>,
    peer: Option<&Vault<impl Backend, impl Worktree>>,
    sum: &mut Summary,
) -> Result<()> {
    let scanned = W::scan(ctx, v, ctx.dry_run)?;
    sum.recorded += scanned.entries.len();
    for path in &scanned.skipped {
        v.event(ctx, sum, "skipped", path, "changed while it was being read");
    }
    retain(ctx, v, peer, sum)
}

/// Take every segment the other vault has and this one lacks. A segment is
/// validated before it is stored; one that fails is reported and left out.
pub fn receive<WV: Worktree, WF: Worktree>(
    ctx: &Context,
    v: &mut Vault<impl Backend, WV>,
    from: &Vault<impl Backend, WF>,
    sum: &mut Summary,
) -> Result<()> {
    let have = v.store.segment_names()?;
    for name in from.store.segment_names()? {
        if have.contains(&name) {
            continue;
        }
        let read = match from.log.fetched.get(&name) {
            Some(bytes) => Ok(bytes.clone()),
            None => from.store.read_segment(&name),
        };
        match read.and_then(|bytes| Ok((segment::decode(&name, &bytes)?, bytes))) {
            Ok((segment, bytes)) => {
                if !ctx.dry_run {
                    v.store.write_segment(&name, &bytes)?;
                }
                if WV::BARE {
                    sum.traffic.sent_segments += 1;
                }
                if WF::BARE {
                    sum.traffic.received_segments += 1;
                }
                for (id, e) in segment.entries {
                    v.log.insert(id, e, &segment.nickname);
                }
            }
            Err(e) => v.event(ctx, sum, "rejected", "", &format!("segment {name}: {e:#}")),
        }
    }
    // What a dry scan of `from` recorded is in its memory, not in a segment.
    // A real run sends it as the one segment that scan writes.
    if ctx.dry_run && absorb(v, from) && WV::BARE {
        sum.traffic.sent_segments += 1;
    }
    Ok(())
}

/// Add every entry of `from`'s log to `v`'s, in memory only. Whether any
/// was new to `v`.
fn absorb(
    v: &mut Vault<impl Backend, impl Worktree>,
    from: &Vault<impl Backend, impl Worktree>,
) -> bool {
    let mut new = false;
    for (id, e) in &from.log.entries {
        new |= !v.log.entries.contains_key(id);
        for nickname in &from.log.authors[id] {
            v.log.insert(id.clone(), e.clone(), nickname);
        }
    }
    new
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
fn conflicts(ctx: &Context, v: &Vault<impl Backend, impl Worktree>, seen: &mut BTreeSet<String>) {
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
            side: ctx.side(&v.name),
        };
        print_stdout(ctx, &conflict);
    }
}

pub fn snapshot(ctx: &Context, cwd: &Path) -> Result<Summary> {
    let mut v = Vault::open(ctx, cwd, true)?;
    let mut sum = Summary::default();
    record(ctx, &mut v, NO_PEER, &mut sum)?;
    autocompact(ctx, &v)?;
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
pub fn compact(ctx: &Context, cwd: &Path, vault: Option<&Path>) -> Result<(usize, u64)> {
    let Some(target) = vault else {
        return compact_in(ctx, &Vault::open(ctx, cwd, true)?);
    };
    // A remote needs no local vault.
    let local = Vault::open(ctx, cwd, true).ok();
    match Target::parse(target, local.as_ref().map(|v| &v.config.remotes)) {
        Target::Url(url) => compact_in(ctx, &open_url(ctx, local.as_ref(), &url)?),
        Target::Dir(dir) => {
            std::mem::drop(local); // it may be the target, which is locked
            compact_in(ctx, &Vault::open(ctx, &dir, true)?)
        }
    }
}

fn compact_in(ctx: &Context, v: &Vault<impl Backend, impl Worktree>) -> Result<(usize, u64)> {
    announce(ctx);
    let (n, bytes) = blobs::compact(ctx, v)?;
    let message = match n {
        0 => "nothing to drop".to_string(),
        _ => format!("dropped {} ({})", versions(n), size(bytes)),
    };
    print_stdout(ctx, &Message::Info { message });
    Ok((n, bytes))
}

/// A vault's state for a path (SPEC.md section 11, `status`); the first rule
/// that applies decides. `deleted` is a recorded removal, which `status`
/// prints as `removed` too, and `leftover` a file whose removal is known,
/// which it prints as `behind` or `incomplete`. `winner` is the hash of the path's current version,
/// None in it for a deletion, and None if the path's history is incomplete.
/// `changes` is what a dry scan of the vault found.
fn state(
    v: &Vault<impl Backend, impl Worktree>,
    path: &str,
    winner: Option<Option<&String>>,
    conflict: bool,
    changes: &BTreeMap<String, &'static str>,
) -> &'static str {
    let (row, change) = (v.worktree.rows().get(path), changes.get(path));
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
/// Records nothing; it saves only what its scans learned of files that have
/// not changed (`Scan::cache`). Returns what it printed.
pub fn status(
    ctx: &Context,
    cwd: &Path,
    paths: &[PathBuf],
    peer: Option<&Path>,
    all: bool,
) -> Result<Vec<State>> {
    let ctx = &Context {
        dry_run: true,
        no_remove: false,
        ..ctx.clone()
    };
    let mut v = Vault::open(ctx, cwd, true)?;
    let mut scopes = prefixes(&v.worktree.root, cwd, paths)
        .context("paths are in this vault; to compare with another, add `--vs <vault>`")?;
    if scopes.is_empty() {
        scopes.push(String::new());
    }
    match peer.map(|peer| Target::parse(peer, Some(&v.config.remotes))) {
        None => status_with(ctx, &mut v, None::<&mut Local>, scopes, all),
        Some(Target::Dir(dir)) => {
            let mut peer = open_dir(ctx, &v, &dir)?;
            status_with(ctx, &mut v, Some(&mut peer), scopes, all)
        }
        Some(Target::Url(url)) => {
            let mut peer = open_url(ctx, Some(&v), &url)?;
            status_with(ctx, &mut v, Some(&mut peer), scopes, all)
        }
    }
}

/// A dry scan, which says what no scan has recorded. The vault was just
/// opened, so the scan began with the dirstate on disk and may save what it
/// learned. The save is a cache write and may fail, as on read-only media,
/// without failing `status`.
fn dry_scan<W: Worktree>(ctx: &Context, v: &mut Vault<impl Backend, W>) -> Result<Scan> {
    let scanned = W::scan(ctx, v, true)?;
    if let (Some(cache), Some(root)) = (&scanned.cache, v.worktree.root()) {
        let _ = cache.save(root);
    }
    Ok(scanned)
}

fn status_with(
    ctx: &Context,
    v: &mut Local,
    mut peer: Option<&mut Vault<impl Backend, impl Worktree>>,
    scopes: Vec<String>,
    all: bool,
) -> Result<Vec<State>> {
    // Dry scans; then one log for both vaults, so that each is compared with
    // the same winner.
    let scanned = dry_scan(ctx, v)?;
    let ours: BTreeMap<_, _> = scanned.changes.into_iter().collect();
    let mut theirs = BTreeMap::new();
    if let Some(peer) = &mut peer {
        theirs = dry_scan(ctx, *peer)?.changes.into_iter().collect();
        absorb(v, peer);
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
        let conflict = copies.is_some();
        let a = state(v, path, winner, conflict, &ours);
        let b = (peer.as_deref()).map(|peer| state(peer, path, winner, conflict, &theirs));
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
    print_states(ctx, &found);
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
pub fn get(ctx: &Context, cwd: &Path, paths: &[PathBuf]) -> Result<Summary> {
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
        let mut v = Vault::open(ctx, cwd, true)?;
        ensure!(
            !nested(&source, &v.worktree.root),
            "{} is in this vault: give a path in the vault to get it from",
            first.display()
        );
        include(&mut v, prefixes(&source, cwd, paths)?)?;
    }
    run(ctx, cwd, &source, Mode::Pull)
}

/// Include `prefixes` in the vault, and save its config.
fn include(v: &mut Vault<impl Backend, impl Worktree>, prefixes: Vec<String>) -> Result<()> {
    for prefix in prefixes {
        v.config.exclude.retain(|x| !prefix_matches(&prefix, x));
        v.config.include.retain(|x| !prefix_matches(&prefix, x));
        v.config.include.push(prefix);
    }
    v.store.save_config(&v.config)
}

/// `get --from`: add paths of the vault `from`, a target, each given from
/// that vault's root. This is the form that reaches a remote vault.
pub fn get_from(ctx: &Context, cwd: &Path, paths: &[String], from: &Path) -> Result<Summary> {
    ensure!(!paths.is_empty(), "no path given");
    vault_paths(paths)?;
    {
        let mut v = Vault::open(ctx, cwd, true)?;
        if let Target::Dir(dir) = Target::parse(from, Some(&v.config.remotes)) {
            ensure!(
                !nested(&find_root(&dir)?, &v.worktree.root),
                "cannot get files from this vault or a vault inside it"
            );
        }
        include(&mut v, paths.to_vec())?;
    }
    run(ctx, cwd, from, Mode::Pull)
}

/// The store of the vault at or above `cwd`, which a command runs in:
/// locked, and brought up to this build's layout if it is behind. For a
/// command that needs the vault's settings and not its log.
fn own_store(ctx: &Context, cwd: &Path) -> Result<Store<Fs>> {
    let fs = Fs::open(cwd)?;
    let name = fs.root().display().to_string();
    migrate::upgrade(ctx, &fs, &name)?;
    Store::open(&name, fs)
}

/// Bring a vault up to this build's layout: the one `vault` names, or the
/// one here. Any command does this for the vault it runs in; this is for a
/// vault that others use too, a remote or a folder on a drive, which no
/// other command changes the layout of. Returns the version it was at.
pub fn migrate(ctx: &Context, cwd: &Path, vault: Option<&Path>) -> Result<u32> {
    let done = |name: &str, from: u32| {
        let message = match from {
            VERSION => format!("{name} is at format version {VERSION}"),
            _ => format!("migrated {name} from format version {from} to {VERSION}"),
        };
        print_stdout(ctx, &Message::Info { message });
        from
    };
    let dir = |start: &Path| -> Result<u32> {
        let fs = Fs::open(start)?;
        let name = fs.root().display().to_string();
        Ok(done(&name, migrate::migrate(&fs, &name)?))
    };
    let Some(target) = vault else {
        return dir(cwd);
    };
    // A nickname is one of the vault here, if there is a vault here.
    let remotes = own_store(ctx, cwd).and_then(|store| Ok(store.load_config()?.remotes));
    match Target::parse(target, remotes.as_ref().ok()) {
        Target::Dir(start) => dir(&start),
        Target::Url(url) => Ok(done(&url, migrate_url(ctx, &url)?)),
    }
}

#[cfg(feature = "remote")]
fn migrate_url(ctx: &Context, url: &str) -> Result<u32> {
    migrate::migrate(&crate::remote::backend(ctx, url)?, url)
}

#[cfg(not(feature = "remote"))]
fn migrate_url(_: &Context, _: &str) -> Result<u32> {
    Err(no_remote())
}

/// List the remotes this vault remembers. Returns them, as printed.
pub fn remotes(ctx: &Context, cwd: &Path) -> Result<Vec<(String, String)>> {
    let config = own_store(ctx, cwd)?.load_config()?;
    for (nickname, location) in &config.remotes {
        let message = Message::Remote {
            nickname: nickname.clone(),
            location: location.clone(),
        };
        print_stdout(ctx, &message);
    }
    Ok(config.remotes.into_iter().collect())
}

/// Remember the vault at `location`, a URL or a path, under `nickname` or its
/// own nickname. Returns the nickname.
pub fn remote_add(
    ctx: &Context,
    cwd: &Path,
    location: &str,
    nickname: Option<String>,
    save_key: bool,
) -> Result<String> {
    let mut v = Vault::open(ctx, cwd, true)?;
    add(ctx, &mut v, location, nickname, save_key)
}

fn add(
    ctx: &Context,
    v: &mut Local,
    location: &str,
    nickname: Option<String>,
    save_key: bool,
) -> Result<String> {
    let (location, config, found) = if location.contains("://") {
        let (config, found) = find_remote(ctx, location)?;
        (location.to_string(), config, Some(found))
    } else {
        let root = resolve_path(Path::new(location))?;
        ensure!(
            root.join(".syncit").is_dir(),
            "{} is not a vault",
            root.display()
        );
        ensure!(
            !nested(&root, &v.worktree.root),
            "a vault cannot be its own remote"
        );
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
        save_found(ctx, &location, found, save_key)?;
    }
    v.config.remotes.insert(nickname.clone(), location.clone());
    v.store.save_config(&v.config)?;
    let message = format!("remote {nickname}: {location}");
    print_stdout(ctx, &Message::Info { message });
    Ok(nickname)
}

/// Forget a remote. Its keychain item and `secrets.toml` entry stay, since
/// other vaults on this machine may use them.
pub fn remote_remove(ctx: &Context, cwd: &Path, nickname: &str) -> Result<()> {
    let store = own_store(ctx, cwd)?;
    let mut config = store.load_config()?;
    let location = config.remotes.remove(nickname);
    let location = location.with_context(|| format!("no remote nicknamed {nickname}"))?;
    store.save_config(&config)?;
    let message = format!("removed remote {nickname}: {location}");
    print_stdout(ctx, &Message::Info { message });
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
fn find_remote(ctx: &Context, url: &str) -> Result<(Config, Found)> {
    use crate::keys::{self, Source};
    use std::io::IsTerminal;
    if let Some(phrase) = &ctx.secret_key {
        keys::parse_phrase(phrase).context("SYNCIT_SECRET_KEY")?;
    }
    let settings = crate::remote::credentials(ctx, url)?;
    let bucket = crate::remote::Bucket::new(url, settings)?;
    let check = crate::sealed::check(&bucket, url)?;
    let found = match keys::find(ctx, url, &check) {
        Ok(found) => found,
        Err(e) if !std::io::stdin().is_terminal() => return Err(e),
        Err(_) => (keys::prompt(url, &check)?, Source::New),
    };
    let sealed = crate::sealed::Sealed::new(bucket, found.0.clone());
    let config = Store::open(url, sealed)?.load_config()?;
    Ok((config, found))
}

#[cfg(not(feature = "remote"))]
fn find_remote(_opts: &Context, _url: &str) -> Result<(Config, Found)> {
    Err(no_remote())
}

#[cfg(feature = "remote")]
fn save_found(ctx: &Context, url: &str, found: Found, save: bool) -> Result<()> {
    crate::keys::save(ctx, url, &found.0, found.1, save)
}

#[cfg(not(feature = "remote"))]
fn save_found(_: &Context, _: &str, _: Found, _: bool) -> Result<()> {
    Ok(())
}

/// Move files to another vault: push to it, confirm each file arrived, then
/// exclude them here. Nothing is removed unless every file is confirmed.
pub fn move_to(ctx: &Context, cwd: &Path, paths: &[PathBuf], target: &Path) -> Result<Summary> {
    let mut v = Vault::open(ctx, cwd, true)?;
    let prefixes = prefixes(&v.worktree.root, cwd, paths)?;
    match Target::parse(target, Some(&v.config.remotes)) {
        Target::Dir(dir) => {
            let t = open_dir(ctx, &v, &dir)?;
            move_with(ctx, &mut v, prefixes, t)
        }
        Target::Url(url) => {
            let t = open_url(ctx, Some(&v), &url)?;
            move_with(ctx, &mut v, prefixes, t)
        }
    }
}

fn move_with(
    ctx: &Context,
    v: &mut Local,
    prefixes: Vec<String>,
    mut t: Vault<impl Backend, impl Worktree>,
) -> Result<Summary> {
    let ctx = &sided(ctx, v);
    let mut sum = Summary::default();
    announce(ctx);
    record(ctx, v, Some(&t), &mut sum)?;
    record(ctx, &mut t, Some(&*v), &mut sum)?;

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let present: Vec<String> = v
        .worktree
        .dirstate
        .rows
        .keys()
        .filter(under)
        .cloned()
        .collect();
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

    exchange(ctx, v, &mut t, Mode::Push, &mut sum)?;
    let arrived = |p: &&String| {
        t.worktree.rows().get(*p).map(|row| &row.hash) == Some(&v.worktree.dirstate.rows[*p].hash)
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

    remove_here(ctx, v, prefixes, present, "moved", &mut sum)?;
    report(ctx, &sum, &t);
    Ok(sum)
}

/// Stop having files here without sending them anywhere: exclude the paths
/// and remove their files from this vault only. Unless `force`, every file's
/// content must be in this vault's history or in the vault named by `check`.
pub fn drop(
    ctx: &Context,
    cwd: &Path,
    paths: &[PathBuf],
    check: Option<&Path>,
    force: bool,
) -> Result<Summary> {
    let mut v = Vault::open(ctx, cwd, true)?;
    let prefixes = prefixes(&v.worktree.root, cwd, paths)?;
    match check.map(|check| Target::parse(check, Some(&v.config.remotes))) {
        None => drop_with(ctx, &mut v, prefixes, None::<Local>, force),
        Some(Target::Dir(dir)) => {
            let check = open_dir(ctx, &v, &dir)?;
            drop_with(ctx, &mut v, prefixes, Some(check), force)
        }
        Some(Target::Url(url)) => {
            let check = open_url(ctx, Some(&v), &url)?;
            drop_with(ctx, &mut v, prefixes, Some(check), force)
        }
    }
}

fn drop_with(
    ctx: &Context,
    v: &mut Local,
    prefixes: Vec<String>,
    mut check: Option<Vault<impl Backend, impl Worktree>>,
    force: bool,
) -> Result<Summary> {
    let ctx = &match check {
        Some(_) => sided(ctx, v),
        None => ctx.clone(),
    };
    let mut sum = Summary::default();
    announce(ctx);
    // Unrecorded changes are recorded before anything is removed, and the
    // checked vault is scanned so that its rows say what it has now.
    record(ctx, v, check.as_ref(), &mut sum)?;
    if let Some(c) = &mut check {
        record(ctx, c, Some(&*v), &mut sum)?;
    }

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let present: Vec<String> = v
        .worktree
        .dirstate
        .rows
        .keys()
        .filter(under)
        .cloned()
        .collect();
    if !force {
        let elsewhere = |hash: &String| {
            v.store.has_blob(hash)
                || check.as_ref().is_some_and(|c| {
                    c.worktree.rows().values().any(|row| &row.hash == hash)
                        || c.store.has_blob(hash)
                })
        };
        let sole: Vec<&str> = present
            .iter()
            .filter(|p| !elsewhere(&v.worktree.dirstate.rows[*p].hash))
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
    remove_here(ctx, v, prefixes, present, "dropped", &mut sum)?;
    Ok(sum)
}

/// Exclude `prefixes` and remove the `present` files under them from this
/// vault only, reporting each as `kind`. No deletion is recorded.
fn remove_here(
    ctx: &Context,
    v: &mut Local,
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
    if !ctx.dry_run {
        v.store.save_config(&v.config)?;
    }
    // Rows go before files: a file with no row gets one again at the next
    // scan, but a row with no file would be recorded as a deletion.
    let rows: Vec<_> = present
        .into_iter()
        .filter_map(|path| v.worktree.dirstate.rows.remove_entry(&path))
        .collect();
    if !ctx.dry_run {
        v.worktree.dirstate.save(&v.worktree.root)?;
    }
    for (path, row) in rows {
        let file = v.worktree.root.join(&path);
        if !v.worktree.dirstate.matches(&row, &file) {
            v.event(ctx, sum, "skipped", &path, "changed since it was scanned");
            continue;
        }
        if !ctx.dry_run {
            fs::remove_file(&file)?;
            prune(&v.worktree.root, &file);
        }
        v.event(ctx, sum, kind, &path, "");
    }
    Ok(())
}

/// Copy files from anywhere into the vault that has the folder `dest`. Each
/// source lands in `dest` under its own name, a folder with all its files; a
/// source with no name of its own (`.`) lands as its contents. A file the
/// vault lacks is added, one that differs is replaced, and nothing is
/// deleted. The copies are not recorded: they are edits for the next scan.
pub fn cp(ctx: &Context, cwd: &Path, sources: &[PathBuf], dest: &Path) -> Result<Summary> {
    let dest = resolve_path(&cwd.join(dest))?;
    let mut v = Vault::open(ctx, &dest, true)?;
    let into = relative(&v.worktree.root, &dest)?;
    let mut sum = Summary::default();
    announce(ctx);
    // What is about to be replaced is recorded first, as `restore` does.
    record(ctx, &mut v, NO_PEER, &mut sum)?;

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
        let file = v.worktree.root.join(&path);
        let theirs = file
            .symlink_metadata()
            .ok()
            .map(|meta| (hash_file(&file).unwrap_or_default(), is_exec(&meta)));
        let kind = match theirs {
            None => "added",
            Some(theirs) if theirs == ours => continue,
            Some(_) => "updated",
        };
        if !ctx.dry_run {
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
        v.event(ctx, &mut sum, kind, &path, "");
    }
    if odd > 0 {
        warn(
            ctx,
            format!("skipped {odd} symbolic links or special files"),
        );
    }
    if ignored > 0 {
        let root = v.worktree.root.display();
        warn(
            ctx,
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
pub fn blame(ctx: &Context, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<(String, Vec<String>)>> {
    let v = Vault::open(ctx, cwd, true)?;
    let mut prefixes = prefixes(&v.worktree.root, cwd, paths)?;
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
        print_stdout(ctx, &blame);
        found.push((path.clone(), nicknames));
    }
    Ok(found)
}

/// The recorded history of each file under `paths`, or of every file if
/// there are none: per path, newest first by mtime. Reads the log only.
/// Returns what it printed.
pub fn log(ctx: &Context, cwd: &Path, paths: &[PathBuf]) -> Result<Vec<Version>> {
    let v = Vault::open(ctx, cwd, true)?;
    let mut prefixes = prefixes(&v.worktree.root, cwd, paths)?;
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
                available: e
                    .hash
                    .as_ref()
                    .is_some_and(|h| available(&v, NO_PEER, h, &mut Traffic::default())),
            };
            print_stdout(ctx, &Message::Version(version.clone()));
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
    ctx: &Context,
    cwd: &Path,
    path: &Path,
    hash: &str,
    in_place: bool,
) -> Result<String> {
    let mut v = Vault::open(ctx, cwd, true)?;
    let p = relative(&v.worktree.root, &resolve_path(&cwd.join(path))?)?;
    let mut sum = Summary::default();
    if in_place {
        record(ctx, &mut v, NO_PEER, &mut sum)?;
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
    let present = v.worktree.dirstate.rows.get(&p).map(|row| row.hash.clone());
    let dest = if in_place {
        p.clone()
    } else {
        copy_name(&p, &h)
    };
    let already = |what: &str| {
        let message = format!("{dest} {what}");
        print_stdout(ctx, &Message::Info { message });
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
        let file = v.worktree.root.join(&dest);
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
        record(ctx, &mut v, NO_PEER, &mut sum)?;
    }
    let kind = match (in_place, present) {
        (false, _) => "copied",
        (true, Some(_)) => "updated",
        (true, None) => "added",
    };
    v.event(ctx, &mut sum, kind, &dest, "");
    Ok(dest)
}

/// A change `config` can make.
pub enum Setting {
    /// How many versions back to keep content for; `None` is all of them.
    KeepHistory(Option<u32>),
    Autocompact(bool),
    Nickname(String),
}

fn print_settings(ctx: &Context, config: &Config) {
    let depth = config.keep_history;
    let auto = if config.autocompact { "on" } else { "off" };
    let settings = [
        (
            "keep-history",
            depth.map_or("all".to_string(), |n| n.to_string()),
        ),
        ("autocompact", auto.to_string()),
        ("nickname", config.nickname.clone()),
    ];
    for (name, value) in settings {
        print_stdout(ctx, &Message::Setting { name, value });
    }
}

/// Print this vault's settings, after changing one if asked. Returns the
/// config as it now is.
pub fn config(
    ctx: &Context,
    cwd: &Path,
    vault: Option<&Path>,
    change: Option<Setting>,
) -> Result<Config> {
    let Some(target) = vault else {
        return match change {
            // To show the settings of the vault here, its config is enough.
            None => {
                let config = own_store(ctx, cwd)?.load_config()?;
                print_settings(ctx, &config);
                Ok(config)
            }
            Some(change) => set(ctx, Vault::open(ctx, cwd, true)?, Some(change)),
        };
    };
    // A remote needs no local vault.
    let local = Vault::open(ctx, cwd, true).ok();
    match Target::parse(target, local.as_ref().map(|v| &v.config.remotes)) {
        Target::Url(url) => set(ctx, open_url(ctx, local.as_ref(), &url)?, change),
        Target::Dir(dir) => {
            std::mem::drop(local); // it may be the target, which is locked
            set(ctx, Vault::open(ctx, &dir, true)?, change)
        }
    }
}

/// Print a vault's settings, after making `change` if there is one.
fn set(
    ctx: &Context,
    mut v: Vault<impl Backend, impl Worktree>,
    change: Option<Setting>,
) -> Result<Config> {
    let Some(change) = change else {
        print_settings(ctx, &v.config);
        return Ok(v.config);
    };
    match change {
        Setting::KeepHistory(depth) => v.config.keep_history = depth,
        Setting::Autocompact(on) => v.config.autocompact = on,
        Setting::Nickname(nickname) => {
            ensure!(
                valid_nickname(&nickname),
                "a nickname is 1 to 64 bytes, with no spaces or control characters"
            );
            v.config.nickname = nickname;
        }
    }
    v.store.save_config(&v.config)?;
    print_settings(ctx, &v.config);
    let auto = v.config.autocompact;
    // Say what a change is about to cost, at the moment it is made.
    let keep = v.log.keep(v.config.keep_history, |_| true);
    let beyond = v.store.drop_blobs_except(&keep, true)?.0;
    if beyond > 0 {
        let by = match auto {
            true => "the next sync or snapshot drops",
            false => "`syncit compact` drops",
        };
        let message = format!(
            "keep-history no longer covers {} stored here; {by} what it does not cover",
            versions(beyond)
        );
        print_stdout(ctx, &Message::Info { message });
    }
    Ok(v.config)
}
