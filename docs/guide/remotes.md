---
title: Remotes
---

[← All guides](index.md)

# Remotes

A vault can live in cloud storage: an S3 bucket, Google Cloud Storage, Azure, Cloudflare R2, or a plain folder by `file://` URL. Such a *remote vault* has no working folder of its own. It holds the files and their history, encrypted, and any vault can sync with it. Two computers that never see each other stay in step by both syncing with the same remote.

This guide sets one up on Cloudflare R2. Other S3-compatible services work the same way with their own endpoint and keys.

## 1. Make a bucket

In the Cloudflare dashboard, under **R2 Object Storage**:

1. **Create bucket**, for example `my-bucket`.
2. **Manage API tokens → Create API token.** Give it *Object Read & Write* permission, scoped to that bucket. Note the **Access Key ID** and **Secret Access Key** it shows; the secret is not shown again.
3. Note your **account ID**. The bucket's S3 endpoint is `https://<account-id>.r2.cloudflarestorage.com`.
4. In the bucket's settings, check that there is an **object lifecycle rule** that aborts incomplete multipart uploads after a few days, and add one if not. Files of 16 MB or more are uploaded in parts; if an upload is interrupted, the parts already sent stay in the bucket, unseen, until the bucket removes them.

## 2. Give syncit the bucket's credentials

Put them in `~/.config/syncit/credentials.toml` (or `$XDG_CONFIG_HOME/syncit/credentials.toml`), one `[[remotes]]` entry for each bucket or remote, and every command finds them. For the R2 bucket above, and an S3 bucket on another account:

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

Make the file readable by your user only:

```sh
chmod 600 ~/.config/syncit/credentials.toml
```

A `url` is a prefix: `s3://my-bucket` covers every remote in that bucket, such as `s3://my-bucket/notes`, and an entry with a longer `url` overrides it for the remotes under it. Syncit never writes this file, and `~/.aws/credentials` is not read.

The settings are the usual ones: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_DEFAULT_REGION`, `AWS_ENDPOINT_URL` (for R2, MinIO and other S3-compatible services) and `AWS_ALLOW_HTTP`, and the `GOOGLE_*` and `AZURE_*` equivalents. Names may be written in lower case, as in `~/.aws/credentials`. Environment variables of the same names override the file.

## 3. Create the remote vault

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

The remote's nickname is `origin` unless you give another with `--nickname`.

## 4. Sync a vault with it

In a vault, remember the remote under its nickname and sync with it like any other vault:

```sh
cd ~/Documents
syncit remote add s3://my-bucket/notes
syncit sync origin
```

`sync`, `push`, `pull`, `move`, `status --vs` and `drop --check` all take a remote by nickname or URL.

## 5. Use it from another computer

Set up `credentials.toml` there too (step 2), then make a vault from the remote, all of it or [part](partial-sync.md):

```sh
syncit clone s3://my-bucket/notes ~/Documents
syncit clone s3://my-bucket/notes ~/Photos --only photos/2024 --only music
syncit get music --from origin     # later, add more
```

`clone` and `remote add` ask for the 24 words the first time.

## Managing the key

`remote add` and `clone` ask for the words once, then keep them in the system keychain (macOS Keychain, or the Secret Service on Linux). After you reinstall or rebuild syncit, macOS asks once whether it may read the keychain item again.

Where there is no keychain, such as a server over SSH, you have two choices:

- **Save it to a file.** Pass `--save-key` (or answer yes when asked) and the key is saved in `~/.config/syncit/secrets.toml`. It is stored there unencrypted, readable by your user only, and every vault on the machine finds it.
- **Pass it in the environment.** Set `SYNCIT_SECRET_KEY` to the 24 words for each command. This is what scripts and cron jobs usually do.

Keep a copy of the words somewhere other than the computers that use them, such as a password manager or on paper. syncit cannot recover them.

## Managing remotes

```sh
syncit remote                      # list the vaults this one remembers
syncit remote remove origin        # forget one (touches nothing in the remote)
```

A folder vault can be remembered too: `syncit remote add /Volumes/Backup/Documents`. To reach a folder that has the same name as a remote, write it as a path: `./origin`.

A remote keeps every version by default. Change that, or compact it, with `--vault` (see [Version history](history.md)):

```sh
syncit config --vault origin keep-history 5
syncit compact --vault origin
```

[← All guides](index.md)
