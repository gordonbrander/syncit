//! Integration tests: vaults in temporary directories, driven through the
//! library's command functions. Names follow SPEC.md section 14.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};
use syncit::commands::{self as cmd, Setting};
use syncit::output::{Context, Event, State, Summary};

static O: LazyLock<Context> = LazyLock::new(|| Context {
    json: false,
    dry_run: false,
    no_remove: false,
    secret_key: None,
    bar: syncit::output::bar(false),
    config_dir: None,
});
use std::sync::LazyLock;
use syncit::config::Config;
use syncit::dirstate::Dirstate;
use syncit::entry::sha256;
use syncit::log::{Log, PathState};
use syncit::materialise::{copy_name, materialise};
use syncit::scan::scan;
use syncit::storage::Dir;
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

fn chmod(dir: &Path, path: &str, mode: u32) {
    fs::set_permissions(dir.join(path), fs::Permissions::from_mode(mode)).unwrap();
}

/// Whether the file is executable; None if it is not there.
fn exec(dir: &Path, path: &str) -> Option<bool> {
    let meta = dir.join(path).metadata().ok()?;
    Some(meta.permissions().mode() & 0o111 != 0)
}

fn read(dir: &Path, path: &str) -> Option<String> {
    fs::read_to_string(dir.join(path)).ok()
}

fn is(dir: &Path, path: &str, content: &str) -> bool {
    read(dir, path).as_deref() == Some(content)
}

/// A temporary folder, and inside it a vault `a` with `files`.
fn vault(files: &[(&str, &str)]) -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path().join("a");
    fs::create_dir(&a).unwrap();
    for (path, content) in files {
        put(&a, path, content, 0);
    }
    cmd::init(&O, &a).unwrap();
    (tmp, a)
}

/// Vault `a` with `files`, and `b` cloned from it.
fn pair(files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf) {
    let (tmp, a) = vault(files);
    let b = tmp.path().join("b");
    assert_eq!(
        cmd::clone(&O, Path::new("/"), &a, &b, &[], false)
            .unwrap()
            .exit_code(),
        0
    );
    (tmp, a, b)
}

/// What `status` lists for the vault at `dir`: the lines that need something.
fn status(dir: &Path) -> Vec<State> {
    cmd::status(&O, dir, &[], None, false).unwrap()
}

fn has(found: &[State], state: &str, path: &str) -> bool {
    found.iter().any(|h| h.ours == state && h.path == path)
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

fn log(dir: &Path) -> Log {
    Log::load(&O, &Dir::new(dir), true).unwrap()
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

/// Make each vault one that keeps no history and has none: what a new vault
/// was before `keep_history` had a default.
fn no_history(dirs: &[&Path]) {
    for dir in dirs {
        configure(dir, |c| c.keep_history = Some(0));
        fs::remove_dir_all(dir.join(".syncit/blobs")).unwrap();
        fs::create_dir(dir.join(".syncit/blobs")).unwrap();
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
    let report = cmd::push(&O, &b, &a).unwrap();
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
    let report = cmd::push(&O, &b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    assert!(is(&a, "new/deep/n.txt", "n"));
    assert!(
        !a.join("dir").exists(),
        "the deletion propagates, and the empty folder goes"
    );
    assert!(!b.join("only-a.txt").exists());
    cmd::pull(&O, &b, &a).unwrap();
    assert!(is(&b, "only-a.txt", "a"));
}

#[test]
fn story_a3_move() {
    let (_tmp, a, b) = pair(&[("x/1.txt", "one"), ("y/2.txt", "two"), ("keep.txt", "k")]);
    let report = cmd::move_to(&O, &b, &paths(&["x", "y/2.txt"]), &a).unwrap();
    assert!(report.has("moved", "x/1.txt") && report.has("moved", "y/2.txt"));
    assert_eq!(report.exit_code(), 0);
    assert!(!b.join("x").exists() && !b.join("y").exists() && is(&b, "keep.txt", "k"));
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.recorded), (0, 0));
    assert!(is(&a, "x/1.txt", "one") && is(&a, "y/2.txt", "two"));
    assert!(!b.join("x").exists() && !b.join("y").exists());
    cmd::get(&O, &b, &[a.join("x"), a.join("y/2.txt")]).unwrap();
    assert!(is(&b, "x/1.txt", "one") && is(&b, "y/2.txt", "two"));
    assert_eq!(tree(&a), tree(&b));
}

#[test]
fn renames_are_shown() {
    let (_tmp, a, b) = pair(&[
        ("notes/x.txt", "x"),
        ("notes/y.txt", "y"),
        ("z.txt", "z"),
        ("gone.txt", "g"),
    ]);
    fs::rename(b.join("notes"), b.join("docs")).unwrap();
    fs::rename(b.join("z.txt"), b.join("a.txt")).unwrap();
    fs::remove_file(b.join("gone.txt")).unwrap();
    put(&b, "c.txt", "c", 1);
    let shown: Vec<_> = status(&b).iter().map(|s| (s.ours, s.shown())).collect();
    assert_eq!(
        shown,
        [
            ("renamed", "z.txt -> a.txt".into()),
            ("new", "c.txt".into()),
            ("renamed", "notes/ -> docs/".into()),
            ("removed", "gone.txt".into()),
        ]
    );
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    let counts = (report.renamed, report.added, report.deleted);
    assert_eq!(counts, (3, 1, 1));
    let renamed = |from: &str, to: &str| {
        let is = |e: &&Event| e.kind == "renamed" && e.path == to;
        report
            .events
            .iter()
            .find(is)
            .and_then(|e| e.from.as_deref())
            == Some(from)
    };
    assert!(renamed("z.txt", "a.txt") && renamed("notes/x.txt", "docs/x.txt"));
    assert_eq!(tree(&a), tree(&b));
}

/// A file renamed in another vault is written from the old file here before
/// that is removed, so it needs no other source: here, none is given.
#[test]
fn rename_needs_no_source() {
    let (_tmp, a, b) = pair(&[("a.txt", "content")]);
    no_history(&[&a, &b]);
    // `a.txt` sorts first, so it would be removed before `b.txt` is written.
    fs::rename(b.join("a.txt"), b.join("b.txt")).unwrap();
    cmd::snapshot(&O, &b).unwrap();
    let mut report = Summary::default();
    let (mut va, vb) = (
        Vault::open(&O, &a, true).unwrap(),
        Vault::open(&O, &b, true).unwrap(),
    );
    cmd::receive(&O, &mut va, &vb, &mut report).unwrap();
    materialise(&O, &mut va, None, false, &mut report).unwrap();
    assert_eq!((report.exit_code(), report.renamed), (0, 1));
    assert!(is(&a, "b.txt", "content") && !a.join("a.txt").exists());
}

#[test]
fn story_a4_clone_subfolder() {
    let (tmp, a) = vault(&[
        ("foo/bar/x.txt", "x"),
        ("foo/z.txt", "z"),
        ("other/y.txt", "y"),
    ]);
    let b = tmp.path().join("b");
    cmd::clone(&O, Path::new("/"), &a.join("foo/bar"), &b, &[], false).unwrap();
    assert_eq!(tree(&b).keys().collect::<Vec<_>>(), ["foo/bar/x.txt"]);
    put(&b, "foo/bar/x.txt", "x2", 1);
    put(&b, "foo/bar/new.txt", "n", 1);
    let report = cmd::sync(&O, &b, &a).unwrap();
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
    cmd::clone(&O, Path::new("/"), &a.join("foo/bar"), &b, &[], false).unwrap();
    cmd::move_to(&O, &b, &paths(&["foo/bar/big"]), &a).unwrap();
    // A second folder.
    cmd::get(&O, &b, &[a.join("other")]).unwrap();
    assert!(is(&b, "other/y.txt", "y") && !b.join("foo/bar/big").exists());
    // New files outside the included paths: the three examples of section 7.
    put(&b, "zap/x/y.txt", "new", 1);
    put(&b, "zap/x/z.txt", "new too", 1);
    put(&b, "foo/new.txt", "n", 1);
    put(&b, "foo/bar/big/n/a.txt", "a", 1);
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().exit_code(), 0);
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
    cmd::sync(&O, &b, &a).unwrap();
    assert!(is(&b, "zap/later.txt", "later"));
    assert!(!b.join("foo/elsewhere.txt").exists() && !b.join("foo/bar/big/f.bin").exists());
    assert!(is(&a, "foo/bar/big/f.bin", "f"));
}

#[test]
fn story_b1_conflict() {
    let (_tmp, a, b) = pair(&[("notes.txt", "base")]);
    put(&a, "notes.txt", "from a", 1);
    put(&b, "notes.txt", "from b", 2);
    let report = cmd::sync(&O, &b, &a).unwrap();
    let copy = copy_of("notes.txt", "from a");
    assert_eq!(report.exit_code(), 2);
    assert_eq!(report.conflicts, 1);
    for dir in [&a, &b] {
        assert!(is(dir, "notes.txt", "from b") && is(dir, &copy, "from a"));
    }
    assert_eq!(tree(&a), tree(&b));
    // It repeats until it is closed, and status shows it without failing.
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().conflicts, 1);
    let open = status(&a);
    assert!(has(&open, "conflict", "notes.txt"));
    assert_eq!(open[0].conflict.as_ref(), Some(&copy));
    // Editing the winner where both versions are visible closes it everywhere.
    put(&a, "notes.txt", "merged", 3);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (0, 0));
    assert!(is(&a, "notes.txt", "merged") && is(&b, "notes.txt", "merged"));
    assert_eq!(tree(&a), tree(&b));
    assert!(status(&b).is_empty());
}

/// A pair with `files` where `b` keeps history and has the files' first
/// versions in it: what a later merge starts from.
fn pair_with_history(files: &[(&str, &str)]) -> (TempDir, PathBuf, PathBuf) {
    let (tmp, a, b) = pair(files);
    configure(&b, |c| c.keep_history = None);
    cmd::snapshot(&O, &b).unwrap();
    (tmp, a, b)
}

#[test]
fn merge_text_conflict() {
    let (_tmp, a, b) = pair_with_history(&[("notes.md", "one\ntwo\nthree\n")]);
    put(&a, "notes.md", "ONE\ntwo\nthree\n", 1);
    put(&b, "notes.md", "one\ntwo\nTHREE\n", 2);
    let report = cmd::sync(&O, &a, &b).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (0, 0), "{report:?}");
    assert_eq!(report.updated, 2, "merged in each vault");
    assert!(report.events.iter().all(|e| e.note == "merged"));
    for dir in [&a, &b] {
        assert!(is(dir, "notes.md", "ONE\ntwo\nTHREE\n"));
        assert_eq!(tree(dir).len(), 1, "no conflict copy");
    }
    // The merge is one more entry, a child of both edits: they stay in the
    // log. Both vaults wrote the same one.
    for dir in [&a, &b] {
        let log = log(dir);
        assert_eq!(log.paths["notes.md"].len(), 4);
        let PathState::Resolved { heads, losers, .. } = log.resolve("notes.md") else {
            panic!("not resolved");
        };
        assert!(heads.len() == 1 && losers.is_empty());
        assert_eq!(log.entries[&heads[0]].parents.len(), 2);
    }
    // Nothing is left to do, and the versions merged can be brought back.
    assert_eq!(cmd::sync(&O, &a, &b).unwrap(), Summary::default());
    assert!(status(&a).is_empty() && status(&b).is_empty());
    let old = sha256(b"ONE\ntwo\nthree\n");
    cmd::restore(&O, &b, Path::new("notes.md"), &old, false).unwrap();
    assert!(is(&b, &copy_name("notes.md", &old), "ONE\ntwo\nthree\n"));
}

/// One vault receiving is enough, if it is the one that keeps history.
#[test]
fn merge_on_pull() {
    let (_tmp, a, b) = pair_with_history(&[("notes.md", "one\ntwo\nthree\n")]);
    put(&a, "notes.md", "ONE\ntwo\nthree\n", 1);
    put(&b, "notes.md", "one\ntwo\nTHREE\n", 2);
    let report = cmd::pull(&O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.updated), (0, 1), "{report:?}");
    assert!(is(&b, "notes.md", "ONE\ntwo\nTHREE\n"));
    assert!(
        is(&a, "notes.md", "ONE\ntwo\nthree\n"),
        "a pull changes one vault"
    );
    // Both versions it replaced are in the history of the vault that merged.
    for old in ["ONE\ntwo\nthree\n", "one\ntwo\nTHREE\n"] {
        assert!(blob_path(&b, &sha256(old.as_bytes())).is_file());
    }
}

