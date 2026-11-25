# Remote vaults: encrypted, hash-addressed stores

Status: built. Milestone 7 ran against S3Mock and Cloudflare R2; GCS and Azure are untested (`SPEC.md` section 17). This is the specification of stores. [`SPEC.md`](SPEC.md) summarises it in section 8 ("Stores") and section 10 ("A run with a remote vault") and lists it as milestone 9; where they disagree, this document wins. `SPEC.md` and [`CRDT.md`](CRDT.md) govern everything else.

Serves stories C.2 and D.2 and requirements `R16`, `R24` and `R28` of [`PRD.md`](PRD.md): a content-addressed store a vault can send to, that vaults can sync through, encrypted where it is kept.

Read `SPEC.md` sections 1, 5, 8, 9 and 10 first, and `CRDT.md` "Segments" and "Validation". Terms used here (vault, segment, entry, blob, include, present, dirstate, nickname) are defined in the `SPEC.md` glossary.

## 1. The idea

There is one kind of thing: a vault. A **store** is not a second kind of thing. It is a vault with two properties, each independent of the other:

- **Bare.** It has no working folder. The content of its current versions lives in `blobs/`, where an ordinary vault keeps only history.
- **Remote.** Its log, blobs and config sit on untrusted object storage, so they are encrypted and reached through an object API, not the filesystem.

In this version a vault reached by a URL is both bare and remote, and a vault reached by a path is neither. The code keeps the two properties apart (a flag, and the backend a vault is stored on, section 4) so that neither leaks into the other.

Because a store is a vault, `sync`, `push`, `pull`, `move`, `get`, `clone`, `status --vs`, `drop --check`, `compact` and `config` mean what they already mean. There is no automatic upload, no `flush`, and no `sync` without a target.

Every difference in behaviour must follow from one of the two properties:

| Behaviour | Follows from |
|---|---|
| Nothing is scanned; no dirstate file; no `.syncitignore` | bare: these describe a working folder |
| It never records an entry | bare: entries come from scanning and merging files |
| "Present" means the blob is stored (section 7) | bare |
| Content and names are encrypted | remote: its backend is a sealed one (section 4) |
| No lock | remote: object storage has none. Safe because log and blob objects are immutable and named by hash |
| A secret key is needed | remote |

Everything else is the same as for any vault: a `config` with `nickname`, `include`, `exclude`, `keep_history` and `autocompact`; a log; blobs; validation of every segment (`CRDT.md`, "Validation") and blob received. The log format does not change.

**Out of scope.** Key rotation. Signed segments. Reading `~/.aws/credentials` (credentials come from `credentials.toml` and the environment, section 5). Parallel transfers. A bare vault on a plain local path.

## 2. Dependencies

```toml
[features]
default = ["remote", "keychain"]
remote = [
    "dep:object_store", "dep:tokio", "dep:futures", "dep:url",
    "dep:chacha20poly1305", "dep:aead-stream", "dep:hkdf", "dep:getrandom",
    "dep:zeroize", "dep:bip39", "dep:rpassword", "dep:toml_edit", "dep:aes-siv",
]
keychain = [
    "remote", "dep:keyring-core",
    "dep:apple-native-keyring-store", "dep:zbus-secret-service-keyring-store",
]

[dependencies]
object_store = { version = "0.14", features = ["aws", "gcp", "azure"], optional = true }
tokio = { version = "1", features = ["rt"], optional = true }
futures = { version = "0.3", optional = true }
url = { version = "2", optional = true }
chacha20poly1305 = { version = "0.11", optional = true }
aead-stream = { version = "0.6", features = ["alloc"], optional = true }
hkdf = { version = "0.13", optional = true }
aes-siv = { version = "0.8", optional = true }
getrandom = { version = "0.4", optional = true }
zeroize = { version = "1", optional = true }
bip39 = { version = "3", features = ["zeroize"], optional = true }
rpassword = { version = "7", optional = true }
toml_edit = { version = "0.25", optional = true }
keyring-core = { version = "1", optional = true }

[target.'cfg(target_os = "macos")'.dependencies]
apple-native-keyring-store = { version = "1", features = ["keychain"], optional = true }

[target.'cfg(target_os = "linux")'.dependencies]
zbus-secret-service-keyring-store = { version = "1", features = ["crypto-rust"], optional = true }
```

These are added to the dependencies the crate has today. Everything this document adds is optional, behind two Cargo features that are both on by default:

| Feature | Adds | Without it |
|---|---|---|
| `remote` | remote vaults: `remote.rs`, `sealed.rs`, `crypt.rs`, `keys.rs`, `init <url>`, URL targets | the build has today's dependencies. A URL target, or a nickname that names one, fails with `this build has no remote support: install with the remote feature` |
| `keychain` | the OS keychain as a place for the key. Needs `remote` | the keychain is left out of the search of section 8, and a key is saved only in `secrets.toml`. `SYNCIT_SECRET_KEY` and `secrets.toml` work as specified |

Rules for the features:

- `cargo install` gives the whole tool. `--no-default-features` gives the small one, and `--no-default-features --features remote` a build for a machine with no keychain.
- `Store`, the `Backend` trait, `Fs`, `config.remotes`, the `remote` commands and targets that are paths or nicknames of paths are **not** behind a feature. The config format is the same in every build, so a vault made by one build opens in another.
- The gates are few and at the edges: the bucket and sealed backends and the two modules beside them, the place a URL target is opened, `init <url>`, and the keychain entry in the key search. No `#[cfg]` inside the run, the log or the blob code.
- There is no feature for each cloud. S3, GCS and Azure share nearly all their dependencies.

Measured on 2026-11-22, as packages in the whole tree for a Linux build: 38 today; 130 with `remote` limited to `file://`; 189 with the keychain; 231 with S3, and the same with GCS and Azure. S3 brings the only part that compiles C, the TLS provider.

These were tried on 2026-11-22 in a throwaway crate, on macOS with Rust 1.96: the list above resolved and built, and small programs ran the stream cipher of section 6, a phrase round trip, the mock keychain, and a `file://` store with a multipart upload. What that established:

- **Stream cipher.** `aead_stream::{EncryptorBE32, DecryptorBE32}` over `chacha20poly1305::XChaCha20Poly1305`, built with `new(key.into(), (&nonce).into())` from a `[u8; 32]` key and a `[u8; 19]` nonce, with `encrypt_next` / `encrypt_last` and `decrypt_next` / `decrypt_last` on `&[u8]`. A wrong key, a changed byte, a cut at a chunk boundary and a short nonce all return errors.
- **Key derivation.** `Hkdf::<Sha256>::from_prk(&key)` then `expand(info, &mut out)`, with `sha2` 0.11.
- **Name cipher** (added with the backends, 2026-11-25). `aes_siv::siv::Aes256Siv::new(key.into())` from a `[u8; 64]` key, then `encrypt(headers, plaintext)` and `decrypt(headers, sealed)` with no headers. The output is the plaintext's length plus 16 bytes, and a plaintext shorter than 16 bytes works. `aes-siv` 0.8 uses the `aead` 0.6 and `cipher` 0.5 the stream cipher already brings. Its README says it has had no security audit.
- **Random bytes.** `getrandom::fill(&mut buf)`.
- **Phrase.** `Mnemonic::from_entropy(&key)`, `to_string()` (24 words), `Mnemonic::parse_in(Language::English, s)`, `to_entropy()`. A misspelt word is `Error::UnknownWord(index)`, counted from 0; a wrong word from the list is `Error::InvalidChecksum`.
- **Keychain.** `keyring_core::set_default_store(store)`, with `keyring_core::mock::Store::new()?` in tests and `apple_native_keyring_store::keychain::Store::new()?` on macOS. `Entry::new(service, user)?`, `get_password()`, `set_password()`. A missing item is `Error::NoEntry`. Two entries with the same service and user see the same item.
- **Object store.** `object_store::parse_url_opts(&url, options)` returns the backend and the path inside it. Option names are matched without regard to case and unknown ones are ignored, so the environment can be passed as it is. `get`, `put`, `head`, `delete` and `put_multipart` come from `ObjectStoreExt`; `list` is on `ObjectStore`. `WriteMultipart::new_with_chunk_size`, `wait_for_capacity`, `write` and `finish` upload in parts, on `file://` too. A current-thread tokio runtime with `block_on` drives all of it.
- **`file://` is rooted at `/`.** The backend for a `file://` URL is the whole filesystem and the returned path is the directory. A listing of a directory that does not exist is empty, not an error, and a `put` creates it. So the rule that syncit never creates a directory needs its own check that the directory exists, before anything is written.
- **Listed names are full paths.** Each `location` in a listing starts with the store's path; strip it.
- **Rust version.** No package in the resolved tree declares a minimum above 1.89. It was not built with 1.89 itself.
- **Size.** The tree is about 290 packages, against a handful today, and the TLS provider compiles C. The first build takes minutes.

Not tried: the Linux keychain crate (it resolved, but was not compiled); any cloud backend against a real service; `rpassword`. `object_store` does not read `~/.aws/credentials`.

If an API differs from this when building, adapt the code and record it in `SPEC.md` section 17.

## 3. Modules

| File | Change |
|---|---|
| `src/backend.rs` | the `Backend` trait, bytes at paths, and `Fs`, a vault folder's `.syncit/` |
| `src/storage.rs` | `Store`: a vault's config, log and blobs, and where each is on its backend |
| `src/remote.rs` | `Bucket`, the object-storage backend, and the credentials for it |
| `src/sealed.rs` | `Sealed`, a backend that encrypts names and contents on top of another, and the `format` file |
| `src/crypt.rs` | key derivation, name encryption, stream encryption. No I/O beyond `Read` and `Write` |
| `src/keys.rs` (new) | phrase and key; keychain, environment and `secrets.toml` |
| `src/vault.rs` | `Vault` holds `storage: Store`, `bare: bool` and a display `name`; opening by target |
| `src/log.rs`, `src/blobs.rs`, `src/scan.rs`, `src/materialise.rs`, `src/commands.rs` | every read or write of a vault's log, blobs or config goes through `v.storage` |
| `src/config.rs` | `remotes` |
| `src/output.rs` | `Context.secret_key` |
| `src/main.rs` | target arguments become strings; `init <url>`; `remote`; `--only` on `clone`; `--from` on `get`; `--vault` on `config` and `compact` |

