//! The log: a set of entries, and the read rules over it (CRDT.md).

use crate::entry::{Entry, Id};
use crate::output::{Context, warn};
use crate::segment;
use crate::storage::Storage;
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub struct Log {
    pub entries: BTreeMap<Id, Entry>,
    pub paths: BTreeMap<String, BTreeSet<Id>>,
    /// For each entry, the nicknames of the segments holding it: the vaults
    /// that recorded it. A label for people; no rule reads it.
    pub authors: BTreeMap<Id, BTreeSet<String>>,
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
    pub exec: bool, // of the head with that mtime; of the greatest id on a tie
}

impl Log {
    /// Add an entry found in a segment written by the vault `nickname`.
    pub fn add(&mut self, entry: Entry, nickname: &str) {
        self.insert(entry.id(), entry, nickname);
    }

    /// The same, for an entry whose id is already in hand. `id` must be
    /// `entry.id()`.
    pub fn insert(&mut self, id: Id, entry: Entry, nickname: &str) {
        self.authors
            .entry(id.clone())
            .or_default()
            .insert(nickname.to_string());
        self.paths
            .entry(entry.path.clone())
            .or_default()
            .insert(id.clone());
        self.entries.insert(id, entry);
    }

    /// Read every segment in `storage`. A bad segment is an error when
    /// `strict` (this vault's own log) and is ignored with a warning otherwise
    /// (a peer's log, which is not trusted).
    pub fn load(ctx: &Context, storage: &dyn Storage, strict: bool) -> Result<Log> {
        let mut log = Log::default();
        for name in storage.segment_names()? {
            let segment = storage
                .read_segment(&name)
                .and_then(|bytes| segment::decode(&name, &bytes));
            match segment {
                Ok(s) => {
                    for (id, e) in s.entries {
                        log.insert(id, e, &s.nickname);
                    }
                }
                Err(e) if strict => bail!("corrupt segment {name}: {e:#}"),
                Err(e) => warn(ctx, format!("ignoring segment {name}: {e:#}")),
            }
        }
        Ok(log)
    }

    /// The entries of a path, leaving out invalid ones: those with a parent
    /// that belongs to another path. In id order.
    pub fn valid(&self, path: &str) -> Vec<(&Id, &Entry)> {
        self.read(path).0
    }

    /// The valid entries of a path, and whether its history is complete: no
    /// entry of the path, valid or not, names a parent that is not in the
    /// log. One pass, so that each parent is looked up once.
    fn read(&self, path: &str) -> (Vec<(&Id, &Entry)>, bool) {
        let (mut valid, mut complete) = (vec![], true);
        for id in self.paths.get(path).into_iter().flatten() {
            let entry = &self.entries[id];
            let mut same_path = true;
            for parent in &entry.parents {
                match self.entries.get(parent) {
                    Some(parent) => same_path &= parent.path == path,
                    None => complete = false,
                }
            }
            if same_path {
                valid.push((id, entry));
            }
        }
        (valid, complete)
    }

