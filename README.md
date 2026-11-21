# syncit

syncit lets you keep folders in sync, in both directions.

- **Sync anything to anything**: Any folder can sync with any other. Sync is decentralized and many-to-many.
- **Partial sync**: replicas can hold just part of a larger archive.
- **Backup and restore**: keep snapshots of old versions (optional).

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
-> A letters/anna.txt
-> A notes.txt
recorded 2, added 2, updated 0, deleted 0, conflicts 0, pending 0, skipped 0
```

Each line is one file: `A` added, `M` modified, `D` deleted. The arrow says which folder it is in: `->` yours (for `clone`, the new one), `<-` the other. The last line sums up the run.

Now work in either folder. When you want them to agree, go into one and name the other:

```sh
cd ~/Documents
syncit sync /Volumes/Backup/Documents
```

```
<- M notes.txt
-> A todo.txt
recorded 2, added 1, updated 1, deleted 0, conflicts 0, pending 0, skipped 0
```

Edits, new files and deletions all travel in both directions. Running it again when nothing has changed does nothing.

You always say which folder to sync with. A vault keeps no record of other vaults, so there is nothing to set up and nothing that breaks when a folder or drive moves.

## Syncing two folders

```sh
cd ~/Documents
syncit init

cd /Volumes/Backup/Documents
syncit init

