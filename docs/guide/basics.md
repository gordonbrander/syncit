---
title: Basics
---

[← All guides](index.md)

# Basics

Everyday syncing: keeping two folders in step, sending changes in one direction, copying files in, and checking what will happen before it does.

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
- Files with the same name are kept as [conflicted copies](conflicts.md). No data is lost.

Any vault can sync with any other. You don't have to route everything through one "main" copy. If A syncs with B and B syncs with C, C gets A's changes.

You always say which folder to sync with. A vault keeps no record of other vaults, so there is nothing to set up and nothing that breaks when a folder or drive moves.

## One direction only: push and pull

```sh
syncit push /Volumes/Backup/Documents    # send changes to theirs, take nothing
syncit pull /Volumes/Backup/Documents    # take changes from theirs, send nothing
```

Use `push` to back up without bringing anything onto your machine, and `pull` to update from a folder you don't want to change.

If the other folder has only [part of a larger archive](partial-sync.md), `push` tells you what it did not take:

```
warning: /Volumes/Small excludes 4 files (new.txt, notes.txt, photos/, todo.txt); they were not copied
```

## Copying files in from any folder

`cp` copies files into a vault from anywhere. The folder they come from does not have to be a vault, and you can run it from there:

```sh
cd ~/Desktop
syncit cp scans notes.txt /Volumes/Backup/Archive
```

```
A notes.txt
A scans/new.pdf
M scans/receipt.pdf
recorded 0, added 2, updated 1, deleted 0, renamed 0, conflicts 0, pending 0, skipped 0
```

The last argument is a folder in the vault, which need not exist yet. Each file or folder you name lands in it under its own name, as with `cp -R`. Name `.` to copy the contents of the folder you are in.

A file the vault lacks is added and one that differs is replaced. Files that are already the same are left alone, and nothing is deleted. What the vault [ignores](#ignoring-files) is not copied, nor are symbolic links.

To see what it would change in the vault first, add `--dry-run`:

```sh
syncit cp --dry-run . /Volumes/Backup/Archive
```

`cp` only copies. The files are then ordinary changes in that vault, as if you had dragged them in, and its next `sync` or `snapshot` records them. Before replacing anything, `cp` records the vault as it is, so a replaced version can be restored from [history](history.md).

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
| `✓` | current | File up to date |
| `✎` | modified | Changed here, not yet snapshotted |
| `+` | new | New here, not yet snapshotted |
| `→` | renamed | Moved here, not yet snapshotted; the old path is shown before the arrow, as in `→  notes/ -> docs/  renamed  12 files` |
| `✗` | removed | Gone from this folder |
| `◐` | behind | Our copy is out-of-date. Sync to update. |
| `○` | missing | File not present (not yet synced) |
| `≠` | conflict | Conflict. Both versions are kept. |
| `!` | incomplete | An earlier sync did not finish. Sync again. |
| `-` | excluded | Filtered from partial replica |
| `·` | ignored | [Ignored](#ignoring-files) due to `.syncitignore` |

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

To list every file the archive knows about, and not only those that need something, add `--all`. This is how to see what a folder has only part of:

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

### Dry runs

To see what a sync with another folder would do, in both directions, add `--dry-run` to the sync. It prints what the sync would print and changes nothing in either folder:

```sh
syncit sync --dry-run /Volumes/Backup/Documents
```

```
dry run: nothing is changed
<- M notes.txt
<- D old.txt
-> A todo.txt
recorded 3, added 1, updated 1, deleted 1, renamed 0, conflicts 0, pending 0, skipped 0
```

`push`, `pull`, `move`, `drop` and `cp` take `--dry-run` too. For `move` and `drop` it shows which files would be removed here, or the error if they would refuse.

### Syncing without removing anything

To sync without removing anything, add `--no-remove`. A file deleted in one folder is kept in the other and put back where it was deleted. Both are adds:

```sh
syncit sync --no-remove /Volumes/Backup/Documents
```

```
<- A old.txt (kept)
-> A old.txt
recorded 2, added 2, updated 0, deleted 0, renamed 0, conflicts 0, pending 0, skipped 0
```

The deletion is undone, not postponed: later syncs leave the file alone. `push` and `pull` take `--no-remove` too; since they write to one folder only, the file comes back to the other at its next sync.

## Reading the output

Every line about a file has a letter, then the file's path in its vault. When a command involves another vault, the line starts with an arrow: `->` for a file in your vault, `<-` for a file in the other one. `snapshot` looks only at your vault and prints no arrow, and nor does `cp`, which writes to one vault. `status` has its own marks, [above](#checking-before-you-sync).

| Letter | Meaning |
|---|---|
| `A` | added |
| `M` | modified |
| `D` | deleted; `D path (moved)` or `D path (dropped)` when `move` or `drop` removed it from this folder |
| `R` | renamed: `R old/path -> new/path` |
| `C` | a conflict copy was written |
| `U` | a conflict is open |
| `!` | needs a look; the reason follows in parentheses |

Lines are printed as things happen, so a long sync shows its progress.

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
- **Folders, or a remote vault.** Another vault is a folder reachable as a path on this machine (an internal disk, an external drive, a mounted network share) or a [remote vault](remotes.md) in cloud storage.
- **Not synced:** symbolic links, empty folders, and file permissions other than whether a file is executable.
- **Renames** are recognised by content, as git does, and the content is copied from the old file, not sent again. A file renamed and edited at once is shown as a delete plus a new file.
- **A vault inside a vault.** If you drag one vault into another, its files are synced as ordinary files of the outer vault, and syncit prints a warning. The two cannot be synced with each other.
- **One at a time.** Running two syncit commands on the same vault at once fails with "vault is in use".

## In scripts

Results go to standard output; warnings and errors go to standard error.

| Exit code | Meaning |
|---|---|
| 0 | done, nothing needs attention |
| 1 | failed: not a vault, vault in use, bad arguments, a refused `move` or `drop` |
| 2 | done, but something needs a look: a conflict, or a file that was skipped or is still pending |

Add `--json` to any command and every line, on both streams, is a JSON object with a `type`:

```sh
syncit --json push /Volumes/Backup/Documents
```

```
{"type":"event","kind":"updated","vault":"/Volumes/Backup/Documents","path":"notes.txt","note":""}
{"type":"summary","recorded":1,"added":0,"updated":1,"deleted":0,"renamed":0,"conflicts":0,"pending":0,"skipped":0,"incomplete":0,"rejected":0}
```

The types are `event`, `conflict`, `summary`, `setting`, `blame`, `version`, `state`, `remote` and `info` on standard output, and `warning` and `error` on standard error. A run that completes ends with one `summary`.

[← All guides](index.md) · Next: [Conflicts](conflicts.md)
