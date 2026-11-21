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
    /// Make a folder a vault
    Init {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
    /// Create a vault from another vault, or from a sub-folder of one
    Clone { src: PathBuf, dir: PathBuf },
    /// Sync both ways with another vault
    Sync {
        vault: PathBuf,
        /// Show what would happen; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Send this vault's changes to another vault
    Push {
        vault: PathBuf,
        /// Show what would happen; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Take another vault's changes into this one
    Pull {
        vault: PathBuf,
        /// Show what would happen; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Record the folder's current state without syncing
    Snapshot,
    /// Start holding files of another vault: give their paths in that vault
    Get {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Move files to another vault: send them, check they arrived, remove them here
    Move {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
        vault: PathBuf,
        /// Show what would happen; change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Show the state of each file here: what needs recording, syncing or you
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
    /// Show which vault recorded the current version of each file
    Blame { paths: Vec<PathBuf> },
    /// List the recorded versions of files, newest first
    Log { paths: Vec<PathBuf> },
    /// Bring back an old version of a file, as a copy beside it
    Restore {
        path: PathBuf,
        /// The start of the version's hash, as `log` prints it
        hash: String,
        /// Replace the file itself instead of writing a copy
        #[arg(long)]
        in_place: bool,
    },
    /// Show this vault's settings, or change one
    Config {
        #[command(subcommand)]
        setting: Option<SettingArg>,
    },
}

#[derive(Subcommand)]
enum SettingArg {
    /// Keep old versions inside this vault: on or off
    LocalHistory {
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
    /// The name this vault's changes are recorded under
    Nickname { value: String },
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
        | Command::Move { dry_run, .. } => dry_run,
        _ => false,
    };
    let opts = &Options {
        json: cli.json,
        dry_run,
    };
    // Commands that run a sync return a summary, printed last.
    let result = match cli.command {
        Command::Init { dir } => commands::init(opts, &dir).map(|_| None),
        Command::Clone { src, dir } => commands::clone(opts, &src, &dir).map(Some),
        Command::Sync { vault, .. } => commands::sync(opts, cwd, &vault).map(Some),
        Command::Push { vault, .. } => commands::push(opts, cwd, &vault).map(Some),
        Command::Pull { vault, .. } => commands::pull(opts, cwd, &vault).map(Some),
        Command::Snapshot => commands::snapshot(opts, cwd).map(Some),
        Command::Status { paths, vs, all } => {
            commands::status(opts, cwd, &paths, vs.as_deref(), all).map(|_| None)
        }
        Command::Get { paths } => commands::get(opts, cwd, &paths).map(Some),
        Command::Move { paths, vault, .. } => {
            commands::move_to(opts, cwd, &paths, &vault).map(Some)
        }
        Command::Blame { paths } => commands::blame(opts, cwd, &paths).map(|_| None),
        Command::Log { paths } => commands::log(opts, cwd, &paths).map(|_| None),
        Command::Restore {
            path,
            hash,
            in_place,
        } => commands::restore(opts, cwd, &path, &hash, in_place).map(|_| None),
        Command::Config { setting } => {
            let change = setting.map(|setting| match setting {
                SettingArg::LocalHistory { value } => Setting::LocalHistory(value == "on"),
                SettingArg::Nickname { value } => Setting::Nickname(value),
            });
            commands::config(opts, cwd, change).map(|_| None)
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
