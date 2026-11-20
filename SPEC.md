# syncit: implementation specification

This is the entry point for building syncit. Given this file alone, you should be able to implement version 1 by following its links and its milestones.

## 1. Start here

### What syncit is

syncit is a command-line tool, written in Rust, for two-way sync and partial replication between folders. A folder becomes a *vault* by gaining a `.syncit/` directory. Any vault can sync with any other. A vault may hold only part of the files. Conflicting edits are kept side by side as visible files. Keeping old versions is optional per vault.

### State of the repository

Three documents and no code:

| Document | Read it for | Authority |
|---|---|---|
| `SPEC.md` (this file) | how to build it: formats, procedures, commands, tests, milestones | wins on implementation detail |
| @CRDT.md | the log: entry fields, segment format, canonical form, validation, read rules, write rule, `base` | wins on anything about the log |
| @PRD.md | why the tool exists, user stories, requirements `R1`–`R25`, rationale for rejected designs | wins on user-visible intent |

Read `CRDT.md` in full before milestone 1 and `PRD.md` in full before milestone 3. This file does not repeat the log format; it links to it. If two documents disagree, follow the authority column, and record the disagreement under "Deviations" (section 17).

### The model in brief

Three things are kept separate. Each can be partial, lost or rebuilt without corrupting the others.

- **The log** says what happened. It is a grow-only set of entries, merged between vaults by set union. Current state is computed from it and never stored.
- **Blobs** are file contents, addressed by SHA-256. Any source may hold any subset.
- **The working folder** is ordinary files. The **stat cache** records which version of each file this vault has on disk.

A sync run, done by each vault on its own folder (`PRD.md`, "A sync run"):

1. Scan the folder against the cache and write new entries as one segment.
2. Retain blobs for the new entries, if the vault retains blobs.
3. Exchange log segments.
4. Compute current state from the merged log.
5. Fetch the blobs needed.
6. Materialise: write files, apply deletions, write conflict copies, update the cache.

### Glossary

| Term | Meaning |
|---|---|
| vault | a folder containing `.syncit/` |
| entry | one log record: a path had some content (`hash`, `mtime`, `parents`, `path`) |
| entry id | SHA-256 of the entry's canonical line |
| tombstone | an entry whose `hash` is `null`: the path was deleted |
| segment | an immutable file of entries, the output of one scan, named by its SHA-256 |
| head | an entry of a path that no other entry names as a parent |
| winner | the head every vault agrees to show, chosen by `(is file, mtime, id)` |
| loser | a distinct file content among the heads that is not the winner's |
| conflict | a path whose heads have more than one distinct `hash` |
| conflict copy | a loser written beside the winner as `<name>-<short hash>.<ext>` |
| incomplete | a path with an entry whose parent is not in the log yet |
| stat cache | local file `.syncit/cache`: per path, `(size, mtime, hash, base)` |
| row | one path's record in the stat cache |
| held | a path with a row: this vault has the file |
| base | in a row, the entries the file on disk was derived from; the parents of the next entry |
| wanted | a path that the vault's include and exclude prefixes select for materialising |
| blob | a file's bytes, addressed by hash |
| peer | the other vault in a run |
| origin | the vault this one was cloned from; the default peer |

### Working rules for the implementer

- The experience goal is Dropbox, not git (`PRD.md`, "Goals"). People manage their files with ordinary tools and run `sync` when they feel like it. When a choice is open, take the one that needs no extra step or command from the user, as long as it cannot lose data.
- Do the milestones in section 15 in order. Each ends with tests passing. Tick its box in this file when it is done.
- Keep it simple. One crate, the dependencies listed in section 3 and no others, no traits with one implementation, no configuration beyond what section 5 defines. Where two designs work, take the one that keeps separate things separate.
- Core rules are pure functions over data (the read rules, wanted-path matching, conflict-copy names). I/O sits around them.
- Do not change the on-disk formats, the read rules or the write rule. If something in these documents is wrong or cannot be built, stop work on that part, write what you found under "Deviations", choose the smallest fix, and continue.
- Never weaken a safety check in section 9 to make a test pass.
- Section 16 lists questions that are deliberately open. Do not solve them in version 1.

### The goal

Version 1 is done when all of these hold:

1. Every milestone box in section 15 is ticked.
2. `cargo test` passes, and every test named in section 14 exists.
3. `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` pass.
4. `cargo run -- --help` lists the nine commands of section 11.

## 2. Scope of version 1

In: every Must (`R1`–`R14`) and every Should (`R15`–`R18`, `R20`, `R25`) in `PRD.md`.

Out:

- The Coulds: `R21` (limit a run to a path), `R22` (copy-on-write stashing), `R23` (mass-delete guard), `R24` (logs through the remote store). `syncit sync` therefore takes no `[path]` argument in version 1.
- Everything under "Non-goals" in `PRD.md`.
- Signed segments, the heads file, and compaction (`CRDT.md`, "Planned extension", "Lookups", "Compaction").
- Peers that are not directories on this machine.
- Windows. Version 1 targets macOS and Linux.

## 3. Crate and dependencies

- One Cargo package named `syncit`, with a library (`src/lib.rs`) and a binary (`src/main.rs`). Integration tests call the library.
- Rust edition 2024. Minimum Rust version 1.89, for `std::fs::File::try_lock`.
- Dependencies: `clap` (with `derive`), `serde` (with `derive`), `serde_json`, `sha2`, `anyhow`.
- Dev dependency: `tempfile`.
- From the standard library, not from crates: directory walking (`fs::read_dir`), hex encoding, setting file times (`File::set_modified`), file locking, atomic rename.

## 4. Modules

Suggested layout. Merge or split if it reads better, but keep the pure parts free of I/O.

| Module | Holds | I/O |
|---|---|---|
| `entry` | `Entry`, canonical encode and decode, entry id, path validation | none |
| `segment` | encode, decode and validate a whole segment | none |
| `log` | load `log/`, the path-to-entries map, `resolve` (the read rules) | load only |
| `config` | `Config`, load and save, `wanted` | load and save |
| `cache` | `Cache`, `Row`, load and save, the unchanged test | load, save, stat |
| `scan` | walk, the write table, segment writing | yes |
| `blobs` | hashing, blob sources, fetch, retain, remote upload | yes |
| `materialise` | the per-path procedure, conflict copies, setting base | yes |
| `vault` | `Vault` (root, config, cache, log, lock), finding the root | yes |
| `commands` | one function per command, returning a `Report` | yes |
| `main` | argument parsing, printing a `Report`, exit code | yes |

Types the rest of this file refers to:

```rust
// Fields are declared in alphabetical order: serde_json writes them in
// declaration order, which gives the canonical key order.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub hash: Option<String>, // 64 lowercase hex, or None for a tombstone
    pub mtime: i64,           // milliseconds since the Unix epoch
    pub parents: Vec<String>, // entry ids, sorted, no duplicates
    pub path: String,
}

pub struct Row { pub size: u64, pub mtime: i64, pub hash: String, pub base: Vec<String> }

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
pub struct Loser { pub hash: String, pub mtime: i64 } // mtime: the largest among heads with this hash
```

`Id` and hashes may be `String`. Do not build a newtype hierarchy.

## 5. On-disk layout and formats

```
<vault>/
  .syncit/
    config    # JSON, local
    cache     # JSON, local
    ignore    # optional text file, local
    lock      # empty file, locked while a command runs
    log/      # segments, named by hash; the only replicated part
    blobs/    # blobs, named by hash, flat
    tmp/      # scratch for atomic writes into .syncit/
```

Every file written under `.syncit/` is written to `.syncit/tmp/` first and renamed into place.

### config

```json
{
  "syncit": 1,
  "origin": "/absolute/path/to/other/vault",
  "include": [""],
  "exclude": [],
  "keep": false,
  "remote": null
}
```

- `syncit`: format version (`R25`). A command refuses to run on a vault, or with a peer, whose version is not 1.
- `origin`: absolute path of the default peer, or `null`.
- `include`, `exclude`: wanted-path prefixes, vault-relative, `/`-separated. `""` is the vault root.
- `keep`: retain every version in `.syncit/blobs/` (`R15`).
- `remote`: absolute path of a directory used as a blob store, or `null` (`R16`). Blobs are files named `<hash>` directly inside it.
- Written pretty-printed. Unknown fields are an error. `keep` and `remote` are changed by editing the file; there is no command for them.

**Wanted** (`PRD.md`, "What a vault holds"): a prefix matches a path if it is `""`, equals the path, or is followed in the path by `/`. Among all matching prefixes in both lists, the longest decides: wanted if it came from `include`. If none matches, the path is not wanted. A prefix is never in both lists.

### cache

```json
{
  "scanned": 1763600000000,
  "rows": {
    "foo/bar/x.txt": { "size": 120, "mtime": 1763599990000, "hash": "9c1f…", "base": ["3f9a…"] }
  }
}
```

- `scanned`: when the most recent scan started, in milliseconds. 0 before the first scan.
- `rows`: one per held path. `mtime` is the file's mtime as the filesystem reported it, in milliseconds (floor; negative before 1970).
- A missing cache file is an empty cache. That is the "lost cache" case and must work (`CRDT.md`, "Writing an entry", third row).

