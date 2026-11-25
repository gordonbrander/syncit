//! Encryption as a backend: `Sealed` stores on another backend, with every
//! name and every file encrypted (store.md, sections 5 and 6). What is above
//! it sees plaintext paths and bytes, as on any backend.

use crate::backend::{Backend, Unreadable};
use crate::crypt::{self, Key};
use crate::migrate::FORMAT;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// The `check` in the `format` of the vault stored on `inner`, which tells
/// its key from another. `format` is the one file read before the key is
/// known, so it is the one file that is not encrypted. `name` is the
/// vault's, for errors.
pub fn check(inner: &impl Backend, name: &str) -> Result<String> {
    let Some(bytes) = inner.read(FORMAT)? else {
        bail!("no vault at {name}: run syncit init {name}");
    };
    let format: Option<serde_json::Value> = serde_json::from_slice(&bytes).ok();
    let check = format
        .as_ref()
        .and_then(|format| format.get("check")?.as_str());
    let check = check.with_context(|| format!("{name}: corrupt format"))?;
    Ok(check.to_string())
}

/// Write the `format` of a new vault whose key is `key`: its `check`. The
/// layout version is added to it when the vault is made (`Store::create`).
pub fn create(inner: &impl Backend, key: &Key) -> Result<()> {
    let format = serde_json::json!({ "check": crypt::check(key) });
    inner.write(FORMAT, (format.to_string() + "\n").as_bytes())
}

/// The backend `B`, with every name and every file encrypted with `key`.
pub struct Sealed<B> {
    inner: B,
    key: Key,
}

/// A scratch file beside `file`, for ciphertext on its way in or out.
fn beside(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(".sealed");
    name.into()
}

impl<B: Backend> Sealed<B> {
    pub fn new(inner: B, key: Key) -> Sealed<B> {
        Sealed { inner, key }
    }

    /// Where `path` is stored: each of its names, encrypted. `format` is
    /// stored as it is.
    fn hide(&self, path: &str) -> String {
        if path == FORMAT {
            return path.to_string();
        }
        let names = path.split('/').filter(|name| !name.is_empty());
        let hidden: Vec<String> = names
            .map(|name| crypt::seal_name(&self.key, name))
            .collect();
        hidden.join("/")
    }

    /// The path stored as `hidden`; None if it is not one of this vault's.
    fn show(&self, hidden: &str) -> Option<String> {
        if hidden == FORMAT {
            return Some(hidden.to_string());
        }
        let names = hidden.split('/');
        let shown: Option<Vec<String>> = names.map(|n| crypt::open_name(&self.key, n)).collect();
        Some(shown?.join("/"))
    }
}

