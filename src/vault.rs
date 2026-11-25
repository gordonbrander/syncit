//! A vault, opened: its store, its config and log, and the files it has.

use crate::backend::{Backend, Fs};
use crate::config::{Config, prefix_matches};
use crate::dirstate::{Dirstate, Row};
use crate::log::{Log, PathState};
use crate::output::{Context, Event, Message, Summary, print_stdout};
use crate::scan::Scan;
use crate::storage::Store;
use anyhow::{Context as _, Result, anyhow};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Working-dir files with this prefix are scratch files of a run.
pub const TMP_PREFIX: &str = ".syncit-tmp-";

/// A vault on the backend `B`, whose files are a `W`: a working dir, or none.
pub struct Vault<B: Backend, W: Worktree> {
    /// Where the vault is, for people: its dir or URL.
    pub name: String,
    /// Its config, log and blobs, as stored.
    pub store: Store<B>,
    pub config: Config,
    pub log: Log,
    pub worktree: W,
}

/// A working dir: the files a person edits.
pub struct Dir {
    pub root: PathBuf,
    pub dirstate: Dirstate,
    pub ignore: Ignore,
    /// Hash to files that may have it. A hint: every use is verified.
    pub hints: HashMap<String, Vec<PathBuf>>,
}

impl Dir {
    pub fn hint(&mut self, hash: &str, file: PathBuf) {
        self.hints.entry(hash.to_string()).or_default().push(file);
    }
}

/// No working dir. The vault's files are its blobs, and the version it has
/// of each path is derived from its log and the blobs it holds (store.md,
/// section 7): a row for each path it takes whose winner is a live entry
/// with a stored blob.
#[derive(Default)]
pub struct Bare {
    pub rows: BTreeMap<String, Row>,
}

/// What a vault's files are. Everything that differs between a vault with a
/// working dir and one without is here.
pub trait Worktree: Sized {
    /// Its blobs are its files: it gathers content whatever `keep_history`
    /// says, and a run reports what was sent to it and received from it.
    const BARE: bool;
    /// The version of each path it has.
    fn rows(&self) -> &BTreeMap<String, Row>;
    /// Whether it never records or writes this path, beyond the names every
    /// vault reserves.
    fn skips(&self, path: &str) -> bool;
    /// Files that may hold this content.
    fn files(&self, hash: &str) -> &[PathBuf];
    /// Its dir, if it has one.
    fn root(&self) -> Option<&Path>;

    /// Record what changed in its files.
    fn scan(ctx: &Context, v: &mut Vault<impl Backend, Self>, dry: bool) -> Result<Scan>;
    /// Bring its files to what the log says.
    fn materialise(
        ctx: &Context,
        v: &mut Vault<impl Backend, Self>,
        peer: Option<&Vault<impl Backend, impl Worktree>>,
        kept: bool,
        sum: &mut Summary,
    ) -> Result<()>;
    /// `--no-remove`: keep the files whose paths the log says are deleted.
    /// Whether any was kept, which a scan then records.
    fn unremove(ctx: &Context, v: &mut Vault<impl Backend, Self>, sum: &mut Summary) -> bool;
    /// After blobs were retained. `arrived` is what a dry run would have
    /// stored.
    fn retained(v: &mut Vault<impl Backend, Self>, arrived: &BTreeSet<String>);
}

impl Worktree for Dir {
    const BARE: bool = false;

    fn rows(&self) -> &BTreeMap<String, Row> {
        &self.dirstate.rows
    }

    fn skips(&self, path: &str) -> bool {
        self.ignore.matches(path)
    }

    fn files(&self, hash: &str) -> &[PathBuf] {
        self.hints.get(hash).map_or(&[], Vec::as_slice)
    }

    fn root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn scan(ctx: &Context, v: &mut Vault<impl Backend, Self>, dry: bool) -> Result<Scan> {
        crate::scan::scan(ctx, v, dry)
    }

    fn materialise(
        ctx: &Context,
        v: &mut Vault<impl Backend, Self>,
        peer: Option<&Vault<impl Backend, impl Worktree>>,
        kept: bool,
        sum: &mut Summary,
    ) -> Result<()> {
        crate::materialise::materialise(ctx, v, peer, kept, sum)
    }

    fn unremove(ctx: &Context, v: &mut Vault<impl Backend, Self>, sum: &mut Summary) -> bool {
        crate::materialise::unremove(ctx, v, sum)
    }

