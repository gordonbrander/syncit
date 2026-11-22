# syncit: implementation specification

This is the entry point for building syncit. Given this file alone, you should be able to implement version 1 by following its links and its milestones.

## 1. Start here

### What syncit is

syncit is a command-line tool, written in Rust, for two-way sync and partial replication between folders. A folder becomes a *vault* by gaining a `.syncit/` directory. Any vault can sync with any other. A vault may have only part of the files. Conflicting edits are kept side by side as visible files. Keeping old versions is optional per vault.

### State of the repository

Four documents:

| Document | Read it for | Authority |
|---|---|---|
| `SPEC.md` (this file) | how to build it: formats, procedures, commands, tests, milestones | wins on implementation detail |
| @CRDT.md | the log: entry fields, segment format, canonical form, validation, read rules, write rule, `base` | wins on anything about the log |
| @PRD.md | why the tool exists, user stories, requirements `R1`–`R28`, rationale for rejected designs | wins on user-visible intent |
| @store.md | remote vaults: the storage trait, the encrypted layout, keys, remotes, and the commands that take a target | wins on anything about remote vaults |

Read `CRDT.md` in full before milestone 1, `PRD.md` in full before milestone 3, and `store.md` in full before milestone 9. This file does not repeat the log format; it links to it. If two documents disagree, follow the authority column, and record the disagreement under "Deviations" (section 17).

### The model in brief

Three things are kept separate. Each can be partial, lost or rebuilt without corrupting the others.

- **The log** says what happened. It is a grow-only set of entries, merged between vaults by set union. Current state is computed from it and never stored.
- **Blobs** are file contents, addressed by SHA-256. Any source may have any subset.
- **The working folder** is ordinary files. The **dirstate** records which version of each file this vault has on disk.

A sync run, done by each vault on its own folder (`PRD.md`, "A sync run"):

1. Scan the folder against the dirstate and write new entries as one segment.
2. Retain blobs for the new entries, if the vault retains blobs.
3. Exchange log segments.
4. Compute current state from the merged log.
5. Fetch the blobs needed.
6. Materialise: write files, apply deletions, write conflict copies, update the dirstate.

### Glossary

| Term | Meaning |
|---|---|
| vault | a folder containing `.syncit/` |
| entry | one log record: a path had some content (`exec`, `hash`, `mtime`, `parents`, `path`) |
| entry id | SHA-256 of the entry's canonical line |
| tombstone | an entry whose `hash` is `null`: the path was deleted |
| segment | an immutable file of entries, the output of one scan, named by its SHA-256 |
| nickname | a vault's label, by default its folder's name, written into each segment it records. Not an identity |
| head | an entry of a path that no other entry names as a parent |
| winner | the head every vault agrees to show, chosen by `(is file, mtime, id)` |
| loser | a distinct file content among the heads that is not the winner's |
| conflict | a path whose heads have more than one distinct `hash` |
| conflict copy | a loser written beside the winner as `<name>-<short hash>.<ext>` |
| incomplete | a path with an entry whose parent is not in the log yet |
| dirstate | local file `.syncit/dirstate`: this vault's position in the log. Per path, `(size, mtime, hash, base)`: what was on disk at the last scan, and the entries it was derived from. It is the vault's record of what is present; between scans the two can differ, and a scan finds the difference |
| row | one path's record in the dirstate |
| present | a path whose file is on disk in this vault: the vault has it |
| base | in a row, the entries the file on disk was derived from; the parents of the next entry |
| included | a path that the vault's filter, its `include` and `exclude` prefixes, selects. After a scan every path with a row is included |
| excluded | a path the filter does not select. Absence here is by choice, not a deletion |
| ignored | a path `.syncitignore` rules out: never scanned and never written here |
| blob | a file's bytes, addressed by hash |
| peer | the other vault in a run; always named on the command line |
| target | another vault as a command names it: a path, a URL, or a nickname in `remotes` (`store.md`, section 9) |
| remote | an entry in a vault's `remotes`: a nickname and the location of another vault |
| store | a vault that is bare (no working folder) and remote (on object storage, encrypted), reached by a URL. It holds old versions, and lets vaults that never meet sync through it. Specified in `store.md` |

### Working rules for the implementer

- The experience goal is Dropbox, not git (`PRD.md`, "Goals"). People manage their files with ordinary tools and run `sync` when they feel like it. When a choice is open, take the one that needs no extra step or command from the user, as long as it cannot lose data.
- Do the milestones in section 15 in order. Each ends with tests passing. Tick its box in this file when it is done.
- Keep it simple. One crate, the dependencies listed in section 3 and no others, no traits with one implementation, no configuration beyond what section 5 defines. Where two designs work, take the one that keeps separate things separate.
- Core rules are pure functions over data (the read rules, the include filter, conflict-copy names). I/O sits around them.
- Do not change the on-disk formats, the read rules or the write rule. If something in these documents is wrong or cannot be built, stop work on that part, write what you found under "Deviations", choose the smallest fix, and continue.
- Never weaken a safety check in section 9 to make a test pass.
- Section 16 lists questions that are deliberately open. Do not solve them in version 1.

### The goal

Version 1 is done when all of these hold:

1. Every milestone box in section 15 is ticked.
2. `cargo test` passes, and every test named in section 14 exists.
3. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` pass.
4. `cargo run -- --help` lists the commands of section 11.

## 2. Scope of version 1

In: every Must (`R1`–`R14`) and every Should (`R15`–`R18`, `R20`, `R24`, `R25`) in `PRD.md`.

Out:

- The Coulds: `R21` (limit a run to a path), `R22` (copy-on-write stashing), `R23` (mass-delete guard). `syncit sync` therefore takes no `[path]` argument in version 1.
- Everything under "Non-goals" in `PRD.md`.
- Signed segments, the heads file, and compaction of the log (`CRDT.md`, "Planned extension", "Lookups", "Compaction"). Compaction keeps the whole log and drops only old blobs. Blobs are compacted (section 8); the log never is.
- Peers reached over a network (SSH, iroh). A vault is a directory on this machine, or a remote vault on object storage (`store.md`).
- Windows. Version 1 targets macOS and Linux.

## 3. Crate and dependencies

- One Cargo package named `syncit`, with a library (`src/lib.rs`) and a binary (`src/main.rs`). Integration tests call the library.
- Rust edition 2024. Minimum Rust version 1.89, for `std::fs::File::try_lock`.
- Dependencies: `clap` (with `derive`), `serde` (with `derive`), `serde_json`, `sha2`, `anyhow`, and `diffy` for the three-way merge of text (section 9).
- Remote vaults need more: an object-store client, a cipher, a mnemonic encoder and a keychain. They are listed in `store.md`, section 2, and are optional: two Cargo features, `remote` and `keychain`, both on by default. A build with neither has only the dependencies above.
- Dev dependency: `tempfile`.
- From the standard library, not from crates: directory walking (`fs::read_dir`), hex encoding, setting file times (`File::set_modified`), file locking, atomic rename.

## 4. Modules

Suggested layout. Merge or split if it reads better, but keep the pure parts free of I/O.

| Module | Holds | I/O |
|---|---|---|
| `entry` | `Entry`, canonical encode and decode, entry id, path validation | none |
| `segment` | encode, decode and validate a whole segment | none |
| `log` | load `log/`, the path-to-entries map, `resolve` (the read rules) | load only |
| `config` | `Config`, load and save, `includes` | load and save |
| `dirstate` | `Dirstate`, `Row`, the deletion records, load and save, the unchanged test | load, save, stat |
| `scan` | walk, the write table, segment writing | yes |
| `blobs` | hashing, blob sources, fetch, retain | yes |
| `materialise` | the per-path procedure, merging text, conflict copies, setting base | yes |
| `vault` | `Vault` (root, config, dirstate, log, lock), finding the root | yes |
| `commands` | one function per command; a command that runs a sync returns a `Summary` | yes |
| `output` | `Options`, the `Message` types with their text and JSON forms, `print_stdout`, `print_stderr`, `Summary` | no |
| `main` | argument parsing, printing the `Summary` and errors, exit code | yes |

Remote vaults add `storage`, `remote`, `crypt` and `keys`, and put every read and write of a vault's log, blobs and config behind one trait (`store.md`, sections 3 and 4).

Types the rest of this file refers to:

```rust
// Fields are declared in alphabetical order: serde_json writes them in
// declaration order, which gives the canonical key order.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub exec: bool,           // the owner may execute the file; false for a tombstone
    pub hash: Option<String>, // 64 lowercase hex, or None for a tombstone
    pub mtime: i64,           // milliseconds since the Unix epoch
    pub parents: Vec<String>, // entry ids, sorted, no duplicates
    pub path: String,
}

pub struct Row { pub size: u64, pub mtime: i64, pub exec: bool, pub hash: String, pub base: Vec<String> }

/// The result of the read rules for one path.
pub enum PathState {
    Absent,      // no valid entries
    Incomplete,  // some entry names a parent that is not in the log
    Resolved {
        heads: Vec<Id>,
        winner: Id,
        losers: Vec<Loser>,  // distinct non-null hashes other than the winner's
        lost_delete: bool,   // winner is a file and some head is a tombstone
    },
}
// mtime: the largest among heads with this hash. exec: that head's; on a tie, the greatest id's.
pub struct Loser { pub hash: String, pub mtime: i64, pub exec: bool }
```

`Id` and hashes may be `String`. Do not build a newtype hierarchy.

## 5. On-disk layout and formats

```
<vault>/
  .syncit/
    config    # JSON, local
    dirstate     # JSON, local
    lock      # empty file, locked while a command runs
    log/      # segments, named by hash; the only replicated part
    blobs/    # blobs, named by hash, flat
    tmp/      # scratch for atomic writes into .syncit/
  .syncitignore   # optional ignore list; an ordinary file