impl<B: Backend> Backend for Sealed<B> {
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
        if path == FORMAT {
            return self.inner.read(path);
        }
        let Some(sealed) = self.inner.read(&self.hide(path))? else {
            return Ok(None);
        };
        let mut plain = vec![];
        match crypt::decrypt(
            &crypt::content_key(&self.key, path),
            &sealed[..],
            &mut plain,
        ) {
            Ok(()) => Ok(Some(plain)),
            Err(_) => Err(Unreadable.into()),
        }
    }

    fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
        if path == FORMAT {
            return self.inner.write(path, bytes);
        }
        let mut sealed = vec![];
        crypt::encrypt(&crypt::content_key(&self.key, path), bytes, &mut sealed)?;
        self.inner.write(&self.hide(path), &sealed)
    }

    fn copy_to(&self, path: &str, dest: &Path) -> Result<bool> {
        if path == FORMAT {
            return self.inner.copy_to(path, dest);
        }
        let sealed = beside(dest);
        if !self.inner.copy_to(&self.hide(path), &sealed)? {
            return Ok(false);
        }
        let key = crypt::content_key(&self.key, path);
        let opened = File::open(&sealed)
            .and_then(|sealed| crypt::decrypt(&key, sealed, File::create(dest)?));
        fs::remove_file(&sealed)?;
        match opened {
            Ok(()) => Ok(true),
            Err(e) => {
                let _ = fs::remove_file(dest);
                match e.kind() {
                    std::io::ErrorKind::InvalidData => Err(Unreadable.into()),
                    _ => Err(e.into()),
                }
            }
        }
    }

    fn move_to(&self, src: &Path, path: &str) -> Result<()> {
        if path == FORMAT {
            return self.inner.move_to(src, path);
        }
        let sealed = beside(src);
        let key = crypt::content_key(&self.key, path);
        let moved = File::open(src)
            .and_then(|src| crypt::encrypt(&key, src, File::create(&sealed)?))
            .map_err(anyhow::Error::from)
            .and_then(|()| self.inner.move_to(&sealed, &self.hide(path)));
        if moved.is_err() {
            let _ = fs::remove_file(&sealed);
        }
        moved?;
        Ok(fs::remove_file(src)?)
    }

    fn remove(&self, path: &str) -> Result<()> {
        self.inner.remove(&self.hide(path))
    }

    /// The size of the plaintext.
    fn size(&self, path: &str) -> Result<Option<u64>> {
        let size = self.inner.size(&self.hide(path))?;
        if path == FORMAT {
            return Ok(size);
        }
        Ok(size.map(|size| crypt::plain_len(size).unwrap_or(size)))
    }

    /// A file under a name this vault's key did not make is not one of its
    /// files, and is left out.
    fn list(&self, dir: &str) -> Result<BTreeSet<String>> {
        let listed = self.inner.list(&self.hide(dir))?;
        Ok(listed
            .iter()
            .filter_map(|hidden| self.show(hidden))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Fs;
    use zeroize::Zeroizing;

    fn key(byte: u8) -> Key {
        Zeroizing::new([byte; 32])
    }

    /// A sealed backend on a folder, and the folder's `.syncit/`.
    fn sealed(tmp: &Path, key: Key) -> (Sealed<Fs>, PathBuf) {
        let root = tmp.join("v");
        fs::create_dir_all(root.join(".syncit")).unwrap();
        let sealed = Sealed::new(Fs::at(&root), key);
        (sealed, root.join(".syncit"))
    }

    #[test]
    fn sealed_backend() {
        let tmp = tempfile::TempDir::new().unwrap();
        let scratch = tmp.path().join("s");
        fs::create_dir(&scratch).unwrap();
        crate::backend::tests::contract(&sealed(tmp.path(), key(1)).0, &scratch);
    }

    #[test]
    fn sealed_is_opaque() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (s, dot) = sealed(tmp.path(), key(1));
        let inner = Fs::at(&tmp.path().join("v"));
        s.write("folder/letter", b"some private words").unwrap();
        fs::write(tmp.path().join("src"), "more private words").unwrap();
        s.move_to(&tmp.path().join("src"), "folder/moved").unwrap();
        let stored = inner.list("").unwrap();
        assert_eq!(stored.len(), 2);
        for name in &stored {
            let bytes = fs::read(dot.join(name)).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("private"));
            for plain in ["folder", "letter", "moved"] {
                assert!(!name.contains(plain), "{name}");
            }
        }
        // Another key sees none of it.
        let other = Sealed::new(Fs::at(&tmp.path().join("v")), key(2));
        assert!(other.list("folder").unwrap().is_empty());
        assert_eq!(other.read("folder/letter").unwrap(), None);
    }

    #[test]
    fn sealed_rejects() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (s, dot) = sealed(tmp.path(), key(1));
        let out = tmp.path().join("out");
        let unreadable = |path: &str| {
            let read = s.read(path).unwrap_err();
            let copied = s.copy_to(path, &out).unwrap_err();
            assert!(
                read.is::<Unreadable>() && copied.is::<Unreadable>(),
                "{path}"
            );
            assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1, "nothing left");
        };
        s.write("log/good", b"a segment").unwrap();
        let at = |path: &str| dot.join(s.hide(path));
        // Someone else's file, under a name that is not ours: not listed.
        fs::write(at("log").join("not-a-name"), "x").unwrap();
        // Garbage under one of our names, and a good file under another of
        // our names: listed, and unreadable.
        fs::write(at("log/junk"), "garbage").unwrap();
        fs::copy(at("log/good"), at("log/copy")).unwrap();
        // A good file cut short.
        s.write("log/cut", &[5u8; 1000]).unwrap();
        let bytes = fs::read(at("log/cut")).unwrap();
        fs::write(at("log/cut"), &bytes[..bytes.len() - 10]).unwrap();
        let listed: Vec<String> = s.list("log").unwrap().into_iter().collect();
        assert_eq!(listed, ["copy", "cut", "good", "junk"]);
        for path in ["log/junk", "log/copy", "log/cut"] {
            unreadable(path);
        }
        assert_eq!(s.read("log/good").unwrap().unwrap(), b"a segment");
    }

    #[test]
    fn sealed_format() {
        let tmp = tempfile::TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".syncit")).unwrap();
        let inner = Fs::at(tmp.path());
        let none = check(&inner, "there").unwrap_err().to_string();
        assert_eq!(none, "no vault at there: run syncit init there");
        create(&inner, &key(1)).unwrap();
        assert_eq!(check(&inner, "there").unwrap(), crypt::check(&key(1)));
        let format = tmp.path().join(".syncit/format");
        let text = fs::read_to_string(&format).unwrap();
        assert!(text.starts_with("{\"check\":\"") && text.ends_with("\"}\n"));
        // It is the one file a sealed backend stores as it is, and the
        // layout version goes into it beside the check.
        let s = Sealed::new(Fs::at(tmp.path()), key(1));
        assert_eq!(s.read(FORMAT).unwrap().unwrap(), text.as_bytes());
        crate::migrate::create(&s).unwrap();
        let text = fs::read_to_string(&format).unwrap();
        let version = crate::migrate::VERSION;
        assert!(
            text.ends_with(&format!("\",\"syncit\":{version}}}\n")),
            "{text}"
        );
        assert_eq!(check(&inner, "there").unwrap(), crypt::check(&key(1)));
        assert!(s.list("").unwrap().contains(FORMAT));
        assert_eq!(s.size(FORMAT).unwrap(), Some(text.len() as u64));
        for junk in ["junk", "{\"syncit\":2}"] {
            fs::write(&format, junk).unwrap();
            assert!(check(&inner, "there").is_err(), "{junk}");
        }
    }
}
