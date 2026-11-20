//! Integration tests: vaults in temporary directories, driven through the
//! library's command functions. Names follow SPEC.md section 14.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use syncit::commands::{self as cmd, Report, Setting};
use syncit::config::Config;
use syncit::entry::sha256;
use syncit::log::Log;
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
    cmd::init(&a).unwrap();
    (tmp, a)
}

/// Vault `a` holding `files`, and `b` cloned from it.
fn pair(files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf) {
    let (tmp, a) = vault(files);
    let b = tmp.path().join("b");
    assert_eq!(cmd::clone(&a, &b).unwrap().exit_code(), 0);
    (tmp, a, b)
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

fn log(dir: &Path) -> Log {
    Log::load(&dir.join(".syncit/log"), true).unwrap()
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
    let report = cmd::push(&b, &a).unwrap();
    assert_eq!((report.exit_code(), report.recorded), (0, 1));
    assert!(report.has("updated", "notes.txt"));
    assert!(is(&a, "notes.txt", "v2"));
}

#[test]
fn story_a2_add_and_push() {
    let (_tmp, a, b) = pair(&[("notes.txt", "v1"), ("dir/old.txt", "o")]);
    put(&b, "new/deep/n.txt", "n", 1);
    fs::remove_file(b.join("dir/old.txt")).unwrap();
    // A push does not bring the peer's changes here.
    put(&a, "only-a.txt", "a", 1);
    let report = cmd::push(&b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    assert!(is(&a, "new/deep/n.txt", "n"));
    assert!(
        !a.join("dir").exists(),
        "the deletion propagates, and the empty folder goes"
    );
    assert!(!b.join("only-a.txt").exists());
    cmd::pull(&b, &a).unwrap();
    assert!(is(&b, "only-a.txt", "a"));
}

#[test]
fn story_a3_drop() {
    let (_tmp, a, b) = pair(&[("x/1.txt", "one"), ("y/2.txt", "two"), ("keep.txt", "k")]);
    let report = cmd::drop(&b, &paths(&["x", "y/2.txt"]), Some(&a), false).unwrap();
    assert_eq!(report.count("dropped"), 2);
    assert!(!b.join("x").exists() && !b.join("y").exists() && is(&b, "keep.txt", "k"));
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!((report.exit_code(), report.recorded), (0, 0));
    assert!(is(&a, "x/1.txt", "one") && is(&a, "y/2.txt", "two"));
    assert!(!b.join("x").exists() && !b.join("y").exists());
    cmd::get(&b, &[a.join("x"), a.join("y/2.txt")]).unwrap();
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
    cmd::clone(&a.join("foo/bar"), &b).unwrap();
    assert_eq!(tree(&b).keys().collect::<Vec<_>>(), ["foo/bar/x.txt"]);
    put(&b, "foo/bar/x.txt", "x2", 1);
    put(&b, "foo/bar/new.txt", "n", 1);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!((report.exit_code(), report.count("deleted")), (0, 0));
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
    cmd::clone(&a.join("foo/bar"), &b).unwrap();
    cmd::drop(&b, &paths(&["foo/bar/big"]), Some(&a), false).unwrap();
    // A second folder.
    cmd::get(&b, &[a.join("other")]).unwrap();
    assert!(is(&b, "other/y.txt", "y") && !b.join("foo/bar/big").exists());
    // New files outside the wanted paths: the three examples of section 7.
    put(&b, "zap/x/y.txt", "new", 1);
    put(&b, "zap/x/z.txt", "new too", 1);
    put(&b, "foo/new.txt", "n", 1);
    put(&b, "foo/bar/big/n/a.txt", "a", 1);
    assert_eq!(cmd::sync(&b, &a).unwrap().exit_code(), 0);
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
    put(&a, "foo/elsewhere.txt", "not wanted in b", 2);
    cmd::sync(&b, &a).unwrap();
    assert!(is(&b, "zap/later.txt", "later"));
    assert!(!b.join("foo/elsewhere.txt").exists() && !b.join("foo/bar/big/f.bin").exists());
    assert!(is(&a, "foo/bar/big/f.bin", "f"));
}

#[test]
fn story_b1_conflict() {
    let (_tmp, a, b) = pair(&[("notes.txt", "base")]);
    put(&a, "notes.txt", "from a", 1);
    put(&b, "notes.txt", "from b", 2);
    let report = cmd::sync(&b, &a).unwrap();
    let copy = copy_of("notes.txt", "from a");
    assert_eq!(report.exit_code(), 2);
    assert!(report.conflicts["notes.txt"].contains(&copy));
    assert!(report.to_string().contains(&format!(
        "conflict: notes.txt — kept the newer version, other saved as {copy}"
    )));
    for dir in [&a, &b] {
        assert!(is(dir, "notes.txt", "from b") && is(dir, &copy, "from a"));
    }
    assert_eq!(tree(&a), tree(&b));
    // It repeats until it is closed, and status shows it without failing.
    assert_eq!(cmd::sync(&b, &a).unwrap().conflicts.len(), 1);
    let status = cmd::status(&a).unwrap();
    assert_eq!((status.conflicts.len(), status.exit_code()), (1, 0));
    // Editing the winner where both versions are visible closes it everywhere.
    put(&a, "notes.txt", "merged", 3);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!((report.exit_code(), report.conflicts.len()), (0, 0));
    assert!(is(&a, "notes.txt", "merged") && is(&b, "notes.txt", "merged"));
    assert_eq!(tree(&a), tree(&b));
    assert_eq!(cmd::status(&b).unwrap().conflicts.len(), 0);
}

#[test]
fn story_c1_three_vaults() {
    let (tmp, a, b) = pair(&[("f.txt", "v1")]);
    let c = tmp.path().join("c");
    cmd::clone(&b, &c).unwrap();
    // A change relays through b between two vaults that never meet.
    put(&a, "f.txt", "from a", 1);
    put(&c, "new.txt", "from c", 2);
    cmd::sync(&a, &b).unwrap();
    cmd::sync(&b, &c).unwrap();
    cmd::sync(&a, &b).unwrap();
    assert!(is(&c, "f.txt", "from a") && is(&a, "new.txt", "from c"));
    // Any pairing, any order, including a conflict.
    put(&b, "g.txt", "from b", 3);
    put(&a, "f.txt", "a again", 4);
    put(&c, "f.txt", "c again", 5);
    fs::remove_file(a.join("new.txt")).unwrap();
    cmd::sync(&c, &a).unwrap();
    cmd::push(&b, &c).unwrap();
    cmd::sync(&b, &a).unwrap();
    cmd::sync(&c, &b).unwrap();
    cmd::sync(&a, &c).unwrap();
    cmd::sync(&b, &a).unwrap();
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
    cmd::sync(&b, &a).unwrap();
    put(&b, "f.txt", "v3", 2);
    put(&a, "f.txt", "v3 conflicting", 3);
    cmd::sync(&b, &a).unwrap();
    cmd::snapshot(&a).unwrap();
    for dir in [&a, &b] {
        assert!(names(&dir.join(".syncit/blobs")).is_empty());
        assert!(names(&dir.join(".syncit/tmp")).is_empty());
    }
}

#[test]
fn story_d2_history() {
    let (tmp, a, b) = pair(&[("f.txt", "v1")]);
    configure(&b, |c| c.keep_local_history = true);
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&b, &a).unwrap();
    put(&b, "f.txt", "v3", 2);
    cmd::sync(&b, &a).unwrap();
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

    // A remote store: blobs wait locally while it is missing.
    let store = tmp.path().join("store");
    let c = tmp.path().join("c");
    cmd::clone(&a, &c).unwrap();
    configure(&c, |c| c.store = Some(store.to_str().unwrap().to_string()));
    put(&c, "f.txt", "v4", 3);
    cmd::snapshot(&c).unwrap();
    let waiting = names(&c.join(".syncit/blobs"));
    assert_eq!(waiting, BTreeSet::from([sha256(b"v4")]));
    fs::create_dir(&store).unwrap();
    cmd::snapshot(&c).unwrap();
    assert_eq!(names(&store), waiting);
    assert!(names(&c.join(".syncit/blobs")).is_empty());
    assert_eq!(fs::read_to_string(store.join(sha256(b"v4"))).unwrap(), "v4");
    // The remote is a blob source: a version held nowhere else comes back.
    put(&c, "f.txt", "v5", 4);
    cmd::snapshot(&c).unwrap();
    let d = tmp.path().join("d");
    cmd::clone(&c, &d).unwrap();
    configure(&d, |c| c.store = Some(store.to_str().unwrap().to_string()));
    fs::remove_file(c.join("f.txt")).unwrap();
    fs::remove_file(d.join("f.txt")).unwrap();
    fs::remove_file(d.join(".syncit/cache")).unwrap();
    fs::remove_file(c.join(".syncit/cache")).unwrap();
    assert_eq!(cmd::pull(&d, &c).unwrap().exit_code(), 0);
    assert!(is(&d, "f.txt", "v5"));
}

#[test]
fn story_d3_snapshot() {
    let (tmp, a) = vault(&[("f.txt", "v1")]);
    assert_eq!(cmd::snapshot(&a).unwrap().recorded, 1);
    put(&a, "f.txt", "v2", 1);
    let report = cmd::snapshot(&a).unwrap();
    assert_eq!((report.recorded, report.exit_code()), (1, 0));
    assert_eq!(cmd::snapshot(&a).unwrap().recorded, 0);
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
    cmd::clone(&a, &b).unwrap();
    assert_eq!(log(&b).entries, entries);
    assert!(is(&b, "f.txt", "v2"));
}

#[test]
fn story_e1_rerun() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    put(&a, "f.txt", "v2", 1);
    put(&b, "d/h.txt", "h", 1);
    let first = cmd::sync(&b, &a).unwrap();
    assert_eq!((first.recorded, first.exit_code()), (2, 0));
    assert!(first.to_string().ends_with(
        "recorded 2, added 1, updated 1, deleted 0, conflicts 0, pending 0, skipped 0\n"
    ));
    let (segments, files) = (names(&a.join(".syncit/log")), tree(&a));
    let second = cmd::sync(&b, &a).unwrap();
    assert_eq!(
        (second.recorded, second.events.len(), second.exit_code()),
        (0, 0, 0)
    );
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
    let report = cmd::sync(&b, &a).unwrap();
    assert!(is(&a, "f.txt", "edited") && is(&b, "f.txt", "edited"));
    assert_eq!(report.exit_code(), 2);
    assert_eq!(
        report.conflicts["f.txt"],
        "kept the file; it was deleted in another vault"
    );
    // The next edit closes it.
    put(&b, "f.txt", "again", 2);
    assert_eq!(cmd::sync(&b, &a).unwrap().exit_code(), 0);
    assert!(is(&a, "f.txt", "again"));
}