```

Every file written under `.syncit/` is written to `.syncit/tmp/` first and renamed into place.

### config

```json
{
  "syncit": 1,
  "include": [""],
  "exclude": [],
  "keep_history": 1,
  "autocompact": true,
  "nickname": "notes"
}
```

- `syncit`: format version (`R25`). A command refuses to run on a vault, or with a peer, whose version is not 1.
- `include`, `exclude`: the filter's prefixes, vault-relative, `/`-separated. `""` is the vault root.
- `keep_history`: how many versions back from a head this vault keeps the content of in `.syncit/blobs/` (`R15`; section 8, "Retaining"). `0` keeps no history, `null` keeps every version, and a new vault has `1`. Local history is whatever blobs are there; it is never required to be complete.
- `autocompact`: whether the end of every run drops the blobs `keep_history` no longer covers (section 8, "Compaction"). A new vault has `true`. With `false` they are dropped only by `syncit compact`.
- `nickname`: the label written into the header of every segment this vault records (`CRDT.md`, "Nickname"). `init` and `clone` set it to the folder's name, with whitespace and control characters replaced by `-`, cut to 64 bytes, or to `vault` if that leaves nothing. It is a label only: `blame` prints it and no other code reads it.
- Written pretty-printed. Unknown fields are an error. `keep_history`, `autocompact` and `nickname` are changed with `syncit config` (section 11). Remote vaults add a `remotes` field (`store.md`, section 9).
- No vault is special and there is no default peer: any vault syncs with any other, and a command that involves another vault names it. A vault may remember others by nickname in `remotes`, its address book (`store.md`, section 9). Nothing else is recorded about them, and a folder that moves breaks only its own entry.

**Included** (`PRD.md`, "What a vault has"): a prefix matches a path if it is `""`, equals the path, or is followed in the path by `/`. Among all matching prefixes in both lists, the longest decides: included if it came from `include`. If none matches, the path is excluded. A prefix is never in both lists.

### dirstate

```json
{
  "scanned": 1763600000000,
  "rows": {
    "foo/bar/x.txt": { "size": 120, "mtime": 1763599990000, "exec": false, "hash": "9c1f…", "base": ["3f9a…"] }
  },
  "deleted": {
    "foo/old.txt": ["77b0…"]
  }
}
```

- `scanned`: when the most recent scan started, in milliseconds. 0 before the first scan.
- `rows`: one per file this vault had on disk at the last scan. `mtime` is the file's mtime as the filesystem reported it, in milliseconds (floor; negative before 1970). `exec` is whether the owner may execute it (`mode & 0o100`), the only permission that is recorded.
- `deleted`: one per path this vault has seen deleted, with its base: the heads, all tombstones, that the absence of a file here reflects. A path is in at most one of `rows` and `deleted`. A dirstate written before this field existed has none, and loads as if it were empty.
- A missing dirstate file is an empty dirstate. That is the "lost dirstate" case and must work (`CRDT.md`, "Writing an entry", third row).

**The unchanged test.** A file is unchanged against its row if its size, mtime and executable bit equal the row's **and** the row's mtime is less than `scanned − 2000`. Otherwise it is hashed and compared with the row's hash; it is the row's version only if its executable bit also equals the row's. The second condition catches an edit made in the same timestamp tick as the scan that recorded the file; the 2000 ms margin covers filesystems with coarse timestamps. Keep the margin a named constant. One function implements this test and is used by scan, materialise, `move` and `drop`.

### Segments

Format, canonical form and validation are in `CRDT.md`, "Segments" and "Validation". Implementation notes:

- Encode an entry with `serde_json::to_string`. Decode by parsing the line into `Entry`, re-encoding, and requiring the bytes to equal the line. This one check enforces key order, absence of whitespace, number form, string escaping, presence of all four fields and absence of others.
- Then check the field contents: `hash` and each parent are 64 lowercase hex characters; `parents` is strictly ascending by byte order; `path` passes path validation; a tombstone's `exec` is `false`.
- Path validation: not empty, does not start with `/`, no component that is empty, `.` or `..`, first component is not `.syncit`.
- The file is: the header line, then zero or more entry lines, each line ended by one `\n`. No blank lines, no missing final newline.
- The header is `{"nickname":"<nickname>","syncit":1}`. It is encoded and checked as an entry is: parse it into a struct with exactly those two fields, re-encode, and require the same bytes. Then check that `syncit` is 1 and the nickname is valid: 1 to 64 bytes, no whitespace, no control characters.
- Decoding returns the nickname with the entries. The log keeps, for each entry id, the set of nicknames of the segments it was found in.
- A writer sorts entries by path (byte order) and writes at most one entry per path.
- The canonical string form must match RFC 8785: `"` and `\` are escaped with a backslash; U+0008, U+0009, U+000A, U+000C and U+000D are written `\b`, `\t`, `\n`, `\f`, `\r`; other characters below U+0020 are written `\u00xx` with lowercase hex; everything else, including `/`, U+007F and all non-ASCII, is written literally. `serde_json` is expected to do exactly this. Test `canonical_escaping` pins it; if it fails, write the string encoder by hand.

Test vector:

```
entry line:  {"exec":false,"hash":null,"mtime":1763600005000,"parents":[],"path":"old.txt"}
entry id:    7143ad25f65a2079629dadd17cde1155bc1152bb41be99705c0f807933e841c9
segment:     {"nickname":"notes","syncit":1}\n<entry line>\n
segment name: 84f57e1040620ebe774999c3e166a89617ec808413902b99f36de3e21b0caec2
```

### .syncitignore

`.syncitignore`, at the vault root, is the optional ignore list (`R20`). One rule per line; blank lines and lines starting with `#` are skipped. Only the root file is read; there are no per-folder ignore lists.

- A line without `/` is a name. It ignores any file or folder with exactly that name, at any depth.
- A line with `/` is a vault-relative path prefix, matched as the filter's prefixes are.
- No wildcards.

An ignored path is invisible: scan does not look at it (so a recorded file that becomes ignored is not read as deleted) and materialise never writes it.

`.syncitignore` is itself an ordinary file. It is scanned, replicated and conflict-handled like any other, so vaults share one set of rules. No code treats its path specially. Two things follow:

- A vault that wants rules of its own lists `.syncitignore` in the file. The file is then ignored there: not sent, and not replaced by another vault's.
- The rules are read once, when a command opens the vault. A `.syncitignore` that arrives or changes during a run takes effect from the next run.

## 6. Reading the log

Implements `CRDT.md`, "Reading the log" and "Incomplete logs". Serves `R8`, `R12`.

- Loading reads every file in `.syncit/log/`, validates it, and adds its entries to a map from id to entry and a sorted map from path to ids. A local segment that fails validation is an error that stops the command and names the file.
- `resolve(path) -> PathState` is a pure function of those maps:
  1. If any entry of the path names a parent that is absent from the log: `Incomplete`.
  2. Drop invalid entries: those with a parent whose path differs.
  3. Superseded, heads, winner and losers as in `CRDT.md`. No entries left: `Absent`.
- Version 1 loads the whole log on every run and stores nothing derived (`CRDT.md`, "Lookups").

## 7. Scan

Implements `PRD.md`, "Scanning", and `CRDT.md`, "Writing an entry". Serves `R3`, `R5`, `R6`, `R17`, `R20`.

### The walk

Walk the vault root recursively. Skip, without descending:

- `.syncit` at the root;
- any other directory named `.syncit`, at any depth. A vault dragged into this one is not skipped: its files are ordinary files of this vault, and only its `.syncit/` stays out. Warn on stderr, naming the inner vault;
- anything ignored;
- symbolic links and anything that is not a regular file or a directory (warn once per run on stderr with a count);
- names that are not valid UTF-8 (warn, naming the parent directory);
- files whose name starts with `.syncit-tmp-`: these are leftovers of an interrupted run, and scan deletes them.

Paths are recorded as the operating system returns them, joined with `/`. No Unicode normalisation.

### Per path

Let `now` be the time the scan started. For each file found, and for each row whose path was not found and is not ignored, apply the table in `CRDT.md`, "Writing an entry":

- **File and row.** Apply the unchanged test. If the hash and the executable bit equal the row's, refresh the row's size and mtime and write nothing. If either differs, write a file entry with `parents` = the row's base; the row becomes `(size, mtime, exec, hash, [new entry id])`. A change of the bit alone is a new version, as in Git; `chmod` leaves the mtime alone, so that entry has its parent's mtime.
- **Row, no file.** Write a tombstone with `mtime = now` and `parents` = the row's base. Replace the row with a deletion record whose base is the tombstone.
- **File, deletion record.** Hash it. Write a file entry with `parents` = the record's base, whatever the hash. Replace the record with the row.
- **File, neither.** Hash it. Then the first of these that applies:
  1. `resolve` gives a winner that is a tombstone (so every head is one): write a file entry with `parents` = the heads and create the row.
  2. Some valid entries of this path have that hash and that executable bit: write nothing and create a row whose base is all of them (superseded ones included).
  3. Some valid entries of this path have that hash, all with the other bit: the vault has that version and has changed its bit. Write a file entry with `parents` = all of them and create the row.
  4. Otherwise write a file entry with `parents = []` and create the row.
- **No file, no row.** Nothing. The vault does not have the path, or the path is deleted and stays so.

**Auto-include.** A file on disk is included. When the walk finds a file whose path is excluded, with or without a row, the scan adds an include for it. A folder dragged into a vault is therefore synced from then on, including files later added to it elsewhere, and a file the vault has is never left out of a run (a `move` stopped partway, a config edited by hand): when in doubt, data is kept. The prefix added is the shortest of the path's prefixes (its ancestor directories, then the path itself) that is not equal to, or an ancestor of, any prefix already in `include` or `exclude`. Examples, for a vault that includes `foo/bar`:

| New file | Include added | Why not shorter |
|---|---|---|
| `zap/x/y.txt` | `zap` | `""` is an ancestor of `foo/bar` |
| `foo/new.txt` | `foo/new.txt` | `foo` is an ancestor of `foo/bar` |
| `foo/bar/big/n/a.txt`, with `foo/bar/big` excluded | `foo/bar/big/n` | `foo/bar/big` is already a prefix |

If every prefix is already listed, which happens only when the file's own path is in `exclude` (a moved file put back by hand), the path moves from `exclude` to `include`, as `get` would do. Includes are added after the walk, shortest first, re-checking the filter, so one dragged folder yields one include. The config is saved before the dirstate. A dry scan adds nothing. `move` and `drop` undo it.

A file entry's `mtime` is the file's mtime in milliseconds, and its `exec` is the file's executable bit; a tombstone's `exec` is `false`. Stat the file before and after hashing; if size or mtime changed in between, leave the path and its row untouched this scan and report it as `skipped`.

### Finishing

1. If there are new entries, write them as one segment into `.syncit/log/`. No entries, no segment.
2. Set `scanned = now` and save the dirstate.
3. Retain blobs (section 8).

The segment is written before the dirstate is saved. If the run stops between the two, the next scan sees the same difference and writes an entry with the same path, hash, mtime and parents, which is the same entry. For a deletion it writes a second tombstone beside the first, which is harmless: two tombstone heads are the same version.

`scan` has a dry mode that writes nothing: no segment, no config, no dirstate, and scratch files are left alone. The vault value in memory is brought up to date all the same, so what follows sees the state a real scan would leave. `status` and `--dry-run` (section 10) use it.

## 8. Blobs

Implements `PRD.md`, "Blobs". Serves `R11`, `R13`, `R15`. Stores (`R16`, `R24`, `R28`) are specified in `store.md` and summarised at the end of this section.

### Sources

`fetch(hash, destination)` tries these in order and stops at the first that yields the right bytes:

1. this vault's working files;
2. this vault's `.syncit/blobs/<hash>`;
3. the peer's working files, if there is a peer in this run;
4. the peer's `.syncit/blobs/<hash>`;

- Working files are found through an in-memory map from hash to paths, one per vault, built at the start of the run from the dirstate rows. Whenever the run writes a file or renames one (including a conflict copy, which has no row), it adds the new location to the map. The map is a hint only.
- Every fetch copies to the destination while hashing and succeeds only if the hash matches (`R11`). On a mismatch the destination is removed and the next source is tried.
- If no source has the blob, `fetch` returns "unavailable". That is not an error.

