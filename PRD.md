# syncit

## TLDR

People who keep the same files in more than one place (a backup drive, a laptop, a second machine, a bucket) can copy them one way with rsync, but they cannot safely sync both ways, have only part of an archive, or find out when two copies diverged. syncit is a command-line tool that turns any folder into a *vault*: a folder of ordinary files plus a small `.syncit/` directory. Any vault can sync with any other vault, in either direction or both. A vault can have a subset of the files. Conflicting edits are kept side by side instead of one overwriting the other. Keeping old versions is optional and decided per vault.

## User stories

Folders `a`, `b` and `c` are vaults. In the A stories, `a` is an archive on a backup drive and `b` is a working copy on a computer.

- **A.1** As someone with an archive on a backup drive, I want to check it out to my computer, edit files, and sync back, so the archive gets my edits.
- **A.2** As the same person, I want files I add to my working copy to appear in the archive when I sync, so new work is backed up without extra steps.
- **A.3** When my working copy has files I no longer need locally, I want to move them to the archive and no longer have them here, without deleting them from the archive, so I can free space and keep the archive whole.
- **A.4** As someone with a large archive, I want to check out a single sub-folder, edit and add files in it, and sync back, so I only carry what I'm working on.
- **A.5** As the same person, I want to check out several sub-folders, and also add a new top-level folder locally, so that both my edits and the new folder reach the archive.
- **B.1** When two folders have both been edited since they last synced, I want them brought into agreement with every conflicting version kept as a visible file, so no edit is silently lost and I can see what to reconcile.
- **C.1** As someone with three or more copies, I want to sync them in any pairing and order and have them all converge, so I don't have to route everything through one machine.
- **C.2** As someone whose copies are never in the same place, such as a home and an office machine, I want them to sync through a bucket or a shared drive, so neither has to reach the other.
- **D.1** As someone who only cares about the latest version, I want to sync without storing old versions anywhere, so the tool costs me no extra disk.
- **D.2** As someone who wants a permanent archive, I want every synced version retained and recoverable, so I can go back to any earlier state.
- **D.3** When I'm away from my other copies, I want to record a snapshot of my folder's current state, so my history is as fine-grained as I choose and not only as fine as my syncs.
- **E.1** As a developer scripting backups, I want sync to be safe to interrupt and re-run, and to print what it changed and what conflicted, so I can run it unattended.

## Goals

- Using a vault feels like ordinary file management: dragging, dropping, editing in applications, saving, renaming, deleting. There is no staging, committing or branching, and no step between changing a file and syncing it. The experience to aim for is Dropbox, not git.
- The folder a person works in contains ordinary files, readable and editable by any application.
- Any vault can sync with any other vault, and any set of vaults converges to the same state regardless of the order of syncs.
- No edit is silently lost: a conflicting, deleted, or overwritten version is either still on disk under a visible name, merged line for line into the file that replaced it, or was explicitly discarded by policy.
- A vault can have part of the whole without that being mistaken for deletion.
- Whether old versions are kept, and where, is a local choice per vault that does not affect how sync behaves.
- The model is small enough to hold in one's head: files, a log, and blobs.

### Non-goals

- Garbage collection of blobs and compaction of the log. When compaction is added it will be a command the user runs and coordinates across vaults, never automatic.
- Chunking or delta transfer; a changed file is transferred whole.
- Rename detection; a rename is a delete and a create.
- Merging the contents of conflicting files, other than plain text whose edits do not overlap (see "Merging text").
- A background daemon or file watcher; syncit runs when invoked.
- Mapping a sub-folder of one vault onto a different path in another.
- A hub-only mode with a privileged central vault.

## Requirements

**Must**