#[test]
fn same_edit_no_conflict() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "same", 1);
    put(&b, "f.txt", "same", 2);
    put(&a, "new.txt", "also same", 3);
    put(&b, "new.txt", "also same", 3);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!(
        (
            report.exit_code(),
            report.conflicts.len(),
            report.events.len()
        ),
        (0, 0, 0)
    );
    assert_eq!(tree(&a).len(), 2);
    assert_eq!(tree(&a), tree(&b));
}

#[test]
fn new_file_at_existing_path() {
    let (tmp, a) = vault(&[("sub/s.txt", "s"), ("f.txt", "F"), ("g.txt", "G")]);
    let b = tmp.path().join("b");
    cmd::clone(&a.join("sub"), &b).unwrap();
    assert!(!b.join("f.txt").exists());
    // Equal content is adopted silently.
    put(&b, "f.txt", "F", 5);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!(
        (report.exit_code(), report.recorded, report.events.len()),
        (0, 0, 0)
    );
    // Different content is an ordinary conflict.
    put(&b, "g.txt", "other", 5);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!((report.exit_code(), report.conflicts.len()), (2, 1));
    let copy = copy_of("g.txt", "G");
    for dir in [&a, &b] {
        assert!(is(dir, "g.txt", "other") && is(dir, &copy, "G"));
    }
    // Both are held from now on.
    put(&a, "f.txt", "F2", 6);
    cmd::sync(&b, &a).unwrap();
    assert!(is(&b, "f.txt", "F2"));
}

