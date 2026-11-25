//! What syncit prints. Code prints a message the moment something happens;
//! nothing is collected. A message has a text form (`Display`) and a JSON
//! form (`Serialize`); the caller picks the stream, `json` picks the form.

use crate::config::Config;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use std::fmt::{self, Display};

/// What everything that prints or writes is passed: the settings of the
/// whole process, and its progress bar.
#[derive(Clone)]
pub struct Context {
    /// Print JSON Lines instead of text.
    pub json: bool,
    /// Run in full but write nothing to either vault.
    pub dry_run: bool,
    /// Keep files the log says are deleted, and record them as re-created.
    pub no_remove: bool,
    /// A remote vault's secret key, as its 24 words: `SYNCIT_SECRET_KEY`.
    /// Read by `main`; the library never reads the environment for it.
    pub secret_key: Option<String>,
    /// Where long loops show how far they are: `bar(shown)`. One bar for the
    /// process, idle between loops; the print functions print above it.
    pub bar: ProgressBar,
    /// The user's syncit folder, which holds `credentials.toml` and
    /// `secrets.toml`. Found by `main`; tests leave it out.
    pub config_dir: Option<std::path::PathBuf>,
    /// In a run with two vaults, the name of the one the command runs in.
    /// What happens there is printed with `->`, and in the other with `<-`.
    pub ours: Option<String>,
}

impl Context {
    /// The arrow for what happens in the vault `name`; none in a command on
    /// one vault.
    pub fn side(&self, name: &str) -> &'static str {
        match &self.ours {
            None => "",
            Some(ours) if ours == name => "->",
            Some(_) => "<-",
        }
    }
}

/// What a run sent to a bare vault and received from it: blob hashes, since
/// a blob may be asked for twice, and how many segments.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traffic {
    pub sent_files: std::collections::BTreeSet<String>,
    pub sent_segments: usize,
    pub received_files: std::collections::BTreeSet<String>,
    pub received_segments: usize,
}

/// The user's syncit folder: under `$XDG_CONFIG_HOME`, else `~/.config`.
pub fn config_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".config")))
        .map(|dir| dir.join("syncit"))
}

/// Something that happened to one file.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    pub kind: &'static str,
    pub vault: String,
    pub path: String,
    pub note: String,
    /// For a rename, the path the file had before.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Text form only: `->` for our vault and `<-` for theirs in a run with
    /// a peer, empty in a command on one vault.
    #[serde(skip)]
    pub side: &'static str,
}

/// One entry of a file's history: a version, or a deletion.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Version {
    pub path: String,
    /// The content's hash; null for a deletion.
    pub hash: Option<String>,
    /// Whether the file is executable in this version.
    pub exec: bool,
    pub mtime: i64,
    /// The vaults that recorded it.
    pub nicknames: Vec<String>,
    /// Whether this is the path's current state.
    pub current: bool,
    /// Whether this vault can supply the content, so `restore` would work.
    pub available: bool,
}

/// One line of `status`: a file, or a folder whose files are all in one
/// state. The states and their marks are in `mark`.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct State {
    /// A folder's path ends in `/`.
    pub path: String,
    /// How many files the line stands for: 1 for a file.
    pub files: usize,
    pub ours: &'static str,
    /// The other vault's state, when `status` compares with one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theirs: Option<&'static str>,
    /// The conflict copies, when a vault's state is `conflict`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflict: Option<String>,
    /// When our state is `renamed`, the path the file had before; a folder's
    /// ends in `/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

/// The mark of a state of `status`: one mark for each.
fn mark(state: &str) -> &'static str {
    match state {
        "current" => "✓",
        "modified" => "✎",
        "new" => "+",
        "renamed" => "→",
        "removed" => "✗",
        "behind" => "◐",
        "missing" => "○",
        "conflict" => "≠",
        "excluded" | "undropped" => "-",
        "ignored" => "·",
        _ => "!", // incomplete
    }
}

impl State {
    /// The path as the text form shows it: `from -> path` for a rename.
    pub fn shown(&self) -> String {
        match &self.from {
            Some(from) => format!("{from} -> {}", self.path),
            None => self.path.clone(),
        }
    }

