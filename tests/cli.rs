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
    let both = |dir: &Path, args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_syncit"))
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    };
    // Stdout only, for the many checks that need nothing else.
    let run = |dir: &Path, args: &[&str]| {
        let (code, out, _) = both(dir, args);
        (code, out)
    };
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    fs::create_dir_all(a.join("sub")).unwrap();
    fs::write(a.join("sub/f.txt"), "v1").unwrap();

    assert_eq!(run(&a, &["status"]).0, 1, "not a vault");
    assert_eq!(run(&a, &["init"]).0, 0);
    assert_eq!(run(&a, &["init"]).0, 1, "already a vault");
    let (code, out) = run(&a, &["status"]);
    assert_eq!(code, 0);
    assert_eq!(out, "+  sub/f.txt  new\n");
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
        (0, "local-history: on\nnickname: a\n")
    );
    assert_eq!(
        run(&a, &["config", "store", "none"]).0,
        1,
        "no such setting"
    );
    assert_eq!(run(&a, &["config"]).1, out);
    assert_eq!(run(&a, &["blame"]), (0, "a sub/f.txt\n".to_string()));
    assert_eq!(run(&a, &["config", "nickname", "two words"]).0, 1);
    let (code, out) = run(&a, &["config", "nickname", "desk"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "local-history: on\nnickname: desk\n")
    );
    let (code, out) = run(&a, &["--json", "blame", "sub"]);
    assert_eq!(code, 0);
    assert_eq!(
        out,
        "{\"type\":\"blame\",\"path\":\"sub/f.txt\",\"nicknames\":[\"a\"]}\n"
    );
    // `log` prints the hash that `restore` takes; the copy is named by it.
    let (code, out) = run(&a, &["log", "sub/f.txt"]);
    assert_eq!(code, 0);
    let short = &out[..8];
    assert!(out.ends_with("Z a sub/f.txt (current)\n"), "{out}");
    assert_eq!(run(&a, &["restore", "sub/f.txt"]).0, 1, "no hash given");
    assert_eq!(
        run(&a, &["restore", "sub/f.txt", "zz"]).0,
        1,
        "no such version"
    );
    let (code, out) = run(&a.join("sub"), &["restore", "f.txt", short]);
    assert_eq!((code, out), (0, format!("C sub/f-{short}.txt\n")));
    fs::remove_file(a.join(format!("sub/f-{short}.txt"))).unwrap();
    let (code, out) = run(&a, &["restore", "--in-place", "sub/f.txt", short]);
    assert_eq!(
        (code, out.as_str()),
        (0, "sub/f.txt is already that version\n")
    );
    assert_eq!(run(&a, &["frobnicate"]).0, 1, "bad arguments");
    assert_eq!(run(tmp.path(), &["clone", "a/sub", "b"]).0, 0);
    assert_eq!(fs::read_to_string(b.join("sub/f.txt")).unwrap(), "v1");
    fs::write(a.join("sub/f.txt"), "from a").unwrap();
    fs::write(b.join("sub/f.txt"), "from b").unwrap();
    let (code, planned) = run(&b, &["sync", "--dry-run", "../a"]);
    assert_eq!(code, 2, "{planned}");
    assert_eq!(fs::read_to_string(a.join("sub/f.txt")).unwrap(), "from a");
    assert_eq!(run(&b, &["status", "--dry-run"]).0, 1, "not for status");
    let (code, out) = run(&b, &["sync", "../a"]);
    assert_eq!(code, 2, "{out}");
    assert_eq!(planned, format!("dry run: nothing is changed\n{out}"));
    assert!(out.contains("-> U sub/f.txt (conflict: sub/f-"), "{out}");
    assert_eq!(run(&b, &["move", "sub"]).0, 1, "no vault named");
    let (code, out) = run(&b, &["move", "sub", "../a"]);
    assert_eq!(code, 2, "moved; the conflict is still open: {out}");
    assert!(out.contains("\n-> R sub/f.txt\n"), "{out}");
    assert!(!b.join("sub").exists());
    assert_eq!(
        run(&b, &["get", "../a/sub"]).0,
        2,
        "the conflict is still open"
    );
    // Which edit won depends on the two mtimes, written a moment apart.
    assert!(b.join("sub/f.txt").exists());

    // Results on stdout, warnings and errors on stderr. c holds only d/ of a.
    fs::create_dir(a.join("d")).unwrap();
    fs::write(a.join("d/g.txt"), "g").unwrap();
    std::os::unix::fs::symlink("g.txt", a.join("d/link")).unwrap();
    assert_eq!(run(tmp.path(), &["clone", "a/d", "c"]).0, 0);
    let (code, out, err) = both(&a, &["push", "../c"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(err.contains("warning: skipped 1 symbolic links"), "{err}");
    assert!(
        err.contains("/c excludes 2 files (sub/); they were not copied"),
        "{err}"
    );
    assert!(
        !out.contains("warning") && out.starts_with("recorded 0,"),
        "{out}"
    );
    let (_, _, err) = both(&a, &["sync", "../c"]);
    assert!(!err.contains("excludes"), "only push says so: {err}");
    let (code, out, err) = both(&a, &["status", "--json"]);
    assert!(
        code == 0
            && out.starts_with(r#"{"type":"holding","#)
            && out.contains(r#""ours":"conflict""#),
        "{out}{err}"
    );

    // --json: every line of both streams is an object with a type.
    fs::write(a.join("d/g.txt"), "g2").unwrap();
    let (code, out, err) = both(&a, &["--json", "push", "../c"]);
    assert_eq!(code, 0, "{out}{err}");
    let types = |text: &str| -> Vec<String> {
        let kind = |line| {
            let value: serde_json::Value = serde_json::from_str(line).expect(line);
            value["type"].as_str().expect(line).to_string()
        };
        text.lines().map(kind).collect()
    };
    assert_eq!(types(&out), ["event", "summary"], "{out}");
    assert_eq!(types(&err), ["warning", "warning"], "{err}");
    assert!(out.contains(r#""kind":"updated""#) && out.contains(r#""path":"d/g.txt""#));
    let (code, out, err) = both(&a, &["--json", "sync", "../nowhere"]);
    assert_eq!((code, out.as_str()), (1, ""));
    assert_eq!(types(&err), ["error"], "{err}");
    let (code, _, err) = both(&a, &["sync", "../nowhere"]);
    assert!(code == 1 && err.starts_with("error: "), "{err}");

    // status --all: one vault, then two; c holds only d/ of a.
    let (code, out) = run(&tmp.path().join("c"), &["status", "--all"]);
    assert!(
        code == 0 && out.contains("✓  d/g.txt") && out.contains("-  sub/"),
        "{out}"
    );
    let (code, out) = run(&a, &["status", "-a", "d", "--vs", "../c"]);
    assert_eq!(
        (code, out.as_str()),
        (0, "✓ ✓  d/g.txt  held        held\n")
    );
    let args = ["--json", "status", "--all", "--vs", "../c", "d", "sub"];
    let (code, out, err) = both(&a, &args);
    assert_eq!(code, 0, "{out}{err}");
    assert!(
        types(&out).iter().all(|t| t == "holding") && !out.is_empty(),
        "{out}"
    );
    assert!(
        out.contains(r#""ours":"held","theirs":"excluded""#),
        "{out}"
    );

    let (code, help) = run(&a, &["status", "--help"]);
    assert!(
        code == 0 && help.contains("!  incomplete  an earlier sync did not finish; sync again"),
        "{help}"
    );

    let (code, help) = run(&a, &["--help"]);
    assert_eq!(code, 0);
    for command in [
        "init", "clone", "sync", "push", "pull", "snapshot", "status", "get", "move", "blame",
        "log", "restore", "config",
    ] {
        assert!(
            help.lines().any(|l| l.trim_start().starts_with(command)),
            "{command} is missing from --help"
        );
    }
}
