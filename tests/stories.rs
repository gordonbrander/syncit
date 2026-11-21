//! Integration tests: vaults in temporary directories, driven through the
//! library's command functions. Names follow SPEC.md section 14.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use syncit::commands::{self as cmd, Setting};
use syncit::output::{Holding, Options, Summary};

const O: &Options = &Options {
    json: false,
    dry_run: false,
};
use syncit::cache::Cache;
use syncit::config::Config;
use syncit::entry::sha256;
use syncit::log::{Log, PathState};
use syncit::materialise::{copy_name, materialise};
use syncit::scan::scan;
use syncit::vault::Vault;
use tempfile::TempDir;

/// Write a file whose mtime is `t` seconds after a fixed time in the past.
fn put(dir: &Path, path: &str, content: &str, t: u64) {
    let file = dir.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, content).unwrap();
    let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000 + t);
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(time)
        .unwrap();
}

fn read(dir: &Path, path: &str) -> Option<String> {
    fs::read_to_string(dir.join(path)).ok()
}

fn is(dir: &Path, path: &str, content: &str) -> bool {
    read(dir, path).as_deref() == Some(content)
}

/// A temporary folder, and inside it a vault `a` holding `files`.
fn vault(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    fs::create_dir(&a).unwrap();
    for (path, content) in files {
        put(&a, path, content, 0);
    }
    cmd::init(O, &a).unwrap();
    (tmp, a)
}

/// Vault `a` holding `files`, and `b` cloned from it.
fn pair(files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf) {
    let (tmp, a) = vault(files);
    let b = tmp.path().join("b");
    assert_eq!(cmd::clone(O, &a, &b).unwrap().exit_code(), 0);
    (tmp, a, b)
}

/// What `status` lists for the vault at `dir`: the lines that need something.
fn status(dir: &Path) -> Vec<Holding> {
    cmd::status(O, dir, &[], None, false).unwrap()
}

fn has(found: &[Holding], state: &str, path: &str) -> bool {
    found.iter().any(|h| h.ours == state && h.path == path)
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

fn log(dir: &Path) -> Log {
    Log::load(O, &dir.join(".syncit/log"), true).unwrap()
}

fn names(dir: &Path) -> BTreeSet<String> {
    fs::read_dir(dir)
        .unwrap()
        .map(|f| f.unwrap().file_name().into_string().unwrap())
        .collect()
}

/// The working files of a vault: path to content.
fn tree(root: &Path) -> BTreeMap<String, String> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for item in fs::read_dir(dir).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() && !path.ends_with(".syncit") {
                go(root, &path, out);
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                out.insert(rel, fs::read_to_string(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    go(root, root, &mut out);
    out
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for item in fs::read_dir(from).unwrap() {
        let item = item.unwrap();
        let dest = to.join(item.file_name());
        if item.file_type().unwrap().is_dir() {
            copy_dir(&item.path(), &dest);
        } else {
            fs::copy(item.path(), dest).unwrap();
        }
    }
}

fn configure(dir: &Path, change: impl FnOnce(&mut Config)) {
    let mut config = Config::load(dir).unwrap();
    change(&mut config);
    config.save(dir).unwrap();
}

fn copy_of(path: &str, content: &str) -> String {
    copy_name(path, &sha256(content.as_bytes()))
}

// ---- Stories -------------------------------------------------------------

#[test]
fn story_a1_edit_and_push() {
    let (_tmp, a, b) = pair(&[("notes.txt", "v1"), ("dir/other.txt", "o")]);
    assert!(is(&b, "notes.txt", "v1") && is(&b, "dir/other.txt", "o"));
    put(&b, "notes.txt", "v2", 1);
    let report = cmd::push(O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.recorded), (0, 1));
    assert_eq!(report.updated, 1);
    assert!(is(&a, "notes.txt", "v2"));
}

#[test]
fn story_a2_add_and_push() {
    let (_tmp, a, b) = pair(&[("notes.txt", "v1"), ("dir/old.txt", "o")]);
    put(&b, "new/deep/n.txt", "n", 1);
    fs::remove_file(b.join("dir/old.txt")).unwrap();
    // A push does not bring the peer's changes here.
    put(&a, "only-a.txt", "a", 1);
    let report = cmd::push(O, &b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    assert!(is(&a, "new/deep/n.txt", "n"));
    assert!(
        !a.join("dir").exists(),
        "the deletion propagates, and the empty folder goes"
    );
    assert!(!b.join("only-a.txt").exists());
    cmd::pull(O, &b, &a).unwrap();
    assert!(is(&b, "only-a.txt", "a"));
}

#[test]
fn story_a3_move() {
    let (_tmp, a, b) = pair(&[("x/1.txt", "one"), ("y/2.txt", "two"), ("keep.txt", "k")]);
    let report = cmd::move_to(O, &b, &paths(&["x", "y/2.txt"]), &a).unwrap();
    assert!(report.has("moved", "x/1.txt") && report.has("moved", "y/2.txt"));
    assert_eq!(report.exit_code(), 0);
    assert!(!b.join("x").exists() && !b.join("y").exists() && is(&b, "keep.txt", "k"));
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.recorded), (0, 0));
    assert!(is(&a, "x/1.txt", "one") && is(&a, "y/2.txt", "two"));
    assert!(!b.join("x").exists() && !b.join("y").exists());
    cmd::get(O, &b, &[a.join("x"), a.join("y/2.txt")]).unwrap();
    assert!(is(&b, "x/1.txt", "one") && is(&b, "y/2.txt", "two"));
    assert_eq!(tree(&a), tree(&b));
}

#[test]
fn story_a4_clone_subfolder() {
    let (tmp, a) = vault(&[
        ("foo/bar/x.txt", "x"),
        ("foo/z.txt", "z"),
        ("other/y.txt", "y"),
    ]);
    let b = tmp.path().join("b");
    cmd::clone(O, &a.join("foo/bar"), &b).unwrap();
    assert_eq!(tree(&b).keys().collect::<Vec<_>>(), ["foo/bar/x.txt"]);
    put(&b, "foo/bar/x.txt", "x2", 1);
    put(&b, "foo/bar/new.txt", "n", 1);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.deleted), (0, 0));
    assert!(is(&a, "foo/bar/x.txt", "x2") && is(&a, "foo/bar/new.txt", "n"));
    assert!(is(&a, "foo/z.txt", "z") && is(&a, "other/y.txt", "y"));
    assert_eq!(tree(&b).len(), 2);
}

#[test]
fn story_a5_get_and_new_folder() {
    let (tmp, a) = vault(&[
        ("foo/bar/x.txt", "x"),
        ("foo/bar/big/f.bin", "f"),
        ("other/y.txt", "y"),
    ]);
    let b = tmp.path().join("b");
    cmd::clone(O, &a.join("foo/bar"), &b).unwrap();
    cmd::move_to(O, &b, &paths(&["foo/bar/big"]), &a).unwrap();
    // A second folder.
    cmd::get(O, &b, &[a.join("other")]).unwrap();
    assert!(is(&b, "other/y.txt", "y") && !b.join("foo/bar/big").exists());
    // New files outside the included paths: the three examples of section 7.
    put(&b, "zap/x/y.txt", "new", 1);
    put(&b, "zap/x/z.txt", "new too", 1);
    put(&b, "foo/new.txt", "n", 1);
    put(&b, "foo/bar/big/n/a.txt", "a", 1);
    assert_eq!(cmd::sync(O, &b, &a).unwrap().exit_code(), 0);
    assert!(
        is(&a, "zap/x/y.txt", "new")
            && is(&a, "foo/new.txt", "n")
            && is(&a, "foo/bar/big/n/a.txt", "a")
    );
    let config = Config::load(&b).unwrap();
    assert_eq!(
        config.include,
        ["foo/bar", "other", "foo/new.txt", "zap", "foo/bar/big/n"]
    );
    assert_eq!(config.exclude, ["foo/bar/big"]);
    // A file added to the new folder elsewhere arrives here.
    put(&a, "zap/later.txt", "later", 2);
    put(&a, "foo/elsewhere.txt", "excluded in b", 2);
    cmd::sync(O, &b, &a).unwrap();
    assert!(is(&b, "zap/later.txt", "later"));
    assert!(!b.join("foo/elsewhere.txt").exists() && !b.join("foo/bar/big/f.bin").exists());
    assert!(is(&a, "foo/bar/big/f.bin", "f"));
}