#[test]
fn merge_falls_back() {
    let conflict = |a: &Path, b: &Path, path: &str, ours: &str, theirs: &str| {
        put(a, path, ours, 1);
        put(b, path, theirs, 2);
        let report = cmd::sync(&O, a, b).unwrap();
        assert_eq!((report.exit_code(), report.conflicts), (2, 1), "{path}");
        for dir in [a, b] {
            assert!(is(dir, path, theirs) && is(dir, &copy_of(path, ours), ours));
        }
    };
    let text = "one\ntwo\nthree\n";
    // Edits to the same line.
    let (_tmp, a, b) = pair_with_history(&[("notes.md", text)]);
    conflict(&a, &b, "notes.md", "ONE\ntwo\nthree\n", "uno\ntwo\nthree\n");
    // Not a text file.
    let (_tmp, a, b) = pair_with_history(&[("data.json", text)]);
    conflict(
        &a,
        &b,
        "data.json",
        "ONE\ntwo\nthree\n",
        "one\ntwo\nTHREE\n",
    );
    // No vault keeps history, so the versions replaced would be lost: though
    // the one both edits started from can be read, from another file.
    let (_tmp, a, b) = pair(&[("notes.md", text), ("spare.md", text)]);
    no_history(&[&a, &b]);
    conflict(&a, &b, "notes.md", "ONE\ntwo\nthree\n", "one\ntwo\nTHREE\n");
    // Only the vault that is not receiving keeps history.
    let (_tmp, a, b) = pair_with_history(&[("notes.md", text)]);
    no_history(&[&a]);
    put(&a, "notes.md", "ONE\ntwo\nthree\n", 1);
    put(&b, "notes.md", "one\ntwo\nTHREE\n", 2);
    let report = cmd::pull(&O, &a, &b).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("notes.md", "ONE\ntwo\nthree\n");
    assert!(is(&a, "notes.md", "one\ntwo\nTHREE\n") && a.join(copy).is_file());
    // An edit against a deletion.
    let (_tmp, a, b) = pair_with_history(&[("notes.md", text)]);
    put(&a, "notes.md", "ONE\ntwo\nthree\n", 1);
    fs::remove_file(b.join("notes.md")).unwrap();
    let report = cmd::sync(&O, &a, &b).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    assert!(is(&b, "notes.md", "ONE\ntwo\nthree\n"));
}

#[test]
fn story_c1_three_vaults() {
    let (tmp, a, b) = pair(&[("f.txt", "v1")]);
    let c = tmp.path().join("c");
    cmd::clone(&O, Path::new("/"), &b, &c, &[], false).unwrap();
    // A change relays through b between two vaults that never meet.
    put(&a, "f.txt", "from a", 1);
    put(&c, "new.txt", "from c", 2);
    cmd::sync(&O, &a, &b).unwrap();
    cmd::sync(&O, &b, &c).unwrap();
    cmd::sync(&O, &a, &b).unwrap();
    assert!(is(&c, "f.txt", "from a") && is(&a, "new.txt", "from c"));
    // Any pairing, any order, including a conflict.
    put(&b, "g.txt", "from b", 3);
    put(&a, "f.txt", "a again", 4);
    put(&c, "f.txt", "c again", 5);
    fs::remove_file(a.join("new.txt")).unwrap();
    cmd::sync(&O, &c, &a).unwrap();
    cmd::push(&O, &b, &c).unwrap();
    cmd::sync(&O, &b, &a).unwrap();
    cmd::sync(&O, &c, &b).unwrap();
    cmd::sync(&O, &a, &c).unwrap();
    cmd::sync(&O, &b, &a).unwrap();
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
    no_history(&[&a, &b]);
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&O, &b, &a).unwrap();
    put(&b, "f.txt", "v3", 2);
    put(&a, "f.txt", "v3 conflicting", 3);
    cmd::sync(&O, &b, &a).unwrap();
    cmd::snapshot(&O, &a).unwrap();
    for dir in [&a, &b] {
        assert!(blobs(dir).is_empty());
        assert!(names(&dir.join(".syncit/tmp")).is_empty());
    }
}

#[test]
fn story_d2_history() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    no_history(&[&a]);
    configure(&b, |c| c.keep_history = None);
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&O, &b, &a).unwrap();
    put(&b, "f.txt", "v3", 2);
    cmd::sync(&O, &b, &a).unwrap();
    let kept: Vec<String> = ["v1", "v2", "v3"]
        .iter()
        .map(|v| sha256(v.as_bytes()))
        .collect();
    assert_eq!(blobs(&b), kept.iter().cloned().collect());
    assert!(blobs(&a).is_empty());
    assert_eq!(fs::read_to_string(blob_path(&b, &kept[0])).unwrap(), "v1");

    // Kept versions are a blob source: one that is nowhere else comes back.
    fs::remove_file(a.join("f.txt")).unwrap();
    fs::remove_file(b.join("f.txt")).unwrap();
    fs::remove_file(a.join(".syncit/dirstate")).unwrap();
    fs::remove_file(b.join(".syncit/dirstate")).unwrap();
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&a, "f.txt", "v3") && is(&b, "f.txt", "v3"));
    // Set to keep nothing, nothing more is collected, and the next run drops
    // the versions that are no longer current.
    cmd::config(&O, &b, None, Some(Setting::KeepHistory(Some(0)))).unwrap();
    put(&b, "f.txt", "v4", 3);
    cmd::sync(&O, &b, &a).unwrap();
    assert!(blobs(&b).is_empty());
}

/// The hashes stored in a vault's `.syncit/blobs/`, in its `ab/abcdef…` folders.
fn blobs(dir: &Path) -> BTreeSet<String> {
    let root = dir.join(".syncit/blobs");
    names(&root)
        .iter()
        .flat_map(|prefix| names(&root.join(prefix)))
        .collect()
}

fn blob_path(dir: &Path, hash: &str) -> PathBuf {
    dir.join(".syncit/blobs").join(&hash[..2]).join(hash)
}

fn hashes(contents: &[&str]) -> BTreeSet<String> {
    contents.iter().map(|c| sha256(c.as_bytes())).collect()
}

#[test]
fn history_depth() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("gone.txt", "g")]);
    configure(&b, |c| c.keep_history = None);
    let edit = |content: &str, t: u64| {
        put(&a, "f.txt", content, t);
        cmd::snapshot(&O, &a).unwrap();
    };
    // A new vault keeps one version back from the current one.
    edit("v2", 1);
    assert_eq!(blobs(&a), hashes(&["v1", "v2", "g"]));
    edit("v3", 2);
    assert_eq!(blobs(&a), hashes(&["v2", "v3", "g"]));
    // Two back.
    configure(&a, |c| c.keep_history = Some(2));
    edit("v4", 3);
    assert_eq!(blobs(&a), hashes(&["v2", "v3", "v4", "g"]));
    // The log is whole: every version is listed, the oldest without content.
    assert_eq!(log(&a).entries.len(), 5);
    let seen = cmd::log(&O, &a, &paths(&["f.txt"])).unwrap();
    let available: Vec<bool> = seen.iter().map(|v| v.available).collect();
    assert_eq!(available, [true, true, true, false]);
    // A vault that keeps everything collects what this one still has, and
    // this one does not collect again what it dropped.
    cmd::sync(&O, &a, &b).unwrap();
    assert_eq!(blobs(&a), hashes(&["v2", "v3", "v4", "g"]));
    assert_eq!(blobs(&b), hashes(&["v1", "v2", "v3", "v4", "g"]));
    cmd::sync(&O, &a, &b).unwrap();
    assert_eq!(blobs(&a), hashes(&["v2", "v3", "v4", "g"]));
    // A deletion is a version: the file's last content is one step back.
    fs::remove_file(a.join("gone.txt")).unwrap();
    cmd::snapshot(&O, &a).unwrap();
    assert!(blobs(&a).contains(&sha256(b"g")));
    cmd::restore(&O, &a, Path::new("gone.txt"), &sha256(b"g")[..8], true).unwrap();
    assert!(is(&a, "gone.txt", "g"));
}

#[test]
fn compact_command() {
    let dry = &Context {
        json: false,
        dry_run: true,
        no_remove: false,
        secret_key: None,
        bar: syncit::output::bar(false),
        config_dir: None,
    };
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("c.bin", "c1"), ("d.txt", "d")]);
    configure(&a, |c| c.autocompact = false);
    let edit = |path: &str, content: &str, t: u64| {
        put(&a, path, content, t);
        cmd::snapshot(&O, &a).unwrap();
    };
    // Without autocompact, versions older than the setting keeps pile up.
    edit("f.txt", "v2", 1);
    edit("f.txt", "v3", 2);
    assert_eq!(blobs(&a), hashes(&["v1", "v2", "v3", "c1", "d"]));
    // A dry run says what would go and drops nothing.
    let entries = log(&a).entries;
    assert_eq!(cmd::compact(dry, &a, None).unwrap(), (1, 2));
    assert_eq!(blobs(&a).len(), 5);
    // `compact` drops them: content only, the log is as it was.
    assert_eq!(cmd::compact(&O, &a, None).unwrap(), (1, 2));
    assert_eq!(blobs(&a), hashes(&["v2", "v3", "c1", "d"]));
    assert_eq!(log(&a).entries, entries);
    assert_eq!(cmd::compact(&O, &a, None).unwrap(), (0, 0));
    // A path in conflict keeps every version until the conflict is closed.
    edit("c.bin", "c2", 3);
    edit("c.bin", "c3", 4);
    put(&b, "c.bin", "theirs", 5);
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().conflicts, 1);
    assert_eq!(cmd::compact(&O, &a, None).unwrap(), (0, 0));
    assert!(blobs(&a).is_superset(&hashes(&["c1", "c2", "c3", "theirs"])));
    // Set to keep nothing back, the current versions stay: among them a file
    // dropped from this vault, whose only copy may be here.
    cmd::drop(&O, &a, &paths(&["d.txt"]), None, false).unwrap();
    cmd::config(&O, &a, None, Some(Setting::KeepHistory(Some(0)))).unwrap();
    cmd::compact(&O, &a, None).unwrap();
    let left = blobs(&a);
    assert!(!a.join("d.txt").exists() && left.is_superset(&hashes(&["d", "v3"])));
    assert!(!left.contains(&sha256(b"v2")));
    // With autocompact on, a run does the same by itself.
    cmd::config(&O, &a, None, Some(Setting::KeepHistory(Some(1)))).unwrap();
    edit("f.txt", "v4", 6);
    edit("f.txt", "v5", 7);
    assert!(blobs(&a).is_superset(&hashes(&["v3", "v4", "v5"])));
    cmd::config(&O, &a, None, Some(Setting::Autocompact(true))).unwrap();
    cmd::snapshot(&O, &a).unwrap();
    assert!(blobs(&a).is_superset(&hashes(&["v4", "v5"])));
    assert!(!blobs(&a).contains(&sha256(b"v3")));
}

#[test]
fn story_d3_snapshot() {
    let (tmp, a) = vault(&[("f.txt", "v1")]);
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 1);
    put(&a, "f.txt", "v2", 1);
    let report = cmd::snapshot(&O, &a).unwrap();
    assert_eq!((report.recorded, report.exit_code()), (1, 0));
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 0);
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
    cmd::clone(&O, Path::new("/"), &a, &b, &[], false).unwrap();
    assert_eq!(log(&b).entries, entries);
    assert!(is(&b, "f.txt", "v2"));
}

#[test]
fn story_e1_rerun() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    put(&a, "f.txt", "v2", 1);
    put(&b, "d/h.txt", "h", 1);
    let first = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((first.recorded, first.exit_code()), (2, 0));
    assert_eq!(
        first.to_string(),
        "recorded 2, added 1, updated 1, deleted 0, renamed 0, conflicts 0, pending 0, skipped 0"
    );
    let (segments, files) = (names(&a.join(".syncit/log")), tree(&a));
    let second = cmd::sync(&O, &b, &a).unwrap();
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
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert!(is(&a, "f.txt", "edited") && is(&b, "f.txt", "edited"));
    assert_eq!(report.exit_code(), 2);
    assert_eq!(report.conflicts, 1);
    // The next edit closes it.
    put(&b, "f.txt", "again", 2);
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().exit_code(), 0);
    assert!(is(&a, "f.txt", "again"));
}

