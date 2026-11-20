//! Blobs: file contents addressed by SHA-256, and where to find them.

use crate::entry::hex;
use crate::vault::{TMP_PREFIX, Vault, unique};
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub fn hash_file(file: &Path) -> io::Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(file)?, &mut hasher)?;
    Ok(hex(&hasher.finalize()))
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
/// bytes: this vault's files, its blob store, the peer's files, the peer's
/// blob store, the store. False means no source has it, which is not an error.
pub fn fetch(v: &Vault, peer: Option<&Vault>, hash: &str, dest: &Path) -> io::Result<bool> {
    let mut sources: Vec<PathBuf> = vec![];
    for vault in [Some(v), peer].into_iter().flatten() {
        sources.extend(vault.hints.get(hash).into_iter().flatten().cloned());
        sources.push(vault.dot().join("blobs").join(hash));
    }
    sources.extend(v.store().map(|r| r.join(hash)));
    for src in sources {
        if copy_verified(&src, dest, hash)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Collect into `.syncit/blobs/` every version in the log that a source can
/// supply, if this vault retains blobs.
pub fn retain(v: &Vault, peer: Option<&Vault>) -> Result<()> {
    if !v.config.keep_local_history && v.config.store.is_none() {
        return Ok(());
    }
    let (blobs, store) = (v.dot().join("blobs"), v.store());
    fs::create_dir_all(&blobs)?;
    fs::create_dir_all(v.dot().join("tmp"))?;
    let hashes: BTreeSet<&String> = v
        .log
        .entries
        .values()
        .filter_map(|e| e.hash.as_ref())
        .collect();
    for hash in hashes {
        if blobs.join(hash).exists() || store.as_ref().is_some_and(|r| r.join(hash).exists()) {
            continue;
        }
        let tmp = v.dot().join("tmp").join(unique());
        if fetch(v, peer, hash, &tmp)? {
            fs::rename(&tmp, blobs.join(hash))?;
        }
    }
    Ok(())
}

/// Upload stored blobs to the store, if it is reachable. Unless local
/// history is kept, blobs the store has are then removed from `.syncit/blobs/`.
pub fn flush(v: &Vault) -> Result<()> {
    let (Some(store), Ok(blobs)) = (v.store(), fs::read_dir(v.dot().join("blobs"))) else {
        return Ok(());
    };
    for blob in blobs {
        let blob = blob?;
        let dest = store.join(blob.file_name());
        if !dest.exists() {
            let tmp = store.join(format!("{TMP_PREFIX}{}", unique()));
            fs::copy(blob.path(), &tmp)?;
            fs::rename(&tmp, &dest)?;
        }
        if !v.config.keep_local_history {
            fs::remove_file(blob.path())?;
        }
    }
    Ok(())
}
