//! Layout versions, and every change from one to the next (store.md,
//! section 14). A migration works on a backend, below `Store`, so it can
//! rework anything `Store` reads; and one migration serves a vault in a
//! folder and a remote alike.
//!
//! To change the layout: write `to_N`, add its arm to `step`, and raise
//! `VERSION`. `every_step_survives_a_stop` then tests it.

use crate::backend::Backend;
use crate::format::Format;
use crate::output::{Context, Message, print_stderr};
use anyhow::{Context as _, Result, bail, ensure};

/// The layout this build reads and writes.
pub const VERSION: u32 = 2;

/// The layout version of the vault on `b`: the `syncit` of its `format`. A
/// vault made before every vault had one is at version 1.
pub fn version(b: &impl Backend) -> Result<u32> {
    Ok(Format::read(b)?.map_or(1, |format| format.syncit))
}

fn set_version(b: &impl Backend, syncit: u32) -> Result<()> {
    let check = Format::read(b)?.and_then(|format| format.check);
    Format { check, syncit }.write(b)
}

/// Mark the new vault on `b` as having this build's layout.
pub fn create(b: &impl Backend) -> Result<()> {
    set_version(b, VERSION)
}

fn newer(name: &str, version: u32) -> String {
    format!(
        "{name} is at format version {version}; this syncit reads up to {VERSION}: upgrade syncit"
    )
}

/// Refuse the vault on `b` unless it has this build's layout. `name` is the
/// vault's, for errors.
pub fn check(b: &impl Backend, name: &str) -> Result<()> {
    match version(b).with_context(|| format!("cannot read the version of {name}"))? {
        VERSION => Ok(()),
        v if v > VERSION => bail!(newer(name, v)),
        v => bail!(
            "{name} is at format version {v}; this syncit uses {VERSION}. Run: syncit migrate {name}"
        ),
    }
}

/// Bring the vault on `b` up to `VERSION`. Returns the version it was at.
/// Only this writes the version, and only after a step is done, so a run that
/// stops leaves the vault at the version whose step it was in, to be run
/// again.
pub fn migrate(b: &impl Backend, name: &str) -> Result<u32> {
    let cannot = || format!("cannot read the version of {name}");
    let from = version(b).with_context(cannot)?;
    ensure!(from <= VERSION, newer(name, from));
    let mut at = from;
    while at != VERSION {
        step(b, at).with_context(|| format!("cannot migrate {name}"))?;
        at += 1;
        set_version(b, at)?;
    }
    Ok(from)
}

/// `migrate`, for the vault a command runs in, which says so if it did
/// anything.
pub fn upgrade(ctx: &Context, b: &impl Backend, name: &str) -> Result<()> {
    if migrate(b, name)? != VERSION {
        let message = format!("migrated {name} to format version {VERSION}");
        print_stderr(ctx, &Message::Info { message });
    }
    Ok(())
}

/// The migration from version `from` to the next. Each must be safe to stop
/// at any change and run again from the top.
// ponytail: a stopped step is run again, not replayed, so each is written to
// allow that. If one cannot be, have steps return a plan of changes that the
// runner journals and replays.
fn step(b: &impl Backend, from: u32) -> Result<()> {
    match from {
        1 => to_2(b),
        v => bail!("no migration from format version {v}"),
    }
}

