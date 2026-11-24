---
title: Conflicts
---

[← All guides](index.md)

# Conflicts

What happens when the same file changed in two folders since they last synced, and how to settle it. In short: syncit keeps both, and never loses data.

## In case of conflict, keep both

If the same file was edited in two folders since they last synced, syncit keeps both. The newer one keeps the name; the other is saved as a copy next to it:

```
<- M notes.txt
-> C notes-b842127a.txt
-> U notes.txt (conflict: notes-b842127a.txt)
recorded 2, added 0, updated 1, deleted 0, renamed 0, conflicts 1, pending 0, skipped 0
```

Both folders end up with the same two files. To settle it:

1. Open `notes.txt` and `notes-b842127a.txt` and decide what you want.
2. Save the result as `notes.txt`.
3. Run `syncit sync`. The conflict is closed in every folder.
4. Delete the `notes-…` copy when you no longer need it. It is an ordinary file, and deleting it deletes it everywhere on the next sync.

The `U … (conflict: …)` line is repeated on every sync until you edit the file. If you delete the copy before syncing in step 3, it may reappear once from the other folder; delete it again and it stays gone.

`syncit status` lists open conflicts with the `≠` mark and the name of the copy. See [Checking before you sync](basics.md#checking-before-you-sync).

## Text files merge themselves

Text files (`.md`, `.markdown`, `.txt`) can settle themselves. If the two edits touch different lines, and one of the folders keeps history (they do unless you turned it off), syncit merges them into one file:

```
-> M notes.md (merged)
```

There is no copy and no conflict. syncit merges only when it can keep both versions, so that nothing is lost. For `push` and `pull` that means history in the folder being written to; for `sync`, in either. Both edits stay in the file's [history](history.md) there, where `syncit log` and `syncit restore` can reach them. Edits to the same lines are never merged: you get the two files, as above.

## Edited in one place, deleted in another

The edit wins and the file is kept everywhere:

```
-> U todo.txt (conflict: deleted in another vault)
```

Delete it again if that is what you want, or edit it to keep it; either one clears the message.

[← All guides](index.md) · Next: [Version history](history.md)