### Retaining

`keep_history` says how many versions back from a head a vault keeps the content of. `Log::keep(depth, paths)` is the rule, a pure function of the log that gives a set of hashes:

- `null`: every non-null hash.
- Otherwise, for each path:
  - one head: the hashes of the entries within `depth` steps of the head along `parents`. The head is step 0, and a tombstone or a change of the executable bit alone is a version like any other;
  - more than one head, or `Incomplete`: every hash of the path. A merge reads the heads' common ancestor and a conflict copy reads a loser, however far back they are.
- The union over paths: content two paths share is kept while either needs it.

With depth 1 or more, the last content of a deleted file is kept: the tombstone is the head.

A vault retains blobs unless `keep_history` is `0`; then it never writes to `.syncit/blobs/` (`R13`). Local history is whatever blobs are there: it is never required to be complete.

`retain()`: for every hash that `Log::keep` gives for the paths this vault takes (included and not ignored), and that is not already in `.syncit/blobs/`, `fetch` it into `.syncit/blobs/`. Hashes with no source are skipped silently. Two limits follow from the rule, and both are meant:

- A vault collects only what it keeps. A peer that keeps more, or has not compacted, does not fill it up again.
- A partial vault does not gather the content of paths it excludes.

A retaining vault calls `retain()` after every scan, and again after segments are exchanged and before anything is materialised, so that a version is saved before it is replaced.

### Compaction

Compaction keeps the whole log and drops only old blobs. No entry and no segment is removed, rewritten or trimmed, so heads, winners, conflicts and ancestry are computed exactly as before, here and in every vault this one syncs with, and nothing becomes `incomplete`. What a compacted vault can no longer do is supply the bytes of an old version: `log` lists it as `no content`, and `restore` of it fails unless a source still has the blob.

`compact()`: remove from `.syncit/blobs/` every file whose name is not in `Log::keep(keep_history)` taken over **all** paths, taken by this vault or not. So:

- The content of a current version is never dropped, at any setting, `0` included. A file removed with `drop` may have its only copy here (section 11).
- A blob the log does not name is dropped.

It runs at the end of a run and of `snapshot` if `autocompact` is on (section 10, step 8), and when the user runs `syncit compact` (section 11). It does not run in `restore`, `cp`, `move` or `drop`: `restore` and `cp` record the folder and then replace files, and must not drop a version in between. A dry run drops nothing.

### Stores

`store.md` has the layout, the encryption, the keys and the commands. This is a summary.

A store is not a second kind of thing. It is a vault that is *bare* (it has no working folder, so the content of its current versions is in `blobs/`) and *remote* (it is on object storage, so it is encrypted and reached by a URL). It is one mechanism for two requirements: it keeps old versions (`R16`), and vaults that never see each other can sync through it (`R24`). A store that held blobs without the log could not be read by anyone but the vault that filled it.

- It has a `config` like any vault: `nickname`, `include`, `exclude`, `keep_history` and `autocompact`. A new one keeps every version and does not compact.
- A run with one is section 10 unchanged. On its side the scan and materialise steps do nothing, and `retain()` is what moves content.
- `fetch` tries each vault's storage after its working files. For a remote vault that is a download, decrypted and then hashed like any other source.
- Every read and write of a vault's log, blobs and config goes through one `Storage` trait with two implementations: a plain directory, and an encrypted object store.
- There is no `store` setting, no automatic upload and no `flush()`. A vault reaches a store with the ordinary commands, by URL or by nickname.

## 9. Materialise

Implements `PRD.md`, "Current state", "Conflict copies", "A sync run" step 6, and `CRDT.md`, "Setting base". Serves `R1`, `R3`, `R4`, `R5`, `R8`, `R9`, `R10`, `R11`, `R14`.

`materialise(vault, peer)` visits every path in the vault's log in sorted order. The peer is only a blob source and may be absent.

### Safety checks

These apply to every write, rename and delete in the working folder. A path that fails one is reported as `skipped` with the reason and left exactly as it was.