#[test]
fn story_b1_conflict() {
    let (_tmp, a, b) = pair(&[("notes.txt", "base")]);
    put(&a, "notes.txt", "from a", 1);
    put(&b, "notes.txt", "from b", 2);
    let report = cmd::sync(O, &b, &a).unwrap();
    let copy = copy_of("notes.txt", "from a");
    assert_eq!(report.exit_code(), 2);
    assert_eq!(report.conflicts, 1);
    for dir in [&a, &b] {
        assert!(is(dir, "notes.txt", "from b") && is(dir, &copy, "from a"));
    }
    assert_eq!(tree(&a), tree(&b));
    // It repeats until it is closed, and status shows it without failing.
    assert_eq!(cmd::sync(O, &b, &a).unwrap().conflicts, 1);
    let open = status(&a);
    assert!(has(&open, "conflict", "notes.txt"));
    assert_eq!(open[0].conflict.as_ref(), Some(&copy));
    // Editing the winner where both versions are visible closes it everywhere.
    put(&a, "notes.txt", "merged", 3);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (0, 0));
    assert!(is(&a, "notes.txt", "merged") && is(&b, "notes.txt", "merged"));
    assert_eq!(tree(&a), tree(&b));
    assert!(status(&b).is_empty());
}

#[test]
fn story_c1_three_vaults() {
    let (tmp, a, b) = pair(&[("f.txt", "v1")]);
    let c = tmp.path().join("c");
    cmd::clone(O, &b, &c).unwrap();
    // A change relays through b between two vaults that never meet.
    put(&a, "f.txt", "from a", 1);
    put(&c, "new.txt", "from c", 2);
    cmd::sync(O, &a, &b).unwrap();
    cmd::sync(O, &b, &c).unwrap();
    cmd::sync(O, &a, &b).unwrap();
    assert!(is(&c, "f.txt", "from a") && is(&a, "new.txt", "from c"));
    // Any pairing, any order, including a conflict.
    put(&b, "g.txt", "from b", 3);
    put(&a, "f.txt", "a again", 4);
    put(&c, "f.txt", "c again", 5);
    fs::remove_file(a.join("new.txt")).unwrap();
    cmd::sync(O, &c, &a).unwrap();
    cmd::push(O, &b, &c).unwrap();
    cmd::sync(O, &b, &a).unwrap();
    cmd::sync(O, &c, &b).unwrap();
    cmd::sync(O, &a, &c).unwrap();
    cmd::sync(O, &b, &a).unwrap();
    assert_eq!(tree(&a), tree(&b));
    assert_eq!(tree(&b), tree(&c));
    let files = tree(&a);
    assert_eq!(files["f.txt"], "c again");
    assert_eq!(files[&copy_of("f.txt", "a again")], "a again");
    assert!(files.contains_key("g.txt") && !files.contains_key("new.txt"));
}

#[test]
fn story_d1_no_history() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "v2", 1);
    cmd::sync(O, &b, &a).unwrap();
    put(&b, "f.txt", "v3", 2);
    put(&a, "f.txt", "v3 conflicting", 3);
    cmd::sync(O, &b, &a).unwrap();
    cmd::snapshot(O, &a).unwrap();
    for dir in [&a, &b] {
        assert!(names(&dir.join(".syncit/blobs")).is_empty());
        assert!(names(&dir.join(".syncit/tmp")).is_empty());
    }
}

#[test]
fn story_d2_history() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    configure(&b, |c| c.keep_local_history = true);
    put(&a, "f.txt", "v2", 1);
    cmd::sync(O, &b, &a).unwrap();
    put(&b, "f.txt", "v3", 2);
    cmd::sync(O, &b, &a).unwrap();
    let kept: Vec<String> = ["v1", "v2", "v3"]
        .iter()
        .map(|v| sha256(v.as_bytes()))
        .collect();
    assert_eq!(
        names(&b.join(".syncit/blobs")),
        kept.iter().cloned().collect()
    );
    assert!(names(&a.join(".syncit/blobs")).is_empty());
    assert_eq!(
        fs::read_to_string(b.join(".syncit/blobs").join(&kept[0])).unwrap(),
        "v1"
    );

    // Kept versions are a blob source: one held nowhere else comes back.
    fs::remove_file(a.join("f.txt")).unwrap();
    fs::remove_file(b.join("f.txt")).unwrap();
    fs::remove_file(a.join(".syncit/cache")).unwrap();
    fs::remove_file(b.join(".syncit/cache")).unwrap();
    assert_eq!(cmd::sync(O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&a, "f.txt", "v3") && is(&b, "f.txt", "v3"));
    // Turned off, nothing more is kept, and what was kept stays.
    cmd::config(O, &b, Some(Setting::LocalHistory(false))).unwrap();
    put(&b, "f.txt", "v4", 3);
    cmd::sync(O, &b, &a).unwrap();
    assert_eq!(names(&b.join(".syncit/blobs")).len(), 3);
}

#[test]
fn story_d3_snapshot() {
    let (tmp, a) = vault(&[("f.txt", "v1")]);
    assert_eq!(cmd::snapshot(O, &a).unwrap().recorded, 1);
    put(&a, "f.txt", "v2", 1);
    let report = cmd::snapshot(O, &a).unwrap();
    assert_eq!((report.recorded, report.exit_code()), (1, 0));
    assert_eq!(cmd::snapshot(O, &a).unwrap().recorded, 0);
    let entries = log(&a).entries;
    let find = |content: &str| {
        entries
            .iter()
            .find(|(_, e)| e.hash == Some(sha256(content.as_bytes())))
            .unwrap()
    };
    let ((first, v1), (_, v2)) = (find("v1"), find("v2"));
    assert_eq!(entries.len(), 2);
    assert!(v1.parents.is_empty());
    assert_eq!(v2.parents, std::slice::from_ref(first));
    assert_eq!(names(&a.join(".syncit/log")).len(), 2);
    // A later sync delivers both.
    let b = tmp.path().join("b");
    cmd::clone(O, &a, &b).unwrap();
    assert_eq!(log(&b).entries, entries);
    assert!(is(&b, "f.txt", "v2"));
}

#[test]
fn story_e1_rerun() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    put(&a, "f.txt", "v2", 1);
    put(&b, "d/h.txt", "h", 1);
    let first = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((first.recorded, first.exit_code()), (2, 0));
    assert_eq!(
        first.to_string(),
        "recorded 2, added 1, updated 1, deleted 0, conflicts 0, pending 0, skipped 0"
    );
    let (segments, files) = (names(&a.join(".syncit/log")), tree(&a));
    let second = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(second, Summary::default());
    assert_eq!(names(&a.join(".syncit/log")), segments);
    assert_eq!(names(&b.join(".syncit/log")), segments);
    assert_eq!(tree(&a), files);
    assert_eq!(tree(&b), files);
}

// ---- Safety and recovery -------------------------------------------------

