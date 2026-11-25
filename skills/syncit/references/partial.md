# Partial vaults

A partial vault keeps only some folders of a larger one. Use it when the archive is bigger than the disk, or the user wants only part of it on this machine. Paths always match the archive's: `photos/2024` in the archive is `photos/2024` here.

## Make one

```sh
# several folders: the source must be the vault's root
syncit clone /Volumes/Backup/Archive ~/Archive --include photos/2024 --include music

# everything except some folders: they are never downloaded
syncit clone /Volumes/Backup/Archive ~/Archive --exclude video

# a folder without part of it: the excluded path must be inside an included one
syncit clone /Volumes/Backup/Archive ~/Archive --include photos --exclude photos/raw

# one folder: name it directly
syncit clone /Volumes/Backup/Archive/photos/2024 ~/Archive
```

The second form still creates `~/Archive/photos/2024/…`.

## See what is here and what is not

```sh
syncit status --all --vs /Volumes/Backup/Archive
```

`-` (excluded) means "not kept here by choice". It is not a deletion and nothing is lost.

`-` (undropped) means the path is excluded but its files are still here, after an interrupted `move` or `drop` or a hand-edited config. They keep syncing, and each run warns about them. Ask the user which they want: `syncit drop <path>` (dry-run first) removes them here, `syncit get <path>` keeps them.

## Get more

```sh
syncit get /Volumes/Backup/Archive/music   # a path into the other vault
syncit get music --from origin             # a path from the root of a named vault or URL
```

To get a folder without part of it, add `--exclude`, written the same way as the paths:

```sh
syncit get photos --from origin --exclude photos/raw
```

`--exclude` removes nothing: files already here under it show as undropped. A plain `get photos` later brings the excluded part in too, so repeat the `--exclude` if it should stay out.

`get` also pulls everything else that changed in the other vault, as `pull` does. It has no `--dry-run`; run `syncit pull --dry-run <vault>` first if the user wants a preview of the rest.

A new folder the user creates inside a partial vault syncs like any other. No extra step.

## Remove files from this vault only

Three things look alike and are not:

| Action | Sent to the other vault first? | Removed here | Effect on other vaults |
|---|---|---|---|
| `syncit move <paths> <vault>` | Yes, and checked | Yes | None. The files stay there |
| `syncit drop <paths>` | No | Yes | None |
| `rm`, Finder, any delete | — | Yes | **Deleted everywhere** on the next sync |

**Use `move` by default.** It is the safe way to free space:

```sh
syncit move --dry-run music photos/2024/raw /Volumes/Backup/Archive
syncit move music photos/2024/raw /Volumes/Backup/Archive
```

It refuses, and removes nothing, when the other vault excludes or ignores any of the files, or has its own newer version of one. In the second case sync with that vault, then move again. If a move is interrupted, run it again.

**Use `drop` only when the files are already in another vault, or are not wanted at all.** Push first, then dry-run with `--check`, and go on only if the dry run exits 0:

```sh
syncit push /Volumes/Backup/Archive
syncit drop --dry-run --check /Volumes/Backup/Archive music
syncit drop --check /Volumes/Backup/Archive music
```

- `drop` sends nothing to the other vault, with or without `--check`.
- **A dry-run refusal is a stop.** The real `drop` can go ahead where the dry run refused, even with `--check`: it counts a copy in this vault's own history as enough. A file changed since the last sync then survives only in that history, which compaction later discards. If the dry run says `nothing dropped`, do what its message says (usually: sync or push first) and dry-run again. Never run the real command to "see if it works".
- `drop --force` skips every check and may remove the only copy. Ask the user first, and say that in the question.
- Dropping a folder that is not here yet means "never fetch this".
- `--keep <path>` keeps part of a dropped folder: `syncit drop --check <vault> photos --keep photos/2024`. The kept files are not touched or re-downloaded. Prefer it to `drop` followed by `get`.

`syncit get` brings moved or dropped paths back.

## Read or edit the rules

`syncit config` prints the vault's `include` and `exclude` lists, as they are in `.syncit/config.toml`. `"."` is the whole vault, and the longest matching path decides.

The user may edit that file by hand. An edit moves no files:

- A path added to `include` arrives with the next `pull` or `sync`.
- A path added to `exclude` whose files are here shows as undropped (above). Nothing is removed until `syncit drop`.
- An excluded file deleted by hand is removed here only; no deletion is sent.

A misspelt key or a bad value stops every command with an error naming it: fix the file, do not delete it.

## Pushing to a partial vault

A partial vault takes only what it keeps. `push` warns about the rest:

```
warning: /Volumes/Small excludes 4 files (new.txt, notes.txt, photos/, todo.txt); they were not copied
```

This is not an error. Tell the user those files are not backed up in that vault.