- **Scan would skip it.** The path is ignored, has a `.syncit` component or has a file name starting with `.syncit-tmp-`. Such a path is never written. (Not reported; it is simply not this vault's business.)
- **Symlinked parent.** Any existing ancestor directory of the path, below the vault root, is a symbolic link. Check with `symlink_metadata`. Segments come from other vaults and must not be able to write outside this one.
- **Changed since scanned (`R10`).** The path has a row and the file fails the unchanged test and its hash differs from the row's, or the file is missing. The next scan will record the change.
- **Untracked file in the way.** The path has no row and something exists there (`symlink_metadata` succeeds).
- **Filesystem error.** Any I/O error on one path, such as a file where a directory is needed. The run continues with the next path.

### Writing a file

To put content `h` with mtime `m` and executable bit `x` at path `p`: create the parent directories; `fetch(h)` into `.syncit-tmp-<unique>` in the same directory; set its mtime to `m`; if `x`, make it executable; rename it onto `p`; stat `p`. Making a file executable adds execute wherever read is allowed, and always for the owner (`mode | 0o100 | (mode & 0o444) >> 2`), so the umask is respected as `chmod +x` respects it; taking the bit away clears `0o111`. No other permission is touched. The hash is verified before the rename. If the blob is unavailable, remove the temporary file and any directories just created.

### Per path

Let `S = resolve(p)`, `row` the dirstate row, `W` the winner, `h` its hash.

1. `S` is `Absent`: nothing.
2. `S` is `Incomplete`: report `incomplete`. Nothing else.
3. The path is excluded or ignored: nothing. (`R5`. The scan has included every file on disk, section 7, so a path with a row is skipped here only if it is ignored.)
4. **`W` is a tombstone.** If there is a row: run the safety checks, delete the file, remove the row, and remove parent directories left empty, up to the vault root. Report `deleted`. Then, row or not, set the path's deletion record to the heads: with no file here, this vault's state reflects all of them. A path that reaches this step is included, so an included path the vault never had gets a record too.
   **4a. `W` is a file, and the path can be merged** ("Merging", below). Run the safety checks, write the merged text and record it. Report `updated`, or `added` if there was no row, with the note `merged`. Nothing else is done for the path: there are no copies.
5. **`W` is a file, and there is a row with `row.hash == h`.** The content is in place. If `row.exec` differs from the winner's: run the safety checks, set the bit on the file where it is, set `row.exec`, and report `updated`. Nothing is fetched and the mtime does not change. Go to "copies and base".
6. **`W` is a file, and there is a row with a different hash.** Run the safety checks.
   - If `row.hash` is a loser hash and its copy path `c` has no entries in the log: the local file is a conflicting version and must stay visible. If something already exists at `c`, report `skipped` (name clash) and stop. Otherwise write the winner to its temporary name first; rename `p` to `c`; rename the temporary file onto `p`; remove the old row. `c` gets no row.
   - Otherwise the local version is superseded, or its copy was already recorded: write the winner over it.
   - If the winner's blob is unavailable: report `pending`, change nothing, and leave the row and its base alone.
   - On success the row becomes `(size, mtime as stat reports, the winner's exec, h, base)`. Report `updated`. Go to "copies and base".
7. **`W` is a file, and there is no row.** Run the safety checks and write the file. Unavailable: `pending`, and a deletion record stays as it is. On success create the row, remove any deletion record and report `added`. Go to "copies and base".

### Copies and base

Reached only when the winner's content is the file at `p`.

For each loser `l` (a distinct losing hash, with `l.mtime` and `l.exec` taken from the head with that hash that is greatest by `(mtime, id)`), with `c = copy_name(p, l.hash)`:

- If `c` has entries in the log: the version was surfaced before. Nothing to write. (`PRD.md`: a copy the user deleted stays deleted.)
- Else if a file exists at `c`: surfaced if its hash is `l.hash`; otherwise a name clash, not surfaced, reported as `skipped`.
- Else write `l.hash` to `c` with mtime `l.mtime` and executable bit `l.exec`, without a dirstate row. Surfaced on success; report `copied`. Unavailable: not surfaced; report `pending`.

Setting the copy's mtime and executable bit from the entry means vaults that create the same copy independently will record identical entries for it.

Heads with the winner's hash and the other executable bit are not losers: no content is lost, so there is no copy, and the file has the winner's bit.

Then set the row's base to:

- every head whose hash is `h`;
- every tombstone head;
- every head whose hash is a surfaced loser.

Heads of losers that were not surfaced stay out, so the conflict stays open (`CRDT.md`, "Setting base").

### Merging

Implements `PRD.md`, "Merging text", and `CRDT.md`, "Merging". A path is merged when all of these hold; if any fails, the steps for a conflict apply unchanged.

1. The versions it replaces are retained in this run: a vault that receives keeps history. A run computes this once, as `kept` = (the local vault receives and its `keep_history` is not `0`) or (the peer receives and its is not), and passes it to `materialise`. Such a vault calls `retain()` after the exchange and before anything is materialised (section 10, step 5), and `retain()` collects from the sources the merge reads, so every version a merge reads is already in that vault's `.syncit/blobs/`. The test is the setting, not the blobs on disk, because a dry run retains nothing and must decide alike. A remote vault that receives is judged by the same test, on its own `keep_history` (`store.md`, section 7).
2. Its file name has the extension `md`, `markdown` or `txt`, compared without regard to ASCII case, after a non-empty stem. A constant, not a setting.
3. It has exactly two heads, both files, with different hashes.
4. `merge_base(path, a, b)` gives an entry: the heads' one lowest common ancestor, a file. It is a pure function of the log.
5. The content of the base, the winner and the loser can each be read from a blob source (section 8, in the same order as `fetch`), with the right hash, at most 1 MiB, and valid UTF-8. The read is into memory and writes nothing, so a dry run makes the same decision by the same means.
6. `diffy::merge(base, winner, loser)` is clean.

Then, after the safety checks:

- Write the merged text at the path as a file is written (temporary name, mtime, executable bit, rename), whatever the row was.
- Build the entry of `CRDT.md`, "Merging", add it to the log, and write it as a segment of its own. File, then segment, then the dirstate at the end of materialise, as a scan orders them. Stopped in between, the next scan records the file as an ordinary edit, or a second head with the same hash; nothing is lost.
- The row becomes `(size, mtime as stat reports, the entry's exec, its hash, [the entry])`, and any deletion record is removed.

A dry run does all of this in memory and writes nothing. The entry is not counted in `recorded`, which counts what scans wrote.

Limits, accepted:

- A vault without history that merges, because the other vault in a `sync` keeps it, lists the versions replaced as `no content`. They are restored from the other vault.
- A conflict first given copies, by a `pull` into a vault without history, may be merged by a later `sync` with a vault that keeps it. The copy stays, an ordinary file.
- `status` shows a conflict that the next run would merge as `conflict`.

### copy_name

`copy_name(path, hash)` inserts `-` and the first 8 hex characters of the hash into the file name, before the last `.` that is not the name's first character:

| Path | Copy |
|---|---|
| `a/notes.txt` | `a/notes-3f9a1c2e.txt` |
| `Makefile` | `Makefile-3f9a1c2e` |
| `.bashrc` | `.bashrc-3f9a1c2e` |
| `x.tar.gz` | `x.tar-3f9a1c2e.gz` |

### Finishing

Save the dirstate once, after all paths.

## 10. A run with a peer

`sync`, `push` and `pull` share one procedure. "Receiver" means both vaults for `sync`, the peer for `push`, the local vault for `pull`.

1. Open both vaults. Refuse if they are the same folder, if one is inside the other (a run would copy the outer vault into the inner, one level deeper each time), or if either has a version other than 1. Lock both.
2. Scan the local vault, then the peer (section 7). Both scan in every mode, so each vault's changes are recorded from its own dirstate.
3. Exchange segments: each receiver takes every segment in the other vault's `log/` whose name it lacks. A segment is read, validated as in `CRDT.md`, and only then written into `log/`. A segment that fails is not stored, is reported as `rejected`, and does not stop the run.
4. Each receiver reloads its log.
5. Each retaining receiver calls `retain()`. This happens for both before either materialises.
6. Each receiver materialises with the other vault as blob source: the peer first, then the local vault.
7. Collect conflicts from each receiver's log, for paths it includes, and report each once: for the local vault if it is open there, otherwise for the peer.
8. Each vault with `autocompact` on compacts (section 8, "Compaction"). Last, so that no step of the run reads a blob that was just dropped. It prints nothing.

`push` also warns when the local vault has files the peer excludes, `<peer> excludes <n> files (<names>); they were not copied`, and likewise for files the peer ignores, `<peer> ignores <n> files (<names>); they were not copied`. Each is printed once. The names are the top-level folders or files involved, at most five, then `and <n> more`. `sync` and `pull` do not warn; for them a partial vault taking only part is the ordinary case. The warning does not change the exit code.

Scanning precedes exchange, so entries are written from what each vault had before it learned anything new (`PRD.md`, "A sync run").

### Dry run

`sync`, `push`, `pull`, `move`, `drop` and `cp` take `--dry-run`. It is the same procedure with every write to either vault left out; the vault values in memory are updated as usual, so each step sees what the real run would see and prints the same lines. `Options.dry_run` carries the flag.

- Scan: dry mode (section 7).
- Exchange: a segment is validated and its entries added to the receiver's log in memory, not stored. What the other vault's dry scan recorded exists only in its memory, so the receiver also takes every entry of the other vault's in-memory log.
- `retain()` does nothing, and nothing is compacted.
- Materialise: the safety checks run. Where the real run would fetch a blob, the dry run asks only whether a source file for it exists; it does not hash it, so a corrupt source reads as available. Nothing is written, renamed or deleted, and the dirstate is not saved. Dirstate rows are updated in memory.

- `move` (section 11): every check runs, on the in-memory state, so a move that would be refused is refused with the same error. The config and dirstate are not saved and no file is removed; the `moved` events are printed.
- `drop` (section 11): likewise, with `dropped` events. `retain()` does nothing in a dry run, so an edit the dry scan records is not yet in local history: a dry `drop` that relies on local history for that edit is refused where the real run would go through.

The output is the line `dry run: nothing is changed` (an `info` message), then exactly what the real run would print, summary included. The exit code is the real run's. Both vaults are locked and every changed file is hashed, as in a real run.

### No remove

`sync`, `push` and `pull` take `--no-remove`: no receiver removes a file because it was deleted. A file a receiver still has is kept, and recorded as re-created, so the deletion is undone in every vault. `Options.no_remove` carries the flag. Between steps 4 and 5:

1. In each receiver, for every path it takes that has a row and whose winner is a tombstone: replace the row with a deletion record whose base is the heads, and report `added` with the note `kept`. This is the state of a vault whose user put the file back.
2. Scan each receiver that kept something (section 7, "File, deletion record"): the file gets an entry whose parents are the tombstones.
3. Exchange segments again, so that each receiver has the other's re-creations before it materialises. One `sync` therefore brings the file back to the vault that deleted it.

Nothing else changes: materialise finds a file as the winner at those paths.

- `push` writes nothing to the local vault, so a file deleted there comes back at its next `pull` or `sync`; `pull` likewise for the peer.
- A file deleted in every vault of the run stays deleted: there is nothing to keep.
- A file replaced by a newer version is replaced as usual.
- `move`, `drop`, `clone` and `get` do not take the flag.

### A run with a remote vault

Specified in `store.md`, sections 7 and 10.

`sync`, `push`, `pull`, `move`, `get`, `status --vs` and `drop --check` take a *target* where they took a vault's path: a path, a URL, or a nickname in the local vault's `remotes`. The run is the procedure above, unchanged. A remote vault is bare, so its scan and materialise steps do nothing and it is not locked; its `retain()` uploads what it keeps and lacks. Its dirstate is derived from its log and the blobs it holds, so the checks that read a peer's rows need no special case.

## 11. Commands

The vault is found by walking up from the current directory to the first folder containing `.syncit/`. A `<vault>` argument is resolved the same way from the given path. Remote vaults widen it to a *target*: a path, a URL or a nickname (`store.md`, section 9). That section also specifies `init <url>`, `remote`, `clone <url>`, `clone --only`, `get --from`, and `--vault` on `config` and `compact`. Every command except `init` and `clone` takes the vault's lock first: open `.syncit/lock` and `try_lock()`; if that fails, exit with "vault is in use". Paths stored in `config` are canonicalised.

| Command | Steps |
|---|---|
| `syncit init [dir]` | `dir` defaults to `.`. Error if `dir/.syncit` exists. Create `.syncit/` with `log/`, `blobs/`, `tmp/` and a config of `include: [""]`. Existing files are untouched and recorded by the first scan. |
| `syncit clone <src> <dir>` | Find the vault root `R` above `src`; `sub` is `src` relative to `R` (`""` if equal). `dir` must not exist or must be empty. Create it as a vault with `include: [sub]`. Then run `pull` from `R`. Nothing about `R` is recorded; once `remotes` exists, `R` is added to it under its nickname if that name is free (`store.md`, section 9). (`R4`) |
| `syncit sync [--dry-run] [--no-remove] <vault>` | Section 10, both directions. (`R1`, `R12`) |
| `syncit push [--dry-run] [--no-remove] <vault>` | Section 10; only the peer receives. (`R2`) |
| `syncit pull [--dry-run] [--no-remove] <vault>` | Section 10; only the local vault receives. (`R2`) |
| `syncit snapshot` | Scan, `retain()`, then compact if `autocompact` is on. No peer. (`R17`) |
| `syncit status [-a] [path]... [--vs <vault>]` | See below. (`R18`, `R27`) |
| `syncit get <path>...` | See below. (`R7`) |
| `syncit move [--dry-run] <path>... <vault>` | See below. (`R7`) |
| `syncit drop [--check <vault>] [--force] [--dry-run] <path>...` | See below. (`R7`) |
| `syncit cp [--dry-run] <path>... <dest>` | See below. |
| `syncit blame [path]...` | See below. |
| `syncit log [path]...` | See below. (`R26`) |
| `syncit restore [--in-place] <path> <hash>` | See below. (`R26`) |
| `syncit compact [--dry-run]` | See below. (`R15`) |
| `syncit config [keep-history <n>\|all]` | See below. (`R15`) |
| `syncit config autocompact on\|off` | See below. (`R15`) |
| `syncit config nickname <name>` | See below. |

`get`, `move` and `drop` take one or more paths. The steps below run once over all the paths together: `move` and `drop` make their checks for every file before removing anything, so they either remove them all or refuse.

### get

The paths of `get` lie inside the *other* vault, as the source of `clone` does: `syncit get /Volumes/Backup/archive/music`, run in a vault, adds `music` to that vault and fetches it from the vault at `/Volumes/Backup/archive`.

1. Find the vault root above each path. All paths must give the same root `R`, and `R` must not be this vault. Each path relative to `R` is a prefix.
2. Remove every `exclude` at or under each prefix and every `include` under it. Add the prefix to `include`. Save the config.
3. Run `pull` from `R` (section 10).

### move

`syncit move <path>... <vault>`, argument order as in `mv`: the paths are in this vault, relative to the current directory, and the last argument names the target vault. It is `push`, a check, and then the removal of the files from this vault only. No deletion is recorded.

1. Open and lock both vaults, as a run does (section 10). Scan both, so that unrecorded changes are recorded and sent.
2. Collect the paths this vault has a row for under the given prefixes. If there are none, fail.
3. **Filter check.** The target must include each of those paths and must not ignore it. If it ignores any, fail with `nothing moved: <target> ignores <paths>`; if it excludes any, fail with `nothing moved: <target> excludes <paths>`. Nothing has been sent. `move` never changes what the target includes.
4. Continue the run as `push` (section 10, steps 3 to 7).
5. **Arrival check.** For each path, the target's row must now have the same hash as this vault's row. If any does not, fail and list them, each with the target's `skipped` or `pending` event for it, or else "it has a different version" (the target has an edit this vault has not pulled, or a conflict this vault's version lost). Nothing is removed. What was pushed stays pushed; running `move` again after the cause is fixed completes it.
6. Remove every `include` at or under each prefix and every `exclude` under it. Add the prefix to `exclude`. Save the config.
7. Remove the rows and save the dirstate.
8. Delete the files, and directories left empty. A file that fails the unchanged test is left on disk and reported as `skipped` (`R10`); the next scan adopts it. Each removed file is reported as `moved`.

A failure in steps 2, 3 or 5 is exit code 1.

### drop

`syncit drop [--check <vault>] [--force] <path>...` is the end of `move` without the push: the files are removed from this vault only, and sent nowhere. The paths are in this vault, relative to the current directory, and need not exist on disk. No deletion is recorded.

1. Open and lock this vault, and the vault named by `--check` if there is one, as a run does (section 10). Scan both: unrecorded changes here are recorded before anything is removed, and the checked vault's rows say what it has now.
2. Collect the paths this vault has a row for under the given prefixes. There may be none: the prefixes are excluded all the same, which is how to say "never fetch this folder".
3. **Copy check**, unless `--force`. For each of those paths, the row's hash must be in this vault's `.syncit/blobs/`, or in the checked vault: a row of its dirstate with that hash, or a file in its `blobs/`. If any is not, fail with `nothing dropped: no other copy is known of <paths>.` and a hint: `Sync with it first` if a vault was named, `Name a vault that has them with --check <vault>` if not; then `, or use --force`. Exit code 1. This prevents dropping the only copy of a file, including a version the scan in step 1 has just recorded.
4. Steps 6 to 8 of `move`. Each removed file is reported as `dropped`.

Nothing is sent to the checked vault; scanning it may write a segment there, as any scan may. `move` is the command for "put it there, then remove it here"; `drop` is for files another vault already has, or that are not wanted at all.

### cp

`syncit cp [--dry-run] <path>... <dest>`, argument order as in `cp -R`. It copies files into a vault from anywhere: the sources are relative to the current directory, which need not be in a vault, and need not be in one themselves. `<dest>` is a folder in the target vault, found by walking up from it; it need not exist. This is the one command whose vault is not the one at the current directory. No second vault is opened, even if the sources are in one.