- **R1** Sync two vaults in both directions so that afterwards they have the same files for the paths both have. (B.1, C.1)
- **R2** Sync in one direction only, as a push or a pull. (A.1, A.2)
- **R3** Propagate edits, additions and deletions. (A.1, A.2, B.1)
- **R4** Create a vault as a partial copy of another by naming a sub-folder, and extend it later with further sub-folders. The directory structure matches the source. (A.4, A.5)
- **R5** A file absent from a partial vault because it was never fetched is not treated as deleted. (A.3, A.4, A.5)
- **R6** A file present in a vault is synced even if it lies outside the sub-folders that vault was created with. (A.5)
- **R7** Provide an explicit way to remove files from a vault without deleting them elsewhere, and to bring them back. (A.3)
- **R8** Detect when the same path was changed independently in two vaults. Choose a winner deterministically, so every vault picks the same one without coordination. (B.1, C.1)
- **R9** Write each losing version next to the winner as `<name>-<short hash>.<ext>`, and print the list of conflicts at the end of the run. (B.1)
- **R10** Never overwrite or delete a local file whose content changed after it was last scanned. (B.1, E.1)
- **R11** Verify the hash of any content before writing it into a working folder. (E.1)
- **R12** Any number of vaults, synced pairwise in any order, converge. A vault can relay changes between two vaults that never meet. (C.1)
- **R13** A vault can operate with no stored history: no blob copies, no extra disk beyond a small log. (D.1)
- **R14** An interrupted run leaves both vaults valid, and re-running completes the work. (E.1)

**Should**

- **R15** A vault can keep every version it sees in a local blob store. (D.2)
- **R16** A vault can send what it has to a content-addressed store, a folder or a bucket, keeping it locally while the store is unreachable. (D.2, D.3)
- **R24** A store holds the log as well as blobs, so vaults that never see each other can sync through it. `R16` and `R24` are one mechanism. (C.1, C.2)
- **R17** Record a snapshot of the current state without syncing. (D.3)
- **R26** List the recorded versions of a file, and bring one back: as a copy beside the file, or in place of it. (D.2)
- **R27** Show what a vault has, lacks and excludes of everything the log knows, alone or against another vault, without changing anything. (A.3, A.4, A.5)
- **R18** Report pending local changes, files waiting on unavailable content, and open conflicts, without changing anything. (E.1)
- **R20** Exclude files by an ignore list. (A.5, E.1)
- **R25** The on-disk format carries a version, so a later change such as a different hash function can be detected and migrated instead of misread. (E.1)

**Could**

- **R21** Limit a single run to a path within the vault. (A.4)
- **R22** Stash blobs with copy-on-write clones where the filesystem supports it, so local history costs no disk until a file changes. (D.2)
- **R23** Abort a run that would delete more than a set fraction of files unless forced. (E.1)

## Proposal

syncit is a single command-line binary written in Rust.

### Vault

A vault is a folder containing a `.syncit/` directory. There is no other kind of participant: every vault is a peer, and none is special. Paths are relative to the vault root and are the same in every vault, so `foo/bar/x.txt` in one vault is `foo/bar/x.txt` in all of them.

```
b/
  .syncit/
    config     # format version, include and exclude prefixes, store, keep_local_history
    log/       # immutable log segments, named by their hash
    dirstate      # local: path -> (size, mtime, hash, base), and deletions seen
    blobs/     # optional: content-addressed file copies
  foo/bar/x.txt   # ordinary files
```

`config` and `dirstate` are local and never replicated. `log/` is replicated. `blobs/` is a store that other vaults may read from.

### Format version and hashing

Every hash in the format is SHA-256: blob names, entry ids and segment names.

The format has a version number, starting at 1. It is recorded in `config` and at the head of every log segment, so a segment is self-describing wherever it ends up. A vault refuses to sync with a peer, or to read a segment, whose version differs from its own. Changing the hash function is a new version and requires migrating a vault: rehashing its content and rewriting its log. Two versions cannot be mixed, because the same content would have two hashes and read as a conflict.

### The log

The log is a grow-only set of entries. `CRDT.md` specifies it in full. Each entry records that a path had some content:

| Field | Meaning |
|---|---|
| path | relative to the vault root |
| hash | hash of the file's bytes, or null for a deletion (a tombstone) |
| exec | whether the file is executable; the only permission recorded, as in Git |
| mtime | the file's modification time when scanned |
| parents | ids of the entries for this path that this one replaces |

- An entry's id is the hash of its canonical bytes. Two byte-identical entries are the same entry.
- Entries are written in segments. A segment is an immutable JSON Lines file named by its hash, holding the entries one scan produced.
- Merging two logs is set union: copy the segments you don't have.
- Only commands that scan write entries: `sync`, `push`, `pull`, `snapshot` and `move`. A segment is frozen the moment it is written locally; a failed transfer re-sends the same bytes.
- A vault has the log entries of every vault it has heard from, directly or through a relay.

### Current state

The current state is computed from the log and never stored as truth. For each path:

1. An entry named as a parent by another entry is superseded. The rest are the heads.
2. Heads with the same hash are the same version.
3. Heads with different hashes conflict. The winner is the maximum by `(is_file, mtime, entry id)`.

`is_file` ranks any file above a tombstone, so an edit beats a concurrent delete. `mtime` makes the most recent edit win. The entry id breaks all remaining ties.

No entry is written to resolve a conflict. Every vault computes the same winner from the same log. A conflict closes when someone next changes the path in a vault where every conflicting version is visible: that entry names all of them as parents. So a person resolves a conflict by opening the copies, patching the winner, and saving.

### Conflict copies

Each losing version that is a file is written beside the winner as `<name>-<short hash>.<ext>`, where the hash is the first 8 hex characters of the loser's content hash, placed before the last extension.

```
notes.txt            # the winner
notes-3f9a1c2e.txt   # the other version
```

```
conflict: notes.txt — kept the newer version, other saved as notes-3f9a1c2e.txt
```

- A copy is created only if its path has no entries in the log. A copy the user has deleted therefore stays deleted.
- On the vault that wrote the losing version, the existing file is renamed to the copy name before the winner is written.
- On other vaults the loser's content is fetched like any other blob.
- Once written, a copy is an ordinary file, recorded by the next scan. Vaults creating the same copy independently produce equal content, which is not a conflict.
- A losing deletion produces no copy; it is reported in the output.

### Merging text

Two conflicting versions of a text file (`.md`, `.markdown`, `.txt`) are merged instead of copied when it can be done without guessing:

- there are exactly two versions, both files, and they descend from one common earlier version;
- a vault receiving in the run keeps history, so the two versions are retained before they are replaced;
- the content of all three can be fetched from some blob source and is UTF-8 text of at most 1 MiB;
- the two sets of edits do not overlap, so a line-by-line three-way merge is clean.

The merged text replaces the file, and is recorded as one more entry whose parents are both versions. They stay in the log, and either can be restored wherever its content is kept. The run reports the file as updated, with the note `merged`, and there is no conflict to report.

- It is opportunistic. Where any condition above fails, the conflict is handled with copies as before.
- A merge never loses a version. It is made only in a run where a vault that receives keeps history: in a `sync` either vault, in a `push` or `pull` the one that receives. That vault retains both versions before either is replaced, so they can always be restored from it. Two vaults that keep no history never merge. A store will count as history does.
- Overlapping edits are never written with conflict markers. That file would sync as if it were the resolution.
- Every vault computes the same merge from the same three versions, and records the identical entry.
- Other formats are left alone: a line merge of structured text can be clean and still broken.

### Restoring a version

`syncit log` lists the recorded versions of a file, each with the short hash of its content, and says which of them this vault can still supply. `syncit restore <path> <hash>` brings one back.

```
syncit log notes.txt
81db67b6 2026-11-20 17:21Z laptop notes.txt (current)
3f9a1c2e 2026-11-19 09:12Z drive notes.txt

syncit restore notes.txt 3f9a
C notes-3f9a1c2e.txt
```

- By default the version is written beside the file under the name a conflict copy of it would have. Nothing is overwritten and nothing is written to the log. The copy is an ordinary new file, recorded by the next scan.
- Moving the copy over the original is an ordinary edit: the old content becomes a new version on top of the current one.
- `--in-place` writes the version at the path itself, replacing the file or bringing back a deleted one. The folder is scanned first, so an edit not yet recorded is in the log before it is replaced, and again afterwards, so the restored version is recorded like any other edit.
- A restored file is dated now, not with the old version's time. Restoring is a new edit, and should not lose a conflict to something older.
- Only content this vault can supply can be restored: a file on disk, or a blob kept by `keep_local_history`. With history off, the version `--in-place` replaces survives only as a hash, unless another vault has it.

### Scanning

A scan compares the working folder to the dirstate and produces new entries.

- A file whose size and mtime match its dirstate row is unchanged. Otherwise it is hashed, and if the hash differs a new entry is written.
- A file whose mtime is not older than the start of the previous scan is hashed regardless, to catch edits within one timestamp tick.
- A dirstate row with no file on disk is a deletion and gets a tombstone. The dirstate then records the deletion in place of the row.
- A file on disk at a path the dirstate records as deleted is a re-creation: it gets an entry that replaces the tombstones this vault had seen.
- A file on disk with neither, wherever it is in the vault, is looked up in the log. If the path is deleted there, the file is new and replaces the deletion. If its hash matches an earlier version of that path, the vault has that version and nothing is written. Otherwise it is new and gets an entry.
- A path in the log with no file, no dirstate row and no recorded deletion is simply one the vault does not have. Nothing is written.

