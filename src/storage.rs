//! A vault's replicated state: its config, its log and its blobs, and where
//! each is on the vault's backend (store.md, section 4). The same for every
//! vault; what differs is the backend.

use crate::backend::{Backend, Fs, Unreadable};
use crate::blobs::hash_file;
use crate::config::Config;
use crate::entry::{is_hash, sha256};
use anyhow::{Context, Result};
use std::cell::{RefCell, RefMut};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;

pub struct Store {
    /// The vault's folder or URL, for errors.
    name: String,
    backend: Box<dyn Backend>,
    /// The names of the segments stored, once listed.
    log: RefCell<Option<BTreeSet<String>>>,
    /// The hashes of the blobs stored, once listed.
    blobs: RefCell<Option<BTreeSet<String>>>,
    /// The segments read so far, kept so that a run that reads one twice (to
    /// load the log, then to copy it) fetches it once. None for a folder,
    /// where a second read costs little.
    read: Option<RefCell<BTreeMap<String, Vec<u8>>>>,
}

/// `blobs/ab/abcdef…`: folders by the first two hex chars, as git does, so
/// no folder grows past what FAT32 and exFAT handle well.
fn blob_path(hash: &str) -> String {
    format!("blobs/{}/{hash}", &hash[..2])
}

/// What `cell` holds, made by `list` the first time.
fn once<T>(cell: &RefCell<Option<T>>, list: impl FnOnce() -> Result<T>) -> Result<RefMut<'_, T>> {
    let mut cell = cell.borrow_mut();
    if cell.is_none() {
        *cell = Some(list()?);
    }
    Ok(RefMut::map(cell, |listed| listed.as_mut().unwrap()))
}

impl Store {
    /// The vault called `name`, stored on `backend`.
    pub fn new(name: &str, backend: Box<dyn Backend>) -> Store {
        Store {
            name: name.to_string(),
            backend,
            log: RefCell::default(),
            blobs: RefCell::default(),
            read: Some(RefCell::default()),
        }
    }

    /// The vault in the folder `root`: its `.syncit/`.
    pub fn dir(root: &Path) -> Store {
        let backend = Box::new(Fs::new(root));
        Store {
            read: None,
            ..Store::new(&root.display().to_string(), backend)
        }
    }

    pub fn load_config(&self) -> Result<Config> {
        let name = &self.name;
        let read = self.backend.read("config");
        let bytes = read.with_context(|| format!("cannot read the config of {name}"))?;
        let bytes = bytes.with_context(|| format!("{name} has no config"))?;
        serde_json::from_slice(&bytes).with_context(|| format!("corrupt config {name}"))
    }

    pub fn save_config(&self, config: &Config) -> Result<()> {
        let text = serde_json::to_string_pretty(config)? + "\n";
        self.backend.write("config", text.as_bytes())
    }