#[test]
fn lost_cache() {
    let (_tmp, a, b) = pair(&[("f1.txt", "v1"), ("f2.txt", "two"), ("d/f3.txt", "three")]);
    // A single vault: nothing is recorded again.
    fs::remove_file(a.join(".syncit/cache")).unwrap();
    let report = cmd::snapshot(&a).unwrap();
    assert_eq!(
        (report.recorded, names(&a.join(".syncit/log")).len()),
        (0, 1)
    );
    // With a peer: old files are brought forward, absent ones are not deletions.
    fs::remove_file(b.join("d/f3.txt")).unwrap();
    fs::remove_file(b.join(".syncit/cache")).unwrap();
    put(&a, "f1.txt", "v2", 1);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!(
        (report.recorded, report.count("deleted"), report.exit_code()),
        (1, 0, 0)
    );
    assert!(report.has("updated", "f1.txt"));
    assert!(is(&b, "f1.txt", "v2") && is(&b, "f2.txt", "two"));
    assert!(is(&a, "d/f3.txt", "three") && is(&b, "d/f3.txt", "three"));
}

#[test]
fn copied_vault() {
    let (tmp, a) = vault(&[("f.txt", "v1"), ("same.txt", "s")]);
    cmd::snapshot(&a).unwrap();
    let twin = tmp.path().join("twin");
    copy_dir(&a, &twin);
    put(&a, "f.txt", "one", 1);
    put(&twin, "f.txt", "two", 2);
    let report = cmd::sync(&a, &twin).unwrap();
    assert_eq!((report.exit_code(), report.conflicts.len()), (2, 1));
    let copy = copy_of("f.txt", "one");
    for dir in [&a, &twin] {
        assert!(is(dir, "f.txt", "two") && is(dir, &copy, "one") && is(dir, "same.txt", "s"));
    }
}

