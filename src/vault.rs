//! A vault: a folder with `.syncit/`, opened and locked.

use crate::config::{Config, prefix_matches};
use crate::dirstate::Dirstate;
use crate::log::Log;
use crate::output::{Event, Message, Options, Summary, print_stdout};
use crate::storage::{Dir, Storage};
use anyhow::{Context, Result, anyhow, ensure};
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Working-folder files with this prefix are scratch files of a run.
pub const TMP_PREFIX: &str = ".syncit-tmp-";

pub struct Vault {
    /// The working folder; empty for a bare vault.
    pub root: PathBuf,
    /// Where the vault is, for people: its folder or URL.
    pub name: String,
    /// Bare: no working folder. Its current versions are in its storage,
    /// and its dirstate is derived from its log and the blobs it holds.
    pub bare: bool,
    /// Its config, log and blobs.
    pub storage: Box<dyn Storage>,
    /// Scratch space for files in transit.
    pub tmp: PathBuf,
    /// In a run with a peer, `->` for our vault and `<-` for theirs.
    pub side: &'static str,
    pub config: Config,
    pub dirstate: Dirstate,
    pub log: Log,
    pub ignore: Ignore,
    /// Hash to working files that may have it. A hint: every use is verified.
    pub hints: HashMap<String, Vec<PathBuf>>,
    /// What a run moved to and from this vault's storage.
    pub traffic: RefCell<Traffic>,
    _lock: Option<File>,
}

/// Blob hashes and segment names, sent to a vault or received from it.
#[derive(Default)]
pub struct Traffic {
    pub sent_files: BTreeSet<String>,
    pub sent_segments: BTreeSet<String>,
    pub received_files: BTreeSet<String>,
    pub received_segments: BTreeSet<String>,
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

impl Vault {
    /// Find, lock and load the vault at or above `start`. `local` is for the
    /// vault a command runs in; a peer's log is read leniently.
    pub fn open(opts: &Options, start: &Path, local: bool) -> Result<Vault> {
        let root = find_root(start)?;
        let dot = root.join(".syncit");
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dot.join("lock"))?;
        lock.try_lock()
            .map_err(|_| anyhow!("vault is in use: {}", root.display()))?;
        let storage = Dir::new(&root);
        let config = storage.load_config()?;
        ensure!(
            config.syncit == 1,
            "{} has format version {}, not 1",
            root.display(),
            config.syncit
        );
        let dirstate = Dirstate::load(&root)?;
        let ignore =
            Ignore::parse(&fs::read_to_string(root.join(".syncitignore")).unwrap_or_default());
        let mut vault = Vault {
            log: Log::load(opts, &storage, local)?,
            name: root.display().to_string(),
            bare: false,
            traffic: RefCell::default(),
            storage: Box::new(storage),
            tmp: dot.join("tmp"),
            side: "",
            hints: HashMap::new(),
            root,
            config,
            dirstate,
            ignore,
            _lock: Some(lock),
        };
        for (path, row) in &vault.dirstate.rows {
            vault
                .hints
                .entry(row.hash.clone())
                .or_default()
                .push(vault.root.join(path));
        }
        Ok(vault)
    }

    /// Open the remote vault at `url` for a command run in `local`, if
    /// there is one: it finds the key there, and moves files through its
    /// `.syncit/tmp`. A remote is bare and is not locked.
    #[cfg(feature = "remote")]
    pub fn open_url(opts: &Options, url: &str, local: Option<&Vault>) -> Result<Vault> {
        use crate::remote::{Bucket, Remote, credentials};
        let root = local.map(|v| v.root.as_path());
        let bucket = Bucket::new(url, credentials(opts, url)?)?;
        let check = bucket.check()?;
        let (key, _) = crate::keys::find(opts, url, &check)?;
        let storage = Remote::open(bucket, key, root.map(Dir::new))?;
        let config = storage.load_config()?;
        ensure!(
            config.syncit == 1,
            "{url} has format version {}, not 1",
            config.syncit
        );
        let mut vault = Vault {
            root: PathBuf::new(),
            name: url.to_string(),
            bare: true,
            log: Log::load(opts, &storage, false)?,
            storage: Box::new(storage),
            tmp: local.map(|v| v.tmp.clone()).unwrap_or_default(),
            side: "",
            config,
            dirstate: Dirstate::default(),
            ignore: Ignore::default(),
            hints: HashMap::new(),
            traffic: RefCell::default(),
            _lock: None,
        };
        vault.derive(|v, hash| v.storage.has_blob(hash));
        Ok(vault)
    }

    /// A bare vault's dirstate (store.md, section 7): a row for each path it
    /// takes whose winner is a live entry with a `stored` blob.
    pub fn derive(&mut self, stored: impl Fn(&Vault, &str) -> bool) {
        let mut rows = std::collections::BTreeMap::new();
        for path in self.log.paths.keys().filter(|p| self.takes(p)) {
            let crate::log::PathState::Resolved { winner, .. } = self.log.resolve(path) else {
                continue;
            };
            let e = &self.log.entries[&winner];
            if let Some(hash) = &e.hash
                && stored(self, hash)
            {
                let row = crate::dirstate::Row {
                    size: 0,
                    mtime: e.mtime,
                    exec: e.exec,
                    hash: hash.clone(),
                    base: vec![winner],
                };
                rows.insert(path.clone(), row);
            }
        }
        self.dirstate.rows = rows;
    }

    pub fn dot(&self) -> PathBuf {
        self.root.join(".syncit")
    }

    pub fn hint(&mut self, hash: &str, file: PathBuf) {
        self.hints.entry(hash.to_string()).or_default().push(file);
    }

    /// Print what happened to one file, and count it.
    pub fn event(
        &self,
        opts: &Options,
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
            side: self.side,
        };
        print_stdout(&Message::Event(event.clone()), opts.json);
        sum.record(event);
    }

    /// Print that a file was moved from `from` to `to`, and count it.
    pub fn renamed(&self, opts: &Options, sum: &mut Summary, from: &str, to: &str) {
        let event = Event {
            kind: "renamed",
            vault: self.name.clone(),
            path: to.to_string(),
            note: String::new(),
            from: Some(from.to_string()),
            side: self.side,
        };
        print_stdout(&Message::Event(event.clone()), opts.json);
        sum.record(event);
    }

    /// Whether a run writes this path here: the vault includes it and does
    /// not ignore it. A scan includes every file on disk, so after one a path
    /// with a row is taken unless it is ignored.
    pub fn takes(&self, path: &str) -> bool {
        self.config.includes(path) && !self.skips(path)
    }

    /// Whether a scan would not look at this path: it is ignored, belongs to a
    /// `.syncit` folder at any depth, or is a scratch file. Such a path is
    /// never recorded and never written.
    pub fn skips(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        self.ignore.matches(path)
            || name.starts_with(TMP_PREFIX)
            || path.split('/').any(|c| c.eq_ignore_ascii_case(".syncit"))
    }
}