#[test]
fn no_remove_keeps_deleted() {
    let keep = |dry_run| Context {
        json: false,
        dry_run,
        no_remove: true,
        secret_key: None,
        bar: syncit::output::bar(false),
        config_dir: None,
    };
    let files = [
        ("f.txt", "f"),
        ("g.txt", "g"),
        ("h.txt", "h"),
        ("k.txt", "k"),
    ];
    let (_tmp, a, b) = pair(&files);

    // Deleted in one vault: it stays in the other and comes back in the first.
    fs::remove_file(a.join("f.txt")).unwrap();
    let before = (tree(&a), tree(&b));
    let planned = cmd::sync(&keep(true), &a, &b).unwrap();
    assert!(before == (tree(&a), tree(&b)), "a dry run wrote something");
    let done = cmd::sync(&keep(false), &a, &b).unwrap();
    assert_eq!(planned, done);
    assert_eq!((done.deleted, done.added, done.exit_code()), (0, 2, 0));
    let kept = |e: &&Event| e.kind == "added" && e.path == "f.txt";
    let notes: Vec<_> = done
        .events
        .iter()
        .filter(kept)
        .map(|e| e.note.as_str())
        .collect();
    assert_eq!(notes, ["kept", ""], "kept in b, written in a");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "f") && replaced_deletion(dir, "f.txt"));
    }
    let again = cmd::sync(&O, &a, &b).unwrap();
    assert_eq!((again.recorded, again.added, again.deleted), (0, 0, 0));

    // A push writes nothing here: the file comes back at the next sync.
    fs::remove_file(a.join("g.txt")).unwrap();
    let pushed = cmd::push(&keep(false), &a, &b).unwrap();
    assert!(pushed.has("added", "g.txt") && pushed.deleted == 0);
    assert!(is(&b, "g.txt", "g") && !a.join("g.txt").exists());
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().deleted, 0);
    assert!(is(&a, "g.txt", "g") && replaced_deletion(&a, "g.txt"));

    // Deleted in both: nothing to keep.
    for dir in [&a, &b] {
        fs::remove_file(dir.join("h.txt")).unwrap();
    }
    let both = cmd::sync(&keep(false), &a, &b).unwrap();
    assert!(!both.has("added", "h.txt") && both.exit_code() == 0);
    assert!(!a.join("h.txt").exists() && !b.join("h.txt").exists());
}

/// One head at `path`, a file, whose parents are all tombstones; and the
/// vault's dirstate has it as a file, not as a deletion.
fn replaced_deletion(dir: &Path, path: &str) -> bool {
    let (log, dirstate) = (log(dir), Dirstate::load(dir).unwrap());
    let PathState::Resolved { heads, .. } = log.resolve(path) else {
        return false;
    };
    let head = &log.entries[&heads[0]];
    heads.len() == 1
        && head.hash.is_some()
        && !head.parents.is_empty()
        && head.parents.iter().all(|p| log.entries[p].hash.is_none())
        && dirstate.rows.contains_key(path)
        && !dirstate.deleted.contains_key(path)
}

#[test]
fn recreate_after_delete() {
    // Re-created where it was deleted, with an old version's bytes and with
    // new ones: an ordinary new version, not an old copy and not a conflict.
    let (_tmp, a, b) = pair(&[("same.txt", "one"), ("diff.txt", "one"), ("k.txt", "k")]);
    for path in ["same.txt", "diff.txt"] {
        fs::remove_file(a.join(path)).unwrap();
    }
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().deleted, 2);
    for dir in [&a, &b] {
        let seen = Dirstate::load(dir).unwrap().deleted;
        assert_eq!(seen.len(), 2, "both vaults record the deletions");
    }
    put(&a, "same.txt", "one", 1);
    put(&a, "diff.txt", "two", 1);
    for _ in 0..2 {
        let report = cmd::sync(&O, &a, &b).unwrap();
        assert_eq!((report.conflicts, report.exit_code()), (0, 0));
    }
    for dir in [&a, &b] {
        assert!(is(dir, "same.txt", "one") && is(dir, "diff.txt", "two"));
        assert!(replaced_deletion(dir, "same.txt") && replaced_deletion(dir, "diff.txt"));
    }

    // The same in the vault that received the deletion.
    fs::remove_file(a.join("same.txt")).unwrap();
    cmd::sync(&O, &a, &b).unwrap();
    put(&b, "same.txt", "one", 2);
    for _ in 0..2 {
        let report = cmd::sync(&O, &b, &a).unwrap();
        assert_eq!((report.conflicts, report.exit_code()), (0, 0));
    }
    assert!(is(&a, "same.txt", "one") && replaced_deletion(&b, "same.txt"));

    // With the dirstate lost there is no record: the file is kept all the same.
    fs::remove_file(a.join("same.txt")).unwrap();
    cmd::sync(&O, &a, &b).unwrap();
    fs::remove_file(a.join(".syncit/dirstate")).unwrap();
    put(&a, "same.txt", "one", 3);
    for _ in 0..2 {
        let report = cmd::sync(&O, &a, &b).unwrap();
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
    cmd::clone(&O, Path::new("/"), &a.join("sub"), &c, &[], false).unwrap();
    fs::remove_file(b.join("f.txt")).unwrap();
    cmd::sync(&O, &b, &a).unwrap();
    put(&a, "f.txt", "other", 1);
    cmd::sync(&O, &c, &a).unwrap();
    let report = cmd::sync(&O, &b, &c).unwrap();
    assert_eq!(report.pending, 1);
    put(&b, "f.txt", "v1", 2);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!(report.conflicts, 1);
    let copy = copy_of("f.txt", "other");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "v1") && is(dir, &copy, "other"));
    }
}

#[test]
fn log_lists_versions() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    no_history(&[&a, &b]);
    cmd::config(&O, &b, None, Some(Setting::Nickname("other".into()))).unwrap();
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&O, &a, &b).unwrap();
    put(&b, "f.txt", "v3", 2);
    cmd::sync(&O, &a, &b).unwrap();
    let seen = cmd::log(&O, &a, &paths(&["f.txt"])).unwrap();
    let line = |i: usize| {
        let v: &syncit::output::Version = &seen[i];
        let names = v.nicknames.join(",");
        (v.hash.clone(), names, v.current, v.available)
    };
    // Newest first. Without history, only what is on disk is available.
    let hash = |content: &str| Some(sha256(content.as_bytes()));
    assert_eq!(seen.len(), 3);
    assert_eq!(line(0), (hash("v3"), "other".into(), true, true));
    assert_eq!(line(1), (hash("v2"), "a".into(), false, false));
    assert_eq!(line(2), (hash("v1"), "a".into(), false, false));
    // A deletion is listed, and the path is still found though no file is there.
    fs::remove_file(a.join("f.txt")).unwrap();
    cmd::snapshot(&O, &a).unwrap();
    let seen = cmd::log(&O, &a.join("d"), &paths(&["../f.txt"])).unwrap();
    assert_eq!(seen.len(), 4);
    assert_eq!((&seen[0].hash, seen[0].current), (&None, true));
    assert!(!seen[1].current && !seen[1].available);
    // No paths: every file. With history, old versions are available.
    assert_eq!(cmd::log(&O, &a, &[]).unwrap().len(), 5);
    // Kept from the first run after it is turned on.
    cmd::config(&O, &b, None, Some(Setting::KeepHistory(None))).unwrap();
    cmd::snapshot(&O, &b).unwrap();
    put(&b, "d/g.txt", "g2", 3);
    cmd::snapshot(&O, &b).unwrap();
    put(&b, "d/g.txt", "g3", 4);
    cmd::snapshot(&O, &b).unwrap();
    let seen = cmd::log(&O, &b, &paths(&["d"])).unwrap();
    let available: Vec<bool> = seen.iter().map(|v| v.available).collect();
    assert_eq!(available, [true, true, true]);
}

#[test]
fn restore_version() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("gone.txt", "g1"), ("k.txt", "k")]);
    no_history(&[&a, &b]);
    let hash = |content: &str| sha256(content.as_bytes());
    let restore = |dir: &Path, path: &str, content: &str, in_place| {
        cmd::restore(&O, dir, Path::new(path), &hash(content)[..8], in_place)
    };
    // Without history an old version cannot be brought back.
    put(&a, "f.txt", "v2", 1);
    cmd::snapshot(&O, &a).unwrap();
    let error = restore(&a, "f.txt", "v1", false).unwrap_err().to_string();
    assert!(error.contains("keep-history"), "{error}");
    // The other vault still has v1 on disk; history collects it from there.
    cmd::config(&O, &a, None, Some(Setting::KeepHistory(None))).unwrap();
    cmd::sync(&O, &a, &b).unwrap();

    // A copy: beside the file, which is left alone; an ordinary new file.
    let copy = copy_of("f.txt", "v1");
    assert_eq!(restore(&a, "f.txt", "v1", false).unwrap(), copy);
    assert!(is(&a, "f.txt", "v2") && is(&a, &copy, "v1"));
    assert_eq!(restore(&a, "f.txt", "v1", false).unwrap(), copy, "again");
    assert!(has(&status(&a), "new", &copy));
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&b, &copy, "v1"));
    // A copy deleted since the last scan can be had again.
    fs::remove_file(a.join(&copy)).unwrap();
    assert_eq!(restore(&a, "f.txt", "v1", false).unwrap(), copy);
    assert!(status(&a).is_empty(), "the same file as before");
    // The copy moved over the file is an edit like any other.
    fs::rename(a.join(&copy), a.join("f.txt")).unwrap();
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().exit_code(), 0);
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
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().exit_code(), 0);
    assert!(is(&b, "f.txt", "v2"));

    // In place, for a deleted file: it stays, here and in the other vault.
    fs::remove_file(a.join("gone.txt")).unwrap();
    assert_eq!(cmd::sync(&O, &a, &b).unwrap().deleted, 1);
    assert_eq!(restore(&a, "gone.txt", "g1", true).unwrap(), "gone.txt");
    for _ in 0..2 {
        let report = cmd::sync(&O, &a, &b).unwrap();
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
        cmd::restore(&O, &a, Path::new("f.txt"), "", false),
        "no version",
    );
    put(&a, &copy, "something else", 3);
    refused(restore(&a, "f.txt", "v1", false), "is taken");
    // c has only k.txt: f.txt is not on disk there.
    let c = a.parent().unwrap().join("c");
    cmd::clone(&O, Path::new("/"), &a.join("k.txt"), &c, &[], false).unwrap();
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
    let report = cmd::sync(&O, &b, &a).unwrap();
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
    cmd::clone(&O, Path::new("/"), &a.join("sub"), &b, &[], false).unwrap();
    assert!(!b.join("f.txt").exists());
    // Equal content is adopted silently.
    put(&b, "f.txt", "F", 5);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!(report, Summary::default());
    // Different content is an ordinary conflict.
    put(&b, "g.txt", "other", 5);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("g.txt", "G");
    for dir in [&a, &b] {
        assert!(is(dir, "g.txt", "other") && is(dir, &copy, "G"));
    }
    // Both are present from now on.
    put(&a, "f.txt", "F2", 6);
    cmd::sync(&O, &b, &a).unwrap();
    assert!(is(&b, "f.txt", "F2"));
}

#[test]
fn lost_dirstate() {
    let (_tmp, a, b) = pair(&[("f1.txt", "v1"), ("f2.txt", "two"), ("d/f3.txt", "three")]);
    // A single vault: nothing is recorded again.
    fs::remove_file(a.join(".syncit/dirstate")).unwrap();
    let report = cmd::snapshot(&O, &a).unwrap();
    assert_eq!(
        (report.recorded, names(&a.join(".syncit/log")).len()),
        (0, 1)
    );
    // With a peer: old files are brought forward, absent ones are not deletions.
    fs::remove_file(b.join("d/f3.txt")).unwrap();
    fs::remove_file(b.join(".syncit/dirstate")).unwrap();
    put(&a, "f1.txt", "v2", 1);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!(
        (report.recorded, report.deleted, report.exit_code()),
        (1, 0, 0)
    );
    assert_eq!(report.updated, 1);
    assert!(is(&b, "f1.txt", "v2") && is(&b, "f2.txt", "two"));
    assert!(is(&a, "d/f3.txt", "three") && is(&b, "d/f3.txt", "three"));
    // A known file with the other executable bit is a change to the version
    // the log has, not a second first version.
    fs::remove_file(a.join(".syncit/dirstate")).unwrap();
    chmod(&a, "f2.txt", 0o755);
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 1);
    let l = log(&a);
    let PathState::Resolved { heads, winner, .. } = l.resolve("f2.txt") else {
        panic!("f2.txt is not resolved");
    };
    let w = &l.entries[&winner];
    assert_eq!((heads.len(), w.exec, w.parents.len()), (1, true, 1));
}