#[test]
fn edit_beats_delete() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "edited", 1);
    fs::remove_file(b.join("f.txt")).unwrap();
    let report = cmd::sync(O, &b, &a).unwrap();
    assert!(is(&a, "f.txt", "edited") && is(&b, "f.txt", "edited"));
    assert_eq!(report.exit_code(), 2);
    assert_eq!(report.conflicts, 1);
    // The next edit closes it.
    put(&b, "f.txt", "again", 2);
    assert_eq!(cmd::sync(O, &b, &a).unwrap().exit_code(), 0);
    assert!(is(&a, "f.txt", "again"));
}

/// One head at `path`, a file, whose parents are all tombstones; and the
/// vault's cache holds it as a file, not as a deletion.
fn replaced_deletion(dir: &Path, path: &str) -> bool {
    let (log, cache) = (log(dir), Cache::load(dir).unwrap());
    let PathState::Resolved { heads, .. } = log.resolve(path) else {
        return false;
    };
    let head = &log.entries[&heads[0]];
    heads.len() == 1
        && head.hash.is_some()
        && !head.parents.is_empty()
        && head.parents.iter().all(|p| log.entries[p].hash.is_none())
        && cache.rows.contains_key(path)
        && !cache.deleted.contains_key(path)
}

#[test]
fn recreate_after_delete() {
    // Re-created where it was deleted, with an old version's bytes and with
    // new ones: an ordinary new version, not an old copy and not a conflict.
    let (_tmp, a, b) = pair(&[("same.txt", "one"), ("diff.txt", "one"), ("k.txt", "k")]);
    for path in ["same.txt", "diff.txt"] {
        fs::remove_file(a.join(path)).unwrap();
    }
    assert_eq!(cmd::sync(O, &a, &b).unwrap().deleted, 2);
    for dir in [&a, &b] {
        let seen = Cache::load(dir).unwrap().deleted;
        assert_eq!(seen.len(), 2, "both vaults record the deletions");
    }
    put(&a, "same.txt", "one", 1);
    put(&a, "diff.txt", "two", 1);
    for _ in 0..2 {
        let report = cmd::sync(O, &a, &b).unwrap();
        assert_eq!((report.conflicts, report.exit_code()), (0, 0));
    }
    for dir in [&a, &b] {
        assert!(is(dir, "same.txt", "one") && is(dir, "diff.txt", "two"));
        assert!(replaced_deletion(dir, "same.txt") && replaced_deletion(dir, "diff.txt"));
    }

    // The same in the vault that received the deletion.
    fs::remove_file(a.join("same.txt")).unwrap();
    cmd::sync(O, &a, &b).unwrap();
    put(&b, "same.txt", "one", 2);
    for _ in 0..2 {
        let report = cmd::sync(O, &b, &a).unwrap();
        assert_eq!((report.conflicts, report.exit_code()), (0, 0));
    }
    assert!(is(&a, "same.txt", "one") && replaced_deletion(&b, "same.txt"));

    // With the cache lost there is no record: the file is kept all the same.
    fs::remove_file(a.join("same.txt")).unwrap();
    cmd::sync(O, &a, &b).unwrap();
    fs::remove_file(a.join(".syncit/cache")).unwrap();
    put(&a, "same.txt", "one", 3);
    for _ in 0..2 {
        let report = cmd::sync(O, &a, &b).unwrap();
        assert_eq!(
            (report.conflicts, report.deleted, report.exit_code()),
            (0, 0, 0)
        );
    }
    assert!(is(&b, "same.txt", "one") && replaced_deletion(&a, "same.txt"));
}

#[test]
fn recreate_against_unseen_recreation() {
    // b deletes f.txt. a re-creates it; b hears of that through c, which
    // carries the news but not the content. b then re-creates it too, with
    // an old version's bytes. b had seen only the deletion, so the two
    // re-creations conflict and both are kept.
    let (tmp, a, b) = pair(&[("f.txt", "v1"), ("sub/s.txt", "s")]);
    let c = tmp.path().join("c");
    cmd::clone(O, &a.join("sub"), &c).unwrap();
    fs::remove_file(b.join("f.txt")).unwrap();
    cmd::sync(O, &b, &a).unwrap();
    put(&a, "f.txt", "other", 1);
    cmd::sync(O, &c, &a).unwrap();
    let report = cmd::sync(O, &b, &c).unwrap();
    assert_eq!(report.pending, 1);
    put(&b, "f.txt", "v1", 2);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(report.conflicts, 1);
    let copy = copy_of("f.txt", "other");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "v1") && is(dir, &copy, "other"));
    }
}

#[test]
fn log_lists_versions() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    cmd::config(O, &b, Some(Setting::Nickname("other".into()))).unwrap();
    put(&a, "f.txt", "v2", 1);
    cmd::sync(O, &a, &b).unwrap();
    put(&b, "f.txt", "v3", 2);
    cmd::sync(O, &a, &b).unwrap();
    let seen = cmd::log(O, &a, &paths(&["f.txt"])).unwrap();
    let line = |i: usize| {
        let v: &syncit::output::Version = &seen[i];
        let names = v.nicknames.join(",");
        (v.hash.clone(), names, v.current, v.available)
    };
    // Newest first. Without local history, only what is on disk is available.
    let hash = |content: &str| Some(sha256(content.as_bytes()));
    assert_eq!(seen.len(), 3);
    assert_eq!(line(0), (hash("v3"), "other".into(), true, true));
    assert_eq!(line(1), (hash("v2"), "a".into(), false, false));
    assert_eq!(line(2), (hash("v1"), "a".into(), false, false));
    // A deletion is listed, and the path is still found though no file is there.
    fs::remove_file(a.join("f.txt")).unwrap();
    cmd::snapshot(O, &a).unwrap();
    let seen = cmd::log(O, &a.join("d"), &paths(&["../f.txt"])).unwrap();
    assert_eq!(seen.len(), 4);
    assert_eq!((&seen[0].hash, seen[0].current), (&None, true));
    assert!(!seen[1].current && !seen[1].available);
    // No paths: every file. With local history, old versions are available.
    assert_eq!(cmd::log(O, &a, &[]).unwrap().len(), 5);
    // Kept from the first run after it is turned on.
    cmd::config(O, &b, Some(Setting::LocalHistory(true))).unwrap();
    cmd::snapshot(O, &b).unwrap();
    put(&b, "d/g.txt", "g2", 3);
    cmd::snapshot(O, &b).unwrap();
    put(&b, "d/g.txt", "g3", 4);
    cmd::snapshot(O, &b).unwrap();
    let seen = cmd::log(O, &b, &paths(&["d"])).unwrap();
    let available: Vec<bool> = seen.iter().map(|v| v.available).collect();
    assert_eq!(available, [true, true, true]);
}