    /// The text form: the marks, the path padded to `width` characters, each
    /// vault's state as a word, then the number of files in a folder or the
    /// conflict copies.
    pub fn row(&self, width: usize) -> String {
        let states = [Some(self.ours), self.theirs];
        let marks: Vec<&str> = states.iter().flatten().map(|s| mark(s)).collect();
        let mut row = format!("{}  {:<width$}", marks.join(" "), self.shown());
        for state in states.iter().flatten() {
            row += &format!("  {state:<10}");
        }
        if self.path.ends_with('/') {
            row += &format!("  {} files", self.files);
        }
        if let Some(copies) = &self.conflict {
            row += &format!("  {copies}");
        }
        row.trim_end().to_string()
    }
}

/// Milliseconds since the epoch as `2026-11-20 14:03Z`.
// ponytail: UTC only, by the days-to-civil arithmetic; use a time crate if
// local time is wanted.
fn utc(ms: i64) -> String {
    let (days, secs) = (ms.div_euclid(86_400_000), ms.rem_euclid(86_400_000) / 1000);
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (h, min) = (secs / 3600, secs % 3600 / 60);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}Z")
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Message {
    Event(Event),
    Version(Version),
    State(State),
    /// An open conflict. `message` names the conflict copies.
    Conflict {
        vault: String,
        path: String,
        message: String,
        #[serde(skip)]
        side: &'static str,
    },
    /// A vault's config: the values of its `config.toml`, and for text the
    /// file as `Config::to_toml` writes it.
    Config {
        #[serde(flatten)]
        config: Config,
        #[serde(skip)]
        toml: String,
    },
    /// A vault this one remembers, in `remotes`.
    Remote {
        nickname: String,
        location: String,
    },
    /// The vaults that recorded the current version of a file.
    Blame {
        path: String,
        nicknames: Vec<String>,
    },
    Info {
        message: String,
    },
    Warning {
        message: String,
    },
    Error {
        message: String,
    },
    Summary(Summary),
}

impl Display for Message {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Whose vault, if there are two; then a letter and the path in the
        // vault, `.` for the vault itself.
        let head = |side: &str, letter: &str, path: &str| {
            let (space, path) = (
                if side.is_empty() { "" } else { " " },
                if path.is_empty() { "." } else { path },
            );
            format!("{side}{space}{letter} {path}")
        };
        match self {
            Message::Event(Event {
                kind,
                path,
                note,
                from,
                side,
                ..
            }) => {
                // The letters            } => {
                // The letters of `git status --short`, where git has one.
                let letter = match *kind {
                    "added" => "A",
                    "updated" => "M",
                    "deleted" | "moved" | "dropped" => "D",
                    "copied" => "C",
                    "renamed" => "R",
                    _ => "!",
                };
                let shown = match from {
                    Some(from) => format!("{from} -> {path}"),
                    None => path.clone(),
                };
                write!(f, "{}", head(side, letter, &shown))?;
                // Removed here by `move` or `drop`: which one, and why.
                let note = match (*kind, note.is_empty()) {
                    ("moved" | "dropped", true) => *kind,
                    _ => note.as_str(),
                };
                match (letter, note.is_empty()) {
                    ("!", true) => write!(f, " ({kind})"),
                    ("!", false) => write!(f, " ({kind}: {note})"),
                    (_, false) => write!(f, " ({note})"),
                    (_, true) => Ok(()),
                }
            }
            Message::Conflict {
                path,
                message,
                side,
                ..
            } => write!(f, "{} (conflict: {message})", head(side, "U", path)),
            Message::Version(v) => {
                let hash = v.hash.as_deref().map_or("--------", |h| &h[..8]);
                let mut notes = vec![];
                if v.current {
                    notes.push("current");
                }
                match &v.hash {
                    None => notes.push("deleted"),
                    Some(_) if !v.available => notes.push("no content"),
                    Some(_) => {}
                }
                if v.exec {
                    notes.push("executable");
                }
                let (time, names) = (utc(v.mtime), v.nicknames.join(","));
                write!(f, "{hash} {time} {names} {}", v.path)?;
                if !notes.is_empty() {
                    write!(f, " ({})", notes.join(", "))?;
                }
                Ok(())
            }
            Message::State(h) => write!(f, "{}", h.row(0)),
            Message::Config { toml, .. } => write!(f, "{}", toml.trim_end()),
            Message::Remote { nickname, location } => write!(f, "{nickname} {location}"),
            Message::Blame { path, nicknames } => write!(f, "{} {path}", nicknames.join(",")),
            Message::Info { message } => write!(f, "{message}"),
            Message::Warning { message } => write!(f, "warning: {message}"),
            Message::Error { message } => write!(f, "error: {message}"),
            Message::Summary(summary) => write!(f, "{summary}"),
        }
    }
}

