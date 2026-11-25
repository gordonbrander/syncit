//! The secret key of a remote vault (store.md, section 8): its phrase, and
//! where it is found and saved: `Context.secret_key`, the OS keychain and
//! the user's `secrets.toml`. The phrase is never put in an error or a
//! warning.

use crate::crypt::{self, Key};
use crate::output::{Context, Message, print_stdout, warn};
use crate::vault::unique;
use anyhow::{Context as _, Result, bail};
use bip39::{Language, Mnemonic};
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use zeroize::Zeroizing;

/// Where a key that matched came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Context,
    Keychain,
    /// The user's `secrets.toml`.
    Secrets,
    /// Generated, or typed in at a prompt.
    New,
}

/// The key as its 24 words.
pub fn phrase(key: &Key) -> Zeroizing<String> {
    let mnemonic = Mnemonic::from_entropy(&key[..]).expect("32 bytes is valid entropy");
    Zeroizing::new(mnemonic.to_string())
}

/// The key a phrase stands for. Errors name a word by its position only.
pub fn parse_phrase(s: &str) -> Result<Key> {
    let words = Zeroizing::new(s.split_whitespace().collect::<Vec<_>>().join(" "));
    let words = Zeroizing::new(words.to_lowercase());
    let mnemonic = match Mnemonic::parse_in(Language::English, words.as_str()) {
        Ok(m) => m,
        Err(bip39::Error::UnknownWord(i)) => {
            bail!("word {} of the secret key is not in the word list", i + 1)
        }
        Err(bip39::Error::InvalidChecksum) => {
            bail!("the secret key's words do not match their checksum: one is wrong")
        }
        Err(_) => bail!("a secret key is 24 words"),
    };
    let entropy = Zeroizing::new(mnemonic.to_entropy());
    let bytes: [u8; 32] = entropy[..]
        .try_into()
        .ok()
        .context("a secret key is 24 words")?;
    Ok(Zeroizing::new(bytes))
}

fn no_key(url: &str) -> anyhow::Error {
    anyhow::anyhow!("no key for {url}: set SYNCIT_SECRET_KEY, or run syncit remote add {url}")
}

/// The key for the remote at `url` whose `format` has `check`: the first
/// candidate that matches, of `Context.secret_key`, the keychain, and the
/// user's `secrets.toml`.
pub fn find(ctx: &Context, url: &str, check: &str) -> Result<(Key, Source)> {
    if let Some(phrase) = &ctx.secret_key {
        let key = parse_phrase(phrase).context("SYNCIT_SECRET_KEY")?;
        if crypt::check(&key) == check {
            return Ok((key, Source::Context));
        }
    }
    if let Some(phrase) = keychain::get(url)
        && let Ok(key) = parse_phrase(&phrase)
        && crypt::check(&key) == check
    {
        return Ok((key, Source::Keychain));
    }
    if let Some(phrase) = saved(ctx, url)?
        && let Ok(key) = parse_phrase(&phrase)
        && crypt::check(&key) == check
    {
        return Ok((key, Source::Secrets));
    }
    Err(no_key(url))
}

/// Ask for the phrase on the terminal, without echo, and check it. Fails
/// with no terminal, as with no key.
pub fn prompt(url: &str, check: &str) -> Result<Key> {
    if !std::io::stdin().is_terminal() {
        return Err(no_key(url));
    }
    let typed = Zeroizing::new(rpassword::prompt_password("Secret key (24 words): ")?);
    let key = parse_phrase(&typed)?;
    if crypt::check(&key) != check {
        bail!("that is not the key of {url}");
    }
    Ok(key)
}

/// Keep a key that matched where later commands find it (store.md,
/// section 8, "Saving a phrase").
pub fn save(ctx: &Context, url: &str, key: &Key, source: Source, save_key: bool) -> Result<()> {
    if source == Source::Context {
        return Ok(());
    }
    let words = phrase(key);
    if keychain::get(url).is_some_and(|w| *w == *words) || keychain::set(url, &words) {
        return Ok(());
    }
    let unsaved = || {
        let message = format!(
            "the key for {url} is not saved: set SYNCIT_SECRET_KEY for every command that uses this remote"
        );
        warn(ctx, message);
    };
    if source == Source::Secrets {
        return Ok(());
    }
    let Some(file) = secrets_path(ctx) else {
        unsaved();
        return Ok(());
    };
    if save_key || ask(&format!("Save the key to {}? [y/N] ", file.display())) {
        add_saved(ctx, url, &words)?;
        let message = format!("saved the key for {url} in {}", file.display());
        print_stdout(ctx, &Message::Info { message });
    } else {
        unsaved();
    }
    Ok(())
}

