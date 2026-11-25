---
name: syncit
description: Drive the syncit CLI to sync folders for the user - two-way sync, push and pull backups, previewing with --dry-run, resolving conflicts, restoring old versions of files, partial copies of a large archive, and encrypted remote vaults in object storage. Use when the user asks to sync, back up, replicate or restore files with syncit, or is working in a folder that contains .syncit/.
---

# syncit

syncit syncs folders with each other. This skill covers how to run it for a user without losing their files.

## How it works

- A **vault** is a folder that contains `.syncit/`. Commands act on the vault the current directory is in, so `cd` into it first.
- You always name the **other vault**: a path to any folder inside it, a URL of a remote vault (`s3://bucket/notes`), or a nickname listed by `syncit remote`.
- Nothing runs in the background. Files change only when a command runs.
- A run first records what changed here, then exchanges changes with the other vault. Deletions are changes: a file deleted in one vault is deleted in the other.

## Look up commands, don't guess

The CLI describes itself. Before using a command for the first time in a session, read its help:

```sh
syncit --help            # commands, vault forms, output letters, exit codes, files
syncit <command> --help  # arguments, what it changes, when it refuses
```

If `syncit --version` fails, syncit is not installed: point the user to the install steps in the project README.

## Rules

1. **Preview before anything that removes files.** `sync`, `push`, `pull`, `move`, `cp`, `drop` and `compact` take `--dry-run`, which prints exactly what the real run would print and changes nothing.
   - Always dry-run `move`, `drop` and `compact` first.
   - Before `sync`, `push` or `pull`, run `syncit status`. It is instant and changes nothing. If it lists any `✗ removed` file, dry-run first.
   - Also dry-run when: these two vaults have never exchanged files (two folders that were each `init`ed; a `clone` already counts as the first exchange); the user asked what will happen; a drive or sub-folder might not be mounted; or the vaults have not synced in a long time.
   - Otherwise a sync the user asked for needs no dry run.
   - Show the user the dry-run output. If it has `D` (deleted, or removed here by `move`/`drop`) lines the user did not ask for, stop and ask before the real run.
2. **Never use `rm` to free space.** Deleting a file is a real deletion that reaches every vault on the next sync. To remove files from this vault only, use `syncit move` (see [references/partial.md](references/partial.md)).
3. **Ask the user before** `drop --force`, `compact`, lowering `keep-history`, or `--no-remove`. The first three discard data for good. `--no-remove` puts deleted files back for good.
4. **Read the exit code.** `0` is done. `1` is failed. `2` means the run finished but something needs a look: a conflict, or a file that was skipped or is pending. Tell the user what the `U` and `!` lines say; do not report a `2` as a clean sync.
5. **One command at a time per vault.** A second one fails with "vault is in use". Never edit anything inside `.syncit/`.
6. **Remote seed phrases stay out of the conversation.** See "Remote vaults" below before running `init`, `clone` or `remote add` with a URL.

## Journeys

### Set up two folders to sync

```sh
cd ~/Documents && syncit init                        # make it a vault
syncit clone ~/Documents /Volumes/Backup/Documents   # make the second copy
```

`clone` needs an empty or missing destination. If both folders already have files, `init` each one and `sync` them instead; dry-run that first sync.

The clone remembers the original by nickname (`syncit remote` lists it). The original does not know the clone: from there, name the clone by path, or run `syncit remote add <path>` once.

### Sync, push, pull

```sh
cd ~/Documents
syncit sync /Volumes/Backup/Documents   # both ways
syncit push /Volumes/Backup/Documents   # send only: back up, change nothing here
syncit pull /Volumes/Backup/Documents   # take only: update here, change nothing there
```

Pick `push` when the user says "back up", `pull` when the other folder must not change, `sync` otherwise.

### See what would change

```sh
syncit status                      # what changed here since the last run
syncit status --vs <vault>         # each file's state here and there
syncit sync --dry-run <vault>      # exactly what a sync would do
```

`status` prints nothing when nothing needs attention. `syncit status --help` explains the marks.

### Resolve a conflict

When both vaults changed the same file, the newer version keeps the name and the other is saved beside it, in both vaults, for example `notes-b842127a.txt`. The run prints `U notes.txt (conflict: notes-b842127a.txt)` and exits 2 on every run until the conflict is closed.

