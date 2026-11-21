//! `.syncit/config`, and which paths a vault includes.

use crate::vault::write_atomic;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub syncit: u32,
    pub include: Vec<String>,
    pub exclude: Vec<String>,
    /// Keep every version this vault sees in `.syncit/blobs/`.
    pub keep_local_history: bool,
    /// A label for this vault, written into every segment it records. Not
    /// unique and not an identity: `blame` shows it, and nothing else reads it.
    pub nickname: String,
}

/// A vault's first nickname: its folder's name, made a valid nickname.
pub fn default_nickname(folder: &str) -> String {
    let dash = |c: char| {
        if c.is_whitespace() || c.is_control() {
            '-'
        } else {
            c
        }
    };
    let mut name: String = folder.chars().map(dash).collect();
    while name.len() > 64 {
        name.pop();
    }
    if name.is_empty() {
        "vault".to_string()
    } else {
        name
    }
}

/// A prefix matches a path if it is `""`, equals it, or is followed by `/`.
pub fn prefix_matches(prefix: &str, path: &str) -> bool {
    prefix.is_empty()
        || path
            .strip_prefix(prefix)
            .is_some_and(|r| r.is_empty() || r.starts_with('/'))
}

impl Config {
    pub fn new(include: String, nickname: String) -> Config {
        Config {
            syncit: 1,
            include: vec![include],
            exclude: vec![],
            keep_local_history: false,
            nickname,
        }
    }

    pub fn load(root: &Path) -> Result<Config> {
        let file = root.join(".syncit/config");
        let text =
            fs::read_to_string(&file).with_context(|| format!("cannot read {}", file.display()))?;
        serde_json::from_str(&text).with_context(|| format!("corrupt config {}", file.display()))
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)? + "\n";
        write_atomic(root, &root.join(".syncit/config"), text.as_bytes())
    }

    /// Whether the vault's filter selects the path. The longest matching
    /// prefix decides; none means excluded.
    pub fn includes(&self, path: &str) -> bool {
        let longest = |list: &[String]| {
            list.iter()
                .filter(|p| prefix_matches(p, path))
                .map(|p| p.len() as isize)
                .max()
                .unwrap_or(-1)
        };
        longest(&self.include) > longest(&self.exclude)
    }

    /// Make a path found on disk included (SPEC.md section 7, "Auto-include"):
    /// add the shortest of its prefixes that is not equal to, or an ancestor
    /// of, a prefix already listed.
    pub fn auto_include(&mut self, path: &str) {
        if self.includes(path) {
            return;
        }
        let listed = |q: &str| {
            self.include
                .iter()
                .chain(&self.exclude)
                .any(|x| prefix_matches(q, x))
        };
        let ends = [0]
            .into_iter()
            .chain(path.match_indices('/').map(|(i, _)| i))
            .chain([path.len()]);
        // Every prefix is listed only when the path itself is excluded
        // (a moved file put back): then it moves from exclude to include.
        let prefix = ends
            .map(|i| &path[..i])
            .find(|q| !listed(q))
            .unwrap_or(path);
        self.exclude.retain(|x| x != prefix);
        self.include.push(prefix.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(include: &[&str], exclude: &[&str]) -> Config {
        Config {
            include: include.iter().map(|s| s.to_string()).collect(),
            exclude: exclude.iter().map(|s| s.to_string()).collect(),
            ..Config::new(String::new(), "v".into())
        }
    }

    #[test]
    fn nickname_default() {
        use crate::segment::valid_nickname;
        assert_eq!(default_nickname("notes"), "notes");
        assert_eq!(default_nickname("My Notes\t2"), "My-Notes-2");
        assert_eq!(default_nickname(""), "vault");
        assert_eq!(default_nickname(&"é".repeat(40)), "é".repeat(32));
        for folder in ["notes", "a b", "", &"x".repeat(200), &"é".repeat(40)] {
            assert!(valid_nickname(&default_nickname(folder)), "{folder}");
        }
    }

    #[test]
    fn includes_cases() {
        let c = config(&["foo", "foo/bar/big/keep"], &["foo/bar/big"]);
        assert!(c.includes("foo"));
        assert!(c.includes("foo/x.txt"));
        assert!(c.includes("foo/bar/y.txt"));
        assert!(!c.includes("foobar"));
        assert!(!c.includes("foobar/x.txt"));
        assert!(!c.includes("other/x.txt"));
        assert!(!c.includes("foo/bar/big/x.bin"));
        assert!(!c.includes("foo/bar/big"));
        assert!(c.includes("foo/bar/big/keep/x.bin"));
        assert!(c.includes("foo/bar/bigger"));
        assert!(config(&[""], &[]).includes("anything/at/all"));
        assert!(!config(&[""], &["a"]).includes("a/b"));
        assert!(!config(&[], &[]).includes("a"));
    }

    #[test]
    fn auto_include_cases() {
        let mut c = config(&["foo/bar"], &["foo/bar/big"]);
        c.auto_include("zap/x/y.txt");
        c.auto_include("zap/x/z.txt");
        c.auto_include("foo/new.txt");
        c.auto_include("foo/bar/big/n/a.txt");
        c.auto_include("foo/bar/fine.txt");
        assert_eq!(
            c.include,
            ["foo/bar", "zap", "foo/new.txt", "foo/bar/big/n"]
        );
        c.auto_include("foo/bar/big");
        assert!(c.includes("foo/bar/big") && c.exclude.is_empty());
    }
}
