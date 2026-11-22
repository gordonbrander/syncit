# Remote vaults: encrypted, hash-addressed stores

Status: to build. This is the specification of stores. [`SPEC.md`](SPEC.md) summarises it in section 8 ("Stores") and section 10 ("A run with a remote vault") and lists it as milestone 9; where they disagree, this document wins. `SPEC.md` and [`CRDT.md`](CRDT.md) govern everything else.

Serves stories C.2 and D.2 and requirements `R16`, `R24` and `R28` of [`PRD.md`](PRD.md): a content-addressed store a vault can send to, that vaults can sync through, encrypted where it is kept.

Read `SPEC.md` sections 1, 5, 8, 9 and 10 first, and `CRDT.md` "Segments" and "Validation". Terms used here (vault, segment, entry, blob, include, present, dirstate, nickname) are defined in the `SPEC.md` glossary.

## 1. The idea

There is one kind of thing: a vault. A **store** is not a second kind of thing. It is a vault with two properties, each independent of the other:

- **Bare.** It has no working folder. The content of its current versions lives in `blobs/`, where an ordinary vault keeps only history.
- **Remote.** Its log, blobs and config sit on untrusted object storage, so they are encrypted and reached through an object API, not the filesystem.

In this version a vault reached by a URL is both bare and remote, and a vault reached by a path is neither. The code keeps the two properties apart (a flag and a trait, section 4) so that neither leaks into the other.

Because a store is a vault, `sync`, `push`, `pull`, `move`, `get`, `clone`, `status --vs`, `drop --check`, `compact` and `config` mean what they already mean. There is no automatic upload, no `flush`, and no `sync` without a target.

Every difference in behaviour must follow from one of the two properties:

| Behaviour | Follows from |
|---|---|
| Nothing is scanned; no dirstate file; no `.syncitignore` | bare: these describe a working folder |
| It never records an entry | bare: entries come from scanning and merging files |
| "Present" means the blob is stored (section 7) | bare |
| Content and names are encrypted | remote |
| No lock | remote: object storage has none. Safe because log and blob objects are immutable and named by hash |
| A secret key is needed | remote |

Everything else is the same as for any vault: a `config` with `nickname`, `include`, `exclude`, `keep_history` and `autocompact`; a log; blobs; validation of every segment (`CRDT.md`, "Validation") and blob received. The log format does not change.

**Out of scope.** Key rotation. Signed segments. Reading `~/.aws/credentials` (credentials come from the environment). Parallel transfers. A bare vault on a plain local path. Cloning a sub-folder of a remote in one step: `clone <url>` takes the whole vault. For part of one, make a vault with an empty filter and `get <paths> --from <url>`; how that vault is made (a flag on `init`, or `clone` of a sub-path) is left open.

## 2. Dependencies

```toml
object_store = { version = "0.14", features = ["aws", "gcp", "azure"] }
tokio = { version = "1", features = ["rt"] }
futures = "0.3"
url = "2"
chacha20poly1305 = "0.11"
aead-stream = { version = "0.6", features = ["alloc"] }
hkdf = "0.13"
getrandom = "0.4"
zeroize = "1"
bip39 = { version = "3", features = ["zeroize"] }
rpassword = "7"
keyring-core = "1"

[target.'cfg(target_os = "macos")'.dependencies]
apple-native-keyring-store = { version = "1", features = ["keychain"] }

[target.'cfg(target_os = "linux")'.dependencies]
zbus-secret-service-keyring-store = { version = "1", features = ["crypto-rust"] }
```

These versions and feature names come from documentation read on 2026-11-22. None of it was compiled. Check each API against docs.rs before relying on it, and record any difference in `SPEC.md` section 17. Known points to check:

- `object_store` is async only. Drive it from a private `tokio::runtime::Builder::new_current_thread().enable_all()` runtime with `block_on`. Since 0.13 the convenience methods (`get`, `put`, `put_multipart`, `head`, `delete`) are on `ObjectStoreExt`.
- `object_store::parse_url_opts` builds a backend from a URL but does not read the environment. Pass it every environment variable whose name starts with `AWS_`, `GOOGLE_` or `AZURE_`, name lowercased.
- `object_store` does not read `~/.aws/credentials`.
- The STREAM construction is in the `aead-stream` crate, not in `aead`.
- `hkdf` 0.13 needs `sha2` 0.11, which the crate already uses.

## 3. Modules

| File | Change |
|---|---|
| `src/storage.rs` (new) | the `Storage` trait and `Dir`, the plain-directory implementation: today's `.syncit/` access, moved |
| `src/remote.rs` (new) | `Remote`, the encrypted object-store implementation |
| `src/crypt.rs` (new) | key derivation, object names, stream encryption. No I/O beyond `Read` and `Write` |
| `src/keys.rs` (new) | phrase and key; keychain, environment and `secrets.env` |
| `src/vault.rs` | `Vault` holds `storage: Box<dyn Storage>`, `bare: bool` and a display `name`; opening by target |
| `src/log.rs`, `src/blobs.rs`, `src/scan.rs`, `src/materialise.rs`, `src/commands.rs` | every read or write of a vault's log, blobs or config goes through `v.storage` |
| `src/config.rs` | `remotes` |
| `src/output.rs` | `Options.secret_key` |
| `src/main.rs` | target arguments become strings; `init <url>`; `remote`; `--vault` on `config` and `compact` |

## 4. The storage trait

One interface for where a vault's replicated state lives. Callers speak only in plaintext names and hashes; keyed names and ciphertext never leave `remote.rs`.

```rust
pub trait Storage {
    fn load_config(&self) -> Result<Config>;
    fn save_config(&self, config: &Config) -> Result<()>;

    /// The names of the segments stored.
    fn segment_names(&self) -> Result<BTreeSet<String>>;
    /// A segment's bytes, not validated.
    fn read_segment(&self, name: &str) -> Result<Vec<u8>>;
    fn write_segment(&self, name: &str, bytes: &[u8]) -> Result<()>;

    fn has_blob(&self, hash: &str) -> bool;
    /// Copy the blob to `dest`, verifying its hash. False if it is absent
    /// or wrong; `dest` is then removed.
    fn get_blob(&self, hash: &str, dest: &Path) -> io::Result<bool>;
    /// The blob's bytes if it is stored, verified, and no larger than `max`.
    fn read_blob(&self, hash: &str, max: u64) -> Option<Vec<u8>>;
    /// Store the file at `src`, whose hash the caller has verified.
    fn put_blob(&self, hash: &str, src: &Path) -> Result<()>;
    /// The stored blobs whose hash is not in `keep`: how many, and their
    /// bytes. They are removed unless `dry_run`.
    fn drop_blobs_except(&self, keep: &BTreeSet<String>, dry_run: bool) -> Result<(usize, u64)>;
}
```

The signatures may be adjusted while building, but these rules hold:

- There is no method that lists blob hashes. An encrypted store cannot answer it (section 6), so no caller may need it. `blobs::droppable` and `compact` become `drop_blobs_except`.
- Working files are not storage. `Vault.hints` and the file sources in `blobs.rs` stay where they are. `blobs::fetch` tries, in order: this vault's working files, this vault's storage, the peer's working files, the peer's storage.
- `Log::load`, `commands::receive`, the segment writes in `scan.rs` and `materialise.rs`, `Config::load` and `save`, and the blob check in `commands::drop` all go through the trait.
- `Dir` must behave exactly as the code does today. Milestone 1 is this refactor alone, with the existing tests unchanged and passing.

The remote implementation needs a scratch directory for plaintext in transit. It is given the local vault's `.syncit/tmp` at open. It never uses the system temp directory. A command with no local vault (`init <url>`, or `config --vault <url>` run outside one) moves no blobs and needs none.