/// A yes from the terminal; no without one.
fn ask(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!("{question}");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

/// The user's `secrets.toml`: a phrase for each remote URL.
fn secrets_path(ctx: &Context) -> Option<PathBuf> {
    Some(ctx.config_dir.as_ref()?.join("secrets.toml"))
}

/// The phrase saved for `url` in `secrets.toml`, if there is one.
pub fn saved(ctx: &Context, url: &str) -> Result<Option<Zeroizing<String>>> {
    let Some(file) = secrets_path(ctx) else {
        return Ok(None);
    };
    let doc = crate::remote::read_toml(&file, "secrets")?;
    let corrupt = |why: &str| format!("corrupt secrets {}: {why}", file.display());
    let Some(remotes) = doc.get("remotes") else {
        return Ok(None);
    };
    let remotes = remotes
        .as_table_like()
        .with_context(|| corrupt("remotes is not a table"))?;
    let Some(item) = remotes.get(url) else {
        return Ok(None);
    };
    let phrase = item
        .as_str()
        .with_context(|| corrupt(&format!("{url} is not a string")))?;
    Ok(Some(Zeroizing::new(phrase.to_string())))
}

/// Save `phrase` for `url` in `secrets.toml`, keeping everything else in the
/// file, comments too. The file is replaced whole and readable by its owner
/// only.
pub fn add_saved(ctx: &Context, url: &str, phrase: &str) -> Result<()> {
    let file = secrets_path(ctx).context("no folder for secrets.toml: set HOME")?;
    let mut doc = crate::remote::read_toml(&file, "secrets")?;
    let remotes = doc
        .entry("remotes")
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .with_context(|| format!("corrupt secrets {}: remotes is not a table", file.display()))?;
    remotes.insert(url, toml_edit::value(phrase));
    let dir = file.parent().context("secrets.toml has no folder")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(unique());
    let text = Zeroizing::new(doc.to_string());
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    out.write_all(text.as_bytes())?;
    fs::rename(&tmp, &file).with_context(|| format!("cannot write {}", file.display()))
}

/// The OS keychain: service `syncit`, account the URL, password the phrase.
/// Any error but "no entry" means it cannot be used here now, which is not
/// an error.
#[cfg(feature = "keychain")]
mod keychain {
    use keyring_core::Entry;
    use zeroize::Zeroizing;

    /// Set the platform's store as the default, once, unless one is set
    /// (tests set a mock). Whether there is one.
    fn ready() -> bool {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            if keyring_core::get_default_store().is_some() {
                return;
            }
            #[cfg(target_os = "macos")]
            if let Ok(store) = apple_native_keyring_store::keychain::Store::new() {
                keyring_core::set_default_store(store);
            }
            #[cfg(target_os = "linux")]
            if let Ok(store) = zbus_secret_service_keyring_store::Store::new() {
                keyring_core::set_default_store(store);
            }
        });
        keyring_core::get_default_store().is_some()
    }

    fn entry(url: &str) -> Option<Entry> {
        ready().then(|| Entry::new("syncit", url).ok()).flatten()
    }

    pub fn get(url: &str) -> Option<Zeroizing<String>> {
        entry(url)?.get_password().ok().map(Zeroizing::new)
    }

    /// Whether the phrase was saved.
    pub fn set(url: &str, phrase: &str) -> bool {
        entry(url).is_some_and(|e| e.set_password(phrase).is_ok())
    }
}

#[cfg(not(feature = "keychain"))]
mod keychain {
    use zeroize::Zeroizing;

    pub fn get(_url: &str) -> Option<Zeroizing<String>> {
        None
    }

    pub fn set(_url: &str, _phrase: &str) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use tempfile::TempDir;

    /// Context with a phrase, whose syncit folder is `dir`.
    fn ctx(secret_key: Option<String>, dir: &Path) -> Context {
        Context {
            json: false,
            dry_run: false,
            no_remove: false,
            secret_key,
            bar: crate::output::bar(false),
            config_dir: Some(dir.to_path_buf()),
            ours: None,
        }
    }

    fn key(byte: u8) -> Key {
        Zeroizing::new([byte; 32])
    }