#[test]
fn restore_version() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("gone.txt", "g1"), ("k.txt", "k")]);
    let hash = |content: &str| sha256(content.as_bytes());
    let restore = |dir: &Path, path: &str, content: &str, in_place| {
        cmd::restore(O, dir, Path::new(path), &hash(content)[..8], in_place)
    };
    // Without local history an old version cannot be brought back.
    put(&a, "f.txt", "v2", 1);
    cmd::snapshot(O, &a).unwrap();
    let error = restore(&a, "f.txt", "v1", false).unwrap_err().to_string();
    assert!(error.contains("local-history"), "{error}");
    // The other vault still has v1 on disk; history collects it from there.
    cmd::config(O, &a, Some(Setting::LocalHistory(true))).unwrap();
    cmd::sync(O, &a, &b).unwrap();

    // A copy: beside the file, which is left alone; an ordinary new file.
    let copy = copy_of("f.txt", "v1");
    assert_eq!(restore(&a, "f.txt", "v1", false).unwrap(), copy);
    assert!(is(&a, "f.txt", "v2") && is(&a, &copy, "v1"));
    assert_eq!(restore(&a, "f.txt", "v1", false).unwrap(), copy, "again");
    assert!(has(&status(&a), "new", &copy));
    assert_eq!(cmd::sync(O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&b, &copy, "v1"));
    // A copy deleted since the last scan can be had again.
    fs::remove_file(a.join(&copy)).unwrap();
    assert_eq!(restore(&a, "f.txt", "v1", false).unwrap(), copy);
    assert!(status(&a).is_empty(), "the same file as before");
    // The copy moved over the file is an edit like any other.
    fs::rename(a.join(&copy), a.join("f.txt")).unwrap();
    assert_eq!(cmd::sync(O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&b, "f.txt", "v1") && !b.join(&copy).exists());

    // In place: unrecorded edits are recorded first, then the old version
    // replaces the file as a new version on top of them.
    put(&a, "f.txt", "unrecorded", 2);
    assert_eq!(restore(&a, "f.txt", "v2", true).unwrap(), "f.txt");
    assert!(is(&a, "f.txt", "v2"));
    let log_a = log(&a);
    let PathState::Resolved { heads, .. } = log_a.resolve("f.txt") else {
        panic!("unresolved");
    };
    let head = &log_a.entries[&heads[0]];
    assert_eq!((heads.len(), head.hash.clone()), (1, Some(hash("v2"))));
    let parent = &log_a.entries[&head.parents[0]];
    assert_eq!(parent.hash, Some(hash("unrecorded")));
    assert!(status(&a).is_empty(), "already recorded");
    assert_eq!(restore(&a, "f.txt", "v2", true).unwrap(), "f.txt", "again");
    // The replaced version was kept, and can be had back in turn.
    assert!(restore(&a, "f.txt", "unrecorded", false).is_ok());
    fs::remove_file(a.join(copy_of("f.txt", "unrecorded"))).unwrap();
    assert_eq!(cmd::sync(O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&b, "f.txt", "v2"));

    // In place, for a deleted file: it stays, here and in the other vault.
    fs::remove_file(a.join("gone.txt")).unwrap();
    assert_eq!(cmd::sync(O, &a, &b).unwrap().deleted, 1);
    assert_eq!(restore(&a, "gone.txt", "g1", true).unwrap(), "gone.txt");
    for _ in 0..2 {
        let report = cmd::sync(O, &a, &b).unwrap();
        assert_eq!((report.conflicts, report.exit_code()), (0, 0));
    }
    assert!(is(&a, "gone.txt", "g1") && is(&b, "gone.txt", "g1"));
    assert!(replaced_deletion(&a, "gone.txt"));

    // Refusals.
    let refused = |result: anyhow::Result<String>, why: &str| {
        let error = result.unwrap_err().to_string();
        assert!(error.contains(why), "{error}");
    };
    refused(restore(&a, "f.txt", "never", false), "no version");
    refused(restore(&a, "nowhere.txt", "v1", false), "no version");
    refused(
        cmd::restore(O, &a, Path::new("f.txt"), "", false),
        "no version",
    );
    put(&a, &copy, "something else", 3);
    refused(restore(&a, "f.txt", "v1", false), "is taken");
    // c holds only k.txt: f.txt is not on disk there.
    let c = a.parent().unwrap().join("c");
    cmd::clone(O, &a.join("k.txt"), &c).unwrap();
    refused(restore(&c, "f.txt", "v1", true), "not on disk");
    assert!(!c.join("f.txt").exists());
}

#[test]
fn same_edit_no_conflict() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "same", 1);
    put(&b, "f.txt", "same", 2);
    put(&a, "new.txt", "also same", 3);
    put(&b, "new.txt", "also same", 3);
    let report = cmd::sync(O, &b, &a).unwrap();
    let recorded = Summary {
        recorded: 4,
        ..Summary::default()
    };
    assert_eq!(report, recorded, "nothing was written or left open");
    assert_eq!(tree(&a).len(), 2);
    assert_eq!(tree(&a), tree(&b));
}

#[test]
fn new_file_at_existing_path() {
    let (tmp, a) = vault(&[("sub/s.txt", "s"), ("f.txt", "F"), ("g.txt", "G")]);
    let b = tmp.path().join("b");
    cmd::clone(O, &a.join("sub"), &b).unwrap();
    assert!(!b.join("f.txt").exists());
    // Equal content is adopted silently.
    put(&b, "f.txt", "F", 5);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(report, Summary::default());
    // Different content is an ordinary conflict.
    put(&b, "g.txt", "other", 5);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("g.txt", "G");
    for dir in [&a, &b] {
        assert!(is(dir, "g.txt", "other") && is(dir, &copy, "G"));
    }
    // Both are held from now on.
    put(&a, "f.txt", "F2", 6);
    cmd::sync(O, &b, &a).unwrap();
    assert!(is(&b, "f.txt", "F2"));
}

#[test]
fn lost_cache() {
    let (_tmp, a, b) = pair(&[("f1.txt", "v1"), ("f2.txt", "two"), ("d/f3.txt", "three")]);
    // A single vault: nothing is recorded again.
    fs::remove_file(a.join(".syncit/cache")).unwrap();
    let report = cmd::snapshot(O, &a).unwrap();
    assert_eq!(
        (report.recorded, names(&a.join(".syncit/log")).len()),
        (0, 1)
    );
    // With a peer: old files are brought forward, absent ones are not deletions.
    fs::remove_file(b.join("d/f3.txt")).unwrap();
    fs::remove_file(b.join(".syncit/cache")).unwrap();
    put(&a, "f1.txt", "v2", 1);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(
        (report.recorded, report.deleted, report.exit_code()),
        (1, 0, 0)
    );
    assert_eq!(report.updated, 1);
    assert!(is(&b, "f1.txt", "v2") && is(&b, "f2.txt", "two"));
    assert!(is(&a, "d/f3.txt", "three") && is(&b, "d/f3.txt", "three"));
}

#[test]
fn copied_vault() {
    let (tmp, a) = vault(&[("f.txt", "v1"), ("same.txt", "s")]);
    cmd::snapshot(O, &a).unwrap();
    let twin = tmp.path().join("twin");
    copy_dir(&a, &twin);
    put(&a, "f.txt", "one", 1);
    put(&twin, "f.txt", "two", 2);
    let report = cmd::sync(O, &a, &twin).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("f.txt", "one");
    for dir in [&a, &twin] {
        assert!(is(dir, "f.txt", "two") && is(dir, &copy, "one") && is(dir, "same.txt", "s"));
    }
}

#[test]
fn restored_backup() {
    let (tmp, a) = vault(&[("f.txt", "v1")]);
    cmd::snapshot(O, &a).unwrap();
    let (backup, b) = (tmp.path().join("backup"), tmp.path().join("b"));
    copy_dir(&a, &backup);
    cmd::clone(O, &a, &b).unwrap();
    put(&a, "f.txt", "v2", 1);
    cmd::sync(O, &a, &b).unwrap();
    put(&a, "f.txt", "v3", 2);
    cmd::sync(O, &a, &b).unwrap();
    fs::remove_dir_all(&a).unwrap();
    copy_dir(&backup, &a);
    assert!(is(&a, "f.txt", "v1"));
    put(&a, "f.txt", "after restore", 3);
    let report = cmd::sync(O, &a, &b).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("f.txt", "v3");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "after restore") && is(dir, &copy, "v3"));
    }
    // Restored and not edited: simply brought forward.
    fs::remove_dir_all(&a).unwrap();
    copy_dir(&backup, &a);
    let report = cmd::sync(O, &a, &b).unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(tree(&a), tree(&b));
    assert_eq!(tree(&a).len(), 2);
}

