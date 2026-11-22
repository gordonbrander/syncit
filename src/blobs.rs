//! Blobs: file contents addressed by SHA-256, and where to find them.

use crate::entry::{hex, sha256};
use crate::output::Options;
use crate::vault::{Vault, unique};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// A SHA-256 hasher that bytes can be written to, so that `io::copy` can
/// feed it. sha2's own hashers are not `io::Write` since 0.11.
struct HashWriter(Sha256);

impl Write for HashWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub fn hash_file(file: &Path) -> io::Result<String> {
    let mut hasher = HashWriter(Sha256::new());
    io::copy(&mut File::open(file)?, &mut hasher)?;
    Ok(hex(&hasher.0.finalize()))
}

/// Copy `src` to `dest` while hashing. On a wrong hash or an unreadable
/// source, `dest` is removed and the answer is false.
fn copy_verified(src: &Path, dest: &Path, hash: &str) -> io::Result<bool> {
    let Ok(mut input) = File::open(src) else {
        return Ok(false);
    };
    let mut out = File::create(dest)?;
    let (mut hasher, mut buf) = (Sha256::new(), vec![0u8; 1 << 16]);
    let read_all = loop {
        match input.read(&mut buf) {
            Ok(0) => break true,
            Ok(n) => {
                hasher.update(&buf[..n]);
                out.write_all(&buf[..n])?;
            }
            Err(_) => break false,
        }
    };
    if read_all && hex(&hasher.finalize()) == hash {
        return Ok(true);
    }
    fs::remove_file(dest)?;
    Ok(false)
}

/// Copy the blob to `dest` from the first source that yields the right
/// bytes: this vault's files, its stored blobs, the peer's files, the peer's
/// stored blobs. False means no source has it, which is not an error.
pub fn fetch(v: &Vault, peer: Option<&Vault>, hash: &str, dest: &Path) -> io::Result<bool> {
    for src in sources(v, peer, hash) {
        if copy_verified(&src, dest, hash)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sources(v: &Vault, peer: Option<&Vault>, hash: &str) -> Vec<PathBuf> {
    let mut sources: Vec<PathBuf> = vec![];
    for vault in [Some(v), peer].into_iter().flatten() {
        sources.extend(vault.hints.get(hash).into_iter().flatten().cloned());
        sources.push(vault.dot().join("blobs").join(hash));
    }
    sources
}

/// The blob's bytes, from the first source that yields them and is no larger
/// than `max`. Nothing is written, so a dry run reads as a real one does.
pub fn read(v: &Vault, peer: Option<&Vault>, hash: &str, max: u64) -> Option<Vec<u8>> {
    sources(v, peer, hash).into_iter().find_map(|src| {
        let small = src.metadata().is_ok_and(|m| m.is_file() && m.len() <= max);
        let bytes = small.then(|| fs::read(&src).ok()).flatten()?;
        (sha256(&bytes) == hash).then_some(bytes)
    })
}

/// Whether `fetch` would find the blob, for a dry run.
// ponytail: checks that a source exists, not its bytes; a corrupt source
// reads as available here and as pending in the real run. Hash it if that matters.
pub fn available(v: &Vault, peer: Option<&Vault>, hash: &str) -> bool {
    sources(v, peer, hash).iter().any(|src| src.is_file())
}

/// Collect into `.syncit/blobs/` every version this vault keeps
/// (`Log::keep`) that a source can supply, for the paths it takes: a partial
/// vault does not gather the content of files it has chosen not to have. A
/// vault that keeps no history collects nothing, and so does a dry run.
pub fn retain(opts: &Options, v: &Vault, peer: Option<&Vault>) -> Result<()> {
    if v.config.keep_history == Some(0) || opts.dry_run {
        return Ok(());
    }
    let blobs = v.dot().join("blobs");
    fs::create_dir_all(&blobs)?;
    fs::create_dir_all(v.dot().join("tmp"))?;
    for hash in &v.log.keep(v.config.keep_history, |path| v.takes(path)) {
        if blobs.join(hash).exists() {
            continue;
        }
        let tmp = v.dot().join("tmp").join(unique());
        if fetch(v, peer, hash, &tmp)? {
            fs::rename(&tmp, blobs.join(hash))?;
        }
    }
    Ok(())
}

/// The stored blobs this vault's `keep_history` no longer covers, with their
/// sizes. Every path counts, taken or not, so the content of a current
/// version is never among them: a file removed with `drop` may have its only
/// copy here.
pub fn droppable(v: &Vault) -> Result<Vec<(PathBuf, u64)>> {
    let keep = v.log.keep(v.config.keep_history, |_| true);
    let mut found = vec![];
    let Ok(dir) = fs::read_dir(v.dot().join("blobs")) else {
        return Ok(found);
    };
    for file in dir {
        let file = file?;
        if !keep.contains(file.file_name().to_string_lossy().as_ref()) {
            found.push((file.path(), file.metadata()?.len()));
        }
    }
    Ok(found)
}

/// Compaction: remove the droppable blobs. The log is not touched. Returns
/// how many were removed and their bytes; a dry run removes nothing and
/// returns the same.
pub fn compact(opts: &Options, v: &Vault) -> Result<(usize, u64)> {
    let found = droppable(v)?;
    if !opts.dry_run {
        for (file, _) in &found {
            fs::remove_file(file)?;
        }
    }
    Ok((found.len(), found.iter().map(|(_, size)| size).sum()))
}
