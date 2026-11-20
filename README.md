# syncit

syncit lets you keep folders in sync, in both directions.

- **Sync anything to anything**: Any folder can sync with any other... a laptop, a backup drive, a second computer.
- **Partial replication**: A folder can hold just part of a larger archive, without the missing files being mistaken for deletions.
- **Conflict resolution**: When the same file was edited in two places, both versions are kept as visible files. Nothing is silently overwritten.
- **Archiving**: Keeping old versions is optional, and you choose it per folder.

syncit works seamlessly with ordinary files and folder management. Create, edit, move and delete, then run `syncit sync` when you want the folders to agree. 

## Install

You need [Rust](https://rustup.rs) 1.89 or later. syncit runs on macOS and Linux.

```sh
git clone <this repository> syncit
cd syncit
cargo install --path .
```

Check that it worked:

```sh
syncit --help
```

## Quick start

Turn a folder you already have into a syncable *vault*.

```sh
cd ~/Documents
syncit init
```

Make a second copy somewhere else, such as a backup drive:

```sh
syncit clone ~/Documents /Volumes/Backup/Documents
```

```
/Volumes/Backup/Documents:
  added: letters/anna.txt
  added: notes.txt
recorded 2, added 2, updated 0, deleted 0, conflicts 0, pending 0, skipped 0
```

Now work in either folder. When you want them to agree, run `sync` from inside one of them:

```sh
cd /Volumes/Backup/Documents
syncit sync
```

```
/Users/me/Documents:
  added: todo.txt
/Volumes/Backup/Documents:
  updated: notes.txt
recorded 2, added 1, updated 1, deleted 0, conflicts 0, pending 0, skipped 0
```

Edits, new files and deletions all travel in both directions. Running it again when nothing has changed does nothing.

## Syncing two folders

**Starting from one folder.** `init` it, then `clone` it, as in the quick start. A clone remembers where it came from, so inside the clone a plain `syncit sync` is enough.

**Both folders already exist.** `init` each one, then name the other folder when you sync:

```sh
cd ~/Documents && syncit init
cd /Volumes/Backup/Documents && syncit init
syncit sync ~/Documents
```

Files that exist on only one side are copied to the other. Files that exist on both sides with the same content are left alone. Files with the same name and different content become a [conflict](#when-both-sides-changed-a-file), so nothing is lost.

**More than two folders.** Any vault can sync with any other; just name it: `syncit sync /path/to/other`. You don't have to route everything through one "main" copy. If A syncs with B and B syncs with C, C gets A's changes.

You can run syncit from any sub-folder of a vault. It always works on the whole vault.

## One direction only: push and pull

```sh
syncit push    # send my changes to the other folder; take nothing back
syncit pull    # take the other folder's changes; send nothing
```

Like `sync`, both use the folder you cloned from unless you name another: `syncit push /Volumes/Backup/Documents`.

Use `push` to back up without bringing anything onto your machine, and `pull` to update from a folder you don't want to change.

## Working with part of a larger backup

Say a backup drive holds a large archive and you only want one folder of it on your laptop. Clone that sub-folder:

```sh
syncit clone /Volumes/Backup/Archive/photos/2024 ~/Archive
```

You get `~/Archive/photos/2024/…` and nothing else. The folder structure is the same as in the archive, so paths always match. Everything you didn't take is simply "not here"; syncing will never delete it from the archive.

**Get more.** Bring in another folder (or a single file) later:

```sh
cd ~/Archive
syncit get music
```

`get` fetches what the vault already knows about. Run `syncit pull` afterwards if the archive may have changed since your last sync.

**Add your own.** Create a new folder anywhere in the vault and it is synced from then on, like everything else. There is no extra step.

**Free up space.** To remove files from this folder but keep them in the archive, use `drop`:

```sh
syncit drop music photos/2024/raw
```

> **Deleting is not dropping.** If you delete a file with Finder or `rm`, that is a real deletion, and the next sync deletes it from the other folder too. `drop` removes it *only here*.

`drop` first records any unsaved changes, and refuses if it would remove the only known copy of something:

```
error: nothing dropped: no other copy is known of lonely.txt. Sync first, or use --force
```

Sync and try again, or pass `--force` if you really mean it. `syncit get` brings dropped files back.

## In case of conflict, keeps both

If the same file was edited in two folders since they last synced, syncit keeps both. The newer one keeps the name; the other is saved as a copy next to it:

```
/Users/me/Documents:
  updated: notes.txt
/Volumes/Backup/Documents:
  copied: notes-b842127a.txt
conflict: notes.txt — kept the newer version, other saved as notes-b842127a.txt
recorded 2, added 1, updated 1, deleted 0, conflicts 1, pending 0, skipped 0
```

Both folders end up with the same two files. To settle it:

1. Open `notes.txt` and `notes-b842127a.txt` and decide what you want.
2. Save the result as `notes.txt`.
3. Run `syncit sync`. The conflict is closed in every folder.
4. Delete the `notes-…` copy when you no longer need it. It is an ordinary file, and deleting it deletes it everywhere on the next sync.

The `conflict:` line is repeated on every sync until you edit the file. If you delete the copy before syncing in step 3, it may reappear once from the other folder; delete it again and it stays gone.

**Edited in one place, deleted in another.** The edit wins and the file is kept everywhere:

```
conflict: todo.txt — kept the file; it was deleted in another vault
```

Delete it again if that is what you want, or edit it to keep it; either one clears the message.

## Checking before you sync

`syncit status` shows what is waiting, and changes nothing:

```
/Users/me/Documents:
  new: todo.txt
  modified: notes.txt
```

| Line | Meaning |
|---|---|
| `new`, `modified`, `deleted` | changes here that the next sync will send |
| `behind` | a newer version is known but not yet in this folder |
| `incomplete` | an earlier sync was interrupted; sync again |
| `conflict` | both versions are on disk, waiting for you |

## Keeping old versions

By default syncit keeps only the latest version of each file and uses no extra disk space. Two settings change that. Both live in `.syncit/config` inside the vault, and you change them by editing that file in a text editor. Each vault has its own settings; they are never synced.

```json
{
  "syncit": 1,
  "origin": "/Users/me/Documents",
  "include": [""],
  "exclude": [],
  "keep": false,
  "remote": null
}
```

Only edit `keep` and `remote`. The other fields are managed by syncit.

### A local archive: `keep`

```json
  "keep": true,
```

With `keep` on, every version of every file this vault sees, whether saved here or received from another folder, is stored in `.syncit/blobs/`. A good choice for the copy on a large backup drive.

To turn it off, set it back to `false`. syncit stops storing new versions. Versions already stored stay where they are; delete the contents of `.syncit/blobs/` yourself if you want the space back.

### A shared archive: `remote`

```json
  "remote": "/Volumes/Vault/syncit-store",
```

With `remote` set, old versions are stored in that folder instead of inside the vault. Several vaults can share one store. The path must be absolute, and the folder must already exist.

If the store isn't reachable, say the drive is unplugged, versions wait in `.syncit/blobs/` and are moved to the store the next time syncit runs and finds it. Set `remote` back to `null` to stop.

You can use both at once: with `keep` on, versions are sent to the store and also kept locally.

### Snapshots

A version is recorded whenever you sync. To record one without syncing, for instance while the backup drive is not attached:

```sh
syncit snapshot
```

Snapshots are delivered with the next sync, so your history is as fine-grained as you make it.

### Getting an old version back

This version of syncit stores old versions but has no command to restore one. Each stored version is a plain file in `.syncit/blobs/` or the store, named by the SHA-256 of its content. You can open or copy it directly.

## Ignoring files

Create `.syncit/ignore` in a vault, one rule per line:

```
# files and folders with exactly this name, anywhere
.DS_Store
node_modules

# a specific folder, by its path from the top of the vault
build/
projects/scratch/tmp
```

There are no wildcards. Ignored files are never sent, never overwritten and never deleted by a sync. The ignore file is not synced, so each vault has its own.

## Good to know

- **Safe to interrupt.** You can stop syncit at any point, or unplug the drive. Run it again and it finishes the job.
- **It won't overwrite work in progress.** If a file changes while a sync is running, that file is skipped and reported, and handled on the next run.
- **Local folders only.** The other folder must be reachable as a path on this machine: an internal disk, an external drive, a mounted network share. syncit does not connect over the network itself.
- **Not synced:** symbolic links, empty folders, file permissions.
- **Renames** are seen as a delete plus a new file, so a renamed file is copied again.
- **A vault inside a vault.** If you drag one vault into another, its files are synced as ordinary files of the outer vault, and syncit prints a warning. The two cannot be synced with each other.
- **One at a time.** Running two syncit commands on the same vault at once fails with "vault is in use".

### In scripts

| Exit code | Meaning |
|---|---|
| 0 | done, nothing needs attention |
| 1 | failed: not a vault, vault in use, bad arguments |
| 2 | done, but something needs a look: a conflict, or a file that was skipped or is still pending |

## Commands

| Command | What it does |
|---|---|
| `syncit init [dir]` | make a folder a vault |
| `syncit clone <vault or sub-folder> <dir>` | make a new vault from another, or from part of one |
| `syncit sync [other]` | sync both ways |
| `syncit push [other]` | send changes only |
| `syncit pull [other]` | receive changes only |
| `syncit status` | show what is waiting; change nothing |
| `syncit snapshot` | record the current state without syncing |
| `syncit get <path>...` | start holding these paths, and fetch them |
| `syncit drop [--force] <path>...` | stop holding these paths, without deleting them elsewhere |

`[other]` defaults to the vault this one was cloned from.

## How it works

For the design and the reasoning behind it, see [PRD.md](PRD.md). The format of the sync log is in [CRDT.md](CRDT.md), and the implementation specification is [SPEC.md](SPEC.md).
