//! What a vault is stored on: bytes at paths, and nothing about vaults
//! (store.md, section 4). `Fs` is a vault folder's `.syncit/`.

use crate::vault::write_atomic;
use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

/// Paths are relative, `/`-separated, and name files only: a folder exists
/// while it holds a file.
pub trait Backend {
    /// The bytes at `path`; None if nothing is there.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>>;
    /// Whole or not at all; replaces what was there.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<()>;
    /// Copy what is at `path` to the local file `dest`. False if nothing is
    /// there, and `dest` is then not made.
    fn copy_to(&self, path: &str, dest: &Path) -> Result<bool>;
    /// Move the local file `src` to `path`. `src` is gone afterwards.
    fn move_to(&self, src: &Path, path: &str) -> Result<()>;
    /// Not an error if nothing is there.
    fn remove(&self, path: &str) -> Result<()>;
    /// The size of what is at `path`; None if nothing is there.
    fn size(&self, path: &str) -> Result<Option<u64>>;
    /// Every file under `dir`, by its path relative to `dir`.
    fn list(&self, dir: &str) -> Result<BTreeSet<String>>;
}

/// The error of a read that found something other than what was written:
/// changed, cut short, or put there by someone else.
#[derive(Debug)]
pub struct Unreadable;

impl std::fmt::Display for Unreadable {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("cannot decrypt")
    }
}

impl std::error::Error for Unreadable {}

/// The `.syncit/` of the vault folder `root`.
pub struct Fs {
    root: PathBuf,
}

fn missing(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound
}

impl Fs {
    pub fn new(root: &Path) -> Fs {
        Fs {
            root: root.to_path_buf(),
        }
    }

    fn dot(&self) -> PathBuf {
        self.root.join(".syncit")
    }

    /// The file for `path`. A path can come from a listing someone else
    /// wrote, so one that would leave `.syncit/` is refused.
    fn at(&self, path: &str) -> Result<PathBuf> {
        let mut file = self.dot();
        for part in path.split('/').filter(|_| !path.is_empty()) {
            let mut parts = Path::new(part).components();
            let plain = matches!(parts.next(), Some(Component::Normal(n)) if n == part);
            ensure!(plain && parts.next().is_none(), "not a path: {path}");
            file.push(part);
        }
        Ok(file)
    }
}

