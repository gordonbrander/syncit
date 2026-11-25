//! Object storage as a backend (store.md, section 5): a prefix in a bucket,
//! reached by URL, and the credentials for it.

use crate::backend::Backend;
use crate::crypt::Key;
use crate::output::Context;
use crate::sealed::Sealed;
use crate::storage::Store;
use anyhow::{Context as _, Result, anyhow, ensure};
use futures::TryStreamExt;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt, WriteMultipart};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;

/// Uploads of this size or more go in parts of this size.
const PART: usize = 16 << 20;

/// A prefix in a bucket, reached by URL. It stores what it is given: no keys.
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

    /// Whether the prefix holds no object at all.
    pub fn is_empty(&self) -> Result<bool> {
        let prefix = (!self.root.is_empty()).then(|| ObjectPath::from(self.root.as_str()));
        let first = self.rt.block_on(async {
            let mut stream = self.store.list(prefix.as_ref());
            stream.try_next().await
        });
        Ok(first.map_err(|e| self.fail(e))?.is_none())
    }

    /// The encrypted vault here, whose key is `key`.
    pub fn store(self, key: Key) -> Store {
        let url = self.url.clone();
        Store::new(&url, Box::new(Sealed::new(Box::new(self), key)))
    }

    fn put(&self, rel: &str, bytes: Vec<u8>) -> Result<()> {
        let at = self.at(rel);
        self.rt
            .block_on(self.store.put(&at, bytes.into()))
            .map_err(|e| self.fail(e))?;
        Ok(())
    }
}

impl Backend for Bucket {
    fn read(&self, rel: &str) -> Result<Option<Vec<u8>>> {
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

    fn write(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        self.put(rel, bytes.to_vec())
    }

    fn copy_to(&self, rel: &str, dest: &Path) -> Result<bool> {
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

    /// Upload the file: in one request under 16 MiB, otherwise in 16 MiB
    /// parts, aborted on error.
    // ponytail: transfers are sequential. Parallelise if a large first
    // upload is too slow.
    fn move_to(&self, src: &Path, rel: &str) -> Result<()> {
        let size = src.metadata()?.len();
        if size < PART as u64 {
            self.put(rel, fs::read(src)?)?;
            return Ok(fs::remove_file(src)?);
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
        })?;
        Ok(fs::remove_file(src)?)
    }

    fn remove(&self, rel: &str) -> Result<()> {
        match self.rt.block_on(self.store.delete(&self.at(rel))) {
            Err(e) if !not_found(&e) => Err(self.fail(e)),
            _ => Ok(()),
        }
    }

    fn size(&self, rel: &str) -> Result<Option<u64>> {
        match self.rt.block_on(self.store.head(&self.at(rel))) {
            Ok(meta) => Ok(Some(meta.size)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(self.fail(e)),
        }
    }

    fn list(&self, dir: &str) -> Result<BTreeSet<String>> {
        let prefix = self.at(dir);
        let listed: Vec<_> = self
            .rt
            .block_on(self.store.list(Some(&prefix)).try_collect())
            .map_err(|e| self.fail(e))?;
        let strip = format!("{}/", prefix.as_ref());
        Ok(listed
            .into_iter()
            .filter_map(|meta| Some(meta.location.as_ref().strip_prefix(&strip)?.to_string()))
            .collect())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn bucket_backend() {
        let tmp = TempDir::new().unwrap();
        let (r, scratch) = (tmp.path().join("r"), tmp.path().join("s"));
        fs::create_dir(&r).unwrap();
        fs::create_dir(&scratch).unwrap();
        let bucket = || Bucket::new(&format!("file://{}", r.display()), vec![]).unwrap();
        crate::backend::tests::contract(&bucket(), &scratch);
        for dir in [&r, &scratch] {
            fs::remove_dir_all(dir).unwrap();
            fs::create_dir(dir).unwrap();
        }
        let key = zeroize::Zeroizing::new([3u8; 32]);
        let sealed = crate::sealed::Sealed::new(Box::new(bucket()), key);
        crate::backend::tests::contract(&sealed, &scratch);
    }

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