/// What a run did: the events, already printed as they happened, and counts
/// of them. Returned by the commands; `main` prints the counts last and takes
/// the exit code from them.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    /// Entries written by scans.
    pub recorded: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub renamed: usize,
    pub conflicts: usize,
    pub pending: usize,
    pub skipped: usize,
    pub incomplete: usize,
    pub rejected: usize,
    #[serde(skip)]
    pub events: Vec<Event>,
    #[serde(skip)]
    pub traffic: Traffic,
}

impl Summary {
    /// 2 if something needs attention, else 0.
    pub fn exit_code(&self) -> i32 {
        let open = self.conflicts + self.pending + self.skipped + self.incomplete + self.rejected;
        if open > 0 { 2 } else { 0 }
    }

    /// Keep an event, and count it if the summary has a count for its kind.
    pub fn record(&mut self, event: Event) {
        match event.kind {
            "added" => self.added += 1,
            "updated" => self.updated += 1,
            "deleted" => self.deleted += 1,
            "renamed" => self.renamed += 1,
            "pending" => self.pending += 1,
            "skipped" => self.skipped += 1,
            "incomplete" => self.incomplete += 1,
            "rejected" => self.rejected += 1,
            _ => {}
        }
        self.events.push(event);
    }

    pub fn has(&self, kind: &str, path: &str) -> bool {
        self.events.iter().any(|e| e.kind == kind && e.path == path)
    }
}

impl Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "recorded {}, added {}, updated {}, deleted {}, renamed {}, conflicts {}, pending {}, skipped {}",
            self.recorded,
            self.added,
            self.updated,
            self.deleted,
            self.renamed,
            self.conflicts,
            self.pending,
            self.skipped
        )
    }
}

fn line<T: Display + Serialize>(value: &T, json: bool) -> String {
    if json {
        serde_json::to_string(value).expect("a message is plain data")
    } else {
        value.to_string()
    }
}

/// The bar for `Context`. Shown, it is drawn on stderr when that is a
/// terminal: `████░░░░ scanning 41,200 / 90,112`. `main` shows it unless
/// `--json`; tests do not. It starts idle, drawing nothing.
pub fn bar(shown: bool) -> ProgressBar {
    if !shown {
        return ProgressBar::hidden();
    }
    let template = "{bar:20} {msg} {human_pos} / {human_len}";
    let style = ProgressStyle::with_template(template).expect("the template is valid");
    let bar = ProgressBar::new(0).with_style(style.progress_chars("█░"));
    bar.finish_and_clear();
    bar
}

/// One long loop on the bar. Dropping it erases the bar and leaves it idle.
// ponytail: counts files, so one huge file stalls the bar; count bytes if
// that misleads.
pub struct Progress(ProgressBar);

impl Progress {
    pub fn new(bar: &ProgressBar, label: &'static str, total: usize) -> Progress {
        bar.set_length(total as u64);
        bar.set_message(label);
        bar.reset();
        Progress(bar.clone())
    }

    /// One more done.
    pub fn tick(&self) {
        self.0.inc(1);
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.0.finish_and_clear();
    }
}

/// Results.
pub fn print_stdout<T: Display + Serialize>(ctx: &Context, value: &T) {
    ctx.bar.suspend(|| println!("{}", line(value, ctx.json)));
}

/// Warnings and errors.
pub fn print_stderr<T: Display + Serialize>(ctx: &Context, value: &T) {
    ctx.bar.suspend(|| eprintln!("{}", line(value, ctx.json)));
}

/// The lines of `status`, with the paths padded to one width.
// ponytail: the width is counted in characters, so wide ones (CJK) put the
// columns out of line; use the unicode-width crate if that matters.
pub fn print_states(ctx: &Context, lines: &[State]) {
    let width = lines.iter().map(|h| h.shown().chars().count()).max();
    for h in lines {
        if ctx.json {
            print_stdout(ctx, &Message::State(h.clone()));
        } else {
            ctx.bar
                .suspend(|| println!("{}", h.row(width.unwrap_or(0))));
        }
    }
}

