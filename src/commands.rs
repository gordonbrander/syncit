//! One function per command. Each returns a `Report` for `main` to print.

use crate::blobs::{flush, retain};
use crate::config::{Config, prefix_matches};
use crate::entry::valid_path;
use crate::log::PathState;
use crate::materialise::{copy_name, materialise, prune};
use crate::scan::scan;
use crate::segment;
use crate::vault::{Vault, find_root, relative, resolve_path, write_atomic};
use anyhow::{Context, Result, ensure};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct Event {
    pub vault: PathBuf,
    pub kind: &'static str,
    pub path: String,
    pub note: String,
}

#[derive(Debug, Default)]
pub struct Report {
    pub events: Vec<Event>,
    /// Path to a description of its open conflict.
    pub conflicts: BTreeMap<String, String>,
    /// Entries written by scans.
    pub recorded: usize,
    pub notes: Vec<String>,
    /// No summary line and exit code 0: `init` and `status`.
    pub plain: bool,
}

impl Report {
    pub fn count(&self, kind: &str) -> usize {
        self.events.iter().filter(|e| e.kind == kind).count()
    }

    pub fn has(&self, kind: &str, path: &str) -> bool {
        self.events.iter().any(|e| e.kind == kind && e.path == path)
    }

    /// 2 if something needs attention, else 0.
    pub fn exit_code(&self) -> i32 {
        let open = ["pending", "skipped", "incomplete", "rejected"]
            .iter()
            .any(|k| self.count(k) > 0);
        if !self.plain && (open || !self.conflicts.is_empty()) {
            2
        } else {
            0
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut vaults: Vec<&Path> = vec![];
        for e in &self.events {
            if !vaults.contains(&e.vault.as_path()) {
                vaults.push(&e.vault);
            }
        }
        for vault in vaults {
            writeln!(f, "{}:", vault.display())?;
            let mut events: Vec<&Event> = self.events.iter().filter(|e| e.vault == vault).collect();
            events.sort_by(|a, b| a.path.cmp(&b.path));
            for e in events {
                let note = if e.note.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", e.note)
                };
                writeln!(f, "  {}: {}{note}", e.kind, e.path)?;
            }
        }
        for (path, what) in &self.conflicts {
            writeln!(f, "conflict: {path} — {what}")?;
        }
        if !self.plain {
            let n = |kind| self.count(kind);
            writeln!(
                f,
                "recorded {}, added {}, updated {}, deleted {}, conflicts {}, pending {}, skipped {}",
                self.recorded,
                n("added"),
                n("updated"),
                n("deleted"),
                self.conflicts.len(),
                n("pending"),
                n("skipped")
            )?;
        }
        self.notes.iter().try_for_each(|note| writeln!(f, "{note}"))
    }
}

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

pub fn init(dir: &Path) -> Result<Report> {
    ensure!(
        !dir.join(".syncit").exists(),
        "{} is already a vault",
        dir.display()
    );
    create(dir, Config::new(String::new()))?;
    let notes = vec![format!(
        "initialised a vault in {}",
        resolve_path(dir)?.display()
    )];
    Ok(Report {
        notes,
        plain: true,
        ..Report::default()
    })
}

pub fn clone(src: &Path, dir: &Path) -> Result<Report> {
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
    run(dir, &root, Mode::Pull)
}

pub fn sync(cwd: &Path, peer: &Path) -> Result<Report> {
    run(cwd, peer, Mode::Sync)
}

pub fn push(cwd: &Path, peer: &Path) -> Result<Report> {
    run(cwd, peer, Mode::Push)
}

pub fn pull(cwd: &Path, peer: &Path) -> Result<Report> {
    run(cwd, peer, Mode::Pull)
}

