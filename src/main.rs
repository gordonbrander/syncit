use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::exit;
use syncit::commands::{self, Setting};
use syncit::output::{Message, Options, print_stderr, print_stdout};

/// Two-way sync and partial replication between folders.
#[derive(Parser)]
#[command(name = "syncit", version)]
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
    Clone {
        /// Vault or URL
        src: PathBuf,
        /// Directory to clone to
        dir: PathBuf,
        /// Clone only this subpath of the vault (partial replication). Repeat to clone multiple subpaths.
        #[arg(long, value_name = "PATH")]
        only: Vec<String>,
        /// Save remote's secret keys to `~/.config/syncit/secrets.toml` if there is no OS keychain
        #[arg(long)]
        save_key: bool,
    },
    /// Sync both ways with another vault
    Sync {
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete anything. Resurrects any files that were deleted on either side.
        #[arg(long)]
        no_remove: bool,
    },
    /// Push this vault's changes to another vault
    Push {
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete anything. Resurrects any files that were deleted on either side.
        #[arg(long)]
        no_remove: bool,
    },
    /// Pull another vault's changes into this one
    Pull {
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Do not delete anything. Resurrects any files that were deleted on either side.
        #[arg(long)]
        no_remove: bool,
    },
    /// Snapshot current state without syncing. File snapshots added to version history.
    Snapshot,
    /// Get subfolders of another vault (partial replication).
    Get {
        #[arg(required = true)]
        /// Subpaths to add (relative to other vault)
        paths: Vec<PathBuf>,
        /// The vault to get from. Path, URL, or remote nickname.
        #[arg(long, value_name = "VAULT")]
        from: Option<PathBuf>,
    },
    /// Move files to another vault. Sends files to the other vault, stops tracking them here.
    /// Useful for offloading files to a larger backup vault (partial replication).
    Move {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        vault: PathBuf,
        /// Show what would happen. Change nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Add files to a vault (original files don't have to be in a vault)
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
    /// Stop tracking paths in this vault. Removes this vault's copy of the files,
    /// and excludes the paths from future replication. Doesn't remove them from other vaults.
    Drop {
        /// Paths to stop tracking
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        /// Another vault that has a backup of the files. The the current state of the dropped files will be snapshotted and pushed there, for backup.
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
    /// Show the vault that edited each version of a file
    Blame { paths: Vec<PathBuf> },
    /// List file version history, newest first
    Log { paths: Vec<PathBuf> },
    /// Restore an old version of a file
    Restore {
        /// Path of file to restore
        path: PathBuf,
        /// The start of the version's hash (find hashes with `syncit log`)
        hash: String,
        /// Replace the current file with the historical version?
        /// If omitted, will place the historical version as a copy beside the current one.
        #[arg(long)]
        in_place: bool,
    },
    /// Delete the file data for old versions to free up space
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
    /// Remember a vault by name (URL or vault directory)
    Add {
        location: String,
        /// Nickname for vault [default: the vault's own nickname]
        #[arg(long)]
        name: Option<String>,
        /// Save remote's secret keys to `~/.config/syncit/secrets.toml` if there is no OS keychain
        #[arg(long)]
        save_key: bool,
    },
    /// Forget a remote
    Remove { name: String },
}

#[derive(Subcommand)]
enum SettingArg {
    /// How many versions back to store snapshot data? (Number of versions to keep, or "all")
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
    Nickname { value: String },
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
                    name,
                    save_key,
                }),
        } => commands::remote_add(opts, cwd, &location, name, save_key).map(|_| None),
        Command::Remote {
            action: Some(RemoteArg::Remove { name }),
        } => commands::remote_remove(opts, cwd, &name).map(|_| None),
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
