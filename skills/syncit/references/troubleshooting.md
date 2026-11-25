# Troubleshooting and scripting

## `!` lines (exit code 2)

A `!` line names a file that needs a look. The run otherwise finished.

| Line | Meaning | What to do |
|---|---|---|
| `(skipped: changed since it was scanned)` | The file was being edited during the run | Run the command again |
| `(skipped: an untracked file is in the way)` | A file here, never recorded, has the name of an incoming file | Ask the user which to keep; move the local one aside, or run `syncit snapshot` to record it, then sync |
| `(skipped: the conflict copy name is taken)` | A different file already has the name the conflict copy needs | Rename or move that file, then sync |
| `(pending: content not available from any source)` | The other vault knows of a version but does not hold its content (often a partial vault relaying) | Sync with a vault that has the file |
| `(incomplete: waiting for missing history)` | An earlier transfer was interrupted | Run `syncit pull <vault>` again |
| `(rejected: …)` | Data from the other vault failed a check | Report it to the user verbatim; do not retry in a loop |

## Errors (exit code 1)

- **`not in a vault: <dir>`**: `cd` into the vault, or the folder needs `syncit init`. Check with the user before running `init` in a folder they did not name.
- **`vault is in use: <dir>`**: another syncit command is running on this vault or the other one. Wait for it. The lock clears when that process ends.
- **`<dir> is already a vault`**: nothing to do; skip `init`.
- **`cannot sync a vault with itself or with a vault inside it`**: the path given is inside this vault. Name the other vault.
- **`nothing moved: …` / `nothing dropped: …`**: the command refused and removed nothing. The message says which files and why. See [partial.md](partial.md).
- **`no key for <url>`**: see [remotes.md](remotes.md).
- **`<vault> is at format version N; this syncit uses M. Run: syncit migrate <vault>`**: the other vault was made by an older syncit. Migrating it stops older syncits from using it, so ask the user first: every machine that uses that vault needs the new syncit. Then run the command in the message and repeat the one that failed. The vault a command runs in needs none of this; it is updated by the command.
- **`… this syncit reads up to M: upgrade syncit`**: the vault was made or migrated by a newer syncit. Nothing here can read it until syncit is updated on this machine.
- **`the content of that version is not in this vault`**: see [history.md](history.md).
- **A nickname stopped working**: the folder it pointed to was moved or renamed. `syncit remote remove <name>`, then `syncit remote add <new path>`. Vault folders can be moved freely; only remembered nicknames go stale.

## Deletions the user did not make

syncit cannot tell a deleted folder from one that is not there right now. If a sub-folder inside the vault is its own mount (an external drive, a network share) and it is not mounted, its files read as deleted and the next sync deletes them from the other vault.

- A dry run with many unexpected `D` lines under one folder is the sign. Stop, and check that the folder is present.
- If the whole vault is on an unmounted drive, commands simply fail with "not in a vault"; that case is safe.
- If the deletions already synced, see "Roll back a folder" in [history.md](history.md).
- `--no-remove` puts deleted files back instead of deleting them, permanently. It is the right tool when the user says "sync, but never delete anything this time". Ask first.

## Things syncit does not sync

Symbolic links, empty folders, and file permissions other than the executable bit. A renamed file is a delete plus a new file. A vault placed inside another vault is synced as ordinary files of the outer one, with a warning.

## Scripts and scheduled backups

- Results go to stdout; warnings and errors go to stderr.
- `--json` makes every line a JSON object with a `type`: `event`, `conflict`, `summary`, `version`, `state`, `config`, `blame`, `remote`, `info` on stdout, `warning` and `error` on stderr. A run that completes ends with one `summary`. Prefer `--json` when you need to parse output, for example `syncit --json log` or `syncit --json status`.
- Exit codes: `0` done, `1` failed, `2` done but something needs a look. A script should treat `2` as "tell someone", not as failure to retry.
- There is no built-in schedule. A scheduled backup is `push` from cron or launchd:

  ```sh
  cd ~/Documents && syncit push /Volumes/Backup/Documents
  ```

- For a remote from cron, the seed phrase comes from the keychain if the job can reach it. Otherwise the user sets `SYNCIT_SECRET_KEY` in the job's environment, or uses `--save-key`. Do not write the seed phrase into a script or crontab yourself; have the user put it where the job reads it.
- Two runs on the same vault at once fail with "vault is in use", so leave enough time between scheduled runs.
- It is safe to interrupt a run. Run it again and it finishes.