/// 1 to 2: the config, JSON in `config`, becomes TOML in `config.toml`,
/// which is its owner's to edit. Its version, the field `syncit`, is left
/// out: the version is in `format`.
fn to_2(b: &impl Backend) -> Result<()> {
    let Some(bytes) = b.read("config")? else {
        // Stopped after the old file went and before the version was written.
        ensure!(b.size("config.toml")?.is_some(), "it has no config");
        return Ok(());
    };
    let old: serde_json::Value = serde_json::from_slice(&bytes).context("corrupt config")?;
    let text = |v: &serde_json::Value| v.as_str().map(str::to_string).context("corrupt config");
    // The root of the vault was "", and is ".".
    let paths = |key: &str| -> Result<toml_edit::Array> {
        let list = old[key].as_array().context("corrupt config")?;
        let dotted = |path: String| if path.is_empty() { ".".into() } else { path };
        list.iter().map(|path| text(path).map(dotted)).collect()
    };
    let mut new = toml_edit::DocumentMut::new();
    new["nickname"] = toml_edit::value(text(&old["nickname"])?);
    // Every version was null, and is "all".
    new["keep-history"] = match &old["keep_history"] {
        serde_json::Value::Null => toml_edit::value("all"),
        depth => toml_edit::value(depth.as_i64().context("corrupt config")?),
    };
    new["autocompact"] = toml_edit::value(old["autocompact"].as_bool().context("corrupt config")?);
    new["include"] = toml_edit::value(paths("include")?);
    new["exclude"] = toml_edit::value(paths("exclude")?);
    let mut remotes = toml_edit::Table::new();
    // A config from before there were remotes has no `remotes`.
    for (name, at) in old["remotes"].as_object().into_iter().flatten() {
        remotes[name] = toml_edit::value(text(at)?);
    }
    new["remotes"] = toml_edit::Item::Table(remotes);
    b.write("config.toml", new.to_string().as_bytes())?;
    b.remove("config")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::Fs;
    use crate::format::FORMAT;
    use std::cell::Cell;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;
    use tempfile::TempDir;

    /// A backend that fails at its `left`th change, counting from 0.
    struct Failing<B> {
        inner: B,
        left: Cell<usize>,
    }

    impl<B> Failing<B> {
        fn change(&self) -> Result<()> {
            match self.left.get() {
                0 => bail!("stopped"),
                n => self.left.set(n - 1),
            }
            Ok(())
        }
    }

    impl<B: Backend> Backend for Failing<B> {
        fn read(&self, path: &str) -> Result<Option<Vec<u8>>> {
            self.inner.read(path)
        }
        fn write(&self, path: &str, bytes: &[u8]) -> Result<()> {
            self.change()?;
            self.inner.write(path, bytes)
        }
        fn copy_to(&self, path: &str, dest: &Path) -> Result<bool> {
            self.inner.copy_to(path, dest)
        }
        fn move_to(&self, src: &Path, path: &str) -> Result<()> {
            self.change()?;
            self.inner.move_to(src, path)
        }
        fn remove(&self, path: &str) -> Result<()> {
            self.change()?;
            self.inner.remove(path)
        }
        fn size(&self, path: &str) -> Result<Option<u64>> {
            self.inner.size(path)
        }
        fn list(&self, dir: &str) -> Result<BTreeSet<String>> {
            self.inner.list(dir)
        }
    }

    /// The config of a version 1 vault, as that version wrote it.
    const CONFIG_1: &str = r#"{
  "syncit": 1,
  "include": [
    "",
    "docs"
  ],
  "exclude": [
    "docs/tmp"
  ],
  "keep_history": null,
  "autocompact": false,
  "nickname": "laptop",
  "remotes": {
    "origin": "s3://bucket/notes"
  }
}
"#;

    /// What that config is at version 2.
    const CONFIG_2: &str = r#"nickname = "laptop"
keep-history = "all"
autocompact = false
include = [".", "docs"]
exclude = ["docs/tmp"]

