---
title: Version history
---

[← All guides](index.md)

# Version history

Each vault can keep old versions of your files, so you can undo a change, bring back a deleted file, or roll back to an earlier version.

Each vault keeps the content of old versions in `.syncit/blobs/`, as far back as its `keep-history` setting says. The setting is the vault's own and is never synced. See it with:

```sh
syncit config
```

```
keep-history: 1
autocompact: on
nickname: Documents
```

(`nickname` is explained under [Who changed what](#who-changed-what).)

## How much history to keep

A new vault keeps one version back: for every file, the current version and the one before it. That is enough to undo your last change, to bring back a file you just deleted, and to let syncit [merge text files](conflicts.md#text-files-merge-themselves).

```sh
syncit config keep-history 10     # ten versions back
syncit config keep-history all    # every version: a good choice for a large backup drive
syncit config keep-history 0      # none
```

History costs disk. With any setting above 0 the vault stores a second copy of every current file, since the version you are about to replace has to be saved before you replace it, plus the older versions you asked for. `keep-history 0` gives that space back: the vault then stores nothing but your files and a small log.

History is what has been collected: raise the number late and it grows from there.

## Dropping old versions

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

## Snapshots

A version is recorded whenever you sync. To record one without syncing, for instance while the backup drive is not attached:

```sh
syncit snapshot
```

Snapshots are delivered with the next sync, so your history is as fine-grained as you make it.

## Getting an old version back

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

Each stored version is also a plain file in `.syncit/blobs/`, named by the SHA-256 of its content, in a folder named by the hash's first two characters: `.syncit/blobs/ab/abcdef…`.

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

[← All guides](index.md) · Next: [Partial sync](partial-sync.md)
