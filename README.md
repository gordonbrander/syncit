# syncit

syncit lets you keep folders in sync, in both directions.

- **Sync anything to anything**: Any folder can sync with any other. Sync is decentralized and many-to-many.
- **Partial sync**: Replicas can sync just part of a larger archive.
- **Backup and restore**: Optionally keep snapshots of old versions.

syncit works seamlessly with ordinary files and folder management. Create, edit, move and delete, then run `syncit sync` when you want the folders to agree.

## Install

You need [Rust](https://rustup.rs) 1.89 or later. syncit runs on macOS and Linux.

```sh
git clone <this repository> syncit
cd syncit
cargo install --path .
```

This builds everything, remote vaults included; the first build takes a few minutes. For a smaller tool that syncs local folders only, add `--no-default-features`. On a machine with no keychain, use `--no-default-features --features remote`.

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

If the other folder has only [part of a larger archive](#working-with-part-of-a-larger-backup), `push` tells you what it did not take:

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
recorded 0, added 2, updated 1, deleted 0, conflicts 0, pending 0, skipped 0
```

The last argument is a folder in the vault, which need not exist yet. Each file or folder you name lands in it under its own name, as with `cp -R`. Name `.` to copy the contents of the folder you are in.

A file the vault lacks is added and one that differs is replaced. Files that are already the same are left alone, and nothing is deleted. What the vault [ignores](#ignoring-files) is not copied, nor are symbolic links.

To see what it would change in the vault first, add `--dry-run`:

```sh
syncit cp --dry-run . /Volumes/Backup/Archive
```

`cp` only copies. The files are then ordinary changes in that vault, as if you had dragged them in, and its next `sync` or `snapshot` records them. Before replacing anything, `cp` records the vault as it is, so a replaced version can be restored from [history](#version-history).

## Working with part of a larger backup

Say a backup drive has a large archive and you only want one folder of it on your laptop. Clone that sub-folder:

```sh
syncit clone /Volumes/Backup/Archive/photos/2024 ~/Archive
```

You get `~/Archive/photos/2024/…` and nothing else. The folder structure is the same as in the archive, so paths always match. The vault *includes* that folder and *excludes* the rest. What is excluded is simply "not here"; syncing will never delete it from the archive. `get` includes more, and `move` or `drop` excludes again.

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

**Remove without sending.** If another vault already has the files, `drop` removes them here and sends nothing. Say which vault has them:

```sh
syncit drop --check /Volumes/Backup/Archive music
```

```
-> R music/m.mp3
recorded 0, added 0, updated 0, deleted 0, conflicts 0, pending 0, skipped 0
```

`drop` first records any unsaved changes, then removes a file only if its current version is in the vault you named. If something has not been synced there yet, nothing is dropped and the files are listed; sync and try again. Without `--check`, `drop` goes ahead only for files whose versions this vault keeps in its [history](#version-history):

```
error: nothing dropped: no other copy is known of music/m.mp3. Name a vault that has them with --check <vault>, or use --force
```

`--force` skips the check, if you really mean it: that may remove the only copy. As with `move`, later syncs neither bring dropped files back nor delete them elsewhere, and `syncit get` brings them back. You can also drop a folder you don't have yet, so that it is never fetched.

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

Text files (`.md`, `.markdown`, `.txt`) can settle themselves. If the two edits touch different lines, and one of the folders keeps history (they do unless you turned it off), syncit merges them into one file:

```
-> M notes.md (merged)
```

There is no copy and no conflict. syncit merges only when it can keep both versions, so that nothing is lost. For `push` and `pull` that means history in the folder being written to; for `sync`, in either. Both edits stay in the file's history there, where `syncit log` and `syncit restore` can reach them. Edits to the same lines are never merged: you get the two files, as above.

The `U … (conflict: …)` line is repeated on every sync until you edit the file. If you delete the copy before syncing in step 3, it may reappear once from the other folder; delete it again and it stays gone.

**Edited in one place, deleted in another.** The edit wins and the file is kept everywhere:

```
-> U todo.txt (conflict: deleted in another vault)
```

Delete it again if that is what you want, or edit it to keep it; either one clears the message.

## Version history

Each vault keeps the content of old versions in `.syncit/blobs/`, as far back as its `keep-history` setting says. The setting is the vault's own and is never synced. See it with:

```sh
syncit config
```

```
keep-history: 1
autocompact: on
nickname: Documents
```

(`nickname` is explained under "Who changed what".)

### How much history to keep

A new vault keeps one version back: for every file, the current version and the one before it. That is enough to undo your last change, to bring back a file you just deleted, and to let syncit [merge text files](#in-case-of-conflict-keep-both).

```sh
syncit config keep-history 10     # ten versions back
syncit config keep-history all    # every version: a good choice for a large backup drive
syncit config keep-history 0      # none
```

History costs disk. With any setting above 0 the vault stores a second copy of every current file, since the version you are about to replace has to be saved before you replace it, plus the older versions you asked for. `keep-history 0` gives that space back: the vault then stores nothing but your files and a small log.

History is what has been collected: raise the number late and it grows from there.

### Dropping old versions

syncit drops the versions older than `keep-history` reaches at the end of every `sync`, `push`, `pull` and `snapshot`. Only content is dropped. The record of every version stays: `syncit log` still lists it, and other vaults sync with this one exactly as before.

If you would rather nothing were deleted until you say so:

```sh
syncit config autocompact off
syncit compact --dry-run    # what would go
syncit compact
```

```
dropped 12 versions (3.4 MB)
```

Lowering `keep-history` deletes nothing at that moment; `syncit config` tells you how many stored versions the new setting no longer covers and what will drop them. The current version of a file is never dropped, whatever the setting.

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

Each line starts with the first characters of the version's hash; times are UTC. `(no content)` marks a version this vault did not keep, because it is older than the vault's `keep-history` reaches, or never passed through here. `(executable)` marks a version in which the file could be run.

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

syncit records the file as it is first, so the version you replace can itself be restored later, unless `keep-history` is 0; then only another vault may still have it.

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
| `✓` | current | File up to date |
| `✎` | modified | Changed here, not yet snapshotted |
| `+` | new | New here, not yet snapshotted |
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

`push`, `pull`, `move`, `drop` and `cp` take `--dry-run` too. For `move` and `drop` it shows which files would be removed here, or the error if they would refuse.

To sync without removing anything, add `--no-remove`. A file deleted in one folder is kept in the other and put back where it was deleted. Both are adds:

```sh
syncit sync --no-remove /Volumes/Backup/Documents
```

```
<- A old.txt (kept)
-> A old.txt
recorded 2, added 2, updated 0, deleted 0, conflicts 0, pending 0, skipped 0
```

The deletion is undone, not postponed: later syncs leave the file alone. `push` and `pull` take `--no-remove` too; since they write to one folder only, the file comes back to the other at its next sync.

## Reading the output

Every line about a file has a letter, then the file's path in its vault. When a command involves another vault, the line starts with an arrow: `->` for a file in your vault, `<-` for a file in the other one. `snapshot` looks only at your vault and prints no arrow, and nor does `cp`, which writes to one vault. `status` has its own marks, [above](#checking-before-you-sync).

| Letter | Meaning |
|---|---|
| `A` | added |
| `M` | modified |
| `D` | deleted |
| `C` | a conflict copy was written |
| `R` | removed from this folder by `move` or `drop` |
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

## Syncing through a remote vault

A vault can live in cloud storage: an S3 bucket, Google Cloud Storage, Azure, Cloudflare R2, or a plain folder by `file://` URL. Such a *remote vault* has no working folder of its own. It holds the files and their history, encrypted, and any vault can sync with it. Two computers that never see each other stay in step by both syncing with the same remote.

Make one in an empty place:

```sh
syncit init s3://my-bucket/notes
```

```
Vault initialized: s3://my-bucket/notes
Nickname: origin
Secret key for s3://my-bucket/notes:

  word1 word2 … word24

Write these words down and keep them safe. They are the only way to
read this vault from another computer, and they are not shown again.
```

**The 24 words are the key.** Everything in the remote is encrypted with it: file contents, names, and the record of changes. The storage provider sees only how many objects there are and how big. If every copy of the words is lost, the remote cannot be read by anyone, including you.

Then, in a vault, remember the remote under its nickname and sync with it like any other vault:

```sh
cd ~/Documents
syncit remote add s3://my-bucket/notes
syncit sync origin
```

On another computer, make a vault from it, all of it or part:

```sh
syncit clone s3://my-bucket/notes ~/Documents
syncit clone s3://my-bucket/notes ~/Photos --only photos/2024 --only music
syncit get music --from origin     # later, add more
```

`sync`, `push`, `pull`, `move`, `status --vs` and `drop --check` all take a remote by nickname or URL. A remote keeps every version by default; change that, or compact it, with `--vault`:

```sh
syncit config --vault origin keep-history 5
syncit compact --vault origin
```

`syncit remote` lists the remotes this vault knows, and `syncit remote remove origin` forgets one (it touches nothing in the remote). A folder vault can be remembered too: `syncit remote add /Volumes/Backup/Documents`. To reach a folder that has the same name as a remote, write it as a path: `./origin`.

**Where the key is kept.** `remote add` and `clone` ask for the words once, then keep them in the system keychain (macOS Keychain, or the Secret Service on Linux). After you reinstall or rebuild syncit, macOS asks once whether it may read the keychain item again. Where there is no keychain, such as a server over SSH, you can save the key in `~/.config/syncit/secrets.toml` (`--save-key`, or answer yes when asked); it is then stored there unencrypted, readable by your user only, and every vault on the machine finds it. Or set `SYNCIT_SECRET_KEY` to the 24 words for each command, which is what scripts and cron jobs usually do.

**Credentials** for the storage are the usual settings: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_DEFAULT_REGION`, `AWS_ENDPOINT_URL` (for R2, MinIO and other S3-compatible services) and `AWS_ALLOW_HTTP`, and the `GOOGLE_*` and `AZURE_*` equivalents. Put them in `~/.config/syncit/credentials.toml` (or `$XDG_CONFIG_HOME/syncit/credentials.toml`), one `[[remotes]]` entry for each bucket or remote, and every command finds them. Names may be written in lower case, as in `~/.aws/credentials`. For a Cloudflare R2 bucket and an S3 bucket on another account:

```toml
[[remotes]]                          # personal, on R2
url = "s3://my-bucket"
aws_access_key_id = "…"
aws_secret_access_key = "…"
aws_endpoint_url = "https://<account-id>.r2.cloudflarestorage.com"
aws_default_region = "auto"

[[remotes]]                          # work
url = "s3://work-backups"
aws_access_key_id = "…"
aws_secret_access_key = "…"
aws_default_region = "us-east-1"
```

A `url` is a prefix: `s3://my-bucket` covers every remote in that bucket, such as `s3://my-bucket/notes`, and an entry with a longer `url` overrides it for the remotes under it. Environment variables override the file. It is plain text, so make it readable by your user only (`chmod 600`). Syncit never writes it. `~/.aws/credentials` is not read.

**Interrupted uploads.** Files of 16 MB or more are uploaded in parts. If an upload is interrupted, the parts already sent stay in the bucket, unseen, until the bucket removes them. Give the bucket a lifecycle rule that aborts incomplete multipart uploads after a few days.

## Good to know

- **Safe to interrupt.** You can stop syncit at any point, or unplug the drive. Run it again and it finishes the job.
- **It won't overwrite work in progress.** If a file changes while a sync is running, that file is skipped and reported, and handled on the next run.
- **Folders, or a remote vault.** Another vault is a folder reachable as a path on this machine (an internal disk, an external drive, a mounted network share) or a [remote vault](#syncing-through-a-remote-vault) in cloud storage.
- **Not synced:** symbolic links, empty folders, and file permissions other than whether a file is executable.
- **Renames** are seen as a delete plus a new file, so a renamed file is copied again.
- **A vault inside a vault.** If you drag one vault into another, its files are synced as ordinary files of the outer vault, and syncit prints a warning. The two cannot be synced with each other.
- **One at a time.** Running two syncit commands on the same vault at once fails with "vault is in use".

### In scripts

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
{"type":"summary","recorded":1,"added":0,"updated":1,"deleted":0,"conflicts":0,"pending":0,"skipped":0,"incomplete":0,"rejected":0}
```

The types are `event`, `conflict`, `summary`, `setting`, `blame`, `version`, `state`, `remote` and `info` on standard output, and `warning` and `error` on standard error. A run that completes ends with one `summary`.

## Commands

| Command | What it does |
|---|---|
| `syncit init [dir]` | make a folder a vault |
| `syncit init <url> [--nickname <name>]` | make a remote vault in an empty place in cloud storage |
| `syncit clone <vault, remote or sub-folder> <dir>` | make a new vault from another, or from part of one |
| `syncit clone <vault> <dir> --only <path>...` | make a new vault with only these paths of a vault or remote |
| `syncit sync <vault>` | sync both ways with another vault; `--dry-run` only shows what would happen; `--no-remove` keeps deleted files |
| `syncit push <vault>` | send changes only; takes `--dry-run` and `--no-remove` |
| `syncit pull <vault>` | receive changes only; takes `--dry-run` and `--no-remove` |
| `syncit status [path]... [--vs <vault>]` | show the files that need something; `--all` lists every file; `--vs` compares with another vault |
| `syncit snapshot` | record the current state without syncing |
| `syncit get <path in another vault>...` | add those files to this vault, and fetch them |
| `syncit get <path>... --from <vault>` | the same, with paths from the root of a vault or remote |
| `syncit move <path>... <vault>` | send these files to another vault, then remove them here; takes `--dry-run` |
| `syncit cp <path>... <folder in a vault>` | copy files from anywhere into a vault; takes `--dry-run` |
| `syncit drop [--check <vault>] [--force] <path>...` | remove these files here only, sending nothing; takes `--dry-run` |
| `syncit blame [path]...` | show which vault recorded the current version of each file |
| `syncit log [path]...` | list the recorded versions of each file, newest first |
| `syncit restore <path> <hash>` | bring back an old version as a copy beside the file; `--in-place` replaces the file |
| `syncit config` | show this vault's settings |
| `syncit config keep-history <n>\|all` | how many old versions of each file to keep inside the vault |
| `syncit config autocompact on\|off` | drop older versions after every sync, or only on `syncit compact` |
| `syncit compact [--dry-run]` | drop the old versions `keep-history` no longer covers |
| `syncit config nickname <name>` | name this vault in the changes it records |
| `syncit config --vault <vault> ...`, `syncit compact --vault <vault>` | act on another vault, such as a remote |
| `syncit remote` | list the vaults this one remembers by nickname |
| `syncit remote add [--nickname <name>] <url or folder>` | remember a vault; for a remote, find or ask for its key |
| `syncit remote remove <nickname>` | forget a remote |

A `<vault>` is the path of any folder in the other vault, a remote vault's URL, or a nickname from `syncit remote`.

## How it works

For the design and the reasoning behind it, see [PRD.md](specs/PRD.md). The format of the sync log is in [CRDT.md](specs/CRDT.md), and the implementation specification is [SPEC.md](specs/SPEC.md). Remote vaults are specified in [store.md](specs/store.md).