**The unchanged test.** A file is unchanged against its row if its size and mtime equal the row's **and** the row's mtime is less than `scanned − 2000`. Otherwise it is hashed and compared with the row's hash. The second condition catches an edit made in the same timestamp tick as the scan that recorded the file; the 2000 ms margin covers filesystems with coarse timestamps. Keep the margin a named constant. One function implements this test and is used by scan, materialise and `drop`.

### Segments

Format, canonical form and validation are in `CRDT.md`, "Segments" and "Validation". Implementation notes:

- Encode an entry with `serde_json::to_string`. Decode by parsing the line into `Entry`, re-encoding, and requiring the bytes to equal the line. This one check enforces key order, absence of whitespace, number form, string escaping, presence of all four fields and absence of others.
- Then check the field contents: `hash` and each parent are 64 lowercase hex characters; `parents` is strictly ascending by byte order; `path` passes path validation.
- Path validation: not empty, does not start with `/`, no component that is empty, `.` or `..`, first component is not `.syncit`.
- The file is: the exact line `{"syncit":1}`, then zero or more entry lines, each line ended by one `\n`. No blank lines, no missing final newline.
- A writer sorts entries by path (byte order) and writes at most one entry per path.
- The canonical string form must match RFC 8785: `"` and `\` are escaped with a backslash; U+0008, U+0009, U+000A, U+000C and U+000D are written `\b`, `\t`, `\n`, `\f`, `\r`; other characters below U+0020 are written `\u00xx` with lowercase hex; everything else, including `/`, U+007F and all non-ASCII, is written literally. `serde_json` is expected to do exactly this. Test `canonical_escaping` pins it; if it fails, write the string encoder by hand.

Test vector:

```
entry line:  {"hash":null,"mtime":1763600005000,"parents":[],"path":"old.txt"}
entry id:    14e254d499907fd5a29ece8053509c7270afa9ad3be8beb60f16836aef423449
segment:     {"syncit":1}\n<entry line>\n
segment name: 131647b457f74cabf5580495b318db71fc518912d8c4a8b21a1b665205374f49
```

### ignore

`.syncit/ignore` is optional, local and never replicated (`R20`). One rule per line; blank lines and lines starting with `#` are skipped.

- A line without `/` is a name. It ignores any file or folder with exactly that name, at any depth.
- A line with `/` is a vault-relative path prefix, matched as wanted prefixes are.
- No wildcards.

An ignored path is invisible: scan does not look at it (so a recorded file that becomes ignored is not read as deleted) and materialise never writes it.

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
- any other directory that contains `.syncit` (a nested vault);
- anything ignored;
- symbolic links and anything that is not a regular file or a directory (warn once per run on stderr with a count);
- names that are not valid UTF-8 (warn, naming the parent directory);
- files whose name starts with `.syncit-tmp-`: these are leftovers of an interrupted run, and scan deletes them.

Paths are recorded as the operating system returns them, joined with `/`. No Unicode normalisation.

### Per path

Let `now` be the time the scan started. For each file found, and for each row whose path was not found and is not ignored, apply the table in `CRDT.md`, "Writing an entry":

- **File and row.** Apply the unchanged test. If the hash equals the row's, refresh the row's size and mtime and write nothing. If it differs, write a file entry with `parents` = the row's base; the row becomes `(size, mtime, new hash, [new entry id])`.
- **Row, no file.** Write a tombstone with `mtime = now` and `parents` = the row's base. Remove the row.
- **File, no row.** Hash it. If any valid entries of this path have that hash, write nothing and create a row whose base is all of them (superseded ones included). Otherwise write a file entry with `parents = []` and create the row.
- **Neither.** Nothing. The path is not held.

**Auto-include.** In the "file, no row" case, if the path is not wanted, the scan adds an include for it, so that a folder dragged into a vault is synced from then on, including files later added to it elsewhere. The prefix added is the shortest of the path's prefixes (its ancestor directories, then the path itself) that is not equal to, or an ancestor of, any prefix already in `include` or `exclude`. Examples, for a vault that includes `foo/bar`:

| New file | Include added | Why not shorter |
|---|---|---|
| `zap/x/y.txt` | `zap` | `""` is an ancestor of `foo/bar` |
| `foo/new.txt` | `foo/new.txt` | `foo` is an ancestor of `foo/bar` |
| `foo/bar/big/n/a.txt`, with `foo/bar/big` excluded | `foo/bar/big/n` | `foo/bar/big` is already a prefix |

Includes are added after the walk, shortest first, re-checking wantedness, so one dragged folder yields one include. The config is saved before the cache. A dry scan adds nothing. `drop` undoes it.

A file entry's `mtime` is the file's mtime in milliseconds. Stat the file before and after hashing; if size or mtime changed in between, leave the path and its row untouched this scan and report it as `skipped`.

