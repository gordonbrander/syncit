//! Where a vault's replicated state lives: its config, its log and its
//! blobs. Callers speak in plaintext names and hashes only (store.md,
//! section 4). `Dir` is a vault's own `.syncit/` folder.

use crate::blobs::copy_verified;
use crate::config::Config;
use crate::entry::sha256;
use anyhow::{Context, Result};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub trait Storage {
    fn load_config(&self) -> Result<Config>;
    fn save_config(&self, config: &Config) -> Result<()>;

    /// The names of the segments stored.
    fn segment_names(&self) -> Result<BTreeSet<String>>;
    /// A segment's bytes, not validated.
    fn read_segment(&self, name: &str) -> Result<Vec<u8>>;
    fn write_segment(&self, name: &str, bytes: &[u8]) -> Result<()>;

    fn has_blob(&self, hash: &str) -> bool;
    /// Which of `hashes` are not stored.
    fn missing(&self, hashes: &BTreeSet<String>) -> BTreeSet<String> {
        let absent = hashes.iter().filter(|hash| !self.has_blob(hash));
        absent.cloned().collect()
    }
    /// Copy the blob to `dest`, verifying its hash. False if it is absent
    /// or wrong; `dest` is then removed.
    fn get_blob(&self, hash: &str, dest: &Path) -> io::Result<bool>;
    /// The blob's bytes if it is stored, verified, and no larger than `max`.
    fn read_blob(&self, hash: &str, max: u64) -> Option<Vec<u8>>;
    /// Store the file at `src`, whose hash the caller has verified. `src` is
    /// consumed.
    fn put_blob(&self, hash: &str, src: &Path) -> Result<()>;
    /// The stored blobs whose hash is not in `keep`: how many, and their
    /// bytes. They are removed unless `dry_run`.
    fn drop_blobs_except(&self, keep: &BTreeSet<String>, dry_run: bool) -> Result<(usize, u64)>;
}

/// A vault folder's `.syncit/`.
#[derive(Clone)]
pub struct Dir {
    /// The vault folder, not its `.syncit/`.
    pub root: PathBuf,
}

impl Dir {
    pub fn new(root: &Path) -> Dir {
        Dir {
            root: root.to_path_buf(),
        }
    }

    fn dot(&self) -> PathBuf {
        self.root.join(".syncit")
    }

    /// `blobs/ab/abcdef…`: folders by the first two hex chars, as git does,
    /// so no folder grows past what FAT32 and exFAT handle well.
    fn blob(&self, hash: &str) -> PathBuf {
        self.dot().join("blobs").join(&hash[..2]).join(hash)
    }

    /// The stored blobs: one read of `blobs/` and of each folder in it, not
    /// a `stat` for each blob. Each file's name is its hash.
    fn stored(&self) -> Vec<fs::DirEntry> {
        let read = |dir: &Path| fs::read_dir(dir).into_iter().flatten().flatten();
        let kind = |e: &fs::DirEntry, dir: bool| e.file_type().is_ok_and(|t| t.is_dir() == dir);
        read(&self.dot().join("blobs"))
            .filter(|e| kind(e, true))
            .flat_map(|prefix| {
                read(&prefix.path())
                    .filter(|e| kind(e, false))
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

impl Storage for Dir {
    fn load_config(&self) -> Result<Config> {
        Config::load(&self.root)
    }

    fn save_config(&self, config: &Config) -> Result<()> {
        config.save(&self.root)
    }

    fn segment_names(&self) -> Result<BTreeSet<String>> {
        let dir = self.dot().join("log");
        let mut names = BTreeSet::new();
        for file in fs::read_dir(&dir).with_context(|| format!("cannot read {}", dir.display()))? {
            names.insert(file?.file_name().to_string_lossy().into_owned());
        }
        Ok(names)
    }

    fn read_segment(&self, name: &str) -> Result<Vec<u8>> {
        let file = self.dot().join("log").join(name);
        fs::read(&file).with_context(|| format!("cannot read {}", file.display()))
    }

    fn write_segment(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let dest = self.dot().join("log").join(name);
        crate::vault::write_atomic(&self.root, &dest, bytes)
    }

    fn has_blob(&self, hash: &str) -> bool {
        self.blob(hash).is_file()
    }

    /// From `stored`. A hash the listing does not show is looked at again,
    /// so the answer is `has_blob`'s.
    fn missing(&self, hashes: &BTreeSet<String>) -> BTreeSet<String> {
        let stored: BTreeSet<String> = self
            .stored()
            .iter()
            .map(|file| file.file_name().to_string_lossy().into_owned())
            .collect();
        let absent = hashes
            .iter()
            .filter(|hash| !stored.contains(*hash) && !self.has_blob(hash));
        absent.cloned().collect()
    }

    fn get_blob(&self, hash: &str, dest: &Path) -> io::Result<bool> {
        copy_verified(&self.blob(hash), dest, hash)
    }

    fn read_blob(&self, hash: &str, max: u64) -> Option<Vec<u8>> {
        let src = self.blob(hash);
        let small = src.metadata().is_ok_and(|m| m.is_file() && m.len() <= max);
        let bytes = small.then(|| fs::read(&src).ok()).flatten()?;
        (sha256(&bytes) == hash).then_some(bytes)
    }

    fn put_blob(&self, hash: &str, src: &Path) -> Result<()> {
        let dest = self.blob(hash);
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::rename(src, dest)?;
        Ok(())
    }

    fn drop_blobs_except(&self, keep: &BTreeSet<String>, dry_run: bool) -> Result<(usize, u64)> {
        let mut found = vec![];
        for file in self.stored() {
            if !keep.contains(file.file_name().to_string_lossy().as_ref()) {
                found.push((file.path(), file.metadata()?.len()));
            }
        }
        if !dry_run {
            for (file, _) in &found {
                fs::remove_file(file)?;
                // Fails, harmlessly, while the folder still holds blobs.
                let _ = fs::remove_dir(file.parent().unwrap());
            }
        }
        Ok((found.len(), found.iter().map(|(_, size)| size).sum()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_blobs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = Dir::new(tmp.path());
        let (a, b) = (sha256(b"a"), sha256(b"b"));
        let both = BTreeSet::from([a.clone(), b.clone()]);
        assert_eq!(dir.missing(&both), both, "no blobs folder yet");
        let src = tmp.path().join("src");
        fs::write(&src, "a").unwrap();
        dir.put_blob(&a, &src).unwrap();
        assert!(dir.blob(&a).ends_with(format!("blobs/{}/{a}", &a[..2])) && dir.blob(&a).is_file());
        assert_eq!(dir.missing(&both), BTreeSet::from([b.clone()]));
        dir.drop_blobs_except(&BTreeSet::new(), false).unwrap();
        assert_eq!(dir.missing(&both), both);
        assert!(!tmp.path().join(".syncit/blobs").join(&a[..2]).exists());
        // A folder is not a blob, as `has_blob` says.
        fs::create_dir_all(dir.blob(&b)).unwrap();
        assert_eq!(dir.missing(&both), both);
    }
}