syncit sync ~/Documents
```

How it works:

- Files that exist on only one side are copied to the other.
- Files that exist on both sides with the same content are left alone.
- Files with the same name are kept as [conflicted copies](#in-case-of-conflict-keeps-both). No data is lost.

Any vault can sync with any other. You don't have to route everything through one "main" copy. If A syncs with B and B syncs with C, C gets A's changes.

## One direction only: push and pull

```sh
syncit push /Volumes/Backup/Documents    # send changes to theirs, take nothing
syncit pull /Volumes/Backup/Documents    # take changes from theirs, send nothing
```

Use `push` to back up without bringing anything onto your machine, and `pull` to update from a folder you don't want to change.

If the other folder holds only [part of a larger archive](#working-with-part-of-a-larger-backup), `push` tells you what it did not take:

```
warning: /Volumes/Small excludes 4 files (new.txt, notes.txt, photos/, todo.txt); they were not copied
```

## Working with part of a larger backup

Say a backup drive holds a large archive and you only want one folder of it on your laptop. Clone that sub-folder:

```sh
syncit clone /Volumes/Backup/Archive/photos/2024 ~/Archive
```

You get `~/Archive/photos/2024/…` and nothing else. The folder structure is the same as in the archive, so paths always match. The vault *includes* that folder and *excludes* the rest. What is excluded is simply "not here"; syncing will never delete it from the archive. `get` includes more, and `move` excludes again.

**Get more.** To bring in another folder (or a single file) later, point `get` at it in the archive:

```sh
cd ~/Archive
syncit get /Volumes/Backup/Archive/music
```

```
-> A music/m.mp3
recorded 0, added 1, updated 0, deleted 0, conflicts 0, pending 0, skipped 0
```

It lands at the same place here, `~/Archive/music`, and is synced from then on. `get` also brings in anything else that changed in the archive, as `pull` does.

**Add your own.** Create a new folder anywhere in the vault and it is synced from then on, like everything else. There is no extra step.

**Free up space.** To send files to the archive and remove them from this folder, use `move`. Give the files or folders, then the vault to move them to:

```sh
syncit move music photos/2024/raw /Volumes/Backup/Archive
```

```
<- M music/m.mp3
-> R music/m.mp3
-> R photos/2024/raw/1.dng
recorded 1, added 0, updated 1, deleted 0, conflicts 0, pending 0, skipped 0
```

`move` sends your changes there as `push` does, including edits you haven't synced yet. It then checks that every file arrived, and only then removes them here (the `R` lines). They stay in the archive, and later syncs neither bring them back nor delete them. `syncit get` brings them back.

> **Deleting is not moving.** If you delete a file with Finder or `rm`, that is a real deletion, and the next sync deletes it from the other folder too. `move` removes it *only here*.

If the other vault excludes these files, nothing is sent and nothing is removed:

```
error: nothing moved: /Volumes/Small excludes photos/2024/a.jpg
```

The same goes if the other vault has its own newer version of a file: nothing is removed. Sync with it, then move again.

## In case of conflict, keep both

If the same file was edited in two folders since they last synced, syncit keeps both. The newer one keeps the name; the other is saved as a copy next to it:

```
<- M notes.txt
-> C notes-b842127a.txt
-> U notes.txt (conflict: notes-b842127a.txt)
recorded 2, added 0, updated 1, deleted 0, conflicts 1, pending 0, skipped 0
```

Both folders end up with the same two files. To settle it:

1. Open `notes.txt` and `notes-b842127a.txt` and decide what you want.
2. Save the result as `notes.txt`.
3. Run `syncit sync`. The conflict is closed in every folder.
4. Delete the `notes-…` copy when you no longer need it. It is an ordinary file, and deleting it deletes it everywhere on the next sync.

The `U … (conflict: …)` line is repeated on every sync until you edit the file. If you delete the copy before syncing in step 3, it may reappear once from the other folder; delete it again and it stays gone.

**Edited in one place, deleted in another.** The edit wins and the file is kept everywhere:

```
-> U todo.txt (conflict: deleted in another vault)
```

Delete it again if that is what you want, or edit it to keep it; either one clears the message.

## Version history

By default syncit keeps only the latest version of each file and uses no extra disk space. One setting changes that. Each vault has its own; it is never synced. See it with:

```sh
syncit config
```

```
local-history: off
nickname: Documents
```

(`nickname` is explained under "Who changed what".)

### Turning history on and off

```sh
syncit config local-history on
```

Every version of every file this vault sees from now on, whether saved here or received from another folder, is stored in `.syncit/blobs/`. A good choice for the copy on a large backup drive.

History is simply what has been collected: turn it on late and it starts from there. Turn it off with `syncit config local-history off`. syncit stops storing new versions; the ones already stored stay where they are, and you can delete the contents of `.syncit/blobs/` yourself to get the space back.

### Snapshots

A version is recorded whenever you sync. To record one without syncing, for instance while the backup drive is not attached:

```sh
syncit snapshot
```

Snapshots are delivered with the next sync, so your history is as fine-grained as you make it.

### Getting an old version back

`log` lists the versions of a file, newest first, with the vault that recorded each:

```sh
syncit log notes.txt
```

```
81db67b6 2026-11-20 17:21Z laptop notes.txt (current)
3f9a1c2e 2026-11-19 09:12Z drive notes.txt
-------- 2026-11-18 20:40Z laptop notes.txt (deleted)
5c02e8aa 2026-11-18 08:05Z laptop notes.txt (no content)
```

Each line starts with the first characters of the version's hash; times are UTC. `(no content)` marks a version this vault did not keep, which is every old version unless history was on when it passed through.

`restore` takes a file and enough of a hash to pick one version:

```sh
syncit restore notes.txt 3f9a
```

```
C notes-3f9a1c2e.txt
```

The old version is written beside the file, under the name a conflict copy would have, and your current file is not touched. Look at it, take what you need, and delete it; or move it over the original to go back to it entirely. Either way the next sync treats what you did as an ordinary edit.

To replace the file in one step, or to bring back a file you deleted:

```sh
syncit restore --in-place notes.txt 3f9a
```

syncit records the file as it is first, so with history on, the version you replace can itself be restored later. With history off it cannot, unless another vault still has it.

Each stored version is also a plain file in `.syncit/blobs/`, named by the SHA-256 of its content.

## Checking before you sync

`syncit status` lists the files in this folder that need something, and changes nothing. When nothing needs anything, it prints nothing.

```
✎  notes.txt   modified
≠  plan.txt    conflict    plan-12345678.txt
◐  report.txt  behind
+  todo.txt    new
```

Each line is a mark, the file, and the name of the mark:

| Mark | Name | Meaning |
|---|---|---|
| `✓` | current | here and up to date |
| `✎` | modified | changed here, and not yet recorded |
| `+` | new | new here, and not yet recorded |
| `✗` | removed | gone from this folder |
| `◐` | behind | an out-of-date copy is here; a sync brings the current version, or removes the file if the other vault shows `✗` |
| `○` | missing | meant to be here, but not here yet; a sync brings it |
| `≠` | conflict | both versions are kept, waiting for you; the copy is named |
| `!` | incomplete | an earlier sync did not finish; sync again |
| `-` | excluded | not here by choice: this folder does not include it |
| `·` | ignored | [ignored](#ignoring-files) by this folder |

On its own, `status` looks only at this folder. "Not yet recorded" means changed since the last `sync` or `snapshot` here; it does not say what another vault lacks. For that, add `--vs`. Each line then has two marks and two names: ours first, theirs second.

```sh
syncit status --vs /Volumes/Backup/Documents
```

```
✎ ◐  notes.txt   modified    behind
◐ ✗  old.txt     behind      removed
◐ ✓  report.txt  behind      current
+ ○  todo.txt    new         missing
```

To list every file the archive knows about, and not only those that need something, add `--all`. This is how to see what a folder holds only part of:

```sh
syncit status --all --vs /Volumes/Small
```

```
✓ -  music/        current     excluded    1340 files
✓ ◐  notes.txt     current     behind
✓ ✓  photos/2024/  current     current     212 files
- ✓  videos/       excluded    current     31 files
```

A folder whose files are all in the same state is shown as one line with a count. Name folders or files to look at only those: `syncit status --all music photos/2025`. A removed file is listed only while a folder still has it or has not recorded the removal. `syncit status --help` lists the marks.

To see what a sync with another folder would do, in both directions, add `--dry-run` to the sync. It prints what the sync would print and changes nothing in either folder:

```sh
syncit sync --dry-run /Volumes/Backup/Documents
```

```
dry run: nothing is changed
<- M notes.txt
<- D old.txt
-> A todo.txt
recorded 3, added 1, updated 1, deleted 1, conflicts 0, pending 0, skipped 0
```

`push`, `pull` and `move` take `--dry-run` too. For `move` it shows which files would be removed here, or the error if the move would be refused.

## Reading the output

Every line about a file has a letter, then the file's path in its vault. When a command involves another vault, the line starts with an arrow: `->` for a file in your vault, `<-` for a file in the other one. `snapshot` looks only at your vault and prints no arrow. `status` has its own marks, [above](#checking-before-you-sync).

| Letter | Meaning |
|---|---|
| `A` | added |
| `M` | modified |
| `D` | deleted |
| `C` | a conflict copy was written |
| `R` | removed from this folder by `move` |
| `U` | a conflict is open |
| `!` | needs a look; the reason follows in parentheses |

Lines are printed as things happen, so a long sync shows its progress.

## Who changed what

Every vault has a nickname, and each change it records carries that nickname to every other vault. `blame` shows which vault recorded the current version of each file:

```sh
syncit blame
```

```
laptop notes.txt
drive photos/2024/beach.jpg
```

Give it paths to look at only those files or folders. A file edited but not yet synced or snapshotted is still listed under whoever recorded it last.

A vault's nickname starts as its folder's name, so `~/Documents` and `/Volumes/Backup/Documents` are both `Documents` until you rename one:

```sh
syncit config nickname laptop
```

The new name is used for changes recorded from then on; earlier ones keep the old name. A nickname has no spaces. It is only a label: it need not be unique, nothing checks it, and other vaults see it, so pick something you don't mind sharing.

## Ignoring files

Create a file named `.syncitignore` at the top of a vault, one rule per line:

```
# files and folders with exactly this name, anywhere
.DS_Store
node_modules