#[test]
fn pending_then_edit() {
    let (tmp, a, b) = pair(&[("f.txt", "v1"), ("sub/s.txt", "s")]);
    let c = tmp.path().join("c");
    cmd::clone(O, &a.join("sub"), &c).unwrap();
    // c carries the news of v2 but not its content.
    put(&a, "f.txt", "v2", 1);
    cmd::sync(O, &c, &a).unwrap();
    let report = cmd::sync(O, &b, &c).unwrap();
    assert_eq!((report.pending, report.exit_code()), (1, 2));
    assert!(is(&b, "f.txt", "v1"));
    assert!(has(&status(&b), "behind", "f.txt"));
    // An edit made without having seen v2 conflicts with it.
    put(&b, "f.txt", "mine", 2);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(report.conflicts, 1);
    let copy = copy_of("f.txt", "v2");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "mine") && is(dir, &copy, "v2"));
    }
}

#[test]
fn changed_during_run() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("gone.txt", "g")]);
    put(&a, "f.txt", "v2", 1);
    fs::remove_file(a.join("gone.txt")).unwrap();
    {
        let (mut va, mut vb) = (
            Vault::open(O, &a, true).unwrap(),
            Vault::open(O, &b, true).unwrap(),
        );
        scan(O, &mut va, false).unwrap();
        scan(O, &mut vb, false).unwrap();
        // Edits that land after the scan and before the files are replaced.
        put(&b, "f.txt", "local edit", 2);
        put(&b, "gone.txt", "kept", 2);
        let mut report = Summary::default();
        cmd::receive(O, &mut vb, &va, &mut report).unwrap();
        materialise(O, &mut vb, Some(&va), &mut report).unwrap();
        assert!(report.has("skipped", "f.txt") && report.has("skipped", "gone.txt"));
        assert_eq!(report.exit_code(), 2);
    }
    assert!(is(&b, "f.txt", "local edit") && is(&b, "gone.txt", "kept"));
    // The next run records them: a conflict, and an edit that beats the delete.
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(report.recorded, 2);
    let copy = copy_of("f.txt", "v2");
    for dir in [&a, &b] {
        assert!(
            is(dir, "f.txt", "local edit") && is(dir, &copy, "v2") && is(dir, "gone.txt", "kept")
        );
    }
}

#[test]
fn untracked_in_the_way() {
    let (tmp, a) = vault(&[("f.txt", "theirs"), ("d/g.txt", "theirs")]);
    let b = tmp.path().join("b");
    cmd::init(O, &b).unwrap();
    {
        let (mut va, mut vb) = (
            Vault::open(O, &a, true).unwrap(),
            Vault::open(O, &b, true).unwrap(),
        );
        scan(O, &mut va, false).unwrap();
        scan(O, &mut vb, false).unwrap();
        put(&b, "f.txt", "mine", 1);
        put(&b, "d", "a file where a folder is needed", 1);
        let mut report = Summary::default();
        cmd::receive(O, &mut vb, &va, &mut report).unwrap();
        materialise(O, &mut vb, Some(&va), &mut report).unwrap();
        assert!(report.has("skipped", "f.txt") && report.has("skipped", "d/g.txt"));
        let way = |e: &syncit::output::Event| e.note == "an untracked file is in the way";
        assert!(report.events.iter().any(way));
    }
    assert!(is(&b, "f.txt", "mine") && is(&b, "d", "a file where a folder is needed"));
    assert_eq!(tree(&b).len(), 2);
}

#[test]
fn same_tick_edit() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path();
    fs::write(a.join("f.txt"), "one").unwrap();
    cmd::init(O, a).unwrap();
    assert_eq!(cmd::snapshot(O, a).unwrap().recorded, 1);
    // Same size, same mtime, different content, right after the scan.
    let mtime = a.join("f.txt").metadata().unwrap().modified().unwrap();
    fs::write(a.join("f.txt"), "two").unwrap();
    File::options()
        .write(true)
        .open(a.join("f.txt"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert!(has(&status(a), "modified", "f.txt"));
    assert_eq!(cmd::snapshot(O, a).unwrap().recorded, 1);
    assert_eq!(cmd::snapshot(O, a).unwrap().recorded, 0);
}

#[test]
fn interrupted_before_cache() {
    let (_tmp, a) = vault(&[("f1.txt", "v1")]);
    cmd::snapshot(O, &a).unwrap();
    let cache = fs::read(a.join(".syncit/cache")).unwrap();
    put(&a, "f1.txt", "v2", 1);
    put(&a, "f2.txt", "new", 1);
    assert_eq!(cmd::snapshot(O, &a).unwrap().recorded, 2);
    let entries = log(&a).entries;
    // As if the run had stopped after the segment and before the cache.
    // The edit is written again as the identical entry; the new file is
    // recognised from the log.
    fs::write(a.join(".syncit/cache"), cache).unwrap();
    assert_eq!(cmd::snapshot(O, &a).unwrap().recorded, 1);
    assert_eq!(log(&a).entries, entries);
    assert_eq!(cmd::snapshot(O, &a).unwrap().recorded, 0);
}

#[test]
fn interrupted_transfer() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "v2", 1);
    cmd::snapshot(O, &a).unwrap();
    let before = names(&a.join(".syncit/log"));
    put(&a, "f.txt", "v3", 2);
    cmd::snapshot(O, &a).unwrap();
    // Only the newest segment arrives: its parent is missing.
    for name in names(&a.join(".syncit/log")).difference(&before) {
        fs::copy(
            a.join(".syncit/log").join(name),
            b.join(".syncit/log").join(name),
        )
        .unwrap();
    }
    assert!(has(&status(&b), "incomplete", "f.txt"));
    {
        let mut report = Summary::default();
        let mut vb = Vault::open(O, &b, true).unwrap();
        materialise(O, &mut vb, None, &mut report).unwrap();
        assert_eq!((report.incomplete, report.exit_code()), (1, 2));
    }
    assert!(is(&b, "f.txt", "v1"));
    // A local edit meanwhile is still recorded from the cache.
    assert_eq!(cmd::snapshot(O, &b).unwrap().recorded, 0);
    let report = cmd::pull(O, &b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    assert!(is(&b, "f.txt", "v3"));
}

#[test]
fn hostile_segment() {
    let (tmp, a) = vault(&[("f.txt", "good")]);
    cmd::snapshot(O, &a).unwrap();
    let outside = tmp.path().join("evil.txt");
    let plant = |text: String| {
        let name = sha256(text.as_bytes());
        fs::write(a.join(".syncit/log").join(&name), text).unwrap();
        name
    };
    let hash = sha256(b"good");
    let escape = plant(format!(
        "{{\"nickname\":\"a\",\"syncit\":1}}\n{{\"hash\":\"{hash}\",\"mtime\":1,\"parents\":[],\"path\":\"../evil.txt\"}}\n"
    ));
    plant(
        "{\"nickname\":\"a\",\"syncit\":1}\n{\"hash\":null, \"mtime\":1,\"parents\":[],\"path\":\"x\"}\n"
            .to_string(),
    );
    // A segment from before nicknames.
    plant("{\"syncit\":1}\n".to_string());
    fs::write(a.join(".syncit/log/not-a-hash"), "junk").unwrap();
    let b = tmp.path().join("b");
    cmd::init(O, &b).unwrap();
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.rejected, report.exit_code()), (4, 2));
    assert!(!b.join(".syncit/log").join(&escape).exists());
    assert!(is(&b, "f.txt", "good"));
    assert_eq!(names(&b.join(".syncit/log")).len(), 1);
    assert!(!outside.exists());
    assert_eq!(tree(&b).len(), 1);
}

