use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::exit;
use syncit::commands::{self, Setting};
use syncit::output::{Message, Options, print_stderr, print_stdout};

const NOTES: &str = "\
Vaults:
  A vault is a folder that contains .syncit/. Commands act on the vault
  the current directory is in. Where a command takes <VAULT>, give one of:
    a path      any folder inside the other vault
    a URL       a remote vault, such as s3://bucket/prefix
    a nickname  a vault remembered with `syncit remote add`

Output:
  -> marks a file in this vault, <- a file in the other vault.
    A  added       M  modified       D  deleted
    R  renamed: R <old path> -> <new path>
    C  copy written (a conflict copy, or a restored version)
    U  conflict still open
    !  needs a look; the reason follows in brackets
  A D line ending (moved) or (dropped) was removed here by move or drop.
  The last line counts: recorded (changes newly noticed, in either
  vault); added, updated, deleted, renamed (files the run changed, in
  either vault); conflicts, pending, skipped (what needs a look).
  Results go to stdout, warnings and errors to stderr. A long run shows
  its progress on stderr when that is a terminal, and never with --json.
  With --json every line is a JSON object with a \"type\" field.

Exit codes:
  0  done
  1  failed
  2  done, but something needs a look: a conflict, or a file that was
     skipped or is still pending
  A --dry-run exits with the code the real run would.

Files:
  .syncitignore  at the vault root: one name or path per line that is
                 not to be synced; no wildcards
  ~/.config/syncit/credentials.toml
                 credentials for remote vaults";

const SYNC_NOTES: &str = "\
Changes made here since the last run are recorded first, then exchanged.
A file deleted in one vault is deleted in the other: preview with --dry-run.

Conflicts:
  When a file was changed in both vaults, the newer version keeps the
  name and the other is written beside it as name-<8 hex digits>.ext.
  The run prints `U <file> (conflict: <copy>)` and exits 2, on every run,
  until the conflict is closed. To close it: edit the file into the
  result you want, run the command again, then delete the copy.
  When one vault edited a file and the other deleted it, the edit is
  kept. Edit the file again or delete it again to close the conflict.";

const LOG_NOTES: &str = "\
Each line is: <hash> <time, UTC> <vault nickname> <path> (<notes>)
  3f9a1c2e 2023-11-14 22:13Z laptop notes.txt (current)

(current) is the last recorded version; the file on disk may be newer
(see `syncit status`). A hash of -------- is a deletion. (no content) marks a version whose
data this vault no longer stores; it cannot be restored from here.
To restore a version, pass the start of its hash to `syncit restore`.";