#[test]
fn exec_bit_syncs() {
    let (_tmp, a, b) = pair(&[("run.sh", "echo")]);
    let mtime = |dir: &Path| dir.join("run.sh").metadata().unwrap().modified().unwrap();
    let written = mtime(&a);
    // The bit alone is a change, and it arrives without touching the file.
    chmod(&a, "run.sh", 0o755);
    assert!(has(&status(&a), "modified", "run.sh"));
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.recorded, report.updated), (1, 1));
    assert_eq!(report.exit_code(), 0);
    assert_eq!(exec(&b, "run.sh"), Some(true));
    assert!(is(&b, "run.sh", "echo") && mtime(&b) == written);
    let l = log(&a);
    let PathState::Resolved { heads, winner, .. } = l.resolve("run.sh") else {
        panic!("run.sh is not resolved");
    };
    let w = &l.entries[&winner];
    assert_eq!((heads.len(), w.exec, w.parents.len()), (1, true, 1));
    assert!(status(&a).is_empty() && status(&b).is_empty());
    // And back off again, from the other side.
    chmod(&b, "run.sh", 0o644);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.recorded, report.updated), (1, 1));
    assert_eq!(exec(&a, "run.sh"), Some(false));
    assert!(is(&a, "run.sh", "echo") && mtime(&a) == written);
    // A new file and an edited one are written with their bit.
    put(&a, "new.sh", "new", 1);
    chmod(&a, "new.sh", 0o700);
    put(&a, "run.sh", "echo 2", 2);
    chmod(&a, "run.sh", 0o755);
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().exit_code(), 0);
    assert_eq!(exec(&b, "new.sh"), Some(true));
    assert_eq!(exec(&b, "run.sh"), Some(true));
    assert!(is(&b, "run.sh", "echo 2"));
    // Nothing more to say.
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.recorded, report.updated), (0, 0));
}

#[test]
fn exec_change_vs_edit() {
    let (_tmp, a, b) = pair(&[("run.sh", "base")]);
    chmod(&a, "run.sh", 0o755);
    put(&b, "run.sh", "edited", 2);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.conflicts, report.exit_code()), (1, 2));
    // The edit wins; the executable version is the conflict copy, in both.
    let copy = copy_of("run.sh", "base");
    for dir in [&a, &b] {
        assert!(is(dir, "run.sh", "edited") && is(dir, &copy, "base"));
        assert_eq!(exec(dir, "run.sh"), Some(false));
        assert_eq!(exec(dir, &copy), Some(true));
    }
    // Both vaults record the copy as the same entry.
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().conflicts, 1);
    assert_eq!(log(&a).paths[&copy].len(), 1);
    assert!(log(&a).entries[&log(&a).paths[&copy].iter().next().unwrap().clone()].exec);
}

#[test]
fn restore_keeps_exec() {
    let (_tmp, a) = vault(&[("run.sh", "v1")]);
    configure(&a, |c| c.keep_history = None);
    chmod(&a, "run.sh", 0o755);
    cmd::snapshot(&O, &a).unwrap();
    put(&a, "run.sh", "v2", 1);
    chmod(&a, "run.sh", 0o644);
    cmd::snapshot(&O, &a).unwrap();
    let v1 = sha256(b"v1");
    let copy = cmd::restore(&O, &a, Path::new("run.sh"), &v1[..8], false).unwrap();
    assert!(is(&a, &copy, "v1"));
    assert_eq!(exec(&a, &copy), Some(true));
}

#[test]
fn copied_vault() {
    let (tmp, a) = vault(&[("f.txt", "v1"), ("same.txt", "s")]);
    cmd::snapshot(&O, &a).unwrap();
    let twin = tmp.path().join("twin");
    copy_dir(&a, &twin);
    put(&a, "f.txt", "one", 1);
    put(&twin, "f.txt", "two", 2);
    let report = cmd::sync(&O, &a, &twin).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("f.txt", "one");
    for dir in [&a, &twin] {
        assert!(is(dir, "f.txt", "two") && is(dir, &copy, "one") && is(dir, "same.txt", "s"));
    }
}

#[test]
fn restored_backup() {
    let (tmp, a) = vault(&[("f.txt", "v1")]);
    cmd::snapshot(&O, &a).unwrap();
    let (backup, b) = (tmp.path().join("backup"), tmp.path().join("b"));
    copy_dir(&a, &backup);
    cmd::clone(&O, Path::new("/"), &a, &b, &[], false).unwrap();
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&O, &a, &b).unwrap();
    put(&a, "f.txt", "v3", 2);
    cmd::sync(&O, &a, &b).unwrap();
    fs::remove_dir_all(&a).unwrap();
    copy_dir(&backup, &a);
    assert!(is(&a, "f.txt", "v1"));
    put(&a, "f.txt", "after restore", 3);
    let report = cmd::sync(&O, &a, &b).unwrap();
    assert_eq!((report.exit_code(), report.conflicts), (2, 1));
    let copy = copy_of("f.txt", "v3");
    for dir in [&a, &b] {
        assert!(is(dir, "f.txt", "after restore") && is(dir, &copy, "v3"));
    }
    // Restored and not edited: simply brought forward.
    fs::remove_dir_all(&a).unwrap();
    copy_dir(&backup, &a);
    let report = cmd::sync(&O, &a, &b).unwrap();
    assert_eq!(report.updated, 1);
    assert_eq!(tree(&a), tree(&b));
    assert_eq!(tree(&a).len(), 2);
}

#[test]
fn pending_then_edit() {
    let (tmp, a, b) = pair(&[("f.txt", "v1"), ("sub/s.txt", "s")]);
    let c = tmp.path().join("c");
    cmd::clone(&O, Path::new("/"), &a.join("sub"), &c, &[], false).unwrap();
    // c carries the news of v2 but not its content.
    put(&a, "f.txt", "v2", 1);
    cmd::sync(&O, &c, &a).unwrap();
    let report = cmd::sync(&O, &b, &c).unwrap();
    assert_eq!((report.pending, report.exit_code()), (1, 2));
    assert!(is(&b, "f.txt", "v1"));
    assert!(has(&status(&b), "behind", "f.txt"));
    // An edit made without having seen v2 conflicts with it.
    put(&b, "f.txt", "mine", 2);
    let report = cmd::sync(&O, &b, &a).unwrap();
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
            Vault::open(&O, &a, true).unwrap(),
            Vault::open(&O, &b, true).unwrap(),
        );
        scan(&O, &mut va, false).unwrap();
        scan(&O, &mut vb, false).unwrap();
        // Edits that land after the scan and before the files are replaced.
        put(&b, "f.txt", "local edit", 2);
        put(&b, "gone.txt", "kept", 2);
        let mut report = Summary::default();
        cmd::receive(&O, &mut vb, &va, &mut report).unwrap();
        materialise(&O, &mut vb, Some(&va), false, &mut report).unwrap();
        assert!(report.has("skipped", "f.txt") && report.has("skipped", "gone.txt"));
        assert_eq!(report.exit_code(), 2);
    }
    assert!(is(&b, "f.txt", "local edit") && is(&b, "gone.txt", "kept"));
    // The next run records them: a conflict, and an edit that beats the delete.
    let report = cmd::sync(&O, &b, &a).unwrap();
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
    cmd::init(&O, &b).unwrap();
    {
        let (mut va, mut vb) = (
            Vault::open(&O, &a, true).unwrap(),
            Vault::open(&O, &b, true).unwrap(),
        );
        scan(&O, &mut va, false).unwrap();
        scan(&O, &mut vb, false).unwrap();
        put(&b, "f.txt", "mine", 1);
        put(&b, "d", "a file where a folder is needed", 1);
        let mut report = Summary::default();
        cmd::receive(&O, &mut vb, &va, &mut report).unwrap();
        materialise(&O, &mut vb, Some(&va), false, &mut report).unwrap();
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
    cmd::init(&O, a).unwrap();
    assert_eq!(cmd::snapshot(&O, a).unwrap().recorded, 1);
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
    assert_eq!(cmd::snapshot(&O, a).unwrap().recorded, 1);
    assert_eq!(cmd::snapshot(&O, a).unwrap().recorded, 0);
}

#[test]
fn status_keeps_stat_cache() {
    let (_tmp, a) = vault(&[("f.txt", "one"), ("g.txt", "g")]);
    cmd::snapshot(&O, &a).unwrap();
    let before = Dirstate::load(&a).unwrap();
    // The same content with another mtime: hashed, and found the same.
    put(&a, "f.txt", "one", 5);
    std::thread::sleep(Duration::from_millis(5));
    assert!(status(&a).is_empty());
    let after = Dirstate::load(&a).unwrap();
    assert_eq!(after.rows["f.txt"].mtime, 1_700_000_005_000);
    assert_eq!(after.rows["f.txt"].base, before.rows["f.txt"].base);
    assert_eq!(after.rows["g.txt"], before.rows["g.txt"]);
    assert!(after.scanned > before.scanned);
    // Nothing more to learn, so nothing is written; and nothing to record.
    let saved = fs::read(a.join(".syncit/dirstate")).unwrap();
    assert!(status(&a).is_empty());
    assert_eq!(fs::read(a.join(".syncit/dirstate")).unwrap(), saved);
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 0);
    // A new file and a removed one are not the cache's business.
    put(&a, "new.txt", "n", 0);
    fs::remove_file(a.join("g.txt")).unwrap();
    put(&a, "f.txt", "one", 6);
    status(&a);
    let after = Dirstate::load(&a).unwrap();
    assert!(after.rows.contains_key("g.txt") && !after.rows.contains_key("new.txt"));
    assert!(after.deleted.is_empty());
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 2);
}

#[test]
fn status_smudges_same_tick_edit() {
    let tmp = TempDir::new().unwrap();
    let a = tmp.path();
    fs::write(a.join("f.txt"), "one").unwrap();
    cmd::init(&O, a).unwrap();
    cmd::snapshot(&O, a).unwrap();
    let scanned = Dirstate::load(a).unwrap().scanned;
    // Same size, same mtime, different content, right after the scan.
    let mtime = a.join("f.txt").metadata().unwrap().modified().unwrap();
    fs::write(a.join("f.txt"), "two").unwrap();
    let file = File::options().write(true).open(a.join("f.txt")).unwrap();
    file.set_modified(mtime).unwrap();
    std::thread::sleep(Duration::from_millis(5));
    assert!(has(&status(a), "modified", "f.txt"));
    // `scanned` has moved on, and the row can no longer pass for the file.
    let smudged = Dirstate::load(a).unwrap();
    assert!(smudged.scanned > scanned);
    assert_eq!(smudged.rows["f.txt"].size, None);
    assert_eq!(smudged.rows["f.txt"].hash, sha256(b"one"));
    assert!(has(&status(a), "modified", "f.txt"));
    assert_eq!(cmd::snapshot(&O, a).unwrap().recorded, 1);
    assert_eq!(Dirstate::load(a).unwrap().rows["f.txt"].size, Some(3));
    assert_eq!(cmd::snapshot(&O, a).unwrap().recorded, 0);
}

#[test]
fn interrupted_before_dirstate() {
    let (_tmp, a) = vault(&[("f1.txt", "v1")]);
    cmd::snapshot(&O, &a).unwrap();
    let dirstate = fs::read(a.join(".syncit/dirstate")).unwrap();
    put(&a, "f1.txt", "v2", 1);
    put(&a, "f2.txt", "new", 1);
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 2);
    let entries = log(&a).entries;
    // As if the run had stopped after the segment and before the dirstate.
    // The edit is written again as the identical entry; the new file is
    // recognised from the log.
    fs::write(a.join(".syncit/dirstate"), dirstate).unwrap();
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 1);
    assert_eq!(log(&a).entries, entries);
    assert_eq!(cmd::snapshot(&O, &a).unwrap().recorded, 0);
}

#[test]
fn interrupted_transfer() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    put(&a, "f.txt", "v2", 1);
    cmd::snapshot(&O, &a).unwrap();
    let before = names(&a.join(".syncit/log"));
    put(&a, "f.txt", "v3", 2);
    cmd::snapshot(&O, &a).unwrap();
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
        let mut vb = Vault::open(&O, &b, true).unwrap();
        materialise(&O, &mut vb, None, false, &mut report).unwrap();
        assert_eq!((report.incomplete, report.exit_code()), (1, 2));
    }
    assert!(is(&b, "f.txt", "v1"));
    // A local edit meanwhile is still recorded from the dirstate.
    assert_eq!(cmd::snapshot(&O, &b).unwrap().recorded, 0);
    let report = cmd::pull(&O, &b, &a).unwrap();
    assert_eq!(report.exit_code(), 0);
    assert!(is(&b, "f.txt", "v3"));
}