#[test]
fn symlink_escape() {
    let (tmp, a) = vault(&[("d/x.txt", "x"), ("ok.txt", "ok")]);
    let (b, outside) = (tmp.path().join("b"), tmp.path().join("outside"));
    cmd::init(O, &b).unwrap();
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, b.join("d")).unwrap();
    let report = cmd::pull(O, &b, &a).unwrap();
    assert!(report.has("skipped", "d/x.txt"));
    assert_eq!(report.exit_code(), 2);
    assert!(names(&outside).is_empty());
    assert!(is(&b, "ok.txt", "ok"));
    // Nor is the link itself recorded or removed.
    assert_eq!(cmd::sync(O, &b, &a).unwrap().recorded, 0);
    assert!(b.join("d").is_symlink() && is(&a, "d/x.txt", "x"));
}

#[test]
fn ignored_files() {
    let (tmp, a) = vault(&[
        ("secret.txt", "a's secret"),
        ("keep.txt", "k"),
        ("build/out.o", "o"),
    ]);
    let b = tmp.path().join("b");
    cmd::init(O, &b).unwrap();
    // Listing the ignore file in itself keeps these rules local to b.
    let rules = "# local rules\n\n.syncitignore\nsecret.txt\nbuild/\n";
    fs::write(b.join(".syncitignore"), rules).unwrap();
    put(&b, "secret.txt", "b's secret", 1);
    put(&b, "sub/secret.txt", "deeper", 1);
    let report = cmd::sync(O, &b, &a).unwrap();
    // Not recorded, not overwritten, not written.
    assert_eq!(report.exit_code(), 0);
    assert!(is(&b, "secret.txt", "b's secret") && is(&a, "secret.txt", "a's secret"));
    assert!(!a.join("sub").exists() && !b.join("build").exists() && is(&b, "keep.txt", "k"));
    let recorded = |dir: &Path, content: &str| {
        log(dir)
            .entries
            .values()
            .any(|e| e.hash == Some(sha256(content.as_bytes())))
    };
    assert!(!recorded(&b, "b's secret") && !recorded(&b, "deeper"));
    // A recorded file that becomes ignored is neither deleted nor read as deleted.
    fs::write(b.join(".syncitignore"), format!("{rules}keep.txt\n")).unwrap();
    fs::remove_file(a.join("keep.txt")).unwrap();
    cmd::sync(O, &b, &a).unwrap();
    assert!(is(&b, "keep.txt", "k"));
    put(&a, "keep.txt", "back", 2);
    cmd::sync(O, &b, &a).unwrap();
    fs::remove_file(b.join("keep.txt")).unwrap();
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!(report.recorded, 0);
    assert!(is(&a, "keep.txt", "back"));
    assert!(!a.join(".syncitignore").exists());
}

#[test]
fn ignore_file_syncs() {
    // `.syncitignore` is an ordinary file: its rules travel with it.
    let (_tmp, a, b) = pair(&[("f.txt", "f")]);
    put(&a, ".syncitignore", "scratch\n", 1);
    put(&a, "scratch/x.txt", "x", 1);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.recorded, report.exit_code()), (1, 0));
    assert!(is(&b, ".syncitignore", "scratch\n") && !b.join("scratch").exists());
    // From the next run on they apply in b too.
    put(&b, "scratch/y.txt", "y", 2);
    assert_eq!(cmd::sync(O, &b, &a).unwrap().recorded, 0);
    assert!(!a.join("scratch/y.txt").exists() && is(&b, "scratch/y.txt", "y"));
    // Two vaults changing the rules is an ordinary conflict.
    put(&a, ".syncitignore", "scratch\nfrom a\n", 3);
    put(&b, ".syncitignore", "scratch\nfrom b\n", 4);
    assert_eq!(cmd::sync(O, &b, &a).unwrap().conflicts, 1);
    assert!(is(&a, ".syncitignore", "scratch\nfrom b\n"));
}

#[test]
fn corrupt_blob_source() {
    let (tmp, a) = vault(&[("f.txt", "good"), ("ok.txt", "ok")]);
    cmd::snapshot(O, &a).unwrap();
    // Same size and mtime: a's scan does not see the change.
    put(&a, "f.txt", "evil", 0);
    let b = tmp.path().join("b");
    let report = cmd::clone(O, &a, &b).unwrap();
    assert_eq!(report.recorded, 0);
    assert_eq!((report.pending, report.exit_code()), (1, 2));
    assert_eq!(tree(&b).keys().collect::<Vec<_>>(), ["ok.txt"]);
}

#[test]
fn copy_deleted_stays_deleted() {
    let (_tmp, a, b) = pair(&[("f.txt", "base")]);
    put(&a, "f.txt", "from a", 1);
    put(&b, "f.txt", "from b", 2);
    cmd::sync(O, &b, &a).unwrap();
    let copy = copy_of("f.txt", "from a");
    assert!(a.join(&copy).exists() && b.join(&copy).exists());
    cmd::sync(O, &b, &a).unwrap(); // the copies are recorded as ordinary files
    fs::remove_file(a.join(&copy)).unwrap();
    for _ in 0..3 {
        cmd::sync(O, &b, &a).unwrap();
        assert!(!a.join(&copy).exists() && !b.join(&copy).exists());
    }
    assert!(is(&a, "f.txt", "from b") && is(&b, "f.txt", "from b"));
}

