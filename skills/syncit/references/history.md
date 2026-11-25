# History and rollback

## What a vault keeps

```sh
syncit config            # prints the vault's config.toml: keep-history, autocompact, nickname, include, exclude, remotes
```

- `keep-history` is how many versions back the vault stores file content for. A new vault keeps `1`: the version before the current one can be restored, older ones cannot. A new remote vault keeps `all`.
- Settings are per vault and are not synced. A big backup drive can keep `all` while a laptop keeps `1`.
- History only grows from the moment the setting is raised. Raising it does not bring back versions already discarded.
- `syncit log <path>` lists every version ever recorded, even those whose content is gone. Those are marked `(no content)`. `syncit --json log <path>` gives the full hash and `"available": true|false` for each.

If the user cares about being able to roll back, suggest raising it, and say that it costs disk space:

```sh
syncit config keep-history 10     # or: all
```

## Undelete a file

```sh
syncit log sub/report.txt
# -------- 2026-11-24 06:13Z laptop sub/report.txt (current, deleted)
# 73cb3858 2026-11-24 06:13Z laptop sub/report.txt
syncit restore --in-place sub/report.txt 73cb
```

The restored file is a new change: the next sync puts it back in the other vaults too.

## Roll back a folder, or undo a bad sync

There is no single command for this. `restore` takes one file, and there is no "undo". Do it file by file:

1. Find what the sync changed. Its output is the best record: `M` (modified), `D` (deleted), `A` (added) and `R` (renamed: `R old -> new`) lines, with `->` for files changed in this vault. If the output is gone, `syncit log <folder>` lists versions of everything under it, newest first.
2. For each modified or deleted file, find the version to go back to with `syncit log <file>`. It is the one below the current line.
3. Check that it is restorable (no `(no content)` mark), then `syncit restore --in-place <file> <hash>`.
4. Files the sync *added* that the user does not want: delete them. That deletion will spread on the next sync, which is what undoing an addition means; confirm with the user.
5. Tell the user plainly which files could not be rolled back because their content was not kept.
6. Run `syncit sync --dry-run <vault>` and show the user what the rollback will send to the other vault before syncing.

If many files are affected and another vault still has the good versions (it has not synced since), it can be simpler to leave that vault alone and ask the user how they want to proceed, since syncing with it would carry the bad changes there.

## A version this vault no longer has

`error: the content of that version is not in this vault` means the content was discarded here. Another vault that keeps more history may have it:

```sh
syncit config keep-history 10
syncit pull <vault with more history>    # collects the versions it has
syncit restore <file> <hash>
```

As a last resort, each stored version is a plain file at `.syncit/blobs/<first 2 chars of hash>/<full hash>`. Read from there; never write there.

## Record a version without syncing

```sh
syncit snapshot
```

Useful before a risky edit, or when the other vault is not reachable. Versions are recorded per run, not per save: edits made between two runs are one version.

## Free up space

Old content is discarded automatically at the end of each `sync`, `push`, `pull` and `snapshot` (`autocompact: on`). To control it by hand:

```sh
syncit config autocompact off
syncit compact --dry-run     # prints what would be dropped
syncit compact
```

- With `autocompact: on` there is normally `nothing to drop`: old content is already gone. The only way to free more is a lower `keep-history`.
- Compaction is permanent. Ask the user before `compact`, and before lowering `keep-history`.
- Lowering `keep-history` deletes nothing at that moment; the next compaction does. `syncit config` reports how many stored versions the new setting no longer covers.
- The current version of a file is never discarded, and a file in conflict keeps every version until the conflict is closed.
- `keep-history 0` stores no old versions at all: nothing can be restored, a `restore --in-place` cannot be undone, and text files stop merging on their own.

For a remote vault, add `--vault`: `syncit config --vault origin keep-history 5`, `syncit compact --vault origin`. Do not compact a remote while another computer may be pushing to it.

## Who changed a file

`syncit blame <path>` shows which vault recorded the current version; several names (`laptop,drive`) mean a merge of their edits, or the same change recorded by each. It gives no time. For "who changed this and when", use `syncit log <path>`, which shows the vault and time of every version. Vaults are identified by nickname (`syncit config nickname <name>`).
