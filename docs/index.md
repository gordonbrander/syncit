---
title: syncit
---

# syncit

syncit lets you keep folders in sync, in both directions.

- **Sync anything to anything**: Any folder can sync with any other. Sync is decentralized and many-to-many.
- **Partial sync**: Replicas can sync just part of a larger archive.
- **Backup and restore**: Optionally keep snapshots of old versions.

syncit works seamlessly with ordinary files and folder management. Create, edit, move and delete, then run `syncit sync` when you want the folders to agree.

## Get started

Install and quick start are in the [README](https://github.com/gordonbrander/syncit#readme). Then read the [guides](guide/index.md):

- [Basics](guide/basics.md): sync, push, pull, copy, status, ignoring files
- [Conflicts](guide/conflicts.md): what happens when two folders change the same file
- [Version history](guide/history.md): keep old versions and roll back
- [Partial sync](guide/partial-sync.md): check out just part of a large backup
- [Remotes](guide/remotes.md): sync through encrypted cloud storage such as Cloudflare R2 or S3

## How it works

For the design and the reasoning behind it, see [PRD.md](https://github.com/gordonbrander/syncit/blob/main/specs/PRD.md). The format of the sync log is in [CRDT.md](https://github.com/gordonbrander/syncit/blob/main/specs/CRDT.md), and the implementation specification is [SPEC.md](https://github.com/gordonbrander/syncit/blob/main/specs/SPEC.md). Remote vaults are specified in [store.md](https://github.com/gordonbrander/syncit/blob/main/specs/store.md).
