# Remote vaults

A remote vault lives in object storage: an S3 bucket, Cloudflare R2, Google Cloud Storage, Azure, or a folder by `file://` URL. It has no working folder. It holds files and history, encrypted, and any vault can sync with it. Two computers that never see each other stay in step by syncing with the same remote.

## The seed phrase: read this first

Everything in a remote is encrypted with a 32-byte key, shown as a **24-word seed phrase**. If every copy of the seed phrase is lost, nobody can read the remote, including the user. There is no recovery and no key rotation.

Three commands touch the seed phrase directly:

| Command | What happens |
|---|---|
| `syncit init <url>` | Creates the remote and **prints the seed phrase once** |
| `syncit clone <url> <dir>` | Asks for the seed phrase at a terminal |
| `syncit remote add <url>` | Asks for the seed phrase at a terminal, unless this machine already has it stored (for example, `init` was run here) |

**Have the user run these themselves, in their own terminal,** whenever the seed phrase will be printed or asked for. Give them the exact command. Reasons: you cannot answer the prompt (without a terminal the command fails), and if you run `init <url>` the seed phrase lands in this conversation.

Rules for the seed phrase:

- Never ask the user to paste the seed phrase into the conversation, and never repeat it, log it, or write it to a file.
- After `init <url>`, remind the user to store the seed phrase somewhere other than this computer (a password manager, or paper). It is not shown again.
- After the user has run one of the three commands, the seed phrase is stored in the system keychain (macOS Keychain, or Secret Service on Linux), and every other command works without asking for it: you can run `sync`, `push`, `pull`, `status --vs`, `get --from`, `move` yourself.
- On a machine with no keychain (a server over SSH), the user chooses between `--save-key`, which stores the seed phrase unencrypted in `~/.config/syncit/secrets.toml`, and setting `SYNCIT_SECRET_KEY` for each command. That is their decision; explain both.
- `no key for <url>: set SYNCIT_SECRET_KEY, or run syncit remote add <url>` means this machine has no seed phrase stored for that remote yet. Ask the user to run `syncit remote add <url>`.
- On macOS, after syncit is reinstalled or rebuilt, a system dialog asks once whether it may read the keychain item. Only the user can answer it; a command that seems to hang on a remote is probably waiting for it.

## Set up a remote on Cloudflare R2

Other S3-compatible services work the same way with their own endpoint and keys.

**1. The user makes a bucket** in the Cloudflare dashboard, under R2 Object Storage:

- Create a bucket, for example `my-bucket`.
- Manage API tokens → Create API token, with *Object Read & Write* permission scoped to that bucket. Note the Access Key ID and Secret Access Key; the secret is shown once.
- Note the account ID. The endpoint is `https://<account-id>.r2.cloudflarestorage.com`.
- In the bucket's settings, make sure a lifecycle rule aborts incomplete multipart uploads after a few days. Interrupted uploads of large files otherwise leave parts in the bucket.

syncit never creates buckets.

**2. Credentials** go in `~/.config/syncit/credentials.toml` (or `$XDG_CONFIG_HOME/syncit/credentials.toml`):

```toml
[[remotes]]
url = "s3://my-bucket"
aws_access_key_id = "…"
aws_secret_access_key = "…"
aws_endpoint_url = "https://<account-id>.r2.cloudflarestorage.com"
aws_default_region = "auto"
```

```sh
chmod 600 ~/.config/syncit/credentials.toml
```

- Have the user fill in the two secrets themselves; you can create the file with placeholders.
- `url` is a prefix: `s3://my-bucket` covers `s3://my-bucket/notes` and every other remote in the bucket. A longer `url` overrides a shorter one.
- For AWS S3, drop `aws_endpoint_url` and set the real region.
- Environment variables with the same names in upper case (`AWS_ACCESS_KEY_ID`, …) override the file. `~/.aws/credentials` is not read.
- Never commit this file or `secrets.toml`.

**3. The user creates the remote** (prints the seed phrase):

```sh
syncit init s3://my-bucket/notes
```

The location must be empty. It can be run from any directory, and it does not connect any vault to the remote; step 4 does that. The remote's nickname is `origin` unless `--nickname` is given.

**4. Connect a vault and sync.** On the machine that ran `init`, the seed phrase is already stored, so `remote add` does not ask for it and you can run:

```sh
cd ~/Documents
syncit remote add s3://my-bucket/notes
syncit sync --dry-run origin
syncit sync origin
```

If `remote add` fails with `no key for <url>`, the seed phrase was not stored on this machine: ask the user to run that `remote add` themselves.

A run with a remote also prints `sent N files, M segments to <url>` and `received …`.

## A second computer, or a replacement for a lost one

The user needs the credentials (step 2) and the seed phrase. Then they run one of:

```sh
syncit clone s3://my-bucket/notes ~/Documents
syncit clone s3://my-bucket/notes ~/Photos --include photos/2024 --include music
```

Without the seed phrase the remote cannot be read. If the user has lost it but still has a computer that syncs with the remote, their files are safe on that computer: they can make a new remote with `init` at a new, empty URL and push to it.

## Managing remotes

```sh
syncit remote                    # list remembered vaults
syncit remote remove origin      # forget one; changes nothing in the remote, keeps the saved seed phrase
syncit remote add /Volumes/Backup/Documents   # folder vaults can be remembered too
```

- To rename a nickname: `remote remove <old>`, then `remote add --nickname <new> <url>`.
- A nickname wins over a folder of the same name; write `./origin` to mean the folder.
- syncit has no command to delete a remote's data. That is done in the storage provider's console, by the user.
- A remote keeps all history by default. See [history.md](history.md) for `--vault`.
- Two computers must not run `init` on the same empty URL at the same time.

## If the build has no remote support

`this build has no remote support: install with the remote feature` means syncit was built with `--no-default-features`. The user needs to reinstall with default features.
