//! A vault: a folder with `.syncit/`, opened and locked.

use crate::cache::Cache;
use crate::commands::Event;
use crate::config::{Config, prefix_matches};
use crate::log::Log;
use anyhow::{Context, Result, anyhow, ensure};
use std::collections::HashMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Working-folder files with this prefix are scratch files of a run.
pub const TMP_PREFIX: &str = ".syncit-tmp-";

pub struct Vault {
    pub root: PathBuf,
    pub config: Config,
    pub cache: Cache,
    pub log: Log,
    pub ignore: Ignore,
    /// Hash to working files that may hold it. A hint: every use is verified.
    pub hints: HashMap<String, Vec<PathBuf>>,
    _lock: File,
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
    /// Find, lock and load the vault at or above `start`. `strict` is for the
    /// vault a command runs in; a peer's log is read leniently.
    pub fn open(start: &Path, strict: bool) -> Result<Vault> {
        let root = find_root(start)?;
        let dot = root.join(".syncit");
        let lock = File::options()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dot.join("lock"))?;
        lock.try_lock()
            .map_err(|_| anyhow!("vault is in use: {}", root.display()))?;
        let config = Config::load(&root)?;
        ensure!(
            config.syncit == 1,
            "{} has format version {}, not 1",
            root.display(),
            config.syncit
        );
        let cache = Cache::load(&root)?;
        let ignore = Ignore::parse(&fs::read_to_string(dot.join("ignore")).unwrap_or_default());
        let mut vault = Vault {
            log: Log::load(&dot.join("log"), strict)?,
            hints: HashMap::new(),
            root,
            config,
            cache,
            ignore,
            _lock: lock,
        };
        for (path, row) in &vault.cache.rows {
            vault
                .hints
                .entry(row.hash.clone())
                .or_default()
                .push(vault.root.join(path));
        }
        Ok(vault)
    }

    pub fn dot(&self) -> PathBuf {
        self.root.join(".syncit")
    }

    /// The blob store, if one is set and reachable.
    pub fn store(&self) -> Option<PathBuf> {
        self.config
            .store
            .as_ref()
            .map(PathBuf::from)
            .filter(|r| r.is_dir())
    }

    pub fn hint(&mut self, hash: &str, file: PathBuf) {
        self.hints.entry(hash.to_string()).or_default().push(file);
    }

    pub fn event(&self, kind: &'static str, path: &str, note: &str) -> Event {
        Event {
            vault: self.root.clone(),
            kind,
            path: path.to_string(),
            note: note.to_string(),
        }
    }

    /// Whether a scan would not look at this path: it is ignored, belongs to a
    /// `.syncit` folder at any depth, or is a scratch file. Such a path is
    /// never recorded and never written.
    pub fn skips(&self, path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        if self.ignore.matches(path)
            || name.starts_with(TMP_PREFIX)
            || path.split('/').any(|c| c.eq_ignore_ascii_case(".syncit"))
        {
            return true;
        }
        false
    }
}
