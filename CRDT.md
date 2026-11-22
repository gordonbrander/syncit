# The syncit log

This document specifies the log that vaults replicate: its entries, its file format, how it is read, and how it is written. `PRD.md` describes the rest of the system.

The model is the one CouchDB uses for a document's revision tree, generalised so that an entry may have several parents: a grow-only set of hash-linked entries, merged by union, with a pure function choosing the winner.

## State and merge

- The log is a set of entries. It only grows.
- Merge is set union. Union is commutative, associative and idempotent, so any vaults that have exchanged logs have the same set.
- Everything else (current files, conflicts, history) is a pure function of the set. Nothing derived is stored as truth.

## Entry

An entry records that a path had some content.

| Field | Type | Meaning |
|---|---|---|
| `exec` | boolean | the owner may execute the file; `false` for a tombstone |
| `hash` | 64 hex chars, or `null` | SHA-256 of the file's bytes; `null` is a tombstone |
| `mtime` | integer | milliseconds since the Unix epoch; for a tombstone, the time the scan noticed the deletion |
| `parents` | sorted array of entry ids | the entries of this path that this one replaces; empty for a file the vault had no prior version of |
| `path` | string | relative to the vault root, `/`-separated |

`mtime` is in milliseconds because nanoseconds since the epoch exceed the integers JSON parsers handle exactly.

There is no replica id and no counter. Causality is carried entirely by `parents`.

## Segments

Entries are stored in segments.

- A segment is one file in `.syncit/log/` holding the entries one scan produced.
- It has exactly one author: the vault that scanned. Every other holder has a byte-for-byte copy.
- It is packaging only. The log is the union of entries across segments; how they are grouped does not affect the result.

A segment is a JSON Lines file:

```
{"nickname":"notes","syncit":1}
{"exec":false,"hash":"9c1f…","mtime":1763600000000,"parents":["3f9a…"],"path":"foo/bar/x.txt"}
{"exec":false,"hash":null,"mtime":1763600005000,"parents":["77b0…"],"path":"old.txt"}
```

- The first line is the header: the format version, and the nickname of the vault that scanned.
- **An entry's id is the SHA-256 of its line's bytes, without the newline.**
- Each line, the header included, is in canonical form: keys sorted alphabetically, no whitespace, integers as plain digits, strings with minimal escaping and non-ASCII characters written literally. These are the RFC 8785 (JCS) rules restricted to the types used here, and match the DAG-JSON convention.
- Every logical entry therefore has exactly one id, and two vaults recording the same change produce the same entry.
- Entries within a segment are sorted by path, so the same scan result in the same vault gives the same segment. Two vaults recording the same change write different segments, which hold the same entry.
- A segment's name is the SHA-256 of the whole file. Once written it never changes.
- A scan that finds no changes writes no segment.
- The same entry may appear in several segments; it is one entry.

### Nickname

The nickname says which vault recorded a segment's entries, for people to read (`syncit blame`).

- It is a label and not an identity. It is chosen by the vault's user, by default the folder's name. It need not be unique, a copied vault duplicates it, and nothing verifies it.
- **No rule reads it.** It is outside every entry, so it does not affect entry ids, causality, the winner or conflicts. The statement above stands: there is no replica id.
- It is 1 to 64 bytes of UTF-8 with no whitespace and no control characters, so that it can be printed as it is.
- Renaming a vault changes the segments it writes from then on. Earlier segments are immutable and keep the old nickname.
- An entry found in several segments was recorded by each of their vaults.

## Validation

A segment arrives from another vault and is not trusted. It is rejected whole if:

- its hash does not match its name;
- its first line is not a JSON object with exactly the fields `nickname` and `syncit`, its version is not 1, or its nickname is not valid (see "Nickname");
- any other line is not a JSON object with exactly the four fields and the types above;
- any line is not in canonical form (re-serialising it gives different bytes);
- any path is absolute, has an empty, `.` or `..` component, or lies under `.syncit/`;
- any `parents` array is unsorted or has duplicates.

One check cannot be made on a segment alone, because a parent may live in another segment: an entry whose parent is present in the log but has a different path is **invalid**. This is a read rule, not a reason to reject a segment, so that it depends only on the set of entries and not on the order they arrived in.

## Reading the log

For a path `p`, with `E` the entries for `p`, leaving out invalid entries (see "Validation"), which are never heads and supersede nothing:

1. **Superseded**: every entry named in the `parents` of an entry in `E`.
2. **Heads**: `E` minus the superseded entries.
3. **Winner**: the maximum head by `(hash is not null, mtime, id)`.
4. **Conflict**: the heads have more than one distinct `hash`.
5. **Losers**: each distinct non-null `hash` among the heads other than the winner's. Each becomes a conflict copy, as `PRD.md` describes.