## 5. Layout of a remote vault

```
<url>/format         plaintext, one line: {"check":"<64 hex>","syncit":1}
<url>/config         the vault's config, encrypted
<url>/log/<name>     a segment, encrypted
<url>/blobs/<name>   a blob, encrypted
```

- `<name>` is 64 lowercase hex characters (section 6). Other names in a listing are ignored.
- Segments and blobs are immutable and written whole. One that exists is not written again.
- `config` is the same JSON as `.syncit/config`, and is the one object that is overwritten. Two vaults changing a remote's settings at the same moment: the later write wins.
- `format` is canonical JSON, keys in alphabetical order. `syncit` other than 1 is refused. `check` lets a wrong key be told from a damaged store.
- A prefix with objects but no `format` is not a vault and is refused, untouched.
- syncit never creates a bucket or a directory. A `file://` directory that does not exist is an error.

**URLs.**

- Any URL `object_store::parse_url_opts` accepts: `file:///abs/dir`, `s3://bucket/prefix`, `gs://…`, `az://…`, and R2 `https://` URLs.
- Credentials and endpoints come only from the environment: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_DEFAULT_REGION`, `AWS_ENDPOINT_URL`, `AWS_ALLOW_HTTP`, and the `GOOGLE_*` and `AZURE_*` equivalents. Nothing of the kind is written to any config.

**Listing and transfers.**

- Opening a remote lists `log/` and `blobs/` once each (names and sizes) and works from those lists for the rest of the run, adding to them as it writes.
- `has_blob` is a lookup in the listing.
- Download: stream to a file in the scratch directory, decrypt from there.
- Upload: encrypt to a file in the scratch directory. Under 16 MiB, one `put`. Otherwise `put_multipart` with 16 MiB parts, aborted on error.
- `// ponytail:` transfers are sequential. Parallelise if a large first upload is too slow.

## 6. Encryption (`crypt.rs`)

The master key `K` is 32 random bytes from `getrandom`, one per remote vault, held in `Zeroizing`.

Everything is derived from `K` with HKDF-SHA256 `expand` (`Hkdf::from_prk(K)`), 32 bytes out:

| Value | `info` |
|---|---|
| `check` in `format` | `"syncit check"` |
| object name | `"syncit name " + kind + " " + hash` |
| object key | `"syncit key " + kind + " " + name` |
| key for `config` | `"syncit key config"` |

`kind` is `log` or `blobs`. `hash` is the plaintext's SHA-256 in hex: the segment's name or the blob's hash, as today. `name` is the object name in hex.

- **Hash addressed.** An object's name is a function of its content's hash and the key. It can be computed without reading the file.
- **Keyed.** Without `K`, a name says nothing about the content, so the provider cannot test whether a known file is stored.
- **One way.** A name cannot be turned back into a hash. This is why the trait has no "list blob hashes".
- The object key is derived from the object's name, not from the plaintext hash, because a vault downloading a segment it has never seen knows only the name.

Body of every encrypted object: a 19-byte random nonce, then XChaCha20-Poly1305 in the STREAM construction (`aead-stream`, `EncryptorBE32` and `DecryptorBE32`), plaintext in 64 KiB chunks, the last sealed with `encrypt_last`. The nonce is random so that a mistake elsewhere can never reuse a key and nonce with different content.

Decryption fails, without panicking, on a wrong key, a changed byte, a truncated object or a short nonce. Plaintext is written to a scratch file and moved into place only after the whole object has decrypted and its hash has been checked.

**Segment names in a remote.** `segment_names()` must return plaintext names, which the listing does not contain. `Remote` is given the local vault's segment names at open. A listed name equal to `name(K, "log", s)` for a local segment `s` is `s`. Each remaining object is downloaded and decrypted; its name is the SHA-256 of its plaintext, and it is kept only if `name(K, "log", that)` equals the object's name. An object that fails either step is reported as `rejected` and left out. The bytes are held in memory for `read_segment`.