The dirstate is this vault's position in the log, which the log itself cannot hold because it is the same in every vault. For each path it records what is on disk here and its `base`: the entries that state was derived from. For a file, that is a row. For a path this vault has seen deleted, it is a deletion record whose base is the tombstones. A new entry's parents are the base, not the newest entries in the log. A vault that has heard of a newer version but not yet put it on disk therefore produces a conflict when its file is edited, or re-created after a deletion, instead of overwriting a version nobody there saw.

The dirstate is also the record of what this vault has. If it is lost, or the vault is restored from a backup, present files are recognised as old versions and brought forward, and absent files are treated as ones the vault does not have; no deletion is invented. One case cannot be decided without it: a file whose path the log says is deleted may be a re-creation, or an old copy the deletion never reached. The file is kept, as a new version.

### What a vault has

`config` holds the vault's filter: path prefixes to include and to exclude, with the longest matching prefix deciding. A path the filter selects is *included*; any other is *excluded*. `init` includes everything. `clone` of a sub-folder includes that sub-folder. Separately, a path is *present* when its file is on disk here, which is to say the vault *has* it, and *ignored* when `.syncitignore` rules it out.

- Paths from other vaults are materialised only if included.
- A file on disk is included. A scan adds an include for any file it finds at an excluded path, so a new folder placed in the vault is synced with no separate step, and a file the vault has is never left behind. `move` undoes it.
- `get <path>...`, given paths inside another vault, adds an include for them and pulls from that vault.
- `move <path>... <vault>` pushes to that vault, checks that every file arrived, then removes the files and their dirstate rows here and adds an exclude. It refuses, removing nothing, if the target excludes or ignores the files or does not end up having them. There is no way to exclude a path a vault has without naming a vault that takes it.
- Deleting a file with ordinary tools is a deletion and propagates.

### Blobs

A blob is a file's content, addressed by its hash. A blob source is anything that can return the bytes for a hash:

- a vault's working folder, located through its dirstate
- a vault's `.syncit/blobs/`
- a store

Any source may have any subset. Fetched bytes are hashed before use. If no reachable source has a blob, the path is reported as pending and retried on the next run. A missing blob for a superseded version means only that the version cannot be restored.

Two independent settings in `config` decide what a vault retains:

| Setting | Values | Effect |
|---|---|---|
| `keep_local_history` | off / on | When on, every version scanned or received is retained in `.syncit/blobs/`, and blobs offered by peers for versions it lacks are collected. |
| `store` | unset / store location | When set, blobs are uploaded to the store. While the store is unreachable they wait in `.syncit/blobs/`. |

With both off, the vault stores no blobs and its log records hashes only. With both on, blobs are uploaded and also kept locally. Local history is never required to be complete: a vault has the blobs it has. Both are set with `syncit config`.

### Stores

Stores are not in the first build, which keeps history locally or not at all. They are specified here and in `SPEC.md` (milestones 9 and 10), and arrive whole: the `store` setting and syncing through a store together.

A store is a log and a set of blobs with no working folder: the part of a vault that replicates, kept somewhere every vault can reach. It can be a folder, such as a shared or external drive, or an S3 bucket.

- A vault's `store` setting sends its history there: blobs and log segments, after every run and snapshot.
- `sync`, `push` and `pull` accept a store in place of a vault. Two vaults that each sync with the same store converge, without ever reaching each other.
- A store needs no lock and no server. Everything in it is immutable and named by its hash, so writers cannot disagree, and everything read from it is verified.
- A store keeps what it is given. It is never pruned.

Keeping history and syncing through a third place are deliberately one feature. A store holding blobs without the log would be readable only by the vault that filled it; adding the log makes the same store a backup anyone can restore from and a rendezvous anyone can sync through.

### A sync run

Both vaults do each step, acting on their own folder:

1. Scan, and write a segment of new entries.
2. Stash or upload blobs for the new entries, per `keep_local_history` and `store`.
3. Exchange log segments.
4. Compute current state from the merged log.
5. Fetch the blobs needed for included paths.
6. Materialise: write each file to a temporary name in the same directory and rename it into place; apply deletions; write conflict copies; update the dirstate.