#[test]
fn restored_backup() {
    let (tmp, a) = vault(&[("f.txt", "v1")]);
    cmd::snapshot(&a).unwrap();
    let (backup, b) = (tmp.path().join("backup"), tmp.path().join("b"));
    copy_dir(&a, &backup);
    cmd::clone(&a, &b).unwrap();
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&a, &b).unwrap();
    put(&a, "f.txt", "v3", 2);
    cmd::sync(&a, &b).unwrap();
    fs::remove_dir_all(&a).unwrap();
    copy_dir(&backup, &a);
    assert!(is(&a, "f.txt", "v1"));
    put(&a, "f.txt", "after restore", 3);
    let report = cmd::sync(&a, &b).unwrap();
    assert_eq!((report.exit_code(), report.conflicts.len()), (2, 1));
    let copy = copy_of("f.txt", "v3");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "after restore") && is(dir, &copy, "v3"));
    }
    // Restored and not edited: simply brought forward.
    fs::remove_dir_all(&a).unwrap();
    copy_dir(&backup, &a);
    let report = cmd::sync(&a, &b).unwrap();
    assert!(report.has("updated", "f.txt"));
    assert_eq!(tree(&a), tree(&b));
    assert_eq!(tree(&a).len(), 2);
}

#[test]
fn pending_then_edit() {
    let (tmp, a, b) = pair(&[("f.txt", "v1"), ("sub/s.txt", "s")]);
    let c = tmp.path().join("c");
    cmd::clone(&a.join("sub"), &c).unwrap();
    // c carries the news of v2 but not its content.
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&c, &a).unwrap();
    let report = cmd::sync(&b, &c).unwrap();
    assert!(report.has("pending", "f.txt"));
    assert_eq!(report.exit_code(), 2);
    assert!(is(&b, "f.txt", "v1"));
    assert!(cmd::status(&b).unwrap().has("behind", "f.txt"));
    // An edit made without having seen v2 conflicts with it.
    put(&b, "f.txt", "mine", 2);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!(report.conflicts.len(), 1);
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
            Vault::open(&a, true).unwrap(),
            Vault::open(&b, true).unwrap(),
        );
        scan(&mut va, false).unwrap();
        scan(&mut vb, false).unwrap();
        // Edits that land after the scan and before the files are replaced.
        put(&b, "f.txt", "local edit", 2);
        put(&b, "gone.txt", "kept", 2);
        let mut report = Report::default();
        cmd::receive(&mut vb, &va, &mut report).unwrap();
        materialise(&mut vb, Some(&va), &mut report.events).unwrap();
        assert!(report.has("skipped", "f.txt") && report.has("skipped", "gone.txt"));
        assert_eq!(report.exit_code(), 2);
    }
    assert!(is(&b, "f.txt", "local edit") && is(&b, "gone.txt", "kept"));
    // The next run records them: a conflict, and an edit that beats the delete.
    let report = cmd::sync(&b, &a).unwrap();
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
    cmd::init(&b).unwrap();
    {
        let (mut va, mut vb) = (
            Vault::open(&a, true).unwrap(),
            Vault::open(&b, true).unwrap(),
        );
        scan(&mut va, false).unwrap();
        scan(&mut vb, false).unwrap();
        put(&b, "f.txt", "mine", 1);
        put(&b, "d", "a file where a folder is needed", 1);
        let mut report = Report::default();
        cmd::receive(&mut vb, &va, &mut report).unwrap();
        materialise(&mut vb, Some(&va), &mut report.events).unwrap();
        assert!(report.has("skipped", "f.txt") && report.has("skipped", "d/g.txt"));
        assert!(
            report
                .to_string()
                .contains("skipped: f.txt — an untracked file is in the way")
        );
    }
    assert!(is(&b, "f.txt", "mine") && is(&b, "d", "a file where a folder is needed"));
    assert_eq!(tree(&b).len(), 2);
}

