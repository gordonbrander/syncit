# The syncit log

This document specifies the log that vaults replicate: its entries, its file format, how it is read, and how it is written. `PRD.md` describes the rest of the system.

The model is the one CouchDB uses for a document's revision tree, generalised so that an entry may have several parents: a grow-only set of hash-linked entries, merged by union, with a pure function choosing the winner.

## State and merge

- The log is a set of entries. It only grows.
- Merge is set union. Union is commutative, associative and idempotent, so any vaults that have exchanged logs hold the same set.
- Everything else (current files, conflicts, history) is a pure function of the set. Nothing derived is stored as truth.

## Entry

An entry records that a path had some content.

| Field | Type | Meaning |
|---|---|---|
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
{"syncit":1}
{"hash":"9c1f…","mtime":1763600000000,"parents":["3f9a…"],"path":"foo/bar/x.txt"}
{"hash":null,"mtime":1763600005000,"parents":["77b0…"],"path":"old.txt"}
```

- The first line is the format version.
- **An entry's id is the SHA-256 of its line's bytes, without the newline.**
- Each line is in canonical form: keys sorted alphabetically, no whitespace, integers as plain digits, strings with minimal escaping and non-ASCII characters written literally. These are the RFC 8785 (JCS) rules restricted to the types used here, and match the DAG-JSON convention.
- Every logical entry therefore has exactly one id, and two vaults recording the same change produce the same entry.
- Entries within a segment are sorted by path, so the same scan result gives the same segment.
- A segment's name is the SHA-256 of the whole file. Once written it never changes.
- A scan that finds no changes writes no segment.
- The same entry may appear in several segments; it is one entry.

## Validation

A segment arrives from another vault and is not trusted. It is rejected whole if:

- its hash does not match its name, or its version is not 1;
- any line is not a JSON object with exactly the four fields and the types above;
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

The path exists in the current state if the winner is not a tombstone.

In the winner's key, `hash is not null` ranks any file above a tombstone, so an edit beats a concurrent delete. `mtime` makes the most recent edit win. The id breaks all remaining ties.

Properties:

- **Deterministic.** The result depends only on the set, and the key is a total order because ids are unique.
- **Monotonic.** Once superseded, an entry is never a head again.
- **Acyclic.** A parent's id is inside its child's hashed bytes.

The winner is the version every vault agrees to show. It is not a judgement that the right edit won. A person resolves a conflict by opening the conflict copies and editing the winner; that edit closes the conflict (see "Setting base").

## Incomplete logs

If an entry names a parent that is not in the log, the path is **incomplete**. This happens when a segment transfer was interrupted. An incomplete path is not materialised and is reported by `status`. It resolves when the missing segment arrives.

Without this rule a superseded entry would look like a head and produce a false conflict copy.

Scanning is unaffected, because new entries take their parents from the stat cache, not from the heads.

## Writing an entry

The only operation is "this vault's file at `p` is now this content". A scan writes it when the disk differs from the stat cache.

The stat cache row for a path is `(size, mtime, hash, base)`. **`base` is the set of entries this vault's file was derived from, and a new entry's parents are the row's base.** They are not the log's current heads. The two differ whenever the vault has heard of a newer version it has not yet put on disk: the blob was unavailable, the file was skipped because it was being edited, or a run was interrupted after merging. Using the heads there would supersede a version the person never saw.

| Disk | Cache row | Entry written | New base |
|---|---|---|---|
| file, hash differs from row | present | file entry, parents = base | the new entry |
| no file | present | tombstone, parents = base | row removed |
| file | none; hash matches entries of this path | none | the matching entries |
| file | none; hash matches nothing | file entry, parents = `[]` | the new entry |
| no file | none | none | — |

The third row is how a vault with a lost cache, or one restored from backup, is recognised as holding an old version and brought forward, instead of having its old files recorded as new edits.

## Setting base

After a path is materialised, its base is the set of heads now visible in this vault:

- the winner, and any head with the same hash, once the winner's content is the file at the path;
- a losing tombstone, once the winner is in place;
- a losing file, once its conflict copy exists on disk or its copy path has entries in the log.

A head that could not be surfaced, because its blob is unavailable, stays out of the base. The next local edit then leaves it as a head, and the conflict stays open until that version has been shown.

When every head is in the base, the next edit names them all as parents and the conflict closes in every vault.

A conflict copy is written without a cache row, so the next scan records it as a new file.

## Worked cases

| Case | What the log holds | Result |
|---|---|---|
| Linear edits | `A ← B ← C` | head C |
| Concurrent edits | `A ← B`, `A ← C` | heads B and C; conflict; newest wins, other becomes a copy |
| Closing a conflict | `D` with parents `[B, C]` | head D |
| Edit against delete | `A ← B`, `A ← tombstone` | B wins; the delete is reported |
| Same edit in two vaults | byte-identical entries | one entry |
| Folder copied, both copies edited | `A ← B`, `A ← C` | ordinary conflict; no identity needed |
| Restored from backup at A, then edited, while `A ← B ← C` exists elsewhere | `A ← D` added | heads C and D; conflict; nothing lost |
| New version heard of but blob unavailable, then local edit | base is still A, so `A ← D` beside `A ← B` | conflict; B not overwritten |
| File created at a path this vault does not hold | parents `[]` | conflict with the existing head unless the hash is equal |

## Lookups

A run reads every segment into a map of path to entries and computes heads from that. No index is stored. This is the "log plus derived state" arrangement that Automerge and git use.

If this becomes slow, the fix is a local, rebuildable file of heads per path, updated from new segments only. The read rules allow it: a new entry is a head unless an existing entry names it as a parent, and it removes its own parents from the heads. This needs no change to the format. It is not in version 1.

## Compaction

Not designed. The direction is decided:

- Compaction is never automatic. It is a command the user runs.
- Coordinating it across vaults is the user's job: every vault should share enough history first. CouchDB treats pruning of revision history the same way.
- A deep backlog is kept so that problems are rare.

The open part is the mechanism. Dropping old entries leaves parents that name absent entries, which the incomplete-log rule treats as "wait". Compaction must leave a marker so that a cut reads as the start of history.

## Planned extension: signed segments

Not in version 1. This is the intended shape, to be designed together with network transport.

- Each machine generates an Ed25519 keypair on first run. iroh uses the same key type for node ids.
- The key gives attribution and authenticity. **It is never used for causality**, because a key can be duplicated by a restore, a copied vault or a cloned disk. Parents carry causality, so a duplicated key is harmless.
- The signature is inside the segment: a `key` field in the first line, and a final `{"sig":…}` line covering all bytes above it. The segment's name covers the signature.

  ```
  {"key":"<public key>","syncit":2}
  {"hash":"9c1f…","mtime":1763600000000,"parents":["3f9a…"],"path":"foo/bar/x.txt"}
  {"sig":"<signature over all bytes above>"}
  ```

- Entries carry no identity. Two vaults recording the same change write two different segments containing the same entry line, and the union of entries makes them one entry. No reconciliation rule is needed.
- An entry found in several segments has several attested authors.
- Signing does not change entries or their ids, so a later format version can read version 1 segments as they are.

Unresolved: where keys are stored and what happens when one is lost; which keys a vault accepts; and compaction, which would replace the original signatures with the compactor's.

## Open questions

- **Unicode paths.** Whether paths are normalised (macOS may return decomposed forms), and what happens to file names that are not valid UTF-8, which JSON cannot carry.
- **Size in the entry.** A `size` field would let `status` report how much data is pending. It is not needed for correctness.
- **Tie-break timestamp.** File mtime reflects when the edit happened but can be set arbitrarily; scan time cannot be forged but reflects when the tool ran.
