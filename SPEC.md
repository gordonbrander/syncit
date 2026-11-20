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
| nickname | a vault's label, by default its folder's name, written into each segment it records. Not an identity |
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
| peer | the other vault in a run; always named on the command line |
| store | a log and a set of blobs with no working folder: a directory or an S3 bucket. Holds old versions, and lets vaults that never meet sync through it. Milestones 9 and 10; not built yet |

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
4. `cargo run -- --help` lists the eleven commands of section 11.

## 2. Scope of version 1

In: every Must (`R1`–`R14`) and every Should (`R15`–`R18`, `R20`, `R24`, `R25`) in `PRD.md`.

Out:

- The Coulds: `R21` (limit a run to a path), `R22` (copy-on-write stashing), `R23` (mass-delete guard). `syncit sync` therefore takes no `[path]` argument in version 1.
- Everything under "Non-goals" in `PRD.md`.
- Signed segments, the heads file, and compaction (`CRDT.md`, "Planned extension", "Lookups", "Compaction").
- Peers reached over a network (SSH, iroh). A vault is a directory on this machine. A store may also be an S3 bucket (section 8).
- Windows. Version 1 targets macOS and Linux.

## 3. Crate and dependencies

- One Cargo package named `syncit`, with a library (`src/lib.rs`) and a binary (`src/main.rs`). Integration tests call the library.
- Rust edition 2024. Minimum Rust version 1.89, for `std::fs::File::try_lock`.
- Dependencies: `clap` (with `derive`), `serde` (with `derive`), `serde_json`, `sha2`, `anyhow`.
- For S3 stores (milestone 10) one more is needed: an S3 client. Which one is open; see section 16.
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
| `blobs` | hashing, blob sources, fetch, retain | yes |
| `materialise` | the per-path procedure, conflict copies, setting base | yes |
| `vault` | `Vault` (root, config, cache, log, lock), finding the root | yes |
| `commands` | one function per command; a command that runs a sync returns a `Summary` | yes |
| `output` | `Options`, the `Message` types with their text and JSON forms, `print_stdout`, `print_stderr`, `Summary` | no |
| `main` | argument parsing, printing the `Summary` and errors, exit code | yes |

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
  "keep_local_history": false,
  "nickname": "notes"
}
```

- `syncit`: format version (`R25`). A command refuses to run on a vault, or with a peer, whose version is not 1.
- `include`, `exclude`: wanted-path prefixes, vault-relative, `/`-separated. `""` is the vault root.
- `keep_local_history`: retain every version this vault sees in `.syncit/blobs/` (`R15`). Local history is whatever blobs are there; it is never required to be complete.
- `nickname`: the label written into the header of every segment this vault records (`CRDT.md`, "Nickname"). `init` and `clone` set it to the folder's name, with whitespace and control characters replaced by `-`, cut to 64 bytes, or to `vault` if that leaves nothing. It is a label only: `blame` prints it and no other code reads it.
- Written pretty-printed. Unknown fields are an error. `keep_local_history` and `nickname` are changed with `syncit config` (section 11). Milestone 9 adds a `store` field beside them.
- A vault records nothing about other vaults. There is no origin or default peer: any vault syncs with any other, and folders may move.

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

**The unchanged test.** A file is unchanged against its row if its size and mtime equal the row's **and** the row's mtime is less than `scanned − 2000`. Otherwise it is hashed and compared with the row's hash. The second condition catches an edit made in the same timestamp tick as the scan that recorded the file; the 2000 ms margin covers filesystems with coarse timestamps. Keep the margin a named constant. One function implements this test and is used by scan, materialise and `move`.

### Segments

Format, canonical form and validation are in `CRDT.md`, "Segments" and "Validation". Implementation notes:

- Encode an entry with `serde_json::to_string`. Decode by parsing the line into `Entry`, re-encoding, and requiring the bytes to equal the line. This one check enforces key order, absence of whitespace, number form, string escaping, presence of all four fields and absence of others.
- Then check the field contents: `hash` and each parent are 64 lowercase hex characters; `parents` is strictly ascending by byte order; `path` passes path validation.
- Path validation: not empty, does not start with `/`, no component that is empty, `.` or `..`, first component is not `.syncit`.
- The file is: the header line, then zero or more entry lines, each line ended by one `\n`. No blank lines, no missing final newline.
- The header is `{"nickname":"<nickname>","syncit":1}`. It is encoded and checked as an entry is: parse it into a struct with exactly those two fields, re-encode, and require the same bytes. Then check that `syncit` is 1 and the nickname is valid: 1 to 64 bytes, no whitespace, no control characters.
- Decoding returns the nickname with the entries. The log keeps, for each entry id, the set of nicknames of the segments it was found in.
- A writer sorts entries by path (byte order) and writes at most one entry per path.
- The canonical string form must match RFC 8785: `"` and `\` are escaped with a backslash; U+0008, U+0009, U+000A, U+000C and U+000D are written `\b`, `\t`, `\n`, `\f`, `\r`; other characters below U+0020 are written `\u00xx` with lowercase hex; everything else, including `/`, U+007F and all non-ASCII, is written literally. `serde_json` is expected to do exactly this. Test `canonical_escaping` pins it; if it fails, write the string encoder by hand.