## 4. Storage: one store, on a backend

A vault's replicated state is its config, its log and its blobs. There are two layers, and each knows nothing of the other's business.

**A backend is bytes at paths.** It knows nothing about vaults.

```rust
pub trait Backend {
    /// The bytes at `path`; None if nothing is there.
    fn read(&self, path: &str) -> Result<Option<Vec<u8>>>;
    /// Whole or not at all; replaces what was there.
    fn write(&self, path: &str, bytes: &[u8]) -> Result<()>;
    /// Copy what is at `path` to the local file `dest`. False if nothing is
    /// there, and `dest` is then not made.
    fn copy_to(&self, path: &str, dest: &Path) -> Result<bool>;
    /// Move the local file `src` to `path`. `src` is gone afterwards.
    fn move_to(&self, src: &Path, path: &str) -> Result<()>;
    /// Not an error if nothing is there.
    fn remove(&self, path: &str) -> Result<()>;
    /// The size of what is at `path`; None if nothing is there.
    fn size(&self, path: &str) -> Result<Option<u64>>;
    /// Every file under `dir`, by its path relative to `dir`.
    fn list(&self, dir: &str) -> Result<BTreeSet<String>>;
}
```

Paths are relative, `/`-separated, and name files only: a folder exists while it holds a file. `copy_to` and `move_to` are there because a blob can be large: it goes from file to file and is never held in memory. `list` gives names and not sizes, because a folder can list its names without looking at a file; `size` is one look at one file, or one request.

| Backend | Is |
|---|---|
| `Fs` | a vault folder's `.syncit/`. `write` goes through `.syncit/tmp/` and a rename; `move_to` is a rename. A path that would leave `.syncit/` is refused. Removing a file removes the folders it leaves empty, except those directly in `.syncit/` |
| `Bucket` | a prefix in object storage (section 5) |
| `Sealed` | another backend, with every name and every file encrypted (section 6). It is a backend itself, so what is above it sees plaintext paths and bytes |

A read that finds something other than what was written (changed, cut short, or put there by someone else) fails with the error `Unreadable`, which is told apart from a backend that cannot be reached.

**`Store` is a vault's state on a backend.** There is one, for every vault. It holds the layout, which is the same everywhere:

```
config              the vault's config
log/<name>          a segment, named by its SHA-256
blobs/<ab>/<hash>   a blob, in a folder named by the first 2 chars of its hash
```

and these operations: `load_config`, `save_config`; `segment_names`, `read_segment`, `write_segment`; `has_blob`, `missing`, `get_blob`, `read_blob`, `put_blob`, `drop_blobs_except`. These rules hold:

- **A folder vault is `Store` on `Fs`; a remote vault is `Store` on `Sealed` on `Bucket`.** Nothing above the backend knows which. A remote is another vault with a different backend, as the idea in section 1 says.
- **Listed once.** The log and the blobs are each listed on first need and the lists are kept for the rest of the run, added to as the store writes. `has_blob` is a lookup in the list of blobs if there is one, and a look at that one blob (`size`) if not: `missing` and compaction list, and opening a remote lists, so a command that asks about a few blobs of a folder vault, as `log` does, reads no folders. A file under `blobs/` that is not at `<ab>/<hash>` is not a blob.
- **Sizes are asked for one at a time**: by `read_blob`, to refuse a blob larger than `max` before reading it, and by `drop_blobs_except`, for the bytes it reports. On a remote each is a request. `// ponytail:` take sizes from the listing if compacting a remote is too slow.
- **Verified here.** `get_blob` and `read_blob` check the content's hash, whatever the backend. A blob that is absent, `Unreadable` or wrong is not to be had: the answer is false, and the path that needs it is `pending`.
- **A segment is read as it is stored**, not validated; the caller validates it (`CRDT.md`, "Validation"). An `Unreadable` one is reported as `rejected`, as a corrupt one is.
- **A segment is the same wherever it is stored.** It is immutable and named by its hash, so any copy will do. `Log::load` is given the local vault's store when it reads a remote's log, and reads from there every segment that vault already has. Only segments new to this vault are fetched. A store on a backend that is not a folder keeps the segments it has read, so that a run that reads one twice (to load the log, then to copy it) fetches it once.
- Working files are not storage. `Vault.hints` and the file sources in `blobs.rs` stay where they are. `blobs::fetch` tries, in order: this vault's working files, this vault's storage, the peer's working files, the peer's storage.
- `Config::load` and `save` are `Store` on the vault's folder.

**Scratch files.** `Sealed` puts ciphertext on its way in or out beside the local file it was given, as `<file>.sealed`, and removes it. That file is in a vault's `.syncit/tmp` or is a `.syncit-tmp-` file of a working folder, so it is never in the system temp directory and a scan never takes it for a file of the vault.

**Not built: an encrypted vault in a folder.** It is `Store` on `Sealed` on `Fs`, and needs no new storage code: only a way to make and open one.

## 5. Layout of a remote vault

