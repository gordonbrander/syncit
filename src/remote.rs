//! A remote vault: its config, log and blobs as encrypted objects on object
//! storage (store.md, sections 5 and 6). Keyed names and ciphertext never
//! leave this module.

use crate::blobs::hash_file;
use crate::config::Config;
use crate::crypt::{self, Key};
use crate::entry::{is_hash, sha256};
use crate::output::Context;
use crate::storage::{Dir, Storage};
use crate::vault::unique;
use anyhow::{Context as _, Result, anyhow, bail, ensure};
use futures::TryStreamExt;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, WriteMultipart};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// Uploads of this size or more go in parts of this size.
const PART: usize = 16 << 20;

/// The plaintext `format` object.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Format {
    check: String,
    syncit: u32,
}

/// A prefix in a bucket, reached by URL. Plain objects only: no keys.
pub struct Bucket {
    pub url: String,
    store: Box<dyn ObjectStore>,
    root: String,
    rt: tokio::runtime::Runtime,
}

fn not_found(e: &object_store::Error) -> bool {
    matches!(e, object_store::Error::NotFound { .. })
}

/// A TOML file the user may edit; a missing one is empty. `what` names it in
/// an error.
pub(crate) fn read_toml(path: &Path, what: &str) -> Result<toml_edit::DocumentMut> {
    match fs::read_to_string(path) {
        Ok(text) => text
            .parse()
            .with_context(|| format!("corrupt {what} {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Default::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

/// Settings, as name and value.
type Settings = Vec<(String, String)>;

/// The `[[remotes]]` entries of a `credentials.toml`: each one's `url`
/// prefix, and its other settings. Entries are named by position in errors.
fn sections(path: &Path) -> Result<Vec<(String, Settings)>> {
    let doc = read_toml(path, "credentials")?;
    let corrupt = |why: String| anyhow!("corrupt credentials {}: {why}", path.display());
    let Some(remotes) = doc.get("remotes") else {
        return Ok(vec![]);
    };
    let remotes = remotes
        .as_array_of_tables()
        .ok_or_else(|| corrupt("remotes is not [[remotes]] entries".into()))?;
    let mut found: Vec<(String, Settings)> = vec![];
    for (i, entry) in remotes.iter().enumerate() {
        let n = i + 1;
        let url = entry
            .get("url")
            .ok_or_else(|| corrupt(format!("entry {n} has no url")))?
            .as_str()
            .ok_or_else(|| corrupt(format!("url in entry {n} is not a string")))?;
        let same = |u: &str| u.trim_end_matches('/') == url.trim_end_matches('/');
        if let Some(m) = found.iter().position(|(u, _)| same(u)) {
            return Err(corrupt(format!(
                "entries {} and {n} have the same url {url}",
                m + 1
            )));
        }
        let mut settings = vec![];
        for (name, value) in entry.iter().filter(|(name, _)| *name != "url") {
            let value = value
                .as_str()
                .ok_or_else(|| corrupt(format!("{name} in entry {n} is not a string")))?;
            settings.push((name.to_string(), value.to_string()));
        }
        found.push((url.to_string(), settings));
    }
    Ok(found)
}

/// Whether the section `prefix` applies to `url`: "" applies to all, and a
/// prefix otherwise ends where the URL does or at a `/`.
fn matches(prefix: &str, url: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    p.is_empty()
        || url
            .strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Settings for the bucket at `url`: the `[[remotes]]` entries of the
/// user's `credentials.toml` whose `url` is a prefix of it, from the
/// shortest prefix to the longest. A later one wins.
pub fn credentials(ctx: &Context, url: &str) -> Result<Settings> {
    let Some(dir) = &ctx.config_dir else {
        return Ok(vec![]);
    };
    let mut found: Vec<_> = sections(&dir.join("credentials.toml"))?
        .into_iter()
        .filter(|(p, _)| matches(p, url))
        .collect();
    found.sort_by_key(|(p, _)| p.trim_end_matches('/').len());
    Ok(found.into_iter().flat_map(|(_, s)| s).collect())
}

impl Bucket {
    /// Reach the prefix at `url`, with `settings` (from `credentials`), then
    /// the environment, which wins. Nothing is read yet.
    pub fn new(url: &str, settings: Vec<(String, String)>) -> Result<Bucket> {
        let parsed = url::Url::parse(url).with_context(|| format!("not a URL: {url}"))?;
        // A file:// backend creates directories as it writes; syncit never
        // creates one.
        if parsed.scheme() == "file" {
            let dir = parsed.to_file_path().ok().context("not a local path")?;
            ensure!(dir.is_dir(), "cannot reach {url}: no such directory");
        }
        let settings = settings.into_iter().chain(std::env::vars());
        let (store, root) = object_store::parse_url_opts(&parsed, settings)
            .with_context(|| format!("cannot reach {url}"))?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        Ok(Bucket {
            url: url.to_string(),
            store,
            root: root.as_ref().to_string(),
            rt,
        })
    }

    fn at(&self, rel: &str) -> ObjectPath {
        match self.root.is_empty() {
            true => ObjectPath::from(rel),
            false => ObjectPath::from(format!("{}/{rel}", self.root)),
        }
    }

    fn fail(&self, e: object_store::Error) -> anyhow::Error {
        anyhow!("cannot reach {}: {e}", self.url)
    }

    /// The object's bytes; None if there is none.
    fn get(&self, rel: &str) -> Result<Option<Vec<u8>>> {
        let got = self.rt.block_on(async {
            let got = self.store.get(&self.at(rel)).await?;
            got.bytes().await
        });
        match got {
            Ok(bytes) => Ok(Some(bytes.to_vec())),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(self.fail(e)),
        }
    }

    /// Copy the object into `dest`. False if there is none.
    fn get_to_file(&self, rel: &str, dest: &Path) -> Result<bool> {
        let mut out = File::create(dest)?;
        let got = self.rt.block_on(async {
            let mut stream = self.store.get(&self.at(rel)).await?.into_stream();
            while let Some(bytes) = stream.try_next().await? {
                io::Write::write_all(&mut out, &bytes).map_err(|e| {
                    object_store::Error::Generic {
                        store: "file",
                        source: e.into(),
                    }
                })?;
            }
            Ok(())
        });
        match got {
            Ok(()) => Ok(true),
            Err(e) => {
                let _ = fs::remove_file(dest);
                if not_found(&e) {
                    Ok(false)
                } else {
                    Err(self.fail(e))
                }
            }
        }
    }

    fn put(&self, rel: &str, bytes: Vec<u8>) -> Result<()> {
        let at = self.at(rel);
        self.rt
            .block_on(self.store.put(&at, bytes.into()))
            .map_err(|e| self.fail(e))?;
        Ok(())
    }

    /// Upload a file: in one request under 16 MiB, otherwise in 16 MiB
    /// parts, aborted on error.
    // ponytail: transfers are sequential. Parallelise if a large first
    // upload is too slow.
    fn put_file(&self, rel: &str, src: &Path) -> Result<()> {
        let size = src.metadata()?.len();
        if size < PART as u64 {
            return self.put(rel, fs::read(src)?);
        }
        let mut file = File::open(src)?;
        self.rt.block_on(async {
            let upload = self.store.put_multipart(&self.at(rel)).await;
            let upload = upload.map_err(|e| self.fail(e))?;
            let mut writer = WriteMultipart::new_with_chunk_size(upload, PART);
            let mut buf = vec![0u8; PART];
            let sent: Result<()> = async {
                loop {
                    let n = read_up_to(&mut file, &mut buf)?;
                    if n == 0 {
                        return Ok(());
                    }
                    writer
                        .wait_for_capacity(1)
                        .await
                        .map_err(|e| self.fail(e))?;
                    writer.write(&buf[..n]);
                }
            }
            .await;
            match sent {
                Ok(()) => writer.finish().await.map(|_| ()).map_err(|e| self.fail(e)),
                Err(e) => {
                    let _ = writer.abort().await;
                    Err(e)
                }
            }
        })
    }

    fn delete(&self, rel: &str) -> Result<()> {
        let at = self.at(rel);
        self.rt
            .block_on(self.store.delete(&at))
            .map_err(|e| self.fail(e))
    }

    /// The objects under `dir`, by name relative to it, with their sizes.
    fn list(&self, dir: &str) -> Result<BTreeMap<String, u64>> {
        let prefix = self.at(dir);
        let listed: Vec<_> = self
            .rt
            .block_on(self.store.list(Some(&prefix)).try_collect())
            .map_err(|e| self.fail(e))?;
        let strip = format!("{}/", prefix.as_ref());
        Ok(listed
            .into_iter()
            .filter_map(|meta| {
                let name = meta.location.as_ref().strip_prefix(&strip)?.to_string();
                Some((name, meta.size))
            })
            .collect())
    }

    /// Whether the prefix holds no object at all.
    pub fn is_empty(&self) -> Result<bool> {
        let prefix = (!self.root.is_empty()).then(|| ObjectPath::from(self.root.as_str()));
        let first = self.rt.block_on(async {
            let mut stream = self.store.list(prefix.as_ref());
            stream.try_next().await
        });
        Ok(first.map_err(|e| self.fail(e))?.is_none())
    }

    /// The `check` in the vault's `format`.
    pub fn check(&self) -> Result<String> {
        let Some(bytes) = self.get("format")? else {
            bail!("no vault at {0}: run syncit init {0}", self.url);
        };
        let format: Format = serde_json::from_slice(&bytes)
            .with_context(|| format!("{}: corrupt format", self.url))?;
        ensure!(
            format.syncit == 1,
            "{} has format version {}, not 1",
            self.url,
            format.syncit
        );
        Ok(format.check)
    }

    /// Make a vault here: `format`, then `config`.
    pub fn create(&self, key: &Key, config: &Config) -> Result<()> {
        let format = Format {
            check: crypt::check(key),
            syncit: 1,
        };
        self.put(
            "format",
            (serde_json::to_string(&format)? + "\n").into_bytes(),
        )?;
        self.put("config", seal_config(key, config)?)
    }
}

/// Read until `buf` is full or the input ends.
fn read_up_to(input: &mut impl Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match input.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn seal(key: &Key, plain: &[u8]) -> Result<Vec<u8>> {
    let mut out = vec![];
    crypt::encrypt(key, plain, &mut out)?;
    Ok(out)
}

fn open(key: &Key, sealed: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = vec![];
    crypt::decrypt(key, sealed, &mut out)?;
    Ok(out)
}

fn seal_config(key: &Key, config: &Config) -> Result<Vec<u8>> {
    let text = serde_json::to_string_pretty(config)? + "\n";
    seal(&crypt::config_key(key), text.as_bytes())
}

/// The vault's config, read with its key.
pub fn read_config(bucket: &Bucket, key: &Key) -> Result<Config> {
    let url = &bucket.url;
    let sealed = bucket.get("config")?;
    let sealed = sealed.with_context(|| format!("{url} has no config"))?;
    let plain = open(&crypt::config_key(key), &sealed)
        .with_context(|| format!("{url}: cannot decrypt its config"))?;
    serde_json::from_slice(&plain).with_context(|| format!("corrupt config {url}"))
}

/// A segment of the remote, by plaintext name.
enum Segment {
    /// The local vault has it.
    Local,
    Bytes(Vec<u8>),
    /// An object that is not a segment of this vault, listed under its
    /// object name so that a run reports it.
    Bad(&'static str),
}

/// A remote vault's storage.
pub struct Remote {
    bucket: Bucket,
    key: Key,
    /// The vault the command runs in: its segments, and its `.syncit/tmp`
    /// for plaintext in transit.
    local: Option<Dir>,
    segments: RefCell<BTreeMap<String, Segment>>,
    /// Object names under `log/`.
    log: RefCell<BTreeSet<String>>,
    /// Object names under `blobs/`, with their sizes.
    blobs: RefCell<BTreeMap<String, u64>>,
}

/// Where a blob object is: `blobs/ab/abcdef…`, as in a vault's `.syncit/`.
fn blob_path(object: &str) -> String {
    format!("blobs/{}/{object}", &object[..2])
}

impl Remote {
    /// List the vault, and download and check the segments the local vault
    /// does not have (store.md, section 6, "Segment names in a remote").
    pub fn open(bucket: Bucket, key: Key, local: Option<Dir>) -> Result<Remote> {
        let hashed = |list: BTreeMap<String, u64>| -> BTreeMap<String, u64> {
            list.into_iter().filter(|(n, _)| is_hash(n)).collect()
        };
        let log: BTreeSet<String> = hashed(bucket.list("log")?).into_keys().collect();
        let blobs = bucket.list("blobs")?.into_iter();
        // Listed as `ab/<name>`; known by the name alone.
        let blobs = hashed(
            blobs
                .map(|(n, s)| (n.rsplit('/').next().unwrap_or("").into(), s))
                .collect(),
        );
        let known: BTreeMap<String, String> = match &local {
            Some(dir) => dir.segment_names()?,
            None => BTreeSet::new(),
        }
        .into_iter()
        .map(|s| (crypt::name(&key, "log", &s), s))
        .collect();
        let mut segments = BTreeMap::new();
        for object in &log {
            if let Some(name) = known.get(object) {
                segments.insert(name.clone(), Segment::Local);
                continue;
            }
            let sealed = bucket.get(&format!("log/{object}"))?.unwrap_or_default();
            let opened = open(&crypt::object_key(&key, "log", object), &sealed);
            let (name, segment) = match opened {
                Err(_) => (object.clone(), Segment::Bad("cannot decrypt")),
                Ok(plain) => {
                    let name = sha256(&plain);
                    match crypt::name(&key, "log", &name) == *object {
                        true => (name, Segment::Bytes(plain)),
                        false => (object.clone(), Segment::Bad("stored under another name")),
                    }
                }
            };
            segments.insert(name, segment);
        }
        Ok(Remote {
            bucket,
            key,
            local,
            segments: RefCell::new(segments),
            log: RefCell::new(log),
            blobs: RefCell::new(blobs),
        })
    }

    fn blob_name(&self, hash: &str) -> String {
        crypt::name(&self.key, "blobs", hash)
    }

    fn scratch(&self) -> io::Result<PathBuf> {
        let dir = self.local.as_ref().map(|d| d.root.join(".syncit/tmp"));
        let dir = dir.ok_or_else(|| io::Error::other("no local vault for files in transit"))?;
        fs::create_dir_all(&dir)?;
        Ok(dir.join(unique()))
    }
}

impl Storage for Remote {
    fn load_config(&self) -> Result<Config> {
        read_config(&self.bucket, &self.key)
    }

    fn save_config(&self, config: &Config) -> Result<()> {
        self.bucket.put("config", seal_config(&self.key, config)?)
    }

    fn segment_names(&self) -> Result<BTreeSet<String>> {
        Ok(self.segments.borrow().keys().cloned().collect())
    }

    fn read_segment(&self, name: &str) -> Result<Vec<u8>> {
        match self.segments.borrow().get(name) {
            Some(Segment::Bytes(bytes)) => Ok(bytes.clone()),
            Some(Segment::Local) => self
                .local
                .as_ref()
                .context("no local vault")?
                .read_segment(name),
            Some(Segment::Bad(why)) => Err(anyhow!("{why}")),
            None => Err(anyhow!("no such segment")),
        }
    }

    fn write_segment(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let object = crypt::name(&self.key, "log", name);
        if !self.log.borrow().contains(&object) {
            let sealed = seal(&crypt::object_key(&self.key, "log", &object), bytes)?;
            self.bucket.put(&format!("log/{object}"), sealed)?;
            self.log.borrow_mut().insert(object);
        }
        let segment = Segment::Bytes(bytes.to_vec());
        self.segments.borrow_mut().insert(name.to_string(), segment);
        Ok(())
    }

    fn has_blob(&self, hash: &str) -> bool {
        self.blobs.borrow().contains_key(&self.blob_name(hash))
    }

    fn get_blob(&self, hash: &str, dest: &Path) -> io::Result<bool> {
        if !self.has_blob(hash) {
            return Ok(false);
        }
        let object = self.blob_name(hash);
        let sealed = self.scratch()?;
        let got = self.bucket.get_to_file(&blob_path(&object), &sealed);
        if !got.map_err(io::Error::other)? {
            return Ok(false);
        }
        let key = crypt::object_key(&self.key, "blobs", &object);
        let opened = crypt::decrypt(&key, File::open(&sealed)?, File::create(dest)?);
        fs::remove_file(&sealed)?;
        if opened.is_ok() && hash_file(dest)? == hash {
            return Ok(true);
        }
        fs::remove_file(dest)?;
        Ok(false)
    }

    fn read_blob(&self, hash: &str, max: u64) -> Option<Vec<u8>> {
        let object = self.blob_name(hash);
        let size = *self.blobs.borrow().get(&object)?;
        if crypt::plain_len(size)? > max {
            return None;
        }
        let sealed = self.bucket.get(&blob_path(&object)).ok()??;
        let plain = open(&crypt::object_key(&self.key, "blobs", &object), &sealed).ok()?;
        (sha256(&plain) == hash).then_some(plain)
    }

    fn put_blob(&self, hash: &str, src: &Path) -> Result<()> {
        let object = self.blob_name(hash);
        if !self.has_blob(hash) {
            let sealed = self.scratch()?;
            let key = crypt::object_key(&self.key, "blobs", &object);
            let done = crypt::encrypt(&key, File::open(src)?, File::create(&sealed)?)
                .map_err(anyhow::Error::from)
                .and_then(|_| self.bucket.put_file(&blob_path(&object), &sealed));
            let size = sealed.metadata().map(|m| m.len());
            let _ = fs::remove_file(&sealed);
            done?;
            self.blobs.borrow_mut().insert(object, size?);
        }
        fs::remove_file(src)?;
        Ok(())
    }

    fn drop_blobs_except(&self, keep: &BTreeSet<String>, dry_run: bool) -> Result<(usize, u64)> {
        let keep: BTreeSet<String> = keep.iter().map(|h| self.blob_name(h)).collect();
        let found: Vec<(String, u64)> = self
            .blobs
            .borrow()
            .iter()
            .filter(|(object, _)| !keep.contains(*object))
            .map(|(object, size)| (object.clone(), crypt::plain_len(*size).unwrap_or(*size)))
            .collect();
        if !dry_run {
            for (object, _) in &found {
                self.bucket.delete(&blob_path(object))?;
                self.blobs.borrow_mut().remove(object);
            }
        }
        Ok((found.len(), found.iter().map(|(_, size)| size).sum()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// Context whose syncit folder is `dir`.
    fn ctx(dir: &Path) -> Context {
        Context {
            json: false,
            dry_run: false,
            no_remove: false,
            secret_key: None,
            bar: crate::output::bar(false),
            config_dir: Some(dir.to_path_buf()),
        }
    }

    #[test]
    fn credentials_match() {
        let tmp = TempDir::new().unwrap();
        // Each entry sets s to its own url, so the order shows; the file
        // lists them out of order.
        let entry = |url: &str| format!("[[remotes]]\nurl = \"{url}\"\ns = \"{url}\"\n");
        let text = ["s3://b/x/", "s3://b2", "s3://b"].map(entry).join("\n");
        fs::write(
            tmp.path().join("credentials.toml"),
            "# by hand\n".to_string() + &text,
        )
        .unwrap();
        let o = ctx(tmp.path());
        let got = credentials(&o, "s3://b/x/y").unwrap();
        assert_eq!(got, pairs(&[("s", "s3://b"), ("s", "s3://b/x/")]));
        let got = credentials(&o, "s3://b").unwrap();
        assert_eq!(got, pairs(&[("s", "s3://b")]));
        assert!(credentials(&o, "s3://other").unwrap().is_empty());
    }

    #[test]
    fn credentials_errors() {
        let tmp = TempDir::new().unwrap();
        let o = ctx(tmp.path());
        assert!(credentials(&o, "s3://b").unwrap().is_empty());
        let file = tmp.path().join("credentials.toml");
        for (text, why) in [
            ("not toml", ""),
            ("remotes = 1", ": remotes is not [[remotes]] entries"),
            (
                "[remotes]\nurl = \"s3://b\"",
                ": remotes is not [[remotes]] entries",
            ),
            ("[[remotes]]\na = \"x\"", ": entry 1 has no url"),
            ("[[remotes]]\nurl = 1", ": url in entry 1 is not a string"),
            (
                "[[remotes]]\nurl = \"s3://b\"\na = 1",
                ": a in entry 1 is not a string",
            ),
            (
                "[[remotes]]\nurl = \"s3://b\"\n[[remotes]]\nurl = \"s3://c\"\n[[remotes]]\nurl = \"s3://b/\"",
                ": entries 1 and 3 have the same url s3://b/",
            ),
        ] {
            fs::write(&file, text).unwrap();
            let err = credentials(&o, "s3://b").unwrap_err().to_string();
            let want = format!("corrupt credentials {}{why}", file.display());
            assert!(err.starts_with(&want), "{text}: {err}");
        }
    }
}