Test vector:

```
entry line:  {"hash":null,"mtime":1763600005000,"parents":[],"path":"old.txt"}
entry id:    14e254d499907fd5a29ece8053509c7270afa9ad3be8beb60f16836aef423449
segment:     {"nickname":"notes","syncit":1}\n<entry line>\n
segment name: 12bf899655031a7997e0f0a4824dc2fe225c6b8e07157384ac2d74c4234a0522
```

### .syncitignore

`.syncitignore`, at the vault root, is the optional ignore list (`R20`). One rule per line; blank lines and lines starting with `#` are skipped. Only the root file is read; there are no per-folder ignore lists.

- A line without `/` is a name. It ignores any file or folder with exactly that name, at any depth.
- A line with `/` is a vault-relative path prefix, matched as wanted prefixes are.
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

Includes are added after the walk, shortest first, re-checking wantedness, so one dragged folder yields one include. The config is saved before the cache. A dry scan adds nothing. `move` undoes it.

A file entry's `mtime` is the file's mtime in milliseconds. Stat the file before and after hashing; if size or mtime changed in between, leave the path and its row untouched this scan and report it as `skipped`.

### Finishing

1. If there are new entries, write them as one segment into `.syncit/log/`. No entries, no segment.
2. Set `scanned = now` and save the cache.
3. Retain blobs (section 8).

The segment is written before the cache is saved. If the run stops between the two, the next scan sees the same difference and writes an entry with the same path, hash, mtime and parents, which is the same entry. For a deletion it writes a second tombstone beside the first, which is harmless: two tombstone heads are the same version.

`scan` has a dry mode that writes nothing: no segment, no config, no cache, and scratch files are left alone. The vault value in memory is brought up to date all the same, so what follows sees the state a real scan would leave. `status` and `--dry-run` (section 10) use it.

## 8. Blobs

Implements `PRD.md`, "Blobs" and "Stores". Serves `R11`, `R13`, `R15`, `R16`, `R24`.

### Sources

`fetch(hash, destination)` tries these in order and stops at the first that yields the right bytes:

1. this vault's working files;
2. this vault's `.syncit/blobs/<hash>`;
3. the peer's working files, if there is a peer in this run;
4. the peer's `.syncit/blobs/<hash>`;

- Working files are found through an in-memory map from hash to paths, one per vault, built at the start of the run from the cache rows. Whenever the run writes a file or renames one (including a conflict copy, which has no row), it adds the new location to the map. The map is a hint only.
- Every fetch copies to the destination while hashing and succeeds only if the hash matches (`R11`). On a mismatch the destination is removed and the next source is tried.
- If no source has the blob, `fetch` returns "unavailable". That is not an error.

### Retaining

A vault retains blobs if `keep_local_history` is on. With it off the vault never writes to `.syncit/blobs/` (`R13`). Local history is whatever blobs are there: it is never required to be complete.

`retain()`: for every non-null hash in the log that is not already in `.syncit/blobs/`, `fetch` it into `.syncit/blobs/`. Hashes with no source are skipped silently.

A retaining vault calls `retain()` after every scan, and again after segments are exchanged and before anything is materialised, so that a version is saved before it is replaced.

### Stores

> **Milestone 9, not built.** Version 1 as built has no store: no `store` setting, no store argument, and local history is the only place old versions are kept. This subsection is the design to build.

A store is a log and a set of blobs with no working folder: the replicated half of a vault, somewhere every vault can reach. It is one mechanism for two requirements. It is where old versions are kept (`R16`), and it is a place that vaults which never see each other can sync through (`R24`). These are not separate features: a store that held blobs without the log could not be read by anyone but the vault that filled it.

```
<store>/
  format          # the single line {"syncit":1}
  log/<name>      # segments, named by hash
  blobs/<hash>    # blobs, named by hash
```

This is `.syncit/` without its local files (`config`, `cache`, `lock`, `tmp/`).