/// Whether the folders are the same or one contains the other. Such vaults
/// share files: a run between them would copy the outer into the inner,
/// one level deeper each time.
fn nested(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

/// Open another vault for a run with `v`.
fn other(v: &Vault, path: &Path) -> Result<Vault> {
    ensure!(
        !nested(&find_root(path)?, &v.root),
        "cannot sync a vault with itself or with a vault inside it"
    );
    Vault::open(path, false)
}

/// A run with a peer (SPEC.md section 10). `a` is the local vault.
fn run(cwd: &Path, peer: &Path, mode: Mode) -> Result<Report> {
    let mut a = Vault::open(cwd, true)?;
    let mut b = other(&a, peer)?;
    let mut report = Report::default();
    let (to_b, to_a) = (mode != Mode::Pull, mode != Mode::Push);

    record(&mut a, Some(&b), &mut report)?;
    record(&mut b, Some(&a), &mut report)?;
    if to_b {
        receive(&mut b, &a, &mut report)?;
    }
    if to_a {
        receive(&mut a, &b, &mut report)?;
    }
    // Both retain before either materialises, so that a version is saved
    // before it is replaced.
    if to_b {
        retain(&b, Some(&a))?;
    }
    if to_a {
        retain(&a, Some(&b))?;
    }
    if to_b {
        materialise(&mut b, Some(&a), &mut report.events)?;
    }
    if to_a {
        materialise(&mut a, Some(&b), &mut report.events)?;
    }
    flush(&a)?;
    flush(&b)?;
    if to_b {
        conflicts(&b, &mut report);
    }
    if to_a {
        conflicts(&a, &mut report);
    }
    Ok(report)
}

/// Scan, then retain blobs for what the scan recorded.
fn record(v: &mut Vault, peer: Option<&Vault>, report: &mut Report) -> Result<()> {
    let scanned = scan(v, false)?;
    report.recorded += scanned.entries.len();
    for path in &scanned.skipped {
        report
            .events
            .push(v.event("skipped", path, "changed while it was being read"));
    }
    retain(v, peer)
}

/// Take every segment the other vault has and this one lacks. A segment is
/// validated before it is stored; one that fails is reported and left out.
pub fn receive(v: &mut Vault, from: &Vault, report: &mut Report) -> Result<()> {
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
            Err(e) => report.events.push(v.event(
                "rejected",
                &format!("segment {name}"),
                &format!("{e:#}"),
            )),
        }
    }
    Ok(())
}

/// Report each open conflict among the paths this vault holds or wants.
fn conflicts(v: &Vault, report: &mut Report) {
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
        {
            continue;
        }
        let copies: Vec<String> = losers.iter().map(|l| copy_name(path, &l.hash)).collect();
        let mut what = match copies.len() {
            0 => "kept the file".to_string(),
            1 => format!("kept the newer version, other saved as {}", copies[0]),
            _ => format!(
                "kept the newest version, others saved as {}",
                copies.join(", ")
            ),
        };
        if lost_delete {
            what += "; it was deleted in another vault";
        }
        report.conflicts.insert(path.clone(), what);
    }
}

pub fn snapshot(cwd: &Path) -> Result<Report> {
    let mut v = Vault::open(cwd, true)?;
    let mut report = Report::default();
    record(&mut v, None, &mut report)?;
    flush(&v)?;
    Ok(report)
}