Immediately before replacing or deleting a file, it is checked against its dirstate row. If it has changed, the path is skipped and reported; the next run sees the change as a new entry.

Scanning always precedes merging, so entries are written from what the vault had before it learned anything new.

In a push or pull, both vaults still scan, but only the receiving vault takes segments and materialises.

### Copied and restored vaults

A vault has no identity. Two copies of one folder, or a vault restored from a backup, are ordinary vaults. If both are edited, their entries share a parent and neither replaces the other, so they conflict in the usual way.

A vault does have a nickname: a label, by default its folder's name, written into each segment it records so that `syncit blame` can say which vault recorded a version. It is not an identity. It need not be unique, a copied vault duplicates it, nothing verifies it, and no rule of the log reads it.

### Commands

| Command | Does |
|---|---|
| `syncit init [dir]` | Make a folder a vault. |
| `syncit clone <vault>[/<subpath>] <dir>` | Create a vault from another. The vault root is found by walking up from the given path. Records the sub-path as included, and nothing about the source. |
| `syncit sync <vault or store>` | Sync both ways with another vault, or with a store. |
| `syncit push <vault or store>` / `syncit pull <vault or store>` | Sync one way. |
| `syncit snapshot` | Scan and record a snapshot. Does not contact a peer. |
| `syncit status [path]... [--vs <vault>]` | Show each file that needs something: unrecorded changes, files behind or missing, open conflicts. `--all` lists every file, with those current, excluded and ignored; `--vs` shows another vault's state beside this one's. Writes nothing. |
| `syncit get <path in another vault>...` / `syncit move <path>... <vault>` | Change what this vault has. |
| `syncit blame [path]...` | Show which vault recorded the current version of each file. |
| `syncit log [path]...` | List the recorded versions of each file, newest first, with their hashes. |
| `syncit restore <path> <hash>` | Bring back a version as a copy beside the file; `--in-place` replaces the file. |
| `syncit config` | Show or change `keep_local_history`, `nickname` and `store`. |

`sync`, `push`, `pull` and `move` take `--dry-run`, which prints what the run would do and changes nothing in either vault.

`sync`, `push` and `pull` take `--no-remove`, which removes no file because it was deleted elsewhere: a vault that still has the file keeps it, and records it as re-created, so it comes back in the vault that deleted it.

Every command prints as it works: results on stdout, one line per file, and warnings and errors on stderr. `--json` makes every line a JSON object, for scripts.

A vault records no other vault. Every command that involves a second vault names it, so there is no default peer and no hierarchy, and nothing breaks when a folder moves.

### How the stories are met

| Story | Requirements | Mechanism |
|---|---|---|
| A.1, A.2 | R2, R3 | scan writes entries; `push` delivers them and the archive materialises |
| A.3 | R5, R7 | `move` removes dirstate rows, so absence is "not present" |
| A.4 | R4, R5, R27 | `clone` of a sub-path; same directory structure; `status --all` shows what is present and what is excluded |
| A.5 | R4, R6 | `get` for further folders; present files are always recorded and become included |
| B.1 | R1, R8, R9, R10 | parent hashes, the ordering key, conflict copies |
| C.1 | R12 | log union relays through any vault; vaults have no identity to collide |
| C.2 | R16, R24 | a store holds the log and blobs; both vaults sync with it |
| D.1 | R13 | `keep_local_history` off, `store` unset |
| D.2 | R15, R16, R26 | `keep_local_history` on, or `store` set; `log` and `restore` |
| D.3 | R17 | `snapshot` |
| E.1 | R10, R11, R14, R18, R25 | immutable segments, atomic writes, hash checks, `status`, format version |

## Rationale and alternatives

The design keeps three things separate that sync tools usually fuse: what happened (the log), what the bytes are (blobs), and what is on disk (the working folder and its dirstate). Each can be partial, lost, or rebuilt without corrupting the others.