- **Two kinds.** A directory, given as a path. An S3 bucket, given as `s3://<bucket>/<prefix>`, whose object keys are `<prefix>/format`, `<prefix>/log/<name>` and `<prefix>/blobs/<hash>`. Both kinds provide the same four operations: read `format`, list the names in `log/` or `blobs/`, get one object into a local file, put one object from a local file. Implement this as one enum with two variants, not a trait.
- **Creating.** syncit never creates a store's directory or bucket. An existing empty directory or prefix becomes a store on the first write, which writes `format`. A non-empty directory or prefix without `format` is not a store and is refused, so a mistyped path cannot be filled with objects. A store whose `format` is not version 1 is refused (`R25`).
- **No lock.** Every object is immutable, named by its hash, and written whole: in a directory by writing a temporary name and renaming, in S3 by a single PUT (multipart above 5 GB). Two vaults writing at once can only write the same bytes under the same name, so no coordination is needed.
- **Not trusted.** A segment read from a store is validated exactly as one from a peer (section 10, step 3). A blob is hashed by `fetch` before use.
- **Never pruned.** A store keeps everything it is given. Garbage collection is a non-goal.
- **One listing per run.** A run lists `log/` and `blobs/` once each and works from those lists. It does not ask about objects one at a time.
- **Reachable** means the directory exists, or the bucket answers. A missing directory or a network failure is "unreachable", not an error, for the vault's configured store: its blobs wait locally. For a store named on the command line it is an error.
- **S3 credentials** come from the standard AWS environment variables and shared credentials file. They are never written to `config`.

A vault may name one store of its own, which then receives its history automatically:

- `config` gains `"store"`: a store location or `null` (`R16`, `R24`): the absolute path of a directory, or `s3://<bucket>/<prefix>`. It is independent of `keep_local_history`; either, both or neither may be set.
- `syncit config store <location>` sets it, to an `s3://` location as given or to the absolute form of a directory path, which need not exist yet. `syncit config store none` clears it. `syncit config` prints it, marking a store that is set but not reachable.
- The vault retains blobs if `keep_local_history` is on **or** `store` is set, and `retain()` skips hashes the store already has.
- `flush()`, at the end of every run and snapshot when the store is reachable: upload each blob in `.syncit/blobs/` that the store lacks, then each segment in `.syncit/log/` that the store lacks. Then, if `keep_local_history` is off, delete from `.syncit/blobs/` every blob the store now has. While the store is unreachable, blobs wait in `.syncit/blobs/`.
- The store's `blobs/<hash>` is the last source `fetch` tries, and is one more place `move` can send files to.

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

1. Open both vaults. Refuse if they are the same folder, if one is inside the other (a run would copy the outer vault into the inner, one level deeper each time), or if either has a version other than 1. Lock both.
2. Scan the local vault, then the peer (section 7). Both scan in every mode, so each vault's changes are recorded from its own cache.
3. Exchange segments: each receiver takes every segment in the other vault's `log/` whose name it lacks. A segment is read, validated as in `CRDT.md`, and only then written into `log/`. A segment that fails is not stored, is reported as `rejected`, and does not stop the run.
4. Each receiver reloads its log.
5. Each retaining receiver calls `retain()`. This happens for both before either materialises.
6. Each receiver materialises with the other vault as blob source: the peer first, then the local vault.
7. Collect conflicts from each receiver's log, for paths it holds or wants, and report each once: for the local vault if it is open there, otherwise for the peer.

`push` also warns, once, when the local vault holds files the peer neither holds nor wants, or ignores: `<peer> does not want <n> files (<names>); they were not copied`. The names are the top-level folders or files involved, at most five, then `and <n> more`. `sync` and `pull` do not warn; for them a partial vault taking only part is the ordinary case. The warning does not change the exit code.

Scanning precedes exchange, so entries are written from what each vault held before it learned anything new (`PRD.md`, "A sync run").

### Dry run

`sync`, `push`, `pull` and `move` take `--dry-run`. It is the same procedure with every write to either vault left out; the vault values in memory are updated as usual, so each step sees what the real run would see and prints the same lines. `Options.dry_run` carries the flag.

- Scan: dry mode (section 7).
- Exchange: a segment is validated and its entries added to the receiver's log in memory, not stored. What the other vault's dry scan recorded exists only in its memory, so the receiver also takes every entry of the other vault's in-memory log.
- `retain()` does nothing.
- Materialise: the safety checks run. Where the real run would fetch a blob, the dry run asks only whether a source file for it exists; it does not hash it, so a corrupt source reads as available. Nothing is written, renamed or deleted, and the cache is not saved. Cache rows are updated in memory.

- `move` (section 11): every check runs, on the in-memory state, so a move that would be refused is refused with the same error. The config and cache are not saved and no file is removed; the `moved` events are printed.