```
<url>/format                          plaintext, one line: {"check":"<64 hex>","syncit":1}
<url>/E(config)                       the vault's config
<url>/E(log)/E(<name>)                a segment
<url>/E(blobs)/E(<ab>)/E(<hash>)      a blob
```

This is the layout of section 4 on a sealed backend: `E(x)` is the name `x`, encrypted (section 6), and every file but `format` is encrypted.

- A name that the vault's key did not make is not one of the vault's files. It is left out of every listing and never touched.
- Segments and blobs are immutable and written whole. A blob that exists is not written again.
- `config` is the same JSON as `.syncit/config`, and is the one file that is overwritten. Two vaults changing a remote's settings at the same moment: the later write wins.
- `format` is canonical JSON, keys in alphabetical order. `syncit` other than 1 is refused. `check` lets a wrong key be told from a damaged store. It is the one file read before the key is known, so it is the one file `Sealed` does not encrypt.
- A prefix with objects but no `format` is not a vault and is refused, untouched.
- syncit never creates a bucket or a directory. A `file://` directory that does not exist is an error, and syncit must check for it itself (section 2).
- A remote made before names were encrypted this way (2026-11-25) cannot be read: make a new one with `init` and push to it.

**URLs.**

- Any URL `object_store::parse_url_opts` accepts: `file:///abs/dir`, `s3://bucket/prefix`, `gs://…`, `az://…`, and R2 `https://` URLs.
- Credentials and endpoints are the settings `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_DEFAULT_REGION`, `AWS_ENDPOINT_URL`, `AWS_ALLOW_HTTP`, and the `GOOGLE_*` and `AZURE_*` equivalents. They come from the user's `credentials.toml`, then the environment, which wins (`remote::credentials`). The file is in the user's syncit folder, `$XDG_CONFIG_HOME/syncit`, else `~/.config/syncit`; `main` finds it with `output::config_dir` and puts it in `Context.config_dir`, so tests never read the real one, except `cloud_remote`, which needs it. The file is a list of `[[remotes]]` entries, each a `url` prefix and its settings:

  ```toml
  [[remotes]]
  url = "s3://bucket"
  aws_access_key_id = "…"
  aws_secret_access_key = "…"
  ```

  A setting's name is passed as written; `object_store` matches names without regard to case, so `aws_access_key_id` and `AWS_ACCESS_KEY_ID` are the same, and the docs use lower case, as `~/.aws/credentials` does. `url` itself is not passed on. A prefix, less any trailing `/`, applies to a URL equal to it or that goes on after a `/`: `s3://bucket` applies to `s3://bucket/notes`, not to `s3://bucket2`. The entries that apply are taken from the shortest prefix to the longest, a later one winning; their order in the file does not matter. There is no entry for every remote. A missing file has no settings. These are errors, `corrupt credentials <path>: …`, naming an entry by its position: bad TOML; a `remotes` that is not `[[remotes]]` entries; an entry with no `url`, or one that is not a string; a setting that is not a string; two entries with the same `url`, less any trailing `/`. The user writes this file; syncit never does, and never reads a phrase from it (phrases are in `secrets.toml`, section 8). The settings are passed to `parse_url_opts` and never set in the process environment.

  Files people edit are TOML, read and written with `toml_edit`, which keeps their comments; everything syncit alone writes is JSON.

**Listing and transfers.**

- Opening a remote lists its log and its blobs once each and works from those lists for the rest of the run (section 4).
- Download: stream to the scratch file, decrypt from there.
- Upload: encrypt to the scratch file. Under 16 MiB, one `put`. Otherwise `put_multipart` with 16 MiB parts, aborted on error.
- `// ponytail:` transfers are sequential. Parallelise if a large first upload is too slow.

## 6. Encryption (`crypt.rs`)

The master key `K` is 32 random bytes from `getrandom`, one per remote vault, held in `Zeroizing`.

Everything is derived from `K` with HKDF-SHA256 `expand` (`Hkdf::from_prk(K)`):

| Value | `info` | Bytes |
|---|---|---|
| `check` in `format` | `"syncit check"` | 32 |
| key for names | `"syncit names"` | 64 |
| key for the file at `path` | `"syncit content " + path` | 32 |

`path` is the plaintext path of section 4, such as `log/<name>` or `config`.

**Names.** Each name in a path is encrypted on its own with AES-256-SIV (RFC 5297, `aes-siv`), with no associated data, and written in lowercase hex: 16 bytes longer than the name, so 160 characters for a hash.

- **Deterministic.** A name always encrypts to the same stored name, so the stored path of a segment or blob can be computed from its hash, without reading anything.
- **Keyed.** Without `K`, a stored name says nothing about the name, so the provider cannot test whether a known file is stored.
- **Reversible with `K`.** A listing decrypts back to plaintext paths. This is what lets everything above the backend work in plaintext names (section 4), and what lets a vault know which of a remote's segments it already has without fetching one.
- **Authenticated.** A stored name the key did not make does not decrypt, and is not one of the vault's files (section 5).
- The same name encrypts to the same stored name wherever it is in the tree. The provider can see that, and nothing else about it.