    fn retained(_: &mut Vault<impl Backend, Self>, _: &BTreeSet<String>) {}
}

impl Worktree for Bare {
    const BARE: bool = true;

    fn rows(&self) -> &BTreeMap<String, Row> {
        &self.rows
    }

    fn skips(&self, _: &str) -> bool {
        false
    }

    fn files(&self, _: &str) -> &[PathBuf] {
        &[]
    }

    fn root(&self) -> Option<&Path> {
        None
    }

    fn scan(_: &Context, _: &mut Vault<impl Backend, Self>, _: bool) -> Result<Scan> {
        Ok(Scan::default())
    }

    fn materialise(
        _: &Context,
        _: &mut Vault<impl Backend, Self>,
        _: Option<&Vault<impl Backend, impl Worktree>>,
        _: bool,
        _: &mut Summary,
    ) -> Result<()> {
        Ok(())
    }

    fn unremove(_: &Context, _: &mut Vault<impl Backend, Self>, _: &mut Summary) -> bool {
        false
    }

    fn retained(v: &mut Vault<impl Backend, Self>, arrived: &BTreeSet<String>) {
        let mut rows = BTreeMap::new();
        let wanted = |p: &&String| v.config.includes(p) && !v.skips(p);
        for path in v.log.paths.keys().filter(wanted) {
            let PathState::Resolved { winner, .. } = v.log.resolve(path) else {
                continue;
            };
            let e = &v.log.entries[&winner];
            if let Some(hash) = &e.hash
                && (v.store.has_blob(hash) || arrived.contains(hash))
            {
                let row = Row {
                    size: None,
                    mtime: e.mtime,
                    exec: e.exec,
                    hash: hash.clone(),
                    base: vec![winner],
                };
                rows.insert(path.clone(), row);
            }
        }
        v.worktree.rows = rows;
    }
}

/// Another vault, as a command names it.
pub enum Target {
    /// A dir in the vault.
    Dir(PathBuf),
    /// The URL of a remote vault.
    Url(String),
}

impl Target {
    /// What `arg` names: the location `remotes` has under that nickname, or
    /// else `arg` itself.
    pub fn parse(arg: &Path, remotes: Option<&BTreeMap<String, String>>) -> Target {
        let text = arg.to_str();
        let location = text.and_then(|t| remotes?.get(t).map(String::as_str));
        match location.or(text) {
            Some(text) if text.contains("://") => Target::Url(text.to_string()),
            Some(text) => Target::Dir(PathBuf::from(text)),
            None => Target::Dir(arg.to_path_buf()),
        }
    }
}

#[derive(Default)]
pub struct Ignore {
    names: Vec<String>,
    prefixes: Vec<String>,
}

impl Ignore {
    pub fn parse(text: &str) -> Ignore {
        let mut ignore = Ignore::default();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            match line.contains('/') {
                false => ignore.names.push(line.to_string()),
                true if line.trim_matches('/').is_empty() => {}
                true => ignore.prefixes.push(line.trim_matches('/').to_string()),
            }
        }
        ignore
    }

    pub fn matches(&self, path: &str) -> bool {
        path.split('/').any(|c| self.names.iter().any(|n| n == c))
            || self.prefixes.iter().any(|p| prefix_matches(p, path))
    }
}

/// A name that no other scratch file of this process has.
pub fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Write a file under `.syncit/` by way of `.syncit/tmp/` and a rename.
pub fn write_atomic(root: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = root.join(".syncit/tmp");
    fs::create_dir_all(&tmp)?;
    let tmp = tmp.join(unique());
    fs::write(&tmp, bytes).with_context(|| format!("cannot write {}", tmp.display()))?;
    fs::rename(&tmp, dest).with_context(|| format!("cannot write {}", dest.display()))
}

/// The absolute, symlink-free form of a path that need not exist.
pub fn resolve_path(path: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let bad = || format!("cannot resolve {}", path.display());
    let (mut base, mut tail) = (abs.as_path(), vec![]);
    while base.symlink_metadata().is_err() {
        tail.push(base.file_name().with_context(bad)?);
        base = base.parent().with_context(bad)?;
    }
    let mut out = base.canonicalize().with_context(bad)?;
    out.extend(tail.into_iter().rev());
    Ok(out)
}

/// The first folder at or above `start` that contains `.syncit/`.
pub fn find_root(start: &Path) -> Result<PathBuf> {
    let start = resolve_path(start)?;
    let root = start.ancestors().find(|d| d.join(".syncit").is_dir());
    root.map(Path::to_path_buf)
        .with_context(|| format!("not in a vault: {}", start.display()))
}