- **Do nothing: `rsync -au` in both directions.** With no memory of the last sync it cannot tell a deletion from a creation, or a one-sided edit from a two-sided one. Deleted files come back and the older of two concurrent edits is silently overwritten. It ties correctness to clocks agreeing across machines.
- **Use `rclone bisync` or `unison`.** These are the boring existing options and they work for two folders. Both keep a snapshot of the last sync per pair of folders. That ties the metadata to a pair: a third folder needs a snapshot per pairing, a moved or restored folder invalidates it, and there is no history. Neither supports drop-without-delete.
- **A pairwise snapshot of our own.** The same model as above. It is the two-replica special case of the log and has the same limits.
- **A state map instead of a log.** Storing only the current entries per path and discarding superseded ones converges equally well, but it forgets. History would then need versioned copies of the map, a second mechanism for something the log gives for free.
- **A hub-only design.** With every sync passing through one central vault, causality reduces to a revision number per path. It rules out syncing two working copies directly and recording snapshots while away from the hub, and it makes one folder special.
- **Symlinks into a blob directory.** The working folder would hold links, as in git-annex. Editors that save by replacing a file break the link, editors that write through it corrupt the blob, and other tools see links where they expect files. It ties the user's files to the storage layout.
- **Sub-path given on every command.** Passing `--path` per run makes scope depend on what was typed last. A bare sync would pull the whole vault, and widening the scope after pruning locally would turn the pruned files into deletions.
- **Mapping a sub-folder to a different root.** Checking out `a/zing/zap` as `b/` needs a path translation at every boundary. Keeping the structure identical removes it.
- **Writing an entry to resolve each conflict.** Unnecessary, since every vault computes the same winner from the log. It would also close a conflict before anyone had looked at it. The one exception is a clean merge of text ("Merging text"): there nothing is chosen over anything else, since every changed line of both versions is in the result, so there is nothing left for a person to decide.
- **Version vectors instead of parent hashes.** A counter per vault per path tolerates an incomplete log and compacts easily. It requires every vault to have a unique identity, and fails silently when that breaks: a vault copied or restored from backup reuses a counter, and a later entry from its twin then replaces a new edit it never saw. Parent hashes name exactly what an entry replaced, so identity is not needed.
- **BLAKE3 instead of SHA-256.** BLAKE3 is faster on a first scan of a large archive and is the hash used by `iroh-blobs`, so blob ids would work unchanged if iroh were later used as a transport. SHA-256 was chosen for now because its tooling is on every machine and it is hardware-accelerated on current CPUs. The format version keeps the switch possible.
- **Requiring blobs to be stored before an entry is published.** This would force every vault to keep or upload history. Treating a missing blob as "not yet" or "not kept" lets retention be optional.

## Prior art

- **rsync**: the size-and-mtime-then-checksum comparison used by the scan.
- **unison, rclone bisync**: two-way sync against a stored snapshot of the previous run.
- **Syncthing**: per-file version vectors across many devices, and conflict copies.
- **git**: a working tree of real files, a content-addressed object store, and an index that caches stat data to avoid rehashing, including its handling of same-tick edits.
- **git-annex**: content-addressed files with partial presence per replica and remote stores; its symlinked working tree is the design avoided here.
- **CouchDB**: a document's revision tree is a grow-only set of hash-linked revisions merged by union, with a deterministic winner. The log is that model with several parents per entry, and with losing versions surfaced as files instead of hidden.
- **Dropbox**: conflicted copies surfaced as sibling files.

## Open questions

- **File metadata.** The executable bit is recorded, and a change to it alone is a new version, as in Git. Whether symlinks or extended attributes ever are is open.
- **Case-insensitive filesystems.** How two paths differing only in case are handled on macOS and Windows.
- **Vaults that are not mounted paths.** The proposal assumes a vault is reachable as a directory. Stores cover syncing through a bucket. Syncing two vaults directly over SSH or iroh is undecided. Signed segments, sketched in `CRDT.md`, belong to the same design.
- **Ignore list.** Settled for version 1: `.syncitignore` at the vault root, an ordinary file that syncs with the rest. Wildcards and per-folder lists are open.
- **Mass-delete guard.** Whether R23 is on by default, and at what threshold.
- **Tie-break timestamp.** File mtime reflects when the edit happened but can be set arbitrarily; scan time cannot be forged but reflects when the tool ran.
- **Conflict-copy names.** Behaviour when a short hash collides with an existing unrelated file, and for names with several extensions such as `.tar.gz`.
- **Log growth.** Compaction is a non-goal for now; at what size it becomes necessary is unknown, and its mechanism is undesigned (see `CRDT.md`).
- **Unicode paths.** Whether paths are normalised, and what happens to file names that are not valid UTF-8.
- **Size in the entry.** Whether entries record file size, so `status` can report how much data is pending.