/// What a scan would record and what a sync would do. Writes nothing.
pub fn status(cwd: &Path) -> Result<Report> {
    let mut v = Vault::open(cwd, true)?;
    let mut report = Report {
        plain: true,
        ..Report::default()
    };
    let scanned = scan(&mut v, true)?;
    for (path, kind) in &scanned.changes {
        report.events.push(v.event(kind, path, ""));
    }
    let changed: BTreeSet<&String> = scanned.changes.iter().map(|(path, _)| path).collect();
    for path in v.log.paths.keys() {
        let row = v.cache.rows.get(path);
        match v.log.resolve(path) {
            PathState::Absent => {}
            PathState::Incomplete => {
                report
                    .events
                    .push(v.event("incomplete", path, "waiting for missing history"))
            }
            PathState::Resolved { winner, .. } => {
                let on_disk = row.map(|row| &row.hash);
                let concerns = (row.is_some() || v.config.wanted(path))
                    && !v.skips(path)
                    && !changed.contains(path);
                if concerns && v.log.entries[&winner].hash.as_ref() != on_disk {
                    report.events.push(v.event("behind", path, ""));
                }
            }
        }
    }
    conflicts(&v, &mut report);
    Ok(report)
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
pub fn get(cwd: &Path, paths: &[PathBuf]) -> Result<Report> {
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
        let mut v = Vault::open(cwd, true)?;
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
    run(cwd, &source, Mode::Pull)
}

/// Stop holding paths, without deleting them elsewhere. Unless forced, the
/// content of every file removed must be known to exist somewhere else: in
/// this vault's stored blobs, its store, or the vault named by `check`.
pub fn drop(cwd: &Path, paths: &[PathBuf], check: Option<&Path>, force: bool) -> Result<Report> {
    let mut v = Vault::open(cwd, true)?;
    let prefixes = prefixes(&v.root, cwd, paths)?;
    let check = check.map(|path| other(&v, path)).transpose()?;
    let mut report = Report::default();
    // Unrecorded changes are recorded before anything is removed.
    record(&mut v, check.as_ref(), &mut report)?;

    let under = |path: &&String| prefixes.iter().any(|prefix| prefix_matches(prefix, path));
    let held: Vec<String> = v.cache.rows.keys().filter(under).cloned().collect();
    if !force {
        let (blobs, store) = (v.dot().join("blobs"), v.store());
        let elsewhere = |hash: &String| {
            blobs.join(hash).exists()
                || store.as_ref().is_some_and(|s| s.join(hash).exists())
                || check.as_ref().is_some_and(|c| {
                    c.cache.rows.values().any(|row| &row.hash == hash)
                        || c.dot().join("blobs").join(hash).exists()
                })
        };
        let sole: Vec<&str> = held
            .iter()
            .filter(|p| !elsewhere(&v.cache.rows[*p].hash))
            .map(String::as_str)
            .collect();
        let hint = match check {
            Some(_) => "Sync with it first",
            None => "Name a vault that holds them with --check <vault>",
        };
        ensure!(
            sole.is_empty(),
            "nothing dropped: no other copy is known of {}. {hint}, or use --force",
            sole.join(", ")
        );
    }

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
            report
                .events
                .push(v.event("skipped", &path, "changed since it was scanned"));
            continue;
        }
        fs::remove_file(&file)?;
        prune(&v.root, &file);
        report.events.push(v.event("dropped", &path, ""));
    }
    Ok(report)
}

pub enum Setting {
    LocalHistory(bool),
    /// A folder, relative to the current directory, or None for no store.
    Store(Option<PathBuf>),
}

/// Show this vault's settings, after changing one if asked.
pub fn config(cwd: &Path, change: Option<Setting>) -> Result<Report> {
    let mut v = Vault::open(cwd, true)?;
    let mut notes = vec![];
    match change {
        None => {}
        Some(Setting::LocalHistory(on)) => {
            if v.config.keep_local_history && !on {
                notes.push("versions already in .syncit/blobs/ are left there".to_string());
            }
            v.config.keep_local_history = on;
        }
        Some(Setting::Store(None)) => v.config.store = None,
        Some(Setting::Store(Some(dir))) => {
            let dir = resolve_path(&cwd.join(dir))?;
            let dir = dir.to_str().context("the store's path is not UTF-8")?;
            v.config.store = Some(dir.to_string());
        }
    }
    v.config.save(&v.root)?;
    let on_off = if v.config.keep_local_history {
        "on"
    } else {
        "off"
    };
    let store = match (&v.config.store, v.store()) {
        (None, _) => "none".to_string(),
        (Some(dir), Some(_)) => dir.clone(),
        (Some(dir), None) => format!("{dir} (not reachable; versions wait in this vault)"),
    };
    notes.splice(
        0..0,
        [
            format!("local-history: {on_off}"),
            format!("store: {store}"),
        ],
    );
    Ok(Report {
        notes,
        plain: true,
        ..Report::default()
    })
}