/// `path` relative to `root`, `/`-separated; `""` for the root itself.
pub fn relative(root: &Path, path: &Path) -> Result<String> {
    let outside = || anyhow!("{} is outside the vault {}", path.display(), root.display());
    let parts: Option<Vec<&str>> = path
        .strip_prefix(root)
        .map_err(|_| outside())?
        .iter()
        .map(|c| c.to_str())
        .collect();
    Ok(parts
        .with_context(|| format!("{} is not UTF-8", path.display()))?
        .join("/"))
}

impl Vault<Fs, Dir> {
    /// Find, lock and load the vault at or above `start`. `local` is for the
    /// vault a command runs in, which is brought up to this build's layout
    /// if it is behind; a peer is refused if it is, and its log is read
    /// leniently.
    pub fn open(ctx: &Context, start: &Path, local: bool) -> Result<Vault<Fs, Dir>> {
        let fs = Fs::open(start)?;
        let root = fs.root().to_path_buf();
        let name = root.display().to_string();
        if local {
            crate::migrate::upgrade(ctx, &fs, &name)?;
        }
        let store = Store::open(&name, fs)?;
        let config = store.load_config()?;
        let dirstate = Dirstate::load(&root)?;
        let ignore =
            Ignore::parse(&fs::read_to_string(root.join(".syncitignore")).unwrap_or_default());
        let mut worktree = Dir {
            hints: HashMap::new(),
            root,
            dirstate,
            ignore,
        };
        for (path, row) in &worktree.dirstate.rows {
            let file = worktree.root.join(path);
            worktree
                .hints
                .entry(row.hash.clone())
                .or_default()
                .push(file);
        }
        Ok(Vault {
            log: Log::load(ctx, &store, None, local)?,
            name,
            store,
            config,
            worktree,
        })
    }
}

impl<B: Backend> Vault<B, Bare> {
    /// Load the vault called `name` that is stored on `backend`, for a
    /// command run in the vault whose store is `known`, if there is one: the
    /// segments that vault has are read from it.
    pub fn bare(
        ctx: &Context,
        name: &str,
        backend: B,
        known: Option<&Store<Fs>>,
    ) -> Result<Vault<B, Bare>> {
        let store = Store::open(name, backend)?;
        let config = store.load_config()?;
        store.list()?;
        let mut vault = Vault {
            name: name.to_string(),
            log: Log::load(ctx, &store, known, false)?,
            store,
            config,
            worktree: Bare::default(),
        };
        Bare::retained(&mut vault, &BTreeSet::new());
        Ok(vault)
    }
}

impl<B: Backend, W: Worktree> Vault<B, W> {
    /// Print what happened to one file, and count it.
    pub fn event(
        &self,
        ctx: &Context,
        sum: &mut Summary,
        kind: &'static str,
        path: &str,
        note: &str,
    ) {
        let event = Event {
            kind,
            vault: self.name.clone(),
            path: path.to_string(),
            note: note.to_string(),
            from: None,
            side: ctx.side(&self.name),
        };
        print_stdout(ctx, &Message::Event(event.clone()));
        sum.record(event);
    }

    /// Print that a file was moved from `from` to `to`, and count it.
    pub fn renamed(&self, ctx: &Context, sum: &mut Summary, from: &str, to: &str) {
        let event = Event {
            kind: "renamed",
            vault: self.name.clone(),
            path: to.to_string(),
            note: String::new(),
            from: Some(from.to_string()),
            side: ctx.side(&self.name),
        };
        print_stdout(ctx, &Message::Event(event.clone()));
        sum.record(event);
    }

    /// Whether a run writes this path here: the vault includes it or has it
    /// already, and does not ignore it. A path with a row is taken whatever
    /// the filter says, until `drop` or `move` removes it.
    pub fn takes(&self, path: &str) -> bool {
        (self.config.includes(path) || self.worktree.rows().contains_key(path)) && !self.skips(path)
    }

    /// Whether a scan would not look at this path: it is ignored, belongs to a
    /// `.syncit` folder at any depth, or is a scratch file. Such a path is
    /// never recorded and never written.
    pub fn skips(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.worktree.skips(path)
            || name.starts_with(TMP_PREFIX)
            || path.split('/').any(|c| c.eq_ignore_ascii_case(".syncit"))
    }
}