The output is the line `dry run: nothing is changed` (an `info` message), then exactly what the real run would print, summary included. The exit code is the real run's. Both vaults are locked and every changed file is hashed, as in a real run.

### A run with a store

> **Milestone 9, not built.** Version 1 as built has no store: no `store` setting, no store argument, and local history is the only place old versions are kept. This subsection is the design to build.

`sync`, `push` and `pull` accept a store location where they accept a vault. A store has no working folder, so nothing is scanned or materialised on its side, and it is not locked.

1. Open and lock the local vault. Open the store: check `format`, list `log/` and `blobs/`.
2. Scan the local vault (section 7).
3. `pull` and `sync`: download every segment the vault lacks, validating each before it is written into `log/`. A segment that fails is `rejected`.
4. Call `retain()` if the vault retains blobs.
5. `pull` and `sync`: materialise, with the store as the blob source.
6. `push` and `sync`: upload every blob for a non-null hash in the local log that the store lacks and that `fetch` can supply from this vault's working files or `.syncit/blobs/`. Then upload every segment the store lacks. Blobs go first, so that a vault reading the store meanwhile rarely sees an entry whose content has not arrived; if it does, the path is `pending` and the next run completes it.
7. Report conflicts as in step 7 above.

A vault with no stored history uploads the versions it has on disk, which is what another vault needs to materialise the current state. Older versions reach the store only from vaults that still hold them.

The vault's configured `store` is the same thing, used automatically: `flush()` at the end of every run and snapshot does step 6 of this procedure for the blobs the vault has retained. Syncing with it by name (`syncit sync <the store>`) is how its contents come back down.

**Resolving the argument.** `s3://…` is a store. Otherwise it is a path: if a vault is found by walking up from it, it is that vault; else if the path is a directory that is empty or contains `format`, it is a store; else it is an error.

## 11. Commands

The vault is found by walking up from the current directory to the first folder containing `.syncit/`. A `<vault>` argument is resolved the same way from the given path. From milestone 9, `sync`, `push` and `pull` also accept a store there (section 10, "A run with a store"). Every command except `init` and `clone` takes the vault's lock first: open `.syncit/lock` and `try_lock()`; if that fails, exit with "vault is in use". Paths stored in `config` are canonicalised.

| Command | Steps |
|---|---|
| `syncit init [dir]` | `dir` defaults to `.`. Error if `dir/.syncit` exists. Create `.syncit/` with `log/`, `blobs/`, `tmp/` and a config of `include: [""]`. Existing files are untouched and recorded by the first scan. |
| `syncit clone <src> <dir>` | Find the vault root `R` above `src`; `sub` is `src` relative to `R` (`""` if equal). `dir` must not exist or must be empty. Create it as a vault with `include: [sub]`. Then run `pull` from `R`. Nothing about `R` is recorded. (`R4`) |
| `syncit sync [--dry-run] <vault>` | Section 10, both directions. (`R1`, `R12`) |
| `syncit push [--dry-run] <vault>` | Section 10; only the peer receives. (`R2`) |
| `syncit pull [--dry-run] <vault>` | Section 10; only the local vault receives. (`R2`) |
| `syncit snapshot` | Scan, `retain()`. No peer. (`R17`) |
| `syncit status` | Dry scan and `resolve`. Writes nothing at all. (`R18`) |
| `syncit get <path>...` | See below. (`R7`) |
| `syncit move [--dry-run] <path>... <vault>` | See below. (`R7`) |
| `syncit blame [path]...` | See below. |
| `syncit config [local-history on\|off]` | See below. (`R15`) |
| `syncit config nickname <name>` | See below. |

`get` and `move` take one or more paths. The steps below run once over all the paths together: `move` makes its checks for every file before removing anything, so it either moves them all or refuses.

### get

The paths of `get` lie inside the *other* vault, as the source of `clone` does: `syncit get /Volumes/Backup/archive/music`, run in a vault, starts holding `music` and fetches it from the vault at `/Volumes/Backup/archive`.

1. Find the vault root above each path. All paths must give the same root `R`, and `R` must not be this vault. Each path relative to `R` is a prefix.
2. Remove every `exclude` at or under each prefix and every `include` under it. Add the prefix to `include`. Save the config.
3. Run `pull` from `R` (section 10).

### move

`syncit move <path>... <vault>`, argument order as in `mv`: the paths are in this vault, relative to the current directory, and the last argument names the target vault. It is `push`, a check, and then the removal of the files from this vault only. No deletion is recorded.