1. Open and lock the vault above `<dest>`. Scan it and `retain()`, as `restore --in-place` does, so that an unrecorded edit is recorded before its file is replaced.
2. Collect the regular files at or under each source, without following symbolic links. A source lands in `<dest>` under its last name, a folder with its files below that; a source with no last name (`.`, `..`) lands as its contents. Symbolic links and special files are counted and left out. A folder named `.syncit` is left out. A source that cannot be read fails the command here, before anything is written.
3. For each file, in order: if the vault would not scan its target path (`skips`), count it and go on. Compare its hash and executable bit with the file at the target path. The same: nothing. No file there: `added`. Otherwise: `updated`. The file is copied to a scratch file beside the target (prefix `.syncit-tmp-`), given the current time as its mtime, and renamed onto the target. The write is refused if a parent folder in the vault is a symbolic link.
4. Warn once with the number of links and special files, and once with the number of ignored files.

Nothing is deleted, no entry is written for the copies and the dirstate is not touched: they are unrecorded edits, and the next scan records them as it would files put there by hand, including adding to `include` in a partial vault. The mtime is the time of the copy because the copy is the edit; it also keeps the unchanged test from passing a file that came with its source's mtime. Events carry no arrow. With `--dry-run` step 1 is a dry scan and step 3 writes nothing; the lines and the summary are the real run's.

### blame

`syncit blame [path]...` prints which vaults recorded the current version of each file. The paths are in this vault, relative to the current directory; with none, every path in the log is considered.

1. Open and lock the vault. Nothing is scanned and nothing is written.
2. For each path in the log at or under a given path, in sorted order: `resolve` it. Skip it unless it is `Resolved` with a winner that is a file.
3. Print a `blame` message with the path and the nicknames of the segments holding the winner, sorted. There is usually one. There are several when vaults recorded the identical entry, as they do for a conflict copy each of them created.

It reads the log only. A vault that receives a file records nothing, so it is never named for it; an edit that no scan has recorded is not attributed to anyone. It has no summary and exits 0.

### status

`syncit status [-a|--all] [path]... [--vs <vault>]` prints the state of each file the log knows in this vault, and with `--vs` in the other vault too. Without `--all` it prints only the files that need something. The paths are files or folders in this vault, relative to the current directory, that need not exist on disk; with none, the vault root is listed. A path outside this vault is an error that points to `--vs`. The vault given to `--vs` is resolved as for `sync`.

1. Open and lock the vault, and the other vault as a run does (section 10, step 1).
2. Dry-scan each vault (section 7), and keep what each scan would record. With two vaults, add the other's entries to this vault's log in memory, so that both are compared with the same winner.
3. For each path that is not `Absent`, find each vault's state. The winner is a file with hash `h` or a tombstone. The first rule that applies decides:

   | State | Mark | Rule |
   |---|---|---|
   | `ignored` | `·` | the vault ignores the path (`skips`) |
   | `excluded` | `-` | no row, nothing unrecorded, and the path is excluded |
   | `incomplete` | `!` | the path is `Incomplete` (and see below) |
   | `conflict` | `≠` | the path has losers, or a deletion lost to a file (section 9) |
   | `new`, `modified`, `removed` | `+`, `✎`, `✗` | the dry scan of this vault would record the path: as new, as modified, or as deleted. This `removed` is unrecorded |
   | `current` | `✓` | its row has hash `h`: the file is here, and is the current version. This is narrower than *present* in the glossary, which a file that is `behind` is too |
   | `behind` or `incomplete` | `◐` or `!` | it has a row, and the winner is a tombstone: the next sync removes the file. `behind` if the other vault on the line is `removed`, which says what it is behind; `incomplete` if not, since a vault learns of a removal and removes the file in one run unless the run stops |
   | `behind` | `◐` | its row has another hash |
   | `removed` | `✗` | no row, and the winner is a tombstone. This `removed` is recorded |
   | `missing` | `○` | no row |

   `current`, `excluded`, `ignored` and a recorded `removed` are settled: no scan and no sync changes them. A line is settled if every vault on it is. A path whose winner is a tombstone is listed only if its line is not settled: the log keeps deletions for ever, and a settled one is `log`'s to show. A line with a vault in `conflict` carries the conflict's copies, named as a run names them.
4. For each given path in turn, list what lies under it, in path order. A given path that is a file is one line. A folder below the given path whose files, more than one, are all in the same state (in both vaults) is one line: its path with a trailing `/`, and the number of files. Any other folder is listed child by child, by the same rule. A line in `conflict` is never folded into a folder.
5. Without `--all`, drop the settled lines. This comes after step 4, so a folder's line always counts all its files.
6. Print a `state` message for each line, the paths padded to the widest.

One mark means one state and one word. `removed` is a file gone from the vault, whether or not a scan has recorded that: the two differ only in being settled, and a folder's line never mixes them. `syncit status --help` carries the legend.

`status` writes nothing to either vault, has no summary and exits 0. It is the one command that collects before printing, because a folder's line depends on every file in it, and the width of the path column on every line. `status` alone knows only what this vault has not recorded; what another vault lacks is shown by `--vs`, and what a run would do by `--dry-run`.

### log

`syncit log [path]...` prints the recorded versions of each file. The paths are in this vault, relative to the current directory, and need not exist on disk; with none, every path in the log is considered.

1. Open and lock the vault. Nothing is scanned and nothing is written.
2. For each path in the log at or under a given path, in sorted order: take its valid entries, newest first by `(mtime, id)`. Superseded entries, tombstones and losing heads are all listed.
3. Print a `version` message for each, with the entry's hash (null for a tombstone), its mtime, the nicknames of the segments holding it, `current` (it is the winner `resolve` gives), and `available` (some blob source of this vault has the content: a working file or `.syncit/blobs/`; checked as a dry run checks it, without hashing).

The order is by mtime, which is the file's and can be set to anything; it is a listing, not the order of supersession. It has no summary and exits 0.

### restore

`syncit restore [--in-place] <path> <hash>` brings back a recorded version of one file. `<hash>` is any non-empty prefix of the content hash that matches exactly one of the distinct hashes among the path's valid entries; none or several is an error. The path need not exist on disk.

Without `--in-place`:

1. Open and lock the vault. Nothing is scanned.
2. Let `c = copy_name(path, hash)`. If `c` is ignored: error. If a file is at `c`: with that hash, say so and stop, exit 0; with other content, error.
3. Write the content at `c` as materialise writes a file (safety checks, temporary name, verified, renamed), with the current time as its mtime. It is executable if the newest valid entry with that hash, by `(mtime, id)`, is. No source has it: error.
4. Report `copied`. No row and no entry is written: `c` is an ordinary file that the next scan records, as it does a conflict copy.

With `--in-place`:

1. Open and lock the vault. Scan and `retain()`, so that an edit no scan has recorded is in the log, and kept if the vault keeps history, before it is replaced.
2. If the row has that hash already: say so and stop, exit 0.
3. If there is no row, the path's winner must be a tombstone; otherwise error. With no file here and a live winner, the vault does not have the path, and a scan would take the restored bytes for an old copy (section 7), which the next run would bring forward.
4. Write the content at the path as in step 3 above.
5. Scan and `retain()` again. Over a file this records an ordinary edit whose parents are the row's base; over a deletion, a re-creation whose parents are the deletion record's base. Report `updated` or `added`.

The mtime is now, not the old entry's: restoring is a new edit, and with the old time it would lose a conflict to anything newer. Neither form has a summary; both exit 0 unless they fail. There is no `--dry-run` and no way to name a peer as the source of the content.

### compact

`syncit compact [--dry-run]` drops the stored versions that `keep_history` no longer covers (section 8, "Compaction"). It is how a vault with `autocompact` off is compacted; with it on there is rarely anything left to drop.

1. Open and lock the vault. Nothing is scanned.
2. Compact.
3. Print an `info` message: `dropped <n> versions (<size>)`, or `nothing to drop`.

With `--dry-run` the first line is `dry run: nothing is changed`, nothing is removed, and the message is the real run's. It has no summary and exits 0.

### config

- `syncit config` prints the vault's settings, as `setting` messages: `keep-history: <n>|all`, `autocompact: on|off`, then `nickname: <nickname>`.
- `syncit config keep-history <n>|all` sets `keep_history`: a number of versions, or `all` for `null`.
- `syncit config autocompact on|off` sets `autocompact`.
- A change deletes nothing by itself. If blobs are stored that `keep_history` no longer covers, an `info` line after the settings gives their number and says what will drop them: the next sync or snapshot with `autocompact` on, `syncit compact` with it off.
- `syncit config nickname <name>` sets `nickname`, and fails if the name is not a valid nickname (section 5). Segments already written keep the old one.

Each form prints the settings afterwards and exits 0. Remote vaults add `--vault <target>`, to show or change another vault's settings (`store.md`, section 9).

## 12. Output and exit codes

Three rules:

- **Code prints the moment something happens.** Nothing is collected for printing, so a long run shows its progress and an interrupted one has already said what it did.
- **stdout is the result; stderr is warnings and errors.** A warning never changes the exit code.
- **`--json`, a flag every command accepts, changes the form and not the stream.** Each line of either stream is then one JSON object with a `type` field (JSON Lines).

How it is built:

- A message is a value of the `Message` enum, which has a text form (`Display`) and a JSON form (`Serialize`).
- `print_stdout(value, json)` and `print_stderr(value, json)` print any value that has both forms. They know nothing else.
- `Options` holds the process-wide settings (`json`, and `dry_run` and `no_remove` of section 10) and is passed to everything that can print or write.
- What a run did is kept apart from printing. Each event is printed when it happens and also added to a `Summary`, which holds the events and counts of them. The command returns it; `main` prints the counts as the last line and takes the exit code from them. `move` reads the events to say why a file did not arrive, and tests read them. `init`, `status`, `blame`, `log`, `restore` and `config` have no summary.

### Messages

| `type` | Fields | Text form | Stream |
|---|---|---|---|
| `event` | `kind`, `vault`, `path`, `note` | a marker, a letter and the path, below | stdout |
| `conflict` | `vault`, `path`, `message` | `[<marker> ]U <path> (conflict: <message>)` | stdout |
| `setting` | `name`, `value` | `<name>: <value>` | stdout |
| `blame` | `path`, `nicknames` | `<nickname>[,<nickname>...] <path>` | stdout |
| `state` | `path`, `files`, `ours`, `theirs` when there are two vaults, and `conflict` (the copies) when a vault is in that state | `<mark>[ <mark>]`, the path padded to the widest of the listing, each vault's state as a word padded to 10, then `<n> files` for a folder or the conflict's copies; two spaces between columns, none at the end. The states and marks are in `status`, section 11 | stdout |
| `version` | `path`, `hash`, `exec`, `mtime`, `nicknames`, `current`, `available` | `<first 8 of hash> <UTC time> <nickname>[,…] <path>`, then in brackets any of `current`, `deleted`, `no content`, `executable`; a deletion's hash is `--------`; the time is `YYYY-MM-DD HH:MMZ` | stdout |
| `info` | `message` | the message | stdout |
| `summary` | the counts | `recorded 3, added 1, …` | stdout |
| `warning` | `message` | `warning: <message>` | stderr |
| `error` | `message` | `error: <message>` | stderr |

