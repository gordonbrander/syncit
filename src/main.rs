use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::exit;
use syncit::commands::{self, Setting};

/// Two-way sync and partial replication between folders.
#[derive(Parser)]
#[command(name = "syncit", version)]
struct Cli {
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
    Sync { vault: PathBuf },
    /// Send this vault's changes to another vault
    Push { vault: PathBuf },
    /// Take another vault's changes into this one
    Pull { vault: PathBuf },
    /// Record the folder's current state without syncing
    Snapshot,
    /// Show unrecorded changes, pending files and open conflicts
    Status,
    /// Start holding files of another vault: give their paths in that vault
    Get {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Stop holding paths in this vault, without deleting them elsewhere
    Drop {
        /// A vault that holds the files, so that dropping them is safe
        #[arg(long, value_name = "VAULT")]
        check: Option<PathBuf>,
        /// Drop even when no other copy of the content is known
        #[arg(long)]
        force: bool,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
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
    /// Send old versions to a shared folder: a path, or `none`
    Store { path: PathBuf },
}

fn main() {
    let cli = Cli::try_parse().unwrap_or_else(|e| {
        let _ = e.print();
        exit(if e.use_stderr() { 1 } else { 0 })
    });
    let cwd = Path::new(".");
    let result = match cli.command {
        Command::Init { dir } => commands::init(&dir),
        Command::Clone { src, dir } => commands::clone(&src, &dir),
        Command::Sync { vault } => commands::sync(cwd, &vault),
        Command::Push { vault } => commands::push(cwd, &vault),
        Command::Pull { vault } => commands::pull(cwd, &vault),
        Command::Snapshot => commands::snapshot(cwd),
        Command::Status => commands::status(cwd),
        Command::Get { paths } => commands::get(cwd, &paths),
        Command::Drop {
            check,
            force,
            paths,
        } => commands::drop(cwd, &paths, check.as_deref(), force),
        Command::Config { setting } => commands::config(
            cwd,
            setting.map(|setting| match setting {
                SettingArg::LocalHistory { value } => Setting::LocalHistory(value == "on"),
                SettingArg::Store { path } if path.as_os_str() == "none" => Setting::Store(None),
                SettingArg::Store { path } => Setting::Store(Some(path)),
            }),
        ),
    };
    match result {
        Ok(report) => {
            print!("{report}");
            exit(report.exit_code());
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            exit(1);
        }
    }
}