    fn log(&self) -> Result<RefMut<'_, BTreeSet<String>>> {
        once(&self.log, || self.backend.list("log"))
    }

    fn blobs(&self) -> Result<RefMut<'_, BTreeSet<String>>> {
        // Listed as `ab/<hash>`. Anything else there is not a blob.
        let blob = |path: String| {
            let (ab, hash) = path.split_once('/')?;
            (is_hash(hash) && ab == &hash[..2]).then(|| hash.to_string())
        };
        let list = || self.backend.list("blobs");
        once(&self.blobs, || {
            Ok(list()?.into_iter().filter_map(blob).collect())
        })
    }

    /// List the log and the blobs now, so that a backend that cannot be
    /// reached is an error here, and not an empty vault later.
    pub fn list(&self) -> Result<()> {
        self.log()?;
        self.blobs()?;
        Ok(())
    }

    /// The names of the segments stored.
    pub fn segment_names(&self) -> Result<BTreeSet<String>> {
        Ok(self.log()?.clone())
    }

    /// A segment's bytes, not validated.
    pub fn read_segment(&self, name: &str) -> Result<Vec<u8>> {
        if let Some(read) = &self.read
            && let Some(bytes) = read.borrow().get(name)
        {
            return Ok(bytes.clone());
        }
        let bytes = self.backend.read(&format!("log/{name}"))?;
        let bytes = bytes.context("no such segment")?;
        if let Some(read) = &self.read {
            read.borrow_mut().insert(name.to_string(), bytes.clone());
        }
        Ok(bytes)
    }

    pub fn write_segment(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.backend.write(&format!("log/{name}"), bytes)?;
        self.log()?.insert(name.to_string());
        Ok(())
    }

    fn size(&self, hash: &str) -> Option<u64> {
        self.backend.size(&blob_path(hash)).ok()?
    }

    /// From the list of blobs if there is one. If not, from a look at this
    /// one blob, which costs less than a list when few are asked about.
    pub fn has_blob(&self, hash: &str) -> bool {
        let listed = self.blobs.borrow().as_ref().map(|b| b.contains(hash));
        listed.unwrap_or_else(|| self.size(hash).is_some())
    }

    /// Which of `hashes` are not stored, from the list of blobs.
    pub fn missing(&self, hashes: &BTreeSet<String>) -> BTreeSet<String> {
        let _ = self.blobs();
        let absent = hashes.iter().filter(|hash| !self.has_blob(hash));
        absent.cloned().collect()
    }

    /// Copy the blob to `dest`, verifying its hash. False if it is absent
    /// or wrong; `dest` is then removed.
    pub fn get_blob(&self, hash: &str, dest: &Path) -> io::Result<bool> {
        if !self.has_blob(hash) {
            return Ok(false);
        }
        match self.backend.copy_to(&blob_path(hash), dest) {
            Ok(true) if hash_file(dest)? == hash => return Ok(true),
            Ok(_) => {}
            Err(e) if e.is::<Unreadable>() => {}
            Err(e) => return Err(io::Error::other(e)),
        }
        let _ = fs::remove_file(dest);
        Ok(false)
    }

    /// The blob's bytes if it is stored, verified, and no larger than `max`.
    pub fn read_blob(&self, hash: &str, max: u64) -> Option<Vec<u8>> {
        let small = self.has_blob(hash) && self.size(hash)? <= max;
        let bytes = small.then(|| self.backend.read(&blob_path(hash)).ok())???;
        (sha256(&bytes) == hash).then_some(bytes)
    }

    /// Store the file at `src`, whose hash the caller has verified. `src` is
    /// consumed.
    pub fn put_blob(&self, hash: &str, src: &Path) -> Result<()> {
        if self.has_blob(hash) {
            return Ok(fs::remove_file(src)?);
        }
        self.backend.move_to(src, &blob_path(hash))?;
        if let Some(blobs) = self.blobs.borrow_mut().as_mut() {
            blobs.insert(hash.to_string());
        }
        Ok(())
    }

    /// The stored blobs whose hash is not in `keep`: how many, and their
    /// bytes. They are removed unless `dry_run`.
    pub fn drop_blobs_except(
        &self,
        keep: &BTreeSet<String>,
        dry_run: bool,
    ) -> Result<(usize, u64)> {
        let found: Vec<String> = self.blobs()?.difference(keep).cloned().collect();
        // ponytail: a look at each blob for its size, which on object storage
        // is a request for each. Take sizes from the listing if compacting a
        // remote is too slow.
        let bytes = found.iter().filter_map(|hash| self.size(hash)).sum();
        if !dry_run {
            for hash in &found {
                self.backend.remove(&blob_path(hash))?;
                self.blobs()?.remove(hash);
            }
        }
        Ok((found.len(), bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_blobs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blobs = tmp.path().join(".syncit/blobs");
        let (a, b) = (sha256(b"a"), sha256(b"b"));
        let both = BTreeSet::from([a.clone(), b.clone()]);
        let put = |store: &Store, hash: &str, text: &str| {
            let src = tmp.path().join("src");
            fs::write(&src, text).unwrap();
            store.put_blob(hash, &src).unwrap();
            assert!(!src.exists(), "consumed");
        };
        let store = Store::dir(tmp.path());
        assert_eq!(store.missing(&both), both, "no blobs folder yet");
        put(&store, &a, "a");
        put(&store, &a, "a");
        assert_eq!(fs::read(blobs.join(&a[..2]).join(&a)).unwrap(), b"a");
        assert_eq!(store.missing(&both), BTreeSet::from([b.clone()]));
        assert_eq!(store.read_blob(&a, 1).unwrap(), b"a");
        assert_eq!(store.read_blob(&a, 0), None, "too large");
        let out = tmp.path().join("out");
        assert!(store.get_blob(&a, &out).unwrap() && fs::read(&out).unwrap() == b"a");
        assert!(!store.get_blob(&b, &out).unwrap());
        // What a fresh look finds: a blob that is not what its name says
        // is there, but is not to be had.
        fs::write(blobs.join(&a[..2]).join(&a), "x").unwrap();
        let store = Store::dir(tmp.path());
        assert!(store.has_blob(&a) && store.read_blob(&a, 1).is_none());
        assert!(!store.get_blob(&a, &out).unwrap() && !out.exists());
        assert_eq!(store.drop_blobs_except(&both, false).unwrap(), (0, 0));
        assert_eq!(
            store.drop_blobs_except(&BTreeSet::new(), true).unwrap(),
            (1, 1)
        );
        assert_eq!(
            store.drop_blobs_except(&BTreeSet::new(), false).unwrap(),
            (1, 1)
        );
        assert_eq!(store.missing(&both), both);
        assert!(blobs.is_dir() && !blobs.join(&a[..2]).exists());
        // A folder is not a blob, and neither is a file out of place.
        fs::create_dir_all(blobs.join(&b[..2]).join(&b)).unwrap();
        fs::create_dir_all(blobs.join("zz")).unwrap();
        fs::write(blobs.join("zz").join(&a), "a").unwrap();
        assert_eq!(Store::dir(tmp.path()).missing(&both), both);
    }
}