An event's text form is a marker, one letter, then `path`, the file's path in its vault. The marker says whose vault the file is in: `->` for the local vault, the one the command runs in, and `<-` for the other. Every command with a peer prints it (`sync`, `push`, `pull`, `clone`, `get`, `move`); `snapshot` involves one vault and prints no marker. `clone` runs in the new vault, so the new folder's lines are `->`. The text form does not name the vault; the JSON form has `vault` and no marker. The letters are those of `git status --short` where git has one:

| Letter | `kind` |
|---|---|
| `A` | `added` |
| `M` | `updated` |
| `D` | `deleted` |
| `C` | `copied`: a conflict copy was written |
| `R` | `moved` or `dropped`: `move` or `drop` removed the file from this vault |
| `!` | anything that needs attention: `skipped`, `pending`, `incomplete`, `rejected` |

Anything more goes in parentheses after the path. A file whose conflicting versions were merged (section 9) is `M notes.md (merged)`. A `!` line gives its kind and reason: `(skipped: changed since it was scanned)`. A rejected segment has no file, so its path is `.`, the vault itself: `-> ! . (rejected: segment 77b0…: not in canonical form)`.

```
<- M notes.txt
-> A foo/new.txt
-> D old.txt
-> C notes-3f9a1c2e.txt
-> ! big.bin (pending: content not available from any source)
-> ! draft.txt (skipped: changed since it was scanned)
-> ! x.txt (incomplete: waiting for missing history)
-> U notes.txt (conflict: notes-3f9a1c2e.txt)
-> U gone.txt (conflict: deleted in another vault)
recorded 3, added 1, updated 1, deleted 1, conflicts 2, pending 1, skipped 1
```

The same run's first and last lines with `--json`:

```
{"type":"event","kind":"updated","vault":"/Volumes/Backup/work","path":"notes.txt","note":""}
{"type":"summary","recorded":3,"added":1,"updated":1,"deleted":1,"conflicts":2,"pending":1,"skipped":1,"incomplete":1,"rejected":0}
```

- Lines come in the order things happen: each vault's events as it is materialised, then the conflicts, then the summary. They are not sorted.
- A conflict's message lists every copy name, and `deleted in another vault` when the kept file beat a deletion. It is printed once per path, and repeats on every run until the conflict is closed.
- `recorded` counts entries written by scans. The text summary leaves out `incomplete` and `rejected`; the JSON one has them.
- The warnings are: symbolic links and special files skipped, a name that is not UTF-8, a vault inside this one, an invalid segment in a peer's log, and the `push` warning of section 10.
- A failure prints one `error` and nothing after it.
- Bad command-line arguments are reported by the argument parser as text on stderr, with or without `--json`.

`status` prints `state` messages only: an open conflict is a line in the state `conflict`, not a `conflict` message.

| Exit code | Meaning |
|---|---|
| 0 | Completed; nothing needs attention. `init`, `status`, `blame`, `log`, `restore` and `config` always exit 0 unless they fail. |
| 1 | Failed: bad arguments, not a vault, vault in use, version mismatch, corrupt local file, a refused `move` or `drop`. |
| 2 | Completed, but there are conflicts or pending, skipped, incomplete or rejected items. |

## 13. Interruption

`R14`: stopping a run at any point leaves both vaults valid, and running again completes the work. The lock is released by the operating system when the process dies.

| Stopped | State left | Why the next run is correct |
|---|---|---|
| after writing a segment, before saving the dirstate | entries exist that the dirstate does not reflect | the scan writes the identical entries again (section 7) |
| during segment exchange | some segments copied; possibly a child without its parent | segments are whole or absent; such paths are `Incomplete` and wait |
| while fetching a blob | a `.syncit-tmp-*` file | never renamed into place; the next scan deletes it |
| after replacing files, before saving the dirstate | files newer than their rows | the scan re-records them as entries with the winner's hash and mtime: either the same entry or a head with the same hash, which is the same version. Files with no row are adopted; one written over a deletion record is re-recorded with the winner's hash and mtime, which is again the same version |
| after renaming a loser to its copy name, before placing the winner | the path is missing, the copy has no row | the scan records a tombstone and the copy; the winner still beats the tombstone, and the losing content is in the copy |
| after deleting a file, before removing its row | a row with no file | the scan writes a second tombstone; all heads are tombstones. The deletion record names only the second until the next materialise sets it to both |
| during `move` or `drop`, after saving the config, before saving the dirstate | present files under an excluded prefix | the scan includes each file on disk again, one include per file; nothing is lost, and running the command again completes it and leaves one exclude (`interrupted_move`) |
| during `move` or `drop`, after saving the dirstate | files with no rows | the scan adopts and includes them; nothing is recorded as deleted |
| while compacting | some of the blobs to drop are gone and others are not | each removal is whole; the next compaction removes the rest. The log was not touched |
| while uploading to a remote vault (`store.md`, section 10) | some blobs or segments are there and others are not | every object is whole or absent; a reader sees a path as `pending` or `incomplete` and waits; the next run uploads the rest |

## 14. Tests

Unit tests live beside the code. Integration tests live in `tests/` and build vaults in temporary directories through the library's command functions. Tests set file mtimes with `File::set_modified` where order matters and never sleep.

### Log (milestones 1 and 2)