The path exists in the current state if the winner is not a tombstone. The file has the winner's `exec`. Heads that differ only in `exec` are not a conflict: no content would be lost.

In the winner's key, `hash is not null` ranks any file above a tombstone, so an edit beats a concurrent delete. `mtime` makes the most recent edit win. The id breaks all remaining ties.

Properties:

- **Deterministic.** The result depends only on the set, and the key is a total order because ids are unique.
- **Monotonic.** Once superseded, an entry is never a head again.
- **Acyclic.** A parent's id is inside its child's hashed bytes.

The winner is the version every vault agrees to show. It is not a judgement that the right edit won. A person resolves a conflict by opening the conflict copies and editing the winner; that edit closes the conflict (see "Setting base").

## Incomplete logs

If an entry names a parent that is not in the log, the path is **incomplete**. This happens when a segment transfer was interrupted. An incomplete path is not materialised and is reported by `status`. It resolves when the missing segment arrives.

Without this rule a superseded entry would look like a head and produce a false conflict copy.

Scanning is unaffected, because new entries take their parents from the dirstate, not from the heads.

## Writing an entry

The only operation is "this vault's file at `p` is now this content". A scan writes it when the disk differs from the dirstate. Materialise writes it in one case: when it merges two conflicting versions of a text file (see "Merging").

The dirstate row for a path is `(size, mtime, exec, hash, base)`. A path this vault has seen deleted has a deletion record in place of a row, holding only a base. **`base` is the set of entries this vault's file, or its absence, was derived from, and a new entry's parents are the base.** They are not the log's current heads. The two differ whenever the vault has heard of a newer version it has not yet put on disk: the blob was unavailable, the file was skipped because it was being edited, or a run was interrupted after merging. Using the heads there would supersede a version the person never saw.

| Disk | Dirstate row | Entry written | New base |
|---|---|---|---|
| file, hash or exec differs from row | row | file entry, parents = base | the new entry |
| no file | row | tombstone, parents = base | row replaced by a deletion record: the tombstone |
| file | deletion record | file entry, parents = base | record replaced by a row: the new entry |
| file | none; every head of this path is a tombstone | file entry, parents = the heads | the new entry |
| file | none; hash and exec match entries of this path | none | the matching entries |
| file | none; hash matches entries of this path, exec matches none of them | file entry, parents = the entries with that hash | the new entry |
| file | none; hash matches nothing | file entry, parents = `[]` | the new entry |
| no file | deletion record, or none | none | — |

The rows with no dirstate entry are tried in the order given.

A file where this vault saw a deletion is a re-creation, whatever its content: the vault knows what it had seen, so nothing is inferred. If another vault re-created the path meanwhile and this one has not yet put that version on disk, the two re-creations are concurrent and conflict.

The fourth row is the same act with no record of what the vault had seen: a lost dirstate, or a file placed at a path the vault never had. A tombstone has no content, so replacing one loses nothing. `parents = []` would not do: a file put back with its old mtime could yield an entry byte-identical to the path's first, which the tombstone already supersedes.

The fifth row is how a vault with a lost dirstate, or one restored from backup, is recognised as having an old version and brought forward, instead of having its old files recorded as new edits. The sixth is the same vault having changed the executable bit of that version: a child of it, so the result does not depend on a tie-break between two first versions.

## Setting base

After a path is materialised, its base is the set of heads now visible in this vault. When the winner is a tombstone and no file is at the path, that is every head, and it is kept in the path's deletion record. Otherwise:

- the winner, and any head with the same hash, once the winner's content is the file at the path;
- a losing tombstone, once the winner is in place;
- a losing file, once its conflict copy exists on disk or its copy path has entries in the log.

A head that could not be surfaced, because its blob is unavailable, stays out of the base. The next local edit then leaves it as a head, and the conflict stays open until that version has been shown.

When every head is in the base, the next edit names them all as parents and the conflict closes in every vault.

A conflict copy is written without a dirstate row, so the next scan records it as a new file.

## Merging

When materialise merges the two heads of a text file (`PRD.md`, "Merging text"), it writes the merged content at the path and then one entry for it:

- `parents` are the two heads. This is the entry a person's edit would produce with both versions in view ("Closing a conflict" below). The heads are superseded, not removed: the log only grows.
- The merge starts from the heads' lowest common ancestor: the one entry that is an ancestor of both and of no other common ancestor. With none (the file was created independently in two vaults) or several (two vaults merged the same heads differently), there is no merge.
- The winner's content is "ours", `mtime` is the winner's plus 2000 ms, and `exec` is the loser's if the winner kept the ancestor's, else the winner's. Nothing depends on the vault or the clock, so vaults that merge the same heads write the same entry.
- The file is written before the entry. An entry is never published for content that exists nowhere.

No read rule changes. A vault that cannot merge sees an ordinary conflict until the merge entry arrives from one that could.

## Worked cases

