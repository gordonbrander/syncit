//! `.syncit/config.toml`, and which paths a vault includes.

use crate::backend::Fs;
use crate::entry::valid_path;
use crate::segment::valid_nickname;
use crate::storage::Store;
use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, Serializer};
use std::collections::BTreeMap;
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Value};

/// A vault's settings. The file is the user's to edit: `to_toml` keeps what
/// they wrote around the values, and `parse` says what is wrong with it.
/// Serialized, it is what `config --json` prints: the values the file shows.
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    /// Path prefixes; `""`, written `.`, is the vault's root.
    #[serde(serialize_with = "dotted")]
    pub include: Vec<String>,
    #[serde(serialize_with = "dotted")]
    pub exclude: Vec<String>,
    /// How many versions back from a head this vault keeps content for in
    /// `.syncit/blobs/`. 0 keeps no history; `None`, written `"all"`, keeps
    /// every version.
    #[serde(serialize_with = "depth")]
    pub keep_history: Option<u32>,
    /// Drop the stored versions `keep_history` no longer covers at the end
    /// of every run. Off, only `syncit compact` drops them.
    pub autocompact: bool,
    /// A label for this vault, written into every segment it records. Not
    /// unique and not an identity: `blame` shows it, and nothing else reads it.
    pub nickname: String,
    /// This vault's address book: nickname to location, a URL or the
    /// absolute path of a vault. Nothing else reads it.
    pub remotes: BTreeMap<String, String>,
}

/// A prefix as the file has it: the root is `.`.
fn dot(prefix: &str) -> &str {
    if prefix.is_empty() { "." } else { prefix }
}

fn dotted<S: Serializer>(list: &[String], s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(list.iter().map(|p| dot(p)))
}

fn depth<S: Serializer>(depth: &Option<u32>, s: S) -> Result<S::Ok, S::Error> {
    match depth {
        Some(n) => s.serialize_u32(*n),
        None => s.serialize_str("all"),
    }
}