**Contents.** The key of a file is derived from its plaintext path, so a file copied or moved to another stored name does not decrypt.

Body of every encrypted object: a 19-byte random nonce, then XChaCha20-Poly1305 in the STREAM construction (`aead-stream`, `EncryptorBE32` and `DecryptorBE32`), plaintext in 64 KiB chunks, the last sealed with `encrypt_last`. Every chunk but the last is exactly 64 KiB. The last holds what remains: 1 byte to 64 KiB, or nothing only when the plaintext is empty, so there is always one last chunk and never an empty one after a full one. An object of `n` plaintext bytes is `19 + n + 16 × max(1, ⌈n / 65536⌉)` bytes long. The nonce is random so that a mistake elsewhere can never reuse a key and nonce with different content.

Decryption fails, without panicking, on a wrong key, a changed byte, a truncated object or a short nonce; the backend's error is then `Unreadable` (section 4). A blob's plaintext is written to a scratch file and moved into place only after the whole object has decrypted and its hash has been checked.

What the provider can still see: how many objects there are, their sizes, when they were written, and the shape of the tree: which are the config, the segments and the blobs, and how blobs fall into their 256 folders.

## 7. A bare vault

`Vault.bare` is true for a vault opened by URL. In a bare vault:

- **Scan** does nothing and records nothing.
- **Retain is how content arrives.** `blobs::retain` collects every hash in `Log::keep(keep_history, takes)` that its storage lacks. `keep` always includes the heads, so a bare vault holds the current version of every path it includes, plus `keep_history` versions back. The early return for `keep_history == Some(0)` applies only to vaults with a working folder.
- **Materialise** does nothing. It writes no files, no conflict copies and no merges.
- **The dirstate is derived, not stored.** After the log is loaded, and again after `retain`, a bare vault's rows are: for each path it includes whose winner is a live entry with a stored blob, a row with that entry's hash, `exec` and `mtime`, and its id as `base`. With this rule `move`'s arrival check, `excluded()`, `status --vs` and `drop --check` need no branch for bare vaults. In a dry run `retain` stores nothing, so a blob counts as stored if a source could supply it (`blobs::available`); a dry `move` then reports what the real one does.
- **No lock, no ignore file.** `takes(path)` is `includes(path)` and the `.syncit` and scratch-name rules.
- **Compaction** is the same code: `drop_blobs_except(Log::keep(keep_history, |_| true))`.

A remote vault created by `init` has `keep_history: null` (all) and `autocompact: false`, so by default it never drops anything. Both can be changed (section 9).

**A known race.** Compacting a remote while another vault pushes to it can drop a blob whose segment has not arrived yet. The path then reads as `pending` to others, and the pusher's next push uploads the blob again, because `retain` asks the storage what it lacks on every run. This is not guarded.

**Order of writes.** A push writes segments, then blobs, as a run between two folders does. A vault that reads the remote in between sees `pending` paths and waits.

## 8. Keys (`keys.rs`)

**The phrase.** `K` is always written as its BIP39 mnemonic: 24 English words, lowercase, single spaces. Use `Mnemonic::from_entropy`, and `parse_in(Language::English, …)` then `to_entropy`. Never `to_seed`. An unknown word is reported with its position; a wrong checksum is reported as such.

**`Context.secret_key`.** `main` reads the environment variable `SYNCIT_SECRET_KEY`, which holds a phrase, into `Context.secret_key`. The library never reads the environment for it, so tests pass a phrase in `Context`. A value that is not a valid phrase is an error in any command that needs a key; it is never passed over in silence.

**Finding the key for a URL.** Candidates, in order. A candidate is used only if its `check` equals the one in the remote's `format`:

1. `Context.secret_key`.
2. The keychain: service `syncit`, account = the URL, password = the phrase.
3. The user's `secrets.toml`: the phrase under the remote's exact URL.

If none matches: `no key for <url>: set SYNCIT_SECRET_KEY, or run syncit remote add <url>`. Only `remote add` and `clone` ever prompt.

**Keychain.** Only in a build with the `keychain` feature (section 2); without it this candidate does not exist. Set the platform store as `keyring_core`'s default once per process. `NoEntry` means "not here". Any other error means the keychain cannot be used on this machine now (a headless Linux box, macOS over SSH): go on to the next candidate, do not fail.

**`secrets.toml`.** In the user's syncit folder (section 5), beside `credentials.toml`, never in a vault, so every vault and every command on this machine finds the same keys. Mode 0600, replaced whole through a temporary file in the same folder. One entry for each remote:

```toml
[remotes]
"s3://bucket/notes" = "abandon ability … zoo"
```

syncit writes it with `toml_edit`, so entries and comments the user added by hand stay. A missing file has no keys; bad TOML, a `remotes` that is not a table or a phrase that is not a string is an error, `corrupt secrets <path>`.

**Saving a phrase**, after `init <url>`, `remote add` or `clone` has a key that matched:

- It came from `Context.secret_key`: save nothing.
- The keychain is usable (and the build has it): save it there. Already there: nothing to do.
- It came from `secrets.toml`: it is already there.
- Otherwise: with `--save-key`, write it to `secrets.toml`; on a terminal, ask `Save the key to <path>? [y/N]`; with neither, do not save, and say that `SYNCIT_SECRET_KEY` must be set for every command that uses this remote. This needs no vault, so `init <url>` run anywhere can save its key.
- With no syncit folder (no `HOME`): do not save, and say the same.