1. Show the user both files (a diff helps) and ask which result they want. Do not choose for them.
2. Write that result to the original name (`notes.txt`). The file must change: syncing again without editing it leaves the conflict open.
3. Sync again. The conflict closes in every vault.
4. Delete the `notes-…` copy, then sync once more so the deletion reaches the other vault.

`U todo.txt (conflict: deleted in another vault)` means one side edited and the other deleted; the edit was kept. Ask the user, then either edit the file or delete it again, and sync.

Text files (`.md`, `.txt`) whose edits touch different lines merge on their own and print `M notes.md (merged)`: nothing to do.

### Roll back a file

```sh
syncit log notes.txt                        # versions, newest first
syncit restore notes.txt 3f9a               # write that version as a copy beside the file
syncit restore --in-place notes.txt 3f9a    # replace the file with that version
```

- The hash is the first column of `log`; a few characters are enough.
- **Which version undoes the last change?** Run `syncit status <file>` first. If it shows `✎ modified`, the last change is not recorded yet and the `(current)` line in `log` is the version before it: restore that one. If `status` shows nothing, restore the line below `(current)`.
- Default to a copy, and show it to the user. Use `--in-place` when the user asked to revert the file. `--in-place` records the current file first, so it can be undone the same way.
- A restored copy is an ordinary file. Delete it once the user is done with it, or the next run syncs it to the other vault.
- To bring back a deleted file, restore `--in-place` the version listed just below the `--------` line.
- A version marked `(no content)` cannot be restored from this vault. New vaults keep only one version back (`syncit config` shows `keep-history`). Tell the user plainly when the version they want is gone; another vault with more history may still have it (see [references/history.md](references/history.md)).
- `restore` takes one file. To roll back a folder or undo a bad sync, see [references/history.md](references/history.md).

### Keep only part of a large archive

```sh
syncit clone /Volumes/Backup/Archive ~/Archive --include photos/2024 --include music
cd ~/Archive
syncit status --all --vs /Volumes/Backup/Archive   # what is here, what is only there
syncit get /Volumes/Backup/Archive/video           # bring in another folder
syncit move music /Volumes/Backup/Archive          # send to the archive, then remove here
```

Folders left out are "not here", not deleted: syncing never removes them from the archive. More in [references/partial.md](references/partial.md).

### Add files from outside a vault

`syncit cp <files…> <folder in a vault>` copies files in. It deletes nothing, but it replaces files of the same name: dry-run it and look for `M` lines. A folder is copied as a folder; see `syncit cp --help` for copying only its contents.

### Ignore files

List names or paths, one per line, in `.syncitignore` at the vault root. The file itself syncs to other vaults.

- A bare name (`build`) matches a file or folder of that name at any depth. A line with a `/` (`projects/site/build`) is a path from the vault root.
- There are no wildcards. A line like `*.log` matches nothing and gives no warning, so list each name, and tell the user that new names will need adding.
- Ignoring a file that is already synced deletes it nowhere; it only stops being updated.
- Check the result with `syncit status --all --vs <vault>`: ignored files show `·`.

## Remote vaults

A remote vault lives in object storage (S3, Cloudflare R2 and others), encrypted with a key that is shown as a 24-word seed phrase. Once it is set up, use its nickname like any other vault: `syncit sync origin`.

Setting one up involves the seed phrase, so read [references/remotes.md](references/remotes.md) first. In short: `syncit init <url>` prints the seed phrase, and `syncit clone <url>` and `syncit remote add <url>` ask for it at a terminal. Have the user run those three commands themselves, so the seed phrase never appears in this conversation.

## More

- [references/partial.md](references/partial.md): partial vaults in depth; `move` vs `drop` vs deleting.
- [references/history.md](references/history.md): how much history is kept, freeing space, rolling back a folder, undoing a bad sync.
- [references/remotes.md](references/remotes.md): setting up a remote (Cloudflare R2 example), credentials, the seed phrase, a second computer.
- [references/troubleshooting.md](references/troubleshooting.md): `!` lines, errors, moved folders, scripts and scheduled backups.
