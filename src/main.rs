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
    /// Show unrecorded changes, pending files and open conflicts
    Status,
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
    /// Show which vault recorded the current version of each file
    Blame { paths: Vec<PathBuf> },
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
        Command::Status => commands::status(opts, cwd).map(|_| None),
        Command::Get { paths } => commands::get(opts, cwd, &paths).map(Some),
        Command::Move { paths, vault, .. } => {
            commands::move_to(opts, cwd, &paths, &vault).map(Some)
        }
        Command::Blame { paths } => commands::blame(opts, cwd, &paths).map(|_| None),
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