1. Open and lock both vaults, as a run does (section 10). Scan both, so that unrecorded changes are recorded and sent.
2. Collect the paths this vault holds (has a row for) under the given prefixes. If there are none, fail.
3. **Want check.** Each of those paths must be one the target holds or wants, and must not be ignored there. If any is not, fail and list them. Nothing has been sent. `move` never changes what the target wants.
4. Continue the run as `push` (section 10, steps 3 to 7).
5. **Arrival check.** For each path, the target's row must now have the same hash as this vault's row. If any does not, fail and list them, each with the target's `skipped` or `pending` event for it, or else "it holds a different version" (the target has an edit this vault has not pulled, or a conflict this vault's version lost). Nothing is removed. What was pushed stays pushed; running `move` again after the cause is fixed completes it.
6. Remove every `include` at or under each prefix and every `exclude` under it. Add the prefix to `exclude`. Save the config.
7. Remove the rows and save the cache.
8. Delete the files, and directories left empty. A file that fails the unchanged test is left on disk and reported as `skipped` (`R10`); the next scan adopts it. Each removed file is reported as `moved`.

A failure in steps 2, 3 or 5 is exit code 1.

There is no way to stop holding a path without naming a vault that takes it. An earlier `drop` command did that and was removed; see section 17.

### blame

`syncit blame [path]...` prints which vaults recorded the current version of each file. The paths are in this vault, relative to the current directory; with none, every path in the log is considered.

1. Open and lock the vault. Nothing is scanned and nothing is written.
2. For each path in the log at or under a given path, in sorted order: `resolve` it. Skip it unless it is `Resolved` with a winner that is a file.
3. Print a `blame` message with the path and the nicknames of the segments holding the winner, sorted. There is usually one. There are several when vaults recorded the identical entry, as they do for a conflict copy each of them created.

It reads the log only. A vault that receives a file records nothing, so it is never named for it; an edit that no scan has recorded is not attributed to anyone. It has no summary and exits 0.

### config

- `syncit config` prints the vault's settings, as `setting` messages: `local-history: on|off`, then `nickname: <nickname>`.
- `syncit config local-history on|off` sets `keep_local_history`. Turning it off deletes nothing; blobs already stored stay until the user removes them.
- `syncit config nickname <name>` sets `nickname`, and fails if the name is not a valid nickname (section 5). Segments already written keep the old one.

Each form prints the settings afterwards and exits 0. Milestone 9 adds `store`.

## 12. Output and exit codes

Three rules:

- **Code prints the moment something happens.** Nothing is collected for printing, so a long run shows its progress and an interrupted one has already said what it did.
- **stdout is the result; stderr is warnings and errors.** A warning never changes the exit code.
- **`--json`, a flag every command accepts, changes the form and not the stream.** Each line of either stream is then one JSON object with a `type` field (JSON Lines).

How it is built:

- A message is a value of the `Message` enum, which has a text form (`Display`) and a JSON form (`Serialize`).
- `print_stdout(value, json)` and `print_stderr(value, json)` print any value that has both forms. They know nothing else.
- `Options` holds the process-wide settings (`json`, and `dry_run` of section 10) and is passed to everything that can print or write.
- What a run did is kept apart from printing. Each event is printed when it happens and also added to a `Summary`, which holds the events and counts of them. The command returns it; `main` prints the counts as the last line and takes the exit code from them. `move` reads the events to say why a file did not arrive, and tests read them. `init`, `status`, `blame` and `config` have no summary.

### Messages

| `type` | Fields | Text form | Stream |
|---|---|---|---|
| `event` | `kind`, `vault`, `path`, `note` | a marker, a letter and the path, below | stdout |
| `conflict` | `vault`, `path`, `message` | `[<marker> ]U <path> (conflict: <message>)` | stdout |
| `setting` | `name`, `value` | `<name>: <value>` | stdout |
| `blame` | `path`, `nicknames` | `<nickname>[,<nickname>...] <path>` | stdout |
| `info` | `message` | the message | stdout |
| `summary` | the counts | `recorded 3, added 1, …` | stdout |
| `warning` | `message` | `warning: <message>` | stderr |
| `error` | `message` | `error: <message>` | stderr |

An event's text form is a marker, one letter, then `path`, the file's path in its vault. The marker says whose vault the file is in: `->` for the local vault, the one the command runs in, and `<-` for the other. Every command with a peer prints it (`sync`, `push`, `pull`, `clone`, `get`, `move`); `status` and `snapshot` involve one vault and print no marker. `clone` runs in the new vault, so the new folder's lines are `->`. The text form does not name the vault; the JSON form has `vault` and no marker. The letters are those of `git status --short` where git has one:

| Letter | `kind` |
|---|---|
| `A` | `added` |
| `M` | `updated`; `modified` in `status` |
| `D` | `deleted` |
| `C` | `copied`: a conflict copy was written |
| `R` | `moved`: `move` removed the file from this vault |
| `?` | `new`, in `status` |
| `!` | anything that needs attention: `skipped`, `pending`, `incomplete`, `rejected`, and `behind` in `status` |

Anything more goes in parentheses after the path. A `!` line gives its kind and reason: `(skipped: changed since it was scanned)`. A rejected segment has no file, so its path is `.`, the vault itself: `-> ! . (rejected: segment 77b0…: not in canonical form)`.

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

`status` prints, for the local vault: `new`, `modified` and `deleted` (what a scan would record), `behind` (held or wanted paths whose winner is not on disk), `incomplete`, and conflicts as above.

| Exit code | Meaning |
|---|---|
| 0 | Completed; nothing needs attention. `init`, `status`, `blame` and `config` always exit 0 unless they fail. |
| 1 | Failed: bad arguments, not a vault, vault in use, version mismatch, corrupt local file, a refused `move`. |
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
| during `move`, after saving the cache | files with no rows | the scan adopts them; nothing is recorded as deleted |
| while uploading to a store (milestone 9) | some blobs or segments are in the store and others are not | every object is whole or absent; a reader sees a path as `pending` or `incomplete` and waits; the next run uploads the rest |

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
| `copy_name_cases` | the table in section 9 |
| `wanted_cases` | longest prefix decides; `foo` does not match `foobar`; nothing matching means not wanted |

### Stories (one per user story in `PRD.md`)

| Test | Story | Asserts |
|---|---|---|
| `story_a1_edit_and_push` | A.1 | clone, edit in the copy, push; the archive has the edit |
| `story_a2_add_and_push` | A.2 | a file added in the copy appears in the archive; a deletion propagates |
| `story_a3_move` | A.3 | given two paths in one call, `move` sends them to the archive and removes the local files; after sync the archive still has them; `get` of their paths in the archive brings them back |
| `story_a4_clone_subfolder` | A.4 | cloning `a/foo/bar` gives `b/foo/bar/…` only; files outside it are not deleted from `a` by a sync |
| `story_a5_get_and_new_folder` | A.5 | `get` of a second folder; a new top-level folder in the copy reaches the archive, is added to `include`, and a file later added to it in the archive arrives in the copy; the three auto-include examples of section 7 |
| `story_b1_conflict` | B.1 | both sides edit; after sync both hold the same winner and the same copy; exit status reports the conflict; editing the winner and syncing closes it everywhere |
| `story_c1_three_vaults` | C.1 | three vaults synced in several pairings and orders converge; a change relays through a middle vault |
| `story_c2_sync_through_store` | C.2 | two vaults that never sync with each other both sync with one directory store, in either order, and converge, including a conflict; a vault with no history supplies its current files; `clone` is not needed, a new vault `pull`s from the store |
| `story_d1_no_history` | D.1 | with local history off, `.syncit/blobs/` stays empty after syncs |
| `story_d2_history` | D.2 | with local history on, every version's blob is present after edits and syncs, and a version held nowhere else is restored from it; turned off, nothing more is kept and what was kept stays |
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
| `ignored_files` | ignored files are not recorded, not deleted, and not overwritten; a `.syncitignore` that lists itself stays local |
| `ignore_file_syncs` | a `.syncitignore` created in one vault arrives in the other and its rules apply there from the next run; edits to it in two vaults are an ordinary conflict |
| `corrupt_blob_source` | a peer file changed behind its cache is not installed (`R11`) |
| `copy_deleted_stays_deleted` | deleting a conflict copy and syncing does not bring it back |
| `move_refuses` | `move` refuses, removing nothing, when the target does not want a path (and then sends nothing), when the target holds a different version of a file, when no files lie under the paths, and for a vault with itself; an unrecorded edit under the path is recorded, sent and moved |
| `config_command` | `config` prints the settings, turns local history on and off, and the setting takes effect; a vault's nickname is its folder's name until `config nickname` changes it, and an invalid nickname is refused |
| `blame_names_the_recording_vault` | `blame` names the vault that recorded each file's current version and never one that only received it; an identical entry recorded by two vaults names both; deleted files are left out; paths limit the output; after a rename new changes carry the new nickname and old ones keep the old; an unrecorded edit is not attributed |
| `store_layout` | a store has `format`, `log/` and `blobs/`; an empty directory is initialised on first write; a non-empty directory without `format`, and one with version 2, are refused and left untouched |
| `store_hostile` | a bad segment and a wrong blob planted in a store are rejected and not installed; the rest of the run completes |
| `store_configured_gets_log` | `config store` sets and clears the setting, keeping a directory path in absolute form; with `store` set, every run and snapshot leaves the vault's segments in the store as well as its blobs; they wait locally while it is unreachable |
| `s3_store` | the store tests above against an S3-compatible endpoint; skipped unless the environment names one |
| `version_mismatch` | a peer with `"syncit": 2` in its config is refused |
| `cli_smoke` | the built binary runs every command and returns the documented exit codes; results are on stdout and warnings and errors on stderr; with `--json` every line of both streams is an object with a `type`, and the last stdout line of a run is the summary; `push` to a partial vault warns and `sync` does not |
| `message_forms` | each message type's text form and JSON form; the summary's counts and exit code |
| `push_counts_unwanted` | the files a push would not deliver: those the target neither holds nor wants, and those it ignores |
| `dry_run_matches_real_run` | for `sync`, `push` and `pull` over edits, a new file, a deletion and a conflict, and for `move`: a dry run leaves every file of both vaults, `.syncit/` included, as it was, and returns the same `Summary` (counts and events) as the real run that follows; a `move` that would be refused is refused |

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
| R15 | 8, 11 | `story_d2`, `config_command` |
| R16, R24 | 8, 10 | milestones 9 and 10: `story_c2`, `store_layout`, `store_hostile`, `store_configured_gets_log`, `s3_store` |
| R17 | 7, 11 | `story_d3` |
| R18 | 11, 12 | `cli_smoke`, `story_b1` |
| R20 | 5, 7 | `ignored_files`, `ignore_file_syncs` |
| R25 | 5, 10 | `version_mismatch`, `segment_rejects` |