## 9. Commands

**Targets.** Wherever a command takes another vault, it now takes a target, a string resolved in this order:

1. A key of the local vault's `config.remotes`: replaced by its location.
2. A string containing `://`: a remote vault at that URL.
3. Otherwise a path, resolved as today.

A folder that shares a remote's nickname is reached as `./name`. A URL may be used directly without `remote add`, as a path may; the key is then found by URL (section 8).

**`syncit init <url>`** makes a remote vault. The prefix must be reachable and empty.

1. `K` is the key in `Context.secret_key` if set, otherwise new random bytes.
2. Write `format`, then `config`: `include: [""]`, `keep_history: null`, `autocompact: false`, `nickname` from `--nickname <name>`, default `origin`.
3. If the key was generated, print it, then save it (section 8):

```
Secret key for <url>:

  word1 word2 … word24

Write these words down and keep them safe. They are the only way to
read this vault from another computer, and they are not shown again.
```

`init` with a path is unchanged.

**`syncit remote`.** `config` gains `remotes`: a map from nickname to location, where a location is a URL or the absolute path of a vault. It is this vault's address book and nothing else reads it. `remotes` is a required field, like the others: vaults made before it are not migrated, since syncit has not shipped. A name in it is a valid nickname with no `/`, so that it cannot be taken for a path or a URL; a target's nickname that does not qualify needs `--nickname`. The nickname stays what `CRDT.md`, "Nickname", says it is: a label that no rule of the log reads.

| Command | Behaviour |
|---|---|
| `syncit remote` | List the remotes: nickname, then location. |
| `syncit remote add [--nickname <name>] [--save-key] <location>` | Remember a vault. See below. |
| `syncit remote remove <name>` | Forget it. Its keychain item and `secrets.toml` entry stay, since other vaults on this machine may use them. Nothing in the remote is touched. |

`remote add`:

1. A location with `://` is a URL, kept as given. Otherwise it is a path, kept in absolute form, and must be a vault.
2. For a URL, read `format` and find the key (section 8). If no candidate matches and there is a terminal, ask `Secret key (24 words): ` without echo and check it. With no terminal, fail.
3. Read the target's `config`. The name is `--nickname` if given, otherwise the target's own `nickname`.
4. If that name is already in `remotes`, fail and say to pass `--nickname`.
5. Save the key (section 8), then add the entry to `config.remotes`. This is last, so a failed add changes nothing.

A URL with no `format` fails with `no vault at <url>: run syncit init <url>`.

**`syncit clone <vault> <dir> [--only <path>]…`.** `<vault>` is a target: a nickname in the vault the command runs in, a URL or a path. Two forms, told apart by the flag, as for `get`:

- Without `--only`: unchanged for a path. The vault is found by walking up from it, and the new vault includes the sub-folder the path names, or everything if the path is the vault's root. A URL is always a whole vault, so the new vault includes everything.
- With `--only`, which may be given more than once: `<vault>` is a URL or the root of a vault, and the new vault includes exactly the paths given, each from that vault's root, written with `/` and validated with `entry::valid_path`. A path below a vault's root together with `--only` is an error that says to give the root. This is how part of a remote vault is cloned: `syncit clone s3://bucket/archive ~/Archive --only photos/2024 --only music`.

Either way: create the vault in `<dir>`, record the source in `remotes` under its nickname if that name is free (for a URL this is `remote add`, which finds or asks for the key), then `pull` from it. `get --from` adds more later.

**`syncit get`.** Two forms, told apart by the flag:

- `syncit get <paths>…`: unchanged. Each argument is a filesystem path inside another vault's folder, and the vault is found by walking up from it.
- `syncit get <paths>… --from <target>`: the vault is the target (a nickname, a URL or a path), and each argument is a path from that vault's root, written with `/` and validated with `entry::valid_path`. This is the only form that reaches a remote vault.

The rest is `get` as it is: include the prefixes, then pull.

**`sync`, `push`, `pull`, `move`, `status --vs`, `drop --check`** take a target. No other change: they run the same code with a bare peer (section 7). The target stays required.

**`config` and `compact`** gain `--vault <target>` and then act on that vault instead of the one in the current folder. This is how a remote's `keep-history`, `autocompact` and `nickname` are changed, and how it is compacted by hand.

## 10. Runs, dry runs and interruption

- A run is `SPEC.md` section 10 unchanged. With a bare peer, its scan and materialise steps do nothing and its retain step uploads or downloads.
- Autocompact at the end of a run applies to each vault by its own setting, as today.
- **Dry run.** Nothing is written to either vault. Segments are read and added to logs in memory, as today.
- **Interruption.** Every object is whole or absent. An interrupted push leaves the remote with some segments or blobs missing; readers see `pending` or `incomplete` and wait, and the next run sends the rest. An abandoned multipart upload costs storage until the bucket's lifecycle rule removes it; the README says to set one.
- **Unreachable.** A remote that cannot be reached, or whose key is not found, is an error, as a missing folder is.