/// Two-way sync and partial replication between folders.
#[derive(Parser)]
#[command(name = "syncit", version, after_long_help = NOTES)]
struct Cli {
    /// Print JSON Lines instead of text
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Initialize a vault in a directory (or create a remote vault at a URL)
    ///
    /// With a URL, creates an encrypted remote vault at that location, which must be empty,
    /// and prints its 24-word seed phrase once. The phrase is the only way to read the remote
    /// from another computer. If SYNCIT_SECRET_KEY is set, that phrase is used and nothing is printed.
    ///
    /// Environment: SYNCIT_SECRET_KEY supplies a remote vault's 24-word seed phrase
    /// when you can't get it from the OS keychain, or type it into the terminal.
    Init {
        /// Directory path (or URL such as s3://bucket/prefix)
        #[arg(default_value = ".")]
        dir: String,
        /// Nickname for the remote vault [default: origin]
        #[arg(long)]
        nickname: Option<String>,
        /// Save remote's secret keys to `~/.config/syncit/secrets.toml` if there is no OS keychain
        #[arg(long)]
        save_key: bool,
    },
    /// Clone (copy) a vault
    ///
    /// Makes a new vault in DIR, pulls SRC into it, and remembers SRC as a remote.
    /// A URL needs the remote's 24-word seed phrase: clone asks for it at a terminal.
    ///
    /// Environment: SYNCIT_SECRET_KEY supplies a remote vault's 24-word seed phrase
    /// when you can't get it from the OS keychain, or type it into the terminal.
    Clone {
        /// Vault or URL
        ///
        /// A remote nickname works too. A path to a sub-folder of a vault clones only that sub-folder.
        src: PathBuf,
        /// Directory to clone to
        ///
        /// Must be empty, or not exist yet.
        dir: PathBuf,
        /// Clone only this subpath of the vault.
        ///
        /// Repeat to clone multiple subpaths.
        /// Makes clone a partial replica of the original, including only some of
        /// its subpaths when syncing. Useful for syncing just part of a large backup.
        ///
        /// With --only, SRC must be the vault's root.
        #[arg(long, value_name = "PATH")]
        only: Vec<String>,
        /// Save remote's secret keys to `~/.config/syncit/secrets.toml` if there is no OS keychain
        #[arg(long)]
        save_key: bool,
    },
    /// Sync both ways with another vault
    #[command(after_long_help = SYNC_NOTES)]
    Sync {
        /// The other vault: a path, URL, or remote nickname
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete anything during sync.
        ///
        /// Resurrects any files that were deleted on either side.
        ///
        /// This is permanent: later runs do not delete them either.
        #[arg(long)]
        no_remove: bool,
    },
    /// Push this vault's changes to another vault
    #[command(after_long_help = SYNC_NOTES)]
    Push {
        /// The other vault: a path, URL, or remote nickname
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete anything during push.
        ///
        /// Resurrects any files that were deleted.
        ///
        /// This is permanent: later runs do not delete them either.
        #[arg(long)]
        no_remove: bool,
    },
    /// Pull another vault's changes into this one
    #[command(after_long_help = SYNC_NOTES)]
    Pull {
        /// The other vault: a path, URL, or remote nickname
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete anything during pull.
        ///
        /// Resurrects any files that were deleted.
        ///
        /// This is permanent: later runs do not delete them either.
        #[arg(long)]
        no_remove: bool,
    },
    /// Snapshot current state without syncing.
    ///
    /// File snapshots are added to version history.
    Snapshot,
    /// Get subfolders of another vault (partial replication).
    ///
    /// Adds the paths to what this vault keeps, then pulls everything that changed in the other vault.
    Get {
        #[arg(required = true)]
        /// Subpaths to add (relative to other vault)
        ///
        /// Without --from, these are paths into the other vault, such as /Volumes/Backup/Archive/music.
        /// With --from, they are relative to that vault's root, such as music.
        paths: Vec<PathBuf>,
        /// The vault to get from. Path, URL, or remote nickname.
        #[arg(long, value_name = "VAULT")]
        from: Option<PathBuf>,
    },
    /// Move files to another vault.
    ///
    /// Sends files to the other vault, stops tracking them here.
    /// Useful for offloading files to a larger backup vault (partial replication).
    ///
    /// The files are removed here only after every one is confirmed to have arrived.
    /// They are not deleted from any other vault, and `syncit get` brings them back.
    /// Nothing is removed if the other vault excludes or ignores any of the files,
    /// or has its own newer version of one: sync with it, then move again.
    Move {
        /// Files and folders in this vault to move
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// The other vault: a path, URL, or remote nickname
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Add files to a vault (original files don't have to be in a vault)
    ///
    /// The vault is the one DEST is in; this can be run from anywhere.
    /// A folder is copied as a folder, to DEST/name. To copy only what is in it, cd into it and pass `.`.
    /// A file already at the destination is replaced. The vault is recorded first,
    /// so the replaced version can be restored.
    Cp {
        /// Files and folders to copy; `.` is this folder's contents
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Destination directory in the vault
        dest: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Stop tracking paths in this vault.
    ///
    /// Removes this vault's copy of the files,
    /// and excludes the paths from future replication. Doesn't remove them from other vaults.
    ///
    /// Refuses unless a copy of every file is known to be kept: in this vault's history,
    /// or in the vault named with --check. `syncit get` brings dropped paths back.
    /// A path that is not here yet can be dropped too, so that it is never fetched.
    Drop {
        /// Paths to stop tracking
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Make sure another vault has a backup of the files.
        ///
        /// Nothing is sent there. To send files and then remove them here, use `syncit move`.
        #[arg(long, value_name = "VAULT")]
        check: Option<PathBuf>,
        /// Drop files, even if no backup can be made
        #[arg(long)]
        force: bool,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the state of each file here
    #[command(after_help = "\
Marks:
  ✓  current     here and up to date
  ✎  modified    changed here, not yet recorded
  +  new         new here, not yet recorded
  →  renamed     moved here from the path before the arrow, not yet
                 recorded
  ✗  removed     gone from this vault
  ◐  behind      an out-of-date copy is here; sync updates it, or removes
                 it if the other vault shows ✗
  ○  missing     meant to be here, not here yet; sync brings it
  ≠  conflict    both versions are kept; the copies are named
  !  incomplete  an earlier sync did not finish; sync again
  -  excluded    not here by choice; get brings it
  ·  ignored     ignored by this vault

Only files that need something are listed; --all lists every file.
With --vs there are two marks and two words: ours first, theirs second.")]
    Status {
        /// Files or folders in this vault to show [default: .]
        paths: Vec<PathBuf>,
        /// Compare with another vault: ours first, theirs second
        #[arg(long = "vs", value_name = "VAULT")]
        vs: Option<PathBuf>,
        /// List every file, not only those that need something
        #[arg(short, long)]
        all: bool,
    },
    /// Show the vault that edited the current version of each file
    #[command(after_long_help = "\
Each line is: <vault nicknames> <path>, for the current version of the file.
Several nicknames, as in `laptop,drive notes.txt`, mean each of those vaults
recorded that same version, or that it is a merge of their edits.
For every version of a file and when it was made, see `syncit log`.")]
    Blame {
        /// Files or folders in this vault to show [default: all]
        paths: Vec<PathBuf>,
    },
    /// List file version history, newest first
    #[command(after_long_help = LOG_NOTES)]
    Log {
        /// Files or folders in this vault to show [default: all]
        paths: Vec<PathBuf>,
    },
    /// Restore an old version of a file
    ///
    /// Restores one file at a time. Without --in-place, the old version is written beside
    /// the file as name-<hash>.ext and the current file is not touched.
    /// Fails if this vault no longer stores that version's content: see `syncit log`
    /// and `syncit config keep-history`.
    Restore {
        /// Path of file to restore
        path: PathBuf,
        /// The start of the version's hash (find hashes with `syncit log`)
        hash: String,
        /// Replace the current file with the historical version?
        ///
        /// If omitted, will place the historical version as a copy beside the current one.
        ///
        /// The current file is recorded first, so the version that is replaced can itself
        /// be restored later, unless keep-history is 0. Also brings back a deleted file.
        #[arg(long)]
        in_place: bool,
    },
    /// Delete the file data for old versions to free up space
    ///
    /// Drops the stored content of versions that keep-history no longer covers.
    /// This cannot be undone. The current version of a file is never dropped,
    /// and the list of versions shown by `syncit log` is kept.
    Compact {
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Compact a different vault (path, URL, or remote nickname)
        #[arg(long, value_name = "VAULT")]
        vault: Option<PathBuf>,
    },
    /// Show this vault's settings, or change a setting
    Config {
        /// Configure a different vault (path, URL, or remote nickname)
        #[arg(long, value_name = "VAULT")]
        vault: Option<PathBuf>,
        /// Key and value to set
        #[command(subcommand)]
        setting: Option<SettingArg>,
    },
    /// List other vaults this one knows about. Add or remove other vaults.
    Remote {
        #[command(subcommand)]
        action: Option<RemoteArg>,
    },
}

#[derive(Subcommand)]
enum RemoteArg {
    /// Remember a vault by nickname (URL or vault directory)
    ///
    /// A URL needs the remote's 24-word seed phrase. It is asked for at a terminal the first time,
    /// then kept in the system keychain.
    ///
    /// Environment: SYNCIT_SECRET_KEY supplies a remote vault's 24-word seed phrase
    /// when you can't get it from the OS keychain, or type it into the terminal.
    Add {
        /// A vault directory, or a remote vault's URL
        location: String,
        /// Nickname for vault [default: the vault's own nickname]
        #[arg(long)]
        nickname: Option<String>,
        /// Save remote's secret keys to `~/.config/syncit/secrets.toml` if there is no OS keychain
        #[arg(long)]
        save_key: bool,
    },
    /// Forget a remote
    ///
    /// Changes nothing in the other vault, and keeps any saved key.
    Remove {
        /// Nickname of the remote to forget
        nickname: String,
    },
}

#[derive(Subcommand)]
enum SettingArg {
    /// How many versions back to store snapshot data?
    ///
    /// (Number of versions to keep, or "all")
    ///
    /// A new vault keeps 1; a new remote vault keeps all.
    /// With 0, old versions cannot be restored and text files are not merged automatically.
    /// Lowering the number deletes nothing at once: the next compaction does.
    KeepHistory {
        #[arg(value_parser = depth, value_name = "N|all")]
        value: Depth,
    },
    /// Automatically delete version data older than the cutoff?
    Autocompact {
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
    /// The nickname this vault's changes are recorded under
    Nickname {
        /// A label with no spaces, at most 64 bytes. Other vaults see it
        value: String,
    },
}

/// A value of `keep-history`: `None` is all.
#[derive(Clone)]
struct Depth(Option<u32>);

fn depth(s: &str) -> Result<Depth, String> {
    match s {
        "all" => Ok(Depth(None)),
        _ => s
            .parse()
            .map(|n| Depth(Some(n)))
            .map_err(|_| "a number, or all".to_string()),
    }
}

fn main() {
    let cli = Cli::try_parse().unwrap_or_else(|e| {
        let _ = e.print();
        exit(if e.use_stderr() { 1 } else { 0 })
    });
    let cwd = Path::new(".");
    let dry_run = match cli.command {
        Command::Sync { dry_run, .. }
        | Command::Push { dry_run, .. }
        | Command::Pull { dry_run, .. }
        | Command::Move { dry_run, .. }
        | Command::Cp { dry_run, .. }
        | Command::Compact { dry_run, .. }
        | Command::Drop { dry_run, .. } => dry_run,
        _ => false,
    };
    let no_remove = match cli.command {
        Command::Sync { no_remove, .. }
        | Command::Push { no_remove, .. }
        | Command::Pull { no_remove, .. } => no_remove,
        _ => false,
    };
    let opts = &Options {
        json: cli.json,
        dry_run,
        no_remove,
        secret_key: std::env::var("SYNCIT_SECRET_KEY").ok(),
        progress: !cli.json,
        config_dir: syncit::output::config_dir(),
    };
    // Commands that run a sync return a summary, printed last.
    let result = match cli.command {
        Command::Init {
            dir,
            nickname,
            save_key,
        } if dir.contains("://") => {
            commands::init_remote(opts, &dir, nickname, save_key).map(|_| None)
        }
        Command::Init {
            nickname: Some(_), ..
        } => Err(anyhow::anyhow!(
            "--nickname is for a remote vault; see `syncit config nickname`"
        )),
        Command::Init { dir, .. } => commands::init(opts, Path::new(&dir)).map(|_| None),
        Command::Clone {
            src,
            dir,
            only,
            save_key,
        } => commands::clone(opts, cwd, &src, &dir, &only, save_key).map(Some),
        Command::Sync { vault, .. } => commands::sync(opts, cwd, &vault).map(Some),
        Command::Push { vault, .. } => commands::push(opts, cwd, &vault).map(Some),
        Command::Pull { vault, .. } => commands::pull(opts, cwd, &vault).map(Some),
        Command::Snapshot => commands::snapshot(opts, cwd).map(Some),
        Command::Status { paths, vs, all } => {
            commands::status(opts, cwd, &paths, vs.as_deref(), all).map(|_| None)
        }
        Command::Get {
            paths,
            from: Some(from),
        } => {
            let paths: Option<Vec<String>> =
                paths.iter().map(|p| p.to_str().map(String::from)).collect();
            match paths {
                Some(paths) => commands::get_from(opts, cwd, &paths, &from).map(Some),
                None => Err(anyhow::anyhow!("a path is not UTF-8")),
            }
        }
        Command::Get { paths, from: None } => commands::get(opts, cwd, &paths).map(Some),
        Command::Move { paths, vault, .. } => {
            commands::move_to(opts, cwd, &paths, &vault).map(Some)
        }
        Command::Cp { paths, dest, .. } => commands::cp(opts, cwd, &paths, &dest).map(Some),
        Command::Drop {
            paths,
            check,
            force,
            ..
        } => commands::drop(opts, cwd, &paths, check.as_deref(), force).map(Some),
        Command::Blame { paths } => commands::blame(opts, cwd, &paths).map(|_| None),
        Command::Log { paths } => commands::log(opts, cwd, &paths).map(|_| None),
        Command::Restore {
            path,
            hash,
            in_place,
        } => commands::restore(opts, cwd, &path, &hash, in_place).map(|_| None),
        Command::Compact { vault, .. } => {
            commands::compact(opts, cwd, vault.as_deref()).map(|_| None)
        }
        Command::Remote { action: None } => commands::remotes(opts, cwd).map(|_| None),
        Command::Remote {
            action:
                Some(RemoteArg::Add {
                    location,
                    nickname,
                    save_key,
                }),
        } => commands::remote_add(opts, cwd, &location, nickname, save_key).map(|_| None),
        Command::Remote {
            action: Some(RemoteArg::Remove { nickname }),
        } => commands::remote_remove(opts, cwd, &nickname).map(|_| None),
        Command::Config { vault, setting } => {
            let change = setting.map(|setting| match setting {
                SettingArg::KeepHistory { value } => Setting::KeepHistory(value.0),
                SettingArg::Autocompact { value } => Setting::Autocompact(value == "on"),
                SettingArg::Nickname { value } => Setting::Nickname(value),
            });
            commands::config(opts, cwd, vault.as_deref(), change).map(|_| None)
        }
    };
    match result {
        Ok(None) => {}
        Ok(Some(summary)) => {
            let code = summary.exit_code();
            print_stdout(&Message::Summary(summary), opts.json);
            exit(code);
        }
        Err(e) => {
            let message = format!("{e:#}");
            print_stderr(&Message::Error { message }, opts.json);
            exit(1);
        }
    }
}