/// Set `key` to `new`, keeping the comment written after the old value.
fn set(doc: &mut DocumentMut, key: &str, new: impl Into<Value>) {
    let mut new = new.into();
    if let Some(old) = doc.get(key).and_then(Item::as_value) {
        *new.decor_mut() = old.decor().clone();
    }
    doc[key] = Item::Value(new);
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
            include: vec![include],
            exclude: vec![],
            keep_history: Some(1),
            autocompact: true,
            nickname,
            remotes: BTreeMap::new(),
        }
    }

    /// Read a config from its TOML. Every key is required and no other is
    /// allowed, so a misspelt key is an error and not a setting ignored.
    pub fn parse(text: &str) -> Result<Config> {
        let doc: DocumentMut = text.parse()?;
        const KEYS: [&str; 6] = [
            "nickname",
            "keep-history",
            "autocompact",
            "include",
            "exclude",
            "remotes",
        ];
        for (key, _) in doc.iter() {
            ensure!(KEYS.contains(&key), "unknown key {key}");
        }
        let get = |key: &str| doc.get(key).with_context(|| format!("{key} is missing"));
        let paths = |key: &str| -> Result<Vec<String>> {
            let list = get(key)?.as_array();
            let list = list.with_context(|| format!("{key} is not a list of paths"))?;
            let path = |v: &Value| match v.as_str() {
                Some(".") => Ok(String::new()),
                Some(p) if valid_path(p) => Ok(p.to_string()),
                Some(p) => bail!("{key}: {p:?} is not a path a vault can have"),
                None => bail!("{key}: {} is not a path", v.to_string().trim()),
            };
            list.iter().map(path).collect()
        };
        let keep = get("keep-history")?;
        let keep_history = match (keep.as_integer(), keep.as_str()) {
            (Some(n), _) => Some(
                u32::try_from(n)
                    .ok()
                    .context("keep-history is out of range")?,
            ),
            (_, Some("all")) => None,
            _ => bail!("keep-history is not a number or \"all\""),
        };
        let nickname = get("nickname")?
            .as_str()
            .context("nickname is not a string")?;
        ensure!(
            valid_nickname(nickname),
            "nickname is not 1 to 64 bytes with no spaces or control characters"
        );
        let remotes = get("remotes")?.as_table_like();
        let remote = |(name, at): (&str, &Item)| -> Result<(String, String)> {
            let at = at.as_str();
            let at = at.with_context(|| format!("remotes: {name} is not a string"))?;
            Ok((name.to_string(), at.to_string()))
        };
        Ok(Config {
            include: paths("include")?,
            exclude: paths("exclude")?,
            keep_history,
            autocompact: get("autocompact")?
                .as_bool()
                .context("autocompact is not true or false")?,
            nickname: nickname.to_string(),
            remotes: remotes
                .context("remotes is not a table")?
                .iter()
                .map(remote)
                .collect::<Result<_>>()?,
        })
    }

    /// The config as TOML. With the text of the file it replaces, only the
    /// values that differ are rewritten: comments, order and layout stay.
    pub fn to_toml(&self, existing: Option<&str>) -> String {
        let old = existing.and_then(|text| Config::parse(text).ok());
        let mut doc: DocumentMut = match (&old, existing) {
            (Some(_), Some(text)) => text.parse().unwrap_or_default(),
            _ => DocumentMut::new(),
        };
        let old = old.as_ref();
        let list = |paths: &[String]| paths.iter().map(|p| dot(p)).collect::<Array>();
        if old.map(|o| &o.nickname) != Some(&self.nickname) {
            set(&mut doc, "nickname", self.nickname.as_str());
        }
        if old.map(|o| o.keep_history) != Some(self.keep_history) {
            match self.keep_history {
                Some(n) => set(&mut doc, "keep-history", i64::from(n)),
                None => set(&mut doc, "keep-history", "all"),
            }
        }
        if old.map(|o| o.autocompact) != Some(self.autocompact) {
            set(&mut doc, "autocompact", self.autocompact);
        }
        if old.map(|o| &o.include) != Some(&self.include) {
            set(&mut doc, "include", list(&self.include));
        }
        if old.map(|o| &o.exclude) != Some(&self.exclude) {
            set(&mut doc, "exclude", list(&self.exclude));
        }
        if old.map(|o| &o.remotes) != Some(&self.remotes) {
            let remotes = doc.entry("remotes").or_insert_with(toml_edit::table);
            if let Some(remotes) = remotes.as_table_like_mut() {
                let gone: Vec<String> = remotes
                    .iter()
                    .map(|(name, _)| name.to_string())
                    .filter(|name| !self.remotes.contains_key(name))
                    .collect();
                gone.iter().for_each(|name| drop(remotes.remove(name)));
                for (name, at) in &self.remotes {
                    if remotes.get(name).and_then(Item::as_str) != Some(at) {
                        remotes.insert(name, toml_edit::value(at));
                    }
                }
            }
        }
        doc.to_string()
    }

    /// The config of the vault in the folder `root`, which is not locked.
    pub fn load(root: &Path) -> Result<Config> {
        Store::open(&root.display().to_string(), Fs::at(root))?.load_config()
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        Store::open(&root.display().to_string(), Fs::at(root))?.save_config(self)
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
    fn toml_cases() {
        let mut c = config(&["", "foo/bar"], &["foo/bar/big"]);
        c.keep_history = None;
        c.nickname = "a".into();
        c.remotes.insert("origin".into(), "s3://bucket/a".into());
        let text = c.to_toml(None);
        let want = "nickname = \"a\"\nkeep-history = \"all\"\nautocompact = true\n\
                    include = [\".\", \"foo/bar\"]\nexclude = [\"foo/bar/big\"]\n\n\
                    [remotes]\norigin = \"s3://bucket/a\"\n";
        assert_eq!(text, want);
        assert_eq!(Config::parse(&text).unwrap(), c);

        // What the user wrote stays: comments, order, and the layout of a
        // value that did not change.
        let mine = "# my laptop\nautocompact = true # tidy\nkeep-history = 1 # one back\n\
                    nickname = \"a\"\ninclude = [\".\"]\nexclude = [\n  \"video\", # big\n]\n\n\
                    [remotes]\n# the drive\ndrive = \"/Volumes/Drive\"\n";
        let mut c = Config::parse(mine).unwrap();
        assert_eq!(c.to_toml(Some(mine)), mine);
        c.keep_history = Some(3);
        c.remotes.insert("origin".into(), "s3://bucket/a".into());
        let text = c.to_toml(Some(mine));
        let want = mine
            .replace("keep-history = 1 # one back", "keep-history = 3 # one back")
            .replace("Drive\"\n", "Drive\"\norigin = \"s3://bucket/a\"\n");
        assert_eq!(text, want);
        assert_eq!(Config::parse(&text).unwrap(), c);

        // What is wrong with a file is said by name.
        let bad = |from: &str, to: &str| {
            let error = Config::parse(&want.replace(from, to)).unwrap_err();
            format!("{error:#}")
        };
        assert!(bad("exclude", "exlude").contains("unknown key exlude"));
        assert!(bad("nickname = \"a\"\n", "").contains("nickname is missing"));
        assert!(bad("\"video\"", "\"../up\"").contains("\"../up\" is not a path"));
        assert!(bad("\"video\"", "7").contains("7 is not a path"));
        assert!(bad("= 3", "= \"some\"").contains("keep-history is not a number"));
        assert!(bad("\"a\"", "\"two words\"").contains("nickname is not"));
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