## 15. Milestones

Each milestone ends with `cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check` passing.

- [x] **1. Entry and segment codec.** `entry`, `segment`. Tests: `entry_id_vector`, `canonical_escaping`, `segment_round_trip`, `segment_rejects`.
- [x] **2. Read rules.** `log` with `resolve`. Tests: the `resolve_*` tests, `winner_order`.
- [x] **3. A single vault.** `config`, `cache`, `vault`, `scan`; commands `init`, `snapshot`, `status`. Tests: `wanted_cases`, `story_d3_snapshot` (without the sync step), `same_tick_edit`, `lost_cache` (single-vault part), `interrupted_before_cache`, `cli_smoke`.
- [x] **4. Sync without conflicts.** Blob fetch from working files, segment exchange, materialise steps 1–4 and 7 and the non-conflict part of 6; commands `sync`, `push`, `pull`, `clone`. Tests: `story_a1`, `story_a2`, `story_e1`, `story_c1`, `changed_during_run`, `untracked_in_the_way`, `corrupt_blob_source`.
- [x] **5. Conflicts.** Conflict copies, base, incomplete paths. Tests: `story_b1`, `copy_name_cases`, `edit_beats_delete`, `same_edit_no_conflict`, `new_file_at_existing_path`, `copied_vault`, `restored_backup`, `pending_then_edit`, `copy_deleted_stays_deleted`, `interrupted_transfer`.
- [x] **6. Partial vaults.** Wanted paths in materialise, `clone` of a sub-folder, `get`, `move`. Tests: `story_a3`, `story_a4`, `story_a5`, `move_refuses`.
- [x] **7. Local history.** `.syncit/blobs/`, `keep_local_history`, `retain`, `config`. Tests: `story_d1`, `story_d2`, `config_command`.
- [x] **8. Hardening.** Locking, ignore file, symlink checks, version checks, output format and exit codes. Tests: `hostile_segment`, `symlink_escape`, `ignored_files`, `version_mismatch`, and the remaining parts of `lost_cache` and `story_d3`.
- [x] **8a. Nicknames.** The segment header with a nickname, the `nickname` setting and `config nickname`, the log's record of which vaults recorded each entry, and `blame`. Tests: `header_rejects`, `nickname_default`, `blame_names_the_recording_vault`, and the nickname parts of `config_command`, `message_forms` and `cli_smoke`.
- [ ] **9. Stores.** Everything under "Stores" in section 8 and "A run with a store" in section 10, for directory stores: the layout, the `store` setting and `config store`, `flush`, the store as a blob source, and `sync`, `push`, `pull` and `move` with a store. Tests: `story_c2_sync_through_store`, `store_layout`, `store_hostile`, `store_configured_gets_log`.
- [ ] **10. S3 stores.** The second kind of store, behind the same four operations. Test: `s3_store`.

## 16. Decisions and open questions

### Decided here

These settle, for version 1, questions that `PRD.md` lists as open.