impl Backend for Fs {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        let file = self.at(path)?;
        match fs::read(&file) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
        }
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        let dest = self.at(path)?;
        fs::create_dir_all(dest.parent().unwrap())?;
        write_atomic(&self.root, &dest, bytes)
    }

    fn copy_to(&self, path: &str, dest: &Path) -> Result<bool> {
        let file = self.at(path)?;
        let mut src = match File::open(&file) {
            Ok(src) => src,
            Err(e) if missing(&e) => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
        };
        io::copy(&mut src, &mut File::create(dest)?)
            .with_context(|| format!("cannot read {}", file.display()))?;
        Ok(true)
    }

    fn move_to(&self, src: &Path, path: &str) -> Result<()> {
        let dest = self.at(path)?;
        fs::create_dir_all(dest.parent().unwrap())?;
        fs::rename(src, &dest).with_context(|| format!("cannot write {}", dest.display()))
    }

    fn remove(&self, path: &str) -> Result<()> {
        let file = self.at(path)?;
        match fs::remove_file(&file) {
            Err(e) if !missing(&e) => {
                return Err(e).with_context(|| format!("cannot remove {}", file.display()));
            }
            _ => {}
        }
        // A folder exists while it holds a file. The folders directly in
        // `.syncit/` are the vault's own and stay. `remove_dir` fails,
        // harmlessly, on a folder that still holds something.
        let dot = self.dot();
        let mut dir = file.parent();
        while let Some(d) = dir
            && d.parent().is_some_and(|up| up != dot)
            && d != dot
            && fs::remove_dir(d).is_ok()
        {
            dir = d.parent();
        }
        Ok(())
    }

    fn size(&self, path: &str) -> Result<Option<u64>> {
        let file = self.at(path)?;
        match fs::metadata(&file) {
            Ok(meta) => Ok(meta.is_file().then_some(meta.len())),
            Err(e) if missing(&e) => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", file.display())),
        }
    }

    /// One read of each folder, and no look at any file.
    fn list(&self, dir: &str) -> Result<BTreeSet<String>> {
        let mut found = BTreeSet::new();
        let mut dirs = vec![(self.at(dir)?, String::new())];
        while let Some((dir, prefix)) = dirs.pop() {
            let cannot = || format!("cannot read {}", dir.display());
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if missing(&e) => continue,
                Err(e) => return Err(e).with_context(cannot),
            };
            for entry in entries {
                let entry = entry.with_context(cannot)?;
                let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
                let kind = entry.file_type().with_context(cannot)?;
                if kind.is_dir() {
                    dirs.push((entry.path(), name + "/"));
                } else if kind.is_file() {
                    found.insert(name);
                }
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// What every backend does. `scratch` is a folder outside it.
    pub(crate) fn contract(b: &dyn Backend, scratch: &Path) {
        let list = |dir: &str| b.list(dir).unwrap().into_iter().collect::<Vec<_>>();
        let (src, out) = (scratch.join("src"), scratch.join("out"));
        // Nothing there.
        assert_eq!(b.read("a/x").unwrap(), None);
        assert!(list("a").is_empty());
        assert_eq!(b.size("a/x").unwrap(), None);
        assert!(!b.copy_to("a/x", &out).unwrap());
        assert!(!out.exists());
        b.remove("a/x").unwrap();
        // Bytes.
        b.write("a/x", b"one").unwrap();
        b.write("a/x", b"three").unwrap();
        assert_eq!(b.read("a/x").unwrap().unwrap(), b"three");
        assert_eq!(b.size("a/x").unwrap(), Some(5));
        b.write("empty", b"").unwrap();
        assert_eq!(b.read("empty").unwrap().unwrap(), b"");
        assert_eq!(b.size("empty").unwrap(), Some(0));
        // Files.
        let big: Vec<u8> = (0..200_000).map(|i| (i % 251) as u8).collect();
        fs::write(&src, &big).unwrap();
        b.move_to(&src, "a/b/y").unwrap();
        assert!(!src.exists(), "moved, not copied");
        assert!(b.copy_to("a/b/y", &out).unwrap());
        assert_eq!(fs::read(&out).unwrap(), big);
        assert_eq!(b.read("a/b/y").unwrap().unwrap(), big);
        assert_eq!(b.size("a/b/y").unwrap(), Some(200_000));
        assert_eq!(fs::read_dir(scratch).unwrap().count(), 1, "only `out`");
        // Listing.
        assert_eq!(list("a"), ["b/y", "x"]);
        assert_eq!(list("a/b"), ["y"]);
        b.remove("a/b/y").unwrap();
        assert_eq!(list("a"), ["x"]);
        assert_eq!(b.read("a/b/y").unwrap(), None);
    }

    #[test]
    fn fs_backend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (root, scratch) = (tmp.path().join("v"), tmp.path().join("s"));
        fs::create_dir_all(root.join(".syncit")).unwrap();
        fs::create_dir(&scratch).unwrap();
        let b = Fs::new(&root);
        contract(&b, &scratch);
        // Emptied folders go, but not those directly in `.syncit/`.
        assert!(root.join(".syncit/a").is_dir() && !root.join(".syncit/a/b").exists());
        b.remove("a/x").unwrap();
        assert!(root.join(".syncit/a").is_dir());
        // A folder is not a file.
        fs::create_dir_all(root.join(".syncit/a/dir/inner")).unwrap();
        assert!(b.list("a").unwrap().is_empty());
        assert_eq!(b.size("a/dir").unwrap(), None);
        // Nothing outside `.syncit/` is reached.
        fs::write(root.join("outside"), "x").unwrap();
        for path in ["../outside", "a/../../outside", "a//x", "./a", "/outside"] {
            assert!(b.read(path).is_err(), "{path}");
            assert!(b.write(path, b"y").is_err(), "{path}");
            assert!(b.remove(path).is_err(), "{path}");
            assert!(b.list(path).is_err(), "{path}");
            assert!(b.size(path).is_err(), "{path}");
        }
        assert_eq!(fs::read_to_string(root.join("outside")).unwrap(), "x");
    }
}
