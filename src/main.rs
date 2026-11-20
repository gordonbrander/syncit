use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::exit;
use syncit::commands;

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
    /// Sync both ways with a peer (default: the origin)
    Sync { peer: Option<PathBuf> },
    /// Send this vault's changes to a peer (default: the origin)
    Push { peer: Option<PathBuf> },
    /// Take a peer's changes into this vault (default: the origin)
    Pull { peer: Option<PathBuf> },
    /// Record the folder's current state without syncing
    Snapshot,
    /// Show unrecorded changes, pending files and open conflicts
    Status,
    /// Start holding paths in this vault, and fetch them
    Get {
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    /// Stop holding paths in this vault, without deleting them elsewhere
    Drop {
        /// Drop even when no other copy of the content is known
        #[arg(long)]
        force: bool,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
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
        Command::Sync { peer } => commands::sync(cwd, peer.as_deref()),
        Command::Push { peer } => commands::push(cwd, peer.as_deref()),
        Command::Pull { peer } => commands::pull(cwd, peer.as_deref()),
        Command::Snapshot => commands::snapshot(cwd),
        Command::Status => commands::status(cwd),
        Command::Get { paths } => commands::get(cwd, &paths),
        Command::Drop { force, paths } => commands::drop(cwd, &paths, force),
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
