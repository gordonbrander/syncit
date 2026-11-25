---
title: Partial sync
---

[← All guides](index.md)

# Partial sync

Keep just part of a large archive on a small disk. Check out one folder, bring in more later, and free up space by sending files back, all without ever deleting anything from the archive.

## Check out part of an archive

Say a backup drive has a large archive and you only want one folder of it on your laptop. Clone that sub-folder:

```sh
syncit clone /Volumes/Backup/Archive/photos/2024 ~/Archive
```

You get `~/Archive/photos/2024/…` and nothing else. The folder structure is the same as in the archive, so paths always match. The vault *includes* that folder and *excludes* the rest. What is excluded is simply "not here"; syncing will never delete it from the archive. `get` includes more, and `move` or `drop` excludes again.

To check out several folders at once, give `--include` for each, with paths from the top of the archive:

```sh
syncit clone /Volumes/Backup/Archive ~/Archive --include photos/2024 --include music
```

To take everything except some folders, give `--exclude` for each. What is excluded is never downloaded:

```sh
syncit clone /Volumes/Backup/Archive ~/Archive --exclude video
```

The two combine, to take a folder without part of it. An excluded path must be inside an included one:

```sh
syncit clone /Volumes/Backup/Archive ~/Archive --include photos --exclude photos/raw
```

To see what you have and what you don't, use `syncit status --all --vs` (see [Checking before you sync](basics.md#checking-before-you-sync)).

## Get more

To bring in another folder (or a single file) later, point `get` at it in the archive:

```sh
cd ~/Archive
syncit get /Volumes/Backup/Archive/music
```

```
-> A music/m.mp3
recorded 0, added 1, updated 0, deleted 0, renamed 0, conflicts 0, pending 0, skipped 0
```

It lands at the same place here, `~/Archive/music`, and is synced from then on. `get` also brings in anything else that changed in the archive, as `pull` does.

If the archive is a [remote](remotes.md), or you'd rather give paths from the top of the archive, name it with `--from`:

```sh
syncit get music --from /Volumes/Backup/Archive
syncit get music --from origin
```

To get a folder without part of it, add `--exclude`, written the same way as the paths you get:

```sh
syncit get photos --from origin --exclude photos/raw
```

A plain `get photos` later brings the excluded part in too.

## Add your own

Create a new folder anywhere in the vault and it is synced from then on, like everything else. There is no extra step.

## Free up space

To send files to the archive and remove them from this folder, use `move`. Give the files or folders, then the vault to move them to:

```sh
syncit move music photos/2024/raw /Volumes/Backup/Archive
```

```
<- M music/m.mp3
-> R music/m.mp3
-> R photos/2024/raw/1.dng
recorded 1, added 0, updated 1, deleted 0, renamed 0, conflicts 0, pending 0, skipped 0
```

`move` sends your changes there as `push` does, including edits you haven't synced yet. It then checks that every file arrived, and only then removes them here (the `D … (moved)` lines). They stay in the archive, and later syncs neither bring them back nor delete them. `syncit get` brings them back.

> **Deleting is not moving.** If you delete a file with Finder or `rm`, that is a real deletion, and the next sync deletes it from the other folder too. `move` removes it *only here*.

If the other vault excludes these files, nothing is sent and nothing is removed:

```
error: nothing moved: /Volumes/Small excludes photos/2024/a.jpg
```

The same goes if the other vault has its own newer version of a file: nothing is removed. Sync with it, then move again.

## Remove without sending

If another vault already has the files, `drop` removes them here and sends nothing. Say which vault has them:

```sh
syncit drop --check /Volumes/Backup/Archive music
```

```
-> R music/m.mp3
recorded 0, added 0, updated 0, deleted 0, renamed 0, conflicts 0, pending 0, skipped 0
```

`drop` first records any unsaved changes, then removes a file only if its current version is in the vault you named. If something has not been synced there yet, nothing is dropped and the files are listed; sync and try again. Without `--check`, `drop` goes ahead only for files whose versions this vault keeps in its [history](history.md):

```
error: nothing dropped: no other copy is known of music/m.mp3. Name a vault that has them with --check <vault>, or use --force
```

`--force` skips the check, if you really mean it: that may remove the only copy. As with `move`, later syncs neither bring dropped files back nor delete them elsewhere, and `syncit get` brings them back. You can also drop a folder you don't have yet, so that it is never fetched.

To drop a folder but keep part of it, add `--keep`. The kept files are not touched, and keep syncing:

```sh
syncit drop --check /Volumes/Backup/Archive photos --keep photos/2024
```

## The rules in the config file

What a vault includes and excludes is two lists in its config, which `syncit config` prints:

```toml
include = ["."]
exclude = ["video", "photos/raw"]
```

`"."` is the whole vault. The longest matching path decides, so `include = ["photos"]` with `exclude = ["photos/raw"]` is "photos, without raw".

`clone`, `get`, `move` and `drop` write these lists for you, and move the files to match. You can also edit them in `.syncit/config.toml`. Editing changes what is fetched from then on, and never moves a file by itself:

- A path you add to `include` arrives with the next `pull` or `sync`.
- A path you add to `exclude` stays on disk and keeps syncing. `syncit status` lists it as `undropped`, and each sync reminds you, until you decide: `syncit drop <path>` removes it here, and `syncit get` keeps it.

If you delete an excluded folder by hand, it is removed from this vault only. Nothing is deleted anywhere else.

## Pushing to a partial vault

The other way round works too: when you `push` to a vault that has only part of the archive, it takes only what it includes, and tells you what it did not take:

```
warning: /Volumes/Small excludes 4 files (new.txt, notes.txt, photos/, todo.txt); they were not copied
```

[← All guides](index.md) · Next: [Remotes](remotes.md)