#[test]
fn same_tick_edit() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path();
    fs::write(a.join("f.txt"), "one").unwrap();
    cmd::init(a).unwrap();
    assert_eq!(cmd::snapshot(a).unwrap().recorded, 1);
    // Same size, same mtime, different content, right after the scan.
    let mtime = a.join("f.txt").metadata().unwrap().modified().unwrap();
    fs::write(a.join("f.txt"), "two").unwrap();
    File::options()
        .write(true)
        .open(a.join("f.txt"))
        .unwrap()
        .set_modified(mtime)
        .unwrap();
    assert!(cmd::status(a).unwrap().has("modified", "f.txt"));
    assert_eq!(cmd::snapshot(a).unwrap().recorded, 1);
    assert_eq!(cmd::snapshot(a).unwrap().recorded, 0);
}

#[test]
fn interrupted_before_cache() {
    let (_tmp, a) = vault(&[("f1.txt", "v1")]);
    cmd::snapshot(&a).unwrap();
    let cache = fs::read(a.join(".syncit/cache")).unwrap();
    put(&a, "f1.txt", "v2", 1);
    put(&a, "f2.txt", "new", 1);
    assert_eq!(cmd::snapshot(&a).unwrap().recorded, 2);
    let entries = log(&a).entries;
    // As if the run had stopped after the segment and before the cache.
    // The edit is written again as the identical entry; the new file is
    // recognised from the log.
    fs::write(a.join(".syncit/cache"), cache).unwrap();
    assert_eq!(cmd::snapshot(&a).unwrap().recorded, 1);
    assert_eq!(log(&a).entries, entries);
    assert_eq!(cmd::snapshot(&a).unwrap().recorded, 0);
}

