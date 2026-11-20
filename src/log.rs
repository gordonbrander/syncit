//! The log: a set of entries, and the read rules over it (CRDT.md).

use crate::entry::{Entry, Id};
use crate::segment;
use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

#[derive(Default)]
pub struct Log {
    pub entries: BTreeMap<Id, Entry>,
    pub paths: BTreeMap<String, BTreeSet<Id>>,
}

/// The result of the read rules for one path.
#[derive(Debug, PartialEq, Eq)]
pub enum PathState {
    Absent,     // no valid entries
    Incomplete, // some entry names a parent that is not in the log
    Resolved {
        heads: Vec<Id>, // sorted
        winner: Id,
        losers: Vec<Loser>, // distinct non-null hashes other than the winner's
        lost_delete: bool,  // winner is a file and some head is a tombstone
    },
}

#[derive(Debug, PartialEq, Eq)]
pub struct Loser {
    pub hash: String,
    pub mtime: i64, // the largest among heads with this hash
}

impl Log {
    pub fn add(&mut self, entry: Entry) {
        let id = entry.id();
        self.paths
            .entry(entry.path.clone())
            .or_default()
            .insert(id.clone());
        self.entries.insert(id, entry);
    }

    /// Read every segment in `dir`. A bad segment is an error when `strict`
    /// (this vault's own log) and is ignored with a warning otherwise (a
    /// peer's log, which is not trusted).
    pub fn load(dir: &Path, strict: bool) -> Result<Log> {
        let mut log = Log::default();
        for file in fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))? {
            let file = file?.path();
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            let entries = fs::read(&file)
                .map_err(anyhow::Error::from)
                .and_then(|bytes| segment::decode(&name, &bytes));
            match entries {
                Ok(entries) => entries.into_iter().for_each(|e| log.add(e)),
                Err(e) if strict => bail!("corrupt segment {}: {e:#}", file.display()),
                Err(e) => eprintln!("warning: ignoring segment {}: {e:#}", file.display()),
            }
        }
        Ok(log)
    }

    /// The entries of a path, leaving out invalid ones: those with a parent
    /// that belongs to another path. In id order.
    pub fn valid(&self, path: &str) -> Vec<(&Id, &Entry)> {
        let other_path = |p: &Id| self.entries.get(p).is_some_and(|e| e.path != path);
        self.paths
            .get(path)
            .into_iter()
            .flatten()
            .map(|id| (id, &self.entries[id]))
            .filter(|(_, e)| !e.parents.iter().any(other_path))
            .collect()
    }

    pub fn resolve(&self, path: &str) -> PathState {
        let ids = self.paths.get(path).into_iter().flatten();
        let missing = |p: &Id| !self.entries.contains_key(p);
        if ids
            .clone()
            .any(|id| self.entries[id].parents.iter().any(missing))
        {
            return PathState::Incomplete;
        }
        let valid = self.valid(path);
        let superseded: BTreeSet<&Id> = valid.iter().flat_map(|(_, e)| &e.parents).collect();
        let heads: Vec<(&Id, &Entry)> = valid
            .into_iter()
            .filter(|(id, _)| !superseded.contains(id))
            .collect();
        let Some(&(winner, w)) = heads
            .iter()
            .max_by_key(|(id, e)| (e.hash.is_some(), e.mtime, *id))
        else {
            return PathState::Absent;
        };
        let mut losers: BTreeMap<&String, i64> = BTreeMap::new();
        for (_, e) in &heads {
            if let Some(hash) = &e.hash
                && e.hash != w.hash
            {
                let mtime = losers.entry(hash).or_insert(e.mtime);
                *mtime = e.mtime.max(*mtime);
            }
        }
        PathState::Resolved {
            winner: winner.clone(),
            lost_delete: w.hash.is_some() && heads.iter().any(|(_, e)| e.hash.is_none()),
            losers: losers
                .into_iter()
                .map(|(hash, mtime)| Loser {
                    hash: hash.clone(),
                    mtime,
                })
                .collect(),
            heads: heads.into_iter().map(|(id, _)| id.clone()).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::sha256;

    fn e(path: &str, content: Option<&str>, mtime: i64, parents: &[&Entry]) -> Entry {
        let mut parents: Vec<Id> = parents.iter().map(|p| p.id()).collect();
        parents.sort();
        Entry {
            hash: content.map(|c| sha256(c.as_bytes())),
            mtime,
            parents,
            path: path.into(),
        }
    }

    fn log(entries: &[&Entry]) -> Log {
        let mut log = Log::default();
        entries.iter().for_each(|e| log.add((*e).clone()));
        log
    }

    /// (heads, winner, loser hashes, lost_delete), with heads given as entries.
    fn expect(log: &Log, heads: &[&Entry], winner: &Entry, losers: &[&Entry], lost_delete: bool) {
        let mut ids: Vec<Id> = heads.iter().map(|e| e.id()).collect();
        ids.sort();
        let mut hashes: Vec<String> = losers.iter().map(|e| e.hash.clone().unwrap()).collect();
        hashes.sort();
        match log.resolve(&winner.path) {
            PathState::Resolved {
                heads,
                winner: w,
                losers,
                lost_delete: d,
            } => {
                assert_eq!(heads, ids);
                assert_eq!(w, winner.id());
                assert_eq!(
                    losers.into_iter().map(|l| l.hash).collect::<Vec<_>>(),
                    hashes
                );
                assert_eq!(d, lost_delete);
            }
            other => panic!("expected a resolved path, got {other:?}"),
        }
    }

    #[test]
    fn resolve_worked_cases() {
        let a = e("p", Some("a"), 1, &[]);
        let b = e("p", Some("b"), 2, &[&a]);
        let c = e("p", Some("c"), 3, &[&b]);
        // Linear edits.
        expect(&log(&[&a, &b, &c]), &[&c], &c, &[], false);

        // Concurrent edits (also: a folder copied and both copies edited; a
        // new version heard of but not seen, then a local edit).
        let c2 = e("p", Some("c"), 3, &[&a]);
        expect(&log(&[&a, &b, &c2]), &[&b, &c2], &c2, &[&b], false);

        // Closing a conflict.
        let d = e("p", Some("d"), 4, &[&b, &c2]);
        expect(&log(&[&a, &b, &c2, &d]), &[&d], &d, &[], false);

        // Edit against delete: the edit wins though the delete is later.
        let t = e("p", None, 99, &[&a]);
        expect(&log(&[&a, &b, &t]), &[&b, &t], &b, &[], true);

        // The same edit in two vaults is one entry.
        let mut twice = log(&[&a, &b]);
        twice.add(b.clone());
        assert_eq!(twice.entries.len(), 2);
        expect(&twice, &[&b], &b, &[], false);

        // Restored from backup at A, then edited, while A <- B <- C exists.
        let d = e("p", Some("d"), 4, &[&a]);
        expect(&log(&[&a, &b, &c, &d]), &[&c, &d], &d, &[&c], false);

        // A file created at a path this vault does not hold: a conflict with
        // the existing head, unless the hash is equal.
        let x = e("p", Some("x"), 1, &[]);
        expect(&log(&[&a, &b, &c, &x]), &[&c, &x], &c, &[&x], false);
        let same = e("p", Some("c"), 9, &[]);
        expect(&log(&[&a, &b, &c, &same]), &[&c, &same], &same, &[], false);

        // A deleted path: every head is a tombstone.
        let t = e("p", None, 5, &[&c]);
        expect(&log(&[&a, &b, &c, &t]), &[&t], &t, &[], false);
        assert_eq!(log(&[&a]).resolve("elsewhere"), PathState::Absent);
    }

    #[test]
    fn winner_order() {
        let a = e("p", Some("a"), 1, &[]);
        // A file beats a tombstone whatever the mtimes.
        let file = e("p", Some("f"), 2, &[&a]);
        let gone = e("p", None, 1_000_000, &[&a]);
        expect(&log(&[&a, &file, &gone]), &[&file, &gone], &file, &[], true);
        // The later mtime wins.
        let late = e("p", Some("late"), 3, &[&a]);
        expect(
            &log(&[&a, &file, &late]),
            &[&file, &late],
            &late,
            &[&file],
            false,
        );
        // The id breaks a tie.
        let x = e("p", Some("x"), 7, &[&a]);
        let y = e("p", Some("y"), 7, &[&a]);
        let (hi, lo) = if x.id() > y.id() { (&x, &y) } else { (&y, &x) };
        expect(&log(&[&a, &x, &y]), &[&x, &y], hi, &[lo], false);
        // A loser's mtime is the largest among heads with its hash.
        let lo2 = e(
            "p",
            lo.hash.as_ref().map(|_| if lo == &x { "x" } else { "y" }),
            5,
            &[],
        );
        match log(&[&a, &x, &y, &lo2]).resolve("p") {
            PathState::Resolved { losers, .. } => assert_eq!(losers[0].mtime, 7),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn resolve_incomplete() {
        let a = e("p", Some("a"), 1, &[]);
        let b = e("p", Some("b"), 2, &[&a]);
        let c = e("p", Some("c"), 3, &[&b]);
        let other = e("q", Some("q"), 1, &[]);
        let partial = log(&[&a, &c, &other]);
        assert_eq!(partial.resolve("p"), PathState::Incomplete);
        expect(&partial, &[&other], &other, &[], false);
        expect(&log(&[&a, &b, &c]), &[&c], &c, &[], false);
    }

    #[test]
    fn resolve_cross_path_parent() {
        let a = e("p", Some("a"), 1, &[]);
        let stray = e("q", Some("s"), 9, &[&a]);
        let l = log(&[&a, &stray]);
        expect(&l, &[&a], &a, &[], false); // supersedes nothing
        assert_eq!(l.resolve("q"), PathState::Absent); // never a head
        let q = e("q", Some("q"), 1, &[]);
        expect(&log(&[&a, &stray, &q]), &[&q], &q, &[], false);
    }

    #[test]
    fn resolve_order_independent() {
        let a = e("p", Some("a"), 1, &[]);
        let b = e("p", Some("b"), 2, &[&a]);
        let c = e("p", Some("c"), 3, &[&a]);
        let d = e("q", Some("d"), 1, &[]);
        let t = e("q", None, 5, &[&d]);
        let x = e("q", Some("x"), 2, &[&d]);
        let stray = e("r", Some("r"), 1, &[&a]);
        let all = [a, b, c, d, t, x, stray];
        let states = |log: &Log| -> Vec<(String, PathState)> {
            log.paths
                .keys()
                .map(|p| (p.clone(), log.resolve(p)))
                .collect()
        };
        let reference = states(&log(&all.iter().collect::<Vec<_>>()));
        assert_eq!(reference.len(), 3);
        for size in [1, 2, 3, all.len()] {
            for turn in 0..all.len() {
                let mut entries = all.to_vec();
                entries.rotate_left(turn);
                if turn % 2 == 1 {
                    entries.reverse();
                }
                let mut log = Log::default();
                for chunk in entries.chunks(size) {
                    let (name, bytes) = segment::encode(chunk);
                    segment::decode(&name, &bytes)
                        .unwrap()
                        .into_iter()
                        .for_each(|e| log.add(e));
                }
                assert_eq!(states(&log), reference, "segments of {size}, turn {turn}");
            }
        }
    }
}