## 11. Output

- Events for a remote vault carry its URL where a folder's path is shown.
- A bare vault produces no file events of its own. After a run, one `info` line for each vault that is remote: `sent N files, M segments to <url>` and `received …`, omitted when both counts are 0. In a dry run: `would send …`.
- The phrase appears only in the `init` message. It is never part of an error or a warning.
- Exit codes are unchanged.

## 12. Tests

No test uses the real keychain or the network. Tests pass a phrase in `Context.secret_key`, which keeps the keychain out of it; the one keychain test installs `keyring_core::mock`. Tests of remote vaults are compiled only with `remote`, and the keychain part of `key_lookup_order` only with `keychain`. Remote vaults in tests are `file://` URLs of temporary directories. Reuse the helpers in `tests/stories.rs`.

| Test | Checks |
|---|---|
| (existing suite) | passes unchanged after the storage refactor |
| `fs_backend`, `bucket_backend`, `sealed_backend` | one contract for every backend: nothing there, bytes written and replaced, a file moved in and copied out, sizes, removal, a nested listing. `Fs` also keeps the folders directly in `.syncit/`, and refuses a path that would leave it |
| `sealed_is_opaque`, `sealed_rejects`, `sealed_format` | a sealed backend stores no plaintext name or content, and another key sees none of it; a file under a name the key did not make is not listed; garbage, a file copied to another name and a truncated one are each `Unreadable` and leave nothing behind; `format` is written, read, and refused at version 2 |
| `missing_blobs` | a store on a folder: where a blob lands, what is missing, a blob that is not what its name says, compaction, and what under `blobs/` is not a blob |
| `crypt_round_trip` | empty, 1 byte, 64 KiB − 1, 64 KiB, 64 KiB + 1, 128 KiB and 1 MiB decrypt to the input, and each object has the length section 6 gives |
| `crypt_rejects` | wrong key, wrong path, a changed byte, truncation at a chunk boundary, a short nonce: each is an error and none panics |
| `names_open` | a name, long or short, encrypts to the same stored name every time, differently under another key, and opens again; a changed stored name, another key's, and a name no key made do not open |
| `phrase_round_trip` | 32 bytes to 24 words and back; a misspelt word is named by position; a swapped word is a checksum error |
| `key_lookup_order` | `Context`, mock keychain, `secrets.toml`: the first whose check matches is used; one that does not match is passed over; none gives the documented error |
| `secrets_file` | mode 0600, in a folder saving makes; two remotes give two entries; saving again replaces; comments and other keys written by hand stay; a missing file gives no key; bad TOML, a `remotes` that is not a table and a phrase that is not a string are each `corrupt secrets` |
| `credentials_match` | an entry's `url` applies to a URL equal to it or under it at a `/`, never to `s3://b2` for `s3://b`; the entries that apply come from the shortest prefix to the longest, whatever their order in the file; `url` is not among the settings; a URL no entry covers gets none |
| `credentials_errors` | a missing file gives no settings; bad TOML, a `remotes` that is not `[[remotes]]` entries, an entry with no `url` or a `url` that is not a string, a setting that is not a string, and two entries with the same `url` are each `corrupt credentials <path>`, naming the entry |
| `init_remote` | writes `format` and the encrypted `config`; prints 24 words when it generates the key and none when given one; refuses a non-empty prefix and leaves it untouched; refuses `format` version 2 |
| `remote_add` | records the target's nickname and location for a URL and for a path; a taken name needs `--nickname`; a wrong phrase is refused and `config` is unchanged; `remote remove` forgets it |
| `story_c2_sync_through_remote` | two vaults that never sync with each other converge through one remote, in either order, including a conflict; a vault with no history supplies its current files; a third machine `clone`s the URL and has everything |
| `remote_is_opaque` | no object in the remote's directory contains a known line of a file, a path, a nickname or a plaintext hash, and no name in the tree is a plaintext hash or a plaintext name |
| `remote_hostile` | a garbage object, a valid object copied to another name, and a truncated blob are rejected or left pending; objects under names the key did not make are ignored; the rest of the run completes |
| `known_segments_stay_put` | a vault that has a remote's segments syncs with it though the remote's copies cannot be read; a vault that lacks them reports them `rejected` |
| `remote_by_nickname` | `sync origin`, `sync <url>` and `sync ./origin` (a folder) each reach the right vault; `clone`, run in the vault, resolves the nickname of a remote and of a folder vault |
| `clone_only` | `clone <url> <dir> --only a --only b/c` gives a vault that includes those two paths and has only their files; the same against a folder vault's root; a later `get --from` adds a third; a path below a vault's root with `--only` is refused and nothing is created |
| `get_from_remote` | a partial vault runs `get music --from origin`; the folder arrives and stays included; nothing else is downloaded. `--from` with a folder vault's path gives the same result as the inferred form |
| `move_to_remote` | files are removed here only when the remote has their content; a remote that excludes them refuses |
| `partial_remote` | a remote whose `include` is one folder takes only that folder's blobs, and `push` warns about the rest as it does for a folder |
| `remote_history_depth` | with `config --vault origin keep-history 1` and `compact --vault origin`, older blobs are dropped, the log is whole, and current versions remain |
| `status_vs_remote` | `status --vs origin` reports the same states as against an equivalent folder vault |
| `dry_run_remote` | a dry `sync` leaves the remote and the vault byte-identical and reports what the real run then does |
| `cloud_remote` | `story_c2_sync_through_remote` against the URL in `SYNCIT_TEST_REMOTE_URL`, with the user's `credentials.toml`; skipped when it is not set |
| `no_remote_feature` | compiled only without `remote`: a URL target and `init <url>` fail with the message of section 2 and change nothing; `remote add` of a folder vault and `sync` by its nickname work |
| `cli_smoke` additions | `init <url>`, `remote`, `remote add`, `remote remove`, `sync origin`, with `SYNCIT_SECRET_KEY` set |

