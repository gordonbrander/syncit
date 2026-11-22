//! The secret key of a remote vault (store.md, section 8): its phrase, and
//! where it is found and saved: `Options.secret_key`, the OS keychain and
//! `.syncit/secrets.env`. The phrase is never put in an error or a warning.

use crate::crypt::{self, Key};
use crate::output::{Message, Options, print_stdout, warn};
use crate::vault::unique;
use anyhow::{Context, Result, bail};
use bip39::{Language, Mnemonic};
use std::fs::{self, OpenOptions};
use std::io::{IsTerminal, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use zeroize::Zeroizing;

/// Where a key that matched came from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    Options,
    Keychain,
    SecretsEnv,
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
/// candidate that matches, of `Options.secret_key`, the keychain, and the
/// `secrets.env` of the vault at `local`.
pub fn find(opts: &Options, url: &str, check: &str, local: Option<&Path>) -> Result<(Key, Source)> {
    if let Some(phrase) = &opts.secret_key {
        let key = parse_phrase(phrase).context("SYNCIT_SECRET_KEY")?;
        if crypt::check(&key) == check {
            return Ok((key, Source::Options));
        }
    }
    if let Some(phrase) = keychain::get(url)
        && let Ok(key) = parse_phrase(&phrase)
        && crypt::check(&key) == check
    {
        return Ok((key, Source::Keychain));
    }
    for phrase in local.map(secrets).unwrap_or_default() {
        if let Ok(key) = parse_phrase(&phrase)
            && crypt::check(&key) == check
        {
            return Ok((key, Source::SecretsEnv));
        }
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
/// section 8, "Saving a phrase"). `local` is the vault the command runs in.
pub fn save(
    opts: &Options,
    url: &str,
    key: &Key,
    source: Source,
    local: Option<&Path>,
    save_key: bool,
) -> Result<()> {
    if source == Source::Options {
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
        warn(opts, message);
    };
    let Some(root) = local else {
        unsaved();
        return Ok(());
    };
    if source == Source::SecretsEnv {
        return Ok(());
    }
    if save_key || ask("Save the key to .syncit/secrets.env? [y/N] ") {
        add_secret(root, url, &words)?;
        let message = format!("saved the key for {url} in .syncit/secrets.env");
        print_stdout(&Message::Info { message }, opts.json);
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

const PREFIX: &str = "SYNCIT_SECRET_KEY=\"";

/// Every phrase in the vault's `secrets.env`.
pub fn secrets(root: &Path) -> Vec<Zeroizing<String>> {
    let text =
        Zeroizing::new(fs::read_to_string(root.join(".syncit/secrets.env")).unwrap_or_default());
    text.lines()
        .filter_map(|l| l.trim().strip_prefix(PREFIX)?.strip_suffix('"'))
        .map(|p| Zeroizing::new(p.to_string()))
        .collect()
}

/// The lines of `secrets.env` without the entry for `url`: its comment and
/// the key line after it.
fn without(text: &str, url: &str) -> String {
    let (mut out, mut skip) = (String::new(), false);
    for line in text.lines() {
        if line == format!("# {url}") {
            skip = true;
            continue;
        }
        if skip && line.starts_with(PREFIX) {
            skip = false;
            continue;
        }
        skip = false;
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn write_secrets(root: &Path, text: &str) -> Result<()> {
    let tmp = root.join(".syncit/tmp");
    fs::create_dir_all(&tmp)?;
    let tmp = tmp.join(unique());
    let dest = root.join(".syncit/secrets.env");
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(text.as_bytes())?;
    fs::rename(&tmp, &dest).with_context(|| format!("cannot write {}", dest.display()))
}

/// Add or replace the entry for `url` in the vault's `secrets.env`.
pub fn add_secret(root: &Path, url: &str, phrase: &str) -> Result<()> {
    let old =
        Zeroizing::new(fs::read_to_string(root.join(".syncit/secrets.env")).unwrap_or_default());
    let text = Zeroizing::new(without(&old, url) + &format!("# {url}\n{PREFIX}{phrase}\"\n"));
    write_secrets(root, &text)
}

/// Remove the entry for `url` from the vault's `secrets.env`, if it has one.
pub fn remove_secret(root: &Path, url: &str) -> Result<()> {
    let Ok(old) = fs::read_to_string(root.join(".syncit/secrets.env")) else {
        return Ok(());
    };
    let old = Zeroizing::new(old);
    let text = Zeroizing::new(without(&old, url));
    if *text != *old {
        write_secrets(root, &text)?;
    }
    Ok(())
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
    use tempfile::TempDir;

    fn opts(secret_key: Option<String>) -> Options {
        Options {
            json: false,
            dry_run: false,
            no_remove: false,
            secret_key,
            credentials: None,
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
        let root = tmp.path();
        fs::create_dir_all(root.join(".syncit")).unwrap();
        let url = "file:///lookup-order";
        let (a, c) = (key(1), key(3));
        let found = |opts: &Options, check: &str| find(opts, url, check, Some(root));

        add_secret(root, url, &phrase(&c)).unwrap();
        let given = opts(Some(phrase(&a).to_string()));
        assert_eq!(found(&given, &crypt::check(&a)).unwrap().1, Source::Options);
        // One that does not match is passed over.
        let (k, source) = found(&given, &crypt::check(&c)).unwrap();
        assert_eq!((*k, source), (*c, Source::SecretsEnv));
        assert_eq!(
            found(&opts(None), &crypt::check(&c)).unwrap().1,
            Source::SecretsEnv
        );

        #[cfg(feature = "keychain")]
        {
            keyring_core::set_default_store(keyring_core::mock::Store::new().unwrap());
            let b = key(2);
            assert!(keychain::set(url, &phrase(&b)));
            let (k, source) = found(&given, &crypt::check(&b)).unwrap();
            assert_eq!((*k, source), (*b, Source::Keychain));
            // The keychain comes before secrets.env.
            assert!(keychain::set(url, &phrase(&c)));
            assert_eq!(
                found(&opts(None), &crypt::check(&c)).unwrap().1,
                Source::Keychain
            );
        }

        let err = found(&given, &crypt::check(&key(9))).map(|_| ());
        assert_eq!(
            err.unwrap_err().to_string(),
            format!("no key for {url}: set SYNCIT_SECRET_KEY, or run syncit remote add {url}")
        );
        // An invalid SYNCIT_SECRET_KEY is an error, not passed over.
        assert!(found(&opts(Some("not a phrase".into())), &crypt::check(&c)).is_err());
    }

    #[test]
    fn secrets_env_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".syncit")).unwrap();
        let (one, two) = (phrase(&key(1)), phrase(&key(2)));
        add_secret(root, "s3://bucket/one", &one).unwrap();
        add_secret(root, "s3://bucket/two", &two).unwrap();
        add_secret(root, "s3://bucket/one", &one).unwrap();
        let file = root.join(".syncit/secrets.env");
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = fs::read_to_string(&file).unwrap();
        assert_eq!(
            text,
            format!(
                "# s3://bucket/two\n{PREFIX}{}\"\n# s3://bucket/one\n{PREFIX}{}\"\n",
                *two, *one
            )
        );
        remove_secret(root, "s3://bucket/one").unwrap();
        let left = secrets(root);
        assert!(left.len() == 1 && *left[0] == *two);
    }
}