#[test]
fn move_refuses() {
    let (tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g"), ("e/h.txt", "h")]);
    let err = |from: &Path, what: &[&str], to: &Path| {
        cmd::move_to(O, from, &paths(what), to)
            .unwrap_err()
            .to_string()
    };
    // The target excludes the path: nothing is sent, nothing removed.
    let c = tmp.path().join("c");
    cmd::clone(O, &a.join("d"), &c).unwrap();
    put(&b, "f.txt", "edit", 1);
    let error = err(&b, &["d", "f.txt"], &c);
    assert!(
        error.contains("excludes f.txt") && !error.contains("d/g.txt"),
        "{error}"
    );
    assert!(is(&b, "f.txt", "edit") && is(&b, "d/g.txt", "g") && !c.join("f.txt").exists());
    assert_eq!(Config::load(&b).unwrap().exclude.len(), 0);
    // A file the target did not know it had is recorded by its scan, and is
    // then a different version: with two paths given, nothing is removed.
    cmd::move_to(O, &a, &paths(&["e"]), &b).unwrap();
    put(&a, "e/h.txt", "mine", 2);
    fs::remove_file(a.join(".syncit/cache")).unwrap();
    let error = err(&b, &["e", "d"], &a);
    assert!(
        error.contains("e/h.txt (it holds a different version") && !error.contains("d/g.txt"),
        "{error}"
    );
    assert!(is(&b, "e/h.txt", "h") && is(&b, "d/g.txt", "g") && is(&a, "e/h.txt", "mine"));
    // The target holds a different, newer version.
    put(&a, "d/g.txt", "newer", 5);
    let error = err(&b, &["d"], &a);
    assert!(
        error.contains("d/g.txt (it holds a different version"),
        "{error}"
    );
    assert!(is(&b, "d/g.txt", "g"));
    // No files under the path; a vault with itself.
    assert!(err(&b, &["nope"], &a).contains("nothing to move"));
    assert!(cmd::move_to(O, &b, &paths(&["f.txt"]), &b).is_err());
    // An unsynced edit is carried over and moved.
    cmd::move_to(O, &b, &paths(&["f.txt"]), &a).unwrap();
    assert!(is(&a, "f.txt", "edit") && !b.join("f.txt").exists());
    // Nothing reads as deleted afterwards.
    assert_eq!(cmd::sync(O, &b, &a).unwrap().recorded, 0);
    assert!(is(&a, "f.txt", "edit"));
}

#[test]
fn interrupted_move() {
    // A move stopped after it saved the filter and before it removed the
    // files: the folder is excluded and its files are still held.
    let (_tmp, a, b) = pair(&[("d/g.txt", "g"), ("d/h.txt", "h"), ("f.txt", "f")]);
    configure(&b, |c| c.exclude.push("d".into()));
    // The next scan includes what is on disk: nothing is lost.
    assert_eq!(cmd::snapshot(O, &b).unwrap().recorded, 0);
    let config = Config::load(&b).unwrap();
    assert!(config.includes("d/g.txt") && config.includes("d/h.txt"));
    assert!(is(&b, "d/g.txt", "g") && is(&b, "d/h.txt", "h"));
    // The files are synced as before, in both directions.
    put(&a, "d/g.txt", "edit", 1);
    assert_eq!(cmd::sync(O, &b, &a).unwrap().exit_code(), 0);
    assert!(is(&b, "d/g.txt", "edit") && is(&a, "d/h.txt", "h"));
    // Moving again finishes the job and leaves one exclude.
    let report = cmd::move_to(O, &b, &paths(&["d"]), &a).unwrap();
    assert!(report.has("moved", "d/g.txt") && report.has("moved", "d/h.txt"));
    let config = Config::load(&b).unwrap();
    assert_eq!(
        (config.include, config.exclude),
        (vec![String::new()], vec!["d".to_string()])
    );
    assert!(!b.join("d").exists() && is(&a, "d/g.txt", "edit") && is(&a, "d/h.txt", "h"));
}

#[test]
fn config_command() {
    let (_tmp, a) = vault(&[("sub/f.txt", "v1")]);
    let config = |change| cmd::config(O, &a.join("sub"), change);
    let history = |change: Option<bool>| {
        let change = change.map(Setting::LocalHistory);
        config(change).unwrap().keep_local_history
    };
    assert!(!history(None));
    assert!(history(Some(true)));
    assert!(Config::load(&a).unwrap().keep_local_history);
    // The setting takes effect.
    cmd::snapshot(O, &a).unwrap();
    assert!(a.join(".syncit/blobs").join(sha256(b"v1")).exists());
    assert!(!history(Some(false)));
    assert!(!Config::load(&a).unwrap().keep_local_history);
    assert!(a.join(".syncit/blobs").join(sha256(b"v1")).exists());
    assert!(!history(None));

    // A vault is nicknamed after its folder until it is renamed.
    assert_eq!(config(None).unwrap().nickname, "a");
    let renamed = config(Some(Setting::Nickname("laptop:notes".into()))).unwrap();
    assert_eq!(renamed.nickname, "laptop:notes");
    assert_eq!(Config::load(&a).unwrap().nickname, "laptop:notes");
    for bad in ["", "two words", "bell\u{7}", &"n".repeat(65)] {
        let refused = config(Some(Setting::Nickname(bad.into()))).is_err();
        assert!(refused, "{bad:?}");
    }
    assert_eq!(config(None).unwrap().nickname, "laptop:notes");
}

#[test]
fn blame_names_the_recording_vault() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("sub/g.txt", "g")]);
    let blame = |dir: &Path, list: &[&str]| cmd::blame(O, dir, &paths(list)).unwrap();
    let by = |path: &str, who: &[&str]| {
        let who = who.iter().map(|w| w.to_string()).collect::<Vec<_>>();
        (path.to_string(), who)
    };
    // A vault that only receives a file records nothing about it.
    assert_eq!(
        blame(&b, &[]),
        [by("f.txt", &["a"]), by("sub/g.txt", &["a"])]
    );

    put(&b, "f.txt", "v2", 1);
    put(&b, "gone.txt", "x", 1);
    cmd::sync(O, &b, &a).unwrap();
    fs::remove_file(b.join("gone.txt")).unwrap();
    // Recorded identically in both vaults: one entry, two recorders.
    put(&a, "same.txt", "same", 2);
    put(&b, "same.txt", "same", 2);
    cmd::sync(O, &b, &a).unwrap();
    let all = [
        by("f.txt", &["b"]),
        by("same.txt", &["a", "b"]),
        by("sub/g.txt", &["a"]),
    ];
    assert_eq!(blame(&a, &[]), all, "a deleted file is left out");
    assert_eq!(blame(&b, &[]), all);
    assert_eq!(blame(&a, &["sub"]), [by("sub/g.txt", &["a"])]);
    assert_eq!(blame(&a.join("sub"), &["g.txt"]), [by("sub/g.txt", &["a"])]);

    // A renamed vault's new changes carry the new name; old ones keep the old.
    cmd::config(O, &a, Some(Setting::Nickname("drive".into()))).unwrap();
    put(&a, "f.txt", "v3", 3);
    cmd::sync(O, &b, &a).unwrap();
    assert_eq!(
        blame(&b, &["f.txt", "sub"]),
        [by("f.txt", &["drive"]), by("sub/g.txt", &["a"])]
    );
    // An edit no scan has recorded is not attributed.
    put(&b, "f.txt", "v4", 4);
    assert_eq!(blame(&b, &["f.txt"]), [by("f.txt", &["drive"])]);
}

#[test]
fn nested_vault_is_ordinary_files() {
    // A vault dragged into another: its files sync as part of the outer one.
    let (tmp, a, b) = pair(&[("f.txt", "f"), ("inner/old.txt", "old")]);
    cmd::init(O, &a.join("inner")).unwrap();
    put(&a, "inner/new.txt", "new", 1);
    let report = cmd::sync(O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.deleted), (0, 0));
    assert!(is(&b, "inner/old.txt", "old") && is(&b, "inner/new.txt", "new"));
    assert!(!b.join("inner/.syncit").exists());
    // The inner vault still works on its own, and its changes reach the outer.
    assert_eq!(cmd::snapshot(O, &a.join("inner")).unwrap().recorded, 2);
    put(&b, "inner/new.txt", "edited", 2);
    cmd::sync(O, &b, &a).unwrap();
    assert!(is(&a, "inner/new.txt", "edited"));
    assert!(has(&status(&a.join("inner")), "modified", "new.txt"));
    // Vaults that share files do not sync with each other.
    assert!(cmd::sync(O, &a, &a.join("inner")).is_err());
    assert!(cmd::pull(O, &a.join("inner"), &a).is_err());
    assert!(cmd::clone(O, &a, &a.join("copy")).is_err());
    assert!(!a.join("copy").exists());
    assert!(cmd::clone(O, &a, &tmp.path().join("c")).is_ok());
}

#[test]
fn version_mismatch() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    configure(&a, |c| c.syncit = 2);
    put(&a, "f.txt", "v2", 1);
    for result in [
        cmd::sync(O, &b, &a),
        cmd::push(O, &b, &a),
        cmd::pull(O, &b, &a),
        cmd::snapshot(O, &a),
    ] {
        let error = result
            .expect_err("a version 2 vault is refused")
            .to_string();
        assert!(error.contains("version 2"), "{error}");
    }
    assert!(is(&b, "f.txt", "v1"));
    assert!(
        cmd::sync(O, &b, &b).is_err(),
        "a vault does not sync with itself"
    );
}

#[test]
fn push_counts_excluded() {
    let (tmp, a) = vault(&[("d/g.txt", "g"), ("music/m.mp3", "m"), ("f.txt", "f")]);
    let c = tmp.path().join("c");
    cmd::clone(O, &a.join("d"), &c).unwrap();
    fs::write(c.join(".syncitignore"), "d/tmp\n.syncitignore\n").unwrap();
    put(&a, "d/tmp/t.txt", "t", 1);
    cmd::snapshot(O, &a).unwrap();
    let (va, vc) = (
        Vault::open(O, &a, true).unwrap(),
        Vault::open(O, &c, true).unwrap(),
    );
    // Excluded there, or ignored there; a path it includes is taken.
    assert_eq!(cmd::excluded(&va, &vc), ["f.txt", "music/m.mp3"]);
    assert_eq!(cmd::ignored(&va, &vc), ["d/tmp/t.txt"]);
    assert!(cmd::excluded(&vc, &va).is_empty());
}