#[test]
fn interrupted_transfer() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "v2", 1);
    cmd::snapshot(&a).unwrap();
    let before = names(&a.join(".syncit/log"));
    put(&a, "f.txt", "v3", 2);
    cmd::snapshot(&a).unwrap();
    // Only the newest segment arrives: its parent is missing.
    for name in names(&a.join(".syncit/log")).difference(&before) {
        fs::copy(
            a.join(".syncit/log").join(name),
            b.join(".syncit/log").join(name),
        )
        .unwrap();
    }
    assert!(cmd::status(&b).unwrap().has("incomplete", "f.txt"));
    {
        let mut report = Report::default();
        let mut vb = Vault::open(&b, true).unwrap();
        materialise(&mut vb, None, &mut report.events).unwrap();
        assert!(report.has("incomplete", "f.txt"));
        assert_eq!(report.exit_code(), 2);
    }
    assert!(is(&b, "f.txt", "v1"));
    // A local edit meanwhile is still recorded from the cache.
    assert_eq!(cmd::snapshot(&b).unwrap().recorded, 0);
    let report = cmd::pull(&b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    assert!(is(&b, "f.txt", "v3"));
}

#[test]
fn hostile_segment() {
    let (tmp, a) = vault(&[("f.txt", "good")]);
    cmd::snapshot(&a).unwrap();
    let outside = tmp.path().join("evil.txt");
    let plant = |text: String| {
        let name = sha256(text.as_bytes());
        fs::write(a.join(".syncit/log").join(&name), text).unwrap();
        name
    };
    let hash = sha256(b"good");
    let escape = plant(format!(
        "{{\"syncit\":1}}\n{{\"hash\":\"{hash}\",\"mtime\":1,\"parents\":[],\"path\":\"../evil.txt\"}}\n"
    ));
    plant(
        "{\"syncit\":1}\n{\"hash\":null, \"mtime\":1,\"parents\":[],\"path\":\"x\"}\n".to_string(),
    );
    fs::write(a.join(".syncit/log/not-a-hash"), "junk").unwrap();
    let b = tmp.path().join("b");
    cmd::init(&b).unwrap();
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!((report.count("rejected"), report.exit_code()), (3, 2));
    assert!(report.has("rejected", &format!("segment {escape}")));
    assert!(is(&b, "f.txt", "good"));
    assert_eq!(names(&b.join(".syncit/log")).len(), 1);
    assert!(!outside.exists());
    assert_eq!(tree(&b).len(), 1);
}

#[test]
fn symlink_escape() {
    let (tmp, a) = vault(&[("d/x.txt", "x"), ("ok.txt", "ok")]);
    let (b, outside) = (tmp.path().join("b"), tmp.path().join("outside"));
    cmd::init(&b).unwrap();
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, b.join("d")).unwrap();
    let report = cmd::pull(&b, &a).unwrap();
    assert!(report.has("skipped", "d/x.txt"));
    assert_eq!(report.exit_code(), 2);
    assert!(names(&outside).is_empty());
    assert!(is(&b, "ok.txt", "ok"));
    // Nor is the link itself recorded or removed.
    assert_eq!(cmd::sync(&b, &a).unwrap().recorded, 0);
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
    cmd::init(&b).unwrap();
    fs::write(
        b.join(".syncit/ignore"),
        "# local rules\n\nsecret.txt\nbuild/\n",
    )
    .unwrap();
    put(&b, "secret.txt", "b's secret", 1);
    put(&b, "sub/secret.txt", "deeper", 1);
    let report = cmd::sync(&b, &a).unwrap();
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
    fs::write(b.join(".syncit/ignore"), "secret.txt\nbuild/\nkeep.txt\n").unwrap();
    fs::remove_file(a.join("keep.txt")).unwrap();
    cmd::sync(&b, &a).unwrap();
    assert!(is(&b, "keep.txt", "k"));
    put(&a, "keep.txt", "back", 2);
    cmd::sync(&b, &a).unwrap();
    fs::remove_file(b.join("keep.txt")).unwrap();
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!(report.recorded, 0);
    assert!(is(&a, "keep.txt", "back"));
}