What the provider can still see: how many objects there are, their sizes, and when they were written.

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

**`Options.secret_key`.** `main` reads the environment variable `SYNCIT_SECRET_KEY`, which holds a phrase, into `Options.secret_key`. The library never reads the environment for it, so tests pass a phrase in `Options`. A value that is not a valid phrase is an error in any command that needs a key; it is never passed over in silence.

**Finding the key for a URL.** Candidates, in order. A candidate is used only if its `check` equals the one in the remote's `format`:

1. `Options.secret_key`.
2. The keychain: service `syncit`, account = the URL, password = the phrase.
3. `.syncit/secrets.env` in the local vault, if the command runs in one: every `SYNCIT_SECRET_KEY="…"` line.

If none matches: `no key for <url>: set SYNCIT_SECRET_KEY, or run syncit remote add <url>`. Only `remote add` and `clone` ever prompt.

**Keychain.** Set the platform store as `keyring_core`'s default once per process. `NoEntry` means "not here". Any other error means the keychain cannot be used on this machine now (a headless Linux box, macOS over SSH): go on to the next candidate, do not fail.

**`.syncit/secrets.env`.** Local, like `config`; `.syncit` is never synced. Mode 0600, written with `write_atomic`. One entry for each remote:

```
# s3://bucket/notes
SYNCIT_SECRET_KEY="abandon ability … zoo"
```

**Saving a phrase**, after `init <url>`, `remote add` or `clone` has a key that matched:

- It came from `Options.secret_key`: save nothing.
- The keychain is usable: save it there. Already there: nothing to do.
- Otherwise, if the command runs in a vault: with `--save-key`, write it to `secrets.env`; on a terminal, ask `Save the key to .syncit/secrets.env? [y/N]`; with neither, do not save, and say that `SYNCIT_SECRET_KEY` must be set for every command that uses this remote.
- Otherwise (no keychain and no local vault): do not save, and say the same.

## 9. Commands

**Targets.** Wherever a command takes another vault, it now takes a target, a string resolved in this order:

1. A key of the local vault's `config.remotes`: replaced by its location.
2. A string containing `://`: a remote vault at that URL.
3. Otherwise a path, resolved as today.

A folder that shares a remote's nickname is reached as `./name`. A URL may be used directly without `remote add`, as a path may; the key is then found by URL (section 8).

**`syncit init <url>`** makes a remote vault. The prefix must be reachable and empty.

1. `K` is the key in `Options.secret_key` if set, otherwise new random bytes.
2. Write `format`, then `config`: `include: [""]`, `keep_history: null`, `autocompact: false`, `nickname` from `--nickname <name>`, default `origin`.
3. If the key was generated, print it, then save it (section 8):

```
Secret key for <url>:

  word1 word2 … word24

Write these words down and keep them safe. They are the only way to
read this vault from another computer, and they are not shown again.
```

`init` with a path is unchanged.

**`syncit remote`.** `config` gains `remotes`: a map from nickname to location, where a location is a URL or the absolute path of a vault. It is this vault's address book and nothing else reads it. `remotes` is a required field, like the others: vaults made before it are not migrated, since syncit has not shipped. A name in it is a valid nickname with no `/`, so that it cannot be taken for a path or a URL; a target's nickname that does not qualify needs `--name`. The nickname stays what `CRDT.md`, "Nickname", says it is: a label that no rule of the log reads.

| Command | Behaviour |
|---|---|
| `syncit remote` | List the remotes: nickname, then location. |
| `syncit remote add [--name <name>] [--save-key] <location>` | Remember a vault. See below. |
| `syncit remote remove <name>` | Forget it. Its `secrets.env` entry is removed. Its keychain item stays, since other vaults on this machine may use it. Nothing in the remote is touched. |

`remote add`:

1. A location with `://` is a URL, kept as given. Otherwise it is a path, kept in absolute form, and must be a vault.
2. For a URL, read `format` and find the key (section 8). If no candidate matches and there is a terminal, ask `Secret key (24 words): ` without echo and check it. With no terminal, fail.
3. Read the target's `config`. The name is `--name` if given, otherwise the target's own `nickname`.
4. If that name is already in `remotes`, fail and say to pass `--name`.
5. Save the key (section 8), then add the entry to `config.remotes`. This is last, so a failed add changes nothing.

A URL with no `format` fails with `no vault at <url>: run syncit init <url>`.

**`syncit clone <url> <dir>`.** Create the vault in `<dir>` as `clone` does, `remote add <url>` in it, then `pull` from it. A clone from a path is unchanged, and now also records the source in `remotes` under its nickname if that name is free.

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

No test uses the real keychain or the network. Tests pass a phrase in `Options.secret_key`, which keeps the keychain out of it; the one keychain test installs `keyring_core::mock`. Remote vaults in tests are `file://` URLs of temporary directories. Reuse the helpers in `tests/stories.rs`.

| Test | Checks |
|---|---|
| (existing suite) | passes unchanged after the storage refactor |
| `crypt_round_trip` | empty, 1 byte, 64 KiB, 64 KiB + 1 and 1 MiB decrypt to the input |
| `crypt_rejects` | wrong key, wrong name, wrong kind, a changed byte, truncation at a chunk boundary, a short nonce: each is an error and none panics |
| `names_are_keyed` | names differ between keys and between kinds, are stable for one key, and are never the plaintext hash |
| `phrase_round_trip` | 32 bytes to 24 words and back; a misspelt word is named by position; a swapped word is a checksum error |
| `key_lookup_order` | `Options`, mock keychain, `secrets.env`: the first whose check matches is used; one that does not match is passed over; none gives the documented error |
| `secrets_env_file` | mode 0600; two remotes give two entries; `remote remove` deletes only its own |
| `init_remote` | writes `format` and `config`; prints 24 words when it generates the key and none when given one; refuses a non-empty prefix and leaves it untouched; refuses `format` version 2 |
| `remote_add` | records the target's nickname and location for a URL and for a path; a taken name needs `--name`; a wrong phrase is refused and `config` is unchanged; `remote remove` forgets it |
| `story_c2_sync_through_remote` | two vaults that never sync with each other converge through one remote, in either order, including a conflict; a vault with no history supplies its current files; a third machine `clone`s the URL and has everything |
| `remote_is_opaque` | no object in the remote's directory contains a known line of a file, a path, a nickname or a plaintext hash, and no object name is a plaintext hash |
| `remote_hostile` | a garbage object, a valid object copied to another name, and a truncated blob are rejected or left pending; the rest of the run completes |
| `remote_by_nickname` | `sync origin`, `sync <url>` and `sync ./origin` (a folder) each reach the right vault |
| `get_from_remote` | a partial vault runs `get music --from origin`; the folder arrives and stays included; nothing else is downloaded. `--from` with a folder vault's path gives the same result as the inferred form |
| `move_to_remote` | files are removed here only when the remote has their content; a remote that excludes them refuses |
| `partial_remote` | a remote whose `include` is one folder takes only that folder's blobs, and `push` warns about the rest as it does for a folder |
| `remote_history_depth` | with `config --vault origin keep-history 1` and `compact --vault origin`, older blobs are dropped, the log is whole, and current versions remain |
| `status_vs_remote` | `status --vs origin` reports the same states as against an equivalent folder vault |
| `dry_run_remote` | a dry `sync` leaves the remote and the vault byte-identical and reports what the real run then does |
| `cloud_remote` | `story_c2_sync_through_remote` against the URL in `SYNCIT_TEST_REMOTE_URL`; skipped when it is not set |
| `cli_smoke` additions | `init <url>`, `remote`, `remote add`, `remote remove`, `sync origin`, with `SYNCIT_SECRET_KEY` set |

