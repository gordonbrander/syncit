//! Blobs: file contents addressed by SHA-256, and where to find them.

use crate::backend::{Backend, Fs};
use crate::entry::sha256;
use crate::output::{Context, Progress, Summary, Traffic};
use crate::vault::{Dir, Vault, Worktree, unique};
use anyhow::{Context as _, Result};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;

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

/// For a call that has no peer.
pub const NO_PEER: Option<&Vault<Fs, Dir>> = None;

/// Note a blob taken from the storage of a peer that is bare, for the report
/// of a run. `traffic` is given only for a peer.
fn received<W: Worktree>(_: &Vault<impl Backend, W>, hash: &str, traffic: Option<&mut Traffic>) {
    if let Some(traffic) = traffic
        && W::BARE
    {
        traffic.received_files.insert(hash.to_string());
    }
}

fn fetch_from(
    v: &Vault<impl Backend, impl Worktree>,
    hash: &str,
    dest: &Path,
    traffic: Option<&mut Traffic>,
) -> io::Result<bool> {
    for src in v.work.files(hash) {
        if copy_verified(src, dest, hash)? {
            return Ok(true);
        }
    }
    let stored = v.store.get_blob(hash, dest)?;
    if stored {
        received(v, hash, traffic);
    }
    Ok(stored)
}

/// Copy the blob to `dest` from the first source that yields the right
/// bytes: this vault's files, its storage, the peer's files, the peer's
/// storage. False means no source has it, which is not an error.
pub fn fetch(
    v: &Vault<impl Backend, impl Worktree>,
    peer: Option<&Vault<impl Backend, impl Worktree>>,
    hash: &str,
    dest: &Path,
    traffic: &mut Traffic,
) -> io::Result<bool> {
    Ok(fetch_from(v, hash, dest, None)?
        || match peer {
            Some(peer) => fetch_from(peer, hash, dest, Some(traffic))?,
            None => false,
        })
}

fn read_from(
    v: &Vault<impl Backend, impl Worktree>,
    hash: &str,
    max: u64,
    traffic: Option<&mut Traffic>,
) -> Option<Vec<u8>> {
    let file = v.work.files(hash).iter().find_map(|src| {
        let small = src.metadata().is_ok_and(|m| m.is_file() && m.len() <= max);
        let bytes = small.then(|| fs::read(src).ok()).flatten()?;
        (sha256(&bytes) == hash).then_some(bytes)
    });
    file.or_else(|| {
        let bytes = v.store.read_blob(hash, max)?;
        received(v, hash, traffic);
        Some(bytes)
    })
}

/// The blob's bytes, from the first source that yields them and is no larger
/// than `max`. Nothing is written, so a dry run reads as a real one does.
pub fn read(
    v: &Vault<impl Backend, impl Worktree>,
    peer: Option<&Vault<impl Backend, impl Worktree>>,
    hash: &str,
    max: u64,
    traffic: &mut Traffic,
) -> Option<Vec<u8>> {
    read_from(v, hash, max, None).or_else(|| read_from(peer?, hash, max, Some(traffic)))
}

fn available_from(
    v: &Vault<impl Backend, impl Worktree>,
    hash: &str,
    traffic: Option<&mut Traffic>,
) -> bool {
    if v.work.files(hash).iter().any(|src| src.is_file()) {
        return true;
    }
    let stored = v.store.has_blob(hash);
    if stored {
        received(v, hash, traffic);
    }
    stored
}

/// Whether `fetch` would find the blob, for a dry run.
// ponytail: checks that a source exists, not its bytes; a corrupt source
// reads as available here and as pending in the real run. Hash it if that matters.
pub fn available(
    v: &Vault<impl Backend, impl Worktree>,
    peer: Option<&Vault<impl Backend, impl Worktree>>,
    hash: &str,
    traffic: &mut Traffic,
) -> bool {
    available_from(v, hash, None) || peer.is_some_and(|p| available_from(p, hash, Some(traffic)))
}

/// Put into the vault's storage every version this vault keeps
/// (`Log::keep`) that a source can supply, for the paths it takes: a partial
/// vault does not gather the content of files it has chosen not to have. A
/// vault with a working dir that keeps no history collects nothing, and a
/// dry run stores nothing. Files in transit go through the `.syncit/tmp` of
/// whichever vault has a dir.
pub fn retain<W: Worktree>(
    ctx: &Context,
    v: &mut Vault<impl Backend, W>,
    peer: Option<&Vault<impl Backend, impl Worktree>>,
    sum: &mut Summary,
) -> Result<()> {
    let traffic = &mut sum.traffic;
    let keep = v.log.keep(v.config.keep_history, |path| v.takes(path));
    // What a dry run would have stored.
    let mut arrived = BTreeSet::new();
    if ctx.dry_run && W::BARE {
        arrived = v.store.missing(&keep);
        arrived.retain(|hash| available(v, peer, hash, traffic));
        traffic.sent_files.extend(arrived.iter().cloned());
    } else if !ctx.dry_run && (W::BARE || v.config.keep_history != Some(0)) {
        let missing = v.store.missing(&keep);
        if !missing.is_empty() {
            let root = v.work.root().or(peer.and_then(|p| p.work.root()));
            let tmp = root.context("no vault here to move files through")?;
            let tmp = tmp.join(".syncit/tmp");
            fs::create_dir_all(&tmp)?;
            let progress = Progress::new(&ctx.bar, "storing", missing.len());
            for hash in &missing {
                progress.tick();
                let tmp = tmp.join(unique());
                if fetch(v, peer, hash, &tmp, traffic)? {
                    v.store.put_blob(hash, &tmp)?;
                    if W::BARE {
                        traffic.sent_files.insert(hash.clone());
                    }
                }
            }
        }
    }
    W::retained(v, &arrived);
    Ok(())
}

/// Compaction: remove the stored blobs this vault's `keep_history` no longer
/// covers. Every path counts, taken or not, so the content of a current
/// version is never among them: a file removed with `drop` may have its only
/// copy here. The log is not touched. Returns how many were removed and
/// their bytes; a dry run removes nothing and returns the same.
pub fn compact(ctx: &Context, v: &Vault<impl Backend, impl Worktree>) -> Result<(usize, u64)> {
    let keep = v.log.keep(v.config.keep_history, |_| true);
    v.store.drop_blobs_except(&keep, ctx.dry_run)
}