pub fn warn(ctx: &Context, message: String) {
    print_stderr(ctx, &Message::Warning { message });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_counts() {
        for shown in [false, true] {
            let ctx = Context {
                json: false,
                dry_run: false,
                no_remove: false,
                secret_key: None,
                bar: bar(shown),
                config_dir: None,
                ours: None,
            };
            let progress = Progress::new(&ctx.bar, "scanning", 3);
            progress.tick();
            progress.tick();
            assert_eq!(progress.0.position(), 2);
        }
    }

    #[test]
    fn message_forms() {
        let event = |kind, note: &str| Event {
            kind,
            vault: "/v".into(),
            path: "a/f.txt".into(),
            note: note.into(),
            from: None,
            side: "",
        };
        let message = |kind, note| Message::Event(event(kind, note));
        let both = |m: &Message| (line(m, false), line(m, true));
        assert_eq!(
            both(&message("updated", "")),
            (
                "M a/f.txt".into(),
                r#"{"type":"event","kind":"updated","vault":"/v","path":"a/f.txt","note":""}"#
                    .into()
            )
        );
        for (kind, text) in [
            ("added", "A a/f.txt"),
            ("deleted", "D a/f.txt"),
            ("copied", "C a/f.txt"),
            ("moved", "D a/f.txt (moved)"),
            ("dropped", "D a/f.txt (dropped)"),
        ] {
            assert_eq!(line(&message(kind, ""), false), text);
        }
        // A rename names both paths, as `git status --short` does.
        let renamed = Message::Event(Event {
            from: Some("old/f.txt".into()),
            side: "<-",
            ..event("renamed", "")
        });
        assert_eq!(
            both(&renamed),
            (
                "<- R old/f.txt -> a/f.txt".into(),
                r#"{"type":"event","kind":"renamed","vault":"/v","path":"a/f.txt","note":"","from":"old/f.txt"}"#
                    .into()
            )
        );
        assert_eq!(line(&message("pending", ""), false), "! a/f.txt (pending)");
        assert_eq!(
            line(&message("skipped", "changed"), false),
            "! a/f.txt (skipped: changed)"
        );
        let conflict = Message::Conflict {
            vault: "/v".into(),
            path: "n.txt".into(),
            message: "n-12345678.txt".into(),
            side: "",
        };
        assert_eq!(
            both(&conflict),
            (
                "U n.txt (conflict: n-12345678.txt)".into(),
                r#"{"type":"conflict","vault":"/v","path":"n.txt","message":"n-12345678.txt"}"#
                    .into()
            )
        );
        // With a peer, each line says whose vault; `.` is the vault itself.
        let sided = |side, kind, path: &str, note: &str| {
            Message::Event(Event {
                side,
                path: path.into(),
                ..event(kind, note)
            })
        };
        assert_eq!(
            both(&sided("->", "updated", "a/f.txt", "")),
            (
                "-> M a/f.txt".into(),
                r#"{"type":"event","kind":"updated","vault":"/v","path":"a/f.txt","note":""}"#
                    .into()
            )
        );
        assert_eq!(
            line(&sided("<-", "rejected", "", "segment x"), false),
            "<- ! . (rejected: segment x)"
        );
        let conflict = Message::Conflict {
            vault: "/v".into(),
            path: "n.txt".into(),
            message: "n-12345678.txt".into(),
            side: "<-",
        };
        assert_eq!(
            line(&conflict, false),
            "<- U n.txt (conflict: n-12345678.txt)"
        );
        let blame = Message::Blame {
            path: "a/f.txt".into(),
            nicknames: vec!["drive".into(), "notes".into()],
        };
        assert_eq!(
            both(&blame),
            (
                "drive,notes a/f.txt".into(),
                r#"{"type":"blame","path":"a/f.txt","nicknames":["drive","notes"]}"#.into()
            )
        );
        let version = |hash: Option<&str>, current: bool, available| {
            Message::Version(Version {
                path: "a/f.txt".into(),
                hash: hash.map(|h| h.repeat(8)),
                exec: hash.is_some() && !current,
                mtime: 1_700_000_000_000,
                nicknames: vec!["drive".into()],
                current,
                available,
            })
        };
        assert_eq!(
            both(&version(Some("3f9a1c2e"), true, true)),
            (
                "3f9a1c2e 2023-11-14 22:13Z drive a/f.txt (current)".into(),
                format!(
                    r#"{{"type":"version","path":"a/f.txt","hash":"{}","exec":false,"mtime":1700000000000,"nicknames":["drive"],"current":true,"available":true}}"#,
                    "3f9a1c2e".repeat(8)
                )
            )
        );
        assert_eq!(
            line(&version(Some("3f9a1c2e"), false, false), false),
            "3f9a1c2e 2023-11-14 22:13Z drive a/f.txt (no content, executable)"
        );
        assert_eq!(
            line(&version(None, true, false), false),
            "-------- 2023-11-14 22:13Z drive a/f.txt (current, deleted)"
        );
        let file_state = |path: &str, files, ours, theirs, conflict: Option<&str>| State {
            path: path.into(),
            files,
            ours,
            theirs,
            conflict: conflict.map(String::from),
            from: None,
        };
        let message = |h: &State| Message::State(h.clone());
        assert_eq!(
            both(&message(&file_state("a/f.txt", 1, "behind", None, None))),
            (
                "◐  a/f.txt  behind".into(),
                r#"{"type":"state","path":"a/f.txt","files":1,"ours":"behind"}"#.into()
            )
        );
        assert_eq!(
            both(&message(&file_state("music/", 1340, "current", Some("excluded"), None))),
            (
                "✓ -  music/  current     excluded    1340 files".into(),
                r#"{"type":"state","path":"music/","files":1340,"ours":"current","theirs":"excluded"}"#
                    .into()
            )
        );
        assert_eq!(
            both(&message(&file_state("n.txt", 1, "conflict", None, Some("n-12345678.txt")))),
            (
                "≠  n.txt  conflict    n-12345678.txt".into(),
                r#"{"type":"state","path":"n.txt","files":1,"ours":"conflict","conflict":"n-12345678.txt"}"#
                    .into()
            )
        );
        // One mark and one word for each state; the path is padded to a width.
        for (state, text) in [
            ("current", "✓  f     current"),
            ("modified", "✎  f     modified"),
            ("new", "+  f     new"),
            ("removed", "✗  f     removed"),
            ("behind", "◐  f     behind"),
            ("missing", "○  f     missing"),
            ("excluded", "-  f     excluded"),
            ("ignored", "·  f     ignored"),
            ("incomplete", "!  f     incomplete"),
        ] {
            assert_eq!(file_state("f", 1, state, None, None).row(4), text);
        }
        let renamed = State {
            from: Some("notes/".into()),
            ..file_state("docs/", 3, "renamed", None, None)
        };
        assert_eq!(
            both(&message(&renamed)),
            (
                "→  notes/ -> docs/  renamed     3 files".into(),
                r#"{"type":"state","path":"docs/","files":3,"ours":"renamed","from":"notes/"}"#
                    .into()
            )
        );
        assert_eq!(
            file_state("f", 1, "behind", Some("removed"), None).row(4),
            "◐ ✗  f     behind      removed"
        );
        assert_eq!(utc(0), "1970-01-01 00:00Z");
        assert_eq!(utc(-1), "1969-12-31 23:59Z");
        assert_eq!(utc(951_782_400_000), "2000-02-29 00:00Z");
        let warning = Message::Warning {
            message: "w".into(),
        };
        assert_eq!(
            both(&warning),
            (
                "warning: w".into(),
                r#"{"type":"warning","message":"w"}"#.into()
            )
        );
        let mut summary = Summary {
            recorded: 2,
            ..Summary::default()
        };
        summary.record(event("added", ""));
        summary.record(event("copied", ""));
        summary.record(event("skipped", "changed"));
        assert!(summary.has("copied", "a/f.txt") && !summary.has("updated", "a/f.txt"));
        assert_eq!(summary.exit_code(), 2);
        let (text, json) = both(&Message::Summary(summary));
        assert_eq!(
            text,
            "recorded 2, added 1, updated 0, deleted 0, renamed 0, conflicts 0, pending 0, skipped 1"
        );
        assert!(
            json.starts_with(r#"{"type":"summary","recorded":2,"added":1,"#),
            "{json}"
        );
        assert_eq!(Summary::default().exit_code(), 0);
    }
}