[remotes]
origin = "s3://bucket/notes"
"#;

    /// A version 1 vault, written as that version wrote it.
    fn version_1(b: &impl Backend) {
        b.write("config", CONFIG_1.as_bytes()).unwrap();
        b.write("log/abc", b"a segment").unwrap();
        b.write("blobs/ab/abcd", b"a blob").unwrap();
    }

    /// Every file of the vault: path to bytes.
    fn files(b: &impl Backend) -> BTreeMap<String, Vec<u8>> {
        let paths = b.list("").unwrap().into_iter();
        paths
            .map(|path| {
                let bytes = b.read(&path).unwrap().unwrap();
                (path, bytes)
            })
            .collect()
    }

    /// The tests, for the backends `new` makes: each empty, in a folder that
    /// lives as long as what comes with it.
    fn suite<B: Backend>(new: impl Fn() -> (TempDir, B)) {
        // A vault at `version`: the version 1 vault, migrated up to it.
        let at = |version: u32| {
            let (tmp, b) = new();
            version_1(&b);
            for from in 1..version {
                step(&b, from).unwrap();
                set_version(&b, from + 1).unwrap();
            }
            (tmp, b)
        };

        // The whole way, and what version 2 is.
        let (_tmp, b) = at(1);
        assert_eq!(version(&b).unwrap(), 1);
        let unmigrated = check(&b, "v").unwrap_err().to_string();
        assert!(
            unmigrated.ends_with("Run: syncit migrate v"),
            "{unmigrated}"
        );
        assert_eq!(migrate(&b, "v").unwrap(), 1);
        assert_eq!(version(&b).unwrap(), VERSION);
        check(&b, "v").unwrap();
        let after = files(&b);
        assert_eq!(
            after[FORMAT],
            format!("{{\"syncit\":{VERSION}}}\n").as_bytes()
        );
        assert!(!after.contains_key("config"));
        let config = std::str::from_utf8(&after["config.toml"]).unwrap();
        assert_eq!(config, CONFIG_2, "every setting, and no version");
        // It is a config of this build.
        let config = crate::config::Config::parse(config).unwrap();
        assert_eq!(config.include, ["", "docs"]);
        assert_eq!(after["log/abc"], b"a segment");
        assert_eq!(after["blobs/ab/abcd"], b"a blob");
        // Again: nothing to do.
        assert_eq!(migrate(&b, "v").unwrap(), VERSION);
        assert_eq!(files(&b), after);

        // A vault from a newer build is refused and not touched.
        let (_tmp, b) = at(VERSION);
        set_version(&b, VERSION + 1).unwrap();
        let before = files(&b);
        for refused in [check(&b, "v"), migrate(&b, "v").map(|_| ())] {
            let refused = format!("{:#}", refused.unwrap_err());
            assert!(refused.contains("upgrade syncit"), "{refused}");
        }
        assert_eq!(files(&b), before);
        b.write(FORMAT, b"{\"syncit\":\"two\"}").unwrap();
        assert!(check(&b, "v").is_err() && migrate(&b, "v").is_err());

        // Stopped after its last change, before the version was written.
        let (_tmp, b) = at(1);
        step(&b, 1).unwrap();
        let done = files(&b);
        assert_eq!(migrate(&b, "v").unwrap(), 1);
        assert_eq!(files(&b), after);
        assert_eq!(done.len(), after.len() - 1, "all but the version");

        // Not a vault.
        let (_tmp, b) = new();
        assert!(migrate(&b, "v").is_err());
        assert!(files(&b).is_empty());

        // Every step survives a stop: stopped at any change and run again,
        // it leaves what it leaves when it is not stopped.
        for from in 1..VERSION {
            let (_tmp, b) = at(from);
            step(&b, from).unwrap();
            let whole = files(&b);
            for stop_at in 0.. {
                let (_tmp, inner) = at(from);
                let failing = Failing {
                    inner,
                    left: Cell::new(stop_at),
                };
                let stopped = step(&failing, from).is_err();
                step(&failing.inner, from).unwrap();
                assert_eq!(files(&failing.inner), whole, "{from}, stopped at {stop_at}");
                if !stopped {
                    assert!(stop_at > 0, "step {from} changed nothing");
                    break;
                }
            }
        }
    }

    #[test]
    fn every_step_survives_a_stop() {
        suite(|| {
            let tmp = TempDir::new().unwrap();
            std::fs::create_dir(tmp.path().join(".syncit")).unwrap();
            let b = Fs::at(tmp.path());
            (tmp, b)
        });
    }

    #[cfg(feature = "remote")]
    #[test]
    fn every_step_survives_a_stop_sealed() {
        use crate::sealed::Sealed;
        suite(|| {
            let tmp = TempDir::new().unwrap();
            std::fs::create_dir(tmp.path().join(".syncit")).unwrap();
            let key = zeroize::Zeroizing::new([7; 32]);
            let b = Sealed::new(Fs::at(tmp.path()), key);
            (tmp, b)
        });
    }
}