    /// The one lowest common ancestor of two entries of a path, if it is a
    /// file: the version a three-way merge starts from. None if they share
    /// no ancestor (created independently) or have several (a criss-cross).
    pub fn merge_base(&self, path: &str, a: &Id, b: &Id) -> Option<Id> {
        fn ancestors<'a>(valid: &BTreeMap<&'a Id, &'a Entry>, id: &'a Id) -> BTreeSet<&'a Id> {
            let (mut seen, mut todo) = (BTreeSet::new(), vec![id]);
            while let Some(id) = todo.pop() {
                for p in valid.get(id).into_iter().flat_map(|e| &e.parents) {
                    if valid.contains_key(p) && seen.insert(p) {
                        todo.push(p);
                    }
                }
            }
            seen
        }
        let valid: BTreeMap<&Id, &Entry> = self.valid(path).into_iter().collect();
        let (of_a, of_b) = (ancestors(&valid, a), ancestors(&valid, b));
        let common: Vec<&Id> = of_a.intersection(&of_b).copied().collect();
        let above: BTreeSet<&Id> = common.iter().flat_map(|c| ancestors(&valid, c)).collect();
        let mut lowest = common.into_iter().filter(|c| !above.contains(c));
        match (lowest.next(), lowest.next()) {
            (Some(id), None) if valid[id].hash.is_some() => Some(id.clone()),
            _ => None,
        }
    }

    /// The hashes of the versions whose content a vault keeps when it keeps
    /// `depth` versions back from a head; `None` is every version. A head is
    /// step 0, so the current versions are always among them. A path with
    /// several heads, or with incomplete history, keeps every version: a
    /// merge or a conflict copy may need any of them. Only paths that `of`
    /// selects are considered.
    pub fn keep(&self, depth: Option<u32>, of: impl Fn(&str) -> bool) -> BTreeSet<String> {
        let mut keep = BTreeSet::new();
        for (path, ids) in self.paths.iter().filter(|(path, _)| of(path)) {
            let head = match (depth, self.resolve(path)) {
                (Some(depth), PathState::Resolved { heads, .. }) if heads.len() == 1 => {
                    Some((depth, heads[0].clone()))
                }
                _ => None,
            };
            let Some((depth, head)) = head else {
                keep.extend(ids.iter().filter_map(|id| self.entries[id].hash.clone()));
                continue;
            };
            let mut seen = BTreeSet::from([head.clone()]);
            let mut level = vec![head];
            for _ in 0..=depth {
                let mut next = vec![];
                for e in level.iter().filter_map(|id| self.entries.get(id)) {
                    keep.extend(e.hash.clone());
                    next.extend(
                        e.parents
                            .iter()
                            .filter(|p| seen.insert((*p).clone()))
                            .cloned(),
                    );
                }
                level = next;
                if level.is_empty() {
                    break;
                }
            }
        }
        keep
    }

    pub fn resolve(&self, path: &str) -> PathState {
        let (valid, complete) = self.read(path);
        if !complete {
            return PathState::Incomplete;
        }
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
        let mut losers: BTreeMap<&String, (i64, &Id, bool)> = BTreeMap::new();
        for (id, e) in &heads {
            if let Some(hash) = &e.hash
                && e.hash != w.hash
            {
                let head = (e.mtime, *id, e.exec);
                let newest = losers.entry(hash).or_insert(head);
                *newest = head.max(*newest);
            }
        }
        PathState::Resolved {
            winner: winner.clone(),
            lost_delete: w.hash.is_some() && heads.iter().any(|(_, e)| e.hash.is_none()),
            losers: losers
                .into_iter()
                .map(|(hash, (mtime, _, exec))| Loser {
                    hash: hash.clone(),
                    mtime,
                    exec,
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
            exec: false,
            hash: content.map(|c| sha256(c.as_bytes())),
            mtime,
            parents,
            path: path.into(),
        }
    }

    fn log(entries: &[&Entry]) -> Log {
        let mut log = Log::default();
        entries.iter().for_each(|e| log.add((*e).clone(), "v"));
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
        twice.add(b.clone(), "w");
        assert_eq!(twice.entries.len(), 2);
        assert_eq!(twice.authors[&b.id()].len(), 2, "recorded by both");
        expect(&twice, &[&b], &b, &[], false);

        // Restored from backup at A, then edited, while A <- B <- C exists.
        let d = e("p", Some("d"), 4, &[&a]);
        expect(&log(&[&a, &b, &c, &d]), &[&c, &d], &d, &[&c], false);

        // A file created at a path this vault does not have: a conflict with
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
    fn keep_cases() {
        let hashes = |contents: &[&str]| -> BTreeSet<String> {
            contents.iter().map(|c| sha256(c.as_bytes())).collect()
        };
        let a = e("p", Some("a"), 1, &[]);
        let b = e("p", Some("b"), 2, &[&a]);
        let c = e("p", Some("c"), 3, &[&b]);
        let chain = log(&[&a, &b, &c]);
        assert_eq!(chain.keep(Some(0), |_| true), hashes(&["c"]));
        assert_eq!(chain.keep(Some(1), |_| true), hashes(&["b", "c"]));
        assert_eq!(chain.keep(Some(2), |_| true), hashes(&["a", "b", "c"]));
        assert_eq!(
            chain.keep(Some(u32::MAX), |_| true),
            hashes(&["a", "b", "c"])
        );
        assert_eq!(chain.keep(None, |_| true), hashes(&["a", "b", "c"]));
        // A deletion is a version: one step back from it is the last content.
        let t = e("p", None, 4, &[&c]);
        let gone = log(&[&a, &b, &c, &t]);
        assert!(gone.keep(Some(0), |_| true).is_empty());
        assert_eq!(gone.keep(Some(1), |_| true), hashes(&["c"]));
        // A conflict keeps every version of its path, whatever the depth.
        let c2 = e("p", Some("c2"), 5, &[&b]);
        let open = log(&[&a, &b, &c, &c2]);
        assert_eq!(open.keep(Some(0), |_| true), hashes(&["a", "b", "c", "c2"]));
        // Closed, the step back from the merge is both versions it replaced.
        let m = e("p", Some("m"), 6, &[&c, &c2]);
        let closed = log(&[&a, &b, &c, &c2, &m]);
        assert_eq!(closed.keep(Some(1), |_| true), hashes(&["c", "c2", "m"]));
        assert_eq!(
            closed.keep(Some(2), |_| true),
            hashes(&["b", "c", "c2", "m"])
        );
        // So does a path with incomplete history.
        assert_eq!(log(&[&a, &c]).keep(Some(0), |_| true), hashes(&["a", "c"]));
        // Content two paths share is kept while either needs it.
        let q = e("q", Some("a"), 1, &[]);
        let shared = log(&[&a, &b, &c, &q]);
        assert_eq!(shared.keep(Some(0), |_| true), hashes(&["a", "c"]));
        assert_eq!(shared.keep(Some(0), |path| path == "p"), hashes(&["c"]));
    }

    #[test]
    fn merge_base_cases() {
        let a = e("p", Some("a"), 1, &[]);
        let b = e("p", Some("b"), 2, &[&a]);
        let c = e("p", Some("c"), 3, &[&a]);
        let base = |entries: &[&Entry], x: &Entry, y: &Entry| {
            log(entries).merge_base("p", &x.id(), &y.id())
        };
        // Concurrent edits, and the same from further down each side.
        assert_eq!(base(&[&a, &b, &c], &b, &c), Some(a.id()));
        let b2 = e("p", Some("b2"), 4, &[&b]);
        let c2 = e("p", Some("c2"), 5, &[&c]);
        assert_eq!(base(&[&a, &b, &c, &b2, &c2], &b2, &c2), Some(a.id()));
        // The lowest one: B, not A above it.
        let d = e("p", Some("d"), 6, &[&b]);
        assert_eq!(base(&[&a, &b, &b2, &d], &b2, &d), Some(b.id()));
        // Created independently: nothing in common.
        let x = e("p", Some("x"), 1, &[]);
        assert_eq!(base(&[&a, &b, &x], &b, &x), None);
        // A criss-cross: two merges of B and C have both as lowest ancestors.
        let m1 = e("p", Some("m1"), 7, &[&b, &c]);
        let m2 = e("p", Some("m2"), 8, &[&b, &c]);
        assert_eq!(base(&[&a, &b, &c, &m1, &m2], &m1, &m2), None);
        // A deletion has no content to merge from.
        let t = e("p", None, 2, &[&a]);
        let r1 = e("p", Some("r1"), 3, &[&t]);
        let r2 = e("p", Some("r2"), 4, &[&t]);
        assert_eq!(base(&[&a, &t, &r1, &r2], &r1, &r2), None);
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
    fn resolve_exec() {
        let on = |e: Entry| Entry { exec: true, ..e };
        let a = e("p", Some("a"), 1, &[]);
        // The same content with the other bit is a head, not a conflict.
        let x = on(e("p", Some("a"), 1, &[&a]));
        let y = e("p", Some("a"), 1, &[&a]);
        expect(&log(&[&a, &x]), &[&x], &x, &[], false);
        let l = log(&[&a, &x, &y]);
        let winner = if x.id() > y.id() { &x } else { &y };
        expect(&l, &[&x, &y], winner, &[], false);

        // A loser has the bit of its newest head.
        let old = on(e("p", Some("b"), 2, &[&a]));
        let new = e("p", Some("b"), 3, &[&a]);
        let w = e("p", Some("c"), 9, &[&a]);
        let exec_of = |heads: &[&Entry]| match log(heads).resolve("p") {
            PathState::Resolved { losers, .. } => (losers.len(), losers[0].mtime, losers[0].exec),
            other => panic!("expected a resolved path, got {other:?}"),
        };
        assert_eq!(exec_of(&[&a, &old, &w]), (1, 2, true));
        assert_eq!(exec_of(&[&a, &old, &new, &w]), (1, 3, false));
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
                    let (name, bytes) = segment::encode("v", chunk);
                    let entries = segment::decode(&name, &bytes).unwrap().entries;
                    entries
                        .into_iter()
                        .for_each(|(id, e)| log.insert(id, e, "v"));
                }
                assert_eq!(states(&log), reference, "segments of {size}, turn {turn}");
            }
        }
    }
}