## 13. Milestones

Each ends with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passing, in each of the three builds of section 14 once the features exist. Do them in order; each is a commit that leaves the tool working.

1. **The storage seam.** `Vault.storage` (first a `Storage` trait with a folder implementation; since 2026-11-25 one `Store` on a `Backend`, section 4); every access to log, blobs and config through it. No behaviour changes and no new dependency. Test: the existing suite, unchanged.
2. **`crypt`.** The `remote` and `keychain` features of section 2 are declared here, with the first optional dependencies, and CI gains its second and third jobs (section 14). Tests: `crypt_round_trip`, `crypt_rejects`, `names_open`.
3. **`keys`.** Tests: `phrase_round_trip`, `key_lookup_order`, `secrets_file`.
4. **The remote backends and `init <url>`.** `Bucket`, `Sealed`, `format`, the encrypted `config`. Test: `init_remote`.
5. **Bare vaults in a run.** `Vault.bare`, the derived dirstate, targets by URL. Tests: `story_c2_sync_through_remote` (with `init` and `pull` in place of `clone`), `remote_is_opaque`, `remote_hostile`, `move_to_remote`, `partial_remote`, `dry_run_remote`, `status_vs_remote`.
6. **Nicknames and the remaining commands.** `config.remotes`, `remote`, targets by nickname, `clone <url>`, `clone --only`, `get --from`, `--vault`; the message for a build without `remote`. Tests: `no_remote_feature`, `remote_add`, `remote_by_nickname`, `clone_only`, `get_from_remote`, `remote_history_depth`, the `clone` part of `story_c2_sync_through_remote`, `cli_smoke`.
7. **A real bucket.** Run `cloud_remote` against MinIO or a real bucket and fix what differs from `file://`.
8. **Documents.** `SPEC.md`, `PRD.md` and `CRDT.md` already describe this design as not built. When it is built: in `SPEC.md`, remove the "Not built" notes in sections 8, 10 and 14, tick milestone 9, drop the "not built yet" entry in section 17 and record there anything built differently; in `PRD.md`, remove "Stores are not built yet". Add a README section: remotes, the phrase and its warning, credentials by environment variable, the macOS keychain prompt after reinstalling, the multipart lifecycle rule.

## 14. Building this

`SPEC.md`, "Working rules for the implementer", applies. In addition:

- **Done** means milestones 1 to 6 and 8 are complete: every test in section 12 except `cloud_remote` exists and passes, with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean, and CI (`.github/workflows/ci.yml`) green on Linux in all three builds.
- **Three builds are kept working.** CI runs clippy and the tests three times: with the default features, with `--no-default-features`, and with `--no-default-features --features remote`. A build that is not tested stops compiling. Run all three locally before each commit from milestone 2 on.
- **Milestone 7 needs a bucket.** If `SYNCIT_TEST_REMOTE_URL` is not set and no local S3-compatible server can be started, write `cloud_remote` so that it skips, leave milestone 7 open, and say so in the final report. Do not mark it done on the strength of `file://`.
- **One commit for each milestone**, on the current branch, with the tool working after each.
- **Milestone 1 changes no behaviour.** If an existing test has to change to make it pass, the refactor is wrong.
- **The formats in sections 5 and 6 are the contract.** Section 2 says which crate APIs were tried. If one differs, adapt the code and record it in `SPEC.md` section 17. Do not change a format to suit a crate; if it cannot be met, stop and report.
- **If the default TLS provider will not build in CI**, use the crate's `ring` variant of the same features, and record it.
- **Do not build what section 1 lists as out of scope.**
- **Never print, log or put in an error the phrase or the key**, outside the one `init` message. Tests assert on output; none may contain a real key in a failure message.
- **No test may touch the real keychain or the network** (section 12).

## 15. Known limits

- If every copy of the phrase is lost, the remote cannot be read.
- On macOS, a rebuilt binary is asked once for keychain access.
- A phrase in `secrets.toml` is plaintext on disk. That is the user's choice.
- Two machines running `init` on the same empty URL at the same moment can both write `format`; the later one wins and the other's key stops matching.
- A push from a vault that has not pulled recently downloads the remote's unseen segments to learn its log.
- A vault that keeps history collects it from a peer, as today. From a remote that is a download: a `pull` fetches the older versions `keep_history` covers as well as the current files.