| Question | Answer |
|---|---|
| Store | a log and blobs with no working folder (section 8): one mechanism for keeping history (`R16`) and for syncing through a third place (`R24`). A directory, or an S3 bucket |
| Origin | none. A vault records no other vault; `sync`, `push`, `pull` and `get` name one. Nothing breaks when a folder moves |
| Settings | changed with `syncit config`, not by editing the file |
| Attribution | each vault has a nickname, by default its folder's name, written into the header of the segments it records; `blame` prints it. A label: not unique, not verified, read by no rule. Hostnames and absolute paths are not recorded, since the log is permanent and replicated |
| Ignore list | `.syncitignore` at the vault root, an ordinary file that syncs; names and path prefixes, no wildcards |
| `move` safety | scans first, so unrecorded changes are sent; removes nothing unless the target wants every file and, after the push, holds the same content at every path |
| File metadata | regular files only; symlinks skipped with a warning; permissions and extended attributes not recorded |
| File mtimes | a materialised file gets its entry's mtime |
| Unicode paths | recorded as the OS returns them; names that are not UTF-8 are skipped with a warning |
| Case-insensitive filesystems | no special handling; a case collision surfaces as "untracked file in the way" and nothing is overwritten |
| Conflict-copy name clash | the existing file is left alone; the conflict is reported and stays open |
| New folder outside the wanted paths | recorded, held, and added to `include` automatically (section 7); `move` undoes it |
| Tie-break timestamp | file mtime |
| Empty directories | not tracked; directories emptied by a deletion are removed |
| Cross-path parents | a read rule, not a segment rejection (`CRDT.md` was corrected to match) |

### Still open, to settle before milestone 10

- **The S3 client.** Section 3 lists five dependencies and no others. S3 needs HTTPS and signed requests. The choices are a crate with a blocking API, or shelling out to an installed `rclone` or `aws`. Avoid one that brings an async runtime into a tool that has none.

### Still open, not for version 1

- Wildcards in the ignore file, and per-folder ignore files.
- A stored heads file, if loading the whole log becomes slow.
- Compaction and blob garbage collection.
- Signed segments, and vaults reached over a network (SSH, iroh). Stores cover the case of syncing through a third place. Nicknames give unverified attribution meanwhile; `CRDT.md`, "Planned extension", lists what signing still has to decide.
- A `size` field in entries.
- Unicode normalisation of paths.
- A guard against mass deletion (`R23`). A whole vault on an unmounted drive cannot trigger one, because `.syncit/` is missing too; a missing mount *inside* a vault can.

## 17. Deviations

Record here anything built differently from these documents, with the reason.

- **Not built yet: milestones 9 and 10, so version 1 has no store.** An earlier store was built with milestone 7 (a `store` setting naming a directory that held blobs as `<hash>` files, with no log) and then removed. It could not be synced through, and its layout would have been incompatible with the store of section 8. Until milestone 9, local history is the only setting, and `R16` and `R24` are unmet.

- **A peer's log is read leniently.** Section 6 says a local segment that fails validation stops the command; section 10 says a bad segment in the other vault is rejected and does not stop the run. Both cannot hold for the peer, which must load its own log to scan. A vault opened as a peer skips its invalid segments with a warning; the receiving vault reports them as `rejected`. The vault a command runs in is still strict.
- **`.syncit` is matched without regard to ASCII case** by the scan and by materialise, at any depth. A valid segment may name `.SYNCIT/x`, which on a case-insensitive filesystem is this vault's own `.syncit/`. Segment validation is unchanged (the format is not touched); the path is simply never written.
- **Auto-include of a path that is itself excluded.** Section 7's rule finds no prefix when the new file's own path is in `exclude` (a moved file put back by hand). The path is then moved from `exclude` to `include`, which is what `get` would do.
- **An interrupted scan may be re-recorded as a smaller segment.** Section 7 says the next scan writes the same entries again. A new file whose entry already reached the log is instead recognised by the "file, no row" rule and adopted, so the second segment can hold fewer entries. The set of entries is the same (`interrupted_before_cache`).
- **`drop` was replaced by `move`.** `drop [--check <vault>] [--force] <path>...` stopped holding paths without sending them anywhere, checking for another copy in local history or a named vault. The check could be stale, and the real task was always "put it there, then remove it here". Two uses went with it: saying "never fetch this folder" with no other vault involved, and discarding files no other copy is known for.
- **A conflict copy deleted before any scan recorded it can come back** from the other vault, where the same copy was recorded as an ordinary new file. "A copy the user deleted stays deleted" holds from the first run after the copies were created (`copy_deleted_stays_deleted` syncs once before deleting). This follows from the documents as written; it is noted, not fixed.