# a specific folder, by its path from the top of the vault
build/
projects/scratch/tmp
```

There are no wildcards. Ignored files are never sent, never overwritten and never deleted by a sync.

`.syncitignore` is an ordinary file, so it syncs too and every vault gets the same rules. Rules that arrive during a sync apply from the next run. If you want a vault to have its own rules, add the line `.syncitignore` to its file; that file then stays put and is not replaced by another vault's.

## Good to know

- **Safe to interrupt.** You can stop syncit at any point, or unplug the drive. Run it again and it finishes the job.
- **It won't overwrite work in progress.** If a file changes while a sync is running, that file is skipped and reported, and handled on the next run.
- **Local folders only.** The other folder must be reachable as a path on this machine: an internal disk, an external drive, a mounted network share. syncit does not connect over the network itself.
- **Not synced:** symbolic links, empty folders, file permissions.
- **Renames** are seen as a delete plus a new file, so a renamed file is copied again.
- **A vault inside a vault.** If you drag one vault into another, its files are synced as ordinary files of the outer vault, and syncit prints a warning. The two cannot be synced with each other.
- **One at a time.** Running two syncit commands on the same vault at once fails with "vault is in use".

### In scripts

Results go to standard output; warnings and errors go to standard error.

| Exit code | Meaning |
|---|---|
| 0 | done, nothing needs attention |
| 1 | failed: not a vault, vault in use, bad arguments, a refused `move` |
| 2 | done, but something needs a look: a conflict, or a file that was skipped or is still pending |

Add `--json` to any command and every line, on both streams, is a JSON object with a `type`:

```sh
syncit --json push /Volumes/Backup/Documents
```

```
{"type":"event","kind":"updated","vault":"/Volumes/Backup/Documents","path":"notes.txt","note":""}
{"type":"summary","recorded":1,"added":0,"updated":1,"deleted":0,"conflicts":0,"pending":0,"skipped":0,"incomplete":0,"rejected":0}
```

The types are `event`, `conflict`, `summary`, `setting`, `blame`, `version`, `holding` and `info` on standard output, and `warning` and `error` on standard error. A run that completes ends with one `summary`.

## Commands

| Command | What it does |
|---|---|
| `syncit init [dir]` | make a folder a vault |
| `syncit clone <vault or sub-folder> <dir>` | make a new vault from another, or from part of one |
| `syncit sync <vault>` | sync both ways with another vault; `--dry-run` only shows what would happen |
| `syncit push <vault>` | send changes only; takes `--dry-run` |
| `syncit pull <vault>` | receive changes only; takes `--dry-run` |
| `syncit status [path]... [--vs <vault>]` | show the files that need something; `--all` lists every file; `--vs` compares with another vault |
| `syncit snapshot` | record the current state without syncing |
| `syncit get <path in another vault>...` | start holding those files here, and fetch them |
| `syncit move <path>... <vault>` | send these files to another vault, then remove them here; takes `--dry-run` |
| `syncit blame [path]...` | show which vault recorded the current version of each file |
| `syncit log [path]...` | list the recorded versions of each file, newest first |
| `syncit restore <path> <hash>` | bring back an old version as a copy beside the file; `--in-place` replaces the file |
| `syncit config` | show this vault's settings |
| `syncit config local-history on\|off` | keep old versions inside the vault, or stop |
| `syncit config nickname <name>` | name this vault in the changes it records |

A `<vault>` is the path of any folder in the other vault.

## How it works

For the design and the reasoning behind it, see [PRD.md](PRD.md). The format of the sync log is in [CRDT.md](CRDT.md), and the implementation specification is [SPEC.md](SPEC.md).