    #[test]
    fn phrase_round_trip() {
        let k = key(42);
        let words = phrase(&k);
        assert_eq!(words.split(' ').count(), 24);
        assert_eq!(words.to_lowercase(), *words);
        assert_eq!(*parse_phrase(&words).unwrap(), *k);
        assert_eq!(
            *parse_phrase(&format!("  {}  ", words.to_uppercase())).unwrap(),
            *k
        );

        let mut list: Vec<&str> = words.split(' ').collect();
        list[4] = "notaword";
        let err = format!("{:#}", parse_phrase(&list.join(" ")).unwrap_err());
        assert!(err.contains("word 5 "), "{err}");
        assert!(!err.contains("notaword"), "{err}");

        let mut list: Vec<&str> = words.split(' ').collect();
        list.swap(0, 1);
        if list[0] != list[1] {
            let err = format!("{:#}", parse_phrase(&list.join(" ")).unwrap_err());
            assert!(err.contains("checksum"), "{err}");
        }
        assert!(parse_phrase("abandon ability").is_err());
    }

    #[test]
    fn key_lookup_order() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        let url = "file:///lookup-order";
        let (a, c) = (key(1), key(3));
        let found = |ctx: &Context, check: &str| find(ctx, url, check);

        add_saved(&ctx(None, dir), url, &phrase(&c)).unwrap();
        let given = ctx(Some(phrase(&a).to_string()), dir);
        assert_eq!(found(&given, &crypt::check(&a)).unwrap().1, Source::Context);
        // One that does not match is passed over.
        let (k, source) = found(&given, &crypt::check(&c)).unwrap();
        assert_eq!((*k, source), (*c, Source::Secrets));
        assert_eq!(
            found(&ctx(None, dir), &crypt::check(&c)).unwrap().1,
            Source::Secrets
        );

        #[cfg(feature = "keychain")]
        {
            keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
            let b = key(2);
            assert!(keychain::set(url, &phrase(&b)));
            let (k, source) = found(&given, &crypt::check(&b)).unwrap();
            assert_eq!((*k, source), (*b, Source::Keychain));
            // The keychain comes before secrets.toml.
            assert!(keychain::set(url, &phrase(&c)));
            assert_eq!(
                found(&ctx(None, dir), &crypt::check(&c)).unwrap().1,
                Source::Keychain
            );
        }

        let err = found(&given, &crypt::check(&key(9))).map(|_| ());
        assert_eq!(
            err.unwrap_err().to_string(),
            format!("no key for {url}: set SYNCIT_SECRET_KEY, or run syncit remote add {url}")
        );
        // An invalid SYNCIT_SECRET_KEY is an error, not passed over.
        let bad = ctx(Some("not a phrase".into()), dir);
        assert!(found(&bad, &crypt::check(&c)).is_err());
    }

    #[test]
    fn secrets_file() {
        let tmp = TempDir::new().unwrap();
        // The folder does not exist yet: saving makes it.
        let dir = tmp.path().join("syncit");
        let o = ctx(None, &dir);
        assert!(saved(&o, "s3://bucket/one").unwrap().is_none());
        let (one, two) = (phrase(&key(1)), phrase(&key(2)));
        add_saved(&o, "s3://bucket/one", &two).unwrap();
        add_saved(&o, "s3://bucket/two", &two).unwrap();
        add_saved(&o, "s3://bucket/one", &one).unwrap();
        let file = dir.join("secrets.toml");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(*saved(&o, "s3://bucket/one").unwrap().unwrap(), *one);
        assert_eq!(*saved(&o, "s3://bucket/two").unwrap().unwrap(), *two);
        assert!(saved(&o, "s3://bucket").unwrap().is_none());

        // What the user wrote by hand stays.
        let text = format!(
            "# mine\nother = 1\n\n[remotes]\n# work\n\"s3://w\" = \"{}\"\n",
            *one
        );
        fs::write(&file, text).unwrap();
        add_saved(&o, "s3://bucket/two", &two).unwrap();
        let after = fs::read_to_string(&file).unwrap();
        assert!(after.starts_with("# mine\nother = 1\n"), "{after}");
        assert!(after.contains("# work\n\"s3://w\""), "{after}");
        assert_eq!(*saved(&o, "s3://w").unwrap().unwrap(), *one);
        assert_eq!(*saved(&o, "s3://bucket/two").unwrap().unwrap(), *two);

        for text in ["not toml", "remotes = 1", "[remotes]\n\"s3://w\" = 1"] {
            fs::write(&file, text).unwrap();
            let err = saved(&o, "s3://w").unwrap_err().to_string();
            assert!(err.starts_with("corrupt secrets "), "{text}: {err}");
        }
    }
}