#[test]
fn corrupt_blob_source() {
    let (tmp, a) = vault(&[("f.txt", "good"), ("ok.txt", "ok")]);
    cmd::snapshot(&a).unwrap();
    // Same size and mtime: a's scan does not see the change.
    put(&a, "f.txt", "evil", 0);
    let b = tmp.path().join("b");
    let report = cmd::clone(&a, &b).unwrap();
    assert_eq!(report.recorded, 0);
    assert!(report.has("pending", "f.txt"));
    assert_eq!(report.exit_code(), 2);
    assert_eq!(tree(&b).keys().collect::<Vec<_>>(), ["ok.txt"]);
}

#[test]
fn copy_deleted_stays_deleted() {
    let (_tmp, a, b) = pair(&[("f.txt", "base")]);
    put(&a, "f.txt", "from a", 1);
    put(&b, "f.txt", "from b", 2);
    cmd::sync(&b, &a).unwrap();
    let copy = copy_of("f.txt", "from a");
    assert!(a.join(&copy).exists() && b.join(&copy).exists());
    cmd::sync(&b, &a).unwrap(); // the copies are recorded as ordinary files
    fs::remove_file(a.join(&copy)).unwrap();
    for _ in 0..3 {
        cmd::sync(&b, &a).unwrap();
        assert!(!a.join(&copy).exists() && !b.join(&copy).exists());
    }
    assert!(is(&a, "f.txt", "from b") && is(&b, "f.txt", "from b"));
}

#[test]
fn drop_refuses_sole_copy() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    let both = paths(&["d", "f.txt"]);
    // No vault to check against and no history: nothing vouches for a copy.
    let error = cmd::drop(&b, &both, None, false).unwrap_err().to_string();
    assert!(
        error.contains("f.txt") && error.contains("--check"),
        "{error}"
    );
    // An unrecorded edit is recorded, and then blocks the drop even against
    // a vault. With two paths given, nothing is dropped.
    put(&b, "f.txt", "edit", 1);
    let error = cmd::drop(&b, &both, Some(&a), false)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("f.txt") && !error.contains("d/g.txt"),
        "{error}"
    );
    assert!(is(&b, "f.txt", "edit") && is(&b, "d/g.txt", "g"));
    let recorded = |e: &syncit::entry::Entry| e.hash == Some(sha256(b"edit"));
    assert!(log(&b).entries.values().any(recorded));
    assert_eq!(Config::load(&b).unwrap().exclude.len(), 0);
    cmd::push(&b, &a).unwrap();
    let report = cmd::drop(&b, &both, Some(&a), false).unwrap();
    assert_eq!(report.count("dropped"), 2);
    assert!(tree(&b).is_empty() && is(&a, "f.txt", "edit"));
    // Nothing reads as deleted afterwards.
    assert_eq!(cmd::sync(&b, &a).unwrap().recorded, 0);
    assert!(is(&a, "f.txt", "edit") && is(&a, "d/g.txt", "g"));
    // Local history is another copy; --force skips the check.
    assert!(cmd::drop(&a, &paths(&["f.txt"]), None, false).is_err());
    cmd::config(&a, Some(Setting::LocalHistory(true))).unwrap();
    cmd::drop(&a, &paths(&["f.txt"]), None, false).unwrap();
    assert!(!a.join("f.txt").exists() && a.join(".syncit/blobs").join(sha256(b"edit")).exists());
    cmd::config(&a, Some(Setting::LocalHistory(false))).unwrap();
    assert!(cmd::drop(&a, &paths(&["d"]), Some(&a), false).is_err());
    cmd::drop(&a, &paths(&["d"]), None, true).unwrap();
    assert!(tree(&a).is_empty());
}