#[test]
fn hostile_segment() {
    let (tmp, a) = vault(&[("f.txt", "good")]);
    cmd::snapshot(&O, &a).unwrap();
    let outside = tmp.path().join("evil.txt");
    let plant = |text: String| {
        let name = sha256(text.as_bytes());
        fs::write(a.join(".syncit/log").join(&name), text).unwrap();
        name
    };
    let hash = sha256(b"good");
    let escape = plant(format!(
        "{{\"nickname\":\"a\",\"syncit\":1}}\n{{\"exec\":false,\"hash\":\"{hash}\",\"mtime\":1,\"parents\":[],\"path\":\"../evil.txt\"}}\n"
    ));
    plant(
        "{\"nickname\":\"a\",\"syncit\":1}\n{\"exec\":false,\"hash\":null, \"mtime\":1,\"parents\":[],\"path\":\"x\"}\n"
            .to_string(),
    );
    // A segment from before nicknames.
    plant("{\"syncit\":1}\n".to_string());
    fs::write(a.join(".syncit/log/not-a-hash"), "junk").unwrap();
    let b = tmp.path().join("b");
    cmd::init(&O, &b).unwrap();
    let report = cmd::sync(&O, &b, &a).unwrap();
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
    cmd::init(&O, &b).unwrap();
    fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, b.join("d")).unwrap();
    let report = cmd::pull(&O, &b, &a).unwrap();
    assert!(report.has("skipped", "d/x.txt"));
    assert_eq!(report.exit_code(), 2);
    assert!(names(&outside).is_empty());
    assert!(is(&b, "ok.txt", "ok"));
    // Nor is the link itself recorded or removed.
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().recorded, 0);
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
    cmd::init(&O, &b).unwrap();
    // Listing the ignore file in itself keeps these rules local to b.
    let rules = "# local rules\n\n.syncitignore\nsecret.txt\nbuild/\n";
    fs::write(b.join(".syncitignore"), rules).unwrap();
    put(&b, "secret.txt", "b's secret", 1);
    put(&b, "sub/secret.txt", "deeper", 1);
    let report = cmd::sync(&O, &b, &a).unwrap();
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
    cmd::sync(&O, &b, &a).unwrap();
    assert!(is(&b, "keep.txt", "k"));
    put(&a, "keep.txt", "back", 2);
    cmd::sync(&O, &b, &a).unwrap();
    fs::remove_file(b.join("keep.txt")).unwrap();
    let report = cmd::sync(&O, &b, &a).unwrap();
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
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.recorded, report.exit_code()), (1, 0));
    assert!(is(&b, ".syncitignore", "scratch\n") && !b.join("scratch").exists());
    // From the next run on they apply in b too.
    put(&b, "scratch/y.txt", "y", 2);
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().recorded, 0);
    assert!(!a.join("scratch/y.txt").exists() && is(&b, "scratch/y.txt", "y"));
    // Two vaults changing the rules is an ordinary conflict.
    put(&a, ".syncitignore", "scratch\nfrom a\n", 3);
    put(&b, ".syncitignore", "scratch\nfrom b\n", 4);
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().conflicts, 1);
    assert!(is(&a, ".syncitignore", "scratch\nfrom b\n"));
}

#[test]
fn corrupt_blob_source() {
    let (tmp, a) = vault(&[("f.txt", "good"), ("ok.txt", "ok")]);
    no_history(&[&a]);
    cmd::snapshot(&O, &a).unwrap();
    // Same size and mtime: a's scan does not see the change.
    put(&a, "f.txt", "evil", 0);
    let b = tmp.path().join("b");
    let report = cmd::clone(&O, Path::new("/"), &a, &b, &[], false).unwrap();
    assert_eq!(report.recorded, 0);
    assert_eq!((report.pending, report.exit_code()), (1, 2));
    assert_eq!(tree(&b).keys().collect::<Vec<_>>(), ["ok.txt"]);
}

#[test]
fn copy_deleted_stays_deleted() {
    let (_tmp, a, b) = pair(&[("f.txt", "base")]);
    put(&a, "f.txt", "from a", 1);
    put(&b, "f.txt", "from b", 2);
    cmd::sync(&O, &b, &a).unwrap();
    let copy = copy_of("f.txt", "from a");
    assert!(a.join(&copy).exists() && b.join(&copy).exists());
    cmd::sync(&O, &b, &a).unwrap(); // the copies are recorded as ordinary files
    fs::remove_file(a.join(&copy)).unwrap();
    for _ in 0..3 {
        cmd::sync(&O, &b, &a).unwrap();
        assert!(!a.join(&copy).exists() && !b.join(&copy).exists());
    }
    assert!(is(&a, "f.txt", "from b") && is(&b, "f.txt", "from b"));
}

#[test]
fn move_refuses() {
    let (tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g"), ("e/h.txt", "h")]);
    let err = |from: &Path, what: &[&str], to: &Path| {
        cmd::move_to(&O, from, &paths(what), to)
            .unwrap_err()
            .to_string()
    };
    // The target excludes the path: nothing is sent, nothing removed.
    let c = tmp.path().join("c");
    cmd::clone(&O, Path::new("/"), &a.join("d"), &c, &[], false).unwrap();
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
    cmd::move_to(&O, &a, &paths(&["e"]), &b).unwrap();
    put(&a, "e/h.txt", "mine", 2);
    fs::remove_file(a.join(".syncit/dirstate")).unwrap();
    let error = err(&b, &["e", "d"], &a);
    assert!(
        error.contains("e/h.txt (it has a different version") && !error.contains("d/g.txt"),
        "{error}"
    );
    assert!(is(&b, "e/h.txt", "h") && is(&b, "d/g.txt", "g") && is(&a, "e/h.txt", "mine"));
    // The target has a different, newer version.
    put(&a, "d/g.txt", "newer", 5);
    let error = err(&b, &["d"], &a);
    assert!(
        error.contains("d/g.txt (it has a different version"),
        "{error}"
    );
    assert!(is(&b, "d/g.txt", "g"));
    // No files under the path; a vault with itself.
    assert!(err(&b, &["nope"], &a).contains("nothing to move"));
    assert!(cmd::move_to(&O, &b, &paths(&["f.txt"]), &b).is_err());
    // An unsynced edit is carried over and moved.
    cmd::move_to(&O, &b, &paths(&["f.txt"]), &a).unwrap();
    assert!(is(&a, "f.txt", "edit") && !b.join("f.txt").exists());
    // Nothing reads as deleted afterwards.
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().recorded, 0);
    assert!(is(&a, "f.txt", "edit"));
}

#[test]
fn drop_refuses_sole_copy() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1"), ("d/g.txt", "g")]);
    no_history(&[&a, &b]);
    let both = paths(&["d", "f.txt"]);
    // No vault to check against and no history: nothing vouches for a copy.
    let error = cmd::drop(&O, &b, &both, None, false)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("f.txt") && error.contains("--check"),
        "{error}"
    );
    // An unrecorded edit is recorded, and then blocks the drop even against
    // a vault. With two paths given, nothing is dropped.
    put(&b, "f.txt", "edit", 1);
    let error = cmd::drop(&O, &b, &both, Some(&a), false)
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
    cmd::push(&O, &b, &a).unwrap();
    // The checked vault is scanned: a file it has since lost does not count.
    fs::remove_file(a.join("d/g.txt")).unwrap();
    let error = cmd::drop(&O, &b, &both, Some(&a), false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("d/g.txt"), "{error}");
    put(&a, "d/g.txt", "g", 2);
    let report = cmd::drop(&O, &b, &both, Some(&a), false).unwrap();
    assert!(report.has("dropped", "f.txt") && report.has("dropped", "d/g.txt"));
    assert!(tree(&b).is_empty() && is(&a, "f.txt", "edit"));
    // Nothing reads as deleted afterwards.
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().recorded, 0);
    assert!(is(&a, "f.txt", "edit") && is(&a, "d/g.txt", "g"));
    // A path with no files here is excluded all the same.
    cmd::drop(&O, &b, &paths(&["later"]), None, false).unwrap();
    assert!(!Config::load(&b).unwrap().includes("later/x.txt"));
    // Local history is another copy; --force skips the check.
    assert!(cmd::drop(&O, &a, &paths(&["f.txt"]), None, false).is_err());
    cmd::config(&O, &a, None, Some(Setting::KeepHistory(None))).unwrap();
    cmd::snapshot(&O, &a).unwrap();
    cmd::drop(&O, &a, &paths(&["f.txt"]), None, false).unwrap();
    assert!(!a.join("f.txt").exists() && blob_path(&a, &sha256(b"edit")).exists());
    cmd::config(&O, &a, None, Some(Setting::KeepHistory(Some(0)))).unwrap();
    assert!(cmd::drop(&O, &a, &paths(&["d"]), Some(&a), false).is_err());
    cmd::drop(&O, &a, &paths(&["d"]), None, true).unwrap();
    assert!(tree(&a).is_empty());
}

#[test]
fn interrupted_move() {
    // A move stopped after it saved the filter and before it removed the
    // files: the folder is excluded and its files are still present.
    let (_tmp, a, b) = pair(&[("d/g.txt", "g"), ("d/h.txt", "h"), ("f.txt", "f")]);
    configure(&b, |c| c.exclude.push("d".into()));
    // The next scan includes what is on disk: nothing is lost.
    assert_eq!(cmd::snapshot(&O, &b).unwrap().recorded, 0);
    let config = Config::load(&b).unwrap();
    assert!(config.includes("d/g.txt") && config.includes("d/h.txt"));
    assert!(is(&b, "d/g.txt", "g") && is(&b, "d/h.txt", "h"));
    // The files are synced as before, in both directions.
    put(&a, "d/g.txt", "edit", 1);
    assert_eq!(cmd::sync(&O, &b, &a).unwrap().exit_code(), 0);
    assert!(is(&b, "d/g.txt", "edit") && is(&a, "d/h.txt", "h"));
    // Moving again finishes the job and leaves one exclude.
    let report = cmd::move_to(&O, &b, &paths(&["d"]), &a).unwrap();
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
    let config = |change| cmd::config(&O, &a.join("sub"), None, change);
    let history = |change: Option<Option<u32>>| {
        let change = change.map(Setting::KeepHistory);
        config(change).unwrap().keep_history
    };
    // A new vault keeps one version back, and compacts by itself.
    assert_eq!(history(None), Some(1));
    assert!(config(None).unwrap().autocompact);
    assert_eq!(history(Some(None)), None);
    assert_eq!(Config::load(&a).unwrap().keep_history, None);
    // The setting takes effect.
    cmd::snapshot(&O, &a).unwrap();
    assert!(blob_path(&a, &sha256(b"v1")).exists());
    // Set to keep nothing back, the current version's content is not dropped.
    assert_eq!(history(Some(Some(0))), Some(0));
    assert_eq!(Config::load(&a).unwrap().keep_history, Some(0));
    cmd::snapshot(&O, &a).unwrap();
    assert!(blob_path(&a, &sha256(b"v1")).exists());
    assert_eq!(history(None), Some(0));
    let auto = |on| config(Some(Setting::Autocompact(on))).unwrap().autocompact;
    assert!(!auto(false) && !Config::load(&a).unwrap().autocompact);
    assert!(auto(true) && Config::load(&a).unwrap().autocompact);

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
    let blame = |dir: &Path, list: &[&str]| cmd::blame(&O, dir, &paths(list)).unwrap();
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
    cmd::sync(&O, &b, &a).unwrap();
    fs::remove_file(b.join("gone.txt")).unwrap();
    // Recorded identically in both vaults: one entry, two recorders.
    put(&a, "same.txt", "same", 2);
    put(&b, "same.txt", "same", 2);
    cmd::sync(&O, &b, &a).unwrap();
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
    cmd::config(&O, &a, None, Some(Setting::Nickname("drive".into()))).unwrap();
    put(&a, "f.txt", "v3", 3);
    cmd::sync(&O, &b, &a).unwrap();
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
    cmd::init(&O, &a.join("inner")).unwrap();
    put(&a, "inner/new.txt", "new", 1);
    let report = cmd::sync(&O, &b, &a).unwrap();
    assert_eq!((report.exit_code(), report.deleted), (0, 0));
    assert!(is(&b, "inner/old.txt", "old") && is(&b, "inner/new.txt", "new"));
    assert!(!b.join("inner/.syncit").exists());
    // The inner vault still works on its own, and its changes reach the outer.
    assert_eq!(cmd::snapshot(&O, &a.join("inner")).unwrap().recorded, 2);
    put(&b, "inner/new.txt", "edited", 2);
    cmd::sync(&O, &b, &a).unwrap();
    assert!(is(&a, "inner/new.txt", "edited"));
    assert!(has(&status(&a.join("inner")), "modified", "new.txt"));
    // Vaults that share files do not sync with each other.
    assert!(cmd::sync(&O, &a, &a.join("inner")).is_err());
    assert!(cmd::pull(&O, &a.join("inner"), &a).is_err());
    assert!(cmd::clone(&O, Path::new("/"), &a, &a.join("copy"), &[], false).is_err());
    assert!(!a.join("copy").exists());
    assert!(cmd::clone(&O, Path::new("/"), &a, &tmp.path().join("c"), &[], false).is_ok());
}

#[test]
fn version_mismatch() {
    let (_tmp, a, b) = pair(&[("f.txt", "v1")]);
    configure(&a, |c| c.syncit = 2);
    put(&a, "f.txt", "v2", 1);
    for result in [
        cmd::sync(&O, &b, &a),
        cmd::push(&O, &b, &a),
        cmd::pull(&O, &b, &a),
        cmd::snapshot(&O, &a),
    ] {
        let error = result
            .expect_err("a version 2 vault is refused")
            .to_string();
        assert!(error.contains("version 2"), "{error}");
    }
    assert!(is(&b, "f.txt", "v1"));
    assert!(
        cmd::sync(&O, &b, &b).is_err(),
        "a vault does not sync with itself"
    );
}

