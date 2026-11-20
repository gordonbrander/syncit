//! One function per command. Each prints as it goes; a command that runs a
//! sync returns a `Summary` for `main` to print last.

use crate::blobs::retain;
use crate::config::{Config, prefix_matches};
use crate::entry::valid_path;
use crate::log::PathState;
use crate::materialise::{copy_name, materialise, prune};
use crate::output::{Event, Message, Options, Summary, print_stdout, warn};
use crate::scan::scan;
use crate::segment;
use crate::vault::{Vault, find_root, relative, resolve_path, write_atomic};
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Sync,
    Push,
    Pull,
}

fn create(dir: &Path, config: Config) -> Result<()> {
    for sub in ["log", "blobs", "tmp"] {
        fs::create_dir_all(dir.join(".syncit").join(sub))?;
    }
    File::create(dir.join(".syncit/lock"))?;
    config.save(dir)
}

pub fn init(opts: &Options, dir: &Path) -> Result<()> {
    ensure!(
        !dir.join(".syncit").exists(),
        "{} is already a vault",
        dir.display()
    );
    create(dir, Config::new(String::new()))?;
    let message = format!("initialised a vault in {}", resolve_path(dir)?.display());
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
    create(dir, Config::new(sub))?;
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
    record(opts, &mut a, Some(&b), &mut sum)?;
    record(opts, &mut b, Some(&a), &mut sum)?;
    exchange(opts, &mut a, &mut b, mode, &mut sum)?;
    Ok(sum)
}

/// The files `from` holds that `to` would not take.
pub fn unwanted(from: &Vault, to: &Vault) -> Vec<String> {
    let declined = |p: &&String| to.declines(p);
    from.cache.rows.keys().filter(declined).cloned().collect()
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
    let files = unwanted(a, b);
    if mode == Mode::Push && !files.is_empty() {
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
                "{} does not want {} files ({}); they were not copied",
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
        retain(b, Some(a))?;
    }
    if to_a {
        retain(a, Some(b))?;
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
    let scanned = scan(opts, v, false)?;
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
    retain(v, peer)
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
            Ok((entries, bytes)) => {
                write_atomic(&v.root, &dir.join(&name), &bytes)?;
                entries.into_iter().for_each(|e| v.log.add(e));
            }
            Err(e) => v.event(opts, sum, "rejected", "", &format!("segment {name}: {e:#}")),
        }
    }
    Ok(())
}

/// Print each open conflict among the paths this vault holds or wants that
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
            || !(v.cache.rows.contains_key(path) || v.config.wanted(path))
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
                let concerns = !v.declines(path) && !changed.contains(path);
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
/// wanted here, and a pull from that vault fetches them.
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
    record(opts, &mut v, Some(&t), &mut sum)?;
    record(opts, &mut t, Some(&v), &mut sum)?;

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let held: Vec<String> = v.cache.rows.keys().filter(under).cloned().collect();
    ensure!(
        !held.is_empty(),
        "nothing to move: no files here under the given paths"
    );
    let unwanted: Vec<&str> = held
        .iter()
        .filter(|p| t.declines(p))
        .map(String::as_str)
        .collect();
    ensure!(
        unwanted.is_empty(),
        "nothing moved: {} does not want {}",
        t.root.display(),
        unwanted.join(", ")
    );

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
    v.config.save(&v.root)?;
    // Rows go before files: a file with no row is held again after the next
    // scan, but a row with no file would be recorded as a deletion.
    let rows: Vec<_> = held
        .into_iter()
        .filter_map(|path| v.cache.rows.remove_entry(&path))
        .collect();
    v.cache.save(&v.root)?;
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
        fs::remove_file(&file)?;
        prune(&v.root, &file);
        v.event(opts, &mut sum, "moved", &path, "");
    }
    Ok(sum)
}

/// Print this vault's settings, after turning local history on or off if
/// asked. Returns whether local history is on.
pub fn config(opts: &Options, cwd: &Path, local_history: Option<bool>) -> Result<bool> {
    let mut v = Vault::open(opts, cwd, true)?;
    let was = v.config.keep_local_history;
    if let Some(on) = local_history {
        v.config.keep_local_history = on;
        v.config.save(&v.root)?;
    }
    let on = v.config.keep_local_history;
    let setting = Message::Setting {
        name: "local-history",
        value: if on { "on" } else { "off" }.to_string(),
    };
    print_stdout(&setting, opts.json);
    if was && !on {
        let message = "versions already in .syncit/blobs/ are left there".to_string();
        print_stdout(&Message::Info { message }, opts.json);
    }
    Ok(on)
}