#[test]
fn config_command() {
    let (tmp, a) = vault(&[("sub/f.txt", "v1")]);
    let notes = |change| cmd::config(&a.join("sub"), change).unwrap().notes;
    assert_eq!(notes(None), ["local-history: off", "store: none"]);
    assert_eq!(
        notes(Some(Setting::LocalHistory(true)))[0],
        "local-history: on"
    );
    assert!(Config::load(&a).unwrap().keep_local_history);
    // A store is given relative to where the command runs, and kept absolute.
    let store = tmp.path().canonicalize().unwrap().join("store");
    let set = notes(Some(Setting::Store(Some("../../store".into()))));
    assert!(set[1].contains("not reachable"), "{set:?}");
    assert_eq!(Config::load(&a).unwrap().store.as_deref(), store.to_str());
    fs::create_dir(&store).unwrap();
    assert_eq!(
        notes(None),
        [
            "local-history: on".to_string(),
            format!("store: {}", store.display())
        ]
    );
    // The settings take effect.
    cmd::snapshot(&a).unwrap();
    assert!(
        store.join(sha256(b"v1")).exists() && a.join(".syncit/blobs").join(sha256(b"v1")).exists()
    );
    assert_eq!(notes(Some(Setting::Store(None)))[1], "store: none");
    assert_eq!(notes(Some(Setting::LocalHistory(false))).len(), 3);
    let config = Config::load(&a).unwrap();
    assert!(!config.keep_local_history && config.store.is_none());
    assert_eq!(cmd::config(&a, None).unwrap().exit_code(), 0);
}

#[test]
fn nested_vault_is_ordinary_files() {
    // A vault dragged into another: its files sync as part of the outer one.
    let (tmp, a, b) = pair(&[("f.txt", "f"), ("inner/old.txt", "old")]);
    cmd::init(&a.join("inner")).unwrap();
    put(&a, "inner/new.txt", "new", 1);
    let report = cmd::sync(&b, &a).unwrap();
    assert_eq!((report.exit_code(), report.count("deleted")), (0, 0));
    assert!(is(&b, "inner/old.txt", "old") && is(&b, "inner/new.txt", "new"));
    assert!(!b.join("inner/.syncit").exists());
    // The inner vault still works on its own, and its changes reach the outer.
    assert_eq!(cmd::snapshot(&a.join("inner")).unwrap().recorded, 2);
    put(&b, "inner/new.txt", "edited", 2);
    cmd::sync(&b, &a).unwrap();
    assert!(is(&a, "inner/new.txt", "edited"));
    assert!(
        cmd::status(&a.join("inner"))
            .unwrap()
            .has("modified", "new.txt")
    );
    // Vaults that share files do not sync with each other.
    assert!(cmd::sync(&a, &a.join("inner")).is_err());
    assert!(cmd::pull(&a.join("inner"), &a).is_err());
    assert!(cmd::clone(&a, &a.join("copy")).is_err());
    assert!(!a.join("copy").exists());
    assert!(cmd::clone(&a, &tmp.path().join("c")).is_ok());
}

#[test]
fn version_mismatch() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    configure(&a, |c| c.syncit = 2);
    put(&a, "f.txt", "v2", 1);
    for result in [
        cmd::sync(&b, &a),
        cmd::push(&b, &a),
        cmd::pull(&b, &a),
        cmd::snapshot(&a),
    ] {
        let error = result
            .expect_err("a version 2 vault is refused")
            .to_string();
        assert!(error.contains("version 2"), "{error}");
    }
    assert!(is(&b, "f.txt", "v1"));
    assert!(
        cmd::sync(&b, &b).is_err(),
        "a vault does not sync with itself"
    );
}
