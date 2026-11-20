//! The built binary. Kept apart from `stories.rs`: spawning a process
//! briefly duplicates every open file of the test process, vault locks
//! included, which makes a concurrent test see its vault as in use.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn names(dir: &Path) -> BTreeSet<String> {
    let names = fs::read_dir(dir).unwrap();
    names
        .map(|f| f.unwrap().file_name().into_string().unwrap())
        .collect()
}

#[test]
fn cli_smoke() {
    let tmp = TempDir::new().unwrap();
    let run = |dir: &Path, args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_syncit"))
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
        )
    };
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::write(a.join("sub/f.txt"), "v1").unwrap();

    assert_eq!(run(&a, &["status"]).0, 1, "not a vault");
    assert_eq!(run(&a, &["init"]).0, 0);
    assert_eq!(run(&a, &["init"]).0, 1, "already a vault");
    let (code, out) = run(&a, &["status"]);
    assert_eq!(code, 0);
    assert!(out.contains("  new: sub/f.txt"), "{out}");
    assert!(
        names(&a.join(".syncit/log")).is_empty(),
        "status writes nothing"
    );
    let (code, out) = run(&a.join("sub"), &["snapshot"]);
    assert_eq!(code, 0);
    assert!(out.contains("recorded 1,"), "{out}");
    assert_eq!(run(&a, &["status"]), (0, String::new()));

    assert_eq!(run(&a, &["sync"]).0, 1, "no vault named");
    assert_eq!(run(&a, &["config", "local-history", "maybe"]).0, 1);
    let (code, out) = run(&a.join("sub"), &["config", "local-history", "on"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "local-history: on\nstore: none\n")
    );
    assert_eq!(run(&a, &["config", "store", "none"]).0, 0);
    assert_eq!(run(&a, &["config"]).1, out);
    assert_eq!(run(&a, &["frobnicate"]).0, 1, "bad arguments");
    assert_eq!(run(tmp.path(), &["clone", "a/sub", "b"]).0, 0);
    assert_eq!(fs::read_to_string(b.join("sub/f.txt")).unwrap(), "v1");
    fs::write(a.join("sub/f.txt"), "from a").unwrap();
    fs::write(b.join("sub/f.txt"), "from b").unwrap();
    let (code, out) = run(&b, &["sync", "../a"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("conflict: sub/f.txt"), "{out}");
    assert_eq!(run(&b, &["drop", "sub"]).0, 1, "no other copy is known");
    assert_eq!(run(&b, &["drop", "--check", "../a", "sub"]).0, 0);
    assert!(!b.join("sub").exists());
    assert_eq!(
        run(&b, &["get", "../a/sub"]).0,
        2,
        "the conflict is still open"
    );
    // Which edit won depends on the two mtimes, written a moment apart.
    assert!(b.join("sub/f.txt").exists());

    let (code, help) = run(&a, &["--help"]);
    assert_eq!(code, 0);
    for command in [
        "init", "clone", "sync", "push", "pull", "snapshot", "status", "get", "drop", "config",
    ] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(command)),
            "{command} is missing from --help"
        );
    }
}