#[test]
fn push_counts_excluded() {
    let (tmp, a) = vault(&[("d/g.txt", "g"), ("music/m.mp3", "m"), ("f.txt", "f")]);
    let c = tmp.path().join("c");
    cmd::clone(&O, Path::new("/"), &a.join("d"), &c, &[], false).unwrap();
    fs::write(c.join(".syncitignore"), "d/tmp\n.syncitignore\n").unwrap();
    put(&a, "d/tmp/t.txt", "t", 1);
    cmd::snapshot(&O, &a).unwrap();
    let (va, vc) = (
        Vault::open(&O, &a, true).unwrap(),
        Vault::open(&O, &c, true).unwrap(),
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
    cmd::clone(&O, Path::new("/"), &a.join("d"), &c, &[], false).unwrap();
    let line = |path: &str, files, ours, theirs| State {
        path: path.into(),
        files,
        ours,
        theirs,
        conflict: None,
        from: None,
    };
    let list = |dir: &Path, paths: &[PathBuf], peer| cmd::status(&O, dir, paths, peer, true);
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
    let needy: Vec<State> = [0, 1, 2, 4, 5].map(|i| every[i].clone()).into();
    assert_eq!(cmd::status(&O, &c, &[], Some(&a), false).unwrap(), needy);
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
    cmd::snapshot(&O, &a).unwrap();
    {
        let mut vc = Vault::open(&O, &c, true).unwrap();
        let va = Vault::open(&O, &a, false).unwrap();
        cmd::receive(&O, &mut vc, &va, &mut Summary::default()).unwrap();
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
    cmd::snapshot(&O, &c).unwrap();
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
    let dry = &Context {
        json: false,
        dry_run: true,
        no_remove: false,
        secret_key: None,
        bar: syncit::output::bar(false),
        config_dir: None,
    };
    type Run = fn(&Context, &Path, &Path) -> anyhow::Result<Summary>;
    for run in [cmd::sync as Run, cmd::push, cmd::pull] {
        let files = [
            ("notes.txt", "base"),
            ("gone.txt", "x"),
            ("d/both.txt", "base"),
            ("merge.md", "one\ntwo\nthree\n"),
        ];
        let (tmp, a, b) = pair_with_history(&files);
        configure(&a, |c| c.keep_history = None);
        put(&a, "merge.md", "ONE\ntwo\nthree\n", 1);
        put(&b, "merge.md", "one\ntwo\nTHREE\n", 2);
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
        let done = run(&O, &a, &b).unwrap();
        assert!(before != listing());
        assert!(done.conflicts == 1 && done.recorded == 8, "{done:?}");
        assert!(done.has("updated", "merge.md"));
        assert_eq!(planned, done);
    }

    // `move`, both when it would go through and when it would be refused.
    let (tmp, a, b) = pair(&[("keep.txt", "k"), ("big/one.bin", "1")]);
    no_history(&[&b]);
    put(&a, "big/two.bin", "2", 1);
    let c = tmp.path().join("c");
    cmd::clone(&O, Path::new("/"), &a.join("keep.txt"), &c, &[], false).unwrap();
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
    let done = cmd::move_to(&O, &a, &paths(&["big"]), &b).unwrap();
    assert!(done.has("moved", "big/one.bin") && done.has("moved", "big/two.bin"));
    assert!(!a.join("big").exists() && is(&b, "big/two.bin", "2"));
    assert_eq!(planned, done);
    // `drop`, refused and then checked against a vault that has the files.
    let refused = cmd::drop(dry, &b, &paths(&["keep.txt"]), None, false).unwrap_err();
    assert!(refused.to_string().contains("nothing dropped"), "{refused}");
    let before = listing();
    let planned = cmd::drop(dry, &b, &paths(&["keep.txt"]), Some(&a), false).unwrap();
    assert!(before == listing(), "a dry run wrote something");
    let done = cmd::drop(&O, &b, &paths(&["keep.txt"]), Some(&a), false).unwrap();
    assert!(done.has("dropped", "keep.txt") && !b.join("keep.txt").exists());
    assert_eq!(planned, done);
}

#[test]
fn cp_from_plain_folder() {
    let dry = &Context {
        json: false,
        dry_run: true,
        no_remove: false,
        secret_key: None,
        bar: syncit::output::bar(false),
        config_dir: None,
    };
    let files = [
        ("same.txt", "s"),
        ("old.txt", "v1"),
        ("keep.txt", "k"),
        (".syncitignore", "secret.txt\n"),
    ];
    let (tmp, a) = vault(&files);
    // A folder that is not a vault, though it has a `.syncit` of its own.
    let plain = tmp.path().join("plain");
    put(&plain, "same.txt", "s", 5);
    put(&plain, "old.txt", "v2", 5);
    put(&plain, "d/new.txt", "n", 5);
    put(&plain, "secret.txt", "x", 5);
    put(&plain, ".syncit/config", "not a config", 5);
    put(&plain, "run.sh", "#!", 5);
    chmod(&plain, "run.sh", 0o555);
    std::os::unix::fs::symlink("old.txt", plain.join("link")).unwrap();

    let listing = || {
        let mut out = BTreeMap::new();
        all(&a, &mut out);
        out
    };
    let before = listing();
    let here = paths(&["."]);
    let planned = cmd::cp(dry, &plain, &here, &a).unwrap();
    assert!(before == listing(), "a dry run wrote something");
    let done = cmd::cp(&O, &plain, &here, &a).unwrap();
    assert_eq!(planned, done);
    // The vault's unrecorded files were recorded before any was replaced.
    assert_eq!((done.recorded, done.added, done.updated), (4, 2, 1));
    assert!(done.has("added", "d/new.txt") && done.has("added", "run.sh"));
    assert!(done.has("updated", "old.txt"));
    let recorded = |e: &syncit::entry::Entry| e.hash == Some(sha256(b"v1"));
    assert!(log(&a).entries.values().any(recorded));
    assert!(is(&a, "old.txt", "v2") && is(&a, "d/new.txt", "n") && is(&a, "keep.txt", "k"));
    assert_eq!(exec(&a, "run.sh"), Some(true));
    // Not copied: what the vault ignores, a link, and the source's `.syncit`.
    assert!(!a.join("secret.txt").exists() && !a.join("link").exists());
    Config::load(&a).unwrap();
    assert!(names(&a).iter().all(|n| !n.starts_with(".syncit-tmp-")));
    // The copies are unrecorded edits, as if made by hand.
    let found = status(&a);
    assert!(has(&found, "modified", "old.txt") && has(&found, "new", "d/new.txt"));
    assert!(!has(&found, "modified", "same.txt"));
    // Again, there is nothing to copy.
    let again = cmd::cp(&O, &plain, &here, &a).unwrap();
    assert_eq!((again.added, again.updated), (0, 0));

    // A named source lands under its name, in a folder that need not exist.
    let named = paths(&["plain/d", "plain/old.txt"]);
    let done = cmd::cp(&O, tmp.path(), &named, &a.join("in/here")).unwrap();
    assert!(done.has("added", "in/here/d/new.txt") && done.has("added", "in/here/old.txt"));
    assert!(is(&a, "in/here/d/new.txt", "n"));
    // A missing source is an error, and nothing is copied.
    let both = paths(&["plain/run.sh", "plain/nowhere"]);
    assert!(cmd::cp(&O, tmp.path(), &both, &a.join("x")).is_err());
    assert!(!a.join("x").exists());
    // The destination must be in a vault.
    assert!(cmd::cp(&O, &a, &paths(&["keep.txt"]), &plain.join("d")).is_err());
}

// ---- Remote vaults (store.md) -------------------------------------------

#[cfg(feature = "remote")]
mod remote {
    use super::*;
    use syncit::keys;
    use syncit::remote::{Bucket, Remote};
    use syncit::storage::Storage;

    /// Context with a phrase. Every remote test starts here, so that the
    /// keychain is the mock one, never the machine's.
    fn ctx(phrase: Option<&str>) -> Context {
        #[cfg(feature = "keychain")]
        {
            static MOCK: std::sync::Once = std::sync::Once::new();
            MOCK.call_once(|| {
                keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap())
            });
        }
        Context {
            json: false,
            dry_run: false,
            no_remove: false,
            secret_key: phrase.map(String::from),
            bar: syncit::output::bar(false),
            config_dir: None,
        }
    }

    fn url(dir: &Path) -> String {
        format!("file://{}", dir.display())
    }

    /// An empty folder for a remote vault, in `tmp`.
    fn place(tmp: &TempDir, name: &str) -> PathBuf {
        let dir = tmp.path().join(name);
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn init_remote() {
        let tmp = TempDir::new().unwrap();
        let r = place(&tmp, "r");
        let words = cmd::init_remote(&ctx(None), &url(&r), None, false).unwrap();
        let words = words.expect("a generated key is printed");
        assert_eq!(words.split(' ').count(), 24);
        assert_eq!(
            names(&r),
            BTreeSet::from(["config".into(), "format".into()])
        );
        let format = fs::read_to_string(r.join("format")).unwrap();
        let check = format
            .strip_prefix(r#"{"check":""#)
            .and_then(|f| f.strip_suffix("\",\"syncit\":1}\n"))
            .unwrap();
        assert!(syncit::entry::is_hash(check), "{format}");
        let key = keys::parse_phrase(&words).unwrap();
        let remote = Remote::open(Bucket::new(&url(&r), vec![]).unwrap(), key, None).unwrap();
        let config = remote.load_config().unwrap();
        assert_eq!(
            (config.include, config.keep_history, config.autocompact),
            (vec![String::new()], None, false)
        );
        assert_eq!(config.nickname, "origin");
        assert!(
            !fs::read(r.join("config"))
                .unwrap()
                .windows(6)
                .any(|w| w == b"origin")
        );

        // Given a key, it prints none.
        let given = ctx(Some(&words));
        let s = place(&tmp, "s");
        let printed = cmd::init_remote(&given, &url(&s), Some("box".into()), false);
        assert_eq!(printed.unwrap(), None);

        // Not empty: refused, and left as it was.
        let t = place(&tmp, "t");
        put(&t, "keep.txt", "mine", 0);
        assert!(cmd::init_remote(&given, &url(&t), None, false).is_err());
        assert_eq!(names(&t), BTreeSet::from(["keep.txt".into()]));
        assert!(cmd::init_remote(&given, &url(&r), None, false).is_err());
        let missing = tmp.path().join("missing");
        assert!(cmd::init_remote(&given, &url(&missing), None, false).is_err());
        assert!(!missing.exists(), "syncit never creates a directory");

        // A format other than version 1 is refused.
        fs::write(
            s.join("format"),
            format.replace("\"syncit\":1", "\"syncit\":2"),
        )
        .unwrap();
        let err = Bucket::new(&url(&s), vec![]).unwrap().check().unwrap_err();
        assert!(err.to_string().contains("format version 2"), "{err}");
    }

    /// The phrase every remote in these tests is made with.
    fn phrase() -> String {
        keys::phrase(&zeroize::Zeroizing::new([7u8; 32])).to_string()
    }

    /// Context with the test phrase.
    fn o() -> Context {
        ctx(Some(&phrase()))
    }

    /// A remote vault in the folder `name` of `tmp`: the folder, and the URL
    /// as a target.
    fn remote(tmp: &TempDir, name: &str) -> (PathBuf, PathBuf) {
        let dir = place(tmp, name);
        cmd::init_remote(&o(), &url(&dir), None, false).unwrap();
        (dir.clone(), PathBuf::from(url(&dir)))
    }

    fn storage(target: &Path) -> Remote {
        let target = target.to_str().unwrap();
        let key = keys::parse_phrase(&phrase()).unwrap();
        Remote::open(Bucket::new(target, vec![]).unwrap(), key, None).unwrap()
    }

    fn configure_remote(target: &Path, change: impl FnOnce(&mut Config)) {
        let r = storage(target);
        let mut config = r.load_config().unwrap();
        change(&mut config);
        r.save_config(&config).unwrap();
    }

    /// Every file under `dir`, recursively: path to bytes.
    fn listing(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut out = BTreeMap::new();
        all(dir, &mut out);
        out
    }

    /// Object names under `kind`; blobs sit in `ab/abcdef…` folders.
    fn objects(dir: &Path, kind: &str) -> BTreeSet<String> {
        let root = dir.join(kind);
        match kind {
            "blobs" => names(&root)
                .iter()
                .flat_map(|p| names(&root.join(p)))
                .collect(),
            _ => names(&root),
        }
    }

    #[test]
    fn story_c2_sync_through_remote() {
        let tmp = TempDir::new().unwrap();
        let (_r, origin) = remote(&tmp, "r");
        story_c2(&o(), tmp, &origin);
    }

    /// Run against `SYNCIT_TEST_REMOTE_URL`, in a new prefix under it, with
    /// credentials from the user's `credentials.toml` and the environment.
    /// Skipped when it is not set.
    #[test]
    fn cloud_remote() {
        let Ok(base) = std::env::var("SYNCIT_TEST_REMOTE_URL") else {
            eprintln!("skipped: SYNCIT_TEST_REMOTE_URL is not set");
            return;
        };
        let tmp = TempDir::new().unwrap();
        let unique = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let url = format!("{}/syncit-test-{unique}", base.trim_end_matches('/'));
        let o = Context {
            bar: syncit::output::bar(false),
            config_dir: syncit::output::config_dir(),
            ..o()
        };
        cmd::init_remote(&o, &url, None, false).unwrap();
        story_c2(&o, tmp, Path::new(&url));
    }

    /// Story C.2 through the remote vault `origin`, made in or for `tmp`.
    fn story_c2(o: &Context, tmp: TempDir, origin: &Path) {
        let a = place(&tmp, "a");
        for (path, content) in [("notes.txt", "from a"), ("d/shared.txt", "base")] {
            put(&a, path, content, 0);
        }
        cmd::init(o, &a).unwrap();
        // A vault with no history supplies its current files.
        no_history(&[&a]);
        assert_eq!(cmd::push(o, &a, origin).unwrap().exit_code(), 0);

        let c = tmp.path().join("c");
        fs::create_dir(&c).unwrap();
        put(&c, "music/song.mp3", "la", 0);
        cmd::init(o, &c).unwrap();
        // a and c never sync with each other.
        assert_eq!(cmd::sync(o, &c, origin).unwrap().exit_code(), 0);
        assert!(is(&c, "notes.txt", "from a") && is(&c, "d/shared.txt", "base"));
        assert_eq!(cmd::pull(o, &a, origin).unwrap().exit_code(), 0);
        assert!(is(&a, "music/song.mp3", "la"));
        assert_eq!(tree(&a), tree(&c));

        // A conflict, either order: both edit, both sync through the remote.
        put(&a, "d/shared.txt", "edit in a", 5);
        put(&c, "d/shared.txt", "edit in c", 6);
        put(&a, "new-a.txt", "na", 5);
        put(&c, "new-c.txt", "nc", 5);
        assert_eq!(cmd::sync(o, &c, origin).unwrap().exit_code(), 0);
        assert_eq!(cmd::sync(o, &a, origin).unwrap().conflicts, 1);
        assert_eq!(cmd::sync(o, &c, origin).unwrap().conflicts, 1);
        assert_eq!(tree(&a), tree(&c));
        assert!(is(&a, "d/shared.txt", "edit in c"));
        assert!(is(&a, &copy_of("d/shared.txt", "edit in a"), "edit in a"));
        assert!(is(&c, "new-a.txt", "na") && is(&a, "new-c.txt", "nc"));
        // A deletion travels too.
        fs::remove_file(a.join("notes.txt")).unwrap();
        cmd::sync(o, &a, origin).unwrap();
        cmd::sync(o, &c, origin).unwrap();
        assert!(!c.join("notes.txt").exists());
        assert_eq!(tree(&a), tree(&c));
        // A third machine clones the URL and has everything.
        let m = tmp.path().join("m");
        assert_eq!(
            cmd::clone(o, Path::new("/"), origin, &m, &[], false)
                .unwrap()
                .conflicts,
            1
        );
        assert_eq!(tree(&m), tree(&a));
    }

    #[test]
    fn remote_is_opaque() {
        let o = &o();
        let (tmp, a) = vault(&[("notes/plan.txt", "a known line of a file\n")]);
        configure(&a, |c| c.nickname = "deskvault".into());
        cmd::snapshot(o, &a).unwrap();
        put(
            &a,
            "notes/plan.txt",
            "a known line of a file\nand another\n",
            1,
        );
        let (r, origin) = remote(&tmp, "r");
        configure_remote(&origin, |c| c.nickname = "originvault".into());
        cmd::push(o, &a, &origin).unwrap();
        let mut hashes: BTreeSet<String> =
            log(&a).entries.keys().map(|id| id.to_string()).collect();
        hashes.extend(log(&a).entries.values().filter_map(|e| e.hash.clone()));
        hashes.extend(names(&a.join(".syncit/log")));
        assert!(hashes.len() > 4, "{hashes:?}");
        let secrets = [
            "a known line",
            "notes/plan.txt",
            "plan.txt",
            "deskvault",
            "originvault",
        ];
        let found = listing(&r);
        assert!(found.len() > 5, "{:?}", found.keys());
        for (path, bytes) in &found {
            let name = path.file_name().unwrap().to_string_lossy();
            assert!(
                !hashes.contains(name.as_ref()),
                "{} is a plaintext hash",
                path.display()
            );
            for text in secrets
                .iter()
                .copied()
                .chain(hashes.iter().map(String::as_str))
            {
                let text = text.as_bytes();
                assert!(
                    !bytes.windows(text.len()).any(|w| w == text),
                    "{} holds {}",
                    path.display(),
                    String::from_utf8_lossy(text)
                );
            }
        }
    }

    #[test]
    fn remote_hostile() {
        let o = &o();
        let (tmp, a) = vault(&[("good.txt", "good"), ("cut.bin", &"x".repeat(100_000))]);
        let (r, origin) = remote(&tmp, "r");
        cmd::push(o, &a, &origin).unwrap();
        let key = keys::parse_phrase(&phrase()).unwrap();
        let logs: Vec<String> = objects(&r, "log").into_iter().collect();
        // A garbage object, and a valid one copied to another name.
        fs::write(r.join("log").join("ab".repeat(32)), b"garbage").unwrap();
        fs::copy(
            r.join("log").join(&logs[0]),
            r.join("log").join("cd".repeat(32)),
        )
        .unwrap();
        fs::write(r.join("log/not-a-name"), b"ignored").unwrap();
        // A truncated blob.
        let cut = syncit::crypt::name(&key, "blobs", &sha256("x".repeat(100_000).as_bytes()));
        let blob = r.join("blobs").join(&cut[..2]).join(&cut);
        let bytes = fs::read(&blob).unwrap();
        fs::write(&blob, &bytes[..bytes.len() - 10]).unwrap();

        let c = tmp.path().join("c");
        fs::create_dir(&c).unwrap();
        cmd::init(o, &c).unwrap();
        let sum = cmd::pull(o, &c, &origin).unwrap();
        assert_eq!(sum.rejected, 2, "{sum:?}");
        assert!(sum.has("pending", "cut.bin"), "{sum:?}");
        assert!(is(&c, "good.txt", "good") && !c.join("cut.bin").exists());
        assert_eq!(sum.exit_code(), 2);
        // A garbage config is an error, not a panic.
        fs::write(r.join("config"), b"garbage").unwrap();
        assert!(cmd::pull(o, &c, &origin).is_err());
    }

    #[test]
    fn move_to_remote() {
        let o = &o();
        let (tmp, a) = vault(&[
            ("keep.txt", "k"),
            ("big/one.bin", "1"),
            ("big/two.bin", "2"),
        ]);
        let (_r, origin) = remote(&tmp, "r");
        let (_s, narrow) = remote(&tmp, "s");
        configure_remote(&narrow, |c| c.include = vec!["keep.txt".into()]);
        let refused = cmd::move_to(o, &a, &paths(&["big"]), &narrow).unwrap_err();
        assert!(refused.to_string().contains("excludes"), "{refused}");
        assert!(is(&a, "big/one.bin", "1"));

        let done = cmd::move_to(o, &a, &paths(&["big"]), &origin).unwrap();
        assert!(done.has("moved", "big/one.bin") && done.has("moved", "big/two.bin"));
        assert!(!a.join("big").exists() && is(&a, "keep.txt", "k"));
        // The remote has their content: a new vault gets them from it.
        let c = tmp.path().join("c");
        fs::create_dir(&c).unwrap();
        cmd::init(o, &c).unwrap();
        assert_eq!(cmd::pull(o, &c, &origin).unwrap().exit_code(), 0);
        assert!(is(&c, "big/one.bin", "1") && is(&c, "big/two.bin", "2"));
        // And a sync does not bring them back here.
        cmd::sync(o, &a, &origin).unwrap();
        assert!(!a.join("big").exists());
    }

    #[test]
    fn partial_remote() {
        let o = &o();
        let (tmp, a) = vault(&[
            ("docs/a.txt", "a"),
            ("docs/b.txt", "b"),
            ("music/m.mp3", "m"),
        ]);
        let (r, origin) = remote(&tmp, "r");
        configure_remote(&origin, |c| c.include = vec!["docs".into()]);
        cmd::push(o, &a, &origin).unwrap();
        assert_eq!(objects(&r, "blobs").len(), 2, "only the folder it includes");
        let va = Vault::open(o, &a, true).unwrap();
        let vr = Vault::open_url(o, origin.to_str().unwrap(), Some(&va)).unwrap();
        assert_eq!(cmd::excluded(&va, &vr), ["music/m.mp3"]);
        assert!(vr.dirstate.rows.contains_key("docs/a.txt"));
        // A vault that pulls gets only what the remote has.
        let c = tmp.path().join("c");
        fs::create_dir(&c).unwrap();
        cmd::init(o, &c).unwrap();
        let sum = cmd::pull(o, &c, &origin).unwrap();
        assert!(
            is(&c, "docs/a.txt", "a") && !c.join("music").exists(),
            "{sum:?}"
        );
    }

    #[test]
    fn dry_run_remote() {
        let o = &o();
        let dry = &Context {
            dry_run: true,
            ..o.clone()
        };
        let (tmp, a) = vault(&[("notes.txt", "base"), ("d/both.txt", "base")]);
        let (r, origin) = remote(&tmp, "r");
        cmd::push(o, &a, &origin).unwrap();
        let c = tmp.path().join("c");
        fs::create_dir(&c).unwrap();
        cmd::init(o, &c).unwrap();
        cmd::pull(o, &c, &origin).unwrap();
        put(&a, "notes.txt", "edited in a", 1);
        put(&a, "new.txt", "new", 1);
        put(&c, "d/both.txt", "from c", 2);
        cmd::sync(o, &c, &origin).unwrap();
        put(&a, "d/both.txt", "from a", 1);

        let before = (listing(&r), listing(&a));
        let planned = cmd::sync(dry, &a, &origin).unwrap();
        assert!(
            before == (listing(&r), listing(&a)),
            "a dry run wrote something"
        );
        let done = cmd::sync(o, &a, &origin).unwrap();
        assert!(before != (listing(&r), listing(&a)));
        assert!(
            done.conflicts == 1 && done.has("updated", "d/both.txt"),
            "{done:?}"
        );
        assert_eq!(planned, done);
    }

    #[test]
    fn status_vs_remote() {
        let o = &o();
        let (tmp, a) = vault(&[("same.txt", "s"), ("edit.txt", "e"), ("gone.txt", "g")]);
        let (_r, origin) = remote(&tmp, "r");
        let f = tmp.path().join("f");
        fs::create_dir(&f).unwrap();
        cmd::init(o, &f).unwrap();
        configure(&f, |c| c.keep_history = None);
        cmd::push(o, &a, &origin).unwrap();
        cmd::push(o, &a, &f).unwrap();
        put(&a, "edit.txt", "edited", 1);
        put(&a, "new.txt", "n", 1);
        fs::remove_file(a.join("gone.txt")).unwrap();
        for all in [false, true] {
            let vs_folder = cmd::status(o, &a, &[], Some(&f), all).unwrap();
            let vs_remote = cmd::status(o, &a, &[], Some(&origin), all).unwrap();
            assert_eq!(vs_folder, vs_remote);
            assert!(!vs_remote.is_empty());
        }
        // After a sync, the remote is current too.
        cmd::sync(o, &a, &origin).unwrap();
        let lines = cmd::status(o, &a, &[], Some(&origin), true).unwrap();
        assert!(
            lines
                .iter()
                .all(|l| l.ours == "current" && l.theirs == Some("current"))
        );
    }

    /// A new, empty vault in the folder `name` of `tmp`.
    fn empty_vault(tmp: &TempDir, name: &str) -> PathBuf {
        let dir = place(tmp, name);
        cmd::init(&O, &dir).unwrap();
        dir
    }

    #[test]
    fn clone_remote() {
        let o = &o();
        let (tmp, a) = vault(&[("notes.txt", "n"), ("d/x.txt", "x")]);
        let (_r, origin) = remote(&tmp, "r");
        put(&a, "notes.txt", "n2", 1);
        cmd::push(o, &a, &origin).unwrap();
        // A third machine clones the URL and has everything.
        let m = tmp.path().join("m");
        assert_eq!(
            cmd::clone(o, Path::new("/"), &origin, &m, &[], false)
                .unwrap()
                .exit_code(),
            0
        );
        assert_eq!(tree(&m), tree(&a));
        let config = Config::load(&m).unwrap();
        assert_eq!(config.include, [""]);
        assert_eq!(config.remotes["origin"], origin.to_str().unwrap());
        // Its history came too: the remote keeps every version.
        assert_eq!(log(&m).entries.len(), log(&a).entries.len());
        put(&m, "from-m.txt", "m", 1);
        cmd::sync(o, &m, Path::new("origin")).unwrap();
        cmd::pull(o, &a, &origin).unwrap();
        assert!(is(&a, "from-m.txt", "m"));
    }

    #[test]
    fn remote_add() {
        let o = &o();
        let (tmp, a) = vault(&[("f.txt", "f")]);
        let (_r, origin) = remote(&tmp, "r");
        let url = origin.to_str().unwrap();
        let f = empty_vault(&tmp, "f");
        assert_eq!(cmd::remote_add(o, &a, url, None, false).unwrap(), "origin");
        let f_path = f.canonicalize().unwrap().display().to_string();
        assert_eq!(
            cmd::remote_add(o, &a, f.to_str().unwrap(), None, false).unwrap(),
            "f"
        );
        let listed = cmd::remotes(o, &a).unwrap();
        assert_eq!(
            listed,
            [
                ("f".to_string(), f_path),
                ("origin".to_string(), url.to_string())
            ]
        );
        // A taken nickname needs --nickname.
        let err = cmd::remote_add(o, &a, url, None, false).unwrap_err();
        assert!(err.to_string().contains("--nickname"), "{err}");
        let named = cmd::remote_add(o, &a, url, Some("backup".into()), false).unwrap();
        assert_eq!(named, "backup");
        // A wrong phrase is refused, and the config is unchanged.
        let before = Config::load(&a).unwrap();
        let other = keys::phrase(&zeroize::Zeroizing::new([8u8; 32])).to_string();
        let err = cmd::remote_add(&ctx(Some(&other)), &a, url, Some("x".into()), false);
        assert!(err.unwrap_err().to_string().starts_with("no key for "));
        let err = cmd::remote_add(&ctx(Some("not words")), &a, url, Some("x".into()), false);
        assert!(err.is_err());
        assert_eq!(Config::load(&a).unwrap(), before);
        // Not a vault.
        assert!(cmd::remote_add(o, &a, tmp.path().to_str().unwrap(), None, false).is_err());
        let empty = place(&tmp, "empty");
        let err = cmd::remote_add(o, &a, &url_of(&empty), None, false).unwrap_err();
        assert!(err.to_string().starts_with("no vault at "), "{err}");

        // The key from the user's secrets.toml, which belongs to no vault:
        // clone, run in a vault, finds it too.
        let (_s, second) = remote(&tmp, "s");
        let second = second.to_str().unwrap();
        let saved = &Context {
            bar: syncit::output::bar(false),
            config_dir: Some(tmp.path().join("config")),
            ..ctx(None)
        };
        keys::add_saved(saved, second, &phrase()).unwrap();
        let added = cmd::remote_add(saved, &a, second, Some("two".into()), false);
        assert_eq!(added.unwrap(), "two");
        let copy = tmp.path().join("copy");
        cmd::clone(saved, &a, Path::new("two"), &copy, &[], false).unwrap();
        // remove forgets the remote, not its key.
        cmd::remote_remove(o, &a, "two").unwrap();
        assert!(keys::saved(saved, second).unwrap().is_some());
        cmd::remote_remove(o, &a, "backup").unwrap();
        assert!(cmd::remote_remove(o, &a, "backup").is_err());
        let names: Vec<String> = cmd::remotes(o, &a)
            .unwrap()
            .into_iter()
            .map(|r| r.0)
            .collect();
        assert_eq!(names, ["f", "origin"]);
    }

    fn url_of(dir: &Path) -> String {
        url(dir)
    }

    #[test]
    fn remote_by_nickname() {
        let o = &o();
        let (tmp, a) = vault(&[("f.txt", "f")]);
        let (r, origin) = remote(&tmp, "r");
        // A folder vault that shares the remote's nickname.
        let folder = empty_vault(&tmp, "origin");
        cmd::remote_add(o, &a, origin.to_str().unwrap(), None, false).unwrap();
        assert!(!r.join("blobs").exists());
        cmd::sync(o, &a, Path::new("origin")).unwrap();
        assert_eq!(objects(&r, "blobs").len(), 1);
        assert!(!folder.join("f.txt").exists());
        put(&a, "g.txt", "g", 1);
        cmd::sync(o, &a, &origin).unwrap();
        assert_eq!(objects(&r, "blobs").len(), 2);
        assert!(!folder.join("g.txt").exists());
        cmd::sync(o, &a, &folder).unwrap();
        assert!(is(&folder, "f.txt", "f") && is(&folder, "g.txt", "g"));
        // A nickname is looked up only in the vault's own remotes.
        assert!(cmd::sync(o, &a, Path::new("nowhere")).is_err());
        // clone, run in the vault, resolves a nickname: the remote's, and
        // one for a folder vault.
        cmd::remote_add(
            o,
            &a,
            folder.to_str().unwrap(),
            Some("backup".into()),
            false,
        )
        .unwrap();
        for (name, location) in [("origin", &origin), ("backup", &folder)] {
            let c = tmp.path().join(format!("clone-{name}"));
            cmd::clone(o, &a, Path::new(name), &c, &[], false).unwrap();
            assert!(is(&c, "f.txt", "f") && is(&c, "g.txt", "g"));
            let remotes = Config::load(&c).unwrap().remotes;
            // A folder is recorded by its canonical path; a URL as it is.
            let want = location.canonicalize().unwrap_or(location.to_path_buf());
            assert!(
                remotes.values().any(|l| Path::new(l) == want),
                "{remotes:?}"
            );
        }
    }

    #[test]
    fn clone_only() {
        let o = &o();
        let files = [
            ("a/1.txt", "1"),
            ("b/c/2.txt", "2"),
            ("b/d/3.txt", "3"),
            ("e.txt", "e"),
        ];
        let (tmp, src) = vault(&files);
        let (_r, origin) = remote(&tmp, "r");
        cmd::push(o, &src, &origin).unwrap();
        let only = ["a".to_string(), "b/c".to_string()];
        for (from, dir) in [(origin.clone(), "x"), (src.clone(), "y")] {
            let x = tmp.path().join(dir);
            assert_eq!(
                cmd::clone(o, Path::new("/"), &from, &x, &only, false)
                    .unwrap()
                    .exit_code(),
                0
            );
            assert_eq!(Config::load(&x).unwrap().include, only);
            let have: Vec<String> = tree(&x).into_keys().collect();
            assert_eq!(have, ["a/1.txt", "b/c/2.txt"], "{dir}");
        }
        // A later get --from adds a third.
        let x = tmp.path().join("x");
        cmd::get_from(o, &x, &["e.txt".to_string()], Path::new("origin")).unwrap();
        assert!(is(&x, "e.txt", "e") && !x.join("b/d").exists());
        // A path below a vault's root with --only is refused; nothing is made.
        let z = tmp.path().join("z");
        let err = cmd::clone(o, Path::new("/"), &src.join("b"), &z, &only, false).unwrap_err();
        assert!(err.to_string().contains("root"), "{err}");
        assert!(!z.exists());
        assert!(
            cmd::clone(
                o,
                Path::new("/"),
                &origin,
                &z,
                &["../up".to_string()],
                false
            )
            .is_err()
        );
        assert!(!z.exists());
    }

    #[test]
    fn get_from_remote() {
        let o = &o();
        let files = [
            ("music/m.mp3", "m"),
            ("music/n.mp3", "n"),
            ("docs/d.txt", "d"),
        ];
        let (tmp, a) = vault(&files);
        let (_r, origin) = remote(&tmp, "r");
        cmd::push(o, &a, &origin).unwrap();
        let p = tmp.path().join("p");
        cmd::clone(o, Path::new("/"), &origin, &p, &["docs".to_string()], false).unwrap();
        let blobs_before = blobs(&p);
        assert_eq!(blobs_before.len(), 1);
        cmd::get_from(o, &p, &["music".to_string()], Path::new("origin")).unwrap();
        assert!(is(&p, "music/m.mp3", "m") && is(&p, "music/n.mp3", "n"));
        assert_eq!(blobs(&p).len(), 3, "nothing else");
        cmd::sync(o, &p, Path::new("origin")).unwrap();
        assert!(Config::load(&p).unwrap().includes("music/m.mp3"));
        assert!(is(&p, "music/m.mp3", "m"));

        // --from with a folder vault's path is the inferred form.
        let (q1, q2) = (tmp.path().join("q1"), tmp.path().join("q2"));
        cmd::clone(o, Path::new("/"), &a.join("docs"), &q1, &[], false).unwrap();
        cmd::clone(o, Path::new("/"), &a.join("docs"), &q2, &[], false).unwrap();
        cmd::get(o, &q1, &[a.join("music")]).unwrap();
        cmd::get_from(o, &q2, &["music".to_string()], &a).unwrap();
        assert_eq!(tree(&q1), tree(&q2));
        let include = |dir: &Path| Config::load(dir).unwrap().include;
        assert_eq!(include(&q1), include(&q2));
        assert!(cmd::get_from(o, &q2, &["/abs".to_string()], &a).is_err());
    }

    #[test]
    fn remote_history_depth() {
        let o = &o();
        let (tmp, a) = vault(&[("f.txt", "v1")]);
        let (r, origin) = remote(&tmp, "r");
        cmd::remote_add(o, &a, origin.to_str().unwrap(), None, false).unwrap();
        let origin = Path::new("origin");
        cmd::push(o, &a, origin).unwrap();
        put(&a, "f.txt", "v2", 1);
        cmd::push(o, &a, origin).unwrap();
        put(&a, "f.txt", "v3", 2);
        cmd::push(o, &a, origin).unwrap();
        assert_eq!(
            objects(&r, "blobs").len(),
            3,
            "a new remote keeps every version"
        );
        let segments = objects(&r, "log");
        configure(&a, |c| c.keep_history = Some(0));
        let config = cmd::config(o, &a, Some(origin), Some(Setting::KeepHistory(Some(1)))).unwrap();
        assert_eq!(config.keep_history, Some(1));
        let own = Config::load(&a).unwrap().keep_history;
        assert_eq!(own, Some(0), "this vault's own is untouched");
        assert_eq!(cmd::compact(o, &a, Some(origin)).unwrap().0, 1);
        assert_eq!(objects(&r, "blobs").len(), 2);
        assert_eq!(objects(&r, "log"), segments, "the log is whole");
        // Outside any vault, by URL.
        let url = url_of(&r);
        let outside = cmd::config(o, tmp.path(), Some(Path::new(&url)), None).unwrap();
        assert_eq!(outside.keep_history, Some(1));
        let m = tmp.path().join("m");
        cmd::clone(o, Path::new("/"), Path::new(&url), &m, &[], false).unwrap();
        assert!(is(&m, "f.txt", "v3"));
    }
}

#[cfg(not(feature = "remote"))]
#[test]
fn no_remote_feature() {
    let (tmp, a) = vault(&[("f.txt", "f")]);
    let message = "this build has no remote support: install with the remote feature";
    let r = tmp.path().join("r");
    fs::create_dir(&r).unwrap();
    let url = format!("file://{}", r.display());
    let err = cmd::sync(&O, &a, Path::new(&url)).unwrap_err();
    assert_eq!(err.to_string(), message);
    let err = cmd::init_remote(&O, &url, None, false).unwrap_err();
    assert_eq!(err.to_string(), message);
    let err = cmd::remote_add(&O, &a, &url, None, false).unwrap_err();
    assert_eq!(err.to_string(), message);
    assert!(names(&r).is_empty());
    // Folders work as ever, by nickname too.
    let f = tmp.path().join("f");
    fs::create_dir(&f).unwrap();
    cmd::init(&O, &f).unwrap();
    assert_eq!(
        cmd::remote_add(&O, &a, f.to_str().unwrap(), None, false).unwrap(),
        "f"
    );
    cmd::sync(&O, &a, Path::new("f")).unwrap();
    assert!(is(&f, "f.txt", "f"));
    // A nickname that names a URL, as another build may have saved.
    configure(&a, |c| {
        c.remotes.insert("origin".into(), url.clone());
    });
    let err = cmd::sync(&O, &a, Path::new("origin")).unwrap_err();
    assert_eq!(err.to_string(), message);
}