| Test | Asserts |
|---|---|
| `entry_id_vector` | the test vector in section 5 |
| `canonical_escaping` | a path containing `"`, `\`, U+0001, U+000A, `é`, `/` and U+007F encodes per section 5 and round-trips |
| `segment_round_trip` | encode then decode returns the entries; entries are sorted by path; the name is the hash |
| `header_rejects` | a segment whose first line lacks the nickname, has another field, is out of key order, has whitespace, or has a nickname that is empty, over 64 bytes, not a string, or contains a space or a control character |
| `nickname_default` | the folder name becomes a valid nickname: spaces replaced, cut to 64 bytes on a character boundary, `vault` when empty |
| `segment_rejects` | one case per bullet of `CRDT.md` "Validation": wrong name, version 2, extra field, missing field, wrong type, whitespace, key order, uppercase hex, absolute path, `..`, empty component, `.syncit/x`, unsorted parents, duplicate parents, missing final newline |
| `resolve_worked_cases` | every row of `CRDT.md` "Worked cases" that concerns the log |
| `resolve_order_independent` | the same entries split into segments in several ways and loaded in several orders give the same `PathState` for every path |
| `resolve_incomplete` | a missing parent gives `Incomplete`; adding it gives the expected heads |
| `resolve_cross_path_parent` | an entry with a parent of another path is never a head and supersedes nothing |
| `winner_order` | file beats tombstone regardless of mtime; later mtime wins; the id breaks a tie |
| `resolve_exec` | heads with one hash and different executable bits are not a conflict; a loser has the bit of its newest head |
| `executable_tombstone_rejected` | a tombstone with `exec` true does not decode |
| `copy_name_cases` | the table in section 9 |
| `includes_cases` | longest prefix decides; `foo` does not match `foobar`; nothing matching means excluded |

### Stories (one per user story in `PRD.md`)

| Test | Story | Asserts |
|---|---|---|
| `story_a1_edit_and_push` | A.1 | clone, edit in the copy, push; the archive has the edit |
| `story_a2_add_and_push` | A.2 | a file added in the copy appears in the archive; a deletion propagates |
| `story_a3_move` | A.3 | given two paths in one call, `move` sends them to the archive and removes the local files; after sync the archive still has them; `get` of their paths in the archive brings them back |
| `story_a4_clone_subfolder` | A.4 | cloning `a/foo/bar` gives `b/foo/bar/…` only; files outside it are not deleted from `a` by a sync |
| `story_a5_get_and_new_folder` | A.5 | `get` of a second folder; a new top-level folder in the copy reaches the archive, is added to `include`, and a file later added to it in the archive arrives in the copy; the three auto-include examples of section 7 |
| `story_b1_conflict` | B.1 | both sides edit; after sync both have the same winner and the same copy; exit status reports the conflict; editing the winner and syncing closes it everywhere |
| `story_c1_three_vaults` | C.1 | three vaults synced in several pairings and orders converge; a change relays through a middle vault |
| `story_c2_sync_through_remote` | C.2 | specified in `store.md`, section 12. Two vaults that never sync with each other converge through one remote vault, in either order, including a conflict; a vault with no history supplies its current files; a third machine clones the URL and has everything |
| `story_d1_no_history` | D.1 | with `keep_history` 0, `.syncit/blobs/` stays empty after syncs |
| `story_d2_history` | D.2 | with `keep_history` all, every version's blob is present after edits and syncs, and a version that is nowhere else is restored from it; set to 0, nothing more is collected and the next run drops what is no longer current |
| `story_d3_snapshot` | D.3 | two snapshots without a peer give two entries in a chain; a later sync delivers both |
| `story_e1_rerun` | E.1 | running sync twice changes nothing the second time and writes no segment |

### Safety and recovery

| Test | Asserts |
|---|---|
| `keep_cases` | which versions `Log::keep` keeps: a chain at depths 0, 1, 2 and all; a deletion counts as a version; a conflict and an incomplete path keep everything; a closed conflict keeps both versions it replaced at depth 1; content two paths share; only the paths selected |
| `history_depth` | a new vault keeps one version back, and two when set so; the log keeps every entry and `log` lists the dropped versions as `no content`; a sync with a vault that keeps all does not bring them back, and that vault collects what this one still has; a deleted file's last content is kept and restored |
| `compact_command` | with `autocompact` off old blobs pile up; `compact --dry-run` reports and drops nothing; `compact` drops them and leaves the log as it was; a path in conflict keeps every version; at `keep_history` 0 the current versions stay, among them a file removed with `drop`; with `autocompact` on a snapshot compacts |
| `merge_base_cases` | the lowest common ancestor of two entries: concurrent edits, from further down each side, the lowest of several; none for independent creations, a criss-cross, or a deletion |
| `mergeable_names` | which file names are merged |
| `merge_text_conflict` | edits to different lines of a text file in two vaults, with the earlier version in one vault's history: both vaults have the merged text, no copy and no conflict; the path has one head whose parents are both edits, which stay in the log and can be restored; a second sync does nothing |
| `merge_on_pull` | a `pull` into a vault that keeps history merges there, changes nothing in the other vault, and leaves both versions replaced in its `.syncit/blobs/` |
| `merge_falls_back` | an ordinary conflict with a copy when the edits overlap, the file is not text, no vault keeps history though the earlier version can be read from another file, or only the vault that is not receiving keeps it; an edit against a deletion is as before |
| `edit_beats_delete` | edit on one side, delete on the other: the file survives everywhere and the delete is reported |
| `same_edit_no_conflict` | identical content written on both sides is not a conflict |
| `new_file_at_existing_path` | a file created in a partial vault at a path another vault already has: equal content is adopted or merges silently, different content is an ordinary conflict with a copy |
| `recreate_after_delete` | a file deleted, synced, and created again at the same path, with an old version's bytes or new ones, in the vault that deleted it or the one that received the deletion, and with the dirstate lost: it stays in both vaults as one head whose parents are the tombstones, with no conflict; both vaults have a deletion record in between |
| `no_remove_keeps_deleted` | with `--no-remove`, a file deleted in one vault stays in the other and is back in the first after one `sync`, as one head whose parents are the tombstones, and the dry run says the same; after a `push` it comes back at the next `sync`; a file deleted in both vaults stays deleted |
| `recreate_against_unseen_recreation` | a path deleted here, re-created in another vault whose content has not arrived, then re-created here with an old version's bytes: a conflict with a copy, not an old file brought forward |
| `status_states` | `status` in a partial vault and against the archive: current, behind, missing, excluded and ignored, in ours and in theirs; an unrecorded edit, new file and removal are `modified`, `new` and `removed` in the vault that has them; a file removed in one vault is `behind` in a vault that still has it, and `incomplete` with no such vault on the line; without `--all` only the lines that are not settled, and nothing for a vault with nothing to do; a folder in one state is one line with a count; a path argument lists that folder's children or that file; a recorded removal that no vault on the line still has the file of is not listed; neither vault is changed |
| `log_lists_versions` | `log` lists a path's entries newest first with the recording vault, marks the current one, lists a deletion, finds a path with no file, and reports content as available only where a working file or a kept blob has it |
| `restore_version` | `restore` writes the copy and leaves the file alone; again is a no-op; the copy syncs as a new file and, moved over the file, as an edit; `--in-place` records unrecorded edits first and then the old version on top of them; over a deleted file it stays in both vaults with no conflict; refused for an unknown or empty hash, content not kept, a taken copy name, and a path not on disk here |
| `lost_dirstate` | delete `.syncit/dirstate`, sync: no entries written for unchanged files, no deletions invented, old files brought forward; a known file with the other executable bit is recorded as a change to the version the log has |
| `exec_bit_syncs` | `chmod` alone is recorded as a change and arrives without rewriting the file or its mtime, in both directions; new and edited files are written with their bit |
| `exec_change_vs_edit` | `chmod +x` against a concurrent edit: the edit wins, the conflict copy is executable in both vaults and recorded as one entry |
| `restore_keeps_exec` | a restored copy of a version that was executable is executable |
| `copied_vault` | copy a vault folder with `cp -R` semantics, edit both, sync all: an ordinary conflict, nothing lost |
| `restored_backup` | restore a vault to an old state, edit, sync: conflict with the newer head, nothing lost |
| `pending_then_edit` | winner's blob unavailable, then a local edit: the result is a conflict, not an overwrite |
| `changed_during_run` | a file modified after the scan is not replaced (`R10`); the next run records it |
| `untracked_in_the_way` | a file created at an incoming path after the scan is not overwritten |
| `same_tick_edit` | an edit that keeps size and mtime right after a scan is still detected |
| `interrupted_before_dirstate` | restore the old dirstate after a snapshot, snapshot again: the set of entries is unchanged |
| `interrupted_transfer` | remove a parent's segment from the receiver: the path is incomplete and untouched; a re-sync resolves it |
| `hostile_segment` | a peer's log containing an invalid segment: it is rejected, the rest of the sync completes |
| `symlink_escape` | an incoming path under a local symlinked directory is skipped and nothing outside the vault changes |
| `ignored_files` | ignored files are not recorded, not deleted, and not overwritten; a `.syncitignore` that lists itself stays local |
| `ignore_file_syncs` | a `.syncitignore` created in one vault arrives in the other and its rules apply there from the next run; edits to it in two vaults are an ordinary conflict |
| `corrupt_blob_source` | a peer file changed behind its dirstate is not installed (`R11`) |
| `copy_deleted_stays_deleted` | deleting a conflict copy and syncing does not bring it back |
| `move_refuses` | `move` refuses, removing nothing, when the target excludes a path (and then sends nothing), when the target has a different version of a file, when no files lie under the paths, and for a vault with itself; an unrecorded edit under the path is recorded, sent and moved |
| `drop_refuses_sole_copy` | `drop` refuses without `--check` when the vault keeps no history; an unrecorded edit under the path is recorded as an entry, and then blocks the drop against the checked vault until it is pushed; a file the checked vault has lost since its last scan blocks it too; with two paths given, nothing is dropped; nothing reads as deleted afterwards; a path with no files is excluded; local history counts as another copy; a vault cannot check itself; `--force` overrides |
| `cp_from_plain_folder` | `cp` from a folder that is not a vault adds what the vault lacks, replaces what differs and leaves the rest; the vault is scanned first; ignored files, links and the source's `.syncit` are not copied; a read-only executable arrives executable; the copies show in `status` as unrecorded; `.` copies contents and a named source lands under its name in a folder that need not exist; a missing source copies nothing; a dry run writes nothing and returns the real run's `Summary` |
| `config_command` | `config` prints the settings; a new vault has `keep_history` 1 and `autocompact` on; `keep-history` and `autocompact` are set and saved, and the setting takes effect; a vault's nickname is its folder's name until `config nickname` changes it, and an invalid nickname is refused |
| `blame_names_the_recording_vault` | `blame` names the vault that recorded each file's current version and never one that only received it; an identical entry recorded by two vaults names both; deleted files are left out; paths limit the output; after a rename new changes carry the new nickname and old ones keep the old; an unrecorded edit is not attributed |
| (remote vaults) | the tests listed in `store.md`, section 12: `crypt_*`, `names_are_keyed`, `phrase_round_trip`, `key_lookup_order` and `secrets_env_file` in `src/`, the rest in the `remote` module of `tests/stories.rs` and in `cli_smoke` |
| `version_mismatch` | a peer with `"syncit": 2` in its config is refused |
| `cli_smoke` | the built binary runs every command and returns the documented exit codes; results are on stdout and warnings and errors on stderr; with `--json` every line of both streams is an object with a `type`, and the last stdout line of a run is the summary; `push` to a partial vault warns and `sync` does not |
| `message_forms` | each message type's text form and JSON form; the summary's counts and exit code |
| `push_counts_excluded` | the files a push would not deliver: those the target excludes, and those it ignores |
| `interrupted_move` | an exclude over files the vault still has, as a `move` stopped partway leaves: the next scan includes them again, they sync as before, and a second `move` completes and leaves one exclude |
| `dry_run_matches_real_run` | for `sync`, `push` and `pull` over edits, a new file, a deletion, a conflict and a merge of text, and for `move` and `drop`: a dry run leaves every file of both vaults, `.syncit/` included, as it was, and returns the same `Summary` (counts and events) as the real run that follows; a `move` or `drop` that would be refused is refused |

### Requirement coverage

| Requirement | Section | Tests |
|---|---|---|
| R1, R3 | 9, 10 | `story_a1`, `story_a2`, `story_b1` |
| R2 | 10, 11 | `story_a1`, `story_a2` |
| R4, R6 | 5, 7, 11 | `story_a4`, `story_a5` |
| R5, R7 | 7, 9, 11 | `story_a3`, `story_a4`, `lost_dirstate` |
| R8, R9 | 6, 9 | `story_b1`, `winner_order`, `edit_beats_delete` |
| R10 | 9 | `changed_during_run`, `untracked_in_the_way`, `same_tick_edit` |
| R11 | 8 | `corrupt_blob_source` |
| R12 | 6, 10 | `story_c1`, `resolve_order_independent` |
| R13 | 8 | `story_d1` |
| R14 | 13 | `story_e1`, `interrupted_before_dirstate`, `interrupted_transfer` |
| R15 | 8, 11 | `story_d2`, `config_command`, `keep_cases`, `history_depth`, `compact_command` |
| R16, R24, R28 | 8, 10; `store.md` | `story_c2_sync_through_remote`, `remote_is_opaque`, `remote_history_depth` and the other tests of `store.md`, section 12 |
| R17 | 7, 11 | `story_d3` |
| R18 | 11, 12 | `cli_smoke`, `story_b1` |
| R20 | 5, 7 | `ignored_files`, `ignore_file_syncs` |
| R25 | 5, 10 | `version_mismatch`, `segment_rejects` |
| R26 | 7, 11 | `log_lists_versions`, `restore_version`, `recreate_after_delete` |
| R27 | 11 | `ls_states` |

## 15. Milestones

Each milestone ends with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passing.

- [x] **1. Entry and segment codec.** `entry`, `segment`. Tests: `entry_id_vector`, `canonical_escaping`, `segment_round_trip`, `segment_rejects`.
- [x] **2. Read rules.** `log` with `resolve`. Tests: the `resolve_*` tests, `winner_order`.
- [x] **3. A single vault.** `config`, `dirstate`, `vault`, `scan`; commands `init`, `snapshot`, `status`. Tests: `includes_cases`, `story_d3_snapshot` (without the sync step), `same_tick_edit`, `lost_dirstate` (single-vault part), `interrupted_before_dirstate`, `cli_smoke`.
- [x] **4. Sync without conflicts.** Blob fetch from working files, segment exchange, materialise steps 1–4 and 7 and the non-conflict part of 6; commands `sync`, `push`, `pull`, `clone`. Tests: `story_a1`, `story_a2`, `story_e1`, `story_c1`, `changed_during_run`, `untracked_in_the_way`, `corrupt_blob_source`.
- [x] **5. Conflicts.** Conflict copies, base, incomplete paths. Tests: `story_b1`, `copy_name_cases`, `edit_beats_delete`, `same_edit_no_conflict`, `new_file_at_existing_path`, `copied_vault`, `restored_backup`, `pending_then_edit`, `copy_deleted_stays_deleted`, `interrupted_transfer`.
- [x] **6. Partial vaults.** The include filter in materialise, `clone` of a sub-folder, `get`, `move`. Tests: `story_a3`, `story_a4`, `story_a5`, `move_refuses`.
- [x] **7. Local history.** `.syncit/blobs/`, `keep_local_history`, `retain`, `config`. Tests: `story_d1`, `story_d2`, `config_command`.
- [x] **8. Hardening.** Locking, ignore file, symlink checks, version checks, output format and exit codes. Tests: `hostile_segment`, `symlink_escape`, `ignored_files`, `version_mismatch`, and the remaining parts of `lost_dirstate` and `story_d3`.
- [x] **8a. Nicknames.** The segment header with a nickname, the `nickname` setting and `config nickname`, the log's record of which vaults recorded each entry, and `blame`. Tests: `header_rejects`, `nickname_default`, `blame_names_the_recording_vault`, and the nickname parts of `config_command`, `message_forms` and `cli_smoke`.
- [x] **8b. Restore.** Deletion records in the dirstate (sections 5, 7 and 9), the `version` message, and the commands `log` and `restore`. Tests: `recreate_after_delete`, `recreate_against_unseen_recreation`, `log_lists_versions`, `restore_version`, and the `version` parts of `message_forms` and `cli_smoke`.
- [x] **8c. One vocabulary for partial replication.** Included, excluded, held and ignored (the glossary) in code, messages and documents; the scan includes every file on disk, with or without a row (section 7). Tests: `interrupted_move`, `push_counts_excluded`, `includes_cases`.
- [x] **8d. ls.** The `holding` message and the command `ls`, with its `--help` legend. Tests: `ls_states`, and the `holding` parts of `message_forms` and `cli_smoke`.
- [x] **8e. One status.** `ls` is folded into `status`: one state and one mark for each case, unrecorded changes, conflicts and incomplete paths as states, `--all`, and the text form in columns. Tests: `status_states` (in place of `ls_states`), and the `holding` parts of `message_forms` and `cli_smoke`.
- [x] **8f. Have, present, dirstate, state.** *Held* is retired: a vault *has* a file, which is then *present*; the record of it is a row in the *dirstate*, the file once called the cache; the `holding` message is `state`. A rename, with no change of behaviour. Tests: `lost_dirstate` and `interrupted_before_dirstate` (renamed), and the `state` parts of `message_forms` and `cli_smoke`.
- [x] **8g. Merging text.** `merge_base`, reading a blob into memory, and the merge step of materialise (section 9, "Merging"). Tests: `merge_base_cases`, `mergeable_names`, `merge_text_conflict`, `merge_on_pull`, `merge_falls_back`, and the merge in `dry_run_matches_real_run`.
- [x] **8h. History depth and compaction.** `keep_history` and `autocompact` in place of `keep_local_history`; `Log::keep`; `retain()` limited to it and to the paths the vault takes; compaction at the end of a run, and the command `compact` (section 8, "Retaining" and "Compaction"). The log is not compacted. Tests: `keep_cases`, `history_depth`, `compact_command`, and the changed parts of `story_d1`, `story_d2`, `config_command` and `cli_smoke`.
- [x] **9. Remote vaults.** Everything in `store.md`, in the eight milestones of its section 13. This takes the place of the earlier milestones 9 (directory stores) and 10 (S3 stores).

## 16. Decisions and open questions

### Decided here

These settle, for version 1, questions that `PRD.md` lists as open.

| Question | Answer |
|---|---|
| Store | a vault that is bare and remote, not a second kind of thing (`store.md`): one mechanism for keeping history (`R16`) and for syncing through a third place (`R24`). On any object storage the `object_store` crate reaches, and always encrypted (`R28`) |
| Origin | none. No vault is special and there is no default peer: a command that involves another vault names it, by path, URL or nickname. A vault may remember others in `remotes`, an address book of nickname to location. A folder that moves breaks only its own entry |
| A partial vault from a store | `clone <vault> <dir> --only <path>`, repeatable, with paths from the vault's root. A URL cannot carry a sub-folder as a path does, so the flag names it. The form that infers the sub-folder from a path stays (`store.md`, section 9) |
| Reaching a store | with the ordinary commands and a target. No `store` setting and no upload after every run: a command does what its name says, and backing up on a schedule is `push` in cron |
| Size of the build | everything remote vaults add is behind two Cargo features, `remote` and `keychain`, on by default. Without them the build is as small as before. No feature for each cloud: they share their dependencies |
| Object-store client | the `object_store` crate, driven from a private single-thread async runtime. No external tools. OpenDAL was ruled out because it needs a newer Rust than 1.89 |
| Store encryption | always on. XChaCha20-Poly1305 in 64 KiB chunks. An object's name is a keyed function of its content's hash, so the provider learns neither the content nor whether a known file is stored |
| Store keys | one 32-byte key for each remote vault, written as a 24-word BIP39 phrase and shown once, at creation. Kept in the OS keychain when there is one; otherwise given in `SYNCIT_SECRET_KEY` or, if the user chooses, saved in `.syncit/secrets.env` |
| Settings | changed with `syncit config`, not by editing the file |
| Attribution | each vault has a nickname, by default its folder's name, written into the header of the segments it records; `blame` prints it. A label: not unique, not verified, read by no rule. Hostnames and absolute paths are not recorded, since the log is permanent and replicated |
| Ignore list | `.syncitignore` at the vault root, an ordinary file that syncs; names and path prefixes, no wildcards |
| `move` safety | scans first, so unrecorded changes are sent; removes nothing unless the target includes every file and, after the push, has the same content at every path |
| `drop` safety | scans first, so unrecorded changes are recorded; refuses unless the content is in local history or in the vault named by `--check`, which is scanned too; `--force` overrides |
| File metadata | regular files only; symlinks skipped with a warning. The executable bit is recorded, as Git does, and a change to it alone is a new version. Other permissions, ownership and extended attributes are not recorded. No setting for filesystems without the bit |
| File mtimes | a materialised file gets its entry's mtime; a restored one gets the current time |
| Re-creating a deleted file | a new version that replaces the tombstones the vault had seen, kept in a deletion record in the dirstate. With no record, a file at a deleted path is still kept as a new version: when it cannot be decided, data is retained |
| Unicode paths | recorded as the OS returns them; names that are not UTF-8 are skipped with a warning |
| Case-insensitive filesystems | no special handling; a case collision surfaces as "untracked file in the way" and nothing is overwritten |
| Conflict-copy name clash | the existing file is left alone; the conflict is reported and stays open |
| A file on disk outside the included paths | added to `include` by the scan, whether new or already with a row (section 7): the filter follows the disk, so data is kept. `move` and `drop` undo it |
| Seeing a partial vault | `status --all`: each vault's state for every live path, against one merged log; states are per vault, so nothing is pairwise. No sizes, since entries do not record them |
| Terms for partial replication | three independent facts, one word each: included or excluded (the filter), present (the disk, which the dirstate records), ignored (`.syncitignore`). Messages name the one responsible; no word blends them |
| Merging conflicting files | text only (`.md`, `.markdown`, `.txt`), only in a run where a receiving vault keeps history, so the versions replaced can be restored, and only when a line-by-line three-way merge from their common ancestor is clean; otherwise copies. The merge is an entry with both versions as parents. `PRD.md` first listed merging as a non-goal |
| History | a depth, not a switch: `keep_history` is how many versions back from a head a vault keeps the content of; 0 is none and `null` is all. A new vault keeps 1, which is enough for most merges of text, at the cost of a second copy of every current file |
| Compaction | Compaction keeps the whole log and drops only old blobs. As CouchDB's does: it drops the bodies of revisions that are not leaves and keeps the revision tree. Automatic at the end of a run unless `autocompact` is off; then `syncit compact`. Per vault, with nothing to coordinate: a vault collects only what it keeps, so a peer that keeps more does not fill it up again |
| Tie-break timestamp | file mtime |
| Empty directories | not tracked; directories emptied by a deletion are removed |
| Cross-path parents | a read rule, not a segment rejection (`CRDT.md` was corrected to match) |

### Still open, not for version 1

- Wildcards in the ignore file, and per-folder ignore files.
- A stored heads file, if loading the whole log becomes slow.
- Compaction of the log. Blobs are compacted (section 8); what cutting the log would need is in `CRDT.md`, "Compaction".
- Signed segments, and vaults reached over a network (SSH, iroh). Remote vaults (`store.md`) cover the case of syncing through a third place. Nicknames give unverified attribution meanwhile; `CRDT.md`, "Planned extension", lists what signing still has to decide.
- A `size` field in entries.
- Unicode normalisation of paths.
- A guard against mass deletion (`R23`). A whole vault on an unmounted drive cannot trigger one, because `.syncit/` is missing too; a missing mount *inside* a vault can.

## 17. Deviations

Record here anything built differently from these documents, with the reason.

- **Remote vaults (milestone 9, `store.md`) are built.** MinIO's images could not be pulled, so `store.md` milestone 7 ran `cloud_remote` against Adobe's S3Mock (`adobe/s3mock:3.11.0` in Docker) through `AWS_ENDPOINT_URL` and `AWS_ALLOW_HTTP`, and pushed and cloned a 20 MB file, which goes in 16 MiB parts. Both passed with no change to the code. No real cloud bucket was used, and GCS and Azure are untested. The crate APIs behaved as `store.md`, section 2, records; nothing had to be adapted, and the default TLS provider built.
- **`store.md` as built.** Where it left a choice, or where building it showed something:
  - `Log::load` takes a vault's storage instead of its `log/` folder, so the test helper that loads a log passes a `Dir`; no assertion changed. A corrupt segment is reported by its name, not its file's path.
  - Commands take a target as a `&Path`: a URL or a nickname is a valid path string, read back with `to_str`. `clone`, `compact` and `config` gained the parameters for `--only`, `--save-key` and `--vault`; the tests pass the defaults.
  - `Storage::put_blob` consumes its source file, which is a scratch file in every caller.
  - `init <url>` and `clone` take `--save-key` as `remote add` does, since section 8 lets all three save a key. `init --nickname` is for a remote; with a path it is refused.
  - `clone` records its source in `remotes` whether it is a URL or a folder. If that fails (no key for a URL), the new `.syncit/` is removed and nothing is pulled.
  - `remote add` with a path needs the root of a vault. A target that is no nickname, no URL and no existing path fails with `no vault at <target>: not a folder, a URL or the name of a remote`, where it used to fail in the nesting check.
  - A remote object that cannot be decrypted, or whose content does not match its name, is listed under its object name, so the run reports it as `rejected` as it would a corrupt segment.
  - The `sent …` and `received …` lines count distinct blobs and segments. In a dry run, received files are those a dry run finds only in the remote; history that a real `retain` would download is not counted.

- **A peer's log is read leniently.** Section 6 says a local segment that fails validation stops the command; section 10 says a bad segment in the other vault is rejected and does not stop the run. Both cannot hold for the peer, which must load its own log to scan. A vault opened as a peer skips its invalid segments with a warning; the receiving vault reports them as `rejected`. The vault a command runs in is still strict.
- **`.syncit` is matched without regard to ASCII case** by the scan and by materialise, at any depth. A valid segment may name `.SYNCIT/x`, which on a case-insensitive filesystem is this vault's own `.syncit/`. Segment validation is unchanged (the format is not touched); the path is simply never written.
- **An interrupted scan may be re-recorded as a smaller segment.** Section 7 says the next scan writes the same entries again. A new file whose entry already reached the log is instead recognised by the "file, no row" rule and adopted, so the second segment can hold fewer entries. The set of entries is the same (`interrupted_before_dirstate`).
- **`drop` was removed, then restored beside `move`.** `move` replaced it because its check could be stale and the usual task is "put it there, then remove it here". Two uses went with it: saying "never fetch this folder" with no other vault involved, and discarding files no other copy is known for. `drop` is back for those (section 11), and now scans the vault named by `--check`, so that vault's rows are current when they are read.
- **A conflict copy deleted before any scan recorded it can come back** from the other vault, where the same copy was recorded as an ordinary new file. "A copy the user deleted stays deleted" holds from the first run after the copies were created (`copy_deleted_stays_deleted` syncs once before deleting). This follows from the documents as written; it is noted, not fixed.