### Finishing

1. If there are new entries, write them as one segment into `.syncit/log/`. No entries, no segment.
2. Set `scanned = now` and save the cache.
3. Retain blobs (section 8).

The segment is written before the cache is saved. If the run stops between the two, the next scan sees the same difference and writes an entry with the same path, hash, mtime and parents, which is the same entry. For a deletion it writes a second tombstone beside the first, which is harmless: two tombstone heads are the same version.

`scan` has a dry mode that returns what it would write and changes nothing. `status` uses it.

## 8. Blobs

Implements `PRD.md`, "Blobs". Serves `R11`, `R13`, `R15`, `R16`.

### Sources

`fetch(hash, destination)` tries these in order and stops at the first that yields the right bytes:

1. this vault's working files;
2. this vault's `.syncit/blobs/<hash>`;
3. the peer's working files, if there is a peer in this run;
4. the peer's `.syncit/blobs/<hash>`;
5. `<remote>/<hash>`, if `remote` is set and the directory exists.

- Working files are found through an in-memory map from hash to paths, one per vault, built at the start of the run from the cache rows. Whenever the run writes a file or renames one (including a conflict copy, which has no row), it adds the new location to the map. The map is a hint only.
- Every fetch copies to the destination while hashing and succeeds only if the hash matches (`R11`). On a mismatch the destination is removed and the next source is tried.
- If no source has the blob, `fetch` returns "unavailable". That is not an error.

### Retaining

A vault retains blobs if `keep` is on or `remote` is set. With both off it never writes to `.syncit/blobs/` (`R13`).

`retain()`: for every non-null hash in the log that is not already in `.syncit/blobs/` (nor in the remote, when it is reachable), `fetch` it into `.syncit/blobs/`. Hashes with no source are skipped silently.

`flush()`, when `remote` is set and its directory exists: copy each blob in `.syncit/blobs/` that the remote lacks to a temporary name inside the remote directory and rename it to `<hash>`. Then, if `keep` is off, delete from `.syncit/blobs/` every blob the remote now has. When the remote is unreachable, blobs wait in `.syncit/blobs/`.

A retaining vault calls `retain()` after every scan, and again after segments are exchanged and before anything is materialised, so that a version is saved before it is replaced. It calls `flush()` at the end of the run.

## 9. Materialise

Implements `PRD.md`, "Current state", "Conflict copies", "A sync run" step 6, and `CRDT.md`, "Setting base". Serves `R1`, `R3`, `R4`, `R5`, `R8`, `R9`, `R10`, `R11`, `R14`.

`materialise(vault, peer)` visits every path in the vault's log in sorted order. The peer is only a blob source and may be absent.

### Safety checks

These apply to every write, rename and delete in the working folder. A path that fails one is reported as `skipped` with the reason and left exactly as it was.