| Case | What the log holds | Result |
|---|---|---|
| Linear edits | `A ← B ← C` | head C |
| Concurrent edits | `A ← B`, `A ← C` | heads B and C; conflict; newest wins, other becomes a copy |
| Closing a conflict | `D` with parents `[B, C]` | head D |
| Text merged by a run | `A ← B`, `A ← C`, then `M` with parents `[B, C]`, merged from A | head M; B and C stay in the log |
| Edit against delete | `A ← B`, `A ← tombstone` | B wins; the delete is reported |
| Same edit in two vaults | byte-identical entries | one entry |
| Folder copied, both copies edited | `A ← B`, `A ← C` | ordinary conflict; no identity needed |
| Restored from backup at A, then edited, while `A ← B ← C` exists elsewhere | `A ← D` added | heads C and D; conflict; nothing lost |
| New version heard of but blob unavailable, then local edit | base is still A, so `A ← D` beside `A ← B` | conflict; B not overwritten |
| File created at a path this vault does not have | parents `[]` | conflict with the existing head unless the hash is equal |

## Lookups

A run reads every segment into a map of path to entries and computes heads from that. No index is stored. This is the "log plus derived state" arrangement that Automerge and git use.

If this becomes slow, the fix is a local, rebuildable file of heads per path, updated from new segments only. The read rules allow it: a new entry is a head unless an existing entry names it as a parent, and it removes its own parents from the heads. This needs no change to the format. It is not in version 1.

## Compaction

Compaction keeps the whole log and drops only old blobs.

**Blobs** are compacted (`SPEC.md`, section 8). A vault keeps the content of the versions within `keep_history` steps of a head, and drops the rest, by itself at the end of a run or when the user runs `syncit compact`. This is CouchDB's compaction: it removes the bodies of revisions that are not leaves and keeps the revision tree, so replicas can still tell which version descends from which. The log is that tree. Because it is kept whole, a compacted vault reads the log exactly as any other does, and compaction needs no coordination between vaults.

**The log** is not compacted, and how it would be is not designed. CouchDB cuts its tree at a depth (`_revs_limit`, 1000 by default); syncit keeps every entry. The direction, if it is ever needed, is decided:

- It is never automatic. It is a command the user runs.
- Coordinating it across vaults is the user's job: every vault should share enough history first. CouchDB treats pruning of revision history the same way: a replica that updates past the cut between replications gets spurious conflicts.
- A deep backlog is kept so that problems are rare.

The open part is the mechanism. Dropping old entries leaves parents that name absent entries, which the incomplete-log rule treats as "wait". Compaction of the log must leave a marker so that a cut reads as the start of history.

## Planned extension: signed segments

Not in version 1. This is the intended shape, to be designed together with network transport. Version 1 has the nickname, which gives unverified attribution; signing would make attribution verifiable and leaves the nickname as it is.

- An Ed25519 keypair signs segments. iroh uses the same key type for node ids.
- The key gives attribution and authenticity. **It is never used for causality**, because a key can be duplicated by a restore, a copied vault or a cloned disk. Parents carry causality, so a duplicated key is harmless.
- The signature is inside the segment: a `key` field in the first line, and a final `{"sig":…}` line covering all bytes above it. The header is canonical, so the signed bytes are determined by the content. The segment's name covers the signature.

  ```
  {"key":"<public key>","nickname":"notes","syncit":2}
  {"exec":false,"hash":"9c1f…","mtime":1763600000000,"parents":["3f9a…"],"path":"foo/bar/x.txt"}
  {"sig":"<signature over all bytes above>"}
  ```

- Entries carry no identity. Two vaults recording the same change write two different segments containing the same entry line, and the union of entries makes them one entry. No reconciliation rule is needed.
- An entry found in several segments has several attested authors.
- Signing does not change entries or their ids, so a later format version can read version 1 segments as they are.

Unresolved:

- Whether the key belongs to a vault or to a device. A vault key travels with the folder, so a secret sits on every drive and is copied with it, and a run that scans both vaults signs with each vault's key. A device key stays on the machine, where the operating system can store it, but then a vault's name is a claim the device makes and not something the key proves.
- Where keys are stored and what happens when one is lost.
- Which keys a vault accepts. Until that is decided a signature gives attribution, not authorisation.
- How people name keys: a table of local petnames is the likely answer, with the nickname as the name a vault proposes for itself.
- Compaction of the log, which would replace the original signatures with the compactor's.

## Open questions

- **Unicode paths.** Whether paths are normalised (macOS may return decomposed forms), and what happens to file names that are not valid UTF-8, which JSON cannot carry.
- **Size in the entry.** A `size` field would let `status` report how much data is pending. It is not needed for correctness.
- **Tie-break timestamp.** File mtime reflects when the edit happened but can be set arbitrarily; scan time cannot be forged but reflects when the tool ran.