#[test]
fn status_states() {
    let (tmp, a) = vault(&[
        ("d/g.txt", "g"),
        ("d/sub/1.txt", "1"),
        ("d/sub/2.txt", "2"),
        ("f.txt", "f"),
        ("music/m.mp3", "m"),
        ("music/n.mp3", "n"),
    ]);
    let c = tmp.path().join("c");
    cmd::clone(O, &a.join("d"), &c).unwrap();
    let line = |path: &str, files, ours, theirs| Holding {
        path: path.into(),
        files,
        ours,
        theirs,
        conflict: None,
    };
    let list = |dir: &Path, paths: &[PathBuf], peer| cmd::status(O, dir, paths, peer, true);
    // One vault: nothing needs anything. Of every file, a folder in one
    // state is one line; the rest is excluded.
    assert!(status(&c).is_empty());
    assert_eq!(
        list(&c, &[], None).unwrap(),
        [
            line("d/", 3, "current", None),
            line("f.txt", 1, "excluded", None),
            line("music/", 2, "excluded", None),
        ]
    );
    // A path lists that folder's children, or that file.
    assert_eq!(
        list(&c, &paths(&["music", "d/g.txt"]), None).unwrap(),
        [
            line("music/m.mp3", 1, "excluded", None),
            line("music/n.mp3", 1, "excluded", None),
            line("d/g.txt", 1, "current", None),
        ]
    );
    // A path outside the vault is refused, and says how to compare.
    let error = list(&c, std::slice::from_ref(&a), None).unwrap_err();
    assert!(format!("{error:#}").contains("--vs <vault>"), "{error:#}");
    // Unrecorded changes are states: an edit, a new file and a deletion
    // there, a new file and a deletion here, and a file that is ignored here.
    // A vault that still has a file removed in the other is behind it.
    fs::remove_file(c.join("d/sub/1.txt")).unwrap();
    put(&a, "d/g.txt", "edit", 1);
    put(&a, "d/tmp/t.txt", "t", 1);
    fs::remove_file(a.join("f.txt")).unwrap();
    put(&c, "d/new.txt", "n", 1);
    fs::write(c.join(".syncitignore"), "d/tmp\n.syncitignore\n").unwrap();
    let listing = || {
        let mut out = BTreeMap::new();
        all(tmp.path(), &mut out);
        out
    };
    let before = listing();
    let every = [
        line("d/g.txt", 1, "behind", Some("modified")),
        line("d/new.txt", 1, "new", Some("missing")),
        line("d/sub/1.txt", 1, "removed", Some("behind")),
        line("d/sub/2.txt", 1, "current", Some("current")),
        line("d/tmp/t.txt", 1, "ignored", Some("new")),
        line("f.txt", 1, "excluded", Some("removed")),
        line("music/", 2, "excluded", Some("current")),
    ];
    assert_eq!(list(&c, &[], Some(&a)).unwrap(), every);
    // Without `all`, only the lines where a vault needs something.
    let needy: Vec<Holding> = [0, 1, 2, 4, 5].map(|i| every[i].clone()).into();
    assert_eq!(cmd::status(O, &c, &[], Some(&a), false).unwrap(), needy);
    // The other way round, and limited to a file or a folder here.
    assert_eq!(
        list(&a, &paths(&["d/g.txt"]), Some(&c)).unwrap(),
        [line("d/g.txt", 1, "modified", Some("behind"))]
    );
    assert_eq!(
        list(&a, &paths(&["music"]), Some(&c)).unwrap(),
        [
            line("music/m.mp3", 1, "current", Some("excluded")),
            line("music/n.mp3", 1, "current", Some("excluded")),
        ]
    );
    assert_eq!(
        list(&a, &paths(&["d/sub/1.txt"]), Some(&c)).unwrap(),
        [line("d/sub/1.txt", 1, "behind", Some("removed"))]
    );
    // One vault: what a scan would record here.
    assert_eq!(
        status(&c),
        [
            line("d/new.txt", 1, "new", None),
            line("d/sub/1.txt", 1, "removed", None),
        ]
    );
    assert!(before == listing(), "status wrote something");
    // One vault that has heard of a deletion and still has the file: a sync
    // did not finish.
    fs::remove_file(a.join("d/sub/2.txt")).unwrap();
    cmd::snapshot(O, &a).unwrap();
    {
        let mut vc = Vault::open(O, &c, true).unwrap();
        let va = Vault::open(O, &a, false).unwrap();
        cmd::receive(O, &mut vc, &va, &mut Summary::default()).unwrap();
    }
    assert_eq!(
        list(&c, &paths(&["d/sub"]), None).unwrap(),
        [
            line("d/sub/1.txt", 1, "removed", None),
            line("d/sub/2.txt", 1, "incomplete", None),
        ]
    );
    // A removal that is recorded, with no vault that has the file, is not
    // listed.
    cmd::snapshot(O, &c).unwrap();
    assert_eq!(
        list(&c, &paths(&["d/sub"]), None).unwrap(),
        [line("d/sub/2.txt", 1, "incomplete", None)]
    );
}

/// Every file and folder under `dir`, `.syncit/` included, with its bytes.
fn all(dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
    for item in fs::read_dir(dir).unwrap() {
        let path = item.unwrap().path();
        if path.is_dir() {
            all(&path, out);
        } else {
            out.insert(path.clone(), fs::read(&path).unwrap());
        }
    }
    out.insert(dir.to_path_buf(), vec![]);
}

#[test]
fn dry_run_matches_real_run() {
    let dry = &Options {
        json: false,
        dry_run: true,
    };
    type Run = fn(&Options, &Path, &Path) -> anyhow::Result<Summary>;
    for run in [cmd::sync as Run, cmd::push, cmd::pull] {
        let files = [
            ("notes.txt", "base"),
            ("gone.txt", "x"),
            ("d/both.txt", "base"),
        ];
        let (tmp, a, b) = pair(&files);
        configure(&b, |c| c.keep_local_history = true);
        put(&a, "notes.txt", "edited in a", 1);
        put(&a, "new/in-a.txt", "new", 1);
        fs::remove_file(a.join("gone.txt")).unwrap();
        put(&b, "new-in-b.txt", "new", 1);
        put(&a, "d/both.txt", "from a", 1);
        put(&b, "d/both.txt", "from b", 2);

        let listing = || {
            let mut out = BTreeMap::new();
            all(tmp.path(), &mut out);
            out
        };
        let before = listing();
        let planned = run(dry, &a, &b).unwrap();
        assert!(before == listing(), "a dry run wrote something");
        let done = run(O, &a, &b).unwrap();
        assert!(before != listing());
        assert!(done.conflicts == 1 && done.recorded == 6, "{done:?}");
        assert_eq!(planned, done);
    }

    // `move`, both when it would go through and when it would be refused.
    let (tmp, a, b) = pair(&[("keep.txt", "k"), ("big/one.bin", "1")]);
    put(&a, "big/two.bin", "2", 1);
    let c = tmp.path().join("c");
    cmd::clone(O, &a.join("keep.txt"), &c).unwrap();
    let listing = || {
        let mut out = BTreeMap::new();
        all(tmp.path(), &mut out);
        out
    };
    let before = listing();
    let refused = cmd::move_to(dry, &a, &paths(&["big"]), &c).unwrap_err();
    assert!(refused.to_string().contains("excludes"), "{refused}");
    let planned = cmd::move_to(dry, &a, &paths(&["big"]), &b).unwrap();
    assert!(before == listing(), "a dry run wrote something");
    let done = cmd::move_to(O, &a, &paths(&["big"]), &b).unwrap();
    assert!(done.has("moved", "big/one.bin") && done.has("moved", "big/two.bin"));
    assert!(!a.join("big").exists() && is(&b, "big/two.bin", "2"));
    assert_eq!(planned, done);
}