- **Scan would skip it.** The path is ignored, has a `.syncit` component, has a file name starting with `.syncit-tmp-`, or lies inside a nested vault. Such a path is never written. (Not reported; it is simply not this vault's business.)
- **Symlinked parent.** Any existing ancestor directory of the path, below the vault root, is a symbolic link. Check with `symlink_metadata`. Segments come from other vaults and must not be able to write outside this one.
- **Changed since scanned (`R10`).** The path has a row and the file fails the unchanged test and its hash differs from the row's, or the file is missing. The next scan will record the change.
- **Untracked file in the way.** The path has no row and something exists there (`symlink_metadata` succeeds).
- **Filesystem error.** Any I/O error on one path, such as a file where a directory is needed. The run continues with the next path.

### Writing a file

To put content `h` with mtime `m` at path `p`: create the parent directories; `fetch(h)` into `.syncit-tmp-<unique>` in the same directory; set its mtime to `m`; rename it onto `p`; stat `p`. The hash is verified before the rename. If the blob is unavailable, remove the temporary file and any directories just created.

### Per path

Let `S = resolve(p)`, `row` the cache row, `W` the winner, `h` its hash.

1. `S` is `Absent`: nothing.
2. `S` is `Incomplete`: report `incomplete`. Nothing else.
3. The path is neither held nor wanted: nothing. (`R5`. A held path is kept up to date whether wanted or not.)
4. **`W` is a tombstone.** If held: run the safety checks, delete the file, remove the row, and remove parent directories left empty, up to the vault root. Report `deleted`. If not held: nothing.
5. **`W` is a file and held with `row.hash == h`.** The content is in place. Go to "copies and base".
6. **`W` is a file and held with a different hash.** Run the safety checks.
   - If `row.hash` is a loser hash and its copy path `c` has no entries in the log: the local file is a conflicting version and must stay visible. If something already exists at `c`, report `skipped` (name clash) and stop. Otherwise write the winner to its temporary name first; rename `p` to `c`; rename the temporary file onto `p`; remove the old row. `c` gets no row.
   - Otherwise the local version is superseded, or its copy was already recorded: write the winner over it.
   - If the winner's blob is unavailable: report `pending`, change nothing, and leave the row and its base alone.
   - On success the row becomes `(size, mtime as stat reports, h, base)`. Report `updated`. Go to "copies and base".
7. **`W` is a file, not held, wanted.** Run the safety checks and write the file. Unavailable: `pending`. On success create the row and report `added`. Go to "copies and base".

### Copies and base

Reached only when the winner's content is the file at `p`.

For each loser `l` (a distinct losing hash, with `l.mtime` the largest mtime among heads with that hash), with `c = copy_name(p, l.hash)`:

- If `c` has entries in the log: the version was surfaced before. Nothing to write. (`PRD.md`: a copy the user deleted stays deleted.)
- Else if a file exists at `c`: surfaced if its hash is `l.hash`; otherwise a name clash, not surfaced, reported as `skipped`.
- Else write `l.hash` to `c` with mtime `l.mtime`, without a cache row. Surfaced on success; report `copied`. Unavailable: not surfaced; report `pending`.

Setting the copy's mtime from the entry means vaults that create the same copy independently will record identical entries for it.

Then set the row's base to:

- every head whose hash is `h`;
- every tombstone head;
- every head whose hash is a surfaced loser.

Heads of losers that were not surfaced stay out, so the conflict stays open (`CRDT.md`, "Setting base").

### copy_name

`copy_name(path, hash)` inserts `-` and the first 8 hex characters of the hash into the file name, before the last `.` that is not the name's first character:

| Path | Copy |
|---|---|
| `a/notes.txt` | `a/notes-3f9a1c2e.txt` |
| `Makefile` | `Makefile-3f9a1c2e` |
| `.bashrc` | `.bashrc-3f9a1c2e` |
| `x.tar.gz` | `x.tar-3f9a1c2e.gz` |

### Finishing

Save the cache once, after all paths.

## 10. A run with a peer

`sync`, `push` and `pull` share one procedure. "Receiver" means both vaults for `sync`, the peer for `push`, the local vault for `pull`.

1. Open both vaults. Refuse if they are the same folder or either has a version other than 1. Lock both.
2. Scan the local vault, then the peer (section 7). Both scan in every mode, so each vault's changes are recorded from its own cache.
3. Exchange segments: each receiver takes every segment in the other vault's `log/` whose name it lacks. A segment is read, validated as in `CRDT.md`, and only then written into `log/`. A segment that fails is not stored, is reported as `rejected`, and does not stop the run.
4. Each receiver reloads its log.
5. Each retaining receiver calls `retain()`. This happens for both before either materialises.
6. Each receiver materialises with the other vault as blob source: the peer first, then the local vault.
7. Each retaining vault calls `flush()`.
8. Collect conflicts from each receiver's log, for paths it holds or wants, and report each once.

Scanning precedes exchange, so entries are written from what each vault held before it learned anything new (`PRD.md`, "A sync run").

## 11. Commands

The vault is found by walking up from the current directory to the first folder containing `.syncit/`. A `<peer>` or `<vault>` argument is resolved the same way from the given path. Every command except `init` and `clone` takes the vault's lock first: open `.syncit/lock` and `try_lock()`; if that fails, exit with "vault is in use". Paths stored in `config` are canonicalised.

| Command | Steps |
|---|---|
| `syncit init [dir]` | `dir` defaults to `.`. Error if `dir/.syncit` exists. Create `.syncit/` with `log/`, `blobs/`, `tmp/` and a config of `include: [""]`, `origin: null`. Existing files are untouched and recorded by the first scan. |
| `syncit clone <src> <dir>` | Find the vault root `R` above `src`; `sub` is `src` relative to `R` (`""` if equal). `dir` must not exist or must be empty. Create it as a vault with `origin: R`, `include: [sub]`. Then run `pull`. (`R4`) |
| `syncit sync [peer]` | Section 10, both directions. The peer defaults to the origin; error if there is none. (`R1`, `R12`) |
| `syncit push [peer]` | Section 10; only the peer receives. (`R2`) |
| `syncit pull [peer]` | Section 10; only the local vault receives. (`R2`) |
| `syncit snapshot` | Scan, `retain()`, `flush()`. No peer. (`R17`) |
| `syncit status` | Dry scan and `resolve`. Writes nothing at all. (`R18`) |
| `syncit get <path>...` | See below. (`R7`) |
| `syncit drop [--force] <path>...` | See below. (`R7`) |

`get` and `drop` take one or more paths. Each is relative to the current directory and is converted to a vault-relative prefix. It need not exist on disk. The steps below run once over all the prefixes together: `drop` makes its checks for every prefix before changing anything, so it either drops them all or refuses.

### get

1. Remove every `exclude` at or under the prefix and every `include` under it. Add the prefix to `include`. Save the config.
2. Materialise (section 9), with the origin as blob source if it is reachable and can be locked. No scan and no segment exchange happen.
3. Print a hint that `syncit pull` fetches newer changes.

### drop

1. Scan (section 7), so that unrecorded changes are recorded before anything is removed. The user does not need to snapshot first.
2. Unless `--force`: for every held path under the prefix, its hash must be in `.syncit/blobs/`, or in the remote if reachable, or in the origin if reachable (a row with that hash in the origin's cache, or a file in its `blobs/`). If any is not, refuse, list them, and say that syncing first will fix it. This prevents dropping the only copy of a file, including a version the scan in step 1 has just recorded.
3. Remove every `include` at or under the prefix and every `exclude` under it. Add the prefix to `exclude`. Save the config.
4. Remove the rows and save the cache.
5. Delete the files, and directories left empty.

The order of 4 and 5 matters. A file with no row is "held again" after the next scan; a row with no file would be recorded as a deletion and propagate.

## 12. Output and exit codes

Commands return a `Report`: a list of events and the conflicts. `main` prints it. Warnings and errors go to stderr; everything below goes to stdout.

Events are grouped by vault, in path order:

```
/Users/me/work:
  added: foo/new.txt
  updated: notes.txt
  deleted: old.txt
  copied: notes-3f9a1c2e.txt
  pending: big.bin — content not available from any source
  skipped: draft.txt — changed since it was scanned
  incomplete: x.txt — waiting for missing history
  rejected: segment 77b0… — not in canonical form
conflict: notes.txt — kept the newer version, other saved as notes-3f9a1c2e.txt
conflict: gone.txt — kept the file; it was deleted in another vault
recorded 3, added 1, updated 1, deleted 1, conflicts 2, pending 1, skipped 1
```

- `conflict` lines come after all vaults, once per path, and repeat on every run until the conflict is closed.
- A conflict with several losers lists every copy name.
- `recorded` in the summary counts entries written by scans.

`status` prints, for the local vault: `new`, `modified` and `deleted` (what a scan would record), `behind` (held or wanted paths whose winner is not on disk), `incomplete`, and `conflict` lines as above.

| Exit code | Meaning |
|---|---|
| 0 | Completed; nothing needs attention. `status` always exits 0 unless it fails. |
| 1 | Failed: bad arguments, not a vault, vault in use, version mismatch, corrupt local file. |
| 2 | Completed, but there are conflicts or pending, skipped, incomplete or rejected items. |

## 13. Interruption

`R14`: stopping a run at any point leaves both vaults valid, and running again completes the work. The lock is released by the operating system when the process dies.

| Stopped | State left | Why the next run is correct |
|---|---|---|
| after writing a segment, before saving the cache | entries exist that the cache does not reflect | the scan writes the identical entries again (section 7) |
| during segment exchange | some segments copied; possibly a child without its parent | segments are whole or absent; such paths are `Incomplete` and wait |
| while fetching a blob | a `.syncit-tmp-*` file | never renamed into place; the next scan deletes it |
| after replacing files, before saving the cache | files newer than their rows | the scan re-records them as entries with the winner's hash and mtime: either the same entry or a head with the same hash, which is the same version. Files with no row are adopted |
| after renaming a loser to its copy name, before placing the winner | the path is missing, the copy has no row | the scan records a tombstone and the copy; the winner still beats the tombstone, and the losing content is in the copy |
| after deleting a file, before removing its row | a row with no file | the scan writes a second tombstone; all heads are tombstones |
| during `drop`, after saving the cache | files with no rows | the scan adopts them; nothing is recorded as deleted |

## 14. Tests

Unit tests live beside the code. Integration tests live in `tests/` and build vaults in temporary directories through the library's command functions. Tests set file mtimes with `File::set_modified` where order matters and never sleep.

### Log (milestones 1 and 2)

| Test | Asserts |
|---|---|
| `entry_id_vector` | the test vector in section 5 |
| `canonical_escaping` | a path containing `"`, `\`, U+0001, U+000A, `é`, `/` and U+007F encodes per section 5 and round-trips |
| `segment_round_trip` | encode then decode returns the entries; entries are sorted by path; the name is the hash |
| `segment_rejects` | one case per bullet of `CRDT.md` "Validation": wrong name, version 2, extra field, missing field, wrong type, whitespace, key order, uppercase hex, absolute path, `..`, empty component, `.syncit/x`, unsorted parents, duplicate parents, missing final newline |
| `resolve_worked_cases` | every row of `CRDT.md` "Worked cases" that concerns the log |
| `resolve_order_independent` | the same entries split into segments in several ways and loaded in several orders give the same `PathState` for every path |
| `resolve_incomplete` | a missing parent gives `Incomplete`; adding it gives the expected heads |
| `resolve_cross_path_parent` | an entry with a parent of another path is never a head and supersedes nothing |
| `winner_order` | file beats tombstone regardless of mtime; later mtime wins; the id breaks a tie |
| `copy_name_cases` | the table in section 9 |
| `wanted_cases` | longest prefix decides; `foo` does not match `foobar`; nothing matching means not wanted |

### Stories (one per user story in `PRD.md`)

| Test | Story | Asserts |
|---|---|---|
| `story_a1_edit_and_push` | A.1 | clone, edit in the copy, push; the archive has the edit |
| `story_a2_add_and_push` | A.2 | a file added in the copy appears in the archive; a deletion propagates |
| `story_a3_drop` | A.3 | given two paths in one call, `drop` removes local files; after sync the archive still has them; `get` brings them back |
| `story_a4_clone_subfolder` | A.4 | cloning `a/foo/bar` gives `b/foo/bar/…` only; files outside it are not deleted from `a` by a sync |
| `story_a5_get_and_new_folder` | A.5 | `get` of a second folder; a new top-level folder in the copy reaches the archive, is added to `include`, and a file later added to it in the archive arrives in the copy; the three auto-include examples of section 7 |
| `story_b1_conflict` | B.1 | both sides edit; after sync both hold the same winner and the same copy; exit status reports the conflict; editing the winner and syncing closes it everywhere |
| `story_c1_three_vaults` | C.1 | three vaults synced in several pairings and orders converge; a change relays through a middle vault |
| `story_d1_no_history` | D.1 | with `keep` off and no remote, `.syncit/blobs/` stays empty after syncs |
| `story_d2_keep` | D.2 | with `keep` on, every version's blob is present after edits and syncs; with `remote` set, blobs reach the remote, and wait locally while it is missing |
| `story_d3_snapshot` | D.3 | two snapshots without a peer give two entries in a chain; a later sync delivers both |
| `story_e1_rerun` | E.1 | running sync twice changes nothing the second time and writes no segment |

### Safety and recovery

| Test | Asserts |
|---|---|
| `edit_beats_delete` | edit on one side, delete on the other: the file survives everywhere and the delete is reported |
| `same_edit_no_conflict` | identical content written on both sides is not a conflict |
| `new_file_at_existing_path` | a file created in a partial vault at a path another vault already has: equal content is adopted or merges silently, different content is an ordinary conflict with a copy |
| `lost_cache` | delete `.syncit/cache`, sync: no entries written for unchanged files, no deletions invented, old files brought forward |
| `copied_vault` | copy a vault folder with `cp -R` semantics, edit both, sync all: an ordinary conflict, nothing lost |
| `restored_backup` | restore a vault to an old state, edit, sync: conflict with the newer head, nothing lost |
| `pending_then_edit` | winner's blob unavailable, then a local edit: the result is a conflict, not an overwrite |
| `changed_during_run` | a file modified after the scan is not replaced (`R10`); the next run records it |
| `untracked_in_the_way` | a file created at an incoming path after the scan is not overwritten |
| `same_tick_edit` | an edit that keeps size and mtime right after a scan is still detected |
| `interrupted_before_cache` | restore the old cache after a snapshot, snapshot again: the set of entries is unchanged |
| `interrupted_transfer` | remove a parent's segment from the receiver: the path is incomplete and untouched; a re-sync resolves it |
| `hostile_segment` | a peer's log containing an invalid segment: it is rejected, the rest of the sync completes |
| `symlink_escape` | an incoming path under a local symlinked directory is skipped and nothing outside the vault changes |
| `ignored_files` | ignored files are not recorded, not deleted, and not overwritten |
| `corrupt_blob_source` | a peer file changed behind its cache is not installed (`R11`) |
| `copy_deleted_stays_deleted` | deleting a conflict copy and syncing does not bring it back |
| `drop_refuses_sole_copy` | `drop` without `--force` refuses when no other source has the content; an unrecorded edit under the path is recorded as an entry, and then also blocks the drop until it is synced |
| `version_mismatch` | a peer with `"syncit": 2` in its config is refused |
| `cli_smoke` | the built binary runs `init`, `snapshot`, `status` and returns the documented exit codes |

### Requirement coverage

| Requirement | Section | Tests |
|---|---|---|
| R1, R3 | 9, 10 | `story_a1`, `story_a2`, `story_b1` |
| R2 | 10, 11 | `story_a1`, `story_a2` |
| R4, R6 | 5, 7, 11 | `story_a4`, `story_a5` |
| R5, R7 | 7, 9, 11 | `story_a3`, `story_a4`, `lost_cache` |
| R8, R9 | 6, 9 | `story_b1`, `winner_order`, `edit_beats_delete` |
| R10 | 9 | `changed_during_run`, `untracked_in_the_way`, `same_tick_edit` |
| R11 | 8 | `corrupt_blob_source` |
| R12 | 6, 10 | `story_c1`, `resolve_order_independent` |
| R13 | 8 | `story_d1` |
| R14 | 13 | `story_e1`, `interrupted_before_cache`, `interrupted_transfer` |
| R15, R16 | 8 | `story_d2` |
| R17 | 7, 11 | `story_d3` |
| R18 | 11, 12 | `cli_smoke`, `story_b1` |
| R20 | 5, 7 | `ignored_files` |
| R25 | 5, 10 | `version_mismatch`, `segment_rejects` |

## 15. Milestones

Each milestone ends with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passing.

- [ ] **1. Entry and segment codec.** `entry`, `segment`. Tests: `entry_id_vector`, `canonical_escaping`, `segment_round_trip`, `segment_rejects`.
- [ ] **2. Read rules.** `log` with `resolve`. Tests: the `resolve_*` tests, `winner_order`.
- [ ] **3. A single vault.** `config`, `cache`, `vault`, `scan`; commands `init`, `snapshot`, `status`. Tests: `wanted_cases`, `story_d3_snapshot` (without the sync step), `same_tick_edit`, `lost_cache` (single-vault part), `interrupted_before_cache`, `cli_smoke`.
- [ ] **4. Sync without conflicts.** Blob fetch from working files, segment exchange, materialise steps 1–4 and 7 and the non-conflict part of 6; commands `sync`, `push`, `pull`, `clone`. Tests: `story_a1`, `story_a2`, `story_e1`, `story_c1`, `changed_during_run`, `untracked_in_the_way`, `corrupt_blob_source`.
- [ ] **5. Conflicts.** Conflict copies, base, incomplete paths. Tests: `story_b1`, `copy_name_cases`, `edit_beats_delete`, `same_edit_no_conflict`, `new_file_at_existing_path`, `copied_vault`, `restored_backup`, `pending_then_edit`, `copy_deleted_stays_deleted`, `interrupted_transfer`.
- [ ] **6. Partial vaults.** Wanted paths in materialise, `clone` of a sub-folder, `get`, `drop`. Tests: `story_a3`, `story_a4`, `story_a5`, `drop_refuses_sole_copy`.
- [ ] **7. Retained blobs.** `.syncit/blobs/`, `keep`, `remote`, `retain`, `flush`. Tests: `story_d1`, `story_d2`.
- [ ] **8. Hardening.** Locking, ignore file, symlink checks, version checks, output format and exit codes. Tests: `hostile_segment`, `symlink_escape`, `ignored_files`, `version_mismatch`, and the remaining parts of `lost_cache` and `story_d3`.

## 16. Decisions and open questions

### Decided here

These settle, for version 1, questions that `PRD.md` lists as open.

| Question | Answer |
|---|---|
| Remote store | a directory path holding `<hash>` files; no network stores |
| Ignore list | `.syncit/ignore`, local; names and path prefixes, no wildcards |
| `drop` safety | scans first, so unrecorded changes are recorded; refuses when no other source has the content unless `--force` |
| File metadata | regular files only; symlinks skipped with a warning; permissions and extended attributes not recorded |
| File mtimes | a materialised file gets its entry's mtime |
| Unicode paths | recorded as the OS returns them; names that are not UTF-8 are skipped with a warning |
| Case-insensitive filesystems | no special handling; a case collision surfaces as "untracked file in the way" and nothing is overwritten |
| Conflict-copy name clash | the existing file is left alone; the conflict is reported and stays open |
| New folder outside the wanted paths | recorded, held, and added to `include` automatically (section 7); `drop` undoes it |
| Tie-break timestamp | file mtime |
| Empty directories | not tracked; directories emptied by a deletion are removed |
| Cross-path parents | a read rule, not a segment rejection (`CRDT.md` was corrected to match) |

### Still open, not for version 1

- Wildcards in the ignore file, and whether it should replicate.
- A stored heads file, if loading the whole log becomes slow.
- Compaction and blob garbage collection.
- Signed segments and peers reached over a network.
- A `size` field in entries.
- Unicode normalisation of paths.
- A guard against mass deletion (`R23`). A whole vault on an unmounted drive cannot trigger one, because `.syncit/` is missing too; a missing mount *inside* a vault can.

## 17. Deviations

Record here anything built differently from these documents, with the reason. Empty at the start.
