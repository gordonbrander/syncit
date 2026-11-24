//! Blobs: file contents addressed by SHA-256, and where to find them.

use crate::entry::sha256;
use crate::output::{Context, Progress};
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
    Ok(base16ct::lower::encode_string(&hasher.0.finalize()))
}

/// Copy `src` to `dest` while hashing. On a wrong hash or an unreadable
/// source, `dest` is removed and the answer is false.
pub(crate) fn copy_verified(src: &Path, dest: &Path, hash: &str) -> io::Result<bool> {
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
    if read_all && base16ct::lower::encode_string(&hasher.finalize()) == hash {
        return Ok(true);
    }
    fs::remove_file(dest)?;
    Ok(false)
}

/// The vaults a run can take content from, in order: this one, then the peer.
fn vaults<'a>(v: &'a Vault, peer: Option<&'a Vault>) -> impl Iterator<Item = &'a Vault> {
    [Some(v), peer].into_iter().flatten()
}

fn hints<'a>(v: &'a Vault, hash: &str) -> impl Iterator<Item = &'a PathBuf> {
    v.hints.get(hash).into_iter().flatten()
}

/// Note a blob taken from a bare peer's storage, for the report of a run.
fn received(peer: Option<&Vault>, vault: &Vault, hash: &str) {
    if peer.is_some_and(|p| std::ptr::eq(p, vault)) && vault.bare {
        let mut traffic = vault.traffic.borrow_mut();
        traffic.received_files.insert(hash.to_string());
    }
}

/// Copy the blob to `dest` from the first source that yields the right
/// bytes: this vault's files, its storage, the peer's files, the peer's
/// storage. False means no source has it, which is not an error.
pub fn fetch(v: &Vault, peer: Option<&Vault>, hash: &str, dest: &Path) -> io::Result<bool> {
    for vault in vaults(v, peer) {
        for src in hints(vault, hash) {
            if copy_verified(src, dest, hash)? {
                return Ok(true);
            }
        }
        if vault.storage.get_blob(hash, dest)? {
            received(peer, vault, hash);
            return Ok(true);
        }
    }
    Ok(false)
}

/// The blob's bytes, from the first source that yields them and is no larger
/// than `max`. Nothing is written, so a dry run reads as a real one does.
pub fn read(v: &Vault, peer: Option<&Vault>, hash: &str, max: u64) -> Option<Vec<u8>> {
    vaults(v, peer).find_map(|vault| {
        let file = hints(vault, hash).find_map(|src| {
            let small = src.metadata().is_ok_and(|m| m.is_file() && m.len() <= max);
            let bytes = small.then(|| fs::read(src).ok()).flatten()?;
            (sha256(&bytes) == hash).then_some(bytes)
        });
        let stored = || {
            let bytes = vault.storage.read_blob(hash, max)?;
            received(peer, vault, hash);
            Some(bytes)
        };
        file.or_else(stored)
    })
}

/// Whether `fetch` would find the blob, for a dry run.
// ponytail: checks that a source exists, not its bytes; a corrupt source
// reads as available here and as pending in the real run. Hash it if that matters.
pub fn available(v: &Vault, peer: Option<&Vault>, hash: &str) -> bool {
    vaults(v, peer).any(|vault| {
        let stored = || {
            let has = vault.storage.has_blob(hash);
            if has {
                received(peer, vault, hash);
            }
            has
        };
        hints(vault, hash).any(|src| src.is_file()) || stored()
    })
}

/// Put into the vault's storage every version this vault keeps
/// (`Log::keep`) that a source can supply, for the paths it takes: a partial
/// vault does not gather the content of files it has chosen not to have. A
/// vault with a working folder that keeps no history collects nothing, and
/// a dry run stores nothing. A bare vault's dirstate is derived afresh.
pub fn retain(ctx: &Context, v: &mut Vault, peer: Option<&Vault>) -> Result<()> {
    let keep = v.log.keep(v.config.keep_history, |path| v.takes(path));
    if ctx.dry_run && v.bare {
        let mut sending = v.storage.missing(&keep);
        sending.retain(|hash| available(v, peer, hash));
        v.traffic.borrow_mut().sent_files.extend(sending);
    } else if !ctx.dry_run && (v.bare || v.config.keep_history != Some(0)) {
        fs::create_dir_all(&v.tmp)?;
        let missing = v.storage.missing(&keep);
        let progress = Progress::new(&ctx.bar, "Storing", missing.len());
        for hash in &missing {
            progress.tick();
            let tmp = v.tmp.join(unique());
            if fetch(v, peer, hash, &tmp)? {
                v.storage.put_blob(hash, &tmp)?;
                if v.bare {
                    v.traffic.borrow_mut().sent_files.insert(hash.clone());
                }
            }
        }
    }
    if v.bare {
        match ctx.dry_run {
            true => v.derive(|v, hash| v.storage.has_blob(hash) || available(v, peer, hash)),
            false => v.derive(|v, hash| v.storage.has_blob(hash)),
        }
    }
    Ok(())
}

/// Compaction: remove the stored blobs this vault's `keep_history` no longer
/// covers. Every path counts, taken or not, so the content of a current
/// version is never among them: a file removed with `drop` may have its only
/// copy here. The log is not touched. Returns how many were removed and
/// their bytes; a dry run removes nothing and returns the same.
pub fn compact(ctx: &Context, v: &Vault) -> Result<(usize, u64)> {
    let keep = v.log.keep(v.config.keep_history, |_| true);
    v.storage.drop_blobs_except(&keep, ctx.dry_run)
}
