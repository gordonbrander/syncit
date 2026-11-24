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

### Agent skill

This repository also ships a skill that teaches coding agents to use syncit.

```sh
# Claude Code (run inside a session)
/plugin install syncit --marketplace gordonbrander/syncit

# Pi
pi install git:github.com/gordonbrander/syncit

# Codex
codex plugin marketplace add gordonbrander/syncit
codex plugin add syncit@syncit
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

## Guides

Short guides to everything else are at **[gordonbrander.github.io/syncit](https://gordonbrander.github.io/syncit/guide/)**:

- [Basics](docs/guide/basics.md): sync, push, pull, copy, status, ignoring files, scripting
- [Conflicts](docs/guide/conflicts.md): what happens when two folders change the same file
- [Version history](docs/guide/history.md): keep old versions and roll back
- [Partial sync](docs/guide/partial-sync.md): check out just part of a large backup
- [Remotes](docs/guide/remotes.md): sync through encrypted cloud storage such as Cloudflare R2 or S3

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