## 13. Milestones

Each ends with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passing. Do them in order; each is a commit that leaves the tool working.

1. **The storage seam.** The `Storage` trait and `Dir`; `Vault.storage`; every access to log, blobs and config through it. No behaviour changes. Test: the existing suite, unchanged.
2. **`crypt`.** Tests: `crypt_round_trip`, `crypt_rejects`, `names_are_keyed`.
3. **`keys`.** Tests: `phrase_round_trip`, `key_lookup_order`, `secrets_env_file`.
4. **`Remote` and `init <url>`.** The object-store implementation of `Storage`, `format`, the encrypted `config`. Test: `init_remote`.
5. **Bare vaults in a run.** `Vault.bare`, the derived dirstate, targets by URL. Tests: `story_c2_sync_through_remote` (with `init` and `pull` in place of `clone`), `remote_is_opaque`, `remote_hostile`, `move_to_remote`, `partial_remote`, `dry_run_remote`, `status_vs_remote`.
6. **Nicknames and the remaining commands.** `config.remotes`, `remote`, targets by nickname, `clone <url>`, `get --from`, `--vault`. Tests: `remote_add`, `remote_by_nickname`, `get_from_remote`, `remote_history_depth`, the `clone` part of `story_c2_sync_through_remote`, `cli_smoke`.
7. **A real bucket.** Run `cloud_remote` against MinIO or a real bucket and fix what differs from `file://`.
8. **Documents.** `SPEC.md`, `PRD.md` and `CRDT.md` already describe this design as not built. When it is built: in `SPEC.md`, remove the "Not built" notes in sections 8, 10 and 14, tick milestone 9, drop the "not built yet" entry in section 17 and record there anything built differently; in `PRD.md`, remove "Stores are not built yet". Add a README section: remotes, the phrase and its warning, credentials by environment variable, the macOS keychain prompt after reinstalling, the multipart lifecycle rule.

## 14. Building this

`SPEC.md`, "Working rules for the implementer", applies. In addition:

- **Done** means milestones 1 to 6 and 8 are complete: every test in section 12 except `cloud_remote` exists and passes, with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` clean, and CI (`.github/workflows/ci.yml`) green on Linux.
- **Milestone 7 needs a bucket.** If `SYNCIT_TEST_REMOTE_URL` is not set and no local S3-compatible server can be started, write `cloud_remote` so that it skips, leave milestone 7 open, and say so in the final report. Do not mark it done on the strength of `file://`.
- **One commit for each milestone**, on the current branch, with the tool working after each.
- **Milestone 1 changes no behaviour.** If an existing test has to change to make it pass, the refactor is wrong.
- **The formats in sections 5 and 6 are the contract.** Crate APIs may differ from what section 2 expects; adapt the code and record it in `SPEC.md` section 17. If a crate cannot give the construction as written (for example, no STREAM with XChaCha20-Poly1305), stop and report; do not substitute another scheme.
- **If the default TLS provider will not build in CI**, use the crate's `ring` variant of the same features, and record it.
- **Do not build what section 1 lists as out of scope.** In particular, do not invent a way to make a partial vault from a remote; that decision is open.
- **Never print, log or put in an error the phrase or the key**, outside the one `init` message. Tests assert on output; none may contain a real key in a failure message.
- **No test may touch the real keychain or the network** (section 12).

## 15. Known limits

- If every copy of the phrase is lost, the remote cannot be read.
- On macOS, a rebuilt binary is asked once for keychain access.
- A phrase in `secrets.env` is plaintext on disk. That is the user's choice.
- Two machines running `init` on the same empty URL at the same moment can both write `format`; the later one wins and the other's key stops matching.
- A push from a vault that has not pulled recently downloads the remote's unseen segments to learn its log.
- A vault that keeps history collects it from a peer, as today. From a remote that is a download: a `pull` fetches the older versions `keep_history` covers as well as the current files.
